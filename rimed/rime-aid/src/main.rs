//! `rime-aid` — the Rime local inference service (roadmap §14).
//!
//! One per user, started by `systemd --user`, unprivileged. It owns:
//!
//! * `$XDG_RUNTIME_DIR/rime-ai/api.sock` — the endpoint applications and agent
//!   clients connect to. It carries the backend's own HTTP API unchanged, so
//!   anything that can talk to an OpenAI-compatible server can talk to this
//!   with no Rime-specific code.
//! * `$XDG_RUNTIME_DIR/rime-ai/control.sock` — line-framed JSON, for the
//!   questions no backend API answers: which model is loaded, on which compute
//!   backend, with how many layers offloaded, and how long until it unloads.
//! * the backend process, its lifetime, and the VRAM it holds.
//!
//! Both sockets are mode `0600` inside a `0700` directory, and every accepted
//! connection's `SO_PEERCRED` is checked anyway. There is no TCP listener and
//! there is no flag that adds one; `rimed_core::ai::refuse_tcp_endpoint`
//! carries the reason, and `rimed_core::ai::plan_launch` refuses to emit a
//! loopback address for the backend child either.
//!
//! ── Why this is a user service and not part of `rimed` ─────────────────────
//!
//! The full argument, with the alternative that was rejected and why, is in
//! `rimed_core::ai`'s module documentation. In one line: a process whose entire
//! job is turning the user's prompts into generated text must not be a
//! privileged daemon shared between accounts, and `AGENTS.md` forbids giving
//! anything like it a polkit action or a system-bus name. Nothing here talks to
//! `rimed`.
//!
//! ── Why it is not socket-activated ────────────────────────────────────────
//!
//! It could be — the API socket is exactly the shape `systemd`'s socket
//! activation wants, and nothing here holds a PTY the way `rime-agentd` does.
//! It is not, for one reason: **the daemon has to outlive its clients to be
//! useful.** Its whole job is keeping a multi-gigabyte model resident between
//! requests, and its own idle policy — measured against the machine's power
//! source — is what decides when to stop. Handing that decision to
//! `systemd`'s idle timer would mean two things could unload the model and
//! neither would know why the other did.
//!
//! It is NOT enabled by default, exactly as `rime-agentd` is not:
//!
//! ```text
//! systemctl --user enable --now rime-aid
//! ```
//!
//! `rime ai` prints that when the socket is absent.
//!
//! Threading: one accept thread per socket, one thread per connection, one
//! timer thread. Connections are few, the work is I/O, and a blocking
//! `io::copy` on a socket is what the kernel is best at — the same reasoning
//! `rime-agentd` records for the same choice.

mod paths;
mod peer;
mod relay;
mod runtime;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rimed_core::ai::{
    self, DeviceInfo, Endpoints, ErrorKind, IdleInputs, Request, Response, Settings, Status,
    Store, PROTOCOL_VERSION,
};
use rimed_core::gpu::RealNvidiaSmi;

use rimed_core::aiprobe::{self as probe, Roots};

/// How often the supervisor looks at the idle policy when no request is
/// waiting.
///
/// Five seconds. The timeout it enforces is measured in minutes, so the
/// resolution costs nothing, and a tighter loop on a laptop is exactly the
/// wakeup the battery timeout exists to avoid.
const IDLE_TICK: Duration = Duration::from_secs(5);

/// How long a connection waits for the supervisor to produce a backend.
///
/// Must exceed the supervisor's own readiness timeout, or a slow first load
/// would be reported as a client-side timeout while the load was still
/// succeeding — two different failures with one message.
const BACKEND_WAIT: Duration = Duration::from_secs(240);

/// What a connection thread asks the supervisor for: a ready backend socket,
/// or the reason there is not one.
type BackendReply = Sender<Result<std::path::PathBuf, String>>;

