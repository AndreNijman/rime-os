//! A phone's SMS threads, read and sent from this machine (contract §2.11).
//!
//! Nothing here touches a disk. A request's answer is held in memory only
//! while the person who asked waits for it, and is dropped the moment the
//! phone's connection ends or the device is revoked, whether or not the
//! answer was complete. There is never more than one request per phone.
//!
//! The phone is trusted with nothing: every reply is checked against the
//! request it answers (same device, same connection, the kind that was
//! asked), and every field is cleaned and cut again here, whatever the app
//! did. No body, number or name is ever written to a log or an error.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::phone::Events;

/// The control lines a phone sends about messages.
pub const VERBS: [&str; 3] = ["messages_reply", "messages_changed", "messages_sent"];

/// What `remote_hello` offers: this machine understands the verbs above.
pub const FEATURES: [&str; 1] = ["phone_messages"];

/// The event that asks the phone for threads or one thread's messages.
pub const REQUEST_EVENT: &str = "messages_request";

/// The event that asks the phone to send one SMS.
pub const SEND_EVENT: &str = "messages_send";

/// The most threads, or messages, one request asks for.
pub const MAX_LIMIT: usize = 50;

/// What a read asks for when the caller named no limit.
pub const DEFAULT_LIMIT: usize = 20;

/// The longest body kept, in characters, and the longest text sent.
pub const MAX_BODY: usize = 2000;

/// The most lines a body keeps.
pub const MAX_BODY_LINES: usize = 40;

/// The longest snippet kept, in characters.
pub const MAX_SNIPPET: usize = 160;

/// The longest contact name kept, in characters.
pub const MAX_NAME: usize = 60;

/// The longest address (phone number or sender name) kept, in characters.
pub const MAX_ADDRESS: usize = 40;

/// The most addresses one thread carries.
pub const MAX_ADDRESSES: usize = 10;

/// The most reply lines one request accepts, and the most bytes in all.
pub const MAX_CHUNKS: usize = 64;
pub const MAX_TOTAL_BYTES: usize = 512 * 1024;

/// How long a read waits for the phone's whole answer: one deadline, not one
/// per chunk.
pub const READ_WAIT: Duration = Duration::from_secs(10);

/// How long a send waits for the phone's `messages_sent`. The phone waits up
/// to 25 s for the radio's result, so this is longer.
pub const SEND_WAIT: Duration = Duration::from_secs(35);

/// The reason words a phone may refuse with. Anything else reads as `failed`.
pub const PHONE_REFUSALS: [&str; 10] = [
    "off",
    "no_permission",
    "locked",
    "busy",
    "no_thread",
    "group",
    "too_long",
    "failed",
    "timeout",
    "no_service",
];

/// One thread, as the computer shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub id: u64,
    pub addresses: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub snippet: String,
    pub date_ms: u64,
    pub unread: u32,
    pub count: u32,
}

/// One message in a thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: u64,
    pub date_ms: u64,
    pub from_me: bool,
    pub body: String,
    /// `received`, `sent`, `delivered`, `sending` or `failed`.
    pub status: String,
}

/// The status words a message may carry.
pub const STATUSES: [&str; 5] = ["received", "sent", "delivered", "sending", "failed"];

/// What a request asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    /// The newest threads, before `before` when set.
    Threads { before: Option<u64>, limit: usize },
    /// One thread's newest messages, before `before` when set.
    Thread { id: u64, before: Option<u64>, limit: usize },
    /// One SMS, to a number or into a thread.
    Send { to: Option<String>, thread: Option<u64>, text: String },
}

/// A complete answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Threads(Vec<Thread>),
    Messages { thread: u64, messages: Vec<Message> },
    Sent,
}

/// Why a request got no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No events channel is open for the phone.
    NotConnected,
    /// The phone's channel belongs to a companion (locked) session, and a
    /// send needs the app unlocked.
    Locked,
    /// Another request for this phone is waiting.
    Busy,
    /// The phone's connection ended before the answer was complete.
    Disconnected,
    /// The device was revoked while the request waited.
    Revoked,
    /// No complete answer within the deadline.
    Timeout,
    /// The phone answered with something that is not an answer to this.
    BadReply(String),
    /// The phone refused, with one of [`PHONE_REFUSALS`].
    Phone(String),
}

