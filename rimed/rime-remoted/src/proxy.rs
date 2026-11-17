//! Forwarding a device's frames into `rime-agentd`, with an origin it can
//! trust.
//!
//! ## The rule, and it is the only rule that matters here
//!
//! **Every connection this module opens to `rime-agentd` sends
//! `DeclareOrigin` first, and refuses to send anything else if that fails.**
//!
//! `rime-remoted` is not a managed session, so the daemon classifies it from
//! its own cgroup. As a systemd user unit that is `scheduled-job` — not
//! local, so it cannot approve a root operation, which is right by accident.
//! Started any other way it is `local-terminal`, and then every request this
//! process forwards for a phone is filed as a human at the keyboard. The
//! declaration is what makes the origin a property of *what this connection
//! is carrying* rather than of how the process happened to be started.
//!
//! The daemon checks it: `rime_agent_core::origin::may_declare` refuses any
//! declaration that is not a narrowing, and refuses the two local origins by
//! name whatever was observed. So this cannot launder a remote request into a
//! local one even if it tried, and a build of the daemon that predates the
//! connection latch answers `permission_denied` — loudly, which is why
//! [`Agentd::open`] treats that as fatal rather than carrying on.
//!
//! ## Why one agentd connection per control frame
//!
//! The daemon serves one connection per thread and reads one request per
//! line. Sharing a single connection between a phone's control traffic and
//! its terminals would serialise them behind each other: a `list` that
//! arrived while an `attach` was streaming would wait for the terminal to
//! close. A connection per control frame costs a `connect(2)` on a Unix
//! socket, which is cheap, and a connection per attached PTY for as long as
//! it is attached, which is what the daemon expects anyway.
//!
//! The cost is that the declaration is sent every time. That is a feature: a
//! latch that survived across requests would be state, and state is what has
//! to be got right.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// How long to wait for the daemon on a control request.
///
/// Generous, because a request that reaches a human — a system-access grant
/// prompting for authentication — legitimately takes as long as the person
/// does. `rime-agentd`'s own `Request::waits_on_a_human` names those, and a
/// proxy that timed out under them would report a failure while a dialog was
/// still on screen.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(300);

/// One connection to `rime-agentd`, already told where it came from.
pub struct Agentd {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

/// Why a proxied connection could not be established or used.
#[derive(Debug)]
pub enum ProxyError {
    /// The daemon is not running, or would not accept a connection.
    Unreachable(String),
    /// The daemon answered a takeover verb with something other than the word
    /// that means it took the connection over.
    ///
    /// Two verbs reach this: `attach`, answered `attached`, and `receive`,
    /// answered `receiving`. Carries the daemon's own reply, so the device is
    /// told "no session 4" or "32 MiB is past the limit" rather than being
    /// handed a channel that closes for no stated reason.
    NotTakenOver(Vec<u8>),
    /// The daemon refused the origin declaration.
    ///
    /// Fatal for the connection, and deliberately not recoverable. A daemon
    /// that will not record where a request came from is a daemon this
    /// service must not send it to.
    OriginRefused(String),
    Io(std::io::Error),
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProxyError::Unreachable(w) => write!(
                f,
                "the agent runtime is not reachable: {w}. Start it with `rime agent enable`"
            ),
            ProxyError::OriginRefused(w) => write!(
                f,
                "the agent runtime would not record this connection as remote ({w}), so nothing \
                 was forwarded: a remote request filed under a local origin is what §7 exists to \
                 prevent"
            ),
            ProxyError::NotTakenOver(reply) => write!(
                f,
                "the agent runtime did not take the channel over: {}",
                String::from_utf8_lossy(reply)
            ),
            ProxyError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ProxyError {}

impl From<std::io::Error> for ProxyError {
    fn from(e: std::io::Error) -> ProxyError {
        ProxyError::Io(e)
    }
}

impl Agentd {
    /// Open a connection and declare its origin, in that order and with
    /// nothing in between.
    ///
    /// `actor` is the paired device's id. It reaches the daemon's session
    /// records and privilege requests, so a human deciding on a root
    /// operation is told *which* device asked and not only that a remote
    /// something did.
    pub fn open(socket: &Path, actor: &str) -> Result<Agentd, ProxyError> {
        let stream = UnixStream::connect(socket)
            .map_err(|e| ProxyError::Unreachable(format!("{}: {e}", socket.display())))?;
        stream.set_read_timeout(Some(CONTROL_TIMEOUT)).ok();
        stream.set_write_timeout(Some(CONTROL_TIMEOUT)).ok();
        let reader = BufReader::new(stream.try_clone()?);
        let mut agentd = Agentd { stream, reader };
        agentd.declare(actor)?;
        Ok(agentd)
    }

