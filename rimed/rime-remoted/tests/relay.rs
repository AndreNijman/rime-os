//! The relay path, end to end, against a relay that is not Cloudflare's.
//!
//! ## Why a double and not a deployment
//!
//! The relay this is written for is a Cloudflare Worker plus a Durable
//! Object, and deploying one is a decision about somebody's account and
//! somebody's money. It is not a thing a test suite gets to do, and a suite
//! that needed it would be a suite that never runs.
//!
//! So the relay here is a `TcpListener` on loopback that implements the same
//! room protocol the Worker under `relay/` implements: two roles on a
//! rendezvous id, one waiting host, an opaque byte copy once they are joined.
//! It is the same shape `rime-secretd`'s Cloudflare provider uses for the
//! API — a loopback double that refuses anything the real thing would refuse
//! — and it buys the same thing: every claim below is about code that ran.
//!
//! **The double is an independent implementation on purpose.** It does not
//! use `rime_remote_core::relay`'s codec. If it did, a codec that masked
//! wrongly or framed a length wrongly would agree with itself and the suite
//! would prove nothing about the wire.
//!
//! ## What it is allowed to conclude, and what it is not
//!
//! It concludes that the client speaks the protocol, that the desktop is
//! reachable through a relay it dialled outbound, that the relay carries
//! ciphertext, and that a relayed session is labelled as one. It does **not**
//! conclude that the TypeScript Worker is correct — that needs `wrangler dev`
//! and is written down in the card as the pre-deploy step.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rime_remote_core::noise::{Channel, Handshake};
use rime_remote_core::pairing::{PairingOffer, PairingRequest};
use rime_remote_core::relay::{
    Endpoint, Notice, Op, Opening, Receiver as WsReceiver, Role, Sender as WsSender,
};
use rime_remote_core::wire::Frame;

const V: u32 = rime_remote_core::REMOTE_PROTOCOL_VERSION;

// `rime-remoted` declares these as `pub const` in a binary crate, which an
// integration test cannot import.
const HELLO_PAIR: u8 = b'P';
const HELLO_SESSION: u8 = b'S';

/// A string distinctive enough that finding it anywhere is conclusive.
const SENTINEL: &str = "rime-relay-sentinel-4f2a91c7-never-in-the-clear";

// ─────────────────────────────────────────────────────────────────────────────
//  The relay double
// ─────────────────────────────────────────────────────────────────────────────

/// What the relay observed. Everything an operator could keep.
#[derive(Default)]
struct Observed {
    /// The role of every connection, in arrival order.
    arrivals: Vec<String>,
    /// Every request target, so a path or a role can be asserted about.
    targets: Vec<String>,
    /// Every byte of every payload copied, in either direction.
    ///
    /// This is the operator's view of the session. The point of the suite is
    /// that nothing recognisable is ever in here.
    payloads: Vec<Vec<u8>>,
    /// Every text frame a client sent. Must stay empty: this end never asks a
    /// relay anything, and a client that chattered would be a client whose
    /// traffic an operator could correlate by content.
    client_text: Vec<Vec<u8>>,
    /// Status codes the relay refused with.
    refusals: Vec<u16>,
}

struct Relay {
    host: String,
    port: u16,
    seen: Arc<Mutex<Observed>>,
    /// Every socket the relay is holding, so a test can drop them all.
    cut: Arc<Mutex<Vec<TcpStream>>>,
    /// Set when `RIME_RELAY_URL` pointed the suite at a relay that is not the
    /// double — in practice `wrangler dev --local` serving `relay/src/index.js`.
    /// `None` is the normal run and nothing about it changes.
    external: Option<String>,
}

impl Relay {
    fn start() -> Relay {
        // `RIME_RELAY_URL=ws://127.0.0.1:8787` runs this suite against the
        // real Worker instead of the double. §4 of
        // ROADMAP/design/P1-052-relay.md records the Worker as the one piece
        // of P1-052 that had never been executed, because running it needs
        // wrangler; `wrangler dev --local` needs no Cloudflare account, so the
        // gap was a missing dependency rather than a missing permission.
        //
        // The double stays the default, and a default run is byte-identical
        // to what it was before this hook existed.
        if let Ok(url) = std::env::var("RIME_RELAY_URL") {
            let endpoint = Endpoint::parse(&url).expect("RIME_RELAY_URL is not a relay address");
            // `wss://` is refused BY DEFAULT and allowed with
            // `RIME_RELAY_TLS=1`, because the two TLS endpoints this suite can
            // meet are not the same thing.
            //
            // Against `wrangler dev --local` the certificate is self-signed, so
            // a run would only re-prove the refusal that rime-remote-core's tls
            // suite already proves against a minted CA — noise, not evidence.
            //
            // Against a DEPLOYED relay the certificate is real, and then this
            // is the only test anywhere that drives the whole stack end to end:
            // rustls, the machine's own root store, Cloudflare's edge, and the
            // Worker's Durable Object. Nothing else covers that seam, so it is
            // reachable rather than forbidden — behind a flag, so a default run
            // is unchanged and nobody points it at production by accident.
            assert!(
                !endpoint.secure || std::env::var_os("RIME_RELAY_TLS").is_some(),
                "this suite drives a relay over ws://. Set RIME_RELAY_TLS=1 to allow \
                 wss://, which is worth doing against a DEPLOYED relay with a real \
                 certificate and pointless against wrangler's self-signed one — what \
                 TLS refuses is already proved in rime-remote-core's tls suite."
            );
            return Relay {
                host: endpoint.host.clone(),
                port: endpoint.port,
                seen: Arc::new(Mutex::new(Observed::default())),
                cut: Arc::new(Mutex::new(Vec::new())),
                external: Some(url),
            };
        }
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback");
        let port = listener.local_addr().expect("addr").port();
        let seen = Arc::new(Mutex::new(Observed::default()));
        let cut: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));
        let rooms: Rooms = Arc::new(Mutex::new(HashMap::new()));
        let recorder = Arc::clone(&seen);
        let holder = Arc::clone(&cut);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let rooms = Arc::clone(&rooms);
                let recorder = Arc::clone(&recorder);
                let holder = Arc::clone(&holder);
                std::thread::spawn(move || {
                    let _ = serve(stream, rooms, recorder, holder);
                });
            }
        });
        Relay { host: "127.0.0.1".into(), port, seen, cut, external: None }
    }

    fn url(&self) -> String {
        match &self.external {
            Some(url) => url.clone(),
            None => format!("ws://127.0.0.1:{}", self.port),
        }
    }

    fn is_external(&self) -> bool {
        self.external.is_some()
    }

    /// What the relay operator saw.
    ///
    /// A real relay does not hand this over, so asking for it in external mode
    /// is a test that should have skipped. It panics rather than returning an
    /// empty `Observed`, because an empty one would make `assert!(x.is_empty())`
    /// pass and turn a skipped claim into a claim that looked proved.
    fn seen(&self) -> std::sync::MutexGuard<'_, Observed> {
        assert!(
            !self.is_external(),
            "this assertion reads the double's own record, which a real relay does \
             not provide — the test should have skipped in external mode"
        );
        self.seen.lock().expect("observed")
    }

    /// Is a desktop parked on this rendezvous right now?
    ///
    /// Asked by dialling as a HOST and expecting to be refused: a 409 is the
    /// relay saying somebody already holds the room, which is precisely the
    /// question. This replaces counting arrivals when the suite is pointed at
    /// a real relay, and it is the stronger question of the two — it is the
    /// Durable Object's own `waiting()` answering about live sockets, rather
    /// than a tally of connections that once arrived and may since have died.
    /// That distinction is the bug §2 records the double having had.
    ///
    /// A 101 means the room was empty and this probe has just taken it. The
    /// socket is shut immediately so the room is freed and the desktop can
    /// re-arm on its own backoff; the caller retries.
    fn host_is_parked(&self, rendezvous: &str) -> bool {
        let endpoint = Endpoint::parse(&self.url()).expect("endpoint");
        let Ok(socket) = TcpStream::connect((self.host.as_str(), self.port)) else {
            return false;
        };
        socket.set_read_timeout(Some(Duration::from_secs(5))).ok();
        let (Ok(mut writing), Ok(mut reading)) = (socket.try_clone(), socket.try_clone()) else {
            return false;
        };
        let opening = Opening::new();
        if writing
            .write_all(&opening.request(&endpoint, rendezvous, Role::Host))
            .is_err()
        {
            return false;
        }
        let _ = writing.flush();
        match opening.accept(&mut reading) {
            Err(e) => format!("{e}").contains("409"),
            Ok(()) => {
                let _ = socket.shutdown(std::net::Shutdown::Both);
                false
            }
        }
    }

    /// Drop every connection the relay holds, the way a network change does:
    /// no close frame, no warning, a socket that simply stops being there.
    fn cut_everything(&self) {
        assert!(
            !self.is_external(),
            "cutting every socket reaches inside the double — the test should have \
             skipped in external mode"
        );
        if let Ok(mut v) = self.cut.lock() {
            for s in v.drain(..) {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
        }
    }

}

