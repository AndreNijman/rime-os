//! `rime-secretd` — the Rime protected secret service (roadmap §11, P0-002).
//!
//! A separate daemon from `rime-agentd` and from `rimed`, for one reason:
//! `rime-agentd` runs as the user because it launches the user's own programs,
//! so anything it can read a managed agent with that uid can read too. A
//! credential store it owns is a credential store the agent owns.
//!
//! ```text
//! agent / rime secret use
//!      │  capability request                    (a NAME, never a URL)
//!      ▼
//! rime-agentd            — as the user; resolves the session and its policy
//!      │  newline-delimited JSON on /run/rime-secretd/control.sock
//!      ▼
//! rime-secretd           — as root; store at /var/lib/rime-secretd, 0700
//!      ├─ SO_PEERCRED + a pinned /proc dirfd            peer.rs
//!      ├─ the verbs, and who may ask for them           service.rs
//!      └─ git, run as the owner with the credential     broker.rs
//!               │
//!               ▼  the operation's output, credential scrubbed out
//! ```
//!
//! Root, and deliberately. The store has to be unreadable by the user's uid or
//! P0-002's first criterion is not met, and the operation has to run *as* the
//! user or git would execute a caller-controlled repository's configuration as
//! root. Only a process that starts privileged can do both. The unit that ships
//! it takes away everything else — see `files/system/units/rime-secretd.service`.
//!
//! Nothing here is agent orchestration, and nothing here talks to `rimed`.
//! AGENTS.md's rule that `rime-agentd` must stay unprivileged is untouched: it
//! stays unprivileged and becomes a *client* of this daemon, which is what let
//! it stop holding credentials at all.

mod broker;
mod peer;
mod present;
mod provider;
mod providers;
mod service;
mod tls;

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rime_secret_core::paths;
use rime_secret_core::protocol::{ErrorKind, Request, Response, MAX_LINE_BYTES};
use rime_secret_core::store::{self, Store};
use rime_secret_core::SecretValue;

use peer::{Peer, ProcHandle};
use service::Service;

const USAGE: &str = "\
rime-secretd — the Rime protected secret service

usage:
  rime-secretd [--socket PATH] [--store DIR]

options:
  --socket PATH   listen here instead of /run/rime-secretd/control.sock
  --store DIR     keep credentials here instead of /var/lib/rime-secretd
  -h, --help      print this
  -V, --version   print the version

Credentials are stored outside every path a managed agent can read, and no
reply this service sends contains one. An agent asks for an operation; the
service performs it and returns the result.

Run as root in the image. Started by hand it works, keeps its store wherever
you pointed it, and says `protected: false` — because a store the user can
read is not a boundary.
";

fn main() {
    let mut socket = paths::socket();
    let mut store_root = paths::store_root();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            "-V" | "--version" => {
                println!("rime-secretd {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--socket" => match args.next() {
                Some(v) => socket = PathBuf::from(v),
                None => fail("--socket needs a path"),
            },
            "--store" => match args.next() {
                Some(v) => store_root = PathBuf::from(v),
                None => fail("--store needs a directory"),
            },
            other => fail(&format!("unknown option '{other}'; try --help")),
        }
    }

    if let Err(e) = run(&socket, &store_root) {
        eprintln!("rime-secretd: {e}");
        std::process::exit(1);
    }
}

fn fail(message: &str) -> ! {
    eprintln!("rime-secretd: {message}");
    std::process::exit(2);
}

