//! `rime-agentd` — the Rime agent runtime.
//!
//! Unprivileged, one per user, started by `systemd --user`. It owns the PTYs
//! that agent sessions run on, the sandbox each session is confined by, the
//! project and checkpoint state around them, and the control socket the `rime`
//! CLI and Rime Shell talk to.
//!
//! It deliberately holds no privilege and never talks to `rimed`. Agent
//! orchestration inside the privileged daemon is the thing the roadmap
//! forbids; when a session eventually needs a system change, that will be a
//! narrow request to the frozen `org.rimeos.Rimed1` surface, made by the user's
//! own `rime` invocation, not a right this process holds.
//!
//! Threading: one thread accepting connections, one per connection, one per
//! session. Sessions are few and the work is I/O, so this is simpler and more
//! predictable than an async runtime — and the blocking `read` on a PTY is
//! exactly what the kernel is good at.

mod broker;
mod browser_ca;
mod budget;
mod clipboard;
mod desktop;
mod disposable;
mod egress;
mod elevation;
mod grants;
mod inject;
mod intercept;
mod origin;
mod peer;
mod privilege;
mod pty;
mod registry;
mod session;
mod worktrees;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rime_agent_core::adapter;
use rime_agent_core::auth::{Authenticator, PolkitAuthenticator};
use rime_agent_core::config::Config;
use rime_agent_core::paths;
use rime_agent_core::profile;
use rime_agent_core::project;
use rime_agent_core::protocol::{
    ErrorKind, Request, Response, SessionInfo, PROTOCOL_VERSION,
};
use rime_agent_core::request;

use crate::registry::Registry;

/// Everything the connection threads share.
pub struct Daemon {
    pub registry: Mutex<Registry>,
    pub config: Mutex<Config>,
    /// The system-access grants this process is holding (§4.4, §4.5).
    ///
    /// Not a `Mutex<...>` around a map here: the authority owns its own lock,
    /// because the invariant it protects is "authority is this process's
    /// memory" and it should not be possible to take the registry lock and
    /// the grant lock in the wrong order.
    pub grants: grants::GrantAuthority,
    /// How a human is asked for a password.
    ///
    /// Boxed behind the trait so a test can build a daemon that never reaches
    /// polkit. Nothing in this repository's test suite raises a prompt, and
    /// this field is why that is enforceable.
    pub auth: Box<dyn Authenticator>,
    /// Test runs seen going past, by worktree (§P1-036).
    ///
    /// Its own lock, and never taken while a session lock is held — the
    /// event handler drops the session before recording. Same discipline as
    /// the grant authority above, for the same reason.
    pub tests: worktrees::TestObservations,
    /// The elevation challenges this process has issued and not yet seen
    /// answered (§7, P0-014).
    ///
    /// In memory, and deliberately: a challenge that survived a restart would
    /// be a request to touch a key outliving the process that asked for it,
    /// and a daemon restarting is exactly when a stale one should stop being
    /// good. `ChallengeStore` expires its own contents on every `issue` and
    /// every `redeem`, so it needs no place in the expiry tick — which is
    /// also why this is a plain `Mutex` like `registry` and `config` rather
    /// than an authority owning its own lock: nothing here has an ordering
    /// relationship with the grant lock, because issuing a challenge grants
    /// nothing.
    pub challenges: Mutex<rime_agent_core::webauthn::ChallengeStore>,
}

impl Daemon {
    fn new() -> Daemon {
        Daemon {
            registry: Mutex::new(Registry::new()),
            config: Mutex::new(Config::load()),
            grants: grants::GrantAuthority::new(),
            auth: Box::new(PolkitAuthenticator),
            tests: worktrees::TestObservations::new(),
            challenges: Mutex::new(rime_agent_core::webauthn::ChallengeStore::new()),
        }
    }
}

/// How often the daemon looks for a grant whose window has run out.
///
/// §3.4 asks for automatic expiry, and a break-glass grant's expiry means
/// ending the session — so the granularity of this tick is the granularity of
/// that promise. Five seconds on a fifteen-minute window is under one percent
/// of it, and the cost is one pass over a map that is almost always empty.
const EXPIRY_TICK: std::time::Duration = std::time::Duration::from_secs(5);

fn main() {
    // The in-sandbox half of the `allowlist` network mode, answered before any
    // daemon state exists. The same binary is used rather than a sibling so
    // both ends of the egress protocol are compiled together and there is no
    // second path for the sandbox to have to be able to see. See egress.rs.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if let Some(bridge) = egress::parse_bridge_argv(&argv) {
        match bridge {
            Ok(b) => std::process::exit(egress::run_bridge(b)),
            Err(e) => {
                eprintln!("rime-agentd: {e}");
                std::process::exit(2);
            }
        }
    }

    if let Err(e) = run() {
        eprintln!("rime-agentd: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    // Writing to an attached client that has gone away must not kill the
    // daemon. Every write site already handles the error.
    // Safe: setting a signal disposition before any thread exists.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };

    let socket = paths::control_socket();
    let dir = socket
        .parent()
        .context("the control socket path has no parent directory")?;
    paths::ensure_private_dir(dir)?;
    paths::ensure_private_dir(&paths::state_dir())?;
    privilege::ensure_dirs();

    // Sessions never outlive the daemon that owned their PTYs, so any record
    // left claiming to be running is from a previous life.
    registry::reconcile_stale_records();

    let listener = bind(&socket)?;
    let daemon = Arc::new(Daemon::new());

    // §3.4's fifth requirement, answered rather than dodged. A grant left on
    // disk has ended — a grant is in force only while the process that minted
    // it is holding it — and this is where the machine says HOW each one
    // ended, once, in the trail and on the daemon's own log. A break-glass
    // window that was open when the machine went down is reported as ended at
    // the reboot; one that ran out before that is reported as expired; one
    // from this boot means this daemon replaced another. Losing the
    // distinction would make "does not silently persist" true only in the
    // sense that nothing says anything.
    //
    // ORDERING, and both halves of it are load-bearing.
    //
    // AFTER `bind`, because `bind` is what establishes that no other
    // rime-agentd is already listening here. Sweeping first would mean a
    // daemon that is about to fail to start had already closed a RUNNING
    // daemon's grants and written their endings to the trail.
    //
    // BEFORE `listener.incoming()`, which is what makes the gap between the
    // two unobservable. The socket is connectable from `bind` onwards, so a
    // client can connect while the sweep is still running — but nothing is
    // ANSWERED until this loop has finished, so no caller can be told about a
    // grant from a previous boot as though it were still live. A reply is
    // therefore proof that the sweep has run, and that is the barrier
    // `tests/system_grants.rs` waits on; it used to wait on the connect
    // instead and read the audit trail before this had written it, which is
    // the flake two rounds reported. Moving the sweep below the accept loop
    // would reopen both.
    for said in daemon.grants.sweep_previous_lives() {
        eprintln!("rime-agentd: {said}");
    }
    spawn_expiry_thread(Arc::clone(&daemon));
    // A save cut short by the last daemon's end left its temporary file in
    // Downloads. After `bind`, for the same reason as the grants: a daemon
    // that is about to fail to start must not touch a running one's files.
    desktop::sweep_downloads();

    // Shutdown runs on its own thread waiting on a blocked signal set, rather
    // than in a handler: stopping sessions means taking locks and iterating a
    // map, neither of which is legal in a signal handler.
    block_termination_signals();
    spawn_signal_thread(Arc::clone(&daemon), socket.clone());

    eprintln!(
        "rime-agentd: listening on {} ({} adapters)",
        socket.display(),
        adapter::ADAPTERS.len()
    );

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let daemon = Arc::clone(&daemon);
                std::thread::Builder::new()
                    .name("rime-agentd-conn".into())
                    .spawn(move || {
                        if let Err(e) = serve(&daemon, stream) {
                            eprintln!("rime-agentd: connection ended: {e:#}");
                        }
                    })
                    .context("spawning a connection thread")?;
            }
            Err(e) => eprintln!("rime-agentd: accept failed: {e}"),
        }
    }
    Ok(())
}