/// A waiting host: the parked socket, and whether a guest has claimed it.
type Rooms = Arc<Mutex<HashMap<String, (TcpStream, Arc<AtomicBool>)>>>;

/// One connection, from its request line to the end of its session.
fn serve(
    mut stream: TcpStream,
    rooms: Rooms,
    seen: Arc<Mutex<Observed>>,
    cut: Arc<Mutex<Vec<TcpStream>>>,
) -> std::io::Result<()> {
    if let Ok(s) = stream.try_clone() {
        cut.lock().expect("cut").push(s);
    }
    // The request head, byte at a time, so nothing after it is swallowed.
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte)? == 0 {
            return Ok(());
        }
        head.push(byte[0]);
        if head.len() > 8192 {
            return Ok(());
        }
    }
    let text = String::from_utf8_lossy(&head).to_string();
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let target = request_line.split_whitespace().nth(1).unwrap_or("").to_string();
    let mut key = String::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("sec-websocket-key") {
                key = value.trim().to_string();
            }
        }
    }
    seen.lock().expect("seen").targets.push(target.clone());

    let refuse = |stream: &mut TcpStream, code: u16, why: &str| -> std::io::Result<()> {
        seen.lock().expect("seen").refusals.push(code);
        write!(
            stream,
            "HTTP/1.1 {code} {why}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?;
        stream.flush()
    };

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target.as_str(), ""),
    };
    let Some(rendezvous) = path.strip_prefix("/r/") else {
        return refuse(&mut stream, 404, "Not Found");
    };
    if rendezvous.is_empty() || rendezvous.contains('/') {
        return refuse(&mut stream, 404, "Not Found");
    }
    let role = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("role="))
        .and_then(Role::parse);
    let Some(role) = role else {
        return refuse(&mut stream, 400, "Bad Request");
    };
    if key.is_empty() {
        return refuse(&mut stream, 400, "Bad Request");
    }

    // The room decision happens BEFORE the upgrade, so a refusal is an HTTP
    // status the client can read rather than a close frame it has to guess at.
    match role {
        Role::Host => {
            // The check and the claim are ONE critical section, and the claim
            // comes before the 101: otherwise a second host that dials the
            // instant the first one reads its 101 finds the room still empty
            // (the CI flake "host number 2 was not treated as an intruder").
            let taken = Arc::new(AtomicBool::new(false));
            {
                let mut rooms = rooms.lock().expect("rooms");
                if rooms.contains_key(rendezvous) {
                    drop(rooms);
                    return refuse(&mut stream, 409, "Conflict");
                }
                rooms.insert(rendezvous.to_string(), (stream.try_clone()?, Arc::clone(&taken)));
            }
            seen.lock().expect("seen").arrivals.push("host".into());
            let upgraded = accept(&mut stream, &key)
                .and_then(|()| send_text(&mut stream, Notice::Waiting.text().as_bytes()));
            if let Err(e) = upgraded {
                rooms.lock().expect("rooms").remove(rendezvous);
                return Err(e);
            }

            // A REQUIREMENT, not double bookkeeping: a relay that parks a
            // waiting host and never notices when its socket dies answers the
            // desktop's very next dial with 409 for ever, and the machine is
            // unreachable until the daemon is restarted. Found by this suite
            // -- the first version of the double did exactly that, and
            // `a_network_change_costs_the_session_and_costs_nothing_else`
            // failed with "the desktop did not re-arm". The Worker has to
            // evict too; see ROADMAP/design/P1-052-relay.md.
            //
            // `peek` rather than `read`: it blocks until there is something
            // to say and consumes nothing, so the session that may later run
            // over this socket still gets every byte.
            {
                let watch = stream.try_clone()?;
                let rooms = Arc::clone(&rooms);
                let taken = Arc::clone(&taken);
                let id = rendezvous.to_string();
                std::thread::spawn(move || {
                    let mut probe = [0u8; 1];
                    let dead = matches!(watch.peek(&mut probe), Ok(0) | Err(_));
                    if dead && !taken.load(Ordering::SeqCst) {
                        rooms.lock().expect("rooms").remove(&id);
                    }
                });
            }
            // Parked (the room already holds a clone of this socket). The guest that arrives next takes it and does the copy.
            Ok(())
        }
        Role::Guest => {
            let host = rooms.lock().expect("rooms").remove(rendezvous);
            let Some((mut host, taken)) = host else {
                return refuse(&mut stream, 409, "Conflict");
            };
            taken.store(true, Ordering::SeqCst);
            seen.lock().expect("seen").arrivals.push("guest".into());
            accept(&mut stream, &key)?;
            send_text(&mut stream, Notice::Paired.text().as_bytes())?;
            send_text(&mut host, Notice::Paired.text().as_bytes())?;

            let (a1, b1) = (host.try_clone()?, stream.try_clone()?);
            let (a2, b2) = (host, stream);
            let seen2 = Arc::clone(&seen);
            let up = std::thread::spawn(move || copy(a1, b1, seen2));
            let _ = copy(b2, a2, seen);
            let _ = up.join();
            Ok(())
        }
    }
}

