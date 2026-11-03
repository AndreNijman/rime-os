//! One connection doing several things at once, against the real
//! `rime-remoted` binary.
//!
//! `end_to_end.rs` proves the chain against the real `rime-agentd`. This file
//! proves what the connection does while a request is **slow** — that the
//! frame loop keeps moving a terminal and answering pings while a control
//! request waits, that replies still come back in the order they were asked
//! for, and that a device which stops answering is let go — and none of that
//! can be arranged with the real daemon: nothing it answers is reliably slow,
//! and what it answers slowly (a privilege prompt) needs a human.
//!
//! ## A stand-in for `rime-agentd`, on the socket path the real one uses
//!
//! `rime-remoted` finds the daemon at `$XDG_RUNTIME_DIR/rime-agentd/
//! control.sock` and forwards control lines to it verbatim. So a Unix
//! listener there, speaking the same line protocol, is indistinguishable to
//! the proxy — it gets the `declare_origin` first, as the real one does — and
//! it can be told to take two seconds over one request. It also records every
//! line it is sent, which is how `remote_hello` is shown never to reach it.
//!
//! ## The device store is written, not paired into
//!
//! Pairing needs a human at the keyboard (§7), which is right, and which is
//! why every pairing test in `end_to_end.rs` skips when this suite runs
//! under an agent or a service. The claims here are about the session AFTER
//! pairing, so the device is written into the store with the store's own
//! `pair` before the daemon starts, and the daemon authenticates it exactly
//! as it would one that scanned a code. The pairing path keeps its own tests.
//!
//! Every process runs in a private runtime and state directory and is killed
//! by pid; nothing here touches a running `rime-remoted` or `rime-agentd`,
//! announces anything on the network (`--no-announce`), or dials a relay.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod common;

use rime_remote_core::device::DeviceStore;
use rime_remote_core::noise::{Channel, Handshake};
use rime_remote_core::wire::Frame;

const V: u32 = rime_remote_core::REMOTE_PROTOCOL_VERSION;
const HELLO_SESSION: u8 = b'S';

/// What the stand-in daemon writes after `attached`, before it starts
/// echoing: far more than the proxy's 8 KiB line buffer, so part of it comes
/// back with the reply and the rest through the pump, and every byte is a
/// function of its index so a loss or a reordering shows.
fn scrollback() -> Vec<u8> {
    let mut out = b"scrollback:".to_vec();
    out.extend((0..40_000u32).map(|i| b'a' + (i % 26) as u8));
    out.extend_from_slice(b":end");
    out
}