/// Bind the control socket, replacing a stale one.
///
/// A socket file left by a daemon that was killed cannot be bound over, but it
/// also cannot be connected to. Probing before unlinking is what stops this
/// from stealing the socket out from under a daemon that is genuinely running.
fn bind(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            anyhow::bail!(
                "another rime-agentd is already listening on {}",
                path.display()
            );
        }
        std::fs::remove_file(path)
            .with_context(|| format!("removing the stale socket {}", path.display()))?;
    }
    let listener = UnixListener::bind(path)
        .with_context(|| format!("binding the control socket {}", path.display()))?;

    // The socket is the control plane for the user's agents; nobody else on
    // the machine may connect to it. The parent directory is already 0700.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restricting {}", path.display()))?;
    Ok(listener)
}

/// Block the termination signals in every thread, so only the signal thread
/// receives them.
fn block_termination_signals() {
    // Safe: building and installing a signal mask for this process.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigaddset(&mut set, libc::SIGHUP);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

/// Close grants whose windows have run out, and end the sessions that cannot
/// survive their grant ending.
///
/// §3.4's "automatic expiry". A session grant stops applying the moment it
/// leaves the authority's map, so for that kind the sweep is the whole of the
/// expiry. Break-glass is different and the difference is a kernel fact:
/// `PR_SET_NO_NEW_PRIVS` was cleared between `fork` and `exec` and no process
/// can put it back, so a break-glass session outliving its window would keep
/// its `sudo` whatever any record said. The only honest expiry for that mode
/// is to end the session, and that is what this does.
fn spawn_expiry_thread(daemon: Arc<Daemon>) {
    use rime_agent_core::grant::SystemGrant;
    use rime_agent_core::lock::{LockWatch, Loginctl};

    std::thread::Builder::new()
        .name("rime-agentd-grants".into())
        .spawn(move || {
            // Break-glass grants whose sessions have been asked to end and
            // have not yet been observed gone. Held here rather than on the
            // session, because what is being tracked is a promise the audit
            // trail made and not a property of the process.
            let mut ending: Vec<SystemGrant> = Vec::new();
            // §7's lock rules. This thread rather than a second one because
            // the two jobs are the same job: both end with a grant closing
            // and a session that cannot outlive it, and sharing `ending`
            // means a break-glass session revoked by a screen lock gets the
            // same SIGTERM-then-SIGKILL escalation as one whose window ran
            // out. A separate thread would have needed its own copy of that,
            // which is exactly how the two paths would drift apart.
            let mut watch = LockWatch::new();
            let mut observer = Loginctl::new();
            loop {
                std::thread::sleep(EXPIRY_TICK);
                let now = request::now_ms();

                for (grant, ends_session) in daemon.grants.expire(now) {
                    eprintln!("rime-agentd: {}", grant.describe(now, daemon.grants.boot()));
                    if !ends_session {
                        continue;
                    }
                    if end_session_for_grant(
                        &daemon,
                        &grant,
                        "its break-glass window is over and no_new_privs cannot be put back on \
                         a running process",
                        now,
                    ) {
                        ending.push(grant);
                    }
                }

                lock_tick(&daemon, &mut watch, &mut observer, &mut ending, now);

                escalate_ending(&daemon, &mut ending, now);
            }
        })
        .ok();
}

/// Ask a session to end because the grant it was running under has closed.
///
/// Shared by expiry and by revoke-on-lock, and that sharing is the point:
/// closing a break-glass grant is not the same thing as ending its session,
/// because `PR_SET_NO_NEW_PRIVS` was cleared between `fork` and `exec` and no
/// process can put it back. A revocation that only dropped the grant from the
/// authority's map would leave a session that still has root and a record
/// saying it does not.
///
/// Returns whether the session is still live and therefore has to be watched
/// through [`escalate_ending`].
fn end_session_for_grant(
    daemon: &Arc<Daemon>,
    grant: &rime_agent_core::grant::SystemGrant,
    why: &str,
    now: u64,
) -> bool {
    let Some(handle) = lookup(daemon, grant.session) else {
        return false;
    };
    let mut s = handle.lock().expect("session lock");
    if !s.info.is_live() {
        return false;
    }
    eprintln!("rime-agentd: ending session {} — {why}", grant.session);
    registry::terminate(&mut s);
    registry::write_record(&s.info);
    drop(s);
    // A separate line from the grant's own closure, and it deliberately does
    // not claim the session is gone. The grant ending and the process ending
    // are two facts, and a line that implied the second would be a log that
    // lies in exactly the case that matters: a session that declined SIGTERM
    // and still has root.
    grant_note(grant, "session-ending", now);
    true
}

/// The escalation, and then the confirmation. `SIGTERM` is a request; §3.4's
/// expiry is not, and neither is §7's revoke-on-lock.
fn escalate_ending(
    daemon: &Arc<Daemon>,
    ending: &mut Vec<rime_agent_core::grant::SystemGrant>,
    now: u64,
) {
    ending.retain(|grant| {
        let Some(handle) = lookup(daemon, grant.session) else {
            grant_note(grant, "session-ended", now);
            return false;
        };
        let mut s = handle.lock().expect("session lock");
        if !s.info.is_live() {
            grant_note(grant, "session-ended", now);
            return false;
        }
        let asked = s.closing_since_ms.unwrap_or(now);
        if now.saturating_sub(asked) >= registry::TERMINATE_GRACE_MS && registry::force_kill(&mut s)
        {
            eprintln!(
                "rime-agentd: session {} did not exit within {}ms of its grant ending; killed",
                grant.session,
                registry::TERMINATE_GRACE_MS
            );
            drop(s);
            grant_note(grant, "session-killed", now);
        }
        true
    });
}

/// One observation of the screen, and whatever §7 says follows from it.
///
/// The policy is re-read from the configuration file on every tick rather
/// than taken from `daemon.config`, which is loaded once at startup. A lock
/// rule that needed the runtime restarted before it applied would be a
/// setting that lies about when it takes effect, and `rime agent lock` is
/// meant to be something the owner changes and then walks away from the
/// machine.
fn lock_tick(
    daemon: &Arc<Daemon>,
    watch: &mut rime_agent_core::lock::LockWatch,
    observer: &mut dyn rime_agent_core::lock::LockObserver,
    ending: &mut Vec<rime_agent_core::grant::SystemGrant>,
    now: u64,
) {
    use rime_agent_core::lock::{GrantView, SessionView};

    let state = observer.observe();
    let policy = Config::load().lock;

    let sessions: Vec<SessionView> = daemon
        .registry
        .lock()
        .expect("registry lock")
        .list()
        .into_iter()
        .filter_map(|h| {
            let s = h.lock().expect("session lock");
            s.info.is_live().then(|| SessionView {
                id: s.info.id,
                origin: s.info.request_origin,
                paused: s.info.paused,
            })
        })
        .collect();
    let grants: Vec<GrantView> = daemon
        .grants
        .active(now)
        .into_iter()
        .map(|g| GrantView { id: g.id })
        .collect();

    let actions = watch.step(&policy, &state, &sessions, &grants);
    if actions.is_empty() {
        return;
    }
    if let Some(why) = state.reason() {
        eprintln!(
            "rime-agentd: the screen state could not be read ({why}), which §7's rules treat \
             as locked"
        );
    }

    for (id, why) in actions.hold {
        hold_session(daemon, id, &why);
    }
    for id in actions.resume {
        resume_session(daemon, id);
    }
    for id in actions.revoke {
        match daemon.grants.revoke(id, now) {
            Ok(grant) => {
                eprintln!(
                    "rime-agentd: {} — the screen locked, and §7 revokes short-lived root \
                     grants by default (`rime agent lock --root-grants keep` to stop this)",
                    grant.describe(now, daemon.grants.boot())
                );
                if grant.kind.expiry_ends_the_session()
                    && end_session_for_grant(
                        daemon,
                        &grant,
                        "the screen locked, its break-glass grant was revoked, and \
                         no_new_privs cannot be put back on a running process",
                        now,
                    )
                {
                    ending.push(grant);
                }
            }
            // Not an error worth failing over: a grant can expire between the
            // list and the revoke, and the outcome is the one that was
            // wanted either way.
            Err(e) => eprintln!("rime-agentd: grant {id} was not revoked on lock: {e}"),
        }
    }
}

/// Stop a session because the screen is locked.
///
/// The same `SIGSTOP` and the same `paused` flag `rime agent pause` sets, so
/// there is one notion of a stopped session rather than two. The flag is set
/// only after the signal succeeded, for the reason `Request::Signal` gives:
/// a flag set first would claim a session was paused when the signal failed.
fn hold_session(daemon: &Arc<Daemon>, id: u32, why: &str) {
    let Some(handle) = lookup(daemon, id) else {
        return;
    };
    let mut s = handle.lock().expect("session lock");
    if !s.info.is_live() {
        return;
    }
    match pty::signal_group(s.pgid, libc::SIGSTOP) {
        Ok(()) => {
            s.info.paused = true;
            s.info.detail = Some("held — the screen is locked".to_string());
            registry::write_record(&s.info);
            eprintln!("rime-agentd: session {id} held — {why}");
        }
        // The watch has already recorded the hold, so the unlock will send a
        // SIGCONT to a session that was never stopped, which is harmless. The
        // line is here so the log does not claim a hold that did not happen.
        Err(e) => eprintln!("rime-agentd: session {id} could not be held ({e})"),
    }
}

/// Start a session again because the screen was unlocked.
///
/// Only ever called for a session this daemon's own lock watch stopped —
/// a session the user paused by hand is never held, so it is never resumed.
fn resume_session(daemon: &Arc<Daemon>, id: u32) {
    let Some(handle) = lookup(daemon, id) else {
        return;
    };
    let mut s = handle.lock().expect("session lock");
    if !s.info.is_live() {
        return;
    }
    match pty::signal_group(s.pgid, libc::SIGCONT) {
        Ok(()) => {
            s.info.paused = false;
            s.info.detail = None;
            registry::write_record(&s.info);
            eprintln!("rime-agentd: session {id} resumed — the screen was unlocked");
        }
        Err(e) => eprintln!("rime-agentd: session {id} could not be resumed ({e})"),
    }
}

/// One more line about a grant that has already ended, in both trails.
///
/// The state is `Ended` with the grant's own recorded reason, because the
/// grant is over — these events are about what happened to the SESSION
/// afterwards, and pretending the grant was still active while they were
/// written would put a second, wrong answer in the record.
fn grant_note(grant: &rime_agent_core::grant::SystemGrant, event: &str, now: u64) {
    use rime_agent_core::grant::GrantState;
    let state = grant
        .closed
        .map(|c| GrantState::Ended {
            why: c.why,
            at_ms: c.ms,
        })
        .unwrap_or(GrantState::Ended {
            why: rime_agent_core::grant::ClosureReason::Expired,
            at_ms: now,
        });
    rime_agent_core::grant::audit(&request::audit_log(), event, grant, &state);
}

fn spawn_signal_thread(daemon: Arc<Daemon>, socket: PathBuf) {
    std::thread::Builder::new()
        .name("rime-agentd-signal".into())
        .spawn(move || {
            let mut sig: libc::c_int = 0;
            // Safe: sigwait blocks until one of the masked signals arrives and
            // writes its number into an int we own.
            unsafe {
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::sigaddset(&mut set, libc::SIGTERM);
                libc::sigaddset(&mut set, libc::SIGINT);
                libc::sigaddset(&mut set, libc::SIGHUP);
                libc::sigwait(&set, &mut sig);
            }
            eprintln!("rime-agentd: signal {sig}, stopping sessions");
            shutdown(&daemon, &socket);
            std::process::exit(0);
        })
        .ok();
}

/// Stop every session and remove the socket.
fn shutdown(daemon: &Daemon, socket: &Path) {
    let handles = {
        let reg = daemon.registry.lock().expect("registry lock");
        reg.list()
    };
    for handle in handles {
        let mut s = handle.lock().expect("session lock");
        registry::terminate(&mut s);
        registry::write_record(&s.info);
    }
    let _ = std::fs::remove_file(socket);
}

/// Serve one control connection: newline-delimited JSON requests until the
/// client goes away, or until an `Attach` turns it into a raw PTY pipe.
fn serve(daemon: &Arc<Daemon>, stream: UnixStream) -> Result<()> {
    // Read the peer credentials ONCE, from the accepted socket, before any
    // request is parsed. The kernel filled them in at connect(2) and they
    // cannot change for the life of the connection — whereas anything read out
    // of a request line is whatever the client chose to send.
    // The peer credentials, plus whatever narrowing this connection latches
    // onto itself. The latch lives here, on the stack of the thread serving
    // one connection, so it dies with the socket: nothing persists it, and no
    // other connection can see it.
    let mut caller = privilege::Caller::new(peer::credentials(&stream));

    let mut reader = BufReader::new(stream.try_clone().context("cloning the connection")?);
    let mut writer = stream;

    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(());
        }
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }

        let request: Request = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                respond(
                    &mut writer,
                    &Response::error(ErrorKind::BadRequest, format!("unparseable request: {e}")),
                )?;
                continue;
            }
        };

        // Attach is the one verb that does not return to this loop: the
        // connection stops being a control channel and becomes the session's
        // terminal.
        if let Request::Attach {
            id,
            cols,
            rows,
            replay,
        } = request
        {
            return session::handle_attach(daemon, &caller, writer, reader, id, cols, rows, replay);
        }

        // And neither does `Receive`, for the same reason with the direction
        // reversed: the connection stops being a control channel and becomes a
        // byte sink that the file being handed over is written into. It is
        // matched here rather than in `dispatch` because `dispatch` cannot
        // reach the connection, and a verb whose payload is the rest of the
        // stream has to be answered by something that holds it.
        if let Request::Receive { id, name, len } = request {
            return inject::handle_receive(daemon, &caller, writer, reader, id, &name, len);
        }

        // `SaveFile` is the third: a sink for a file a phone shared, written
        // into Downloads rather than a session's inbox (contract §1.8).
        if let Request::SaveFile { name, len } = request {
            return desktop::handle_save(daemon, &caller, writer, reader, &name, len);
        }

        let response = dispatch(daemon, request, &mut caller);
        respond(&mut writer, &response)?;
    }
}