fn run(socket: &Path, store_root: &Path) -> Result<(), String> {
    // Safe: geteuid cannot fail.
    let protected = unsafe { libc::geteuid() } == 0;

    store::ensure_private_dir(store_root)
        .map_err(|e| format!("cannot create {}: {e}", store_root.display()))?;
    let listener = bind(socket)?;

    if !protected {
        eprintln!(
            "rime-secretd: running as uid {}, so the store at {} is readable by \
             that account — this is a test instance, not a boundary",
            // Safe: getuid cannot fail.
            unsafe { libc::getuid() },
            store_root.display()
        );
    }
    eprintln!(
        "rime-secretd: listening on {} (store {})",
        socket.display(),
        store_root.display()
    );

    // Fatal, not a warning. A daemon that dropped a provider would answer
    // "that is not an operation this build offers" for a capability the owner
    // had granted, which is the most confusing possible refusal.
    let store = Store::new(store_root.to_path_buf());
    let registry = match providers::default_registry(store.run_dir()) {
        Ok(registry) => registry,
        Err(e) => {
            eprintln!("rime-secretd: a shipped provider does not declare validly: {e}");
            std::process::exit(1);
        }
    };
    let service = Arc::new(Service::new(store, protected, registry));
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let service = Arc::clone(&service);
                std::thread::spawn(move || serve(&service, stream));
            }
            // One bad accept must not take the daemon down; the next connection
            // may well work.
            Err(e) => eprintln!("rime-secretd: accept failed: {e}"),
        }
    }
    Ok(())
}

/// Bind the control socket.
///
/// `0666`, in a directory the daemon creates `0755`. Every local account has to
/// be able to reach the daemon — a per-uid store is useless if only one uid can
/// connect — and who the caller is gets decided from `SO_PEERCRED`, not from a
/// mode bit.
///
/// The mode is set after the bind rather than through the umask, and that is
/// safe in this one direction: `bind(2)` applies the umask, so the socket can
/// only ever start *more* restrictive than `0666` and is then widened. The
/// directory is the other way round — a umask of zero would create it
/// world-writable, and somebody could replace the socket — so it is tightened
/// immediately, and in the image `RuntimeDirectory=` has already made it.
fn bind(socket: &Path) -> Result<UnixListener, String> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        let mut perms = std::fs::metadata(parent)
            .map_err(|e| format!("cannot stat {}: {e}", parent.display()))?
            .permissions();
        if perms.mode() & 0o777 != 0o755 {
            perms.set_mode(0o755);
            std::fs::set_permissions(parent, perms).ok();
        }
    }

    // A socket left behind by a killed daemon refuses `connect`, so a stale one
    // is a dead daemon and not a running one. Probing first rather than
    // unlinking blindly: unlinking a *live* daemon's socket would leave two
    // instances, one of them unreachable.
    if socket.exists() {
        if UnixStream::connect(socket).is_ok() {
            return Err(format!(
                "another rime-secretd is already listening on {}",
                socket.display()
            ));
        }
        std::fs::remove_file(socket).ok();
    }

    let listener = UnixListener::bind(socket)
        .map_err(|e| format!("cannot listen on {}: {e}", socket.display()))?;

    let mut perms = std::fs::metadata(socket)
        .map_err(|e| format!("cannot stat {}: {e}", socket.display()))?
        .permissions();
    if perms.mode() & 0o777 != 0o666 {
        perms.set_mode(0o666);
        std::fs::set_permissions(socket, perms)
            .map_err(|e| format!("cannot set the mode of {}: {e}", socket.display()))?;
    }
    Ok(listener)
}

/// One connection: request lines until the peer goes away.
fn serve(service: &Service, stream: UnixStream) {
    let Some(peer) = peer::credentials(&stream) else {
        // An unknown peer is unauthenticated, never trusted.
        let _ = write_response(
            &stream,
            &Response::error(
                ErrorKind::PermissionDenied,
                "this connection has no peer credentials, so there is nobody to \
                 answer"
                    .to_string(),
            ),
        );
        return;
    };
    // Pinned once, right after accept. Every later question about the peer goes
    // through this handle, so a reused pid cannot be mistaken for the caller.
    let handle = ProcHandle::open(peer.pid);

    let Ok(reader_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader_stream);
    let mut writer = stream;

    loop {
        let mut line = String::new();
        let read = (&mut reader)
            .take(MAX_LINE_BYTES as u64)
            .read_line(&mut line);
        match read {
            Ok(0) => return,
            Ok(_) => {}
            Err(_) => return,
        }
        if line.trim().is_empty() {
            continue;
        }
        let request = match serde_json::from_str::<Request>(line.trim_end()) {
            Ok(request) => request,
            Err(e) => {
                let _ = write_response(
                    &writer,
                    &Response::error(
                        ErrorKind::BadRequest,
                        format!("cannot parse that request: {e}"),
                    ),
                );
                let _ = writer.flush();
                continue;
            }
        };
        // The one verb that consumes the connection. It is handled here and
        // not in `dispatch` because everything after its reply is the
        // capsule's own plaintext: a loop that came back here would read the
        // first bytes of an HTTP request as a request line of this protocol.
        if let Request::Present { record, destination } = request {
            present::serve(service, peer, *record, &destination, writer);
            return;
        }
        let response = dispatch(
            service,
            peer,
            handle.as_ref(),
            request,
            &mut reader,
            &writer,
        );
        if write_response(&writer, &response).is_err() {
            return;
        }
        let _ = writer.flush();
    }
}

