//! The whole chain, against real processes: a device, `rime-remoted` and
//! `rime-agentd`.
//!
//! Everything else about this protocol is a pure function over values and is
//! tested beside the code. Three claims cannot be made about a value, and
//! they are the three that decide whether any of this works:
//!
//! * a device that scans a pairing code can complete a `Noise_NK` handshake
//!   against the real service and end up in the real store;
//! * a paired device can open a `Noise_IK` session and get an answer from the
//!   real `rime-agentd` through the real proxy;
//! * **a request that arrives this way is recorded as
//!   `claude-remote-control`, by the device's id, and cannot approve a root
//!   operation** — while the same daemon, asked directly from this test
//!   process, can. That last comparison is the point: "the remote request was
//!   refused" proves nothing unless a local one is allowed by the same daemon
//!   in the same run.
//!
//! Both daemons get their own `XDG_RUNTIME_DIR` and `XDG_STATE_HOME`, so
//! nothing here touches a running `rime-agentd` or the user's own devices.
//! Both are killed by pid; nothing goes looking for a process by name.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

mod common;

use rime_remote_core::noise::{Channel, Handshake};
use rime_remote_core::pairing::{PairingOffer, PairingRequest};
use rime_remote_core::wire::Frame;

const V: u32 = rime_remote_core::REMOTE_PROTOCOL_VERSION;

// The two hello bytes, named. `rime-remoted` declares them as `pub const` in a
// binary crate, which an integration test cannot import, so they are written
// out here — and named rather than inlined so a reader can see that the two
// spellings are meant to be the same value.
fn serve_hello_pair() -> u8 {
    b'P'
}
fn serve_hello_session() -> u8 {
    b'S'
}

/// Where cargo put the workspace binaries.
///
/// This test needs two of them and cargo only exports the one belonging to
/// its own package, so the sibling is found next to it. A **panic** rather
/// than a skip when it is missing: an absent binary is a build that did not
/// happen, and a suite that reports success for a test it never ran is the
/// failure mode this program has already shipped twice.
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
    refuse_if_stale(&path, name);
    path
}

/// Refuse a sibling binary that is older than the sources it was built from.
///
/// Found while mutation-testing the upload path, and it is the same class of
/// defect this file already panics about one line up. `cargo test -p
/// rime-remoted` does **not** rebuild `rime-agentd`: cargo builds the package
/// under test and its dependencies, and this suite's second daemon is neither.
/// Three mutations of `rime-agentd/src/inject.rs` — the size cap deleted, the
/// echoed length zeroed, the file written empty — all came back GREEN on a
/// targeted run, because the harness had started a binary compiled before any
/// of them existed. The same three are red the moment `cargo build -p
/// rime-agentd` runs first.
///
/// So the existence check was necessary and not sufficient: a stale binary is
/// a test that ran, reported success, and asserted nothing about the code in
/// the tree. `rime-agent-core` is checked too, because `rime-agentd` is built
/// from it and a change there leaves the same stale binary behind.
///
/// Each binary is compared only against the crates IT is built from, which is
/// the correction this check needed after its first version failed everything:
/// cargo does not relink a binary whose own inputs are unchanged, so
/// `rime-remoted` is legitimately older than a change to `rime-agentd` and
/// saying otherwise made a true guard cry wolf on every run.
///
/// A refusal rather than a rebuild: a test that shells out to cargo while
/// cargo is running it is a deadlock waiting for a lock it already holds.
fn refuse_if_stale(path: &std::path::Path, name: &str) {
    let built = match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(t) => t,
        // A filesystem with no mtime is not a reason to fail a suite; the
        // existence check above is still in force.
        Err(_) => return,
    };
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the workspace root");
    let sources: &[&str] = match name {
        "rime-agentd" => &["rime-agentd", "rime-agent-core"],
        "rime-remoted" => &["rime-remoted", "rime-remote-core"],
        _ => return,
    };
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for c in sources {
        let mut stack = vec![crates.join(c).join("src")];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    if let Ok(t) = e.metadata().and_then(|m| m.modified()) {
                        if newest.as_ref().map_or(true, |(best, _)| t > *best) {
                            newest = Some((t, p));
                        }
                    }
                }
            }
        }
    }
    if let Some((t, source)) = newest {
        assert!(
            t <= built,
            "{name} was built before {} was last changed, so this suite would run a daemon \
             that predates the code under test and report success for it. Run \
             `cargo build --workspace` (or `cargo test --workspace`) first.",
            source.display()
        );
    }
}