/// Everything the threads share.
struct Daemon {
    endpoints: Endpoints,
    /// Proof that the backend sandbox works, obtained before any socket
    /// existed. Every backend start needs it; see `runtime::Confinement`.
    confinement: runtime::Confinement,
    store: Store,
    roots: Roots,
    /// Bumped on every API connection and every relayed byte count, so the
    /// supervisor never has to take the big lock to know whether to bother.
    open: AtomicU32,
    /// How a connection thread asks the supervisor to start the backend.
    ///
    /// A channel rather than a direct call, and the reason is a kernel detail
    /// that cost a real debugging session on the katana: `bwrap
    /// --die-with-parent` uses `PR_SET_PDEATHSIG`, and Linux delivers that
    /// signal when the parent **thread** exits, not when the parent process
    /// does. Spawning the backend from a connection thread therefore had it
    /// SIGKILLed the instant that request finished — the generation succeeded,
    /// then the model unloaded itself, and the next request paid a full reload.
    /// A service whose entire purpose is keeping a model resident had a
    /// residency of one request.
    ///
    /// So exactly one thread ever creates the backend, and it never exits.
    /// That is also the right shape independently: the backend's lifetime
    /// should not be tied to whichever connection happened to trigger the load.
    spawn: Mutex<Sender<BackendReply>>,
    state: Mutex<State>,
}

/// The part that needs a lock.
struct State {
    /// Re-read from disk whenever it is consulted, so editing `ai.toml` takes
    /// effect without restarting the daemon. Cheap: one small TOML file.
    settings: Settings,
    /// The model the next API connection will load.
    selected: Option<String>,
    /// The backend, when one is resident.
    running: Option<runtime::Running>,
    /// When the last byte moved, or the daemon started.
    last_activity: Instant,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("rime-aid: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    // A client that goes away mid-relay must not kill the daemon. Every write
    // site handles the error.
    // Safe: setting a signal disposition before any thread exists.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };

    // No arguments. A daemon with an accidental `--listen` is precisely what
    // this design refuses, so an unrecognised argument is a refusal with the
    // reason rather than something ignored.
    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("rime-aid {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            // Recognised in order to be refused, the same way
            // `rimed_core::ai::Settings` declares the four TCP-shaped keys: a
            // bare "unknown argument" would read as "not implemented yet".
            other
                if other.starts_with("--listen")
                    || other.starts_with("--host")
                    || other.starts_with("--port") =>
            {
                anyhow::bail!("{}", ai::refuse_tcp_endpoint("listen"));
            }
            other => anyhow::bail!(
                "rime-aid takes no arguments; got {other:?}. \
                 Configuration is ~/.config/rime/ai.toml — see `rime ai status`"
            ),
        }
    }

    // FAIL CLOSED, and FIRST. The backend runs only inside bwrap, and when the
    // sandbox cannot be applied the daemon does not start at all: before this
    // it ran the backend unconfined and noted it in `rime ai status`. Checked
    // before the runtime directory or either socket exists, so a refusal
    // leaves nothing behind that a client could mistake for a service.
    let confinement = runtime::require_confinement()?;

    let endpoints = Endpoints::new(&paths::runtime_dir());
    paths::ensure_private_dir(endpoints.dir())?;

    let store = probe::store_from_env();
    let roots = Roots::from_env();

    let settings = load_settings();
    let (spawn_tx, spawn_rx) = mpsc::channel::<BackendReply>();
    let daemon = Arc::new(Daemon {
        endpoints: endpoints.clone(),
        confinement,
        store: store.clone(),
        roots,
        open: AtomicU32::new(0),
        spawn: Mutex::new(spawn_tx),
        state: Mutex::new(State {
            selected: settings.model.clone(),
            settings,
            running: None,
            last_activity: Instant::now(),
        }),
    });

    let control = bind(&endpoints.control())?;
    let api = bind(&endpoints.api())?;

    block_termination_signals();
    spawn_signal_thread(Arc::clone(&daemon));
    spawn_supervisor(Arc::clone(&daemon), spawn_rx)?;

    eprintln!(
        "rime-aid: api {} control {} store {} (backends confined with bwrap)",
        endpoints.api().display(),
        endpoints.control().display(),
        store.root().display(),
    );