pub(crate) fn write_response(mut stream: &UnixStream, response: &Response) -> std::io::Result<()> {
    let mut line = serde_json::to_string(response)
        .unwrap_or_else(|_| r#"{"reply":"error","kind":"internal","message":"unserialisable"}"#.into());
    line.push('\n');
    stream.write_all(line.as_bytes())
}

/// Route one request, having first decided whether the caller may ask for it.
fn dispatch(
    service: &Service,
    peer: Peer,
    handle: Option<&ProcHandle>,
    request: Request,
    reader: &mut BufReader<UnixStream>,
    stream: &UnixStream,
) -> Response {
    match request {
        Request::Hello => service.hello(),
        Request::List => service.list(peer),
        Request::Grants => service.grants(peer),
        Request::Approvals => service.approvals(peer),
        Request::Audit { lines, project } => service.audit(peer, lines, project.as_deref()),
        // Read-only, like the four above it: it reports what the trail already
        // holds and changes nothing, so a managed session may ask. An agent
        // being able to see the budget it is running under is the point.
        Request::Usage { project } => service.usage(peer, &project),

        // Mutating verbs. A session may not change what it is allowed to do.
        Request::Add {
            service: name,
            host,
            scheme,
            username,
            path,
            auth,
            port,
            value_len,
        } => {
            // The bytes are read whatever the decision, or the next request
            // line would be parsed out of the middle of a credential.
            let value = match read_value(reader, value_len, stream, VALUE_TIMEOUT) {
                Ok(v) => v,
                Err(message) => return Response::error(ErrorKind::BadRequest, message),
            };
            if let Some(refusal) = refuse_a_session(handle, "store a credential") {
                return refusal;
            }
            service.add(
                peer,
                service::NewService {
                    service: &name,
                    host: &host,
                    scheme: &scheme,
                    username: username.as_deref(),
                    path: &path,
                    auth: auth.as_deref(),
                    port,
                },
                value,
            )
        }
        Request::Remove { service: name } => {
            if let Some(refusal) = refuse_a_session(handle, "delete a credential") {
                return refusal;
            }
            service.remove(peer, &name)
        }
        Request::Grant {
            project,
            service: name,
            capability,
            revoke,
        } => {
            if let Some(refusal) = refuse_a_session(handle, "change its own capabilities") {
                return refusal;
            }
            service.grant(peer, &project, &name, &capability, revoke)
        }
        // §13.8. A session that could approve its own production deployment
        // would have been made to type one more line rather than made to ask,
        // so this is refused from inside one exactly like `Grant` is — and the
        // refusal is what makes "production approval is default" mean anything
        // at all when the thing being approved is the agent's own request.
        Request::Approve {
            project,
            service: name,
            operation,
            resource,
            ttl_ms,
            withdraw,
        } => {
            if let Some(refusal) = refuse_a_session(handle, "approve its own operations") {
                return refusal;
            }
            service.approve(
                peer,
                service::NewApproval {
                    project: &project,
                    service: &name,
                    operation: &operation,
                    resource: &resource,
                    ttl_ms,
                    withdraw,
                },
            )
        }

        // The broker. Sessions are exactly who this is for.
        Request::Use { record, body_len } => {
            // Read whatever was promised before deciding anything, or the next
            // request line would be parsed out of the middle of this message.
            let body = match read_body(reader, body_len, stream, VALUE_TIMEOUT) {
                Ok(b) => b,
                Err(message) => return Response::error(ErrorKind::BadRequest, message),
            };
            service.use_capability(peer, *record, body)
        }

        // Handled in `serve`, which hands the whole connection to
        // `present::serve` before this function is reached. Reaching it means
        // somebody moved the hijack and did not move this — so it refuses, and
        // the refusal is the fail-closed direction: a capsule gets an error
        // instead of an unauthenticated tunnel it would spend minutes not
        // understanding.
        Request::Present { .. } => Response::error(
            ErrorKind::Internal,
            "a present request reached the ordinary dispatch path, which cannot carry one: \
             it consumes the connection and is handled where connections are accepted"
                .to_string(),
        ),
    }
}

/// Refuse a mutating verb that came from inside a managed agent session.
///
/// A missing handle is a refusal too: it means the peer's `/proc` entry could
/// not be pinned, which is either a process that exited or a pid that has been
/// reused, and neither is a caller to grant anything to.
fn refuse_a_session(handle: Option<&ProcHandle>, what: &str) -> Option<Response> {
    let Some(handle) = handle else {
        return Some(Response::error(
            ErrorKind::PermissionDenied,
            "this caller cannot be identified, so it cannot change what is \
             allowed"
                .to_string(),
        ));
    };
    if peer::looks_like_a_session(handle, peer::live_lookup) {
        return Some(Response::error(
            ErrorKind::PermissionDenied,
            format!(
                "an agent session cannot {what}; run this from your own shell \
                 instead"
            ),
        ));
    }
    None
}

/// How long the daemon will wait for the bytes an `add` promised.
///
/// The one place a caller names a length the daemon then blocks on, so it is
/// the one place a local process can pin a connection thread by promising bytes
/// it never sends. Long enough for a credential arriving over a pipe from a
/// password manager, short enough that holding threads open costs something.
const VALUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Read exactly `len` bytes of credential from the socket.
///
/// `stream` is the same connection the reader wraps, borrowed to set a timeout
/// for this read alone. It is cleared afterwards, because the connection stays
/// open for further requests and a caller may sit idle between them. The
/// timeout is a parameter rather than the constant so the test can prove the
/// wait actually ends without waiting [`VALUE_TIMEOUT`] to find out.
fn read_value(
    reader: &mut BufReader<UnixStream>,
    len: usize,
    stream: &UnixStream,
    timeout: std::time::Duration,
) -> Result<SecretValue, String> {
    if len == 0 {
        return Err("that credential is empty; nothing was stored".to_string());
    }
    if len > SecretValue::MAX_BYTES {
        return Err(format!(
            "that credential is {len} bytes; the limit is {}",
            SecretValue::MAX_BYTES
        ));
    }
    let mut buf = vec![0u8; len];
    stream.set_read_timeout(Some(timeout)).ok();
    let read = reader.read_exact(&mut buf);
    stream.set_read_timeout(None).ok();
    read.map_err(|e| format!("the credential did not arrive: {e}"))?;
    Ok(SecretValue::new(buf))
}

/// Read exactly `len` bytes of message from the socket.
///
/// The same framing as [`read_value`] and bounded by the same reasoning, with
/// one difference: a message body is not a credential, so it is a plain `Vec`
/// and a length of zero is legitimate — every git capability sends none.
fn read_body(
    reader: &mut BufReader<UnixStream>,
    len: usize,
    stream: &UnixStream,
    timeout: std::time::Duration,
) -> Result<Vec<u8>, String> {
    if len == 0 {
        return Ok(Vec::new());
    }
    if len > MAX_BODY_BYTES {
        return Err(format!(
            "that message is {len} bytes; the limit is {MAX_BODY_BYTES}"
        ));
    }
    let mut buf = vec![0u8; len];
    stream.set_read_timeout(Some(timeout)).ok();
    let read = reader.read_exact(&mut buf);
    stream.set_read_timeout(None).ok();
    read.map_err(|e| format!("the message did not arrive: {e}"))?;
    Ok(buf)
}

/// Longest message body a capability may carry.
///
/// An MCP `write_note` of a long document is the case this has to fit; a
/// caller that could name any length at all is a way to make the daemon
/// allocate.
const MAX_BODY_BYTES: usize = 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        // The clock is in the name as well as the pid: a run that panicked
        // left its directory behind, and a later run whose pid happened to
        // match would then bind over a socket it did not create.
        let dir = std::env::temp_dir().join(format!(
            "rime-secretd-main-{}-{tag}-{}",
            std::process::id(),
            store::now_ms()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_socket_is_world_reachable_and_its_directory_is_not_writable_by_all() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("bind");
        let socket = dir.join("nested/control.sock");
        let listener = bind(&socket).expect("bind");

        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        // Every local account must be able to connect; who they are is decided
        // from SO_PEERCRED afterwards.
        assert_eq!(mode(&socket), 0o666);
        // The directory is not world-writable, so nobody else can replace the
        // socket with one of their own.
        assert_eq!(mode(socket.parent().unwrap()), 0o755);

        // A second daemon on the same path is refused rather than silently
        // taking over.
        assert!(bind(&socket).is_err());

        drop(listener);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Set in the child that plays the dead daemon: bind this path, then exit.
    const DEAD_DAEMON_ENV: &str = "RIME_SECRETD_TEST_BIND_AND_EXIT";

    #[test]
    fn a_stale_socket_from_a_dead_daemon_is_replaced() {
        // The child half: be the daemon that dies. It binds and returns, and
        // the process exits, which is what a killed daemon leaves behind.
        if let Some(path) = std::env::var_os(DEAD_DAEMON_ENV) {
            let _listener = bind(Path::new(&path)).expect("bind in the dead daemon");
            return;
        }

        let dir = temp_dir("stale");
        let socket = dir.join("control.sock");
        // The predecessor is a separate PROCESS that binds and exits, never a
        // listener dropped in this one. A listener created here could be
        // copied into a child that another test thread is forking at that
        // moment, and that copy keeps the socket accepting until the child
        // execs: the stale socket then accepts, and this test failed once on a
        // GitHub runner and once in a Nix build under load for exactly that
        // reason. A process that has exited holds nothing, so the socket is
        // dead every time.
        let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", "tests::a_stale_socket_from_a_dead_daemon_is_replaced", "--test-threads=1"])
            .env(DEAD_DAEMON_ENV, &socket)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("start the daemon that dies");
        assert!(status.success(), "the dead daemon could not bind: {status}");
        // The file survives the daemon, and connecting to it now fails, which
        // is what makes it stale rather than live.
        assert!(socket.exists(), "the dead daemon left no socket behind");

        // Reported rather than merely asserted, so a failure arrives with
        // enough to diagnose it instead of the bare word `false`. `bind()`'s
        // staleness rule is exactly this connect, so a machine where it
        // succeeds would silently refuse to start a daemon whose predecessor
        // had died.
        if let Ok(live) = UnixStream::connect(&socket) {
            let meta = std::fs::symlink_metadata(&socket);
            panic!(
                "connecting to a socket whose daemon has exited SUCCEEDED.\n\
                 \x20 path:      {}\n\
                 \x20 file type: {:?}\n\
                 \x20 peer:      {:?}\n\
                 \x20 tmpdir:    {:?}\n\
                 If this is reproducible, `bind()`'s staleness probe cannot tell a \
                 dead daemon from a live one on this system.",
                socket.display(),
                meta.as_ref().map(|m| m.file_type()),
                live.peer_addr(),
                std::env::temp_dir(),
            );
        }
        let _listener = bind(&socket).expect("rebind over a stale socket");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_value_longer_than_the_limit_is_refused_before_it_is_read() {
        let (a, _b) = UnixStream::pair().unwrap();
        let mut reader = BufReader::new(a.try_clone().unwrap());
        let err = read_value(&mut reader, SecretValue::MAX_BYTES + 1, &a, VALUE_TIMEOUT).unwrap_err();
        assert!(err.contains("limit is"), "{err}");
        // And an empty one is an error rather than a stored blank.
        assert!(read_value(&mut reader, 0, &a, VALUE_TIMEOUT)
            .unwrap_err()
            .contains("empty"));
        // Neither refusal touched the connection's timeout, so a caller that
        // asked for something silly has not changed how the next request is
        // read.
        assert_eq!(a.read_timeout().unwrap(), None);
    }

    #[test]
    fn a_promised_credential_that_never_arrives_times_out() {
        // The one place a caller names a length the daemon then blocks on. A
        // socketpair whose other end sends nothing is exactly that case.
        let (a, _b) = UnixStream::pair().unwrap();
        let mut reader = BufReader::new(a.try_clone().unwrap());
        let short = std::time::Duration::from_millis(50);
        let started = std::time::Instant::now();
        let err = read_value(&mut reader, 32, &a, short).unwrap_err();
        assert!(err.contains("did not arrive"), "{err}");
        assert!(started.elapsed() < VALUE_TIMEOUT, "the caller's timeout was ignored");
        assert_eq!(a.read_timeout().unwrap(), None, "the timeout was left set");
    }

    #[test]
    fn the_usage_text_says_what_the_service_refuses_to_do() {
        // `--help` is where somebody learns what this daemon is for, and the
        // one thing they must not be left guessing is whether it will hand over
        // a credential.
        assert!(USAGE.contains("reply this service sends contains one"));
        assert!(USAGE.contains("--socket"));
        assert!(USAGE.contains("--store"));
    }

    #[test]
    fn approving_is_a_mutating_verb_and_goes_through_the_session_gate() {
        // §13.8 means nothing if the thing being asked for approval can
        // approve itself. `Approve` has to be routed exactly like `Grant`, and
        // the cheapest honest proof is the arm of the gate that needs no
        // session: a caller whose `/proc` entry could not be pinned is refused,
        // and it is refused by `dispatch` rather than by `Service::approve` —
        // which is the whole point, because `Service::approve` cannot see the
        // peer's placement at all.
        let dir = temp_dir("approve-gate");
        let store = Store::new(dir.clone());
        let registry = providers::default_registry(store.run_dir()).expect("registry");
        let service = Service::new(store, false, registry);
        let (a, b) = UnixStream::pair().unwrap();
        let mut reader = BufReader::new(a.try_clone().unwrap());
        let peer = Peer {
            pid: std::process::id() as libc::pid_t,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
        };

        for request in [
            Request::Approve {
                project: "/tmp/p".into(),
                service: "demo".into(),
                operation: "git.push".into(),
                resource: "origin".into(),
                ttl_ms: None,
                withdraw: false,
            },
            Request::Grant {
                project: "/tmp/p".into(),
                service: "demo".into(),
                capability: "git.push".into(),
                revoke: false,
            },
        ] {
            let name = request.clone();
            let reply = dispatch(&service, peer, None, request, &mut reader, &a);
            let (kind, message) = reply
                .as_error()
                .unwrap_or_else(|| panic!("{name:?} was allowed without an identifiable caller"));
            assert_eq!(kind, ErrorKind::PermissionDenied, "{message}");
            assert!(message.contains("cannot be identified"), "{message}");
        }

        // And reading what is approved is NOT gated, for the reason `list` and
        // `grants` are not: seeing what is allowed changes nothing, and an
        // agent that could not see it would report a refusal it cannot explain.
        let reply = dispatch(&service, peer, None, Request::Approvals, &mut reader, &a);
        assert!(
            matches!(reply, Response::Approvals { .. }),
            "reading approvals must not need the session gate: {reply:?}"
        );

        drop(b);
        std::fs::remove_dir_all(&dir).ok();
    }
}
