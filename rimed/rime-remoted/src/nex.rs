//! The bridge to `nexd`, the agent harness, for a paired device.
//!
//! ## One pairing, one network face
//!
//! `nexd` listens on a local socket only. A phone reaches it through this
//! service, over the connection it already authenticated, so there is no second
//! pairing stack, no second listener and no second thing to revoke: revoking
//! the device here revokes its nex access with everything else.
//!
//! ## What the device is to nexd
//!
//! Every connection this module opens says FIRST, before anything the device
//! asked for, `system.declare_origin {kind: "device", id: <device id>}`. The id
//! is the one this service authenticated, never one the device supplied, and
//! the declaration only ever narrows: nexd refuses local-only methods to a
//! declared device, lets it work only in workspaces that already have a
//! session, approves only the scopes the owner listed in `remote_approve`, and
//! records `device:<id>` as the actor of everything it writes.
//!
//! **Fail closed.** If the declaration is refused, times out, or nexd answers
//! with something that is not its reply, the device's request is never
//! written. A request that reached nexd undeclared would run with the authority
//! of a local caller.
//!
//! ## The two words a device uses
//!
//! * `{"cmd":"nex","request":{"method":..,"params":..}}` on the control channel
//!   answers `{"reply":"nex","response":<the JSON-RPC response>}`. nexd's own
//!   error (for example `-32002`, refused for a paired device) stays inside
//!   `response`; `{"reply":"error","kind":"unavailable"}` means nexd itself
//!   could not be reached. The same request as a channel `Open` answers
//!   `{"reply":"streaming"}` and carries the response line as `Data`, then
//!   closes: that is the form for an answer past one control frame.
//! * `{"cmd":"nex_events","after_gseq":N}` as a channel `Open` is nex's global
//!   event feed: the backlog after `N` as one response line, then one `event`
//!   notification line per event, each with its `gseq`, for as long as the
//!   channel is open. `"tail":true` starts from now. The channel is read-only:
//!   whatever the device sends on it is dropped.
//!
//! ## Retries
//!
//! A write's `request_id` is nexd's idempotency key. It is passed through as
//! the device wrote it and never minted here, so a `turns.submit` whose reply
//! was lost on the relay and is sent again is answered from nexd's record
//! instead of running twice.
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// How long one request may take, from connecting to its reply: one deadline
/// for the declaration and the request together, not a clock per read.
///
/// The control worker is serial, so a nexd that stopped answering would
/// otherwise hold up every control request of that phone behind it.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// The verbs this module answers: `nex` on the control channel or as a
/// channel, `nex_events` as a channel only.
pub const VERBS: [&str; 2] = ["nex", "nex_events"];

/// The JSON-RPC id of the declaration; the device's request always gets `1`.
const DECLARE_ID: &str = "remoted-declare";

/// nexd's default socket, the path `nexd` and `nex` both use.
pub fn default_socket() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(r) => PathBuf::from(r).join("nex/nexd.sock"),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
            .join(".local/share/nex/nexd.sock"),
    }
}

/// Whether nexd is listening, which is what decides the `nex` feature.
pub fn available(socket: &Path) -> bool {
    UnixStream::connect(socket).is_ok()
}

#[derive(Debug, PartialEq)]
pub enum NexError {
    /// The device id is not one nexd accepts. Refused, not cleaned up: two
    /// ids cleaned to the same string would be one actor in the audit trail.
    BadDevice,
    /// What the device sent is not a request this bridge forwards.
    BadRequest(String),
    /// nexd is not running, or the connection to it failed.
    Unavailable(String),
    /// nexd is running and did not answer in time.
    Slow(Duration),
    /// nexd refused the declaration. Its JSON-RPC error, verbatim.
    Refused(Value),
    /// nexd answered with something that is not its protocol.
    Protocol(String),
}

impl std::fmt::Display for NexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NexError::BadDevice => write!(f, "this device's id is not one nex accepts"),
            NexError::BadRequest(m) => write!(f, "{m}"),
            NexError::Unavailable(m) => write!(f, "nex is not running on this machine ({m})"),
            NexError::Slow(t) => write!(f, "nex did not answer within {} s", t.as_secs_f32()),
            NexError::Refused(e) => write!(
                f,
                "nex refused this device: {}",
                e["message"].as_str().unwrap_or("no reason given")
            ),
            NexError::Protocol(m) => write!(f, "nex answered something unexpected: {m}"),
        }
    }
}

impl NexError {
    fn kind(&self) -> &'static str {
        match self {
            NexError::BadDevice | NexError::Refused(_) => "forbidden",
            NexError::BadRequest(_) => "bad_request",
            NexError::Unavailable(_) | NexError::Slow(_) => "unavailable",
            NexError::Protocol(_) => "internal",
        }
    }

    /// The reply line a device gets in place of an answer.
    pub fn reply(&self) -> Vec<u8> {
        json!({"reply": "error", "kind": self.kind(), "message": self.to_string()})
            .to_string()
            .into_bytes()
    }
}

/// nexd's own rule for an origin id: 1 to 128 of `[A-Za-z0-9-_.:]`.
fn valid_device(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}