    // The control listener runs on its own thread; this one serves the API.
    {
        let daemon = Arc::clone(&daemon);
        std::thread::Builder::new()
            .name("rime-aid-control".into())
            .spawn(move || accept_loop(&daemon, control, serve_control))
            .context("spawning the control accept thread")?;
    }
    accept_loop(&daemon, api, serve_api);
    Ok(())
}

fn print_help() {
    println!(
        "rime-aid — the Rime local inference service (one per user)\n\
         \n\
         It takes no arguments. Settings live in ~/.config/rime/ai.toml and the\n\
         model store is {}.\n\
         \n\
         Endpoints, under $XDG_RUNTIME_DIR/{}:\n\
         \x20 {}   the backend's HTTP API, relayed unchanged\n\
         \x20 {}   line-framed JSON: status, select, unload\n\
         \n\
         There is no TCP listener and no option that adds one. Use `rime ai` to\n\
         drive it and `rime ai status` to see what it decided.",
        ai::STORE_ROOT,
        ai::RUNTIME_SUBDIR,
        ai::API_SOCKET,
        ai::CONTROL_SOCKET
    );
}

/// Read `~/.config/rime/ai.toml`, or defaults.
///
/// An unreadable or invalid file is reported and then ignored, rather than
/// stopping the daemon. The alternative — refusing to start — would leave a
/// user with a typo in one line unable to use local inference at all, and the
/// refusal is already delivered where it can be acted on: `rime ai status`
/// re-reads the same file and prints the same error.
fn load_settings() -> Settings {
    let path = paths::settings_file();
    match std::fs::read_to_string(&path) {
        Ok(text) => match Settings::parse(&text) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("rime-aid: ignoring {}: {e}", path.display());
                Settings::default()
            }
        },
        Err(_) => Settings::default(),
    }
}

/// Bind a socket at `0600`, replacing a stale one.
///
/// A socket file left by a daemon that was killed cannot be bound over but also
/// cannot be connected to. Probing before unlinking is what stops this from
/// stealing the socket from a daemon that is genuinely running.
fn bind(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            anyhow::bail!(
                "another rime-aid is already listening on {}",
                path.display()
            );
        }
        std::fs::remove_file(path)
            .with_context(|| format!("removing the stale socket {}", path.display()))?;
    }
    let listener = UnixListener::bind(path)
        .with_context(|| format!("binding {}", path.display()))?;

    // 0600 explicitly, not by umask. The parent directory is already 0700, so
    // this is the second of two independent reasons no other account can open
    // it — and the peer-credential check is the third.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restricting {}", path.display()))?;
    Ok(listener)
}

/// Accept forever, handing each connection to `handler` on its own thread.
fn accept_loop(
    daemon: &Arc<Daemon>,
    listener: UnixListener,
    handler: fn(&Arc<Daemon>, UnixStream),
) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                // The credential check happens before the thread, so a
                // connection from another account costs nothing.
                if !peer::is_own_user(&stream) {
                    eprintln!("rime-aid: refusing a connection that is not this user's");
                    continue;
                }
                let daemon = Arc::clone(daemon);
                if let Err(e) = std::thread::Builder::new()
                    .name("rime-aid-conn".into())
                    .spawn(move || handler(&daemon, stream))
                {
                    eprintln!("rime-aid: cannot spawn a connection thread: {e}");
                }
            }
            Err(e) => eprintln!("rime-aid: accept failed: {e}"),
        }
    }
}

// ── the API endpoint ─────────────────────────────────────────────────────────

