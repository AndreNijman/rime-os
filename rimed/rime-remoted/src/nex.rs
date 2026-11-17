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
use std::time::Duration;

use serde_json::{json, Value};

/// How long one request may take, from connecting to its reply.
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
    /// nexd is not running, or stopped answering.
    Unavailable(String),
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
            NexError::Unavailable(_) => "unavailable",
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
}

impl Declared {
    fn open(socket: &Path, device_id: &str) -> Result<Declared, NexError> {
        if !valid_device(device_id) {
            return Err(NexError::BadDevice);
        }
        let stream =
            UnixStream::connect(socket).map_err(|e| NexError::Unavailable(e.to_string()))?;
        stream
            .set_read_timeout(Some(CALL_TIMEOUT))
            .and_then(|_| stream.set_write_timeout(Some(CALL_TIMEOUT)))
            .map_err(|e| NexError::Unavailable(e.to_string()))?;
        let mut d = Declared {
            reader: BufReader::new(stream),
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

    fn send(&mut self, v: &Value) -> Result<(), NexError> {
        let mut line = v.to_string().into_bytes();
        line.push(b'\n');
        let s = self.reader.get_mut();
        s.write_all(&line)
            .and_then(|_| s.flush())
            .map_err(|e| NexError::Unavailable(e.to_string()))
    }

    /// The response with this id; notifications before it are skipped.
    fn response(&mut self, id: &Value) -> Result<Value, NexError> {
        loop {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => return Err(NexError::Unavailable("nexd closed the connection".into())),
                Ok(_) => {}
                Err(e) => return Err(NexError::Unavailable(e.to_string())),
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
    let reply = match call(socket, device_id, line) {
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

fn call(socket: &Path, device_id: &str, line: &[u8]) -> Result<Value, NexError> {
    let v: Value =
        serde_json::from_slice(line).map_err(|e| NexError::BadRequest(format!("not JSON: {e}")))?;
    if v["cmd"] == "nex_events" {
        return Err(NexError::BadRequest(
            "nex_events is a feed: send it as a channel Open".into(),
        ));
    }
    let (method, params) = request(&v)?;
    let mut d = Declared::open(socket, device_id)?;
    d.call(&method, params)
}

/// A channel `Open` for `nex` or `nex_events`: the nexd connection to pump to
/// the device, the reply that says it is open, and bytes already read past it.
pub fn open_channel(
    socket: &Path,
    device_id: &str,
    request_line: &[u8],
) -> Result<(UnixStream, Vec<u8>, Vec<u8>), NexError> {
    let v: Value = serde_json::from_slice(request_line)
        .map_err(|e| NexError::BadRequest(format!("not JSON: {e}")))?;
    let streaming = json!({"reply": "streaming"}).to_string().into_bytes();
    match v["cmd"].as_str() {
        Some("nex") => {
            let (method, params) = request(&v)?;
            let mut d = Declared::open(socket, device_id)?;
            let response = d.call(&method, params)?;
            let mut bytes = response.to_string().into_bytes();
            bytes.push(b'\n');
            // Nothing more will come: the pump reads end-of-file and closes the
            // channel once the response has gone out.
            let stream = d.reader.into_inner();
            let _ = stream.shutdown(std::net::Shutdown::Both);
            Ok((stream, streaming, bytes))
        }
        Some("nex_events") => {
            let after = v["after_gseq"].as_u64().unwrap_or(0);
            let tail = v["tail"].as_bool().unwrap_or(false);
            let mut d = Declared::open(socket, device_id)?;
            d.send(&json!({"jsonrpc": "2.0", "id": 1, "method": "sessions.stream",
                           "params": {"session_id": "*", "after": after, "follow": true, "tail": tail}}))?;
            let buffered = d.reader.buffer().to_vec();
            let stream = d.reader.into_inner();
            // A feed is quiet for as long as nothing happens; only the request
            // above was on a clock.
            stream
                .set_read_timeout(None)
                .map_err(|e| NexError::Unavailable(e.to_string()))?;
            Ok((stream, streaming, buffered))
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
        // Answers the declaration with a different id and nothing else, so
        // the bridge must wait for its own and give up — not proceed.
        let f = fake(|v| match v["method"].as_str() {
            Some("system.declare_origin") => {
                vec![json!({"jsonrpc": "2.0", "method": "event", "params": {}})]
            }
            _ => vec![],
        });
        // Close the connection by hand: the fake keeps it open, so this is
        // proved with a short-lived socket in place of the 30 s clock.
        let s = f.socket.clone();
        let handle = std::thread::spawn(move || {
            control(
                &s,
                "phone",
                &line(json!({"cmd": "nex", "request": {"method": "turns.submit"}})),
            )
        });
        std::thread::sleep(Duration::from_millis(300));
        let seen = f.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0]["method"], "system.declare_origin");
        drop(f); // removes the socket dir; the call ends on its timeout
        drop(handle);
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
        let (stream, open, buffered) = open_channel(&f.socket, "phone", &req).unwrap();
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
    fn the_event_feed_is_the_global_stream_after_the_devices_position() {
        let f = fake(nexd_like);
        let (stream, open, buffered) = open_channel(
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
        let (stream, open, buffered) = open_channel(
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
}