fn accept(stream: &mut TcpStream, key: &str) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
         Connection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        rime_remote_core::relay::accept_for(key)
    )?;
    stream.flush()
}

/// Copy one direction, recording every payload the operator would see.
fn copy(mut from: TcpStream, mut to: TcpStream, seen: Arc<Mutex<Observed>>) -> std::io::Result<()> {
    while let Some((op, payload)) = read_client_frame(&mut from)? {
        match op {
            0x2 | 0x0 => {
                seen.lock().expect("seen").payloads.push(payload.clone());
                write_server_frame(&mut to, 0x2, &payload)?;
            }
            0x1 => seen.lock().expect("seen").client_text.push(payload),
            0x9 => write_server_frame(&mut from, 0xa, &payload)?,
            0xa => {}
            0x8 => {
                let _ = write_server_frame(&mut to, 0x8, &[0x03, 0xe8]);
                break;
            }
            _ => break,
        }
    }
    let _ = to.shutdown(std::net::Shutdown::Both);
    Ok(())
}

/// Read one frame as a client must send it: masked, or it is not a client.
///
/// Deliberately its own implementation rather than the shipped decoder. Two
/// halves of one codec agreeing proves that the code is self-consistent, not
/// that it is right.
fn read_client_frame(from: &mut TcpStream) -> std::io::Result<Option<(u8, Vec<u8>)>> {
    let mut head = [0u8; 2];
    if let Err(e) = from.read_exact(&mut head) {
        return if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Ok(None)
        } else {
            Err(e)
        };
    }
    let op = head[0] & 0x0f;
    assert_ne!(head[1] & 0x80, 0, "a client frame that is not masked");
    let len = match head[1] & 0x7f {
        126 => {
            let mut n = [0u8; 2];
            from.read_exact(&mut n)?;
            u16::from_be_bytes(n) as usize
        }
        127 => {
            let mut n = [0u8; 8];
            from.read_exact(&mut n)?;
            u64::from_be_bytes(n) as usize
        }
        short => short as usize,
    };
    let mut mask = [0u8; 4];
    from.read_exact(&mut mask)?;
    let mut payload = vec![0u8; len];
    from.read_exact(&mut payload)?;
    for (i, b) in payload.iter_mut().enumerate() {
        *b ^= mask[i % 4];
    }
    Ok(Some((op, payload)))
}

fn write_server_frame(to: &mut TcpStream, op: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut out = vec![0x80 | op];
    let n = payload.len();
    if n < 126 {
        out.push(n as u8);
    } else if n <= u16::MAX as usize {
        out.push(126);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(127);
        out.extend_from_slice(&(n as u64).to_be_bytes());
    }
    out.extend_from_slice(payload);
    to.write_all(&out)?;
    to.flush()
}

fn send_text(to: &mut TcpStream, text: &[u8]) -> std::io::Result<()> {
    write_server_frame(to, 0x1, text)
}

// ─────────────────────────────────────────────────────────────────────────────
//  The daemons
// ─────────────────────────────────────────────────────────────────────────────

fn bin(name: &str) -> PathBuf {
    let mine = PathBuf::from(env!("CARGO_BIN_EXE_rime-remoted"));
    let dir = mine.parent().expect("a target directory");
    let path = dir.join(name);
    assert!(
        path.exists(),
        "{} is not built. This suite drives both daemons, so run it as \
         `cargo test --workspace` (or `cargo build -p rime-agentd` first).",
        path.display()
    );
    path
}