struct Harness {
    agentd: Child,
    remoted: Child,
    root: PathBuf,
    agentd_socket: PathBuf,
    remoted_socket: PathBuf,
    port: u16,
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
        let root = std::env::temp_dir().join(format!(
            "rime-remote-e2e-{}-{tag}-{}",
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
            // The last path no XDG variable reaches. Without it this fixture's
            // sessions share `/tmp/rime-agent-<uid>` with the user's real
            // daemon — and both number their sessions from 1, while a session
            // reap runs `remove_dir_all` on its own id's directory. See
            // `paths::SCRATCH_ROOT_ENV`. It is also what lets the upload test
            // below read back the file the daemon wrote.
            .env("RIME_AGENT_SCRATCH_ROOT", root.join("scratch"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let agentd_socket = runtime.join("rime-agentd").join("control.sock");

        // `--port 0`: the kernel picks the port when the daemon binds, and the
        // fixture reads back the one it got (see `common`). Its stderr is kept:
        // it is where that port is, and where the reason is when it does not
        // come up.
        let log = root.join("remoted.log");
        let remoted = Command::new(bin("rime-remoted"))
            .args([
                "--port",
                "0",
                "--allow-foreground",
                // The real deadline is thirty seconds, which is right for a
                // phone waking its radio and wrong for a suite that runs on
                // every commit. Clamped by the daemon to a second at the
                // bottom, so this is the shortest a test can ask for.
                "--handshake-timeout-ms",
                "1000",
            ])
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("XDG_STATE_HOME", &state)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).ok()?)
            .spawn()
            .ok()?;
        let remoted_socket = runtime.join("rime-remoted").join("control.sock");

        let mut h = Harness {
            agentd,
            remoted,
            root,
            agentd_socket,
            remoted_socket,
            port: 0,
        };
        match h.wait_ready(&log) {
            Ok(port) => {
                h.port = port;
                Some(h)
            }
            Err(why) => {
                eprintln!("{why}");
                None
            }
        }
    }

    /// Both daemons answer; the port `rime-remoted` got.
    fn wait_ready(&mut self, log: &std::path::Path) -> Result<u16, String> {
        let within = Duration::from_secs(15);
        let deadline = Instant::now() + within;
        let port = common::remoted_port(&mut self.remoted, &self.remoted_socket, log, within)?;
        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.agentd.try_wait() {
                return Err(format!("rime-agentd exited ({status})"));
            }
            if UnixStream::connect(&self.agentd_socket).is_ok() {
                return Ok(port);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err("rime-agentd did not come up in 15 s".to_string())
    }

    /// One request on `rime-remoted`'s local control socket.
    fn control(&self, line: &str) -> serde_json::Value {
        one_line(&self.remoted_socket, line)
    }

    /// One request straight to `rime-agentd`, as this test process.
    ///
    /// The control arm of every assertion below: this connection is whatever
    /// `cargo test` classifies as, and the point is to compare it with what
    /// the same daemon says about the proxied one.
    fn direct(&self, line: &str) -> serde_json::Value {
        one_line(&self.agentd_socket, line)
    }

    fn tcp(&self) -> TcpStream {
        let s = TcpStream::connect(("127.0.0.1", self.port)).expect("connect");
        s.set_read_timeout(Some(Duration::from_secs(15))).ok();
        s
    }
}

fn one_line(socket: &PathBuf, line: &str) -> serde_json::Value {
    let stream = UnixStream::connect(socket).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(15))).ok();
    let mut writer = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);
    writeln!(writer, "{line}").expect("write");
    writer.flush().ok();
    let mut reply = String::new();
    reader.read_line(&mut reply).expect("read");
    serde_json::from_str(reply.trim()).unwrap_or_else(|e| panic!("{e}: {reply}"))
}

macro_rules! harness {
    ($tag:literal) => {
        match Harness::start($tag) {
            Some(h) => h,
            None => {
                // Harness::start printed the reason above this line.
                eprintln!("SKIP: the daemons did not come up in this environment");
                return;
            }
        }
    };
}

/// A device: a keypair, and the two things it can do with one.
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

    fn key(&self) -> String {
        rime_remote_core::b64_encode(&self.public)
    }

    /// Scan a code and pair. Returns whatever the desktop answered.
    fn pair(&self, h: &Harness, offer: &PairingOffer, name: &str) -> serde_json::Value {
        let desktop: [u8; 32] = rime_remote_core::b64_decode(&offer.key)
            .expect("a key")
            .as_slice()
            .try_into()
            .expect("32");
        let mut hs = Handshake::pairing_initiator(&desktop, V).expect("initiator");
        let request = PairingRequest {
            key: self.key(),
            name: name.to_string(),
            token: offer.token.clone(),
            user_verification: true,
        };
        let m1 = hs
            .write(&serde_json::to_vec(&request).expect("serialise"))
            .expect("write");
        let mut socket = h.tcp();
        socket.write_all(&[serve_hello_pair()]).expect("hello");
        write_message(&mut socket, &m1);
        let m2 = read_message(&mut socket);
        let payload = hs.read(&m2).expect("read");
        serde_json::from_slice(&payload).expect("a JSON answer")
    }

    /// Open a session. `None` when the desktop refuses the key.
    fn connect(&self, h: &Harness) -> Option<Session> {
        let mut socket = h.tcp();
        socket.write_all(&[serve_hello_session()]).ok()?;
        let desktop = desktop_key(h);
        let mut hs = Handshake::session_initiator(&self.secret, &desktop, V).ok()?;
        let m1 = hs.write(b"").ok()?;
        write_message(&mut socket, &m1);
        // A refused device gets the connection closed rather than an answer,
        // which is what `read_exact` on the length prefix reports as EOF.
        let mut len = [0u8; 4];
        socket.read_exact(&mut len).ok()?;
        let n = u32::from_be_bytes(len) as usize;
        let mut buf = vec![0u8; n];
        socket.read_exact(&mut buf).ok()?;
        hs.read(&buf).ok()?;
        let channel = hs.into_transport().ok()?;
        Some(Session {
            socket,
            channel,
            pending: Vec::new(),
        })
    }
}

struct Session {
    socket: TcpStream,
    channel: Channel,
    /// Frames that arrived while waiting for a reply.
    ///
    /// The connection is multiplexed, so a terminal's output legitimately
    /// arrives between a control request and its answer — that is the whole
    /// reason this layer exists rather than handing the connection over the
    /// way `rime agent attach` does. A `call` that assumed the next frame was
    /// its reply was wrong about the protocol, and it failed intermittently
    /// in exactly the way a wrong assumption about interleaving does.
    ///
    /// Kept rather than dropped, so a test can assert about what interleaved.
    pending: Vec<Frame>,
}

impl Session {
    /// Seal one frame and write it.
    fn send(&mut self, frame: Frame) {
        let bytes = frame.encode().expect("encode");
        let sealed = self.channel.seal(&bytes).expect("seal");
        write_message(&mut self.socket, &sealed);
    }

    /// Read one frame, answering keepalives on the way.
    ///
    /// The desktop measures its own connections, so a `Ping` arrives whenever
    /// it likes — including as the first frame of a session, before the
    /// device has asked for anything. It is not a session event and no
    /// assertion here is about one, so it is answered and stepped over, which
    /// is exactly what a device does. A test that treated it as the next
    /// frame would be asserting that the desktop never measures anything.
    fn recv(&mut self) -> Frame {
        loop {
            let message = read_message(&mut self.socket);
            let plain = self.channel.open(&message).expect("open");
            match Frame::decode(&plain).expect("decode") {
                Frame::Ping { token } => self.send(Frame::Pong { token }),
                frame => return frame,
            }
        }
    }