    /// The first line on every connection this service opens.
    fn declare(&mut self, actor: &str) -> Result<(), ProxyError> {
        let line = serde_json::json!({
            "cmd": "declare_origin",
            // §7's name for "a human elsewhere is driving this machine". The
            // wire spelling is `claude-remote-control` and Rime Remote is not
            // Claude Remote Control; reusing it is a deliberate decision, made
            // because the policy table is keyed on it and an eighth origin
            // would be a protocol break for a rename. See the design note.
            "origin": "claude-remote-control",
            "actor": actor,
        })
        .to_string();
        writeln!(self.stream, "{line}")?;
        self.stream.flush()?;
        let mut reply = String::new();
        self.reader.read_line(&mut reply)?;
        let value: serde_json::Value = serde_json::from_str(reply.trim())
            .map_err(|e| ProxyError::OriginRefused(format!("unreadable reply: {e}")))?;
        if value["reply"] == "ok" {
            return Ok(());
        }
        Err(ProxyError::OriginRefused(
            value["message"]
                .as_str()
                .unwrap_or("the runtime did not accept the declaration")
                .to_string(),
        ))
    }

    /// Send one request line and read one reply line.
    pub fn round_trip(&mut self, request: &[u8]) -> Result<Vec<u8>, ProxyError> {
        self.stream.write_all(request)?;
        self.stream.write_all(b"\n")?;
        self.stream.flush()?;
        let mut reply = String::new();
        self.reader.read_line(&mut reply)?;
        Ok(reply.trim_end_matches('\n').as_bytes().to_vec())
    }