/// A stand-in `rime-agentd`: the origin declaration, then one request per
/// connection, answered from a tiny vocabulary of its own.
///
/// * `{"cmd":"slow","ms":N}` — waits N ms, then `{"reply":"slow","ms":N}`.
/// * `{"cmd":"echo","n":N}` — `{"reply":"echo","n":N}` at once.
/// * `{"cmd":"attach","id":N}` — `attached`, the scrollback, then echoes
///   every byte it is sent, like `cat` in a PTY.
/// * anything else — the daemon's `bad_request`.
fn stand_in_agentd(socket: &Path) -> Arc<Mutex<Vec<String>>> {
    std::fs::create_dir_all(socket.parent().expect("a directory")).expect("mkdir");
    let listener = UnixListener::bind(socket).expect("bind the stand-in daemon");
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let record = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let record = Arc::clone(&record);
            std::thread::spawn(move || {
                let mut writer = stream.try_clone().expect("clone");
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let decl: serde_json::Value = serde_json::from_str(line.trim()).expect("json");
                assert_eq!(decl["cmd"], "declare_origin", "the proxy skipped its declaration");
                let _ = writeln!(writer, r#"{{"reply":"ok"}}"#);
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap_or_default();
                let cmd = v["cmd"].as_str().unwrap_or_default().to_string();
                record.lock().expect("seen").push(cmd.clone());
                match cmd.as_str() {
                    "slow" => {
                        let ms = v["ms"].as_u64().unwrap_or(0);
                        std::thread::sleep(Duration::from_millis(ms));
                        let _ = writeln!(writer, r#"{{"reply":"slow","ms":{ms}}}"#);
                    }
                    "echo" => {
                        let _ = writeln!(writer, r#"{{"reply":"echo","n":{}}}"#, v["n"]);
                    }
                    "attach" => {
                        let mut out = format!(r#"{{"reply":"attached","id":{}}}"#, v["id"])
                            .into_bytes();
                        out.push(b'\n');
                        out.extend_from_slice(&scrollback());
                        if writer.write_all(&out).is_err() {
                            return;
                        }
                        let mut buf = [0u8; 4096];
                        loop {
                            match reader.read(&mut buf) {
                                Ok(0) | Err(_) => return,
                                Ok(n) => {
                                    if writer.write_all(&buf[..n]).is_err() {
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    _ => {
                        let _ = writeln!(
                            writer,
                            r#"{{"reply":"error","kind":"bad_request","message":"not in the stand-in's vocabulary"}}"#
                        );
                    }
                }
            });
        }
    });
    seen
}

struct Harness {
    remoted: Child,
    root: PathBuf,
    port: u16,
    control: PathBuf,
    log: PathBuf,
    /// Every request line the stand-in daemon received, by verb.
    daemon_saw: Arc<Mutex<Vec<String>>>,
    device: Device,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.remoted.kill();
        let _ = self.remoted.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct Device {
    secret: [u8; 32],
}

impl Harness {
    fn start(tag: &str, extra: &[&str]) -> Harness {
        let root = std::env::temp_dir().join(format!(
            "rime-remote-live-{}-{tag}-{}",
            std::process::id(),
            rime_remote_core::now_ms()
        ));
        let runtime = root.join("run");
        let state = root.join("state");
        std::fs::create_dir_all(&runtime).expect("runtime");
        std::fs::create_dir_all(&state).expect("state");

        // The device, written into the store with the store's own `pair`.
        let kp = rime_remote_core::noise::builder()
            .generate_keypair()
            .expect("keypair");
        let device = Device {
            secret: kp.private.as_slice().try_into().expect("32"),
        };
        let store_path = DeviceStore::path_in(&state);
        std::fs::create_dir_all(store_path.parent().expect("dir")).expect("store dir");
        let mut store = DeviceStore::load(&store_path).expect("an empty store");
        store
            .pair(
                &rime_remote_core::b64_encode(&kp.public),
                "live-test",
                rime_remote_core::now_ms(),
                false,
            )
            .expect("pair into the store");
        store.save(&store_path).expect("save the store");

        let daemon_saw = stand_in_agentd(&runtime.join("rime-agentd").join("control.sock"));

        // `--port 0`, and the port the daemon got read back from its log: see
        // `common` for why a probed port handed over as `--port N` races.
        let log = root.join("remoted.log");
        let mut args = vec![
            "--port".to_string(),
            "0".to_string(),
            "--allow-foreground".into(),
            "--no-announce".into(),
            "--handshake-timeout-ms".into(),
            "1000".into(),
        ];
        args.extend(extra.iter().map(|s| s.to_string()));
        let remoted = Command::new(env!("CARGO_BIN_EXE_rime-remoted"))
            .args(&args)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("XDG_STATE_HOME", &state)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).expect("log file"))
            .spawn()
            .expect("spawn rime-remoted");
        let mut h = Harness {
            remoted,
            root,
            port: 0,
            control: runtime.join("rime-remoted").join("control.sock"),
            log,
            daemon_saw,
            device,
        };
        match common::remoted_port(&mut h.remoted, &h.control, &h.log, Duration::from_secs(15)) {
            Ok(port) => {
                h.port = port;
                h
            }
            Err(why) => panic!("{why}"),
        }
    }

    fn status(&self) -> serde_json::Value {
        let stream = UnixStream::connect(&self.control).expect("connect");
        stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
        let mut writer = stream.try_clone().expect("clone");
        let mut reader = BufReader::new(stream);
        writeln!(writer, r#"{{"cmd":"status"}}"#).expect("write");
        let mut reply = String::new();
        reader.read_line(&mut reply).expect("read");
        serde_json::from_str(reply.trim()).unwrap_or_else(|e| panic!("{e}: {reply}"))
    }

    fn connect(&self) -> Session {
        let desktop: [u8; 32] =
            rime_remote_core::b64_decode(self.status()["key"].as_str().expect("a key"))
                .expect("base64")
                .as_slice()
                .try_into()
                .expect("32");
        let mut socket = TcpStream::connect(("127.0.0.1", self.port)).expect("connect");
        socket.set_read_timeout(Some(Duration::from_secs(15))).ok();
        socket.write_all(&[HELLO_SESSION]).expect("hello");
        let mut hs = Handshake::session_initiator(&self.device.secret, &desktop, V).expect("ik");
        write_message(&mut socket, &hs.write(b"").expect("m1"));
        let m2 = read_message(&mut socket).expect("the desktop refused the seeded device");
        hs.read(&m2).expect("m2");
        Session {
            socket,
            channel: hs.into_transport().expect("transport"),
        }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

struct Session {
    socket: TcpStream,
    channel: Channel,
}

impl Session {
    fn send(&mut self, frame: Frame) {
        let sealed = self.channel.seal(&frame.encode().expect("encode")).expect("seal");
        write_message(&mut self.socket, &sealed);
    }

    /// The next frame, or `None` when the desktop closed the connection.
    /// Answers nothing.
    fn raw(&mut self) -> Option<Frame> {
        let message = read_message(&mut self.socket)?;
        let plain = self.channel.open(&message).expect("open");
        Some(Frame::decode(&plain).expect("decode"))
    }

    /// The next frame that is not the desktop's own ping, answering those on
    /// the way as a phone does. A `Pong` to a ping THIS end sent is returned.
    fn next(&mut self) -> Frame {
        loop {
            match self.raw().expect("the desktop closed the connection") {
                Frame::Ping { token } => self.send(Frame::Pong { token }),
                frame => return frame,
            }
        }
    }

    fn call(&mut self, request: &str) -> serde_json::Value {
        self.send(Frame::Control(request.as_bytes().to_vec()));
        loop {
            if let Frame::Control(line) = self.next() {
                return serde_json::from_slice(&line).expect("json");
            }
        }
    }

    fn open(&mut self, channel: u32, id: u32) {
        self.send(Frame::Open {
            channel,
            request: format!(r#"{{"cmd":"attach","id":{id},"cols":80,"rows":24}}"#).into_bytes(),
        });
    }
}

fn write_message(w: &mut impl Write, m: &[u8]) {
    let mut framed = (m.len() as u32).to_be_bytes().to_vec();
    framed.extend_from_slice(m);
    w.write_all(&framed).expect("write");
}

fn read_message(r: &mut impl Read) -> Option<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).ok()?;
    let mut buf = vec![0u8; u32::from_be_bytes(len) as usize];
    r.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn json(line: &[u8]) -> serde_json::Value {
    serde_json::from_slice(line).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(line)))
}

#[test]
fn remote_hello_is_answered_by_the_service_and_never_reaches_the_daemon() {
    let h = Harness::start("hello", &[]);
    let mut s = h.connect();
    let hello = s.call(r#"{"cmd":"remote_hello"}"#);
    assert_eq!(hello["reply"], "remote", "{hello}");
    assert_eq!(hello["version"], V, "{hello}");
    assert_eq!(
        hello["features"],
        serde_json::json!(["nodelay", "control_worker", "mux_attach", "liveness"]),
        "{hello}"
    );
    // The same list a pairing code carries, read the same way at the same
    // moment — so a phone that refreshes its stored addresses from this gets
    // exactly what it would get by pairing again.
    assert_eq!(hello["lan"], h.status()["lan"], "{hello}");
    for a in hello["lan"].as_array().expect("an array") {
        let addr: std::net::SocketAddr = a.as_str().expect("a string").parse().expect("host:port");
        assert_eq!(addr.port(), h.port, "{a} is not the port this daemon listens on");
    }

    // Everything else still reaches the daemon, verbatim.
    assert_eq!(s.call(r#"{"cmd":"echo","n":1}"#)["reply"], "echo");
    let saw = h.daemon_saw.lock().expect("seen").clone();
    assert!(saw.contains(&"echo".to_string()), "{saw:?}");
    assert!(
        !saw.iter().any(|c| c == "remote_hello"),
        "remote_hello was forwarded to the daemon: {saw:?}"
    );
}

#[test]
fn a_slow_request_does_not_stop_terminals_or_pings_and_replies_keep_their_order() {
    let h = Harness::start("worker", &[]);
    let mut s = h.connect();

    // A terminal on the control connection. The FIRST frame back must be the
    // `attached` reply: the pump used to start before it was sent, so
    // scrollback could reach the phone ahead of the reply that tells it the
    // channel exists, and the phone drops Data for a channel it does not know.
    s.open(1, 1);
    match s.next() {
        Frame::Control(line) => assert_eq!(json(&line)["reply"], "attached"),
        other => panic!("scrollback overtook the attach reply: {other:?}"),
    }
    let want = scrollback();
    let mut got = Vec::new();
    while got.len() < want.len() {
        match s.next() {
            Frame::Data { channel: 1, bytes } => got.extend_from_slice(&bytes),
            other => panic!("unexpected {other:?} during the scrollback"),
        }
    }
    assert_eq!(got, want, "the scrollback was lost or reordered between reply, buffer and pump");

    // Now a request the daemon takes two seconds over, and behind it an
    // attach and an ordinary request. The phone pairs replies with requests
    // by order alone, so all three must come back in the order sent.
    let asked = Instant::now();
    s.send(Frame::Control(br#"{"cmd":"slow","ms":2000}"#.to_vec()));
    s.open(2, 2);
    // Typed into channel 2 before its `attached` has come back: queued
    // behind its Open, delivered after it, not dropped and not reordered.
    s.send(Frame::Data {
        channel: 2,
        bytes: b"typed-before-the-reply".to_vec(),
    });
    s.send(Frame::Control(br#"{"cmd":"echo","n":2}"#.to_vec()));

    // While the slow one is in flight: the terminal on channel 1 still
    // echoes, and a ping is still answered. Before the worker, both waited
    // two seconds behind the request.
    s.send(Frame::Data {
        channel: 1,
        bytes: b"still-moving".to_vec(),
    });
    s.send(Frame::Ping { token: 77 });
    let (mut echoed, mut ponged) = (Vec::new(), None);
    let mut replies: Vec<serde_json::Value> = Vec::new();
    let mut on_two = Vec::new();
    while echoed.len() < b"still-moving".len() || ponged.is_none() {
        match s.next() {
            Frame::Data { channel: 1, bytes } => echoed.extend_from_slice(&bytes),
            Frame::Pong { token: 77 } => ponged = Some(asked.elapsed()),
            Frame::Control(line) => replies.push(json(&line)),
            Frame::Data { channel: 2, bytes } => on_two.extend_from_slice(&bytes),
            other => panic!("unexpected {other:?}"),
        }
    }
    let moving = asked.elapsed();
    assert_eq!(echoed, b"still-moving");
    assert!(
        replies.is_empty(),
        "a reply arrived before the slow request had finished: {replies:?}"
    );
    assert!(
        moving < Duration::from_millis(1500),
        "the terminal and the ping waited {moving:?} behind a slow request"
    );
    assert!(ponged.is_some_and(|t| t < Duration::from_millis(1500)), "{ponged:?}");

    // The three replies, in the order asked.
    while replies.len() < 3 {
        match s.next() {
            Frame::Control(line) => replies.push(json(&line)),
            Frame::Data { channel: 2, bytes } => on_two.extend_from_slice(&bytes),
            Frame::Data { channel: 1, .. } => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(asked.elapsed() >= Duration::from_millis(2000), "the stand-in was not slow");
    let order: Vec<&str> = replies
        .iter()
        .map(|r| r["reply"].as_str().unwrap_or("?"))
        .collect();
    assert_eq!(order, ["slow", "attached", "echo"], "{replies:?}");
    assert_eq!(replies[2]["n"], 2);

    // And the bytes typed ahead of channel 2's reply: its scrollback first,
    // then the echo of what was typed.
    let mut want = scrollback();
    want.extend_from_slice(b"typed-before-the-reply");
    while on_two.len() < want.len() {
        match s.next() {
            Frame::Data { channel: 2, bytes } => on_two.extend_from_slice(&bytes),
            Frame::Data { channel: 1, .. } => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(on_two, want, "bytes typed before the attach reply were lost or reordered");
}

#[test]
fn a_device_that_stops_answering_pings_is_let_go_and_one_that_answers_is_kept() {
    // 100 ms pings, so three misses take a fraction of a second — and the
    // two-second floor is what decides when the silent one is let go.
    let h = Harness::start("liveness", &["--ping-interval-ms", "100"]);

    let mut silent = h.connect();
    let since = Instant::now();
    let mut pings = 0;
    // Read everything, answer nothing, until the desktop hangs up.
    while let Some(frame) = silent.raw() {
        assert!(matches!(frame, Frame::Ping { .. }), "{frame:?}");
        pings += 1;
        assert!(since.elapsed() < Duration::from_secs(10), "a silent device was kept");
    }
    let took = since.elapsed();
    assert!(pings >= 3, "closed after {pings} pings");
    assert!(took >= Duration::from_millis(1500), "closed inside the floor, after {took:?}");

    // One line, saying why, for the one connection.
    let deadline = Instant::now() + Duration::from_secs(5);
    let lines = loop {
        let n = h.log().matches("answered none of the last 3 pings").count();
        if n > 0 || Instant::now() > deadline {
            break n;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(lines, 1, "{}", h.log());

    // A device that answers is kept well past the same window.
    let mut kept = h.connect();
    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until {
        match kept.raw().expect("a device that answers was let go") {
            Frame::Ping { token } => kept.send(Frame::Pong { token }),
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(kept.call(r#"{"cmd":"remote_hello"}"#)["reply"], "remote");
    // And the status page lists exactly the live one.
    let connections = h.status()["connections"].clone();
    assert_eq!(
        connections.as_array().map(Vec::len),
        Some(1),
        "the silent connection was left in the live list: {connections}"
    );
}