    /// Send one control frame and read its reply, keeping anything that
    /// arrived in between.
    fn call(&mut self, request: &str) -> serde_json::Value {
        self.send(Frame::Control(request.as_bytes().to_vec()));
        loop {
            match self.recv() {
                Frame::Control(line) => {
                    return serde_json::from_slice(&line)
                        .unwrap_or_else(|e| panic!("{e}: {line:?}"))
                }
                other => self.pending.push(other),
            }
        }
    }

    /// Whether a channel was closed by the desktop without being asked.
    fn closed_unasked(&self, channel: u32) -> Option<&Frame> {
        self.pending
            .iter()
            .find(|f| matches!(f, Frame::Close { channel: c, .. } if *c == channel))
    }
}

fn desktop_key(h: &Harness) -> [u8; 32] {
    let status = h.control(r#"{"cmd":"status"}"#);
    rime_remote_core::b64_decode(status["key"].as_str().expect("a key"))
        .expect("base64")
        .as_slice()
        .try_into()
        .expect("32")
}

fn write_message(w: &mut impl Write, m: &[u8]) {
    w.write_all(&(m.len() as u32).to_be_bytes()).expect("len");
    w.write_all(m).expect("body");
    w.flush().expect("flush");
}

fn read_message(r: &mut impl Read) -> Vec<u8> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).expect("length prefix");
    let mut buf = vec![0u8; u32::from_be_bytes(len) as usize];
    r.read_exact(&mut buf).expect("body");
    buf
}