/// The device's line, as a request rebuilt from its parts.
///
/// Rebuilt rather than forwarded so nothing but `method` and `params` reaches
/// nexd: not a second line, not an id of the device's choosing.
fn request(line: &Value) -> Result<(String, Value), NexError> {
    let r = &line["request"];
    let method = r["method"]
        .as_str()
        .ok_or_else(|| NexError::BadRequest("a nex request needs a method".into()))?;
    if method == "system.declare_origin" {
        return Err(NexError::BadRequest(
            "the origin is declared by this machine, not by the device".into(),
        ));
    }
    let params = match &r["params"] {
        Value::Null => json!({}),
        p @ Value::Object(_) => p.clone(),
        _ => return Err(NexError::BadRequest("nex params must be an object".into())),
    };
    Ok((method.to_string(), params))
}

/// A connection to nexd that has declared the device as its origin.
struct Declared {
    reader: BufReader<UnixStream>,
    deadline: Instant,
    limit: Duration,
}

impl Declared {
    fn open(socket: &Path, device_id: &str, limit: Duration) -> Result<Declared, NexError> {
        if !valid_device(device_id) {
            return Err(NexError::BadDevice);
        }
        let deadline = Instant::now() + limit;
        let stream =
            UnixStream::connect(socket).map_err(|e| NexError::Unavailable(e.to_string()))?;
        let mut d = Declared {
            reader: BufReader::new(stream),
            deadline,
            limit,
        };
        d.send(
            &json!({"jsonrpc": "2.0", "id": DECLARE_ID, "method": "system.declare_origin",
                       "params": {"kind": "device", "id": device_id}}),
        )?;
        let reply = d.response(&json!(DECLARE_ID))?;
        if let Some(e) = reply.get("error") {
            return Err(NexError::Refused(e.clone()));
        }
        if reply.get("result").is_none() {
            return Err(NexError::Protocol(
                "the declaration had neither a result nor an error".into(),
            ));
        }
        Ok(d)
    }

    /// What is left of the deadline, put on the socket for the next read or
    /// write; `Slow` once nothing is left.
    fn arm(&mut self) -> Result<(), NexError> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(NexError::Slow(self.limit));
        }
        let s = self.reader.get_ref();
        s.set_read_timeout(Some(left))
            .and_then(|_| s.set_write_timeout(Some(left)))
            .map_err(|e| NexError::Unavailable(e.to_string()))
    }

    fn io(&self, e: std::io::Error) -> NexError {
        match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                NexError::Slow(self.limit)
            }
            _ => NexError::Unavailable(e.to_string()),
        }
    }

    fn send(&mut self, v: &Value) -> Result<(), NexError> {
        let mut line = v.to_string().into_bytes();
        line.push(b'\n');
        self.arm()?;
        let s = self.reader.get_mut();
        let r = s.write_all(&line).and_then(|_| s.flush());
        r.map_err(|e| self.io(e))
    }

    /// The response with this id; notifications before it are skipped.
    fn response(&mut self, id: &Value) -> Result<Value, NexError> {
        loop {
            self.arm()?;
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => return Err(NexError::Unavailable("nexd closed the connection".into())),
                Ok(_) => {}
                Err(e) => return Err(self.io(e)),
            }
            let v: Value = serde_json::from_str(line.trim_end())
                .map_err(|e| NexError::Protocol(format!("not JSON: {e}")))?;
            if v.get("id") == Some(id) {
                return Ok(v);
            }
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, NexError> {
        self.send(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))?;
        self.response(&json!(1))
    }
}

/// The control-channel `nex` verb: one request, its whole JSON-RPC response.
pub fn control(socket: &Path, device_id: &str, line: &[u8]) -> Vec<u8> {
    control_within(socket, device_id, line, CALL_TIMEOUT)
}

fn control_within(socket: &Path, device_id: &str, line: &[u8], limit: Duration) -> Vec<u8> {
    let reply = match call(socket, device_id, line, limit) {
        Ok(response) => json!({"reply": "nex", "response": response})
            .to_string()
            .into_bytes(),
        Err(e) => return e.reply(),
    };
    if reply.len() > rime_remote_core::wire::MAX_PAYLOAD {
        return json!({"reply": "error", "kind": "too_large",
                      "message": format!("the answer is {} bytes, more than one control frame holds; \
                                          send the same request as a channel Open", reply.len())})
        .to_string()
        .into_bytes();
    }
    reply
}

fn call(socket: &Path, device_id: &str, line: &[u8], limit: Duration) -> Result<Value, NexError> {
    let v: Value =
        serde_json::from_slice(line).map_err(|e| NexError::BadRequest(format!("not JSON: {e}")))?;
    if v["cmd"] == "nex_events" {
        return Err(NexError::BadRequest(
            "nex_events is a feed: send it as a channel Open".into(),
        ));
    }
    let (method, params) = request(&v)?;
    let mut d = Declared::open(socket, device_id, limit)?;
    d.call(&method, params)
}