struct Harness {
    agentd: Child,
    remoted: Child,
    root: PathBuf,
    agentd_socket: PathBuf,
    remoted_socket: PathBuf,
    port: u16,
    relay: Relay,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.remoted.kill();
        let _ = self.remoted.wait();
        let _ = self.agentd.kill();
        let _ = self.agentd.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Harness {
    fn start(tag: &str) -> Option<Harness> {
        // The relay comes up first, so the daemon's very first dial succeeds
        // and the suite is not measuring the backoff.
        let relay = Relay::start();

        let root = std::env::temp_dir().join(format!(
            "rime-relay-e2e-{}-{tag}-{}",
            std::process::id(),
            rime_remote_core::now_ms()
        ));
        let runtime = root.join("run");
        let state = root.join("state");
        std::fs::create_dir_all(&runtime).ok()?;
        std::fs::create_dir_all(&state).ok()?;

        let agentd = Command::new(bin("rime-agentd"))
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("XDG_STATE_HOME", &state)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        let port = {
            let probe = TcpListener::bind(("127.0.0.1", 0)).ok()?;
            let p = probe.local_addr().ok()?.port();
            drop(probe);
            p
        };

        let remoted = Command::new(bin("rime-remoted"))
            .args([
                "--port",
                &port.to_string(),
                "--relay",
                &relay.url(),
                "--allow-foreground",
                "--handshake-timeout-ms",
                "1000",
                // The shipped interval is fifteen seconds, which is right for
                // a phone and wrong for a suite that has to watch a
                // measurement happen. The daemon clamps this at 50 ms.
                "--ping-interval-ms",
                "100",
            ])
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("XDG_STATE_HOME", &state)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        let h = Harness {
            agentd,
            remoted,
            root,
            agentd_socket: runtime.join("rime-agentd").join("control.sock"),
            remoted_socket: runtime.join("rime-remoted").join("control.sock"),
            port,
            relay,
        };
        h.wait_ready().then_some(h)
    }

    fn wait_ready(&self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let up = UnixStream::connect(&self.agentd_socket).is_ok()
                && UnixStream::connect(&self.remoted_socket).is_ok()
                && TcpStream::connect(("127.0.0.1", self.port)).is_ok();
            // And the relay leg: the desktop has to have dialled out and be
            // waiting before a device can be joined to it. Waiting for this
            // rather than sleeping is what makes the suite deterministic.
            if up {
                let parked = if self.relay.is_external() {
                    self.relay.host_is_parked(&self.rendezvous())
                } else {
                    self.relay.seen().arrivals.iter().any(|a| a == "host")
                };
                if parked {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// Wait until the relay is holding `n` host connections in total.
    fn hosts_seen(&self, n: usize) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        if self.relay.is_external() {
            // The tally does not exist against a real relay, so the same
            // question is asked of the relay's live state: is a desktop parked
            // again? "Re-armed" means exactly that, and every caller of this
            // is asking whether the desktop came back after a device consumed
            // its waiting connection.
            let rendezvous = self.rendezvous();
            while Instant::now() < deadline {
                if self.relay.host_is_parked(&rendezvous) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            return false;
        }
        while Instant::now() < deadline {
            if self.relay.seen().arrivals.iter().filter(|a| *a == "host").count() >= n {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }

    fn control(&self, line: &str) -> serde_json::Value {
        let stream = UnixStream::connect(&self.remoted_socket).expect("connect");
        stream.set_read_timeout(Some(Duration::from_secs(15))).ok();
        let mut writer = stream.try_clone().expect("clone");
        let mut reader = BufReader::new(stream);
        writeln!(writer, "{line}").expect("write");
        writer.flush().ok();
        let mut reply = String::new();
        reader.read_line(&mut reply).expect("read");
        serde_json::from_str(reply.trim()).unwrap_or_else(|e| panic!("{e}: {reply}"))
    }

    fn rendezvous(&self) -> String {
        self.control(r#"{"cmd":"status"}"#)["rendezvous"]
            .as_str()
            .expect("a rendezvous id")
            .to_string()
    }

    /// A pairing offer, or `None` because of where this process is running.
    ///
    /// ## What this replaced, and why
    ///
    /// It used to be `reply["qr"].as_str().unwrap_or_else(|| panic!(...))`.
    /// A `cargo test --workspace` run from a systemd user service then failed
    /// five of this file's eight tests on one reply:
    ///
    /// ```text
    /// a scheduled-job request cannot pair a device: pairing hands a phone
    /// standing access to this machine's agents, so §7 reserves it for a
    /// human at the keyboard
    /// ```
    ///
    /// Nothing was broken. `rime_agent_core::origin::classify` reads the
    /// connecting process's cgroup, everything a user's own systemd instance
    /// starts sits below `user@<uid>.service`, and §7 calls that a
    /// `scheduled-job` — so `rime-remoted`'s `may_pair` refused, exactly as
    /// designed, and this suite reported FAILED where the honest answer was
    /// COULD NOT RUN. CI does not see it because the `Tests` step goes through
    /// `tests/in-login-session.sh`, which mints a real logind session through
    /// PAM; the runner's own log says
    /// `0::/user.slice/user-1001.slice/session-3.scope`.
    ///
    /// ## Why it is not simply a skip either
    ///
    /// A skip that fired whenever the daemon said no would swallow two things
    /// worth knowing: a §7 hole that let an unattended process pair a phone,
    /// and PAM failing under `in-login-session.sh` — which would turn five red
    /// tests green on a runner and tell nobody. So the placement is
    /// established INDEPENDENTLY of the reply, from `/proc`, through the same
    /// function the daemon calls, and all four combinations are decided:
    ///
    /// | this process | the daemon | verdict |
    /// |---|---|---|
    /// | local     | offered | run the test |
    /// | local     | refused | FAIL — §7's local column has become unreachable |
    /// | not local | refused | SKIP, naming the placement |
    /// | not local | offered | FAIL — an unattended caller was handed a phone's access |
    ///
    /// `is_local()` rather than a second copy of `MAY_PAIR`: `control.rs`'s
    /// own `every_origin_that_may_pair_is_local_and_no_other_is` asserts the
    /// two agree over all seven origins, so this reads the rule rather than
    /// restating it. `MAY_PAIR` itself is unreachable from here — `rime-remoted`
    /// is a binary-only crate.
    ///
    /// `RIME_REQUIRE_LOCAL_ORIGIN` turns the skip into a failure, the same
    /// shape as `RIME_REQUIRE_RIME_CLI` in rime-agentd's suites. CI sets it on
    /// the step that goes through `in-login-session.sh`; a developer running
    /// this crate from a user service or a cron job does not.
    fn offer(&self) -> Option<PairingOffer> {
        let reply = self.control(r#"{"cmd":"pair"}"#);
        // Asked about this process, because this process is the peer the
        // daemon saw: `control()` connects from here.
        let placement = rime_agent_core::origin::observe_pid(std::process::id() as i32);
        let local = matches!(&placement, Ok(o) if o.is_local());

        if let Some(qr) = reply["qr"].as_str() {
            assert!(
                local,
                "a caller §7 does not call local was handed a pairing code. \
                 /proc says {placement:?}, and the daemon offered anyway: {reply}"
            );
            return Some(PairingOffer::decode(qr).expect("a pairing offer"));
        }

        assert_eq!(reply["reply"], "error", "neither an offer nor an error: {reply}");
        assert!(
            !local,
            "pairing was refused for a caller §7 calls local ({placement:?}), so the \
             local column of §7 cannot be reached at all: {reply}"
        );

        // The refusal has to be ABOUT the placement. `local_caller` can also
        // fail because the kernel would not report peer credentials or because
        // the connection belongs to another account, and neither of those is
        // an environment this suite may shrug at — they mean the harness is
        // wrong, not that a human is absent.
        let why = reply["message"].as_str().unwrap_or_default().to_string();
        let names_the_placement = match &placement {
            Ok(o) => why.contains(o.as_str()),
            Err(_) => why.contains("/proc/") || why.contains("cgroup"),
        };
        assert!(
            names_the_placement,
            "pairing was refused for a reason that is not this process's placement \
             ({placement:?}), so it is not a could-not-run: {reply}"
        );

        assert!(
            std::env::var_os("RIME_REQUIRE_LOCAL_ORIGIN").is_none(),
            "RIME_REQUIRE_LOCAL_ORIGIN is set, so this run was supposed to hold a real \
             logind session and does not: {why}"
        );
        eprintln!("SKIP: this process may not pair — {why}");
        None
    }

    /// One connection to the desktop THROUGH THE RELAY, as a guest.
    ///
    /// A 409 is retried rather than fatal, and that is what a device does
    /// too: the desktop consumes its waiting connection the moment a device
    /// attaches and opens the next one immediately after, so a device that
    /// arrives in that window is told "no host" about a machine that is
    /// running. Treating it as an outage would make every second connection
    /// a failure.
    fn through_the_relay(&self) -> WsPipe {
        let endpoint = Endpoint::parse(&self.relay.url()).expect("endpoint");
        let rendezvous = self.rendezvous();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let socket = TcpStream::connect((self.relay.host.as_str(), self.relay.port))
                .expect("dial relay");
            socket.set_nodelay(true).ok();
            socket.set_read_timeout(Some(Duration::from_secs(20))).ok();
            let mut writing = socket.try_clone().expect("clone");
            let mut reading = socket.try_clone().expect("clone");
            let opening = Opening::new();
            writing
                .write_all(&opening.request(&endpoint, &rendezvous, Role::Guest))
                .expect("request");
            writing.flush().expect("flush");
            match opening.accept(&mut reading) {
                Ok(()) => return self.joined(socket, reading, writing),
                Err(e) if format!("{e}").contains("409") && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("the relay refused the guest: {e}"),
            }
        }
    }

    fn joined(&self, socket: TcpStream, reading: TcpStream, writing: TcpStream) -> WsPipe {
        let _ = socket;
        let mut rx = WsReceiver::new(reading);
        // The relay says `paired` before a byte of the session moves.
        loop {
            let m = rx.message().expect("a notice");
            if m.op == Op::Text {
                assert_eq!(
                    Notice::parse(&m.payload),
                    Some(Notice::Paired),
                    "an unexpected notice: {}",
                    String::from_utf8_lossy(&m.payload)
                );
                break;
            }
        }
        WsPipe {
            rx,
            tx: WsSender::new(writing),
            spare: Vec::new(),
            at: 0,
        }
    }
}

/// A relay connection, as a byte stream.
///
/// This is the device's half of the same idea the daemon's splice implements:
/// the WebSocket carries the ordinary transport, so `read_message`,
/// `write_message`, the Noise handshake and every frame are unchanged.
struct WsPipe {
    rx: WsReceiver<TcpStream>,
    tx: WsSender<TcpStream>,
    spare: Vec<u8>,
    at: usize,
}

impl Read for WsPipe {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.at >= self.spare.len() {
            let m = self
                .rx
                .message()
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            match m.op {
                Op::Binary | Op::Continuation => {
                    self.spare = m.payload;
                    self.at = 0;
                }
                Op::Close => return Ok(0),
                _ => continue,
            }
        }
        let n = (self.spare.len() - self.at).min(out.len());
        out[..n].copy_from_slice(&self.spare[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

impl Write for WsPipe {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.tx.binary(bytes)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn write_message(w: &mut impl Write, message: &[u8]) {
    w.write_all(&(message.len() as u32).to_be_bytes()).expect("length");
    w.write_all(message).expect("message");
    w.flush().expect("flush");
}

fn read_message(r: &mut impl Read) -> Vec<u8> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).expect("a length");
    let mut buf = vec![0u8; u32::from_be_bytes(len) as usize];
    r.read_exact(&mut buf).expect("a message");
    buf
}

/// A device with a keypair, which only ever dials the relay.
struct Device {
    secret: [u8; 32],
    public: [u8; 32],
}

impl Device {
    fn new() -> Device {
        let kp = rime_remote_core::noise::builder()
            .generate_keypair()
            .expect("keypair");
        Device {
            secret: kp.private.as_slice().try_into().expect("32"),
            public: kp.public.as_slice().try_into().expect("32"),
        }
    }

    /// Pair, over the relay.
    fn pair(&self, h: &Harness, offer: &PairingOffer, name: &str) -> serde_json::Value {
        let desktop: [u8; 32] = rime_remote_core::b64_decode(&offer.key)
            .expect("a key")
            .as_slice()
            .try_into()
            .expect("32");
        let mut hs = Handshake::pairing_initiator(&desktop, V).expect("initiator");
        let request = PairingRequest {
            key: rime_remote_core::b64_encode(&self.public),
            name: name.to_string(),
            token: offer.token.clone(),
            user_verification: true,
        };
        let m1 = hs.write(&serde_json::to_vec(&request).expect("serialise")).expect("write");
        let mut pipe = h.through_the_relay();
        pipe.write_all(&[HELLO_PAIR]).expect("hello");
        write_message(&mut pipe, &m1);
        let m2 = read_message(&mut pipe);
        let payload = hs.read(&m2).expect("read");
        serde_json::from_slice(&payload).expect("a JSON answer")
    }

    /// Open a session, over the relay.
    fn connect(&self, h: &Harness, desktop: [u8; 32]) -> Option<Session> {
        let mut pipe = h.through_the_relay();
        pipe.write_all(&[HELLO_SESSION]).ok()?;
        let mut hs = Handshake::session_initiator(&self.secret, &desktop, V).ok()?;
        let m1 = hs.write(b"").ok()?;
        write_message(&mut pipe, &m1);
        let mut len = [0u8; 4];
        pipe.read_exact(&mut len).ok()?;
        let mut buf = vec![0u8; u32::from_be_bytes(len) as usize];
        pipe.read_exact(&mut buf).ok()?;
        hs.read(&buf).ok()?;
        let channel = hs.into_transport().ok()?;
        Some(Session { pipe, channel })
    }
}

struct Session {
    pipe: WsPipe,
    channel: Channel,
}

impl Session {
    fn send(&mut self, frame: Frame) {
        let bytes = frame.encode().expect("encode");
        let sealed = self.channel.seal(&bytes).expect("seal");
        write_message(&mut self.pipe, &sealed);
    }

    /// Read one frame, answering keepalives on the way.
    ///
    /// The desktop measures its own connections, so a `Ping` arrives whenever
    /// it likes. Answering it is what makes the measurement possible at all;
    /// `a_device_that_never_answers` below is the control that shows the
    /// desktop does not simply invent one.
    fn recv(&mut self) -> Frame {
        loop {
            let message = read_message(&mut self.pipe);
            let plain = self.channel.open(&message).expect("open");
            match Frame::decode(&plain).expect("decode") {
                Frame::Ping { token } => self.send(Frame::Pong { token }),
                frame => return frame,
            }
        }
    }

    /// Read one frame WITHOUT answering anything.
    fn recv_raw(&mut self) -> Frame {
        let message = read_message(&mut self.pipe);
        let plain = self.channel.open(&message).expect("open");
        Frame::decode(&plain).expect("decode")
    }

    fn call(&mut self, request: &str) -> serde_json::Value {
        self.send(Frame::Control(request.as_bytes().to_vec()));
        loop {
            if let Frame::Control(line) = self.recv() {
                return serde_json::from_slice(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"));
            }
        }
    }
}

macro_rules! harness {
    ($tag:literal) => {
        match Harness::start($tag) {
            Some(h) => h,
            None => {
                eprintln!("SKIP: the daemons did not come up in this environment");
                return;
            }
        }
    };
}

fn desktop_key(h: &Harness) -> [u8; 32] {
    rime_remote_core::b64_decode(h.control(r#"{"cmd":"status"}"#)["key"].as_str().expect("key"))
        .expect("decode")
        .as_slice()
        .try_into()
        .expect("32")
}

// ─────────────────────────────────────────────────────────────────────────────
//  The claims
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_device_reaches_the_desktop_through_a_relay_neither_of_them_listens_on() {
    let h = harness!("reach");

    // The desktop dialled out before any device existed. That ordering is
    // P1-052's "no inbound router port forwarding" made mechanical: the relay
    // never connects to the desktop, so the only way this works is the
    // connection the desktop already made.
    if h.relay.is_external() {
        // A real relay does not report what arrived, so the same fact is
        // established from the outside: a host is already holding this
        // rendezvous, and this test has not put it there.
        assert!(
            h.relay.host_is_parked(&h.rendezvous()),
            "no desktop was parked on the rendezvous, so it never dialled out"
        );
    } else {
        let seen = h.relay.seen();
        assert_eq!(seen.arrivals.first().map(String::as_str), Some("host"));
        assert!(
            seen.targets.iter().any(|t| t.ends_with("?role=host")),
            "the desktop did not name its role: {:?}",
            seen.targets
        );
    }

    let device = Device::new();
    let Some(offer) = h.offer() else { return };
    let paired = device.pair(&h, &offer, "a phone");
    assert_eq!(paired["ok"], true, "pairing through the relay failed: {paired}");
    assert!(paired["device"].as_str().is_some_and(|d| !d.is_empty()), "{paired}");

    // A second relay connection, for the session — and the desktop had to
    // have re-armed after the pairing one was consumed for this to work at
    // all.
    assert!(h.hosts_seen(2), "the desktop did not re-arm after a device arrived");
    let mut session = device.connect(&h, desktop_key(&h)).expect("a session");
    // The agent runtime's own vocabulary, forwarded untouched: `hello` is the
    // one request a client of any version may rely on, and its answer names
    // the protocol the daemon speaks. A relay that had corrupted a byte in
    // either direction could not produce it.
    let reply = session.call(r#"{"cmd":"hello"}"#);
    assert_eq!(
        reply["reply"], "hello",
        "the agent runtime did not answer through the relay: {reply}"
    );
    assert!(
        reply["version"].as_u64().is_some(),
        "the answer did not come from the runtime: {reply}"
    );

    // And a listing, so the reply carried something of variable length rather
    // than a fixed greeting the transport could have short-circuited.
    let listed = session.call(r#"{"cmd":"list"}"#);
    assert!(listed["sessions"].is_array(), "{listed}");
}

#[test]
fn the_relay_carries_the_session_and_can_read_none_of_it() {
    if std::env::var_os("RIME_RELAY_URL").is_some() {
        // The claim here is about what the relay OPERATOR can see, and it is
        // asserted by reading everything the double copied. A real relay does
        // not hand that over, and inferring it from the outside would be
        // weaker than the thing being claimed. The double remains the right
        // place for this one.
        eprintln!(
            "SKIP-EXTERNAL: the_relay_carries_the_session_and_can_read_none_of_it: \
             needs the double's record of every payload it copied"
        );
        return;
    }
    let h = harness!("opaque");
    let device = Device::new();
    let Some(offer) = h.offer() else { return };
    assert_eq!(device.pair(&h, &offer, SENTINEL)["ok"], true);
    assert!(h.hosts_seen(2));

    let mut session = device.connect(&h, desktop_key(&h)).expect("a session");
    // A request whose text is unmistakable, and whose reply names the device
    // by the same unmistakable name.
    let reply = session.call(&format!(r#"{{"cmd":"hello","{SENTINEL}":"{SENTINEL}"}}"#));
    assert_eq!(reply["reply"], "hello", "{reply}");

    let seen = h.relay.seen();
    assert!(!seen.payloads.is_empty(), "the relay copied nothing, so it proved nothing");
    let everything: Vec<u8> = seen.payloads.concat();
    assert!(
        !window_contains(&everything, SENTINEL.as_bytes()),
        "the relay saw the session in the clear"
    );
    // And the structural half of the same claim: nothing the device sent was
    // JSON, or a frame tag, or anything but sealed bytes. `list_sessions` is
    // the word that would appear if the Noise channel were not there.
    for word in [b"hello".as_slice(), b"cmd".as_slice(), b"reply".as_slice()] {
        assert!(
            !window_contains(&everything, word),
            "the relay saw {:?} in the clear",
            String::from_utf8_lossy(word)
        );
    }

    // The relay was never told anything either. This end sends binary only.
    assert!(
        seen.client_text.is_empty(),
        "a client sent the relay a text frame: {:?}",
        seen.client_text
    );
}

#[test]
fn a_relayed_session_is_recorded_as_relayed_and_a_direct_one_is_not() {
    let h = harness!("labels");
    let device = Device::new();
    let Some(offer) = h.offer() else { return };
    assert_eq!(device.pair(&h, &offer, "labelled")["ok"], true);
    assert!(h.hosts_seen(2));

    // Through the relay first.
    {
        let mut session = device.connect(&h, desktop_key(&h)).expect("a relayed session");
        session.call(r#"{"cmd":"hello"}"#);
    }
    let devices = h.control(r#"{"cmd":"devices"}"#);
    let listed = &devices["devices"][0];
    assert_eq!(
        listed["last_path"], "relay",
        "a session that came through a relay was recorded as local: {devices}"
    );

    // Then straight to the listener, which is what a device on this network
    // does. The comparison is the assertion: a `last_path` that said "relay"
    // for everything would pass the half above on its own.
    {
        let mut socket = TcpStream::connect(("127.0.0.1", h.port)).expect("lan");
        socket.set_read_timeout(Some(Duration::from_secs(15))).ok();
        socket.write_all(&[HELLO_SESSION]).expect("hello");
        let mut hs =
            Handshake::session_initiator(&device.secret, &desktop_key(&h), V).expect("initiator");
        let m1 = hs.write(b"").expect("write");
        write_message(&mut socket, &m1);
        let m2 = read_message(&mut socket);
        hs.read(&m2).expect("read");
    }
    // The store is written during the handshake, before any frame, so the
    // connection above is enough. Give the daemon a moment to have done it.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = serde_json::Value::Null;
    while Instant::now() < deadline {
        last = h.control(r#"{"cmd":"devices"}"#)["devices"][0]["last_path"].clone();
        if last == "lan" {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(last, "lan", "a session straight to the listener was recorded as relayed");
}

#[test]
fn a_guest_that_arrives_with_no_desktop_waiting_is_refused_by_status_not_by_silence() {
    // Plaintext-only BY CONSTRUCTION: this test dials a raw TcpStream and
    // speaks the WebSocket handshake itself, so a `wss://` endpoint can only
    // reject it — which it does in 0.01 s, long before any room logic runs.
    // Skipped rather than made to fail, and skipped on the SCHEME rather than
    // on "is it external", because against `wrangler dev --local` over ws://
    // it still tests exactly what it was written to test.
    {
        let probe = Relay::start();
        let ep = Endpoint::parse(&probe.url()).expect("endpoint");
        if ep.secure {
            println!(
                "SKIP-EXTERNAL: a_guest_that_arrives_with_no_desktop_waiting_is_refused_by_status_not_by_silence: this assertion is made \
                 over a raw TCP socket, which cannot reach a TLS relay. It runs \
                 against the double and against a ws:// relay."
            );
            return;
        }
    }
    // The relay's own refusal, checked through the shipped client: a device
    // whose desktop is off should be told so, not left holding a socket.
    let relay = Relay::start();
    let endpoint = Endpoint::parse(&relay.url()).expect("endpoint");
    let socket = TcpStream::connect((relay.host.as_str(), relay.port)).expect("dial");
    let mut writing = socket.try_clone().expect("clone");
    let mut reading = socket.try_clone().expect("clone");
    let opening = Opening::new();
    writing
        .write_all(&opening.request(&endpoint, "nobody-is-here", Role::Guest))
        .expect("request");
    writing.flush().ok();
    let refused = opening.accept(&mut reading);
    let Err(e) = refused else { panic!("a guest with no host was upgraded") };
    assert!(format!("{e}").contains("409"), "{e}");
    if !relay.is_external() {
        assert_eq!(relay.seen().refusals, vec![409]);
    }
}

#[test]
fn two_desktops_cannot_hold_one_rendezvous() {
    // Plaintext-only BY CONSTRUCTION: this test dials a raw TcpStream and
    // speaks the WebSocket handshake itself, so a `wss://` endpoint can only
    // reject it — which it does in 0.01 s, long before any room logic runs.
    // Skipped rather than made to fail, and skipped on the SCHEME rather than
    // on "is it external", because against `wrangler dev --local` over ws://
    // it still tests exactly what it was written to test.
    {
        let probe = Relay::start();
        let ep = Endpoint::parse(&probe.url()).expect("endpoint");
        if ep.secure {
            println!(
                "SKIP-EXTERNAL: two_desktops_cannot_hold_one_rendezvous: this assertion is made \
                 over a raw TCP socket, which cannot reach a TLS relay. It runs \
                 against the double and against a ws:// relay."
            );
            return;
        }
    }
    // Whoever has seen the QR code knows the rendezvous id and can dial as a
    // host. They cannot impersonate the desktop — they have no static private
    // key, so Noise_IK fails for them — but a relay that let a second host
    // displace the first would hand them a denial of service against a
    // machine they have never touched. So the second one is refused.
    let relay = Relay::start();
    let endpoint = Endpoint::parse(&relay.url()).expect("endpoint");
    let mut held = Vec::new();
    for (n, want) in [(0usize, true), (1usize, false)] {
        let socket = TcpStream::connect((relay.host.as_str(), relay.port)).expect("dial");
        let mut writing = socket.try_clone().expect("clone");
        let mut reading = socket.try_clone().expect("clone");
        let opening = Opening::new();
        writing
            .write_all(&opening.request(&endpoint, "one-room", Role::Host))
            .expect("request");
        writing.flush().ok();
        assert_eq!(
            opening.accept(&mut reading).is_ok(),
            want,
            "host number {} was not treated as {}",
            n + 1,
            if want { "the holder" } else { "an intruder" }
        );
        held.push(socket);
    }
    if !relay.is_external() {
        assert_eq!(relay.seen().refusals, vec![409]);
    }
}

#[test]
fn no_relay_is_configured_unless_the_owner_configures_one() {
    // The BINARY has no default relay: without --relay the QR code carries no
    // relay at all, so nothing but an explicit flag ever routes a machine's
    // metadata through somebody's deployment. The image's unit passes Rime's
    // own relay (2026-09-23, Andre's decision); that choice lives in the unit,
    // where it is visible and editable, and never in this binary.
    let root = std::env::temp_dir().join(format!(
        "rime-relay-default-{}-{}",
        std::process::id(),
        rime_remote_core::now_ms()
    ));
    let runtime = root.join("run");
    let state = root.join("state");
    std::fs::create_dir_all(&runtime).expect("runtime");
    std::fs::create_dir_all(&state).expect("state");
    let port = {
        let probe = TcpListener::bind(("127.0.0.1", 0)).expect("probe");
        let p = probe.local_addr().expect("addr").port();
        drop(probe);
        p
    };
    let mut child = Command::new(bin("rime-remoted"))
        .args(["--port", &port.to_string(), "--allow-foreground"])
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", &state)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let socket = runtime.join("rime-remoted").join("control.sock");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && UnixStream::connect(&socket).is_err() {
        std::thread::sleep(Duration::from_millis(50));
    }
    let reply = {
        let stream = UnixStream::connect(&socket).expect("connect");
        let mut writer = stream.try_clone().expect("clone");
        let mut reader = BufReader::new(stream);
        writeln!(writer, r#"{{"cmd":"status"}}"#).expect("write");
        let mut line = String::new();
        reader.read_line(&mut line).expect("read");
        serde_json::from_str::<serde_json::Value>(line.trim()).expect("json")
    };
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(reply["relay"], serde_json::Value::Null, "{reply}");
}

/// A substring search over bytes. `Vec::windows` on an empty needle is a
/// panic, and on a needle longer than the haystack is an empty iterator, so
/// both are answered here rather than at the call sites.
fn window_contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack.len() >= needle.len()
        && haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn the_desktop_measures_the_connection_and_a_device_cannot_invent_the_answer() {
    let h = harness!("quality");
    let device = Device::new();
    let Some(offer) = h.offer() else { return };
    assert_eq!(device.pair(&h, &offer, "measured")["ok"], true);
    assert!(h.hosts_seen(2));

    let mut session = device.connect(&h, desktop_key(&h)).expect("a session");
    // `recv` answers pings, so driving the session is what lets the
    // measurement complete.
    assert_eq!(session.call(r#"{"cmd":"hello"}"#)["reply"], "hello");

    let measured = wait_for(&h, |c| c.first().and_then(|c| c["rtt_ms"].as_u64()).is_some());
    let c = &measured[0];
    assert_eq!(c["path"], "relay", "the measured connection was not the relayed one: {c}");
    assert_eq!(c["device_name"], "measured", "{c}");
    // A round trip over loopback is not zero and is not a second. Both ends
    // matter: 0 would mean the clock was read once, and a large number would
    // mean the elapsed time was measured from the wrong event.
    let rtt = c["rtt_ms"].as_u64().expect("a round trip");
    assert!(rtt < 5_000, "a loopback round trip of {rtt} ms is not a measurement");
    drop(session);

    // The control. A device that answers nothing must leave the desktop with
    // no measurement rather than with a good one: an unmeasured connection
    // and a fast one have to be different states, or the number means
    // nothing.
    assert!(h.hosts_seen(3));
    let silent = Device::new();
    let Some(offer) = h.offer() else { return };
    assert_eq!(silent.pair(&h, &offer, "silent")["ok"], true);
    assert!(h.hosts_seen(4));
    let mut quiet = silent.connect(&h, desktop_key(&h)).expect("a session");
    // Read the pings and deliberately do not answer them -- and then answer
    // with a token this desktop never sent. A device that could name its own
    // token could report any quality it liked, including a good one for a
    // connection that is unusable, which is the whole value of the number
    // gone. The desktop has to time only round trips it started.
    for _ in 0..3 {
        assert!(matches!(quiet.recv_raw(), Frame::Ping { .. }));
    }
    quiet.send(Frame::Pong { token: 0xdead_beef });
    // Long enough that a desktop which was going to record it has.
    std::thread::sleep(Duration::from_millis(500));
    let listed = h.control(r#"{"cmd":"status"}"#)["connections"].clone();
    let quiet_entry = listed
        .as_array()
        .expect("connections")
        .iter()
        .find(|c| c["device_name"] == "silent")
        .unwrap_or_else(|| panic!("the silent device was not listed: {listed}"));
    assert!(
        quiet_entry["rtt_ms"].is_null(),
        "a device that answered nothing was given a round trip: {quiet_entry}"
    );
}

#[test]
fn a_network_change_costs_the_session_and_costs_nothing_else() {
    if std::env::var_os("RIME_RELAY_URL").is_some() {
        // A network change is simulated by cutting every socket the relay
        // holds, from inside it. Nothing outside a relay can do that to it,
        // and killing the client end instead would be testing a different
        // event — the desktop noticing its own socket close, rather than the
        // relay's peer vanishing.
        eprintln!(
            "SKIP-EXTERNAL: a_network_change_costs_the_session_and_costs_nothing_else: \
             needs to cut the sockets the double is holding"
        );
        return;
    }
    // What this proves, exactly: when every relay socket dies mid-session,
    // the desktop re-arms on its own, a device that dials again gets a fresh
    // Noise_IK session, the agent runtime's sessions are all still there with
    // the same ids, the device is still paired, and the dead connection is
    // not left in the live list.
    //
    // What it does NOT prove, and must not be read as proving: that a phone
    // re-dials. The device half of "reconnect across Wi-Fi/mobile-network
    // changes" is the Android app's (P1-053) and is not in this repository.
    // Nor is "the PTY survives" a property this branch adds -- rime-agentd
    // owns the PTYs and goes on owning them whether or not anything is
    // connected, which is P1-030's decision. What is asserted here is that
    // the remote layer does not undo it.
    let h = harness!("cut");
    let device = Device::new();
    let Some(offer) = h.offer() else { return };
    assert_eq!(device.pair(&h, &offer, "roaming")["ok"], true);
    assert!(h.hosts_seen(2));

    let mut session = device.connect(&h, desktop_key(&h)).expect("a session");
    let started = session.call(
        r#"{"cmd":"run","agent":"generic","cwd":"/tmp","sandbox":"unrestricted","cols":80,"rows":24,"args":["/bin/cat"]}"#,
    );
    assert_eq!(started["reply"], "session", "{started}");
    let id = started["id"].as_u64().expect("an id");
    let hosts_before = h.relay.seen().arrivals.iter().filter(|a| *a == "host").count();

    // The network goes away. No close frame, no warning.
    h.relay.cut_everything();
    drop(session);

    // The desktop notices and re-arms, without being told to.
    assert!(
        h.hosts_seen(hosts_before + 1),
        "the desktop did not re-arm after the relay dropped everything"
    );
    // And it does not leave the dead connection in the live list, which is
    // what a status page would render as a device that is still there.
    let empty = wait_for(&h, |c| c.is_empty());
    assert!(empty.is_empty(), "a connection survived its own socket: {empty:?}");

    // A fresh session, over a fresh relay connection, finds the session the
    // old one started -- same id, still running.
    let mut again = device.connect(&h, desktop_key(&h)).expect("a second session");
    let listed = again.call(r#"{"cmd":"list"}"#);
    let ids: Vec<u64> = listed["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .filter_map(|s| s["id"].as_u64())
        .collect();
    assert!(ids.contains(&id), "the session did not survive the reconnect: {listed}");

    // The device never had to pair again.
    let devices = h.control(r#"{"cmd":"devices"}"#);
    assert_eq!(devices["devices"].as_array().map(Vec::len), Some(1), "{devices}");
}

/// Poll the daemon's live connection list until `ready`, then return it.
fn wait_for(
    h: &Harness,
    ready: impl Fn(&[serde_json::Value]) -> bool,
) -> Vec<serde_json::Value> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = Vec::new();
    while Instant::now() < deadline {
        last = h.control(r#"{"cmd":"status"}"#)["connections"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if ready(&last) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    last
}
