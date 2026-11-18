//! The way out of an allowlisted session.
//!
//! `--network allowlist` is `bwrap --unshare-net` — the same namespace with
//! nothing in it that `offline` gets — plus one route back through the daemon.
//! Two processes make that route, and the split between them is the whole
//! security argument:
//!
//! ```text
//! inside the namespace                    outside it
//!
//!   agent  ──HTTP CONNECT──▶  bridge  ──▶ socket ──▶  daemon ──▶ internet
//!          127.0.0.1:3128    (no policy)  AF_UNIX    (decides)
//! ```
//!
//! The bridge is `rime-agentd` re-executed with `--net-bridge`, running as the
//! session's parent process inside the sandbox. It has to be inside, because
//! the loopback an HTTP client can reach is the session's own and a listener
//! in the daemon would be on the host's. It carries bytes and nothing else: it
//! does not parse the request, does not know what the allowlist says, and
//! cannot be usefully replaced, because the far end of its socket is still the
//! daemon.
//!
//! The daemon speaks the proxy protocol. It reads the `CONNECT`, asks
//! [`Allowlist::decide`] about the name and port, resolves, asks
//! [`Allowlist::accepts_address`] about each address that came back, connects
//! to an address it checked, and then copies bytes. Both questions are pure
//! functions in `rime-agent-core`, asserted there against a table rather than
//! against a network.
//!
//! ## Why this cannot be walked around from inside
//!
//! Not because the session is asked nicely to use the proxy. `HTTPS_PROXY` and
//! friends are set so that a client knows where the bridge is, but unsetting
//! them does not open anything: the namespace has no route, no resolver and no
//! addresses. A session that ignores the proxy variables does not get a direct
//! connection, it gets `ENETUNREACH`.
//!
//! ## What it does not stop
//!
//! - **Anything that is not proxy-aware HTTP.** There is no DNS inside the
//!   namespace — the daemon resolves, which is why the destination is checked
//!   as a name — so `ssh`, raw sockets and UDP do not work at all. That is a
//!   limitation of the mode, not a hole in it.
//! - **Anything sent to an allowed destination.** `CONNECT` opens a tunnel and
//!   the tunnel is opaque; this is a destination policy, and a session allowed
//!   to reach a host can send it whatever it likes, in any volume.
//! - **A poisoned name.** The rule names a host, and whoever controls that
//!   host's DNS controls where it points. `accepts_address` refuses an answer
//!   that lands on this machine or its LAN, which is the case that matters
//!   locally; past that, the client's own TLS validation is the check.
//! - **A second process of the same user.** The socket is `0600` inside a
//!   `0700` directory, which is the same trust boundary the control socket
//!   has: it separates this user's agents from other users, not from the user.

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rime_agent_core::destination::{Allowlist, Destination, Verdict};
use rime_agent_core::sandbox::BRIDGE_FLAG;

use crate::intercept::Intercept;
use crate::pty;

/// How long the accept loop sleeps between checks that the session is still
/// there. One second, like the session reader's own poll.
const POLL_INTERVAL_MS: i32 = 1000;

/// Longest a proxy request head may be before it is abandoned.
///
/// A `CONNECT` is one short line plus a handful of headers. Anything past this
/// is a client that is not speaking the protocol, and reading it forever would
/// hold a thread open for as long as it kept sending.
const MAX_HEAD_BYTES: usize = 8 * 1024;

/// How long to wait for that head, and how long to spend on one connect.
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

// ── the daemon's half ───────────────────────────────────────────────────────

/// Start the egress proxy for one session.
///
/// `allow` is a snapshot taken when the session started. A destination added
/// to the configuration afterwards does not widen a session that is already
/// running: what a session may reach is decided once, when the user starts it,
/// which is also the only moment they were asked.
pub fn start(
    id: u32,
    socket: &Path,
    allow: Allowlist,
    intercept: Option<Arc<Intercept>>,
) -> std::io::Result<()> {
    // A socket left by a daemon that died would make bind fail with EADDRINUSE
    // and take the session down with it.
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;

    let socket = socket.to_path_buf();
    std::thread::Builder::new()
        .name(format!("rime-agentd-egress-{id}"))
        .spawn(move || accept_loop(id, listener, socket, Arc::new(allow), intercept))?;
    Ok(())
}