/// Serve one API connection: ensure a backend, then relay.
fn serve_api(daemon: &Arc<Daemon>, client: UnixStream) {
    // Counted BEFORE the backend is started, so the idle timer cannot unload
    // between a client connecting and the model finishing its load.
    daemon.open.fetch_add(1, Ordering::SeqCst);
    // Asked of the supervisor, never done here: see `Daemon::spawn`.
    let result = request_backend(daemon);
    match result {
        Ok(socket) => match UnixStream::connect(&socket) {
            Ok(backend) => match relay::duplex(client, backend) {
                Ok(moved) => {
                    if moved.total() > 0 {
                        touch(daemon);
                    }
                }
                Err(e) => eprintln!("rime-aid: relay ended: {e}"),
            },
            Err(e) => {
                eprintln!("rime-aid: cannot reach the backend at {}: {e}", socket.display());
            }
        },
        Err(e) => {
            // The client is speaking HTTP and expects HTTP, so the refusal is
            // an HTTP response rather than a closed socket — a bare close reads
            // as "connection reset" in every client and tells nobody anything.
            http_error(client, &e);
        }
    }
    daemon.open.fetch_sub(1, Ordering::SeqCst);
    touch(daemon);
}

/// Reply to an HTTP client that no backend could be started.
///
/// `503`, because the condition is temporary and fixable: install a runtime,
/// pull a model. The body is `text/plain` and carries the same message the CLI
/// would print, including the command to type.
fn http_error(mut client: UnixStream, message: &str) {
    let body = format!("rime-aid: {message}\n");
    let head = format!(
        "HTTP/1.1 503 Service Unavailable\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let _ = client.write_all(head.as_bytes());
    let _ = client.write_all(body.as_bytes());
    let _ = client.flush();
}

/// Record activity.
fn touch(daemon: &Arc<Daemon>) {
    if let Ok(mut s) = daemon.state.lock() {
        s.last_activity = Instant::now();
    }
}

/// Ask the supervisor for a ready backend socket.
///
/// Bounded: a supervisor that has wedged must produce an HTTP error rather than
/// a client that hangs forever.
fn request_backend(daemon: &Arc<Daemon>) -> Result<std::path::PathBuf, String> {
    let (tx, rx) = mpsc::channel();
    {
        let sender = daemon
            .spawn
            .lock()
            .map_err(|_| "the daemon's spawn channel is poisoned".to_string())?;
        sender
            .send(tx)
            .map_err(|_| "the backend supervisor has stopped".to_string())?;
    }
    match rx.recv_timeout(BACKEND_WAIT) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Err(format!(
            "the backend supervisor did not answer within {}s",
            BACKEND_WAIT.as_secs()
        )),
        Err(RecvTimeoutError::Disconnected) => {
            Err("the backend supervisor has stopped".to_string())
        }
    }
}

/// Make sure a backend is running for the selected model, and return its
/// socket.
///
/// **Called only from the supervisor thread.** That is not a style preference:
/// `bwrap --die-with-parent` is `PR_SET_PDEATHSIG`, which fires when the
/// creating THREAD exits, so a backend spawned from a connection thread is
/// killed the moment that request completes. See [`Daemon::spawn`].
///
/// Holds the lock for the whole start, deliberately: nothing else may observe a
/// half-started backend, and the state it mutates is the one `status` reads.
fn ensure_backend(daemon: &Arc<Daemon>) -> Result<std::path::PathBuf> {
    let mut state = daemon
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("the daemon's state lock is poisoned"))?;

    // Settings are re-read here as well as at startup, so a changed default
    // model or context takes effect on the next load rather than needing a
    // restart.
    state.settings = load_settings();

    if let Some(r) = state.running.as_mut() {
        if r.alive() {
            return Ok(r.socket.clone());
        }
        // It died. Drop it before starting another, so a crash loop does not
        // accumulate zombies.
        state.running = None;
    }

    let plan = build_plan(daemon, &state)?;
    let socket = daemon.endpoints.backend();
    let devices = runtime::device_nodes(plan.choice.backend);
    let exposed = runtime::Exposed {
        model: &plan.model_path,
        socket_dir: daemon.endpoints.dir(),
        devices,
    };
    let running = runtime::start(
        &daemon.confinement,
        &plan.launch,
        &plan.model_id,
        &socket,
        &exposed,
    )?;
    eprintln!(
        "rime-aid: {} loaded on {} ({}), confined",
        plan.model_id,
        plan.choice.backend,
        plan.launch.describe(),
    );
    state.running = Some(running);
    state.last_activity = Instant::now();
    Ok(socket)
}