/// The origin the push watcher declares to nexd.
///
/// A device, like a phone: the watcher only reads what is waiting, and it
/// must not be a more powerful caller than the phones it serves. nexd files
/// it as `device:rime-remote-push`, which is also what its audit trail shows.
pub const PUSH_ORIGIN: &str = "rime-remote-push";

/// How long the push watcher waits for nexd. Short: it asks every few
/// seconds, and a slow answer is simply asked again.
pub const PUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// The ids of the approvals nexd is waiting on, or `None` when nexd did not
/// answer.
///
/// `None` is not "nothing is waiting": an empty set would make the watcher
/// forget every approval it has announced and announce them all again on the
/// next answer.
pub fn pending_approvals(socket: &Path) -> Option<std::collections::BTreeSet<String>> {
    pending_within(socket, PUSH_TIMEOUT)
}

fn pending_within(socket: &Path, limit: Duration) -> Option<std::collections::BTreeSet<String>> {
    let mut d = Declared::open(socket, PUSH_ORIGIN, limit).ok()?;
    let reply = d.call("tools.pending", json!({})).ok()?;
    let list = reply.get("result")?.as_array()?;
    Some(
        list.iter()
            .filter_map(|a| a["approval_id"].as_str().map(str::to_string))
            .collect(),
    )
}

/// An opened nex channel.
pub struct Opened {
    /// The nexd connection, to pump to the device.
    pub stream: UnixStream,
    /// The reply that says the channel is open.
    pub reply: Vec<u8>,
    /// Bytes already read past the reply, to send before the pump starts.
    pub buffered: Vec<u8>,
    /// A feed stays open until the device closes it; a one-shot answer is
    /// over once its bytes are sent and is not kept in the channel map.
    pub feed: bool,
}

/// A channel `Open` for `nex` or `nex_events`.
pub fn open_channel(
    socket: &Path,
    device_id: &str,
    request_line: &[u8],
) -> Result<Opened, NexError> {
    open_channel_within(socket, device_id, request_line, CALL_TIMEOUT)
}

fn open_channel_within(
    socket: &Path,
    device_id: &str,
    request_line: &[u8],
    limit: Duration,
) -> Result<Opened, NexError> {
    let v: Value = serde_json::from_slice(request_line)
        .map_err(|e| NexError::BadRequest(format!("not JSON: {e}")))?;
    let streaming = json!({"reply": "streaming"}).to_string().into_bytes();
    match v["cmd"].as_str() {
        Some("nex") => {
            let (method, params) = request(&v)?;
            let mut d = Declared::open(socket, device_id, limit)?;
            let response = d.call(&method, params)?;
            let mut bytes = response.to_string().into_bytes();
            bytes.push(b'\n');
            // Nothing more will come: the pump reads end-of-file and closes the
            // channel once the response has gone out.
            let stream = d.reader.into_inner();
            let _ = stream.shutdown(std::net::Shutdown::Both);
            Ok(Opened {
                stream,
                reply: streaming,
                buffered: bytes,
                feed: false,
            })
        }
        Some("nex_events") => {
            // Refused rather than read as 0: 0 means "everything", and a typo
            // would quietly send the phone the whole 10,000-event backlog.
            let after = match &v["after_gseq"] {
                Value::Null => 0,
                g => g.as_u64().ok_or_else(|| {
                    NexError::BadRequest("after_gseq must be a whole number".into())
                })?,
            };
            let tail = match &v["tail"] {
                Value::Null => false,
                t => t
                    .as_bool()
                    .ok_or_else(|| NexError::BadRequest("tail must be true or false".into()))?,
            };
            let mut d = Declared::open(socket, device_id, limit)?;
            d.send(&json!({"jsonrpc": "2.0", "id": 1, "method": "sessions.stream",
                           "params": {"session_id": "*", "after": after, "follow": true, "tail": tail}}))?;
            let buffered = d.reader.buffer().to_vec();
            let stream = d.reader.into_inner();
            // A feed is quiet for as long as nothing happens; only the request
            // above was on a clock.
            stream
                .set_read_timeout(None)
                .and_then(|_| stream.set_write_timeout(None))
                .map_err(|e| NexError::Unavailable(e.to_string()))?;
            Ok(Opened {
                stream,
                reply: streaming,
                buffered,
                feed: true,
            })
        }
        _ => Err(NexError::BadRequest("not a nex channel".into())),
    }
}

/// Whether a channel request is one of this module's.
pub fn is_channel(request_line: &[u8]) -> bool {
    serde_json::from_slice::<Value>(request_line)
        .ok()
        .and_then(|v| v["cmd"].as_str().map(|c| VERBS.contains(&c)))
        .unwrap_or(false)
}