    /// Send a takeover request and take the raw stream, plus whatever bytes
    /// arrived alongside the reply.
    ///
    /// The buffered bytes matter: after `attached` the daemon immediately
    /// begins the scrollback replay, so a reader that dropped its buffer would
    /// lose the first burst of a terminal the phone asked to see. After
    /// `receiving` there is nothing to lose — the daemon is waiting to be
    /// written to — and the same code path returns an empty buffer, which is
    /// why this is one function rather than two.
    ///
    /// The timeouts come off for both. A PTY may sit idle for hours, and a
    /// deadline on it would end the terminal rather than the request; an
    /// upload is bounded by the DAEMON's two clocks, which are the ones that
    /// can see how far it has got.
    pub fn take_over(
        mut self,
        request: &[u8],
    ) -> Result<(UnixStream, Vec<u8>, Vec<u8>), ProxyError> {
        self.stream.write_all(request)?;
        self.stream.write_all(b"\n")?;
        self.stream.flush()?;
        let mut reply = String::new();
        self.reader.read_line(&mut reply)?;
        let buffered = self.reader.buffer().to_vec();
        self.stream.set_read_timeout(None).ok();
        self.stream.set_write_timeout(None).ok();
        Ok((
            self.stream,
            reply.trim_end_matches('\n').as_bytes().to_vec(),
            buffered,
        ))
    }
}

/// Whether a control payload is a verb that takes the connection over, and so
/// needs a channel rather than a reply.
///
/// Three of them. `attach` turns the daemon's connection into the session's
/// PTY; `receive` turns it into a sink for a file handed to a session, and
/// `save_file` into a sink for a file saved in Downloads. All three stop
/// being control channels the moment the daemon answers, so both are wrong on
/// channel zero for the same reason — the proxy would be left expecting reply
/// lines from something that is now a byte pipe, and the device's bytes would
/// go nowhere.
///
/// Read out of the JSON rather than out of the frame tag, so a client that
/// sends one as an ordinary control frame gets the same treatment as one that
/// opens a channel.
pub fn takes_over_the_channel(payload: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(payload)
        .map(|v| v["cmd"] == "attach" || v["cmd"] == "receive" || v["cmd"] == "save_file")
        .unwrap_or(false)
}

/// The daemon's replies that mean "this connection is no longer control".
///
/// A pair rather than a single string because there are two takeover verbs,
/// and a proxy that recognised only the older one would open a channel onto a
/// `receive` and then treat the daemon's refusal line as the first bytes of a
/// terminal.
pub const TAKEOVER_REPLIES: [&str; 3] = ["attached", "receiving", "saving"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_takeover_verbs_are_recognised_however_the_client_sends_them() {
        assert!(takes_over_the_channel(
            br#"{"cmd":"attach","id":1,"cols":80,"rows":24}"#
        ));
        // The second one. A proxy that knew only `attach` would answer this on
        // channel zero and then wait for reply lines from a daemon that is
        // waiting to be written to — both ends blocked on each other.
        assert!(takes_over_the_channel(
            br#"{"cmd":"receive","id":1,"name":"shot.png","len":2048}"#
        ));
        // Key order is not fixed on the wire.
        assert!(takes_over_the_channel(br#"{"id":1,"cmd":"attach"}"#));
        assert!(takes_over_the_channel(br#"{"len":1,"cmd":"receive"}"#));
        // The third, contract §1.8. On channel zero it would wedge the same way.
        assert!(takes_over_the_channel(
            br#"{"cmd":"save_file","name":"IMG.jpg","len":2048}"#
        ));
        assert!(takes_over_the_channel(br#"{"len":1,"cmd":"save_file"}"#));
        // Its sibling is an ordinary request: a phone sends it through
        // `MachineLink.request`, and as a takeover it would never be answered.
        assert!(!takes_over_the_channel(br#"{"cmd":"clipboard_write","text":"x"}"#));
        assert!(!takes_over_the_channel(br#"{"cmd":"list"}"#));
        // `clipboard` is the verb this list was most recently checked against,
        // and the answer has to be NO. The phone builds it as an ordinary
        // control request and sends it through `MachineLink.request` rather
        // than through `Upload`; if it ever became a takeover verb, that
        // client would open a channel nobody answers and the reply would
        // never come. Asserted here, in the file that owns the predicate,
        // rather than restated as a string comparison in `rime-agent-core`,
        // which cannot see this function.
        assert!(!takes_over_the_channel(br#"{"cmd":"clipboard"}"#));
        assert!(!takes_over_the_channel(br#"{"cmd":"inject","id":1,"source":"/x"}"#));
        assert!(!takes_over_the_channel(br#"{"cmd":"attach_something_else"}"#));
        assert!(!takes_over_the_channel(br#"{"cmd":"receive_something_else"}"#));
        // Not JSON at all: not a takeover, and not a panic either.
        assert!(!takes_over_the_channel(b"attach"));
        assert!(!takes_over_the_channel(b""));
    }

    #[test]
    fn the_declaration_is_the_first_thing_on_the_wire_and_is_fatal_when_refused() {
        // A real Unix socket standing in for the daemon, so this measures the
        // order bytes actually leave rather than the order the code reads in.
        let dir = std::env::temp_dir().join(format!(
            "rime-remoted-proxy-{}-{}",
            std::process::id(),
            rime_remote_core::now_ms()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let socket = dir.join("control.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");

        // A stand-in daemon that refuses the declaration, as a build without
        // the connection latch does.
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut first = String::new();
            reader.read_line(&mut first).expect("read");
            let mut w = stream;
            writeln!(
                w,
                r#"{{"reply":"error","kind":"permission_denied","message":"only a managed session can narrow its own origin"}}"#
            )
            .expect("write");
            w.flush().ok();
            // Anything the proxy sends after being refused would arrive here.
            let mut second = String::new();
            let n = reader.read_line(&mut second).unwrap_or(0);
            (first, n, second)
        });

        let err = match Agentd::open(&socket, "pixel-8-office") {
            Err(e) => e,
            Ok(_) => panic!("a refused declaration produced a usable connection"),
        };
        assert!(
            matches!(err, ProxyError::OriginRefused(_)),
            "{err}"
        );
        let (first, n, second) = handle.join().expect("join");
        let v: serde_json::Value = serde_json::from_str(first.trim()).expect("json");
        assert_eq!(v["cmd"], "declare_origin", "{first}");
        assert_eq!(v["origin"], "claude-remote-control", "{first}");
        assert_eq!(v["actor"], "pixel-8-office", "{first}");
        assert_eq!(
            n, 0,
            "the proxy sent {second:?} after its declaration was refused"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_accepted_declaration_yields_a_connection_that_forwards() {
        let dir = std::env::temp_dir().join(format!(
            "rime-remoted-proxy-ok-{}-{}",
            std::process::id(),
            rime_remote_core::now_ms()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let socket = dir.join("control.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut w = stream;
            let mut decl = String::new();
            reader.read_line(&mut decl).expect("read");
            writeln!(w, r#"{{"reply":"ok"}}"#).expect("write");
            w.flush().ok();
            let mut req = String::new();
            reader.read_line(&mut req).expect("read");
            writeln!(w, r#"{{"reply":"sessions","sessions":[]}}"#).expect("write");
            w.flush().ok();
            req
        });

        let mut agentd = Agentd::open(&socket, "pixel-8").expect("open");
        let reply = agentd.round_trip(br#"{"cmd":"list"}"#).expect("round trip");
        assert_eq!(reply, br#"{"reply":"sessions","sessions":[]}"#);
        assert_eq!(handle.join().expect("join").trim(), r#"{"cmd":"list"}"#);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_absent_daemon_is_unreachable_and_names_the_way_out() {
        let e = match Agentd::open(Path::new("/nonexistent/rime-agentd.sock"), "d") {
            Err(e) => e,
            Ok(_) => panic!("connected to nothing"),
        };
        assert!(matches!(e, ProxyError::Unreachable(_)), "{e}");
        assert!(e.to_string().contains("rime agent enable"), "{e}");
    }
}