fn open_offer(h: &Harness) -> Option<PairingOffer> {
    let reply = h.control(r#"{"cmd":"pair"}"#);
    if reply["reply"] == "error" {
        // This process is not local, so pairing is refused — which is itself
        // the rule under test in `pairing_is_refused_for_a_caller_that_is_not_local`.
        //
        // The refusal has to be about the PLACEMENT. `local_caller` also
        // refuses when the kernel would not report peer credentials and when
        // the connection belongs to another account, and neither of those is
        // a could-not-run: they mean this harness is wrong. Established from
        // /proc rather than read off the message, so the two halves are
        // independent.
        let placement = rime_agent_core::origin::observe_pid(std::process::id() as i32);
        let why = reply["message"].as_str().unwrap_or_default().to_string();
        let names_the_placement = match &placement {
            Ok(o) => why.contains(o.as_str()),
            Err(_) => why.contains("/proc/") || why.contains("cgroup"),
        };
        assert!(
            names_the_placement,
            "pairing was refused for a reason that is not this process's placement \
             ({placement:?}): {reply}"
        );
        // Without this the skip is a swallow. CI runs this crate's tests
        // through `tests/in-login-session.sh`, and that helper "says out loud
        // why it could not and runs the tests exactly as before" when PAM
        // fails — so a broken session would turn every pairing assertion in
        // this file into a silent pass on the runner and tell nobody. See
        // `Harness::offer` in relay.rs for the same knob and the four-way
        // table behind it.
        assert!(
            std::env::var_os("RIME_REQUIRE_LOCAL_ORIGIN").is_none(),
            "RIME_REQUIRE_LOCAL_ORIGIN is set, so this run was supposed to hold a real \
             logind session and does not: {why}"
        );
        eprintln!("SKIP: this process may not pair ({why})");
        return None;
    }
    Some(PairingOffer::decode(reply["qr"].as_str().expect("a qr")).expect("decode"))
}

#[test]
fn a_device_pairs_over_the_real_socket_and_lands_in_the_real_store() {
    let h = harness!("pair");
    let Some(offer) = open_offer(&h) else { return };
    assert_eq!(offer.v, V);
    assert!(offer.expires_ms > rime_remote_core::now_ms());

    let device = Device::new();
    let answer = device.pair(&h, &offer, "pixel-8-office");
    assert_eq!(answer["ok"], true, "{answer}");

    let listed = h.control(r#"{"cmd":"devices"}"#);
    let devices = listed["devices"].as_array().expect("an array");
    assert_eq!(devices.len(), 1, "{listed}");
    assert_eq!(devices[0]["name"], "pixel-8-office");
    assert_eq!(devices[0]["public_key"], device.key());
    assert_eq!(devices[0]["requires_user_verification"], true);
    // The listing is what a person reads, and it must not carry key material
    // beyond the public half it is meant to.
    let text = serde_json::to_string(&listed).expect("serialise");
    assert!(!text.contains(&rime_remote_core::b64_encode(&device.secret)));
}

#[test]
fn a_second_device_cannot_reuse_the_first_ones_code() {
    // The property that stops a photographed QR pairing a phone in the next
    // room, measured against the real service rather than against the offer
    // object.
    let h = harness!("reuse");
    let Some(offer) = open_offer(&h) else { return };
    assert_eq!(Device::new().pair(&h, &offer, "first")["ok"], true);
    let second = Device::new().pair(&h, &offer, "second");
    assert_eq!(second["ok"], false, "{second}");
    assert_eq!(h.control(r#"{"cmd":"devices"}"#)["devices"]
        .as_array()
        .expect("an array")
        .len(), 1);
}

#[test]
fn an_unpaired_device_cannot_open_a_session() {
    let h = harness!("unpaired");
    // The desktop's key is public, so an attacker who learns it still gets
    // nowhere without being in the store.
    assert!(
        Device::new().connect(&h).is_none(),
        "an unpaired key opened a session"
    );
}

#[test]
fn a_paired_device_reaches_the_agent_runtime_through_the_proxy() {
    let h = harness!("session");
    let Some(offer) = open_offer(&h) else { return };
    let device = Device::new();
    assert_eq!(device.pair(&h, &offer, "pixel-8")["ok"], true);

    let mut session = device.connect(&h).expect("a session");
    // The daemon's own protocol, unaltered, all the way to the daemon.
    let hello = session.call(r#"{"cmd":"hello"}"#);
    assert_eq!(hello["reply"], "hello", "{hello}");
    assert_eq!(
        hello["version"],
        rime_agent_core::protocol::PROTOCOL_VERSION,
        "{hello}"
    );
    let list = session.call(r#"{"cmd":"list"}"#);
    assert_eq!(list["reply"], "sessions", "{list}");

    // A ping, because it is the only measurement of connection quality either
    // end has and a token that came back wrong would make it meaningless.
    session.send(Frame::Ping { token: 0xdead_beef });
    assert_eq!(session.recv(), Frame::Pong { token: 0xdead_beef });
}

#[test]
fn a_request_from_a_phone_is_remote_and_cannot_approve_root() {
    // The claim the whole branch exists to make, end to end.
    //
    // Written as a comparison against the same daemon in the same run: "the
    // remote request was refused" proves nothing unless a local one is
    // allowed. If this process is not local — a container, a CI runner — the
    // comparison says so instead of asserting a fixed answer.
    let h = harness!("origin");
    let Some(offer) = open_offer(&h) else { return };
    let device = Device::new();
    let paired = device.pair(&h, &offer, "pixel-8-office");
    assert_eq!(paired["ok"], true, "{paired}");
    let device_id = paired["device"].as_str().expect("an id").to_string();

    let mut session = device.connect(&h).expect("a session");
    let filed = session.call(
        r#"{"cmd":"privilege_request","verb":"install","args":["clang"],"reason":"filed from a paired device"}"#,
    );
    assert_eq!(filed["reply"], "request", "{filed}");
    // Origin: established by the daemon from the connection the proxy
    // declared on, not from anything the device sent.
    assert_eq!(filed["request_origin"], "claude-remote-control", "{filed}");
    assert_eq!(filed["origin_source"], "declared", "{filed}");
    // And which device, which is what a person deciding actually wants.
    assert_eq!(filed["actor"], device_id.as_str(), "{filed}");
    let id = filed["id"].as_u64().expect("an id");

    // The phone tries to approve it.
    let refused = session.call(&format!(
        r#"{{"cmd":"decide","id":{id},"decision":"allow"}}"#
    ));
    assert_eq!(refused["reply"], "error", "a phone approved root: {refused}");
    assert_eq!(refused["kind"], "permission_denied", "{refused}");
    assert!(
        refused["message"]
            .as_str()
            .unwrap_or_default()
            .contains("claude-remote-control"),
        "{refused}"
    );

    // The control arm. Same daemon, same request, this process's own
    // connection.
    let direct = h.direct(&format!(
        r#"{{"cmd":"decide","id":{id},"decision":"deny"}}"#
    ));
    let observed = h.direct(
        r#"{"cmd":"privilege_request","verb":"pin","args":[],"reason":"probing this process's own origin"}"#,
    );
    let local = observed["request_origin"]
        .as_str()
        .and_then(rime_agent_core::policy::RequestOrigin::parse)
        .map(|o| o.is_local())
        .unwrap_or(false);
    if local {
        assert_eq!(
            direct["reply"], "request",
            "a local connection could not decide, so the refusal above is not \
             attributable to the origin: {direct}"
        );
        assert_eq!(direct["decision"], "denied", "{direct}");
    } else {
        eprintln!(
            "NOTE: this process is not local, so the refusal above is not attributable to the \
             declaration"
        );
        assert_eq!(direct["reply"], "error", "{direct}");
    }
}

#[test]
fn revoking_a_device_ends_the_connection_it_is_already_holding() {
    // What the owner of a lost phone is asking for. "Refused next time" is a
    // different and much weaker promise.
    let h = harness!("revoke");
    let Some(offer) = open_offer(&h) else { return };
    let device = Device::new();
    let paired = device.pair(&h, &offer, "lost-phone");
    assert_eq!(paired["ok"], true, "{paired}");
    let mut session = device.connect(&h).expect("a session");
    assert_eq!(session.call(r#"{"cmd":"list"}"#)["reply"], "sessions");

    let revoked = h.control(r#"{"cmd":"revoke","device":"lost-phone"}"#);
    assert_eq!(revoked["reply"], "ok", "{revoked}");

    // The live connection is gone: the write may succeed into a closed
    // socket's buffer, so the read is what proves it.
    let frame = Frame::Control(b"{\"cmd\":\"list\"}".to_vec())
        .encode()
        .expect("encode");
    let sealed = session.channel.seal(&frame).expect("seal");
    let _ = write_all(&mut session.socket, &sealed);
    let mut len = [0u8; 4];
    assert!(
        session.socket.read_exact(&mut len).is_err(),
        "a revoked device's live connection still answered"
    );

    // And it cannot come back.
    assert!(
        device.connect(&h).is_none(),
        "a revoked device reconnected"
    );

    // The record is still there, marked, which is what P1-051's listing needs.
    let listed = h.control(r#"{"cmd":"devices"}"#);
    let devices = listed["devices"].as_array().expect("an array");
    assert_eq!(devices.len(), 1, "{listed}");
    assert!(devices[0]["revoked_ms"].as_u64().is_some(), "{listed}");
}

fn write_all(w: &mut TcpStream, m: &[u8]) -> std::io::Result<()> {
    w.write_all(&(m.len() as u32).to_be_bytes())?;
    w.write_all(m)?;
    w.flush()
}

#[test]
fn pairing_is_refused_for_a_caller_that_is_not_local() {
    // The other half of "pairing cannot be completed silently by an agent".
    // Asserted as the rule rather than as a fixed answer, because it depends
    // on what this process classifies as — and the daemon is asked, in the
    // same run, what that is.
    let h = harness!("who");
    let reply = h.control(r#"{"cmd":"pair"}"#);
    let observed = h.direct(
        r#"{"cmd":"privilege_request","verb":"rollback","args":[],"reason":"probing this process's own origin"}"#,
    );
    let origin = observed["request_origin"]
        .as_str()
        .and_then(rime_agent_core::policy::RequestOrigin::parse);
    match origin {
        Some(o) if o.is_local() => assert_eq!(reply["reply"], "offer", "{reply}"),
        Some(o) => {
            assert_eq!(reply["reply"], "error", "a {o} caller was offered a code: {reply}");
            assert!(
                reply["message"].as_str().unwrap_or_default().contains(o.as_str()),
                "{reply}"
            );
        }
        None => assert_eq!(reply["reply"], "error", "{reply}"),
    }
}

#[test]
fn a_peer_that_connects_and_says_nothing_is_dropped() {
    // The only network listener in the stack, so an unauthenticated peer that
    // holds a thread by staying silent is the cheapest denial of service
    // there is. The first version of this daemon had exactly that bug: `main`
    // set a deadline and the spawned thread cleared it as its first line,
    // which is the opposite of what its own comment said.
    //
    // The harness runs with the deadline clamped to one second, so this costs
    // a second rather than thirty.
    let h = harness!("silent");
    let mut socket = h.tcp();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    // Not even the hello byte.
    let started = Instant::now();
    let mut buf = [0u8; 1];
    let out = socket.read(&mut buf);
    let waited = started.elapsed();
    match out {
        Ok(0) => {}
        Ok(n) => panic!("the daemon sent {n} bytes to a peer that had said nothing"),
        Err(e) => panic!("the connection was not closed: {e}"),
    }
    assert!(
        waited < Duration::from_secs(8),
        "the daemon waited {waited:?} on a silent peer"
    );
}

#[test]
fn a_terminal_streams_both_ways_over_one_channel() {
    // P1-050's "PTY stream, input", and the half of P1-055 the brief said was
    // closer than it looks. The daemon owns the PTY; this proves a device can
    // open a channel onto one, type into it and see the output, on the same
    // connection that is carrying control frames.
    let h = harness!("pty");
    let Some(offer) = open_offer(&h) else { return };
    let device = Device::new();
    let paired = device.pair(&h, &offer, "pixel-8");
    assert_eq!(paired["ok"], true, "{paired}");
    let device_id = paired["device"].as_str().expect("an id").to_string();
    let mut session = device.connect(&h).expect("a session");

    // `cat` is the honest probe: it echoes exactly what it is given, so an
    // assertion about the output is an assertion about the path rather than
    // about a program's opinion. A shell would print a prompt and a banner
    // and the test would be about those.
    let started = session.call(
        r#"{"cmd":"run","agent":"generic","cwd":"/tmp","sandbox":"unrestricted","cols":80,"rows":24,"args":["/bin/cat"]}"#,
    );
    assert_eq!(started["reply"], "session", "{started}");
    let id = started["id"].as_u64().expect("an id");
    // Started through the proxy, so the session itself is remote-origin.
    assert_eq!(started["request_origin"], "claude-remote-control", "{started}");
    // The device ID, not its name. Deliberate: a name is chosen on the phone
    // and two devices may share one, and an audit record that says "phone"
    // when the owner has two is worse than one that says nothing. The id is
    // derived from the key, so it names exactly one device — and
    // `rime remote devices` is where the two are put side by side.
    assert_eq!(started["actor"], device_id.as_str(), "{started}");

    // Open a channel onto it. The payload is the daemon's own attach request:
    // one vocabulary, expressed once.
    let open = Frame::Open {
        channel: 1,
        request: format!(
            r#"{{"cmd":"attach","id":{id},"cols":80,"rows":24,"replay":0}}"#
        )
        .into_bytes(),
    };
    session.send(open);
    let reply = session.recv();
    match reply {
        Frame::Control(line) => {
            let v: serde_json::Value = serde_json::from_slice(&line).expect("json");
            assert_eq!(v["reply"], "attached", "{v}");
        }
        other => panic!("expected the attach reply, got {other:?}"),
    }

    // Type into it, and read it back.
    session.send(Frame::Data {
        channel: 1,
        bytes: b"hello from a phone\n".to_vec(),
    });
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match session.recv() {
            Frame::Data { channel, bytes } => {
                assert_eq!(channel, 1, "terminal bytes on the wrong channel");
                seen.extend_from_slice(&bytes);
                if String::from_utf8_lossy(&seen).contains("hello from a phone") {
                    break;
                }
            }
            Frame::Close { channel, reason } => {
                panic!("channel {channel} closed early: {reason}")
            }
            other => panic!("unexpected {other:?} while reading a terminal"),
        }
    }
    assert!(
        String::from_utf8_lossy(&seen).contains("hello from a phone"),
        "the terminal never echoed: {:?}",
        String::from_utf8_lossy(&seen)
    );

    // Control still works on the same connection while the terminal is open,
    // which is the whole reason this layer multiplexes rather than handing the
    // connection over the way `rime agent attach` does. Terminal output that
    // arrives in between is kept, not dropped, so the assertion below is
    // about a channel that is genuinely still open.
    let list = session.call(r#"{"cmd":"list"}"#);
    assert!(
        session.closed_unasked(1).is_none(),
        "the desktop closed the terminal channel on its own: {:?}",
        session.closed_unasked(1)
    );
    assert_eq!(list["reply"], "sessions", "{list}");
    let sessions = list["sessions"].as_array().expect("an array");
    assert!(
        sessions.iter().any(|s| s["id"] == id && s["attached"] == 1),
        "the daemon does not report the device as attached: {list}"
    );

    // Closing the channel detaches; the session goes on running in the
    // daemon, which is what "a remote client is a viewport" means.
    session.send(Frame::Close {
        channel: 1,
        reason: String::new(),
    });
    let after = session.call(&format!(r#"{{"cmd":"info","id":{id}}}"#));
    assert_eq!(after["reply"], "session", "{after}");
    assert!(after["exit_code"].is_null(), "closing a channel killed the session: {after}");
}

/// The bytes a phone hands over: far past one frame, and not compressible
/// into a pattern a truncation would still satisfy.
///
/// 200_000 bytes against `MAX_PAYLOAD`'s 65514 is four frames, which is the
/// whole point — the criterion was refused on the arithmetic of ONE frame, and
/// a test that uploaded 40 KB would pass without touching the thing that was
/// said to be impossible. Every byte is a function of its index, so a dropped
/// frame, a duplicated one and a reordered one are all visible in the
/// comparison rather than only a short read.
fn upload_body() -> Vec<u8> {
    let mut body = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut x: u32 = 0x5eed;
    while body.len() < 200_000 {
        x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        body.push((x >> 16) as u8);
    }
    body
}

/// Read frames until `want` says it has what it needs, parking everything else
/// in `pending` the way `call` does.
fn drain_until(session: &mut Session, what: &str, mut want: impl FnMut(&Frame) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let frame = session.recv();
        let done = want(&frame);
        session.pending.push(frame);
        if done {
            return;
        }
    }
    panic!("never saw {what}; frames seen: {:?}", session.pending.len());
}

#[test]
fn a_file_from_the_phone_crosses_in_frames_and_its_path_is_typed_into_the_session() {
    // P1-059's second criterion, end to end: a photo on a phone reaching a
    // permitted path on the machine with a safe path typed into the agent.
    //
    // It was recorded as impossible with arithmetic about `Frame::Control`'s
    // 65514-byte payload. This uploads 200 KB — which that arithmetic says
    // cannot be done — through `Open`/`Data`/`Close`, the same three frames a
    // terminal has always used, and then reads the file back off the disk and
    // compares every byte.
    let h = harness!("receive");
    let Some(offer) = open_offer(&h) else { return };
    let device = Device::new();
    assert_eq!(device.pair(&h, &offer, "pixel-8")["ok"], true);
    let mut session = device.connect(&h).expect("a session");

    let started = session.call(
        r#"{"cmd":"run","agent":"generic","cwd":"/tmp","sandbox":"unrestricted","cols":80,"rows":24,"args":["/bin/cat"]}"#,
    );
    assert_eq!(started["reply"], "session", "{started}");
    let id = started["id"].as_u64().expect("an id");

    // A terminal on channel 1, so the typed path can be seen arriving rather
    // than inferred from the reply that claims it was sent. `cat` echoes, so
    // what comes back on this channel is what the agent was given.
    session.send(Frame::Open {
        channel: 1,
        request: format!(r#"{{"cmd":"attach","id":{id},"cols":80,"rows":24,"replay":0}}"#)
            .into_bytes(),
    });
    match session.recv() {
        Frame::Control(line) => {
            let v: serde_json::Value = serde_json::from_slice(&line).expect("json");
            assert_eq!(v["reply"], "attached", "{v}");
        }
        other => panic!("expected the attach reply, got {other:?}"),
    }

    // The upload, on a channel of its own and on the same connection.
    let body = upload_body();
    session.send(Frame::Open {
        channel: 2,
        request: format!(
            r#"{{"cmd":"receive","id":{id},"name":"shot.png","len":{}}}"#,
            body.len()
        )
        .into_bytes(),
    });
    // Everything refusable was decided before this line, and the length is
    // echoed: a device that reads `receiving` knows the cap was not exceeded
    // and knows the count the daemon agreed to read.
    match session.recv() {
        Frame::Control(line) => {
            let v: serde_json::Value = serde_json::from_slice(&line).expect("json");
            assert_eq!(v["reply"], "receiving", "{v}");
            assert_eq!(v["id"], id, "{v}");
            assert_eq!(v["len"], body.len() as u64, "{v}");
        }
        other => panic!("expected the takeover reply, got {other:?}"),
    }

    let frames = Frame::data_frames(2, &body);
    assert!(
        frames.len() > 1,
        "this test is about a body that does not fit in one frame; it sent {} \
         frame(s) for {} bytes",
        frames.len(),
        body.len()
    );
    for f in frames {
        session.send(f);
    }

    // The daemon answers a second time when the last byte is in, and that
    // answer comes back as `Data` on the upload's own channel — the pump in
    // `rime-remoted` does not know whether it is carrying a terminal or a
    // reply, which is why one channel type serves both.
    let mut tail: Vec<u8> = Vec::new();
    drain_until(&mut session, "the injected reply", |f| match f {
        Frame::Data { channel: 2, bytes } => {
            tail.extend_from_slice(bytes);
            serde_json::from_slice::<serde_json::Value>(&tail).is_ok()
        }
        Frame::Close { channel: 2, reason } => {
            panic!("the upload channel closed before answering: {reason}")
        }
        _ => false,
    });
    let done: serde_json::Value = serde_json::from_slice(&tail).expect("the daemon's answer");
    assert_eq!(done["reply"], "injected", "{done}");
    assert_eq!(done["id"], id, "{done}");
    let path = done["path"].as_str().expect("a path").to_string();

    // Where it landed: the session's inbox, under a name the daemon chose.
    assert!(
        path.contains("/inbox/") && path.ends_with("-shot.png"),
        "the copy did not land in the session's inbox: {path}"
    );

    // THE assertion. Not "an upload was accepted" — the bytes, all of them, in
    // order, on the disk the agent reads from.
    let landed = std::fs::read(&path).unwrap_or_else(|e| panic!("reading {path}: {e}"));
    assert_eq!(
        landed.len(),
        body.len(),
        "the file is {} bytes and {} were sent",
        landed.len(),
        body.len()
    );
    assert!(landed == body, "the file that landed is not the file that was sent");

    // And the path reached the agent's terminal, which is the half of the
    // criterion that is about the agent rather than about the filesystem.
    let mut typed: Vec<u8> = Vec::new();
    for f in &session.pending {
        if let Frame::Data { channel: 1, bytes } = f {
            typed.extend_from_slice(bytes);
        }
    }
    if !String::from_utf8_lossy(&typed).contains(&path) {
        drain_until(&mut session, "the typed path", |f| match f {
            Frame::Data { channel: 1, bytes } => {
                typed.extend_from_slice(bytes);
                String::from_utf8_lossy(&typed).contains(&path)
            }
            _ => false,
        });
    }
    assert!(
        String::from_utf8_lossy(&typed).contains(&path),
        "the path never reached the agent's terminal: {:?}",
        String::from_utf8_lossy(&typed)
    );

    // A second upload, with a name that is a traversal rather than a name. The
    // caller chooses no part of the path: `safe_name`'s alphabet has no `/`,
    // so this becomes one flat file in the same inbox.
    session.send(Frame::Open {
        channel: 3,
        request: format!(
            r#"{{"cmd":"receive","id":{id},"name":"../../.ssh/authorized_keys","len":5}}"#
        )
        .into_bytes(),
    });
    drain_until(&mut session, "the second takeover reply", |f| {
        matches!(f, Frame::Control(_))
    });
    for f in Frame::data_frames(3, b"key\n\n") {
        session.send(f);
    }
    let mut tail3: Vec<u8> = Vec::new();
    drain_until(&mut session, "the second injected reply", |f| match f {
        Frame::Data { channel: 3, bytes } => {
            tail3.extend_from_slice(bytes);
            serde_json::from_slice::<serde_json::Value>(&tail3).is_ok()
        }
        _ => false,
    });
    let done3: serde_json::Value = serde_json::from_slice(&tail3).expect("the daemon's answer");
    assert_eq!(done3["reply"], "injected", "{done3}");
    let hostile = done3["path"].as_str().expect("a path");
    assert!(
        hostile.ends_with(".._.._.ssh_authorized_keys"),
        "a name that was a path was not reduced to a name: {hostile}"
    );
    assert!(
        hostile.contains("/inbox/"),
        "a traversal escaped the inbox: {hostile}"
    );
}

#[test]
fn an_upload_is_refused_before_a_byte_of_it_is_sent() {
    // The three refusals a phone can provoke, and the one thing they have in
    // common: every one of them is answered BEFORE the device starts sending.
    // A cap enforced after 32 MiB has crossed a mobile connection is a bill
    // somebody pays for a decision that was makeable at the start.
    let h = harness!("receive-refused");
    let Some(offer) = open_offer(&h) else { return };
    let device = Device::new();
    assert_eq!(device.pair(&h, &offer, "pixel-8")["ok"], true);
    let mut session = device.connect(&h).expect("a session");

    // 1. A takeover verb on channel zero. `attach` there wedges with the
    //    device's terminal bytes going nowhere; `receive` wedges harder,
    //    because the daemon would then block reading an upload that never
    //    comes while the proxy blocks reading a reply that never comes. The
    //    proxy refuses it itself rather than forwarding it.
    let wedge = session.call(r#"{"cmd":"receive","id":1,"name":"a.png","len":10}"#);
    assert_eq!(wedge["reply"], "error", "{wedge}");
    assert_eq!(wedge["kind"], "bad_request", "{wedge}");
    assert!(
        wedge["message"].as_str().unwrap_or("").contains("channel"),
        "the refusal does not say what to do instead: {wedge}"
    );

    // 2. Past the size cap. The number in the request is a claim, and it is
    //    checked against the cap before the takeover reply — so this comes
    //    back as the daemon's own refusal and the channel is closed rather
    //    than opened onto a sink that would read 33 MiB to discard it.
    session.send(Frame::Open {
        channel: 5,
        request: br#"{"cmd":"receive","id":1,"name":"huge.bin","len":34000000}"#.to_vec(),
    });
    match session.recv() {
        Frame::Control(line) => {
            let v: serde_json::Value = serde_json::from_slice(&line).expect("json");
            assert_eq!(v["reply"], "error", "{v}");
            assert!(
                v["message"].as_str().unwrap_or("").contains("33554432"),
                "the refusal does not name the limit: {v}"
            );
        }
        other => panic!("expected the daemon's refusal, got {other:?}"),
    }
    assert_eq!(
        session.recv(),
        Frame::Close {
            channel: 5,
            reason: String::new()
        },
        "a refused upload left its channel open"
    );

    // 3. No such session. Same shape, and it is the assertion that proves the
    //    proxy reads the daemon's answer rather than assuming a takeover
    //    succeeded: a channel opened here would have a pump thread delivering
    //    reply lines to the phone as if they were file contents.
    session.send(Frame::Open {
        channel: 6,
        request: br#"{"cmd":"receive","id":9999,"name":"a.png","len":10}"#.to_vec(),
    });
    match session.recv() {
        Frame::Control(line) => {
            let v: serde_json::Value = serde_json::from_slice(&line).expect("json");
            assert_eq!(v["reply"], "error", "{v}");
            assert_eq!(v["kind"], "no_such_session", "{v}");
        }
        other => panic!("expected the daemon's refusal, got {other:?}"),
    }
    assert_eq!(
        session.recv(),
        Frame::Close {
            channel: 6,
            reason: String::new()
        }
    );

    // The connection survives all three: a refused upload is not fatal.
    assert_eq!(session.call(r#"{"cmd":"list"}"#)["reply"], "sessions");
}

#[test]
fn a_channel_onto_a_session_that_does_not_exist_is_refused_with_the_daemons_own_words() {
    // The defect this closes: the proxy pushed a PTY channel whether or not
    // the daemon had actually attached, so a pump thread would sit reading a
    // connection that was still in control mode and deliver reply lines to
    // the device as terminal output.
    let h = harness!("noattach");
    let Some(offer) = open_offer(&h) else { return };
    let device = Device::new();
    assert_eq!(device.pair(&h, &offer, "pixel-8")["ok"], true);
    let mut session = device.connect(&h).expect("a session");

    session.send(Frame::Open {
        channel: 4,
        request: br#"{"cmd":"attach","id":9999,"cols":80,"rows":24,"replay":0}"#.to_vec(),
    });
    // The daemon's own answer, so the device can render it.
    match session.recv() {
        Frame::Control(line) => {
            let v: serde_json::Value = serde_json::from_slice(&line).expect("json");
            assert_eq!(v["reply"], "error", "{v}");
            assert_eq!(v["kind"], "no_such_session", "{v}");
        }
        other => panic!("expected the daemon's refusal, got {other:?}"),
    }
    // And the channel is closed rather than left open onto nothing.
    assert_eq!(
        session.recv(),
        Frame::Close {
            channel: 4,
            reason: String::new()
        }
    );

    // The connection is still usable: a refused channel is not a fatal error.
    assert_eq!(session.call(r#"{"cmd":"list"}"#)["reply"], "sessions");
}

// ─────────────────────────────────────────────────────────────────────────────
//  Push registration (P1-058)
// ─────────────────────────────────────────────────────────────────────────────

/// Where a harness keeps its two stores.
fn stores(h: &Harness) -> (PathBuf, PathBuf) {
    let remote = h.root.join("state").join("rime").join("remote");
    (remote.join("devices.json"), remote.join("push.json"))
}

#[test]
fn a_phone_registers_for_push_over_the_real_session_and_the_key_lands_in_its_own_file() {
    // `push::control` is unit-tested against a `State`. What that cannot say is
    // whether the verb ever REACHES it: `serve.rs` is where the interception
    // lives, and everything else on that channel is forwarded verbatim to
    // `rime-agentd`, which has never heard of a push endpoint. A phone talking
    // to a build where the seam is missing gets `bad_request` and silently
    // falls back to polling — the failure this suite exists to catch, because
    // nothing on either side says a word about it.
    let h = harness!("push");
    let Some(offer) = open_offer(&h) else { return };
    let device = Device::new();
    assert_eq!(device.pair(&h, &offer, "pixel-8")["ok"], true);
    let mut session = device.connect(&h).expect("a session");

    let key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAM";
    let ok = session.call(&format!(
        r#"{{"cmd":"push_register","endpoint":"https://ntfy.sh/upAbCdEf","key":"{key}"}}"#
    ));
    assert_eq!(ok["reply"], "ok", "the push verb did not reach rime-remoted: {ok}");

    let (devices, push) = stores(&h);
    let stored = read_when_it_appears(&push).expect("push.json was never written");
    assert!(stored.contains("https://ntfy.sh/upAbCdEf"), "{stored}");
    assert!(stored.contains(key), "{stored}");

    // 0600 and its own file. `device.rs` asserts `devices.json` holds no
    // secret, and a registration key is one — so it must be neither in that
    // file nor readable by the rest of the machine.
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&push).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "push.json is mode {mode:o}");
    let device_store = std::fs::read_to_string(&devices).expect("devices.json");
    assert!(
        !device_store.contains(key),
        "the registration key was written into devices.json: {device_store}"
    );

    // Unregistering is idempotent and empties it.
    assert_eq!(session.call(r#"{"cmd":"push_unregister"}"#)["reply"], "ok");
    assert_eq!(session.call(r#"{"cmd":"push_unregister"}"#)["reply"], "ok");
    let after = std::fs::read_to_string(&push).expect("push.json");
    assert!(!after.contains("ntfy.sh"), "the registration survived unregister: {after}");

    // Everything else on the channel still goes to the daemon untouched.
    assert_eq!(session.call(r#"{"cmd":"list"}"#)["reply"], "sessions");
}

#[test]
fn a_push_endpoint_the_machine_cannot_use_is_refused_and_nothing_is_stored() {
    // Refused at the machine as well as at the phone. The phone checks first
    // so it does not burn a round trip, but a client that skipped the check —
    // or an older one — must not be able to make this machine post an alert in
    // the clear, and the endpoint URL is the capability to notify that phone.
    let h = harness!("push-refuse");
    let Some(offer) = open_offer(&h) else { return };
    let device = Device::new();
    assert_eq!(device.pair(&h, &offer, "pixel-8")["ok"], true);
    let mut session = device.connect(&h).expect("a session");

    for bad in [
        r#"{"cmd":"push_register","endpoint":"http://ntfy.sh/upA","key":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAM"}"#,
        r#"{"cmd":"push_register","endpoint":"https://ntfy.sh/upA","key":"AAAA"}"#,
        r#"{"cmd":"push_register","endpoint":"","key":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAM"}"#,
    ] {
        let reply = session.call(bad);
        assert_eq!(reply["reply"], "error", "{bad} was accepted: {reply}");
        // And it is refused for the RIGHT reason. Without this the test passes
        // on a build where the seam in `serve.rs` is missing entirely: the line
        // reaches `rime-agentd`, which does not know the verb and also answers
        // `error` — the same shape for "your endpoint is wrong" and "this
        // machine cannot push at all", which are opposite facts.
        let message = reply["message"].as_str().unwrap_or_default();
        assert!(!message.is_empty(), "{reply}");
        assert!(
            !message.contains("unknown variant"),
            "the verb never reached rime-remoted; this is a machine that cannot push, \
             not an endpoint it refused: {reply}"
        );
    }
    let (_, push) = stores(&h);
    let stored = std::fs::read_to_string(&push).unwrap_or_default();
    assert!(!stored.contains("ntfy.sh"), "a refused endpoint was stored: {stored}");
}

#[test]
fn revoking_a_device_also_stops_it_being_woken() {
    // The one direction of "revoke, eventually" that survives losing the
    // connection. Push needs no connection by construction, so a registration
    // that outlived the pairing would go on waking a phone the owner threw off
    // this machine — and dropping the live socket, which is what revoke did
    // before, would not touch it.
    let h = harness!("push-revoke");
    let Some(offer) = open_offer(&h) else { return };
    let device = Device::new();
    assert_eq!(device.pair(&h, &offer, "lost-phone")["ok"], true);
    let mut session = device.connect(&h).expect("a session");
    let key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAM";
    assert_eq!(
        session.call(&format!(
            r#"{{"cmd":"push_register","endpoint":"https://ntfy.sh/upLost","key":"{key}"}}"#
        ))["reply"],
        "ok"
    );
    let (_, push) = stores(&h);
    assert!(
        read_when_it_appears(&push).expect("push.json").contains("upLost"),
        "the registration was not stored, so revoking it proves nothing"
    );

    assert_eq!(h.control(r#"{"cmd":"revoke","device":"lost-phone"}"#)["reply"], "ok");
    let after = std::fs::read_to_string(&push).expect("push.json");
    assert!(
        !after.contains("upLost"),
        "a revoked phone is still registered for push: {after}"
    );
}

/// Read a file the daemon writes, once it is there.
///
/// The reply is sent after the store is written, so this should not have to
/// wait — but asserting on a file another process writes is the one place a
/// zero-length read means "not yet" rather than "empty", and a flake here
/// would be read as a defect in the code under test.
fn read_when_it_appears(path: &std::path::Path) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(path) {
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}