/// Accept until the session's scratch directory goes away.
///
/// The session's own lifetime is the loop's: `session::finish` deletes the
/// scratch directory, taking the socket with it, and this notices within a
/// poll interval. A stop flag threaded through the registry would be one more
/// thing for a future edit to forget to set, and forgetting it would leak a
/// thread per session for the life of the daemon.
fn accept_loop(
    id: u32,
    listener: UnixListener,
    socket: PathBuf,
    allow: Arc<Allowlist>,
    intercept: Option<Arc<Intercept>>,
) {
    let fd = listener.as_raw_fd();
    loop {
        if !socket.exists() {
            break;
        }
        if !pty::wait_readable(fd, POLL_INTERVAL_MS) {
            continue;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let allow = Arc::clone(&allow);
                let intercept = intercept.clone();
                let spawned = std::thread::Builder::new()
                    .name(format!("rime-agentd-egress-{id}-conn"))
                    .spawn(move || serve(id, stream, &allow, intercept.as_deref()));
                if spawned.is_err() {
                    // No thread means no connection. Saying so is better than
                    // dropping it silently, which reads inside the agent as a
                    // network that works intermittently.
                    eprintln!("rime-agentd: session {id} could not start an egress thread");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => {
                eprintln!("rime-agentd: session {id} egress listener stopped: {e}");
                break;
            }
        }
    }
    let _ = std::fs::remove_file(&socket);
}

/// One proxy connection, from its request line to the end of its tunnel.
fn serve(id: u32, client: UnixStream, allow: &Allowlist, intercept: Option<&Intercept>) {
    let _ = client.set_read_timeout(Some(HEAD_TIMEOUT));
    let head = match read_head(&client) {
        Ok(h) => h,
        Err(_) => return,
    };

    let Some(target) = connect_target(&head) else {
        // Absolute-form (`GET http://host/path`) is refused rather than
        // rewritten. A proxy that forwards plain HTTP has to parse and rewrite
        // the request, and every one of those parsers has been a source of
        // request-smuggling bugs. CONNECT is opaque and is what an agent's
        // HTTPS traffic uses anyway.
        reply(
            &client,
            405,
            "Method Not Allowed",
            "this proxy tunnels CONNECT only, so a plain http:// URL cannot go through it; \
             use https://, or ask Rime to perform the operation with `rime secret use`",
        );
        return;
    };

    let dest = match Destination::parse(&target) {
        Ok(d) => d,
        Err(e) => {
            reply(&client, 400, "Bad Request", &e.to_string());
            return;
        }
    };

    let verdict = allow.decide(&dest);
    if !verdict.is_allowed() {
        let why = verdict.explain(&dest);
        eprintln!("rime-agentd: session {id} denied {dest}");
        reply(&client, 403, "Forbidden", &why);
        return;
    }

    // P2-012 route B: the ONE destination this session was told to
    // authenticate is terminated here rather than tunnelled.
    //
    // After `decide` and before the resolve, and both halves of that placement
    // are deliberate. After, because a destination the allowlist refuses is
    // refused whatever else was asked for. Before, because this path never
    // opens a connection of its own — `rime-secretd` dials the credential's
    // own pinned host — so the resolve and `accepts_address` below have nothing
    // to check. What replaces them is that daemon's pin check, which is a
    // boundary this one is not: `rime-agentd` runs as the user.
    //
    // `covers` is exact equality on host and port, NOT a question for the
    // allowlist. A narrowed list may hold a wildcard, and a rule that covers
    // the pin is not the pin.
    if let Some(intercept) = intercept {
        if intercept.covers(&dest) {
            let _ = client.set_read_timeout(None);
            if client
                .try_clone()
                .and_then(|mut w| w.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n"))
                .is_err()
            {
                return;
            }
            if let Err(why) = intercept.serve(client) {
                // Said out loud rather than dropped: the capsule sees a
                // connection that closed, and the reason — a grant that is not
                // there, a secret service that is not running — is only
                // visible here.
                eprintln!("rime-agentd: session {id} could not authenticate {dest}: {why}");
            }
            return;
        }
    }

    // Resolve once, check every answer, and connect to an address that was
    // checked. Handing the name back to `TcpStream::connect` would resolve it
    // a second time, and the second answer is the one an attacker gets to
    // choose.
    let resolved: Vec<SocketAddr> = match (dest.host(), dest.port()).to_socket_addrs() {
        Ok(addrs) => addrs.collect(),
        Err(e) => {
            reply(
                &client,
                502,
                "Bad Gateway",
                &format!("{} could not be resolved: {e}", dest.host()),
            );
            return;
        }
    };

    let mut refusal = None;
    let mut allowed: Vec<SocketAddr> = Vec::new();
    for addr in resolved {
        match allow.accepts_address(&dest, addr.ip()) {
            Verdict::Allow => allowed.push(addr),
            denied => {
                if refusal.is_none() {
                    refusal = Some(denied.explain(&dest));
                }
            }
        }
    }
    if allowed.is_empty() {
        let why = refusal.unwrap_or_else(|| format!("{} resolved to no usable address", dest.host()));
        eprintln!("rime-agentd: session {id} denied {dest} after it resolved");
        reply(&client, 403, "Forbidden", &why);
        return;
    }

    let mut upstream = None;
    for addr in &allowed {
        if let Ok(s) = TcpStream::connect_timeout(addr, CONNECT_TIMEOUT) {
            upstream = Some(s);
            break;
        }
    }
    let Some(upstream) = upstream else {
        reply(
            &client,
            502,
            "Bad Gateway",
            &format!("could not connect to {dest}"),
        );
        return;
    };

    // The tunnel is open from here, so the head timeout has to go: a session
    // holding a long-lived connection is not a session that has stalled.
    let _ = client.set_read_timeout(None);
    if client
        .try_clone()
        .and_then(|mut w| w.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n"))
        .is_err()
    {
        return;
    }
    couple(upstream, client);
}

/// Read a request head, one byte at a time, stopping at the blank line.
///
/// Byte at a time because anything buffered past the blank line belongs to the
/// tunnel, and a `BufReader` would swallow it — the client is allowed to send
/// its first TLS bytes immediately after the CONNECT.
fn read_head(mut stream: &UnixStream) -> std::io::Result<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if stream.read(&mut byte)? == 0 {
            break;
        }
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") || head.ends_with(b"\n\n") {
            break;
        }
        if head.len() >= MAX_HEAD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "proxy request head too long",
            ));
        }
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