impl Refusal {
    /// The sentence the person reads. Never carries a body or a number.
    pub fn sentence(&self, name: &str) -> String {
        match self {
            Refusal::NotConnected => format!("{name} is not connected. Open Rime Remote on it, then try again"),
            Refusal::Locked => format!(
                "{name} is locked: Rime Remote sends a message only while it is unlocked on the phone"
            ),
            Refusal::Busy => format!("{name} is already answering another messages request; try again in a moment"),
            Refusal::Disconnected => format!("{name} disconnected before it finished answering"),
            Refusal::Revoked => format!("{name} was revoked"),
            Refusal::Timeout => format!("{name} did not answer in time"),
            Refusal::BadReply(why) => format!("{name} answered with something this computer does not accept: {why}"),
            Refusal::Phone(word) => match word.as_str() {
                "off" => format!("Messages on your computer is turned off on {name} (Rime Remote → Messages)"),
                "no_permission" => format!(
                    "Rime Remote on {name} is not allowed to read or send SMS. Allow it in Rime Remote → Messages"
                ),
                "locked" => format!("{name} is locked: unlock Rime Remote on it to send"),
                "busy" => format!("{name} is busy with another message; try again in a moment"),
                "no_thread" => format!("{name} has no such conversation"),
                "group" => format!("that is a group conversation; {name} sends to one number at a time"),
                "too_long" => format!("that text is too long for {name} to send"),
                "timeout" => format!("{name} did not hear back from its mobile network in time"),
                "no_service" => format!("{name} has no mobile service right now"),
                _ => format!("{name} could not send the message"),
            },
        }
    }
}

/// The kind of answer a request waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Threads,
    Thread(u64),
    Send,
}

/// What has come in for one request so far.
struct Pending {
    device: String,
    connection: u64,
    kind: Kind,
    limit: usize,
    threads: Vec<Thread>,
    messages: Vec<Message>,
    chunks: usize,
    bytes: usize,
    outcome: Option<Result<Answer, Refusal>>,
}

impl Pending {
    fn finish(&mut self, outcome: Result<Answer, Refusal>) {
        if self.outcome.is_none() {
            // What was gathered goes now, not when the waiter wakes: on a
            // disconnect or a revoke, nothing of it outlives the cause.
            if outcome.is_err() {
                self.threads = Vec::new();
                self.messages = Vec::new();
            }
            self.outcome = Some(outcome);
        }
    }
}

#[derive(Default)]
struct Book {
    pending: HashMap<u64, Pending>,
    /// When each phone last said its messages changed (unix ms).
    changed: HashMap<String, u64>,
}

/// The requests waiting on phones, and when each phone last said something
/// changed.
#[derive(Default)]
pub struct Messages {
    next: AtomicU64,
    book: Mutex<Book>,
    wake: Condvar,
}