/// Read what the pump would, for tests: everything until end-of-file.
#[cfg(test)]
fn drain(mut s: UnixStream) -> String {
    use std::io::Read;
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::{Arc, Mutex};

    /// A nexd that records every line it is sent and answers by a rule.
    struct Fake {
        socket: PathBuf,
        seen: Arc<Mutex<Vec<Value>>>,
        _dir: PathBuf,
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self._dir);
        }
    }

    fn fake(answer: impl Fn(&Value) -> Vec<Value> + Send + Sync + 'static) -> Fake {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rime-nex-fake-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("nexd.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let answer = Arc::new(answer);
        let s2 = Arc::clone(&seen);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { return };
                let seen = Arc::clone(&s2);
                let answer = Arc::clone(&answer);
                std::thread::spawn(move || {
                    let mut w = conn.try_clone().unwrap();
                    for line in BufReader::new(conn).lines() {
                        let Ok(line) = line else { return };
                        let v: Value = serde_json::from_str(&line).unwrap_or(json!({"raw": line}));
                        seen.lock().unwrap().push(v.clone());
                        for out in answer(&v) {
                            // A pause in the answer, so a test can trickle lines.
                            if let Some(ms) = out["__sleep_ms"].as_u64() {
                                std::thread::sleep(Duration::from_millis(ms));
                                continue;
                            }
                            if writeln!(w, "{out}").is_err() {
                                return;
                            }
                        }
                    }
                });
            }
        });
        Fake {
            socket,
            seen,
            _dir: dir,
        }
    }

    /// nexd's answers, shaped as the real daemon's (captured from nexd).
    fn nexd_like(v: &Value) -> Vec<Value> {
        let id = v["id"].clone();
        match v["method"].as_str() {
            Some("system.declare_origin") => {
                vec![json!({"jsonrpc": "2.0", "id": id, "result": {"origin": v["params"]}})]
            }
            Some("system.shutdown") => {
                vec![json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32002,
                "message": "system.shutdown is not available from a paired device; use this machine"}})]
            }
            Some("sessions.stream") => vec![
                json!({"jsonrpc": "2.0", "id": id, "result": {"events": [], "last_gseq": 7}}),
                json!({"jsonrpc": "2.0", "method": "event", "params": {"gseq": 8, "kind": "turn.started"}}),
            ],
            Some(m) => vec![
                json!({"jsonrpc": "2.0", "id": id, "result": {"method": m, "params": v["params"]}}),
            ],
            None => vec![],
        }
    }

    fn line(v: Value) -> Vec<u8> {
        v.to_string().into_bytes()
    }

    fn reply(b: Vec<u8>) -> Value {
        serde_json::from_slice(&b).unwrap()
    }

    #[test]
    fn the_declaration_comes_first_and_names_the_authenticated_device() {
        let f = fake(nexd_like);
        let r = reply(control(
            &f.socket,
            "AbC-12_x",
            &line(json!({"cmd": "nex",
            "request": {"method": "sessions.list", "params": {}}})),
        ));
        assert_eq!(r["reply"], "nex", "{r}");
        assert_eq!(r["response"]["result"]["method"], "sessions.list", "{r}");
        let seen = f.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert_eq!(seen[0]["method"], "system.declare_origin");
        assert_eq!(
            seen[0]["params"],
            json!({"kind": "device", "id": "AbC-12_x"})
        );
        assert_eq!(seen[1]["method"], "sessions.list");
    }

    #[test]
    fn a_refused_declaration_never_sends_the_request() {
        let f = fake(|v| {
            vec![
                json!({"jsonrpc": "2.0", "id": v["id"], "error": {"code": -32002, "message": "no"}}),
            ]
        });
        let r = reply(control(
            &f.socket,
            "phone",
            &line(json!({"cmd": "nex",
            "request": {"method": "turns.submit", "params": {"session_id": "s", "text": "rm -rf"}}})),
        ));
        assert_eq!(r["reply"], "error", "{r}");
        assert_eq!(r["kind"], "forbidden", "{r}");
        let seen = f.seen.lock().unwrap().clone();
        assert_eq!(
            seen.len(),
            1,
            "only the declaration may reach nexd: {seen:?}"
        );
        assert_eq!(seen[0]["method"], "system.declare_origin");
    }

    #[test]
    fn an_unanswered_declaration_never_sends_the_request() {
        // Answers the declaration with a notification and nothing else, so
        // the bridge must wait for its own reply and give up, not proceed.
        let f = fake(|v| match v["method"].as_str() {
            Some("system.declare_origin") => {
                vec![json!({"jsonrpc": "2.0", "method": "event", "params": {}})]
            }
            _ => vec![],
        });
        let t = Instant::now();
        let r = reply(control_within(
            &f.socket,
            "phone",
            &line(json!({"cmd": "nex", "request": {"method": "turns.submit"}})),
            Duration::from_millis(200),
        ));
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        assert_eq!(r["kind"], "unavailable", "{r}");
        assert!(
            r["message"].as_str().unwrap().contains("did not answer"),
            "{r}"
        );
        let seen = f.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0]["method"], "system.declare_origin");
    }

    #[test]
    fn the_deadline_covers_the_whole_call_not_each_read() {
        // A nexd that trickles notifications, each well inside the limit,
        // and never answers: a clock per read would wait forever.
        let f = fake(|v| match v["method"].as_str() {
            Some("system.declare_origin") => {
                vec![json!({"jsonrpc": "2.0", "id": v["id"], "result": {}})]
            }
            _ => (0..40)
                .flat_map(|_| {
                    [
                        json!({"__sleep_ms": 50}),
                        json!({"jsonrpc": "2.0", "method": "event", "params": {}}),
                    ]
                })
                .collect(),
        });
        let t = Instant::now();
        let r = reply(control_within(
            &f.socket,
            "phone",
            &line(json!({"cmd": "nex", "request": {"method": "sessions.list"}})),
            Duration::from_millis(300),
        ));
        assert_eq!(r["kind"], "unavailable", "{r}");
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
    }

    #[test]
    fn a_device_id_nexd_would_refuse_is_refused_here_without_connecting() {
        let f = fake(nexd_like);
        for bad in ["", "has space", "slash/no", &"x".repeat(129), "ünï"] {
            let r = reply(control(
                &f.socket,
                bad,
                &line(json!({"cmd": "nex",
                "request": {"method": "sessions.list"}})),
            ));
            assert_eq!(r["kind"], "forbidden", "{bad:?}: {r}");
        }
        assert!(f.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn the_device_cannot_declare_or_smuggle_a_second_line() {
        let f = fake(nexd_like);
        let r = reply(control(
            &f.socket,
            "phone",
            &line(json!({"cmd": "nex",
            "request": {"method": "system.declare_origin", "params": {"kind": "device", "id": "other"}}})),
        ));
        assert_eq!(r["kind"], "bad_request", "{r}");
        assert!(f.seen.lock().unwrap().is_empty());
        // A newline inside a value travels as JSON, so nexd sees one line.
        let r = reply(control(
            &f.socket,
            "phone",
            &line(json!({"cmd": "nex",
            "request": {"method": "sessions.list", "params": {"x": "a\n{\"method\":\"system.shutdown\"}"}}})),
        ));
        assert_eq!(r["reply"], "nex", "{r}");
        let seen = f.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert!(
            seen.iter().all(|v| v["method"] != "system.shutdown"),
            "{seen:?}"
        );
    }

    #[test]
    fn the_request_id_is_the_devices_and_is_never_minted_here() {
        let f = fake(nexd_like);
        let r = reply(control(
            &f.socket,
            "phone",
            &line(json!({"cmd": "nex", "request": {"method": "turns.submit",
            "params": {"session_id": "s", "text": "go", "request_id": "phone-7"}}})),
        ));
        assert_eq!(
            r["response"]["result"]["params"]["request_id"], "phone-7",
            "{r}"
        );
        let r = reply(control(
            &f.socket,
            "phone",
            &line(json!({"cmd": "nex", "request": {"method": "turns.submit",
            "params": {"session_id": "s", "text": "go"}}})),
        ));
        assert!(
            r["response"]["result"]["params"]
                .get("request_id")
                .is_none(),
            "{r}"
        );
    }

    #[test]
    fn nexds_refusal_stays_inside_the_response_and_unreachable_is_different() {
        let f = fake(nexd_like);
        let r = reply(control(
            &f.socket,
            "phone",
            &line(json!({"cmd": "nex",
            "request": {"method": "system.shutdown"}})),
        ));
        assert_eq!(r["reply"], "nex", "{r}");
        assert_eq!(r["response"]["error"]["code"], -32002, "{r}");
        let gone = reply(control(
            Path::new("/nonexistent/nexd.sock"),
            "phone",
            &line(json!({"cmd": "nex",
            "request": {"method": "sessions.list"}})),
        ));
        assert_eq!(gone["kind"], "unavailable", "{gone}");
        assert!(!available(Path::new("/nonexistent/nexd.sock")));
        assert!(available(&f.socket));
    }

    #[test]
    fn an_answer_past_one_control_frame_says_to_use_a_channel() {
        let big = "x".repeat(rime_remote_core::wire::MAX_PAYLOAD + 10);
        let f = fake(move |v| match v["method"].as_str() {
            Some("system.declare_origin") => {
                vec![json!({"jsonrpc": "2.0", "id": v["id"], "result": {}})]
            }
            _ => vec![json!({"jsonrpc": "2.0", "id": v["id"], "result": {"blob": big}})],
        });
        let req = line(
            json!({"cmd": "nex", "request": {"method": "sessions.stream", "params": {"session_id": "s", "follow": false}}}),
        );
        let r = reply(control(&f.socket, "phone", &req));
        assert_eq!(r["kind"], "too_large", "{r}");
        let Opened {
            stream,
            reply: open,
            buffered,
            ..
        } = open_channel(&f.socket, "phone", &req).unwrap();
        assert_eq!(reply(open)["reply"], "streaming");
        let mut all = String::from_utf8(buffered).unwrap();
        all.push_str(&drain(stream));
        let v: Value = serde_json::from_str(all.trim_end()).unwrap();
        assert_eq!(
            v["result"]["blob"].as_str().unwrap().len(),
            rime_remote_core::wire::MAX_PAYLOAD + 10
        );
    }

    #[test]
    fn a_reply_of_exactly_one_frame_goes_on_the_control_channel_and_one_byte_more_does_not() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let size = Arc::new(AtomicUsize::new(0));
        let s2 = Arc::clone(&size);
        let f = fake(move |v| match v["method"].as_str() {
            Some("system.declare_origin") => {
                vec![json!({"jsonrpc": "2.0", "id": v["id"], "result": {}})]
            }
            _ => vec![json!({"jsonrpc": "2.0", "id": v["id"],
                             "result": {"blob": "x".repeat(s2.load(Ordering::SeqCst))}})],
        });
        let req = line(json!({"cmd": "nex", "request": {"method": "sessions.list"}}));
        let fixed = control(&f.socket, "phone", &req).len();
        size.store(
            rime_remote_core::wire::MAX_PAYLOAD - fixed,
            Ordering::SeqCst,
        );
        let exact = control(&f.socket, "phone", &req);
        assert_eq!(exact.len(), rime_remote_core::wire::MAX_PAYLOAD);
        assert_eq!(reply(exact.clone())["reply"], "nex");
        // The same check the frame loop's `send` makes, which drops what fails it.
        rime_remote_core::wire::Frame::Control(exact)
            .encode()
            .expect("a reply this module lets through always fits a control frame");
        size.fetch_add(1, Ordering::SeqCst);
        let over = control(&f.socket, "phone", &req);
        assert_eq!(reply(over.clone())["kind"], "too_large");
        rime_remote_core::wire::Frame::Control(over)
            .encode()
            .expect("the refusal fits too");
    }

    #[test]
    fn a_feed_position_that_is_not_a_number_is_refused_not_read_as_zero() {
        let f = fake(nexd_like);
        for bad in [json!("5"), json!(-1), json!(1.5)] {
            let r = open_channel(
                &f.socket,
                "phone",
                &line(json!({"cmd": "nex_events", "after_gseq": bad})),
            );
            assert!(matches!(r, Err(NexError::BadRequest(_))), "{bad}");
        }
        assert!(f.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn the_event_feed_is_the_global_stream_after_the_devices_position() {
        let f = fake(nexd_like);
        let Opened {
            stream,
            reply: open,
            buffered,
            ..
        } = open_channel(
            &f.socket,
            "phone",
            &line(json!({"cmd": "nex_events", "after_gseq": 5})),
        )
        .unwrap();
        assert_eq!(reply(open)["reply"], "streaming");
        let mut r = BufReader::new(stream);
        let mut text = String::from_utf8(buffered).unwrap();
        while text.lines().count() < 2 {
            let mut l = String::new();
            r.read_line(&mut l).unwrap();
            text.push_str(&l);
        }
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines[0]["result"]["last_gseq"], 7);
        assert_eq!(lines[1]["params"]["gseq"], 8);
        let seen = f.seen.lock().unwrap().clone();
        assert_eq!(seen[0]["method"], "system.declare_origin");
        assert_eq!(
            seen[1]["params"],
            json!({"session_id": "*", "after": 5, "follow": true, "tail": false})
        );
    }

    #[test]
    fn only_the_two_nex_words_are_nex_channels() {
        assert!(is_channel(br#"{"cmd":"nex_events"}"#));
        assert!(is_channel(br#"{"cmd":"nex","request":{}}"#));
        assert!(!is_channel(br#"{"cmd":"attach","id":1}"#));
        assert!(!is_channel(b"nex"));
    }

    /// The bridge against a REAL nexd, not the fake above. CI cannot run it (nex is not
    /// public yet), so it is ignored unless `RIME_NEXD_BIN` names a nexd binary:
    ///
    /// ```text
    /// RIME_NEXD_BIN=/path/to/nexd cargo test -p rime-remoted real_nexd -- --ignored
    /// ```
    #[test]
    #[ignore = "needs RIME_NEXD_BIN"]
    fn real_nexd_narrows_the_device_dedupes_retries_and_feeds_events() {
        let Some(bin) = std::env::var_os("RIME_NEXD_BIN") else {
            panic!("set RIME_NEXD_BIN to a nexd binary");
        };
        let dir = std::env::temp_dir().join(format!("rime-nex-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ws = dir.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(
            dir.join("config.json"),
            json!({"providers": [{"id": "scripted", "kind": "scripted",
                                  "script": dir.join("script.json")}], "policy": {}})
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.join("script.json"),
            json!({"steps": [{"text": "Done."}]}).to_string(),
        )
        .unwrap();
        let socket = dir.join("nexd.sock");
        let mut child = std::process::Command::new(bin)
            .arg("--data-dir")
            .arg(dir.join("data"))
            .arg("--socket")
            .arg(&socket)
            .arg("--config")
            .arg(dir.join("config.json"))
            .stdout(std::process::Stdio::null())
            .stderr(std::fs::File::create(dir.join("nexd.log")).unwrap())
            .spawn()
            .unwrap();
        let t = std::time::Instant::now();
        while !available(&socket) {
            assert!(t.elapsed() < Duration::from_secs(15), "nexd did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
        // The owner, at this machine: an undeclared connection makes the session.
        let local = UnixStream::connect(&socket).unwrap();
        let mut lr = BufReader::new(local.try_clone().unwrap());
        let mut lw = local;
        writeln!(
            lw,
            "{}",
            json!({"jsonrpc": "2.0", "id": 1, "method": "sessions.create",
            "params": {"root": ws, "mode": "build", "title": "owner", "request_id": "owner-1"}})
        )
        .unwrap();
        let mut l = String::new();
        lr.read_line(&mut l).unwrap();
        let sid = serde_json::from_str::<Value>(&l).unwrap()["result"]["session_id"]
            .as_str()
            .expect(&l)
            .to_string();

        let dev = "AbCdEfGh12345678";
        let ask = |method: &str, params: Value| -> Value {
            reply(control(
                &socket,
                dev,
                &line(json!({"cmd": "nex",
                "request": {"method": method, "params": params}})),
            ))
        };
        let r = ask("sessions.list", json!({}));
        assert!(r.to_string().contains(&sid), "{r}");
        let r = ask("system.shutdown", json!({"request_id": "dev-x"}));
        assert_eq!(r["response"]["error"]["code"], -32002, "{r}");
        let r = ask(
            "sessions.create",
            json!({"root": "/", "mode": "build", "title": "x", "request_id": "dev-y"}),
        );
        assert_eq!(r["response"]["error"]["code"], -32002, "{r}");
        // A retry with the same request_id is answered from nexd's record.
        let submit = json!({"session_id": sid, "text": "go", "request_id": "dev-turn-1"});
        let a = ask("turns.submit", submit.clone());
        let b = ask("turns.submit", submit);
        assert!(a["response"]["result"].is_object(), "{a}");
        assert_eq!(
            a["response"]["result"], b["response"]["result"],
            "{a} vs {b}"
        );

        // The feed: the backlog, carrying the device as the actor of its turn.
        let Opened {
            stream,
            reply: open,
            buffered,
            ..
        } = open_channel(
            &socket,
            dev,
            &line(json!({"cmd": "nex_events", "after_gseq": 0})),
        )
        .unwrap();
        assert_eq!(reply(open)["reply"], "streaming");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut text = String::from_utf8(buffered).unwrap();
        let mut r = BufReader::new(stream);
        while !text.contains('\n') {
            r.read_line(&mut text).unwrap();
        }
        let backlog: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        let events = backlog["result"]["events"].as_array().expect("events");
        let submitted: Vec<&Value> = events
            .iter()
            .filter(|e| e["event"]["actor"]["id"] == format!("device:{dev}"))
            .collect();
        assert!(
            !submitted.is_empty(),
            "no event names the device: {backlog}"
        );
        // Then live notifications until the turn has finished, so "ran once" is
        // measured over the whole turn and not over however much had happened yet.
        let mut all: Vec<Value> = events.iter().map(|e| e["event"].clone()).collect();
        let ours = |e: &Value, k: &str| e["stream_id"] == sid.as_str() && e["kind"] == k;
        while !all
            .iter()
            .any(|e| ours(e, "turn.completed") || ours(e, "turn.failed"))
        {
            let mut l = String::new();
            r.read_line(&mut l)
                .expect("the feed went quiet before the turn finished");
            let n: Value = serde_json::from_str(&l).unwrap();
            assert_eq!(n["method"], "event", "{n}");
            all.push(n["params"].clone());
        }
        let started = all.iter().filter(|e| ours(e, "turn.started")).count();
        assert_eq!(
            started, 1,
            "the retried turn should run exactly once: {all:?}"
        );
        assert!(all.iter().any(|e| ours(e, "turn.completed")), "{all:?}");
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_push_watcher_reads_what_is_waiting_as_a_device_and_nothing_else() {
        let f = fake(|v: &Value| {
            let id = v["id"].clone();
            match v["method"].as_str() {
                Some("system.declare_origin") => {
                    vec![json!({"jsonrpc": "2.0", "id": id, "result": {"origin": v["params"]}})]
                }
                // tools.pending, shaped as nexd answers it.
                Some("tools.pending") => vec![json!({"jsonrpc": "2.0", "id": id, "result": [
                    {"approval_id": "ap-1", "session_id": "s-1", "tool": "run_command",
                     "summary": "run `bash test.sh`", "scopes": ["process.exec"]},
                    {"approval_id": "ap-2", "session_id": "s-2", "tool": "edit_file"},
                ]})],
                _ => vec![json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "no"}})],
            }
        });
        let got = pending_approvals(&f.socket).expect("an answer");
        assert_eq!(got.into_iter().collect::<Vec<_>>(), vec!["ap-1", "ap-2"]);
        let seen = f.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert_eq!(seen[0]["method"], "system.declare_origin");
        assert_eq!(seen[0]["params"], json!({"kind": "device", "id": PUSH_ORIGIN}));
        assert_eq!(seen[1]["method"], "tools.pending");
    }

    #[test]
    fn a_refusal_or_no_nexd_is_no_answer_not_an_empty_list() {
        let gone = std::env::temp_dir().join("rime-nex-no-such-socket.sock");
        assert_eq!(pending_approvals(&gone), None);
        let refuses = fake(|v: &Value| {
            vec![json!({"jsonrpc": "2.0", "id": v["id"], "error": {"code": -32002, "message": "no"}})]
        });
        assert_eq!(pending_approvals(&refuses.socket), None);
        let errs = fake(|v: &Value| match v["method"].as_str() {
            Some("system.declare_origin") => {
                vec![json!({"jsonrpc": "2.0", "id": v["id"], "result": {"origin": v["params"]}})]
            }
            _ => vec![json!({"jsonrpc": "2.0", "id": v["id"], "error": {"code": -32603, "message": "x"}})],
        });
        assert_eq!(pending_approvals(&errs.socket), None);
    }

    #[test]
    fn a_nexd_that_never_answers_costs_the_push_loop_one_short_wait() {
        let mute = fake(|_: &Value| vec![]);
        let t = Instant::now();
        assert_eq!(pending_within(&mute.socket, Duration::from_millis(300)), None);
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    }

    /// The push watcher's read against a REAL nexd: a turn that asks before it
    /// runs a command shows up as waiting, and goes when the turn is stopped.
    ///
    /// ```text
    /// RIME_NEXD_BIN=/path/to/nexd cargo test -p rime-remoted real_nexd -- --ignored
    /// ```
    #[test]
    #[ignore = "needs RIME_NEXD_BIN"]
    fn real_nexd_shows_the_push_watcher_an_approval_until_the_turn_ends() {
        let Some(bin) = std::env::var_os("RIME_NEXD_BIN") else {
            panic!("set RIME_NEXD_BIN to a nexd binary");
        };
        let dir = std::env::temp_dir().join(format!("rime-nex-push-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ws = dir.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(
            dir.join("config.json"),
            json!({"providers": [{"id": "scripted", "kind": "scripted",
                                  "script": dir.join("script.json")}], "policy": {}})
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.join("script.json"),
            json!({"steps": [
                {"text": "Running it.", "tool_calls": [{"name": "run_command", "args": {"command": "true"}}]},
                {"text": "Done."}]})
            .to_string(),
        )
        .unwrap();
        let socket = dir.join("nexd.sock");
        let mut child = std::process::Command::new(bin)
            .arg("--data-dir")
            .arg(dir.join("data"))
            .arg("--socket")
            .arg(&socket)
            .arg("--config")
            .arg(dir.join("config.json"))
            .stdout(std::process::Stdio::null())
            .stderr(std::fs::File::create(dir.join("nexd.log")).unwrap())
            .spawn()
            .unwrap();
        let t = Instant::now();
        while !available(&socket) {
            assert!(t.elapsed() < Duration::from_secs(15), "nexd did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
        let local = UnixStream::connect(&socket).unwrap();
        let mut lr = BufReader::new(local.try_clone().unwrap());
        let mut lw = local;
        let mut owner = |n: u32, method: &str, params: Value| -> Value {
            writeln!(lw, "{}", json!({"jsonrpc": "2.0", "id": n, "method": method, "params": params}))
                .unwrap();
            loop {
                let mut l = String::new();
                lr.read_line(&mut l).unwrap();
                let v: Value = serde_json::from_str(&l).unwrap();
                if v["id"] == n {
                    return v;
                }
            }
        };
        let r = owner(1, "sessions.create",
            json!({"root": ws, "mode": "build", "title": "owner", "request_id": "own-1"}));
        let sid = r["result"]["session_id"].as_str().expect(&r.to_string()).to_string();

        let mut w = crate::push::Watcher::new();
        let observe = |w: &mut crate::push::Watcher| {
            w.observe(&crate::push::Observation { nex: pending_approvals(&socket), ..Default::default() })
        };
        assert!(observe(&mut w).is_empty());
        assert_eq!(pending_approvals(&socket), Some(Default::default()));

        let r = owner(2, "turns.submit",
            json!({"session_id": sid, "text": "go", "request_id": "own-2"}));
        assert!(r.get("result").is_some(), "{r}");
        let t = Instant::now();
        let waiting = loop {
            let p = pending_approvals(&socket).expect("nexd answers");
            if !p.is_empty() {
                break p;
            }
            assert!(t.elapsed() < Duration::from_secs(10), "no approval appeared");
            std::thread::sleep(Duration::from_millis(50));
        };
        let r = owner(3, "tools.pending", json!({}));
        let theirs: std::collections::BTreeSet<String> = r["result"].as_array().unwrap().iter()
            .map(|a| a["approval_id"].as_str().unwrap().to_string()).collect();
        assert_eq!(waiting, theirs, "the watcher sees what the owner sees");
        let raised = observe(&mut w);
        assert_eq!(raised.len(), 1, "{raised:?}");
        assert_eq!(raised[0].kind, rime_remote_core::push::Kind::NexApproval);

        let r = owner(4, "turns.cancel", json!({"session_id": sid, "request_id": "own-4"}));
        assert!(r.get("result").is_some(), "{r}");
        let t = Instant::now();
        while pending_approvals(&socket) != Some(Default::default()) {
            assert!(t.elapsed() < Duration::from_secs(10), "the approval outlived its turn");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(observe(&mut w).is_empty(), "an answer is not news");
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