/// The authority a `CONNECT` names, or `None` for anything else.
///
/// Public for the tests, which is where the shapes a real client sends are
/// pinned down. Case-insensitive on the method because the grammar is, and
/// tolerant of the extra whitespace some clients emit.
pub fn connect_target(head: &str) -> Option<String> {
    let line = head.lines().next()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    if !method.eq_ignore_ascii_case("CONNECT") {
        return None;
    }
    let target = parts.next()?;
    // A version is required by the grammar; a request line with nothing after
    // the target is not one this will guess at.
    parts.next()?;
    Some(target.to_string())
}

/// Answer with a status and a body that says what to do next.
///
/// The body matters: an agent shown a bare 403 will retry the same destination
/// until it gives up, and the session cannot read the allowlist to find out
/// why — it is in the daemon's configuration, outside the sandbox.
fn reply(mut stream: &UnixStream, code: u16, status: &str, body: &str) {
    let body = format!("{body}\n");
    let head = format!(
        "HTTP/1.1 {code} {status}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

// ── the in-sandbox half ─────────────────────────────────────────────────────

/// What a `--net-bridge` invocation asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bridge {
    pub socket: PathBuf,
    pub port: u16,
    pub program: String,
    pub args: Vec<String>,
}

/// Parse `rime-agentd --net-bridge <socket> <port> -- <program> [args…]`.
///
/// Returns `None` when this is not a bridge invocation at all, so `main` can
/// carry on being a daemon. A *malformed* one is an error rather than a
/// fall-through: starting a second daemon because the argv did not parse would
/// be a confusing way to lose a session.
pub fn parse_bridge_argv<S: AsRef<str>>(args: &[S]) -> Option<Result<Bridge, String>> {
    let mut it = args.iter().map(|s| s.as_ref());
    if it.next() != Some(BRIDGE_FLAG) {
        return None;
    }
    let bad = |what: &str| Some(Err(format!("{BRIDGE_FLAG}: {what}")));

    let Some(socket) = it.next() else {
        return bad("no socket path");
    };
    let Some(port) = it.next() else {
        return bad("no port");
    };
    let Ok(port) = port.parse::<u16>() else {
        return bad("the port is not a number");
    };
    if port == 0 {
        return bad("the port is not a number");
    }
    if it.next() != Some("--") {
        return bad("expected `--` before the program to run");
    }
    let Some(program) = it.next() else {
        return bad("no program to run");
    };
    Some(Ok(Bridge {
        socket: PathBuf::from(socket),
        port,
        program: program.to_string(),
        args: it.map(str::to_string).collect(),
    }))
}

/// Run as the bridge: listen, launch the session, carry bytes, exit with it.
///
/// The order is deliberate. The listener is bound *before* the fork so the
/// agent cannot start, resolve a proxy and be refused a connection that is not
/// there yet; the fork happens *before* any thread exists, because a forked
/// child of a threaded process inherits one thread and any lock the others
/// held; and the signal dispositions are set *after* the fork, because
/// `SIG_IGN` survives `execve` and the agent must keep its own ctrl-C.
pub fn run_bridge(b: Bridge) -> i32 {
    use std::ffi::CString;

    let listener = match TcpListener::bind(("127.0.0.1", b.port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("rime: the session's network bridge could not start: {e}");
            return 126;
        }
    };

    // Everything that can allocate has to happen before the fork: between fork
    // and exec only async-signal-safe calls are legal.
    let mut argv: Vec<String> = vec![b.program.clone()];
    argv.extend(b.args.iter().cloned());
    let c_argv: Vec<CString> = match argv
        .iter()
        .map(|a| CString::new(a.as_bytes()))
        .collect::<Result<_, _>>()
    {
        Ok(v) => v,
        Err(_) => {
            eprintln!("rime: an argument contained a NUL byte");
            return 126;
        }
    };
    let mut ptrs: Vec<*const libc::c_char> = c_argv.iter().map(|s| s.as_ptr()).collect();
    ptrs.push(std::ptr::null());

    // Safe: fork takes no arguments.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        eprintln!("rime: could not start the session under its network bridge");
        return 126;
    }
    if pid == 0 {
        // Safe: exec reads the argv we built; on success it never returns.
        unsafe {
            libc::execvp(ptrs[0], ptrs.as_ptr());
            // 127 is the shell's convention for "command not found".
            libc::_exit(127);
        }
    }

    // The agent shares this process group, so ctrl-C reaches both. The bridge
    // outliving the signal is the point: if it died first the session would
    // lose its network mid-keystroke, and the agent's own handler is what
    // should decide what ctrl-C means.
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
        // Safe: setting a disposition on the parent, after the fork, so
        // nothing is inherited across the child's exec.
        unsafe { libc::signal(sig, libc::SIG_IGN) };
    }

    let socket = b.socket.clone();
    let carrier = std::thread::Builder::new()
        .name("rime-net-bridge".into())
        .spawn(move || bridge_loop(listener, socket));
    if carrier.is_err() {
        eprintln!("rime: the session's network bridge could not start its carrier thread");
    }

    let mut status: libc::c_int = 0;
    loop {
        // Safe: waitpid writes one int we own.
        let rc = unsafe { libc::waitpid(pid, &mut status, 0) };
        if rc >= 0 {
            break;
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return 126;
        }
    }

    if libc::WIFSIGNALED(status) {
        // Die the same way the agent did, so the runtime records a signal
        // rather than a code that happens to look like one.
        let sig = libc::WTERMSIG(status);
        // Safe: restoring the default disposition and raising it on ourselves.
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }
    libc::WEXITSTATUS(status)
}