impl Messages {
    fn book(&self) -> std::sync::MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Ask `device` for `want` over its events channel and wait up to `limit`
    /// for the whole answer.
    pub fn ask(&self, events: &Events, device: &str, want: Want, limit: Duration) -> Result<Answer, Refusal> {
        let Some((connection, full)) = events.holder(device) else {
            return Err(Refusal::NotConnected);
        };
        if matches!(want, Want::Send { .. }) && !full {
            // Answered here and at once: the phone could not deliver its own
            // refusal, since a companion session may not say `messages_sent`.
            return Err(Refusal::Locked);
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let (kind, cap, line) = match &want {
            Want::Threads { before, limit } => {
                let limit = (*limit).clamp(1, MAX_LIMIT);
                let mut line = json!({"event": REQUEST_EVENT, "id": id, "limit": limit});
                if let Some(b) = before {
                    line["before"] = json!(b);
                }
                (Kind::Threads, limit, line)
            }
            Want::Thread { id: thread, before, limit } => {
                let limit = (*limit).clamp(1, MAX_LIMIT);
                let mut line = json!({"event": REQUEST_EVENT, "id": id, "thread": thread, "limit": limit});
                if let Some(b) = before {
                    line["before"] = json!(b);
                }
                (Kind::Thread(*thread), limit, line)
            }
            Want::Send { to, thread, text } => {
                let mut line = json!({"event": SEND_EVENT, "id": id, "text": text});
                if let Some(to) = to {
                    line["to"] = json!([to]);
                }
                if let Some(t) = thread {
                    line["thread"] = json!(t);
                }
                (Kind::Send, 0, line)
            }
        };
        {
            let mut b = self.book();
            if b.pending.values().any(|p| p.device == device) {
                return Err(Refusal::Busy);
            }
            b.pending.insert(
                id,
                Pending {
                    device: device.to_string(),
                    connection,
                    kind,
                    limit: cap,
                    threads: Vec::new(),
                    messages: Vec::new(),
                    chunks: 0,
                    bytes: 0,
                    outcome: None,
                },
            );
        }
        if !events.send(device, &line) {
            self.book().pending.remove(&id);
            return Err(Refusal::NotConnected);
        }
        self.wait(id, limit)
    }

    fn wait(&self, id: u64, limit: Duration) -> Result<Answer, Refusal> {
        let deadline = Instant::now() + limit;
        let mut b = self.book();
        loop {
            if b.pending.get(&id).is_some_and(|p| p.outcome.is_some()) {
                let p = b.pending.remove(&id).expect("present");
                return p.outcome.expect("set");
            }
            if !b.pending.contains_key(&id) {
                return Err(Refusal::Revoked);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                b.pending.remove(&id);
                return Err(Refusal::Timeout);
            }
            b = self.wake.wait_timeout(b, left).unwrap_or_else(|p| p.into_inner()).0;
        }
    }

    /// Answer one of [`VERBS`] from `device` on `connection`. `None` for a
    /// line this module does not own.
    pub fn control(&self, device: &str, connection: u64, line: &[u8]) -> Option<Vec<u8>> {
        let v: Value = serde_json::from_slice(line).ok()?;
        let cmd = v.get("cmd")?.as_str()?;
        if !VERBS.contains(&cmd) {
            return None;
        }
        if cmd == "messages_changed" {
            // A hint and nothing else: what changed is never said.
            self.book().changed.insert(device.to_string(), rime_remote_core::now_ms());
            return Some(ok());
        }
        let Some(id) = v["id"].as_u64() else {
            return Some(error("bad_request", "id must be a whole number"));
        };
        let mut b = self.book();
        // Counted only from the device and the connection it was written to,
        // and only while somebody waits: anything else is an unknown id.
        let Some(p) = b
            .pending
            .get_mut(&id)
            .filter(|p| p.device == device && p.connection == connection && p.outcome.is_none())
        else {
            return Some(error("unknown_id", "no request with that id is waiting"));
        };
        let answer = match cmd {
            "messages_reply" => take_reply(p, &v, line.len()),
            _ => take_sent(p, &v),
        };
        let reply = match &answer {
            Err(Refusal::BadReply(why)) => error("bad_request", why),
            _ => ok(),
        };
        if let Some(outcome) = answer.transpose() {
            p.finish(outcome);
            drop(b);
            self.wake.notify_all();
        }
        Some(reply)
    }

    /// `connection` ended: what was waiting on it fails now, and what it had
    /// sent so far is dropped.
    pub fn ended(&self, device: &str, connection: u64) {
        let mut b = self.book();
        let mut any = false;
        for p in b.pending.values_mut() {
            if p.device == device && p.connection == connection {
                p.finish(Err(Refusal::Disconnected));
                any = true;
            }
        }
        drop(b);
        if any {
            self.wake.notify_all();
        }
    }

    /// Revoke: everything about the phone goes.
    pub fn forget(&self, device: &str) {
        let mut b = self.book();
        b.changed.remove(device);
        for p in b.pending.values_mut() {
            if p.device == device {
                p.finish(Err(Refusal::Revoked));
            }
        }
        drop(b);
        self.wake.notify_all();
    }

    /// When `device` last said its messages changed.
    pub fn changed_ms(&self, device: &str) -> Option<u64> {
        self.book().changed.get(device).copied()
    }

    /// How many threads and messages are held for `device` right now. For
    /// the tests that prove nothing outlives a disconnect.
    #[cfg(test)]
    pub fn held(&self, device: &str) -> usize {
        self.book()
            .pending
            .values()
            .filter(|p| p.device == device)
            .map(|p| p.threads.len() + p.messages.len())
            .sum()
    }
}

/// One `messages_reply` line into `p`. `Ok(None)` when more is coming.
fn take_reply(p: &mut Pending, v: &Value, len: usize) -> Result<Option<Answer>, Refusal> {
    if let Some(word) = v.get("error") {
        return Err(Refusal::Phone(refusal_word(word)));
    }
    if p.kind == Kind::Send {
        return Err(Refusal::BadReply("a reply to a send must be messages_sent".into()));
    }
    p.chunks += 1;
    p.bytes += len;
    if p.chunks > MAX_CHUNKS || p.bytes > MAX_TOTAL_BYTES {
        return Err(Refusal::BadReply("the answer is larger than this computer accepts".into()));
    }
    match p.kind {
        Kind::Threads => {
            if !v["messages"].is_null() {
                return Err(Refusal::BadReply("threads were asked for".into()));
            }
            let list = v["threads"].as_array().ok_or_else(|| bad("threads must be a list"))?;
            for t in list {
                p.threads.push(Thread::parse(t)?);
            }
            if p.threads.len() > p.limit {
                return Err(bad("more threads than were asked for"));
            }
        }
        Kind::Thread(id) => {
            if v["thread"].as_u64() != Some(id) {
                return Err(bad("the answer is for another thread"));
            }
            let list = v["messages"].as_array().ok_or_else(|| bad("messages must be a list"))?;
            for m in list {
                p.messages.push(Message::parse(m)?);
            }
            if p.messages.len() > p.limit {
                return Err(bad("more messages than were asked for"));
            }
        }
        Kind::Send => unreachable!("refused above"),
    }
    if v["more"].as_bool() == Some(true) {
        return Ok(None);
    }
    Ok(Some(match p.kind {
        Kind::Threads => Answer::Threads(std::mem::take(&mut p.threads)),
        Kind::Thread(id) => Answer::Messages {
            thread: id,
            messages: std::mem::take(&mut p.messages),
        },
        Kind::Send => unreachable!("refused above"),
    }))
}

/// One `messages_sent` line into `p`.
fn take_sent(p: &mut Pending, v: &Value) -> Result<Option<Answer>, Refusal> {
    if p.kind != Kind::Send {
        return Err(bad("messages_sent answers a send"));
    }
    match v["ok"].as_bool() {
        Some(true) => Ok(Some(Answer::Sent)),
        Some(false) => Err(Refusal::Phone(refusal_word(&v["error"]))),
        None => Err(bad("ok must be true or false")),
    }
}

fn refusal_word(v: &Value) -> String {
    let w = v.as_str().unwrap_or("");
    if PHONE_REFUSALS.contains(&w) {
        w.to_string()
    } else {
        "failed".to_string()
    }
}

fn bad(why: &str) -> Refusal {
    Refusal::BadReply(why.to_string())
}

impl Thread {
    fn parse(v: &Value) -> Result<Thread, Refusal> {
        let addresses = v["addresses"].as_array().ok_or_else(|| bad("addresses must be a list"))?;
        if addresses.is_empty() || addresses.len() > MAX_ADDRESSES {
            return Err(bad("a thread has from 1 to 10 addresses"));
        }
        let addresses = addresses
            .iter()
            .map(|a| a.as_str().map(|a| clean(a, MAX_ADDRESS, 0)).ok_or_else(|| bad("an address must be text")))
            .collect::<Result<Vec<_>, _>>()?;
        let name = match &v["name"] {
            Value::Null => None,
            n => Some(clean(n.as_str().ok_or_else(|| bad("name must be text"))?, MAX_NAME, 0)).filter(|n| !n.is_empty()),
        };
        Ok(Thread {
            id: whole(v, "id")?,
            addresses,
            name,
            snippet: clean(v["snippet"].as_str().unwrap_or(""), MAX_SNIPPET, 0),
            date_ms: whole(v, "date_ms")?,
            unread: small(v, "unread")?,
            count: small(v, "count")?,
        })
    }
}

impl Message {
    fn parse(v: &Value) -> Result<Message, Refusal> {
        let status = v["status"].as_str().unwrap_or("");
        if !STATUSES.contains(&status) {
            return Err(bad("status must be received, sent, delivered, sending or failed"));
        }
        Ok(Message {
            id: whole(v, "id")?,
            date_ms: whole(v, "date_ms")?,
            from_me: v["from_me"].as_bool().ok_or_else(|| bad("from_me must be true or false"))?,
            body: clean(v["body"].as_str().ok_or_else(|| bad("body must be text"))?, MAX_BODY, MAX_BODY_LINES),
            status: status.to_string(),
        })
    }
}

fn whole(v: &Value, field: &str) -> Result<u64, Refusal> {
    v[field].as_u64().ok_or_else(|| bad(&format!("{field} must be a whole number")))
}

fn small(v: &Value, field: &str) -> Result<u32, Refusal> {
    v[field]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| bad(&format!("{field} must be a whole number")))
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Text from a phone, made safe to print in a terminal or show in the Shell:
/// control and bidi-override characters go, up to `lines` newlines stay (the
/// rest become spaces), and it is cut to `cap` characters with an ellipsis.
pub fn clean(raw: &str, cap: usize, lines: usize) -> String {
    let mut out = String::new();
    let mut count = 0;
    let mut kept = 0;
    for c in raw.chars() {
        let c = match c {
            '\n' if kept < lines => {
                kept += 1;
                '\n'
            }
            '\n' | '\t' => ' ',
            '\r' => continue,
            c if c.is_control() || is_bidi_control(c) || c == '\u{2028}' || c == '\u{2029}' => continue,
            c => c,
        };
        if count == cap {
            out = out.trim_end().to_string();
            out.push('…');
            return out;
        }
        count += 1;
        out.push(c);
    }
    out
}

/// A number a send goes to: digits, with an optional leading `+`, and the
/// spaces, dashes, dots and brackets people write between them. Returns the
/// number with those removed.
pub fn number(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    let mut out = String::new();
    for (i, c) in raw.chars().enumerate() {
        match c {
            '0'..='9' => out.push(c),
            '+' if i == 0 => out.push(c),
            ' ' | '-' | '.' | '(' | ')' => {}
            _ => return Err("a number is digits, with an optional + in front".into()),
        }
    }
    let digits = out.trim_start_matches('+').len();
    if !(3..=20).contains(&digits) {
        return Err("a number has from 3 to 20 digits".into());
    }
    Ok(out)
}

/// Text a send carries: not empty, at most [`MAX_BODY`] characters, and no
/// control characters but newlines.
pub fn text(raw: &str) -> Result<String, String> {
    let t = raw.trim_end_matches(['\n', '\r']);
    if t.trim().is_empty() {
        return Err("there is no text to send".into());
    }
    if t.chars().count() > MAX_BODY {
        return Err(format!("a message is at most {MAX_BODY} characters"));
    }
    if t.chars().any(|c| (c.is_control() && c != '\n') || is_bidi_control(c)) {
        return Err("the text has control characters in it".into());
    }
    Ok(t.to_string())
}

fn ok() -> Vec<u8> {
    json!({"reply": "ok"}).to_string().into_bytes()
}

fn error(code: &str, message: &str) -> Vec<u8> {
    json!({"reply": "error", "code": code, "message": message})
        .to_string()
        .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::sync::Arc;

    fn thread_json(id: u64) -> Value {
        json!({"id": id, "addresses": ["5556"], "snippet": "hi", "date_ms": 10 + id, "unread": 0, "count": 1})
    }

    /// A phone end: the events channel opened by `connection`, and a reader of
    /// what the computer writes to it.
    fn phone(events: &Events, device: &str, connection: u64, full: bool) -> BufReader<std::os::unix::net::UnixStream> {
        let (theirs, _) = events.open_for(device, connection, full).expect("open");
        BufReader::new(theirs)
    }

    fn next_event(r: &mut BufReader<std::os::unix::net::UnixStream>) -> Value {
        let mut line = String::new();
        r.read_line(&mut line).expect("read");
        serde_json::from_str(&line).expect("json")
    }

    fn asking(
        m: &Arc<Messages>,
        events: &Arc<Events>,
        want: Want,
        limit: Duration,
    ) -> std::thread::JoinHandle<Result<Answer, Refusal>> {
        let (m, events) = (Arc::clone(m), Arc::clone(events));
        std::thread::spawn(move || m.ask(&events, "dev-a", want, limit))
    }

    #[test]
    fn a_chunked_answer_is_put_back_together_and_only_the_last_chunk_finishes_it() {
        let (m, events) = (Arc::new(Messages::default()), Arc::new(Events::default()));
        let mut r = phone(&events, "dev-a", 7, true);
        let h = asking(&m, &events, Want::Threads { before: Some(99), limit: 5 }, READ_WAIT);
        let ev = next_event(&mut r);
        assert_eq!(ev["event"], REQUEST_EVENT);
        assert_eq!(ev["limit"], 5);
        assert_eq!(ev["before"], 99);
        assert!(ev.get("thread").is_none(), "{ev}");
        let id = ev["id"].as_u64().unwrap();
        let first = json!({"cmd": "messages_reply", "id": id, "threads": [thread_json(1), thread_json(2)], "more": true});
        assert_eq!(m.control("dev-a", 7, first.to_string().as_bytes()).unwrap(), ok());
        std::thread::sleep(Duration::from_millis(50));
        assert!(!h.is_finished(), "a chunk with more:true finished the request");
        let last = json!({"cmd": "messages_reply", "id": id, "threads": [thread_json(3)]});
        m.control("dev-a", 7, last.to_string().as_bytes()).unwrap();
        let Answer::Threads(t) = h.join().unwrap().expect("answer") else { panic!() };
        assert_eq!(t.iter().map(|t| t.id).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(m.held("dev-a"), 0);
    }

    #[test]
    fn a_reply_counts_only_from_the_device_and_connection_it_was_asked_on() {
        let (m, events) = (Arc::new(Messages::default()), Arc::new(Events::default()));
        let mut r = phone(&events, "dev-a", 7, true);
        let h = asking(&m, &events, Want::Thread { id: 4, before: None, limit: 10 }, Duration::from_millis(400));
        let id = next_event(&mut r)["id"].as_u64().unwrap();
        let line = json!({"cmd": "messages_reply", "id": id, "thread": 4, "messages": []}).to_string();
        for (dev, conn) in [("dev-b", 7), ("dev-a", 8)] {
            let out = String::from_utf8(m.control(dev, conn, line.as_bytes()).unwrap()).unwrap();
            assert!(out.contains("unknown_id"), "{dev}/{conn}: {out}");
        }
        assert_eq!(h.join().unwrap(), Err(Refusal::Timeout));
    }

    #[test]
    fn a_disconnect_fails_the_request_at_once_and_drops_what_had_come_in() {
        let (m, events) = (Arc::new(Messages::default()), Arc::new(Events::default()));
        let mut r = phone(&events, "dev-a", 7, true);
        let h = asking(&m, &events, Want::Threads { before: None, limit: 5 }, READ_WAIT);
        let id = next_event(&mut r)["id"].as_u64().unwrap();
        let first = json!({"cmd": "messages_reply", "id": id, "threads": [thread_json(1)], "more": true});
        m.control("dev-a", 7, first.to_string().as_bytes());
        assert_eq!(m.held("dev-a"), 1);
        let started = Instant::now();
        m.ended("dev-a", 7);
        assert_eq!(m.held("dev-a"), 0, "data outlived the disconnect");
        assert_eq!(h.join().unwrap(), Err(Refusal::Disconnected));
        assert!(started.elapsed() < Duration::from_secs(2));
        // And the rest of the answer, arriving late, completes nothing.
        let last = json!({"cmd": "messages_reply", "id": id, "threads": [thread_json(2)]});
        let out = String::from_utf8(m.control("dev-a", 7, last.to_string().as_bytes()).unwrap()).unwrap();
        assert!(out.contains("unknown_id"), "{out}");
    }

    #[test]
    fn another_connection_ending_leaves_the_request_alone() {
        let (m, events) = (Arc::new(Messages::default()), Arc::new(Events::default()));
        let mut r = phone(&events, "dev-a", 7, true);
        let h = asking(&m, &events, Want::Threads { before: None, limit: 5 }, READ_WAIT);
        let id = next_event(&mut r)["id"].as_u64().unwrap();
        m.ended("dev-a", 6);
        let last = json!({"cmd": "messages_reply", "id": id, "threads": [thread_json(2)]});
        m.control("dev-a", 7, last.to_string().as_bytes());
        assert!(h.join().unwrap().is_ok());
    }

    #[test]
    fn revoke_fails_the_request_and_forgets_the_hint() {
        let (m, events) = (Arc::new(Messages::default()), Arc::new(Events::default()));
        let mut r = phone(&events, "dev-a", 7, true);
        m.control("dev-a", 7, br#"{"cmd":"messages_changed"}"#);
        assert!(m.changed_ms("dev-a").is_some());
        let h = asking(&m, &events, Want::Threads { before: None, limit: 5 }, READ_WAIT);
        next_event(&mut r);
        m.forget("dev-a");
        assert_eq!(h.join().unwrap(), Err(Refusal::Revoked));
        assert_eq!(m.changed_ms("dev-a"), None);
    }

    #[test]
    fn a_send_to_a_companion_session_is_refused_here_without_writing_anything() {
        let (m, events) = (Arc::new(Messages::default()), Arc::new(Events::default()));
        let (theirs, _) = events.open_for("dev-a", 7, false).expect("open");
        theirs.set_nonblocking(true).unwrap();
        let started = Instant::now();
        let got = m.ask(&events, "dev-a", Want::Send { to: Some("5556".into()), thread: None, text: "x".into() }, SEND_WAIT);
        assert_eq!(got, Err(Refusal::Locked));
        assert!(started.elapsed() < Duration::from_secs(1));
        let mut buf = [0u8; 16];
        assert!(std::io::Read::read(&mut &theirs, &mut buf).is_err(), "something was written to a locked phone");
        // A read is fine from a companion session.
        let h = asking(&m, &events, Want::Threads { before: None, limit: 5 }, Duration::from_millis(300));
        assert_eq!(h.join().unwrap(), Err(Refusal::Timeout));
    }

    #[test]
    fn one_request_per_phone_and_no_request_without_a_channel() {
        let (m, events) = (Arc::new(Messages::default()), Arc::new(Events::default()));
        assert_eq!(
            m.ask(&events, "dev-a", Want::Threads { before: None, limit: 5 }, READ_WAIT),
            Err(Refusal::NotConnected)
        );
        let mut r = phone(&events, "dev-a", 7, true);
        let h = asking(&m, &events, Want::Threads { before: None, limit: 5 }, Duration::from_millis(500));
        next_event(&mut r);
        assert_eq!(
            m.ask(&events, "dev-a", Want::Threads { before: None, limit: 5 }, READ_WAIT),
            Err(Refusal::Busy)
        );
        assert_eq!(h.join().unwrap(), Err(Refusal::Timeout));
    }

    #[test]
    fn a_send_carries_the_number_and_text_and_finishes_on_messages_sent() {
        let (m, events) = (Arc::new(Messages::default()), Arc::new(Events::default()));
        let mut r = phone(&events, "dev-a", 7, true);
        let h = asking(&m, &events, Want::Send { to: Some("5556".into()), thread: None, text: "a\nb".into() }, SEND_WAIT);
        let ev = next_event(&mut r);
        assert_eq!(ev["event"], SEND_EVENT);
        assert_eq!(ev["to"], json!(["5556"]));
        assert_eq!(ev["text"], "a\nb");
        let id = ev["id"].as_u64().unwrap();
        // A reply of the wrong kind is refused and does not finish it.
        let wrong = json!({"cmd": "messages_reply", "id": id, "threads": []});
        assert!(String::from_utf8(m.control("dev-a", 7, wrong.to_string().as_bytes()).unwrap()).unwrap().contains("error"));
        let sent = json!({"cmd": "messages_sent", "id": id, "ok": false, "error": "something-new"});
        m.control("dev-a", 7, sent.to_string().as_bytes());
        // The wrong-kind reply already finished it as a bad reply.
        assert!(matches!(h.join().unwrap(), Err(Refusal::BadReply(_))));

        let h = asking(&m, &events, Want::Send { to: None, thread: Some(3), text: "x".into() }, SEND_WAIT);
        let ev = next_event(&mut r);
        assert_eq!(ev["thread"], 3);
        assert!(ev.get("to").is_none());
        let id = ev["id"].as_u64().unwrap();
        let sent = json!({"cmd": "messages_sent", "id": id, "ok": false, "error": "something-new"});
        m.control("dev-a", 7, sent.to_string().as_bytes());
        assert_eq!(h.join().unwrap(), Err(Refusal::Phone("failed".into())), "an unknown word reads as failed");
    }

    #[test]
    fn the_phone_is_held_to_the_limit_and_the_size() {
        let (m, events) = (Arc::new(Messages::default()), Arc::new(Events::default()));
        let mut r = phone(&events, "dev-a", 7, true);
        let h = asking(&m, &events, Want::Threads { before: None, limit: 2 }, READ_WAIT);
        let id = next_event(&mut r)["id"].as_u64().unwrap();
        let line = json!({"cmd": "messages_reply", "id": id, "threads": [thread_json(1), thread_json(2), thread_json(3)]});
        m.control("dev-a", 7, line.to_string().as_bytes());
        assert!(matches!(h.join().unwrap(), Err(Refusal::BadReply(_))));

        let h = asking(&m, &events, Want::Thread { id: 1, before: None, limit: 50 }, READ_WAIT);
        let id = next_event(&mut r)["id"].as_u64().unwrap();
        let chunk = json!({"cmd": "messages_reply", "id": id, "thread": 1, "messages": [], "more": true}).to_string();
        for _ in 0..MAX_CHUNKS {
            m.control("dev-a", 7, chunk.as_bytes());
        }
        assert!(!h.is_finished());
        m.control("dev-a", 7, chunk.as_bytes());
        assert!(matches!(h.join().unwrap(), Err(Refusal::BadReply(_))));
    }

    #[test]
    fn the_limit_asked_for_is_clamped() {
        let (m, events) = (Arc::new(Messages::default()), Arc::new(Events::default()));
        let mut r = phone(&events, "dev-a", 7, true);
        let h = asking(&m, &events, Want::Threads { before: None, limit: 5000 }, Duration::from_millis(200));
        assert_eq!(next_event(&mut r)["limit"], MAX_LIMIT);
        let _ = h.join();
    }

    #[test]
    fn text_from_the_phone_is_cleaned_and_cut() {
        let raw = "a\u{202E}b\u{0007}c\r\nd\te";
        assert_eq!(clean(raw, 100, 5), "abc\nd e");
        assert_eq!(clean("x\ny\nz", 100, 1), "x\ny z");
        assert_eq!(clean(&"é".repeat(10), 4, 0), "éééé…");
        let t = Thread::parse(&json!({"id": 1, "addresses": ["+1 555\u{202E}"], "name": "\u{0007}", "snippet": "s\nt",
            "date_ms": 2, "unread": 1, "count": 3}))
        .expect("parse");
        assert_eq!(t.addresses, ["+1 555"]);
        assert_eq!(t.name, None, "a name that cleans to nothing is no name");
        assert_eq!(t.snippet, "s t");
        let m = Message::parse(&json!({"id": 1, "date_ms": 2, "from_me": true, "body": "x".repeat(3000), "status": "sent"}))
            .expect("parse");
        assert_eq!(m.body.chars().count(), MAX_BODY + 1);
        assert!(Message::parse(&json!({"id": 1, "date_ms": 2, "from_me": true, "body": "x", "status": "draft"})).is_err());
        assert!(Thread::parse(&json!({"id": 1, "addresses": [], "snippet": "", "date_ms": 2, "unread": 0, "count": 0})).is_err());
    }

    #[test]
    fn a_number_and_a_text_are_checked_before_anything_is_asked() {
        assert_eq!(number(" +1 (555) 521-5556 ").unwrap(), "+15555215556");
        assert_eq!(number("5556").unwrap(), "5556");
        for bad in ["", "12", "5+56", "abc", "555;rm", &"1".repeat(21)] {
            assert!(number(bad).is_err(), "{bad:?}");
        }
        assert_eq!(text("hello\n").unwrap(), "hello");
        assert!(text("  \n").is_err());
        assert!(text(&"x".repeat(MAX_BODY + 1)).is_err());
        assert!(text(&"x".repeat(MAX_BODY)).is_ok());
        assert!(text("a\u{0007}b").is_err());
        assert!(text("a\u{202E}b").is_err());
    }

    #[test]
    fn no_sentence_carries_what_the_phone_sent() {
        for r in [
            Refusal::NotConnected,
            Refusal::Locked,
            Refusal::Busy,
            Refusal::Disconnected,
            Refusal::Revoked,
            Refusal::Timeout,
            Refusal::Phone("off".into()),
            Refusal::Phone("no_permission".into()),
        ] {
            let s = r.sentence("Pixel");
            assert!(s.contains("Pixel"), "{s}");
        }
    }
}