fn respond(writer: &mut UnixStream, response: &Response) -> Result<()> {
    let mut line = serde_json::to_string(response)?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush().ok();
    Ok(())
}

/// Handle every verb except `Attach`.
///
/// `caller` is the connection: the peer credentials the kernel reported at
/// `connect(2)`, and any origin the connection has narrowed itself to. It is
/// passed rather than looked up so that no handler can accidentally consult
/// the request for identity instead, and it is `&mut` for exactly one verb —
/// `DeclareOrigin`, which is the only thing that may change it.
fn dispatch(daemon: &Arc<Daemon>, request: Request, caller: &mut privilege::Caller) -> Response {
    match request {
        Request::Hello => {
            // Fresh, for the same reason a session start is: the default can
            // change under a running daemon.
            let cfg = Config::load();
            Response::Hello {
                version: PROTOCOL_VERSION,
                agents: adapter::ids().into_iter().map(|s| s.to_string()).collect(),
                default_agent: cfg.default_agent.clone(),
                features: rime_agent_core::protocol::FEATURES
                    .iter()
                    .map(|f| f.to_string())
                    .collect(),
            }
        }

        Request::Run(req) => match session::start(daemon, *req, caller) {
            Ok(info) => Response::Session(Box::new(info)),
            Err(e) => session::run_error(e),
        },

        Request::List => Response::Sessions {
            sessions: collect_sessions(daemon),
        },

        Request::Info { id } => match live_info(daemon, id) {
            Some(info) => Response::Session(Box::new(info)),
            None => match registry::historical_records()
                .into_iter()
                .find(|i| i.id == id)
            {
                Some(info) => Response::Session(Box::new(info)),
                None => no_such_session(id),
            },
        },

        Request::Attach { .. } => Response::error(
            ErrorKind::Internal,
            "attach is handled before dispatch and must never reach it",
        ),

        Request::Receive { .. } => Response::error(
            ErrorKind::Internal,
            "receive is handled before dispatch and must never reach it",
        ),

        Request::SaveFile { .. } => Response::error(
            ErrorKind::Internal,
            "save_file is handled before dispatch and must never reach it",
        ),

        Request::Resize { id, cols, rows } => {
            // Whose size this is (§1.7), from the connection's origin: a
            // phone's resize arrives on a connection of its own, not on its
            // attach, so the origin is the only thing that ties the two
            // together. Resolved before the session lock is taken, because
            // resolving it takes session locks of its own.
            let viewer = privilege::viewer(&privilege::origin(daemon, caller));
            let Some(handle) = lookup(daemon, id) else {
                return no_such_session(id);
            };
            let mut s = handle.lock().expect("session lock");
            if !s.info.is_live() {
                return Response::error(
                    ErrorKind::SessionExited,
                    format!("session {id} has already exited"),
                );
            }
            let size = rime_agent_core::term::WinSize { cols, rows }.or_fallback();
            // `Ok` whether or not the PTY took it. A phone's resize that lands
            // after the phone's terminal closed is remembered and not applied
            // — applying it would shrink the desktop with nothing left to give
            // its size back — and that is the runtime deciding, not a failure
            // the phone could act on.
            match s.resize_from(viewer, size) {
                Ok(_) => Response::Ok,
                Err(e) => Response::error(ErrorKind::Internal, e.to_string()),
            }
        }

        Request::Inject { id, source } => inject::handle(daemon, caller, id, &source),
        Request::Input { id, data, submit } => {
            // A session may not type into a sibling.
            //
            // This is the only verb on the socket that acts on a session other
            // than the caller's own AND has an effect the target cannot tell
            // from a person at the keyboard. `Signal` acts on another session
            // too, but a signal is visible to the agent as a signal; text
            // arriving on the PTY is indistinguishable from typing, so an
            // agent that could send it could instruct another agent and
            // borrow its permissions. Hooks run inside the sandbox and reach
            // this socket, so the caller has to be established rather than
            // assumed.
            //
            // Resolved from the connection's peer credentials by the same
            // ancestry walk the privilege verbs use, never from the request:
            // `$RIME_AGENT_SESSION` lives inside a sandbox the agent controls.
            // A connection that is not inside any session — the shell, or a
            // person in an ordinary terminal — is what this verb is for.
            let who = privilege::origin(daemon, caller);
            if let Some(refusal) = privilege::refuse_input(&who, id) {
                return refusal;
            }
            let Some(handle) = lookup(daemon, id) else {
                return no_such_session(id);
            };
            // With `submit`, Enter goes as its own write a SUBMIT_GAP later
            // (§1.2); this connection's thread sleeps through the gap, which
            // costs nobody else anything — every connection has its own.
            let written = if submit {
                session::submit_input(&handle, data.as_bytes())
            } else {
                session::write_input(&handle, data.as_bytes())
            };
            match written {
                session::Input::Written => Response::Ok,
                session::Input::Exited => Response::error(
                    ErrorKind::SessionExited,
                    format!("session {id} has already exited"),
                ),
                session::Input::Failed(e) => Response::error(ErrorKind::Internal, e),
            }
        }

        // The one verb here that carries something out of this machine which
        // no session produced. It takes no `id` — one seat, one clipboard —
        // so there is nothing to look up, and the whole of the decision is
        // who is asking. See `clipboard.rs`.
        Request::Clipboard => clipboard::handle(daemon, caller),
        Request::ClipboardWrite { text } => desktop::handle_clipboard_write(daemon, caller, &text),

        // §1.5. Only a person names a session: `refuse_input`'s callers, with
        // a sentence of its own. The name is checked after the caller and
        // before the lookup, so a bad name is `bad_request` whether or not
        // the session exists, and a session never learns it was asked.
        Request::Rename { id, name } => {
            let who = privilege::origin(daemon, caller);
            if let Some(refusal) = privilege::refuse_rename(&who, id) {
                return refusal;
            }
            let name = match rime_agent_core::session::session_name_opt(name.as_deref()) {
                Ok(name) => name,
                Err(why) => return Response::error(ErrorKind::BadRequest, why),
            };
            let Some(handle) = lookup(daemon, id) else {
                return no_such_session(id);
            };
            match session::rename(&handle, name) {
                Response::Session(mut info) => {
                    add_process_children(std::slice::from_mut(&mut *info));
                    Response::Session(info)
                }
                other => other,
            }
        }

        // §1.6. The tail of a session's screen, without attaching: no resize,
        // no attach, `attached` unchanged. Agents do not read each other's
        // screens, so `refuse_input`'s callers are refused here too.
        Request::Peek { id, bytes } => {
            let who = privilege::origin(daemon, caller);
            if let Some(refusal) = privilege::refuse_peek(&who, id) {
                return refusal;
            }
            let Some(handle) = lookup(daemon, id) else {
                return no_such_session(id);
            };
            session::peek(&handle, bytes)
        }

        Request::Signal { id, signal } => {
            let Some(number) = rime_agent_core::session::signal_number(&signal) else {
                return Response::error(
                    ErrorKind::BadRequest,
                    format!("unknown signal {signal:?}; use int, term, kill, stop or cont"),
                );
            };
            let Some(handle) = lookup(daemon, id) else {
                return no_such_session(id);
            };
            let mut s = handle.lock().expect("session lock");
            if !s.info.is_live() {
                return Response::error(
                    ErrorKind::SessionExited,
                    format!("session {id} has already exited"),
                );
            }
            match pty::signal_group(s.pgid, number) {
                Ok(()) => {
                    // A stopped session is neither working nor waiting on the
                    // user, and nothing will produce output to correct the
                    // state later, so record it now. Recorded AFTER the signal
                    // succeeded: a flag set before the kill would claim a
                    // session was paused when the signal failed.
                    if number == libc::SIGSTOP {
                        s.info.paused = true;
                        s.info.detail = Some("paused".to_string());
                    } else if number == libc::SIGCONT {
                        s.info.paused = false;
                        s.info.detail = None;
                    }
                    Response::Ok
                }
                Err(e) => Response::error(ErrorKind::Internal, e.to_string()),
            }
        }

        Request::Event {
            id,
            state,
            event,
            detail,
            native,
            agent_id,
            agent_type,
            test,
        } => {
            // An event that names neither is not a smaller event, it is a
            // request that says nothing. Refused rather than recorded, because
            // a silent no-op here would look identical to a working hook.
            if state.is_none() && event.is_none() {
                return Response::error(
                    ErrorKind::BadRequest,
                    "an event must carry a state, a lifecycle event, or both".to_string(),
                );
            }
            let parsed = match state.as_deref() {
                None => None,
                Some(s) => match rime_agent_core::protocol::AgentState::parse(s) {
                    Some(p) => Some(p),
                    None => {
                        return Response::error(
                            ErrorKind::BadRequest,
                            format!(
                                "unknown state {s:?}; expected one of \
                                 working, waiting_for_user, permission_request, complete, failed"
                            ),
                        )
                    }
                },
            };
            let lifecycle = match event.as_deref() {
                None => None,
                Some(e) => match rime_agent_core::hook::HookEvent::parse(e) {
                    Some(p) => Some(p),
                    None => {
                        return Response::error(
                            ErrorKind::BadRequest,
                            format!(
                                "unknown lifecycle event {e:?}; expected one of {}",
                                rime_agent_core::hook::HookEvent::ALL
                                    .iter()
                                    .map(|e| e.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        )
                    }
                },
            };
            let Some(handle) = lookup(daemon, id) else {
                return no_such_session(id);
            };
            let mut s = handle.lock().expect("session lock");
            // §4.1's third criterion: the agent's own permission mode is what
            // dimension 1 actually is, and `policy.native` says only what Rime
            // did about it — which for the default is nothing. Recorded on
            // every event that carries one, because Claude's mode can change
            // mid-session (a `/permissions` switch, a plan-mode exit) and a
            // value captured once at start would go stale silently.
            //
            // Nothing branches on it. It is the agent describing itself, in
            // exactly the class `detail` is in.
            if let Some(mode) = native {
                if s.info.native_observed.as_deref() != Some(mode.as_str()) {
                    s.info.native_observed = Some(mode);
                }
            }
            // The tool flag first: a `stop` publishes `waiting_for_user` and
            // also ends any tool call the last `pre_tool_use` claimed, and the
            // order matters only in that both must happen.
            if let Some(e) = lifecycle {
                s.apply_tool_transition(e.tool_transition());
                s.apply_graph_event(e, agent_id.as_deref(), agent_type.as_deref());
            }
            match parsed {
                Some(p) => s.set_state(p, detail),
                // No state: the event is a fact worth recording — a task, a
                // compaction — and the session goes on doing whatever it was.
                // `last_activity` still moves, because the session demonstrably
                // is not idle.
                None => {
                    if detail.is_some() {
                        s.info.detail = detail;
                    }
                    s.info.last_activity = registry::now_secs();
                }
            }
            registry::write_record(&s.info);

            // §P1-036. The session lock is released BEFORE the observation
            // store is touched. Two locks held at once, in this order here and
            // the opposite order anywhere else, is how this daemon would
            // deadlock — so it never holds both.
            //
            // The worktree comes from the session's OWN recorded `cwd`, never
            // from the request: a session cannot report a test run against a
            // tree it does not live in.
            let cwd = s.info.cwd.clone();
            drop(s);
            if let Some(note) = test {
                daemon.tests.record(Path::new(&cwd), &note);
            }
            Response::Ok
        }

        Request::Telemetry { id, telemetry } => {
            let Some(handle) = lookup(daemon, id) else {
                return no_such_session(id);
            };
            let mut s = handle.lock().expect("session lock");
            // `last_activity` is deliberately NOT touched. A status line runs
            // on a timer, so treating it as activity would keep every idle
            // session looking busy — and `session::next_state`'s idle rule,
            // which decides `waiting_for_user`, reads exactly that field. The
            // session is described here, not observed doing anything.
            s.info.telemetry = Some(*telemetry);
            registry::write_record(&s.info);
            Response::Ok
        }

        Request::Worktrees { project } => {
            // A snapshot, and the registry lock is released before any git
            // command runs: enumerating worktrees and probing merges takes
            // long enough that holding it across the work would block every
            // other request — including the event another session is waiting
            // on to publish its own state.
            let sessions: Vec<worktrees::SessionWhere> = {
                let reg = daemon.registry.lock().expect("registry lock");
                reg.list()
                    .iter()
                    .filter_map(|h| {
                        let s = h.lock().ok()?;
                        Some(worktrees::SessionWhere {
                            id: s.info.id,
                            cwd: s.info.cwd.clone(),
                        })
                    })
                    .collect()
            };
            worktrees::handle(project, &daemon.tests, &sessions)
        }

        Request::Projects => {
            // No origin check, and the same one `Worktrees` does not have.
            // That verb already answers a caller with the absolute root of
            // every remembered project and the path of every worktree under
            // it; this answers with the roots and nothing git-derived, so
            // gating it would refuse a strictly smaller disclosure than the
            // one beside it allows — a gate that reads as security and buys
            // none.
            //
            // The registry is NOT touched. Joining sessions to projects is the
            // client's to do from `SessionInfo::project`, which is the project
            // root verbatim, so there is nothing to hold a lock for.
            Response::Projects {
                projects: project::list(),
            }
        }

        Request::Profiles => {
            // `paths::home()`, not `$HOME` read here: the daemon may have been
            // started by `systemd --user` without a full login environment,
            // and `paths::home` falls back to the passwd database for exactly
            // that case. A profile listing that reported nothing installed
            // because the environment was thin would be read as a broken
            // install.
            Response::Profiles {
                profiles: profile::summarise_adapters(&paths::home()),
            }
        }

        Request::ToolCheck {
            id,
            tool_name,
            tool_input,
        } => {
            // Every failure below answers "no opinion", never "deny". A policy
            // point that refused when it could not decide would stop an agent
            // for reasons nobody could state, and the layer that actually
            // enforces this — the sandbox the session is already inside — has
            // not gone anywhere.
            let Some(handle) = lookup(daemon, id) else {
                return Response::ToolDecision { deny: None };
            };
            let s = handle.lock().expect("session lock");
            let Some(confinement) = s.confinement.as_ref() else {
                return Response::ToolDecision { deny: None };
            };
            let payload = rime_agent_core::hook::Payload {
                tool_name: Some(tool_name),
                tool_input,
                ..Default::default()
            };
            let decision =
                rime_agent_core::hook::decide(&payload, &confinement.spec, &confinement.allowlist);
            let deny = match decision {
                rime_agent_core::hook::Decision::Allow => None,
                rime_agent_core::hook::Decision::Deny { reason, .. } => Some(reason),
            };
            Response::ToolDecision { deny }
        }

        Request::Logs { id, bytes } => {
            // Bound the request so a client cannot ask the daemon to read a
            // 32 MiB transcript into memory by accident.
            let bytes = bytes.min(registry::LOG_LIMIT_BYTES as usize);
            match registry::read_log(id, bytes) {
                Ok(text) => Response::Logs { id, text },
                Err(e) => Response::error(ErrorKind::NoSuchSession, e.to_string()),
            }
        }

        Request::Remove { id } => {
            let handle = lookup(daemon, id);
            if let Some(handle) = handle {
                let live = {
                    let s = handle.lock().expect("session lock");
                    s.info.is_live()
                };
                if live {
                    return Response::error(
                        ErrorKind::BadRequest,
                        format!("session {id} is still running; stop it first with `rime agent kill {id}`"),
                    );
                }
                daemon.registry.lock().expect("registry lock").remove(id);
            }
            registry::forget_record(id);
            Response::Ok
        }

        Request::DeclareOrigin { origin, actor } => {
            privilege::declare(daemon, caller, &origin, actor)
        }

        Request::Prune => {
            let handles = daemon.registry.lock().expect("registry lock").list();
            let mut removed = Vec::new();
            for handle in handles {
                let s = handle.lock().expect("session lock");
                if !s.info.is_live() {
                    removed.push(s.info.id);
                }
            }
            {
                let mut reg = daemon.registry.lock().expect("registry lock");
                for id in &removed {
                    reg.remove(*id);
                }
            }
            for info in registry::historical_records() {
                if info.state.is_terminal() {
                    registry::forget_record(info.id);
                }
            }
            for id in removed {
                registry::forget_record(id);
            }
            Response::Ok
        }

        // ── privilege requests ──────────────────────────────────────────────
        // Every one of these takes `caller` and none of them takes a session id
        // from the wire.
        Request::PrivilegeRequest { verb, args, reason } => {
            privilege::file(daemon, caller, &verb, &args, &reason)
        }

        Request::Requests => privilege::list(),

        Request::Decide { id, decision } => match request::Decision::parse(&decision) {
            Some(d) => privilege::decide(daemon, caller, id, d),
            None => Response::error(
                ErrorKind::BadRequest,
                format!("'{decision}' is not a decision; use once, project or deny"),
            ),
        },

        Request::RequestExecuted { id, exit_code } => privilege::executed(id, exit_code),

        Request::Grants => privilege::grants(),

        Request::Revoke { project, key } => {
            privilege::revoke(daemon, caller, &project, key.as_deref())
        }

        // ── system-access grants ────────────────────────────────────────────
        Request::SystemGrants => privilege::system_grants(daemon),

        Request::RevokeSystemGrant { id } => privilege::revoke_system_grant(daemon, caller, id),

        Request::RenewSystemGrant { id, ttl_ms, second_factor } => {
            privilege::renew_system_grant(daemon, caller, id, ttl_ms, second_factor.as_ref())
        }

        // ── the secret broker ───────────────────────────────────────────────
        Request::SecretUse {
            service,
            operation,
            resource,
            params,
            body,
            project,
        } => broker::use_capability(
            daemon,
            caller,
            &service,
            &operation,
            &resource,
            &params,
            body.as_deref(),
            project.as_deref(),
        ),

        // §7's remote elevation. Issuing a challenge grants nothing and gates
        // on nothing — see `elevation`'s module comment for why the gate is
        // not consulted here.
        Request::ElevationChallenge {
            session,
            kind,
            ttl_ms,
            credential,
        } => elevation::challenge(daemon, session, kind, ttl_ms, credential.as_deref()),

        // Granting is NOT here. A grant changes what is allowed, and
        // `rime-secretd` refuses one from any caller inside a session —
        // which covers a session that skips this daemon and opens the
        // socket itself, something a check here could not see.
    }
}

fn lookup(daemon: &Arc<Daemon>, id: u32) -> Option<registry::Handle> {
    daemon.registry.lock().expect("registry lock").get(id)
}

fn no_such_session(id: u32) -> Response {
    Response::error(ErrorKind::NoSuchSession, format!("no session {id}"))
}

fn live_info(daemon: &Arc<Daemon>, id: u32) -> Option<SessionInfo> {
    let handle = lookup(daemon, id)?;
    let mut info = handle.lock().expect("session lock").info.clone();
    add_process_children(std::slice::from_mut(&mut info));
    Some(info)
}

/// Where the process table is read from. A constant so the one place that is
/// not the fixture-driven parser is named rather than spelled inline twice.
const PROC: &str = "/proc";

/// Add each live session's forked processes to the copy about to be sent out.
///
/// Read time, not event time, and never written to the record. A subagent is
/// history — it happened, and the record is the only evidence — but a process
/// is a thing that either exists right now or does not, and the kernel is
/// already keeping that list. Persisting it would mean writing the session
/// record every time a compiler started, and answering "is it still running?"
/// from a file rather than from `/proc`.
///
/// This is also what §P1-020 means by MCP servers and tool processes being
/// representable: they publish nothing, and they do not have to. A confined
/// session is inside its own pid namespace, but the daemon is outside it and
/// the host `/proc` still lists every descendant, so the tree is readable for
/// every adapter.
fn add_process_children(infos: &mut [SessionInfo]) {
    if !infos.iter().any(|i| i.is_live()) {
        return;
    }
    let procs = rime_agent_core::graph::read_processes(Path::new(PROC));
    for info in infos.iter_mut() {
        if !info.is_live() {
            continue;
        }
        let mut tree = rime_agent_core::graph::process_tree(&procs, info.pid);
        rime_agent_core::graph::fill_rss(Path::new(PROC), &mut tree);
        info.children.append(&mut tree);
    }
}

/// Live sessions plus persisted records for ones this daemon no longer owns.
fn collect_sessions(daemon: &Arc<Daemon>) -> Vec<SessionInfo> {
    let live: Vec<SessionInfo> = daemon
        .registry
        .lock()
        .expect("registry lock")
        .list()
        .into_iter()
        .map(|h| h.lock().expect("session lock").info.clone())
        .collect();

    let mut out = live;
    let known: std::collections::HashSet<u32> = out.iter().map(|i| i.id).collect();
    for info in registry::historical_records() {
        if !known.contains(&info.id) {
            out.push(info);
        }
    }
    out.sort_by_key(|i| i.id);
    add_process_children(&mut out);
    out
}

/// The one lock for every test in this crate that redirects an XDG variable.
///
/// `set_var` is process-global and `cargo test` runs targets in threads, so a
/// test that points `XDG_STATE_HOME` at a temporary directory is changing what
/// every other thread sees. `grants.rs` had a lock for that and so did
/// `lock_tests`, and two locks over one variable serialise nothing: a
/// `grants::tests` tempdir dropping between this module's `issue` and its
/// `grant::load` restored the real `XDG_STATE_HOME` mid-test, and the grant
/// record was looked for in a directory it had never been written to. It
/// failed only under the full suite, never alone, which is the signature.
///
/// One lock, at the crate root, where both can reach it.
#[cfg(test)]
pub mod test_env {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    pub fn lock() -> MutexGuard<'static, ()> {
        static L: OnceLock<Mutex<()>> = OnceLock::new();
        L.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod lock_tests {
    //! §7's lock rules, at the point where a decision becomes an effect
    //! (P0-015).
    //!
    //! `rime_agent_core::lock` already asserts [`LockWatch::step`] over 8
    //! policies by 7 origins by 4 states. `step` returns a list and touches
    //! nothing, so every one of those tests would still pass if the daemon
    //! ignored the list — and until this module nothing anywhere called
    //! [`lock_tick`], which is the function that performs it. P0-015's second
    //! criterion is "short-lived root grants revoke on lock by default", and
    //! the only honest subject for it is a real grant in a real
    //! [`GrantAuthority`] that is gone afterwards.
    //!
    //! The screen is faked and nothing else is. A test that locked the screen
    //! would need a compositor and a person, and [`lock_tick`] takes
    //! `&mut dyn LockObserver` precisely so the one thing that cannot be
    //! driven from a test is the one thing replaced.

    use std::sync::Arc;

    use rime_agent_core::auth::{AuthError, Authenticator, ProcessSubject, Verdict};
    use rime_agent_core::grant::GrantKind;
    use rime_agent_core::lock::{LockObserver, LockState, LockWatch};
    use rime_agent_core::policy::RequestOrigin;

    use super::*;

    /// An authenticator that authorises, so a grant can exist to be revoked.
    ///
    /// The opposite choice from `privilege.rs`'s `CountingRefusal`, and for a
    /// reason: there the question is whether polkit was asked at all, here it
    /// is what happens to a grant that already exists. Nothing in this module
    /// asserts anything about authentication.
    struct AlwaysAuthorises;

    impl Authenticator for AlwaysAuthorises {
        fn check(&self, _a: &str, _s: &ProcessSubject) -> Result<Verdict, AuthError> {
            Ok(Verdict::Authorized)
        }
    }

    /// A screen whose state the test sets.
    struct FakeScreen(LockState);

    impl LockObserver for FakeScreen {
        fn observe(&mut self) -> LockState {
            self.0.clone()
        }
    }

    /// `XDG_STATE_HOME` and `XDG_CONFIG_HOME` pointed at a fresh directory.
    ///
    /// Both, not just state: `lock_tick` re-reads the policy with
    /// `Config::load()` on every tick, so a developer's own
    /// `~/.config/rime/agent.toml` with `revoke_root_grants = false` in it
    /// would turn the central assertion here into one that passes for the
    /// wrong reason — or fails on their machine and nowhere else.
    struct Sandbox {
        path: std::path::PathBuf,
        previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl Sandbox {
        fn new() -> Sandbox {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let guard = crate::test_env::lock();
            let path = std::env::temp_dir().join(format!(
                "rime-lock-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::remove_dir_all(&path).ok();
            std::fs::create_dir_all(&path).expect("mkdir");
            let mut previous = Vec::new();
            for name in ["XDG_STATE_HOME", "XDG_CONFIG_HOME"] {
                previous.push((name, std::env::var_os(name)));
                // Safe: the lock above serialises every test that touches it.
                unsafe { std::env::set_var(name, &path) };
            }
            Sandbox {
                path,
                previous,
                _guard: guard,
            }
        }

        /// Write the `agent.json` carrying `lock` settings, at the path
        /// `rime agent lock` writes and `Config::load` reads.
        ///
        /// Only the `lock` key, deliberately: `LockPolicy` is
        /// `#[serde(default)]` on the container as well as per field, so this
        /// also asserts that setting one lock rule does not silently reset the
        /// other two — which is the failure that comment in `lock.rs` exists
        /// to prevent.
        fn with_lock_policy(&self, json: &str) {
            let dir = self.path.join("rime");
            std::fs::create_dir_all(&dir).expect("mkdir config");
            std::fs::write(dir.join("agent.json"), json).expect("write config");
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            for (name, value) in &self.previous {
                // Safe: same lock, still held.
                unsafe {
                    match value {
                        Some(v) => std::env::set_var(name, v),
                        None => std::env::remove_var(name),
                    }
                }
            }
            std::fs::remove_dir_all(&self.path).ok();
        }
    }

    fn daemon() -> Arc<Daemon> {
        Arc::new(Daemon {
            registry: Mutex::new(Registry::new()),
            config: Mutex::new(Config::default()),
            grants: grants::GrantAuthority::new(),
            auth: Box::new(AlwaysAuthorises),
            tests: worktrees::TestObservations::new(),
            challenges: Mutex::new(rime_agent_core::webauthn::ChallengeStore::new()),
        })
    }

    /// This process, as a peer polkit can be asked about.
    fn me() -> crate::peer::Peer {
        crate::peer::Peer {
            pid: std::process::id() as libc::pid_t,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
        }
    }

    /// Issue a real grant of `kind` for `session`, the way `session::start`
    /// does — through `authenticate`, so the proof is one this crate minted
    /// rather than a value a test conjured.
    fn issue(daemon: &Arc<Daemon>, kind: GrantKind, session: u32, now: u64) -> u32 {
        let proof = grants::authenticate(daemon.auth.as_ref(), kind, &me())
            .expect("the stub authorises");
        let caps = rime_agent_core::grant::capabilities_for(kind, None).expect("issuable");
        daemon
            .grants
            .issue(
                proof,
                kind,
                session,
                "claude",
                None,
                900_000,
                caps,
                RequestOrigin::LocalTerminal,
                now,
            )
            .id
    }

    /// A real child in its own process group, in the registry, live.
    ///
    /// `hold_session` sends SIGSTOP to a process GROUP, so a fake pgid would
    /// either signal nothing or signal this test runner. `process_group(0)`
    /// makes the child its own leader, so its pgid is its pid and the signal
    /// reaches exactly one process that exists to receive it.
    fn live_session(
        d: &Arc<Daemon>,
        id: u32,
        origin: Option<RequestOrigin>,
    ) -> (libc::pid_t, std::process::Child) {
        use std::os::unix::process::CommandExt;
        let child = unsafe {
            std::process::Command::new("sleep")
                .arg("60")
                .pre_exec(|| {
                    // Its own process group. `libc` rather than
                    // `CommandExt::process_group`, which is the same call and
                    // is already in use elsewhere in this repository.
                    if libc::setpgid(0, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                })
                .spawn()
        }
        .expect("spawn sleep");
        let pid = child.id() as libc::pid_t;
        let info = rime_agent_core::protocol::SessionInfo {
            id,
            agent: "generic".into(),
            program: "sleep".into(),
            args: vec![],
            cwd: "/tmp".into(),
            project: None,
            project_name: None,
            name: None,
            title: None,
            worktree: None,
            state: rime_agent_core::protocol::AgentState::Working,
            detail: None,
            paused: false,
            policy: rime_agent_core::policy::AgentPolicy::default(),
            allowlist: None,
            request_origin: origin,
            origin_source: None,
            actor: None,
            grant: None,
            grant_expires_ms: None,
            native_observed: None,
            telemetry: None,
            children: Vec::new(),
            pid,
            started: 0,
            last_activity: 0,
            // Live, which is what `hold_session` requires. Written out rather
            // than defaulted so the one field the whole test depends on is
            // visible in it.
            exit_code: None,
            exit_signal: None,
            attached: 0,
            checkpoint: None,
            cols: 80,
            rows: 24,
            injected: 0,
            capsule: None,
        };
        d.registry
            .lock()
            .expect("registry lock")
            .insert(info, -1, pid, pid);
        (pid, child)
    }

    /// The kernel's own word for what a process is doing, from
    /// `/proc/<pid>/stat`. `T` is stopped.
    ///
    /// Read from /proc rather than inferred from `info.paused`, which is the
    /// daemon's own bookkeeping: a `paused` flag set beside a signal that never
    /// arrived is exactly the failure this is here to catch.
    fn proc_state(pid: libc::pid_t) -> char {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        // The comm field can contain spaces and parentheses, so the state is
        // the first field after the LAST ')'.
        stat.rfind(')')
            .and_then(|i| stat[i + 1..].split_whitespace().next())
            .and_then(|f| f.chars().next())
            .unwrap_or('?')
    }

    /// Wait for a process to reach `want`, up to two seconds.
    ///
    /// A signal is delivered asynchronously; asserting immediately after
    /// `lock_tick` returns would be a race that passes on a fast machine.
    fn settles_to(pid: libc::pid_t, want: char) -> char {
        let mut last = '?';
        for _ in 0..200 {
            last = proc_state(pid);
            if last == want {
                return last;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        last
    }

    #[test]
    fn a_remote_control_session_is_actually_stopped_by_a_lock_and_started_again_by_an_unlock() {
        // P0-015 criterion 1, at the same depth criterion 2 is asserted at.
        // §7: "ordinary agents may continue; Remote Control may continue IF
        // CONFIGURED" — so with nothing configured a Remote Control session is
        // held, and `LockPolicy::remote_control_continues` defaults to false
        // for exactly that reading.
        //
        // The assertion is on /proc, not on `actions.hold` and not on
        // `info.paused`: `step` already has tests for the decision, and a
        // `paused` flag is the daemon agreeing with itself. What had never
        // been asserted is that a process actually stops.
        let sb = Sandbox::new();
        let d = daemon();
        let (pid, mut child) = live_session(&d, 11, Some(RequestOrigin::RemoteControl));
        let mut watch = LockWatch::new();
        let mut ending = Vec::new();
        let now = 10_000;

        lock_tick(&d, &mut watch, &mut FakeScreen(LockState::Unlocked), &mut ending, now);
        assert_ne!(proc_state(pid), 'T', "an unlocked screen stopped a session");

        lock_tick(&d, &mut watch, &mut FakeScreen(LockState::Locked), &mut ending, now + 1);
        let state = settles_to(pid, 'T');
        assert_eq!(state, 'T', "the screen locked and the session is still running");
        let paused = d
            .registry
            .lock()
            .expect("registry lock")
            .list()
            .into_iter()
            .any(|h| {
                let s = h.lock().expect("session lock");
                s.info.id == 11 && s.info.paused
            });
        assert!(paused, "the process stopped but the record does not say so");

        // And an unlock starts it again. A hold that could not be lifted would
        // be a lock policy that ends every Remote Control session permanently.
        lock_tick(&d, &mut watch, &mut FakeScreen(LockState::Unlocked), &mut ending, now + 2);
        let state = settles_to(pid, 'S');
        assert_ne!(state, 'T', "the screen unlocked and the session is still stopped");

        // SIGKILL, not SIGTERM: a stopped process does not act on SIGTERM
        // until it is continued, and this one may have been left stopped if
        // the assertions above failed.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        let _ = child.wait();
        drop(sb);
    }

    #[test]
    fn remote_control_keeps_running_across_a_lock_when_the_owner_configured_it() {
        // "may continue IF CONFIGURED" — the other half, and what makes the
        // test above about the DEFAULT rather than about `hold_session` being
        // the only thing `lock_tick` can do. This is Andre's own setting:
        // `rime agent lock --remote continue`.
        let sb = Sandbox::new();
        sb.with_lock_policy(r#"{"lock": {"remote_control_continues": true}}"#);
        let d = daemon();
        let (pid, mut child) = live_session(&d, 12, Some(RequestOrigin::RemoteControl));
        let mut watch = LockWatch::new();
        let mut ending = Vec::new();
        let now = 10_000;

        lock_tick(&d, &mut watch, &mut FakeScreen(LockState::Unlocked), &mut ending, now);
        lock_tick(&d, &mut watch, &mut FakeScreen(LockState::Locked), &mut ending, now + 1);
        // Give a wrong implementation the same 2s the other test gives a right
        // one, so this is not passing merely by being read too early.
        let state = settles_to(pid, 'T');
        assert_ne!(state, 'T', "`--remote continue` was configured and the session was stopped anyway");

        unsafe { libc::kill(pid, libc::SIGKILL) };
        let _ = child.wait();
        drop(sb);
    }

    #[test]
    fn a_root_grant_is_gone_after_the_screen_locks_and_the_default_is_what_does_it() {
        // P0-015 criterion 2. Asserted on `active_for` — the authority's own
        // answer to "is this session elevated right now" — and not on
        // `actions.revoke`, which is a list `step` already has tests for and
        // which the daemon could build correctly and then ignore.
        let sb = Sandbox::new();
        // Deliberately NO config file: the default is the subject. §7 says
        // "should default to revocation", and a test that had to switch the
        // behaviour on would be testing a setting rather than a default.
        let d = daemon();
        let now = 10_000;
        let id = issue(&d, GrantKind::SystemAccess, 7, now);
        assert!(d.grants.active_for(7, now).is_some(), "the grant did not take");

        let mut watch = LockWatch::new();
        let mut ending = Vec::new();

        // Unlocked first, so the locked tick is a TRANSITION. §7 words the
        // rule as an event, and `step` fires revocation on the edge.
        lock_tick(
            &d,
            &mut watch,
            &mut FakeScreen(LockState::Unlocked),
            &mut ending,
            now,
        );
        assert!(
            d.grants.active_for(7, now).is_some(),
            "an unlocked screen revoked a grant"
        );

        lock_tick(
            &d,
            &mut watch,
            &mut FakeScreen(LockState::Locked),
            &mut ending,
            now + 1,
        );
        assert!(
            d.grants.active_for(7, now + 1).is_none(),
            "the screen locked and grant {id} is still in force"
        );
        // And it is recorded as revoked rather than merely dropped, because
        // `rime agent grants` has to be able to say why it ended.
        let stored = rime_agent_core::grant::load(&rime_agent_core::grant::grants_dir(), id)
            .expect("readable")
            .expect("the record survives the revocation");
        assert_eq!(
            stored.closed.map(|c| c.why),
            Some(rime_agent_core::grant::ClosureReason::Revoked)
        );
        drop(sb);
    }

    #[test]
    fn the_owner_can_keep_root_grants_across_a_lock_and_the_setting_is_read_per_tick() {
        // P0-015 criterion 1's other half: "user policy may override". The
        // same transition as above with `rime agent lock --root-grants keep`
        // set, so the assertion above is about the DEFAULT and not about
        // `lock_tick` being unable to revoke anything.
        let sb = Sandbox::new();
        sb.with_lock_policy(r#"{"lock": {"revoke_root_grants": false}}"#);
        let d = daemon();
        let now = 10_000;
        issue(&d, GrantKind::SystemAccess, 7, now);

        let mut watch = LockWatch::new();
        let mut ending = Vec::new();
        lock_tick(
            &d,
            &mut watch,
            &mut FakeScreen(LockState::Unlocked),
            &mut ending,
            now,
        );
        lock_tick(
            &d,
            &mut watch,
            &mut FakeScreen(LockState::Locked),
            &mut ending,
            now + 1,
        );
        assert!(
            d.grants.active_for(7, now + 1).is_some(),
            "`--root-grants keep` was ignored and the grant was taken anyway"
        );
        drop(sb);
    }

    #[test]
    fn an_unreadable_screen_revokes_exactly_as_a_locked_one_does() {
        // The variant that exists so a lock policy does not fail open. A
        // machine whose lock state cannot be read is not a machine that is
        // unlocked, and this is the assertion that makes `treat_as_locked`
        // true of the daemon rather than only of the enum.
        let sb = Sandbox::new();
        let d = daemon();
        let now = 10_000;
        issue(&d, GrantKind::SystemAccess, 7, now);
        let mut watch = LockWatch::new();
        let mut ending = Vec::new();
        lock_tick(
            &d,
            &mut watch,
            &mut FakeScreen(LockState::Unlocked),
            &mut ending,
            now,
        );
        lock_tick(
            &d,
            &mut watch,
            &mut FakeScreen(LockState::Unreadable("loginctl is not on PATH".into())),
            &mut ending,
            now + 1,
        );
        assert!(
            d.grants.active_for(7, now + 1).is_none(),
            "a screen state that could not be read was treated as unlocked"
        );
        drop(sb);
    }

    #[test]
    fn a_headless_machine_keeps_its_grants_because_there_is_no_screen_to_lock() {
        // The other half of not failing open: `NoDisplaySession` is a
        // measurement, not a failure. A build server that revoked every grant
        // on every tick because it has no screen would be unusable, and the
        // distinction between "could not read" and "there is nothing to read"
        // is the whole reason `LockState` has four variants rather than being
        // a bool.
        let sb = Sandbox::new();
        let d = daemon();
        let now = 10_000;
        issue(&d, GrantKind::SystemAccess, 7, now);
        let mut watch = LockWatch::new();
        let mut ending = Vec::new();
        lock_tick(
            &d,
            &mut watch,
            &mut FakeScreen(LockState::NoDisplaySession),
            &mut ending,
            now,
        );
        assert!(
            d.grants.active_for(7, now).is_some(),
            "a machine with no screen revoked a grant"
        );
        drop(sb);
    }
}