/// Accept on loopback and hand every connection to the daemon's socket.
///
/// No parsing, no policy, no idea what a destination is. Everything it accepts
/// goes to the same place, and that place decides.
fn bridge_loop(listener: TcpListener, socket: PathBuf) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let socket = socket.clone();
        let _ = std::thread::Builder::new()
            .name("rime-net-bridge-conn".into())
            .spawn(move || match UnixStream::connect(&socket) {
                Ok(up) => couple(stream, up),
                Err(_) => {
                    // The daemon's end is gone, which for a session means its
                    // network has. Closing is the honest answer; a client sees
                    // a refused proxy rather than a hang.
                    let _ = stream.shutdown(Shutdown::Both);
                }
            });
    }
}

// ── shared ──────────────────────────────────────────────────────────────────

/// Copy bytes between a TCP connection and a Unix one until both ends close.
///
/// Half-close is propagated in each direction rather than tearing the whole
/// connection down on the first `EOF`: a client that has finished sending is
/// still waiting to receive, and a proxy that closed on it would truncate the
/// response.
fn couple(tcp: TcpStream, unix: UnixStream) {
    let (Ok(tcp2), Ok(unix2)) = (tcp.try_clone(), unix.try_clone()) else {
        return;
    };
    let up = std::thread::Builder::new()
        .name("rime-egress-up".into())
        .spawn(move || {
            let _ = std::io::copy(&mut &tcp2, &mut &unix2);
            let _ = unix2.shutdown(Shutdown::Write);
            let _ = tcp2.shutdown(Shutdown::Read);
        });
    let _ = std::io::copy(&mut &unix, &mut &tcp);
    let _ = tcp.shutdown(Shutdown::Write);
    let _ = unix.shutdown(Shutdown::Read);
    if let Ok(handle) = up {
        let _ = handle.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── P2-012 route B: what the daemon terminates, and what it cannot ──────
    //
    // The property this section exists for cannot be asserted from inside the
    // daemon, because a daemon that intercepted everything would look exactly
    // like one that intercepted the pin — from in here. So it is asserted from
    // the CAPSULE's side, by the only thing a capsule can actually observe:
    // WHICH CERTIFICATE it was handed.
    //
    // Two loopback origins, each with a certificate signed by a CA of its own,
    // and a client that trusts exactly one root set at a time. A tunnelled
    // `CONNECT` reaches the origin's own certificate; a terminated one reaches
    // the minted leaf, which the origin's CA did not sign and could not.
    //
    // The pair is what gives either half meaning. "The handshake succeeded"
    // alone says nothing — it succeeds on both arms, against different roots.

    /// A certificate authority and one server certificate under it.
    struct Pki {
        ca: PathBuf,
        leaf: PathBuf,
        key: PathBuf,
    }

    fn openssl(args: &[&str]) {
        let out = std::process::Command::new("openssl")
            .args(args)
            .output()
            .expect("openssl is in the image and is what mints these");
        assert!(
            out.status.success(),
            "openssl {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Mint a CA and a `127.0.0.1` server certificate under it.
    fn pki(dir: &Path, tag: &str) -> Pki {
        let ca_key = dir.join(format!("{tag}-ca.key"));
        let ca = dir.join(format!("{tag}-ca.pem"));
        let key = dir.join(format!("{tag}.key"));
        let csr = dir.join(format!("{tag}.csr"));
        let leaf = dir.join(format!("{tag}.pem"));
        let ext = dir.join(format!("{tag}.ext"));
        openssl(&[
            "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes",
            "-days", "1", "-subj", &format!("/CN={tag} authority"),
            "-addext", "basicConstraints=critical,CA:TRUE",
            "-addext", "keyUsage=critical,keyCertSign",
            "-keyout", &ca_key.to_string_lossy(), "-out", &ca.to_string_lossy(),
        ]);
        openssl(&[
            "req", "-new", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes",
            "-subj", "/CN=127.0.0.1",
            "-keyout", &key.to_string_lossy(), "-out", &csr.to_string_lossy(),
        ]);
        std::fs::write(
            &ext,
            "subjectAltName=IP:127.0.0.1\nextendedKeyUsage=serverAuth\nbasicConstraints=critical,CA:FALSE\n",
        )
        .expect("ext");
        openssl(&[
            "x509", "-req", "-days", "1", "-set_serial", "2",
            "-in", &csr.to_string_lossy(),
            "-CA", &ca.to_string_lossy(), "-CAkey", &ca_key.to_string_lossy(),
            "-extfile", &ext.to_string_lossy(), "-out", &leaf.to_string_lossy(),
        ]);
        Pki { ca, leaf, key }
    }

    /// A TLS origin on loopback that records the `Authorization` it was sent
    /// and echoes it back in the body.
    ///
    /// Echoing is deliberate: it is the one way a capsule could end up holding
    /// the credential without Rime handing it one, so the scrub has to be
    /// exercised by a site that really does send it back.
    struct Origin {
        port: u16,
        seen: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Origin {
        fn start(pki: &Pki) -> Origin {
            let config = crate::intercept::server_config(&pki.leaf, &pki.key).expect("server config");
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let port = listener.local_addr().expect("addr").port();
            let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
            let recorder = Arc::clone(&seen);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let config = Arc::clone(&config);
                    let recorder = Arc::clone(&recorder);
                    std::thread::spawn(move || {
                        let Ok(mut conn) = rustls::ServerConnection::new(config) else {
                            return;
                        };
                        let mut sock = stream;
                        while conn.is_handshaking() {
                            if conn.complete_io(&mut sock).is_err() {
                                return;
                            }
                        }
                        let mut head = Vec::new();
                        let mut byte = [0u8; 1];
                        let mut tls = rustls::Stream::new(&mut conn, &mut sock);
                        while !head.ends_with(b"\r\n\r\n") {
                            match tls.read(&mut byte) {
                                Ok(0) | Err(_) => return,
                                Ok(_) => head.push(byte[0]),
                            }
                        }
                        let text = String::from_utf8_lossy(&head).into_owned();
                        let authorization: Vec<String> = text
                            .split("\r\n")
                            .filter(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                            .map(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()).unwrap_or_default())
                            .collect();
                        recorder.lock().expect("lock").extend(authorization.iter().cloned());
                        let body = format!("you sent [{}]\n", authorization.join(" AND "));
                        let answer = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = tls.write_all(answer.as_bytes());
                        let _ = tls.flush();
                        conn.send_close_notify();
                        let _ = conn.complete_io(&mut sock);
                    });
                }
            });
            Origin { port, seen }
        }

        fn authorizations(&self) -> Vec<String> {
            self.seen.lock().expect("lock").clone()
        }
    }

    fn roots(pems: &[&Path]) -> rustls::RootCertStore {
        use rustls::pki_types::pem::PemObject;
        let mut store = rustls::RootCertStore::empty();
        for pem in pems {
            for cert in rustls::pki_types::CertificateDer::pem_file_iter(pem).expect("pem") {
                store.add(cert.expect("cert")).expect("add");
            }
        }
        store
    }

    /// What a capsule saw when it opened one `CONNECT`.
    struct Seen {
        established: bool,
        handshake: Result<(), String>,
        body: String,
    }

    /// Open one `CONNECT` through `serve` and speak TLS over it.
    ///
    /// This is the capsule's side of the tunnel, which is the side the whole
    /// property is about: it trusts exactly the roots it is given and nothing
    /// else, the way a capsule's browser trusts exactly what was installed in
    /// its profile.
    fn through_the_proxy(
        allow: &Allowlist,
        intercept: Option<&crate::intercept::Intercept>,
        target: &str,
        trust: rustls::RootCertStore,
    ) -> Seen {
        let (server, client) = UnixStream::pair().expect("socketpair");
        std::thread::scope(|scope| {
            scope.spawn(|| serve(1, server, allow, intercept));

            let mut capsule = client;
            capsule
                .write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes())
                .expect("connect line");
            capsule.set_read_timeout(Some(ANSWER_DEADLINE)).ok();
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match (&capsule).read(&mut byte) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => head.push(byte[0]),
                }
            }
            let answer = String::from_utf8_lossy(&head).into_owned();
            if !answer.starts_with("HTTP/1.1 200") {
                return Seen {
                    established: false,
                    handshake: Err(answer),
                    body: String::new(),
                };
            }

            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("versions")
                .with_root_certificates(trust)
                .with_no_client_auth();
            let name = rustls::pki_types::ServerName::try_from("127.0.0.1").expect("name");
            let mut conn =
                rustls::ClientConnection::new(Arc::new(config), name).expect("client connection");
            while conn.is_handshaking() {
                if let Err(e) = conn.complete_io(&mut capsule) {
                    return Seen {
                        established: true,
                        handshake: Err(e.to_string()),
                        body: String::new(),
                    };
                }
            }
            let mut tls = rustls::Stream::new(&mut conn, &mut capsule);
            let _ = tls.write_all(b"GET /page HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
            let _ = tls.flush();
            let mut body = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                match tls.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => body.extend_from_slice(&buf[..n]),
                }
            }
            Seen {
                established: true,
                handshake: Ok(()),
                body: String::from_utf8_lossy(&body).into_owned(),
            }
        })
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rime-egress-{}-{tag}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    fn capsule_record(service: &str) -> rime_secret_core::capability::CapabilityRecord {
        let mut rec = rime_secret_core::capability::CapabilityRecord::new(
            service,
            rime_secret_core::operation::BROWSER_PRESENT,
            "",
        );
        rec.project = Some("/tmp/capsule".into());
        rec
    }

    /// The whole of "which destination is read and which is not", from the
    /// only place it can be observed.
    #[test]
    fn the_pinned_destination_is_terminated_and_every_other_stays_an_opaque_tunnel() {
        let dir = scratch("terminate");
        let pinned_pki = pki(&dir, "pinned");
        let other_pki = pki(&dir, "other");
        let pinned = Origin::start(&pinned_pki);
        let other = Origin::start(&other_pki);

        let allow = Allowlist::parse(&[
            format!("127.0.0.1:{}", pinned.port),
            format!("127.0.0.1:{}", other.port),
        ])
        .expect("allowlist");
        let pin = Destination::parse(&format!("127.0.0.1:{}", pinned.port)).expect("pin");
        // A socket nothing is listening on: the certificate a capsule is shown
        // is decided before the secret service is ever contacted, and this
        // test is about the certificate.
        let minted = crate::intercept::mint(
            &dir,
            &pin,
            capsule_record("intranet"),
            dir.join("no-such-secret-service.sock"),
        )
        .expect("mint");
        let intercept = &minted.intercept;

        let per_run = || roots(&[&minted.ca]);
        let pinned_own = || roots(&[&pinned_pki.ca]);
        let other_own = || roots(&[&other_pki.ca]);

        // THE PIN IS TERMINATED. The capsule is handed a leaf the daemon
        // minted, so the root that verifies it is the per-run CA and the
        // origin's own authority does not.
        let seen = through_the_proxy(&allow, Some(intercept), &pin.to_string(), per_run());
        assert!(seen.established, "the tunnel was refused: {:?}", seen.handshake);
        assert!(
            seen.handshake.is_ok(),
            "the capsule did not accept the minted leaf: {:?}",
            seen.handshake
        );
        let seen = through_the_proxy(&allow, Some(intercept), &pin.to_string(), pinned_own());
        assert!(
            seen.handshake.is_err(),
            "the ORIGIN's own authority verified this handshake, so the daemon did not \
             terminate it"
        );

        // EVERY OTHER DESTINATION IS A TUNNEL. The capsule reaches the
        // origin's own certificate, which the daemon holds no key for and
        // could not have presented.
        let elsewhere = format!("127.0.0.1:{}", other.port);
        let seen = through_the_proxy(&allow, Some(intercept), &elsewhere, other_own());
        assert!(
            seen.handshake.is_ok(),
            "a destination that is not the pin did not reach its own certificate: {:?}",
            seen.handshake
        );
        assert!(seen.body.contains("200 OK"), "{}", seen.body);
        let seen = through_the_proxy(&allow, Some(intercept), &elsewhere, per_run());
        assert!(
            seen.handshake.is_err(),
            "the per-run CA verified a destination that was supposed to be tunnelled, which \
             means the daemon terminated it"
        );

        // AND A SESSION THAT ASKED FOR NOTHING GETS WHAT IT ALWAYS GOT. The
        // control that separates "the pin is terminated" from "this daemon
        // terminates whatever it is given".
        let seen = through_the_proxy(&allow, None, &pin.to_string(), pinned_own());
        assert!(
            seen.handshake.is_ok(),
            "a session with no credential had its connection terminated: {:?}",
            seen.handshake
        );
        assert!(seen.body.contains("200 OK"), "{}", seen.body);
        let seen = through_the_proxy(&allow, None, &pin.to_string(), per_run());
        assert!(
            seen.handshake.is_err(),
            "a session with no credential was handed a minted leaf"
        );

        // Nothing was authenticated anywhere in this test: the secret service
        // it was pointed at does not exist. The origins agree.
        assert!(
            pinned.authorizations().iter().all(|a| a.is_empty()),
            "{:?}",
            pinned.authorizations()
        );
        assert!(
            other.authorizations().iter().all(|a| a.is_empty()),
            "{:?}",
            other.authorizations()
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The secret service, on a private socket and store, started by this test
    /// and killed with it.
    ///
    /// The binary is found beside this test's own executable rather than
    /// through `CARGO_BIN_EXE_rime-secretd`, which cargo defines only for
    /// integration tests of the package that declares the binary — and `serve`
    /// is private to this one. It PANICS when the binary is not there rather
    /// than skipping: a fixture that cannot tell "not built" from "passed" is
    /// a defect this repository has shipped before.
    ///
    /// **It can be STALE, and nothing here can tell.** `cargo test -p
    /// rime-agentd` does not rebuild another package's binary, so this test
    /// runs against whatever `rime-secretd` was last compiled. Two mutations
    /// of `present.rs` were measured as GREEN before that was noticed and were
    /// red the moment the binary was rebuilt. `cargo test --workspace` — what
    /// CI runs — builds every package's binaries first and does not have the
    /// problem; anyone mutating `rime-secretd` and running this file alone
    /// must `cargo build -p rime-secretd` between the edit and the run.
    struct SecretService {
        child: std::process::Child,
        socket: PathBuf,
    }

    impl SecretService {
        fn start(dir: &Path, trust: &Path) -> SecretService {
            // The workspace's binaries sit in the profile directory
            // (`target/debug`). The test binary is in `target/debug/deps` on
            // older Cargo and in `target/debug/build/<pkg>/<hash>/out` on
            // newer Cargo, so look upwards rather than a fixed two levels.
            let exe = std::env::current_exe().expect("the test's own path");
            let binary = exe
                .ancestors()
                .skip(1)
                .map(|dir| dir.join("rime-secretd"))
                .find(|p| p.is_file())
                .unwrap_or_else(|| {
                    exe.parent()
                        .and_then(|deps| deps.parent())
                        .map(|target| target.join("rime-secretd"))
                        .expect("a target directory above this test")
                });
            assert!(
                binary.exists(),
                "{} is not built, so this test would assert nothing. Run `cargo build -p \
                 rime-secretd` or `cargo test --workspace`",
                binary.display()
            );
            let socket = dir.join("secretd.sock");
            let child = std::process::Command::new(&binary)
                .arg("--socket")
                .arg(&socket)
                .arg("--store")
                .arg(dir.join("store"))
                // The origin's certificate is signed by a CA this test minted
                // a moment ago, so the machine's own trust store knows nothing
                // about it. This is the variable `rustls-native-certs` reads
                // first, and it is the same one `curl` and `git` honour — not
                // a hook this test asked for.
                .env("SSL_CERT_FILE", trust)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("start rime-secretd");
            let service = SecretService { child, socket };
            for _ in 0..200 {
                if rime_secret_core::client::Client::connect_at(&service.socket).is_ok() {
                    return service;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            panic!("rime-secretd did not come up on {}", service.socket.display());
        }

        fn client(&self) -> rime_secret_core::client::Client {
            rime_secret_core::client::Client::connect_at(&self.socket).expect("connect")
        }
    }

    impl Drop for SecretService {
        fn drop(&mut self) {
            // Only ever this test's own child.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// The whole chain, from the capsule's socket to the site's log.
    ///
    /// Three claims in one run, and none of them is worth anything without the
    /// other two: the site was sent the credential, the capsule was not, and
    /// the capsule is the thing that asked.
    #[test]
    fn a_capsules_request_reaches_the_site_with_a_credential_it_is_never_given() {
        const SENTINEL: &str = "rime-egress-4b71ef03-do-not-leak";

        let dir = scratch("chain");
        let site_pki = pki(&dir, "site");
        let site = Origin::start(&site_pki);
        let service = SecretService::start(&dir, &site_pki.ca);

        service
            .client()
            .add(
                "intranet",
                "127.0.0.1",
                "https",
                Some("x-access-token"),
                "",
                "bearer",
                Some(site.port),
                &rime_secret_core::SecretValue::new(SENTINEL.into()),
            )
            .expect("add");
        service
            .client()
            .call(&rime_secret_core::protocol::Request::Grant {
                // What an owner types for a capsule: `--everywhere`, because a
                // capsule's working directory is a throwaway tree that will
                // not exist the next time one runs.
                project: rime_secret_core::store::ANY_PROJECT.to_string(),
                service: "intranet".into(),
                capability: rime_secret_core::operation::BROWSER_PRESENT.into(),
                revoke: false,
            })
            .expect("grant");

        let allow = Allowlist::parse(&[format!("127.0.0.1:{}", site.port)]).expect("allowlist");
        let pin = Destination::parse(&format!("127.0.0.1:{}", site.port)).expect("pin");
        let minted = crate::intercept::mint(
            &dir,
            &pin,
            capsule_record("intranet"),
            service.socket.clone(),
        )
        .expect("mint");

        let seen = through_the_proxy(
            &allow,
            Some(&minted.intercept),
            &pin.to_string(),
            roots(&[&minted.ca]),
        );
        assert!(seen.established, "the tunnel was refused: {:?}", seen.handshake);
        assert!(seen.handshake.is_ok(), "{:?}", seen.handshake);
        assert!(seen.body.starts_with("HTTP/1.1 200 OK"), "{}", seen.body);

        // The site was sent the credential — so this run is about
        // authentication and not about a tunnel that happened to work.
        assert_eq!(
            site.authorizations(),
            vec![format!("Bearer {SENTINEL}")],
            "the site was not authenticated"
        );

        // And the capsule was not, even though the site sent it straight back.
        // Same length, because the site declared a `Content-Length` the
        // browser is going to read to the end of.
        assert!(
            !seen.body.contains(SENTINEL),
            "the credential reached the capsule: {}",
            seen.body
        );
        assert!(
            seen.body.contains(&format!("you sent [Bearer {}]", "x".repeat(SENTINEL.len()))),
            "the echo was not redacted in place: {}",
            seen.body
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// How long a test will wait for the proxy's answer before calling it a
    /// defect. Generous: the work behind every one of these is microseconds.
    const ANSWER_DEADLINE: Duration = Duration::from_secs(10);

    /// Read the proxy's answer to end-of-file, with a deadline.
    ///
    /// These tests used `read_to_string` with no timeout, and that waits for
    /// EVERY copy of the far end to be closed — not just the one this test
    /// handed to `serve`. A copy can be somewhere the test has never heard of.
    /// It was: `pty::spawn` forked while these sockets were open, the child
    /// wedged before exec, and because it never exec'd, FD_CLOEXEC never fired
    /// and it held the server end open for as long as it lived. The read here
    /// then waited for an end-of-file that could no longer come.
    ///
    /// That child is fixed. This deadline is here because the next fd-holder
    /// will not be: a test with no deadline does not fail, it HANGS, reporting
    /// neither pass nor fail until somebody notices hours later. With one, the
    /// same defect is a named failure in ten seconds — and the name says where
    /// to look, because "an inherited descriptor" is not the first guess anyone
    /// makes when a proxy test stops returning.
    fn read_answer(client: &UnixStream) -> String {
        client
            .set_read_timeout(Some(ANSWER_DEADLINE))
            .expect("set a read deadline");
        let mut reader: &UnixStream = client;
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                // The connection ending abruptly IS an end: `serve` drops a
                // client it refuses to keep reading from, and one of these
                // tests asserts exactly that. Only a DEADLINE means the far end
                // is still held open, which is the defect this guard is for.
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::BrokenPipe
                    ) =>
                {
                    break
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    panic!(
                        "the server end of this socket pair was STILL OPEN after {:?}, \
                         with {} bytes read. `serve` has returned, so something else is \
                         holding a copy of it — an inherited descriptor in a forked child \
                         that has not reached exec is how this happened before (see \
                         pty::spawn). Partial answer: {:?}",
                        ANSWER_DEADLINE,
                        out.len(),
                        String::from_utf8_lossy(&out)
                    );
                }
                Err(e) => panic!("reading the proxy's answer: {e}"),
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn a_connect_line_is_read_and_anything_else_is_not() {
        // The shapes a real client sends, including the ones that would make a
        // naive `starts_with("CONNECT ")` wrong.
        for (head, want) in [
            ("CONNECT api.example.com:443 HTTP/1.1\r\nHost: x\r\n\r\n", Some("api.example.com:443")),
            ("connect api.example.com:443 HTTP/1.1\r\n\r\n", Some("api.example.com:443")),
            ("CONNECT   api.example.com:443   HTTP/1.1\r\n\r\n", Some("api.example.com:443")),
            ("CONNECT [2001:db8::1]:443 HTTP/1.1\r\n\r\n", Some("[2001:db8::1]:443")),
            // Absolute-form plain HTTP: refused, not rewritten.
            ("GET http://api.example.com/v1 HTTP/1.1\r\n\r\n", None),
            ("POST http://api.example.com/ HTTP/1.1\r\n\r\n", None),
            // Not the protocol at all.
            ("\x16\x03\x01\x02\x00", None),
            ("", None),
            ("CONNECT\r\n\r\n", None),
            ("CONNECT api.example.com:443\r\n\r\n", None),
        ] {
            assert_eq!(
                connect_target(head).as_deref(),
                want,
                "{}",
                head.escape_debug()
            );
        }
    }

    #[test]
    fn a_bridge_invocation_is_recognised_and_a_daemon_one_is_not() {
        let parsed = parse_bridge_argv(&[
            BRIDGE_FLAG,
            "/tmp/rime-agent/7/egress.sock",
            "3128",
            "--",
            "claude",
            "--permission-mode",
            "bypassPermissions",
        ])
        .expect("a bridge invocation")
        .expect("parses");
        assert_eq!(parsed.socket, PathBuf::from("/tmp/rime-agent/7/egress.sock"));
        assert_eq!(parsed.port, 3128);
        assert_eq!(parsed.program, "claude");
        assert_eq!(parsed.args, vec!["--permission-mode", "bypassPermissions"]);

        // The daemon's own argv, which must fall straight through.
        assert!(parse_bridge_argv::<&str>(&[]).is_none());
        assert!(parse_bridge_argv(&["--version"]).is_none());
    }

    #[test]
    fn a_malformed_bridge_invocation_is_an_error_not_a_second_daemon() {
        // Falling through to the daemon here would start a second one inside
        // somebody's sandbox, which is a confusing way to lose a session.
        for argv in [
            vec![BRIDGE_FLAG],
            vec![BRIDGE_FLAG, "/tmp/s.sock"],
            vec![BRIDGE_FLAG, "/tmp/s.sock", "not-a-port", "--", "claude"],
            vec![BRIDGE_FLAG, "/tmp/s.sock", "0", "--", "claude"],
            vec![BRIDGE_FLAG, "/tmp/s.sock", "3128", "claude"],
            vec![BRIDGE_FLAG, "/tmp/s.sock", "3128", "--"],
        ] {
            let parsed = parse_bridge_argv(&argv).expect("a bridge invocation");
            assert!(parsed.is_err(), "{argv:?} was accepted");
        }
    }

    #[test]
    fn the_proxy_answers_a_denied_destination_with_the_reason() {
        // End to end over a real socket pair, without a network: the
        // destination is refused before anything is resolved, so no name in
        // this test is ever looked up.
        let allow = Allowlist::parse(&["api.example.com"]).expect("allowlist");
        let (client, server) = UnixStream::pair().expect("socketpair");
        let worker = std::thread::spawn(move || serve(1, server, &allow, None));

        let mut client_w = client.try_clone().unwrap();
        client_w
            .write_all(b"CONNECT secrets.example.com:443 HTTP/1.1\r\n\r\n")
            .unwrap();
        let answer = read_answer(&client);
        worker.join().unwrap();

        assert!(answer.starts_with("HTTP/1.1 403 Forbidden"), "{answer}");
        assert!(answer.contains("secrets.example.com"), "{answer}");
        assert!(answer.contains("rime agent allow"), "{answer}");
    }

    #[test]
    fn plain_http_through_the_proxy_is_refused_rather_than_rewritten() {
        let allow = Allowlist::parse(&["api.example.com"]).expect("allowlist");
        let (client, server) = UnixStream::pair().expect("socketpair");
        let worker = std::thread::spawn(move || serve(1, server, &allow, None));

        let mut client_w = client.try_clone().unwrap();
        client_w
            .write_all(b"GET http://api.example.com/v1 HTTP/1.1\r\nHost: api.example.com\r\n\r\n")
            .unwrap();
        let answer = read_answer(&client);
        worker.join().unwrap();

        // Refused even though the host IS on the allowlist: the method is the
        // problem, and a proxy that rewrote the request would be a request
        // parser nobody asked for.
        assert!(answer.starts_with("HTTP/1.1 405"), "{answer}");
        assert!(answer.contains("CONNECT"), "{answer}");
    }

    #[test]
    fn a_head_that_never_ends_is_abandoned_rather_than_held_open() {
        let allow = Allowlist::parse(&["api.example.com"]).expect("allowlist");
        let (client, server) = UnixStream::pair().expect("socketpair");
        let worker = std::thread::spawn(move || serve(1, server, &allow, None));

        let mut client_w = client.try_clone().unwrap();
        // No blank line, ever, and more than the cap allows.
        let junk = vec![b'A'; MAX_HEAD_BYTES + 1024];
        let _ = client_w.write_all(&junk);
        let answer = read_answer(&client);
        worker.join().unwrap();
        assert!(answer.is_empty(), "expected the connection to be dropped: {answer}");
    }

    #[test]
    fn a_destination_that_is_not_a_host_and_port_is_a_bad_request() {
        let allow = Allowlist::parse(&["api.example.com"]).expect("allowlist");
        let (client, server) = UnixStream::pair().expect("socketpair");
        let worker = std::thread::spawn(move || serve(1, server, &allow, None));

        let mut client_w = client.try_clone().unwrap();
        client_w
            .write_all(b"CONNECT api.example.com HTTP/1.1\r\n\r\n")
            .unwrap();
        let answer = read_answer(&client);
        worker.join().unwrap();
        assert!(answer.starts_with("HTTP/1.1 400"), "{answer}");
    }
}