/// Resolve one load, through the single shared resolver.
///
/// A thin wrapper on purpose: `rimed_core::aiprobe::resolve` is the ONE
/// implementation, and `rime ai status` calls exactly the same function. If the
/// daemon had its own, "which backend will this machine use" would have two
/// answers and the one printed would not be the one that ran.
fn build_plan(daemon: &Arc<Daemon>, state: &State) -> Result<probe::Resolved> {
    probe::resolve(
        &daemon.store,
        &daemon.roots,
        &state.settings,
        state.selected.as_deref(),
        &daemon.endpoints,
        &RealNvidiaSmi,
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

// ── the control endpoint ─────────────────────────────────────────────────────

/// Serve one control connection: newline-delimited JSON until the client goes
/// away.
fn serve_control(daemon: &Arc<Daemon>, stream: UnixStream) {
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(clone);
    let mut writer = stream;

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(line) {
            Ok(r) => dispatch(daemon, r),
            Err(e) => Response::error(
                ErrorKind::BadRequest,
                format!("unparseable request: {e}"),
            ),
        };
        let Ok(mut text) = serde_json::to_string(&response) else {
            return;
        };
        text.push('\n');
        if writer.write_all(text.as_bytes()).is_err() {
            return;
        }
        let _ = writer.flush();
    }
}

fn dispatch(daemon: &Arc<Daemon>, request: Request) -> Response {
    match request {
        Request::Hello => Response::Hello {
            version: PROTOCOL_VERSION,
            api_socket: daemon.endpoints.api().display().to_string(),
        },

        Request::Status => Response::Status(Box::new(status(daemon))),

        Request::Models => {
            let Ok(state) = daemon.state.lock() else {
                return Response::error(ErrorKind::Internal, "the state lock is poisoned");
            };
            let loaded = state.running.as_ref().map(|r| r.model.clone());
            match probe::model_infos(&daemon.store, state.selected.as_deref(), loaded.as_deref())
            {
                Ok(models) => Response::Models { models },
                // Never fall back to an empty list: that reads as "nothing is
                // installed" to a client with no way to tell the difference.
                Err(e) => Response::error(ErrorKind::Internal, e),
            }
        }

        Request::Select { model } => {
            if let Err(e) = ai::validate_model_id(&model) {
                return Response::error(ErrorKind::BadRequest, e.to_string());
            }
            let installed = match probe::installed(&daemon.store) {
                Ok(l) => l,
                Err(e) => return Response::error(ErrorKind::Internal, e),
            };
            if !installed.iter().any(|(m, _)| m.id == model) {
                return Response::error(
                    ErrorKind::NoSuchModel,
                    format!(
                        "no model named {model:?}. `rime ai models` lists what is installed"
                    ),
                );
            }
            let Ok(mut state) = daemon.state.lock() else {
                return Response::error(ErrorKind::Internal, "the state lock is poisoned");
            };
            // A different model resident means the VRAM is spoken for. One at a
            // time, and switching unloads first — which is the whole of the
            // within-one-user arbitration this design can offer.
            let switching = state
                .running
                .as_ref()
                .is_some_and(|r| r.model != model);
            if switching {
                if daemon.open.load(Ordering::SeqCst) > 0 {
                    return Response::error(
                        ErrorKind::BadRequest,
                        format!(
                            "{} is loaded and in use by {} client(s); switching would cut them \
                             off. Try again when they are done",
                            state.running.as_ref().map(|r| r.model.as_str()).unwrap_or("?"),
                            daemon.open.load(Ordering::SeqCst)
                        ),
                    );
                }
                if let Some(r) = state.running.as_mut() {
                    runtime::stop(r);
                }
                state.running = None;
            }
            state.selected = Some(model);
            Response::Ok
        }

        Request::Unload => {
            let Ok(mut state) = daemon.state.lock() else {
                return Response::error(ErrorKind::Internal, "the state lock is poisoned");
            };
            if daemon.open.load(Ordering::SeqCst) > 0 {
                return Response::error(
                    ErrorKind::BadRequest,
                    "a client is attached; unloading now would cut it off".to_string(),
                );
            }
            if let Some(r) = state.running.as_mut() {
                runtime::stop(r);
            }
            state.running = None;
            Response::Ok
        }
    }
}

/// Everything `rime ai status` prints.
///
/// The plan is rebuilt here rather than cached, so a status taken while nothing
/// is loaded still says what *would* happen — which is the only way the command
/// is useful before the first request. When a backend is running, its own plan
/// is reported instead, because that is what is true.
fn status(daemon: &Arc<Daemon>) -> Status {
    let accel = probe::accel(&daemon.roots);
    let devices = probe::devices(&daemon.roots, &RealNvidiaSmi);
    let on_battery = probe::on_battery(&daemon.roots);

    let mut st = Status {
        protocol: PROTOCOL_VERSION,
        api_socket: daemon.endpoints.api().display().to_string(),
        store: daemon.store.root().display().to_string(),
        accel: accel.available().iter().map(|b| b.as_str().to_string()).collect(),
        devices: devices.iter().map(DeviceInfo::from).collect(),
        open_connections: daemon.open.load(Ordering::SeqCst),
        on_battery,
        ..Default::default()
    };

    let Ok(mut state) = daemon.state.lock() else {
        st.notes.push("the daemon's state lock is poisoned".to_string());
        return st;
    };
    state.settings = load_settings();
    st.idle_secs = state.last_activity.elapsed().as_secs();
    st.idle_timeout = ai::idle_timeout(state.settings.idle_timeout, on_battery);
    st.selected = state.selected.clone().or_else(|| state.settings.model.clone());

    if let Some(r) = state.running.as_ref() {
        st.loaded = Some(r.model.clone());
        st.runtime_path = Some(r.plan.program.clone());
        st.notes.extend(r.plan.notes.iter().cloned());
        st.notes.push(format!(
            "the backend is confined with bwrap --unshare-net, so it has no network at all, \
             and has been up for {}s",
            r.uptime().as_secs()
        ));
    }

    // What would happen, whether or not something is loaded. The same resolver
    // the daemon's start path uses, so this is a report and not a rehearsal of
    // different code.
    match build_plan(daemon, &state) {
        Ok(p) => {
            st.runtime = Some(p.located.runtime.as_str().to_string());
            st.runtime_path = Some(p.located.path.display().to_string());
            st.backend = Some(p.choice.backend.as_str().to_string());
            st.device = p.choice.device;
            st.gpu_layers = Some(p.fit.placement.gpu_layers());
            st.total_layers = match p.fit.placement {
                ai::Placement::Split { total, .. } => Some(total),
                // See the same match in rime/src/ai.rs: an undeclared layer
                // count is not a total, and the sentinel must not be reported
                // as one.
                ai::Placement::Gpu { layers } if layers == ai::ALL_LAYERS_UNKNOWN => None,
                ai::Placement::Gpu { layers } => Some(layers),
                ai::Placement::Cpu => None,
            };
            st.context = Some(p.fit.context);
            st.vram_mib = Some(p.fit.vram_mib);
            st.notes.extend(p.notes);
        }
        Err(e) => {
            st.notes.push(format!("{e:#}"));
            // The install hint is the actionable half, so it is a field rather
            // than prose the CLI would have to grep for.
            if let Some(r) = probe::locate_any() {
                st.runtime = Some(r.runtime.as_str().to_string());
                st.runtime_path = Some(r.path.display().to_string());
            } else {
                let backend = ai::select_backend(&accel, &devices, None)
                    .map(|c| c.backend)
                    .unwrap_or(ai::Backend::Cpu);
                st.install_hint = Some(ai::Runtime::LlamaCpp.install_hint(backend));
            }
        }
    }
    st
}

// ── the idle timer ───────────────────────────────────────────────────────────

/// The one thread that owns the backend's lifetime.
///
/// It both starts the backend and enforces the idle policy, in one loop, for
/// two reasons. The first is the `PR_SET_PDEATHSIG` semantics recorded on
/// [`Daemon::spawn`]: the thread that spawns the child must outlive it. The
/// second is that "start it" and "stop it when idle" are the same
/// responsibility, and splitting them across two threads would need a second
/// lock ordering to get right.
///
/// A pending request is served before the idle check, so a load never waits on
/// a timer tick. While a load is in progress the idle check does not run, which
/// is correct: there is nothing to unload yet.
fn spawn_supervisor(daemon: Arc<Daemon>, rx: mpsc::Receiver<BackendReply>) -> Result<()> {
    std::thread::Builder::new()
        .name("rime-aid-super".into())
        .spawn(move || loop {
            match rx.recv_timeout(IDLE_TICK) {
                Ok(reply) => {
                    let result = ensure_backend(&daemon).map_err(|e| format!("{e:#}"));
                    // A client that gave up before the load finished is
                    // ordinary, not a fault: the backend is up either way and
                    // the next request finds it.
                    let _ = reply.send(result);
                }
                Err(RecvTimeoutError::Timeout) => tick(&daemon),
                // Only reachable once every sender is gone, which means the
                // daemon is shutting down.
                Err(RecvTimeoutError::Disconnected) => return,
            }
        })
        .context("spawning the backend supervisor")?;
    Ok(())
}

/// One pass of the idle policy, on the supervisor thread.
///
/// The decision itself is `rimed_core::ai::plan_idle`, so every rule about
/// attached clients, configured timeouts and the battery default is unit-tested
/// without a clock. This only measures and acts.
fn tick(daemon: &Arc<Daemon>) {
    let Ok(mut state) = daemon.state.lock() else {
        return;
    };
    let loaded = state.running.is_some();
    let inputs = IdleInputs {
        loaded,
        open_connections: daemon.open.load(Ordering::SeqCst),
        idle_secs: state.last_activity.elapsed().as_secs(),
        configured_timeout: state.settings.idle_timeout,
        on_battery: probe::on_battery(&daemon.roots),
    };
    // A backend that died while nobody was looking is reaped here, so `status`
    // and `models` do not keep claiming it is loaded.
    if let Some(r) = state.running.as_mut() {
        if !r.alive() {
            state.running = None;
            return;
        }
    }
    if !ai::plan_idle(&inputs).unloads() {
        return;
    }
    if let Some(r) = state.running.as_mut() {
        eprintln!(
            "rime-aid: unloading {} after {}s idle (timeout {}s{})",
            r.model,
            inputs.idle_secs,
            ai::idle_timeout(inputs.configured_timeout, inputs.on_battery),
            if inputs.on_battery { ", on battery" } else { "" }
        );
        runtime::stop(r);
    }
    state.running = None;
}

// ── shutdown ────────────────────────────────────────────────────────────────

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

/// Shutdown runs on its own thread waiting on a blocked signal set, rather than
/// in a handler: stopping the backend means taking a lock and waiting on a
/// child, neither of which is legal in a signal handler.
fn spawn_signal_thread(daemon: Arc<Daemon>) {
    std::thread::Builder::new()
        .name("rime-aid-signal".into())
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
            eprintln!("rime-aid: signal {sig}, stopping the backend");
            shutdown(&daemon);
            std::process::exit(0);
        })
        .ok();
}

/// Stop the backend and remove the sockets.
///
/// The VRAM matters here: a daemon that exits leaving `llama-server` holding six
/// gigabytes would make `systemctl --user restart rime-aid` fail to load
/// anything, and the cause would be invisible.
fn shutdown(daemon: &Daemon) {
    if let Ok(mut state) = daemon.state.lock() {
        if let Some(r) = state.running.as_mut() {
            runtime::stop(r);
        }
        state.running = None;
    }
    let _ = std::fs::remove_file(daemon.endpoints.api());
    let _ = std::fs::remove_file(daemon.endpoints.control());
    let _ = std::fs::remove_file(daemon.endpoints.backend());
}
