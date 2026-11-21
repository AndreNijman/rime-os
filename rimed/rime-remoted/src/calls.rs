//! A phone's calls, seen and handled from this machine (contract §2.12).
//!
//! The phone says when a call rings, is answered and ends (`call_state`); a
//! ringing call shows here as a notification with Answer and Decline, and
//! closes once the phone says it stopped ringing. From this machine the
//! person may answer, decline or hang up, dial a number, and read the call
//! history. There is no audio: the call itself stays on the phone.
//!
//! Nothing here touches a disk. The current call is held in memory with the
//! connection that reported it and goes when that connection ends; a history
//! answer is held only while the person who asked waits for it. No number or
//! name is ever written to a log or an error.
//!
//! The phone is trusted with nothing: every reply is checked against the
//! request it answers (same device, same connection, the kind that was
//! asked), and every field is cleaned and cut again here.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::messages::clean;
use crate::mirror::{Job, Mirror, Shown};
use crate::phone::{Events, PhoneReport};

/// The control lines a phone sends about calls.
pub const VERBS: [&str; 3] = ["call_state", "call_history_reply", "call_done"];

/// What `remote_hello` offers: this machine understands the verbs above.
pub const FEATURES: [&str; 1] = ["phone_calls"];

/// The event that asks the phone to answer, decline, hang up or dial.
pub const ACTION_EVENT: &str = "call_action";

/// The event that asks the phone for its call history.
pub const HISTORY_EVENT: &str = "call_history_request";

/// The mirror key of the ringing notification. A phone's own keys never hold
/// a `:`, so it can be neither replaced nor closed by one of them.
pub const CALL_KEY: &str = "call:ringing";

/// The states a phone reports. `held` is in the contract for a phone app that
/// can see it; Rime Remote on Android cannot, and never sends it.
pub const STATES: [&str; 4] = ["ringing", "active", "held", "idle"];

/// The kinds a history entry may carry.
pub const TYPES: [&str; 4] = ["incoming", "outgoing", "missed", "rejected"];

/// What the computer may ask the phone to do.
pub const ACTIONS: [&str; 4] = ["answer", "decline", "hangup", "dial"];

/// The most calls one history request asks for.
pub const MAX_LIMIT: usize = 50;

/// What a history read asks for when the caller named no limit.
pub const DEFAULT_LIMIT: usize = 20;

/// The longest contact name kept, in characters.
pub const MAX_NAME: usize = 60;

/// The longest number kept, in characters.
pub const MAX_NUMBER: usize = 40;

/// The most reply lines one history request accepts, and the most bytes.
pub const MAX_CHUNKS: usize = 64;
pub const MAX_TOTAL_BYTES: usize = 256 * 1024;

/// How long a history read waits for the phone's whole answer.
pub const HISTORY_WAIT: Duration = Duration::from_secs(10);

/// How long an action waits for the phone's `call_done`. Answering, ending
/// and placing a call are each one call into the phone's telephony service.
pub const ACTION_WAIT: Duration = Duration::from_secs(10);

/// The reason words a phone may refuse with. Anything else reads as `failed`.
pub const PHONE_REFUSALS: [&str; 7] = ["off", "no_permission", "locked", "busy", "no_call", "emergency", "failed"];

/// The call a phone has right now, as it last said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    /// `ringing`, `active` or `held`. An idle phone has no call.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// When the phone entered this state (unix ms, the phone's clock).
    pub since_ms: u64,
}

/// One entry in the call history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Empty when the caller withheld it.
    pub number: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// One of [`TYPES`].
    #[serde(rename = "type")]
    pub kind: String,
    pub date_ms: u64,
    pub duration_s: u64,
}

/// Why a request got no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No events channel is open for the phone.
    NotConnected,
    /// The phone's channel belongs to a companion (locked) session, and a
    /// dial needs the app unlocked.
    Locked,
    /// The phone's report does not offer this (switch off, or the phone did
    /// not grant the permission it needs).
    Unsupported(&'static str),
    /// There is no call for this action (nothing ringing to answer, nothing
    /// to hang up).
    NoCall,
    /// A dial while a call is in progress.
    InCall,
    /// Another request of the same kind for this phone is waiting.
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
    /// The sentence the person reads. Never carries a number or a name.
    pub fn sentence(&self, name: &str) -> String {
        match self {
            Refusal::NotConnected => format!("{name} is not connected. Open Rime Remote on it, then try again"),
            Refusal::Locked => {
                format!("{name} is locked: Rime Remote dials a number only while it is unlocked on the phone")
            }
            Refusal::Unsupported(what) => format!(
                "{name} does not let this computer {what}. Allow it in Rime Remote → Calls on your computer"
            ),
            Refusal::NoCall => format!("{name} has no call for that"),
            Refusal::InCall => format!("{name} is already on a call"),
            Refusal::Busy => format!("{name} is already answering another calls request; try again in a moment"),
            Refusal::Disconnected => format!("{name} disconnected before it finished answering"),
            Refusal::Revoked => format!("{name} was revoked"),
            Refusal::Timeout => format!("{name} did not answer in time"),
            Refusal::BadReply(why) => format!("{name} answered with something this computer does not accept: {why}"),
            Refusal::Phone(word) => match word.as_str() {
                "off" => format!("Calls on your computer is turned off on {name} (Rime Remote → Calls on your computer)"),
                "no_permission" => {
                    format!("Rime Remote on {name} is not allowed to do that. Allow it in Rime Remote → Calls on your computer")
                }
                "locked" => format!("{name} is locked: unlock Rime Remote on it to dial"),
                "busy" => format!("{name} is busy; try again in a moment"),
                "no_call" => format!("{name} has no call for that"),
                "emergency" => format!("{name} does not dial emergency numbers from a computer. Dial them on the phone"),
                _ => format!("{name} could not do that"),
            },
        }
    }
}

/// The kind of answer a request waits for. One of each per phone at a time,
/// so a slow history read never makes Answer wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    History,
    Action,
}

struct Pending {
    device: String,
    connection: u64,
    lane: Lane,
    limit: usize,
    calls: Vec<Entry>,
    chunks: usize,
    bytes: usize,
    outcome: Option<Result<Vec<Entry>, Refusal>>,
}

impl Pending {
    fn finish(&mut self, outcome: Result<Vec<Entry>, Refusal>) {
        if self.outcome.is_none() {
            if outcome.is_err() {
                self.calls = Vec::new();
            }
            self.outcome = Some(outcome);
        }
    }
}

#[derive(Default)]
struct Book {
    pending: HashMap<u64, Pending>,
    /// Each phone's current call and the connection that reported it.
    current: HashMap<String, (u64, Call)>,
}

/// The requests waiting on phones, and the call each phone has now.
#[derive(Default)]
pub struct Calls {
    next: AtomicU64,
    book: Mutex<Book>,
    wake: Condvar,
}

/// The sub-capability words of contract §2.12, kept from a report only with
/// [`crate::phone::CAN_CALLS`].
fn may(report: &PhoneReport, word: &str) -> bool {
    report.can_calls() && report.can.iter().any(|c| c == word)
}

impl Calls {
    fn book(&self) -> std::sync::MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The call `device` has now, as its live connection last said.
    pub fn current(&self, device: &str) -> Option<Call> {
        self.book().current.get(device).map(|(_, c)| c.clone())
    }

    /// Ask `device` to answer, decline, hang up or dial `number`, and wait up
    /// to `limit` for its `call_done`.
    pub fn act(
        &self,
        events: &Events,
        device: &str,
        report: &PhoneReport,
        action: &str,
        number: Option<&str>,
        limit: Duration,
    ) -> Result<(), Refusal> {
        if !ACTIONS.contains(&action) {
            return Err(Refusal::BadReply("not a call action".into()));
        }
        let dial = action == "dial";
        if !report.can_calls() {
            return Err(Refusal::Phone("off".into()));
        }
        if dial && !may(report, crate::phone::CAN_CALLS_DIAL) {
            return Err(Refusal::Unsupported("dial"));
        }
        if !dial && !may(report, crate::phone::CAN_CALLS_ANSWER) {
            return Err(Refusal::Unsupported("answer or end calls"));
        }
        let Some((connection, full)) = events.holder(device) else {
            return Err(Refusal::NotConnected);
        };
        if dial && !full {
            // Answered here and at once: a companion session never sees a
            // dial, so a lifted companion key cannot place a call.
            return Err(Refusal::Locked);
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let mut line = json!({"event": ACTION_EVENT, "id": id, "action": action});
        {
            let mut b = self.book();
            let now = b.current.get(device).filter(|(c, _)| *c == connection).map(|(_, call)| call.state.as_str());
            match (action, now) {
                ("answer" | "decline", Some("ringing")) => {}
                ("hangup", Some("ringing" | "active" | "held")) => {}
                ("dial", None) => {}
                ("dial", Some(_)) => return Err(Refusal::InCall),
                _ => return Err(Refusal::NoCall),
            }
            if dial {
                let n = crate::messages::number(number.unwrap_or("")).map_err(Refusal::BadReply)?;
                line["number"] = json!(n);
            }
            if b.pending.values().any(|p| p.device == device && p.lane == Lane::Action) {
                return Err(Refusal::Busy);
            }
            b.pending.insert(id, pending(device, connection, Lane::Action, 0));
        }
        if !events.send(device, &line) {
            self.book().pending.remove(&id);
            return Err(Refusal::NotConnected);
        }
        self.wait(id, limit).map(|_| ())
    }

    /// Ask `device` for its newest calls, before `before` when set, and wait
    /// up to `limit` for the whole answer.
    pub fn history(
        &self,
        events: &Events,
        device: &str,
        report: &PhoneReport,
        before: Option<u64>,
        count: usize,
        limit: Duration,
    ) -> Result<Vec<Entry>, Refusal> {
        if !report.can_calls() {
            return Err(Refusal::Phone("off".into()));
        }
        if !may(report, crate::phone::CAN_CALLS_HISTORY) {
            return Err(Refusal::Unsupported("read its call history"));
        }
        let Some((connection, _)) = events.holder(device) else {
            return Err(Refusal::NotConnected);
        };
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let count = count.clamp(1, MAX_LIMIT);
        let mut line = json!({"event": HISTORY_EVENT, "id": id, "limit": count});
        if let Some(b) = before {
            line["before"] = json!(b);
        }
        {
            let mut b = self.book();
            if b.pending.values().any(|p| p.device == device && p.lane == Lane::History) {
                return Err(Refusal::Busy);
            }
            b.pending.insert(id, pending(device, connection, Lane::History, count));
        }
        if !events.send(device, &line) {
            self.book().pending.remove(&id);
            return Err(Refusal::NotConnected);
        }
        self.wait(id, limit)
    }

    fn wait(&self, id: u64, limit: Duration) -> Result<Vec<Entry>, Refusal> {
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
    /// line this module does not own. `phone` is the device store's name for
    /// it, `report` its last report, and `m` where the ringing notification
    /// goes.
    pub fn control(
        &self,
        device: &str,
        phone: &str,
        connection: u64,
        report: Option<&PhoneReport>,
        m: &Mirror,
        line: &[u8],
    ) -> Option<Vec<u8>> {
        let v: Value = serde_json::from_slice(line).ok()?;
        let cmd = v.get("cmd")?.as_str()?;
        if !VERBS.contains(&cmd) {
            return None;
        }
        if cmd == "call_state" {
            // The switch is the phone's, and its report says where it is: a
            // phone that did not say "calls" shows nothing here.
            let Some(report) = report.filter(|r| r.can_calls()) else {
                return Some(error("off", "this phone's report does not offer calls"));
            };
            return Some(match parse_state(&v) {
                Ok(call) => {
                    self.state(device, phone, connection, report, m, call);
                    ok()
                }
                Err(why) => error("bad_request", &why),
            });
        }
        let Some(id) = v["id"].as_u64() else {
            return Some(error("bad_request", "id must be a whole number"));
        };
        let mut b = self.book();
        let Some(p) = b
            .pending
            .get_mut(&id)
            .filter(|p| p.device == device && p.connection == connection && p.outcome.is_none())
        else {
            return Some(error("unknown_id", "no request with that id is waiting"));
        };
        let answer = match cmd {
            "call_history_reply" => take_history(p, &v, line.len()),
            _ => take_done(p, &v),
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

    /// A `call_state` the phone sent: stored, and the ringing notification
    /// shown, updated or closed to match.
    fn state(&self, device: &str, phone: &str, connection: u64, report: &PhoneReport, m: &Mirror, call: Option<Call>) {
        let mut b = self.book();
        let before = b.current.get(device).filter(|(c, _)| *c == connection).map(|(_, call)| call.clone());
        let was_ringing = b.current.get(device).is_some_and(|(_, c)| c.state == "ringing");
        let Some(mut call) = call else {
            b.current.remove(device);
            drop(b);
            if was_ringing {
                m.submit(Job::Remove { device: device.into(), key: CALL_KEY.into() });
            }
            return;
        };
        if call.state == "ringing" {
            // Android says a call rings twice, once before the number is
            // known: the second, numberless line must not erase the first.
            if let Some(prev) = before.as_ref().filter(|p| p.state == "ringing") {
                if call.number.is_none() {
                    call.number = prev.number.clone();
                    call.name = call.name.or_else(|| prev.name.clone());
                }
            }
            let changed = before.as_ref() != Some(&call);
            let fresh = !before.as_ref().is_some_and(|p| p.state == "ringing");
            b.current.insert(device.to_string(), (connection, call.clone()));
            drop(b);
            // Charged like a phone notification, so a phone that rings over
            // and over cannot flood the desktop; an update is not charged.
            if changed && (!fresh || m.allow(device, Instant::now())) {
                let can_answer = may(report, crate::phone::CAN_CALLS_ANSWER);
                m.submit(Job::Post { device: device.into(), key: CALL_KEY.into(), shown: ringing(phone, &call, can_answer) });
            }
            return;
        }
        b.current.insert(device.to_string(), (connection, call));
        drop(b);
        if was_ringing {
            m.submit(Job::Remove { device: device.into(), key: CALL_KEY.into() });
        }
    }

    /// The phone's report no longer says "calls": what it said goes, the
    /// ringing notification closes, and what waited on it fails.
    pub fn off(&self, device: &str, m: &Mirror) {
        let mut b = self.book();
        let gone = b.current.remove(device);
        let mut any = false;
        for p in b.pending.values_mut() {
            if p.device == device {
                p.finish(Err(Refusal::Phone("off".into())));
                any = true;
            }
        }
        drop(b);
        if gone.is_some_and(|(_, c)| c.state == "ringing") {
            m.submit(Job::Remove { device: device.into(), key: CALL_KEY.into() });
        }
        if any {
            self.wake.notify_all();
        }
    }

    /// `connection` ended: its call is no longer known, what waited on it
    /// fails, and what it had sent so far is dropped.
    pub fn ended(&self, device: &str, connection: u64, m: &Mirror) {
        let mut b = self.book();
        let mut any = false;
        for p in b.pending.values_mut() {
            if p.device == device && p.connection == connection {
                p.finish(Err(Refusal::Disconnected));
                any = true;
            }
        }
        let gone = if b.current.get(device).is_some_and(|(c, _)| *c == connection) {
            b.current.remove(device)
        } else {
            None
        };
        drop(b);
        if gone.is_some_and(|(_, c)| c.state == "ringing") {
            m.submit(Job::Remove { device: device.into(), key: CALL_KEY.into() });
        }
        if any {
            self.wake.notify_all();
        }
    }

    /// Revoke: everything about the phone goes. The mirror closes its
    /// notifications itself.
    pub fn forget(&self, device: &str) {
        let mut b = self.book();
        b.current.remove(device);
        for p in b.pending.values_mut() {
            if p.device == device {
                p.finish(Err(Refusal::Revoked));
            }
        }
        drop(b);
        self.wake.notify_all();
    }

    /// How many history entries are held for `device` right now. For the
    /// tests that prove nothing outlives a disconnect.
    #[cfg(test)]
    pub fn held(&self, device: &str) -> usize {
        self.book().pending.values().filter(|p| p.device == device).map(|p| p.calls.len()).sum()
    }
}

fn pending(device: &str, connection: u64, lane: Lane, limit: usize) -> Pending {
    Pending {
        device: device.to_string(),
        connection,
        lane,
        limit,
        calls: Vec::new(),
        chunks: 0,
        bytes: 0,
        outcome: None,
    }
}

/// The ringing notification. Answer and Decline only when the phone said it
/// can do them from here.
pub fn ringing(phone: &str, call: &Call, can_answer: bool) -> Shown {
    let who = call.name.as_deref().or(call.number.as_deref());
    let summary = match who {
        Some(who) => format!("Call from {who}"),
        None => "Incoming call".to_string(),
    };
    // The number under a name, so a known caller can still be told apart.
    let body = match (&call.name, &call.number) {
        (Some(_), Some(n)) => n.clone(),
        _ => String::new(),
    };
    let phone = crate::mirror::clean(phone, MAX_NAME, 0, false);
    let actions = if can_answer {
        vec![("answer".to_string(), "Answer".to_string()), ("decline".to_string(), "Decline".to_string())]
    } else {
        Vec::new()
    };
    Shown {
        app_name: if phone.is_empty() { "Calls".into() } else { format!("{phone} · Calls") },
        summary: crate::mirror::clean(&summary, 120, 0, false),
        body: crate::mirror::clean(&body, MAX_NUMBER, 0, true),
        reply: None,
        actions,
        sticky: true,
        expires_ms: None,
    }
}

/// The notice for a button pressed here whose action did not happen.
pub fn not_done(action: &str, why: &Refusal, phone: &str) -> Shown {
    let summary = match action {
        "answer" => "Call not answered",
        "decline" => "Call not declined",
        _ => "Call not ended",
    };
    Shown {
        app_name: "Rime Remote".into(),
        summary: summary.into(),
        body: crate::mirror::clean(&format!("{}.", why.sentence(phone)), 400, 0, true),
        reply: None,
        actions: Vec::new(),
        sticky: false,
        expires_ms: None,
    }
}

/// A `call_state` line's call, `None` for idle, or why it is refused.
fn parse_state(v: &Value) -> Result<Option<Call>, String> {
    let state = v["state"].as_str().unwrap_or("");
    if !STATES.contains(&state) {
        return Err("state must be ringing, active, held or idle".into());
    }
    let since_ms = v["since_ms"].as_u64().ok_or("since_ms must be a whole number")?;
    if state == "idle" {
        return Ok(None);
    }
    Ok(Some(Call {
        state: state.to_string(),
        number: text(v, "number", MAX_NUMBER).map_err(|e| e.to_string())?,
        name: text(v, "name", MAX_NAME).map_err(|e| e.to_string())?,
        since_ms,
    }))
}

/// An optional text field, cleaned; empty reads as absent.
fn text(v: &Value, field: &'static str, cap: usize) -> Result<Option<String>, &'static str> {
    match &v[field] {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(clean(s, cap, 0)).filter(|s| !s.trim().is_empty())),
        _ => Err(if field == "name" { "name must be text" } else { "number must be text" }),
    }
}

/// One `call_history_reply` line into `p`. `Ok(None)` when more is coming.
fn take_history(p: &mut Pending, v: &Value, len: usize) -> Result<Option<Vec<Entry>>, Refusal> {
    if p.lane != Lane::History {
        return Err(bad("call_history_reply answers a history request"));
    }
    if let Some(word) = v.get("error") {
        return Err(Refusal::Phone(refusal_word(word)));
    }
    p.chunks += 1;
    p.bytes += len;
    if p.chunks > MAX_CHUNKS || p.bytes > MAX_TOTAL_BYTES {
        return Err(bad("the answer is larger than this computer accepts"));
    }
    let list = v["calls"].as_array().ok_or_else(|| bad("calls must be a list"))?;
    for c in list {
        p.calls.push(Entry::parse(c)?);
    }
    if p.calls.len() > p.limit {
        return Err(bad("more calls than were asked for"));
    }
    if v["more"].as_bool() == Some(true) {
        return Ok(None);
    }
    Ok(Some(std::mem::take(&mut p.calls)))
}

/// One `call_done` line into `p`.
fn take_done(p: &mut Pending, v: &Value) -> Result<Option<Vec<Entry>>, Refusal> {
    if p.lane != Lane::Action {
        return Err(bad("call_done answers a call action"));
    }
    match v["ok"].as_bool() {
        Some(true) => Ok(Some(Vec::new())),
        Some(false) => Err(Refusal::Phone(refusal_word(&v["error"]))),
        None => Err(bad("ok must be true or false")),
    }
}

impl Entry {
    fn parse(v: &Value) -> Result<Entry, Refusal> {
        let kind = v["type"].as_str().unwrap_or("");
        if !TYPES.contains(&kind) {
            return Err(bad("type must be incoming, outgoing, missed or rejected"));
        }
        let number = match &v["number"] {
            Value::Null => String::new(),
            n => clean(n.as_str().ok_or_else(|| bad("number must be text"))?, MAX_NUMBER, 0),
        };
        let name = text(v, "name", MAX_NAME).map_err(bad)?;
        Ok(Entry {
            number,
            name,
            kind: kind.to_string(),
            date_ms: v["date_ms"].as_u64().ok_or_else(|| bad("date_ms must be a whole number"))?,
            duration_s: v["duration_s"].as_u64().ok_or_else(|| bad("duration_s must be a whole number"))?,
        })
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

fn ok() -> Vec<u8> {
    json!({"reply": "ok"}).to_string().into_bytes()
}

fn error(code: &str, message: &str) -> Vec<u8> {
    json!({"reply": "error", "code": code, "message": message}).to_string().into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::Stub;
    use std::io::{BufRead, BufReader};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::Receiver;
    use std::sync::Arc;

    struct Desk {
        dir: PathBuf,
        m: Mirror,
        rx: Receiver<Job>,
    }

    impl Drop for Desk {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    impl Desk {
        fn new(tag: &str) -> Desk {
            let dir = std::env::temp_dir().join(format!("rime-calls-{tag}-{}-{}", std::process::id(), rime_remote_core::now_ms()));
            let m = Mirror::new(Box::new(Stub::new(dir.clone())));
            let rx = m.open_queue();
            Desk { dir, m, rx }
        }

        /// Do what was queued, as the worker would, and return every line the
        /// stand-in desktop holds.
        fn shown(&self) -> Vec<Value> {
            while let Ok(job) = self.rx.try_recv() {
                self.m.handle(job);
            }
            lines(&self.dir)
        }
    }

    fn lines(dir: &Path) -> Vec<Value> {
        std::fs::read_to_string(dir.join("notifications.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn report(can: &[&str]) -> PhoneReport {
        PhoneReport {
            battery: None,
            charging: None,
            can: can.iter().map(|s| s.to_string()).collect(),
            network: None,
            dnd: None,
            reported_ms: 1,
        }
    }

    fn all() -> PhoneReport {
        report(&["calls", "calls_answer", "calls_dial", "calls_history"])
    }

    fn phone(events: &Events, device: &str, connection: u64, full: bool) -> BufReader<std::os::unix::net::UnixStream> {
        let (theirs, _) = events.open_for(device, connection, full).expect("open");
        theirs.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        BufReader::new(theirs)
    }

    fn next_event(r: &mut BufReader<std::os::unix::net::UnixStream>) -> Value {
        let mut line = String::new();
        r.read_line(&mut line).expect("read");
        serde_json::from_str(&line).expect("json")
    }

    /// Nothing more was written to the phone's channel.
    fn quiet(r: &mut BufReader<std::os::unix::net::UnixStream>) {
        r.get_ref().set_nonblocking(true).unwrap();
        let mut line = String::new();
        let got = r.read_line(&mut line);
        assert!(matches!(&got, Err(e) if e.kind() == std::io::ErrorKind::WouldBlock), "{got:?} {line}");
        r.get_ref().set_nonblocking(false).unwrap();
    }

    fn state_line(state: &str, number: Option<&str>, name: Option<&str>) -> Vec<u8> {
        let mut v = json!({"cmd": "call_state", "state": state, "since_ms": 1000});
        if let Some(n) = number {
            v["number"] = json!(n);
        }
        if let Some(n) = name {
            v["name"] = json!(n);
        }
        v.to_string().into_bytes()
    }

    fn reply(out: Option<Vec<u8>>) -> Value {
        serde_json::from_slice(&out.expect("answered")).expect("json")
    }

    #[test]
    fn a_ringing_call_shows_with_answer_and_decline_and_closes_when_it_stops() {
        let d = Desk::new("ring");
        let c = Calls::default();
        let r = all();
        assert_eq!(reply(c.control("dev", "Pixel", 7, Some(&r), &d.m, &state_line("ringing", None, None)))["reply"], "ok");
        // Android's second ringing line, with the number and the contact.
        let line = state_line("ringing", Some("5556"), Some("Emulator <b>Two</b>\u{202E}"));
        assert_eq!(reply(c.control("dev", "Pixel", 7, Some(&r), &d.m, &line))["reply"], "ok");
        // And a third, numberless again: it must not erase what is known.
        c.control("dev", "Pixel", 7, Some(&r), &d.m, &state_line("ringing", None, None));
        assert_eq!(c.current("dev").unwrap().number.as_deref(), Some("5556"));
        let shown = d.shown();
        assert_eq!(shown.len(), 2, "{shown:?}");
        assert_eq!(shown[0]["summary"], "Incoming call");
        assert_eq!(shown[0]["actions"], json!([["answer", "Answer"], ["decline", "Decline"]]));
        assert_eq!(shown[0]["sticky"], true);
        assert_eq!(shown[0]["app"], "Pixel · Calls");
        // The update replaces the same notification, made safe.
        assert_eq!(shown[1]["replaces"], shown[0]["id"]);
        assert_eq!(shown[1]["summary"], "Call from Emulator ‹b›Two‹/b›");
        assert_eq!(shown[1]["body"], "5556");
        // Answered on the phone: the notification closes.
        c.control("dev", "Pixel", 7, Some(&r), &d.m, &state_line("active", Some("5556"), None));
        assert_eq!(c.current("dev").unwrap().state, "active");
        let shown = d.shown();
        assert_eq!(shown.last().unwrap(), &json!({"op": "close", "id": shown[0]["id"]}));
        // Hung up: nothing is held, nothing more is shown.
        c.control("dev", "Pixel", 7, Some(&r), &d.m, &state_line("idle", None, None));
        assert_eq!(c.current("dev"), None);
        assert_eq!(d.shown().len(), 3);
    }

    #[test]
    fn ringing_then_idle_closes_the_notification() {
        let d = Desk::new("idle");
        let c = Calls::default();
        let r = all();
        c.control("dev", "Pixel", 7, Some(&r), &d.m, &state_line("ringing", Some("5556"), None));
        c.control("dev", "Pixel", 7, Some(&r), &d.m, &state_line("idle", None, None));
        let shown = d.shown();
        assert_eq!(shown.len(), 2, "{shown:?}");
        assert_eq!(shown[1], json!({"op": "close", "id": shown[0]["id"]}));
    }

    #[test]
    fn without_answering_allowed_the_notification_has_no_buttons() {
        let d = Desk::new("nobuttons");
        let c = Calls::default();
        c.control("dev", "Pixel", 7, Some(&report(&["calls"])), &d.m, &state_line("ringing", Some("5556"), None));
        let shown = d.shown();
        assert!(shown[0].get("actions").is_none(), "{shown:?}");
        assert_eq!(shown[0]["summary"], "Call from 5556");
    }

    #[test]
    fn a_phone_that_did_not_say_calls_shows_and_keeps_nothing() {
        let d = Desk::new("off");
        let c = Calls::default();
        for r in [None, Some(report(&["ring", "messages", "calls_answer"]))] {
            let v = reply(c.control("dev", "Pixel", 7, r.as_ref(), &d.m, &state_line("ringing", Some("5556"), None)));
            assert_eq!(v["code"], "off", "{v}");
        }
        assert_eq!(c.current("dev"), None);
        assert!(d.shown().is_empty());
    }

    #[test]
    fn a_bad_call_state_is_refused_and_changes_nothing() {
        let d = Desk::new("bad");
        let c = Calls::default();
        let r = all();
        for line in [
            json!({"cmd": "call_state", "state": "dialing", "since_ms": 1}),
            json!({"cmd": "call_state", "state": "ringing"}),
            json!({"cmd": "call_state", "state": "ringing", "since_ms": 1, "number": 5556}),
            json!({"cmd": "call_state", "state": "ringing", "since_ms": 1, "name": ["x"]}),
        ] {
            let v = reply(c.control("dev", "Pixel", 7, Some(&r), &d.m, line.to_string().as_bytes()));
            assert_eq!(v["code"], "bad_request", "{line} {v}");
        }
        assert_eq!(c.current("dev"), None);
        assert!(d.shown().is_empty());
    }

    #[test]
    fn the_connection_ending_or_calls_turned_off_closes_a_ringing_call() {
        let d = Desk::new("ended");
        let c = Calls::default();
        let r = all();
        c.control("dev", "Pixel", 7, Some(&r), &d.m, &state_line("ringing", Some("5556"), None));
        // Another connection ending says nothing about this one's call.
        c.ended("dev", 8, &d.m);
        assert!(c.current("dev").is_some());
        c.ended("dev", 7, &d.m);
        assert_eq!(c.current("dev"), None);
        let shown = d.shown();
        assert_eq!(shown.last().unwrap()["op"], "close");
        c.control("dev", "Pixel", 9, Some(&r), &d.m, &state_line("ringing", Some("5556"), None));
        c.off("dev", &d.m);
        assert_eq!(c.current("dev"), None);
        let shown = d.shown();
        assert_eq!(shown.len(), 4, "{shown:?}");
        assert_eq!(shown[3], json!({"op": "close", "id": shown[2]["id"]}));
    }

    #[test]
    fn a_phone_ringing_over_and_over_is_charged_like_its_notifications() {
        let d = Desk::new("flood");
        let c = Calls::default();
        let r = all();
        for _ in 0..40 {
            c.control("dev", "Pixel", 7, Some(&r), &d.m, &state_line("ringing", Some("5556"), None));
            c.control("dev", "Pixel", 7, Some(&r), &d.m, &state_line("idle", None, None));
            d.shown();
        }
        let posted = d.shown().iter().filter(|l| l["op"] == "notify").count();
        assert!(posted <= crate::mirror::BURST as usize + 1, "{posted}");
    }

    fn asking_history(
        c: &Arc<Calls>,
        events: &Arc<Events>,
        before: Option<u64>,
        count: usize,
        limit: Duration,
    ) -> std::thread::JoinHandle<Result<Vec<Entry>, Refusal>> {
        let (c, events) = (Arc::clone(c), Arc::clone(events));
        std::thread::spawn(move || c.history(&events, "dev", &all(), before, count, limit))
    }

    fn entry(n: u64) -> Value {
        json!({"number": "5556", "type": "missed", "date_ms": 100 - n, "duration_s": 0})
    }

    #[test]
    fn a_chunked_history_is_put_back_together_and_only_the_last_chunk_finishes_it() {
        let d = Desk::new("hist");
        let (c, events) = (Arc::new(Calls::default()), Arc::new(Events::default()));
        let mut r = phone(&events, "dev", 7, true);
        let h = asking_history(&c, &events, Some(99), 3, HISTORY_WAIT);
        let ev = next_event(&mut r);
        assert_eq!(ev["event"], HISTORY_EVENT);
        assert_eq!(ev["limit"], 3);
        assert_eq!(ev["before"], 99);
        let id = ev["id"].as_u64().unwrap();
        let first = json!({"cmd": "call_history_reply", "id": id, "calls": [entry(1), entry(2)], "more": true});
        assert_eq!(reply(c.control("dev", "Pixel", 7, Some(&all()), &d.m, first.to_string().as_bytes()))["reply"], "ok");
        // From another connection, it is nobody's answer.
        let stray = json!({"cmd": "call_history_reply", "id": id, "calls": []});
        assert_eq!(reply(c.control("dev", "Pixel", 8, Some(&all()), &d.m, stray.to_string().as_bytes()))["code"], "unknown_id");
        assert_eq!(c.held("dev"), 2);
        let last = json!({"cmd": "call_history_reply", "id": id,
                          "calls": [{"number": "", "name": "A\u{7}nn", "type": "rejected", "date_ms": 5, "duration_s": 0}]});
        c.control("dev", "Pixel", 7, Some(&all()), &d.m, last.to_string().as_bytes());
        let got = h.join().unwrap().expect("answer");
        assert_eq!(got.len(), 3);
        assert_eq!(got[2], Entry { number: String::new(), name: Some("Ann".into()), kind: "rejected".into(), date_ms: 5, duration_s: 0 });
        assert_eq!(c.held("dev"), 0);
    }

    #[test]
    fn a_history_larger_than_asked_or_malformed_is_refused() {
        let d = Desk::new("histbad");
        let (c, events) = (Arc::new(Calls::default()), Arc::new(Events::default()));
        let mut r = phone(&events, "dev", 7, true);
        for (calls, why) in [
            (json!([entry(1), entry(2), entry(3)]), "more calls"),
            (json!([{"number": "5556", "type": "voicemail", "date_ms": 1, "duration_s": 0}]), "type must be"),
            (json!([{"number": "5556", "type": "missed", "date_ms": -1, "duration_s": 0}]), "date_ms"),
        ] {
            let h = asking_history(&c, &events, None, 2, HISTORY_WAIT);
            let id = next_event(&mut r)["id"].as_u64().unwrap();
            let line = json!({"cmd": "call_history_reply", "id": id, "calls": calls});
            let v = reply(c.control("dev", "Pixel", 7, Some(&all()), &d.m, line.to_string().as_bytes()));
            assert_eq!(v["code"], "bad_request");
            match h.join().unwrap() {
                Err(Refusal::BadReply(w)) => assert!(w.contains(why), "{w}"),
                other => panic!("{other:?}"),
            }
        }
        // A done line is not a history answer.
        let h = asking_history(&c, &events, None, 2, HISTORY_WAIT);
        let id = next_event(&mut r)["id"].as_u64().unwrap();
        c.control("dev", "Pixel", 7, Some(&all()), &d.m, json!({"cmd": "call_done", "id": id, "ok": true}).to_string().as_bytes());
        assert!(matches!(h.join().unwrap(), Err(Refusal::BadReply(_))));
        // And the phone's own refusal word reads back, an unknown one as failed.
        for (word, read) in [("no_permission", "no_permission"), ("whatever", "failed")] {
            let h = asking_history(&c, &events, None, 2, HISTORY_WAIT);
            let id = next_event(&mut r)["id"].as_u64().unwrap();
            let line = json!({"cmd": "call_history_reply", "id": id, "error": word});
            c.control("dev", "Pixel", 7, Some(&all()), &d.m, line.to_string().as_bytes());
            assert_eq!(h.join().unwrap(), Err(Refusal::Phone(read.into())));
        }
    }

    #[test]
    fn a_history_times_out_and_a_disconnect_or_revoke_drops_what_came() {
        let d = Desk::new("histend");
        let (c, events) = (Arc::new(Calls::default()), Arc::new(Events::default()));
        let mut r = phone(&events, "dev", 7, true);
        let h = asking_history(&c, &events, None, 5, Duration::from_millis(200));
        next_event(&mut r);
        assert_eq!(h.join().unwrap(), Err(Refusal::Timeout));
        assert_eq!(c.held("dev"), 0);

        let h = asking_history(&c, &events, None, 5, HISTORY_WAIT);
        let id = next_event(&mut r)["id"].as_u64().unwrap();
        let first = json!({"cmd": "call_history_reply", "id": id, "calls": [entry(1)], "more": true});
        c.control("dev", "Pixel", 7, Some(&all()), &d.m, first.to_string().as_bytes());
        assert_eq!(c.held("dev"), 1);
        c.ended("dev", 7, &d.m);
        assert_eq!(c.held("dev"), 0, "gathered calls outlived the connection");
        assert_eq!(h.join().unwrap(), Err(Refusal::Disconnected));

        let h = asking_history(&c, &events, None, 5, HISTORY_WAIT);
        next_event(&mut r);
        // Busy: one history request per phone at a time.
        assert_eq!(c.history(&events, "dev", &all(), None, 5, HISTORY_WAIT), Err(Refusal::Busy));
        c.forget("dev");
        assert_eq!(h.join().unwrap(), Err(Refusal::Revoked));
    }

    fn acting(
        c: &Arc<Calls>,
        events: &Arc<Events>,
        report: PhoneReport,
        action: &'static str,
        number: Option<&'static str>,
    ) -> std::thread::JoinHandle<Result<(), Refusal>> {
        let (c, events) = (Arc::clone(c), Arc::clone(events));
        std::thread::spawn(move || c.act(&events, "dev", &report, action, number, ACTION_WAIT))
    }

    #[test]
    fn answer_decline_and_hangup_go_only_with_a_call_to_act_on() {
        let d = Desk::new("act");
        let (c, events) = (Arc::new(Calls::default()), Arc::new(Events::default()));
        let mut r = phone(&events, "dev", 7, false);
        // Nothing rings: nothing is written.
        for a in ["answer", "decline", "hangup"] {
            assert_eq!(c.act(&events, "dev", &all(), a, None, ACTION_WAIT), Err(Refusal::NoCall));
        }
        quiet(&mut r);
        c.control("dev", "Pixel", 7, Some(&all()), &d.m, &state_line("ringing", Some("5556"), None));
        // A history read waiting does not hold up an answer.
        let hist = asking_history(&c, &events, None, 5, HISTORY_WAIT);
        let hist_id = next_event(&mut r)["id"].as_u64().unwrap();
        // On a companion (locked) lane: answering is allowed, as on the phone's
        // own lock screen.
        let h = acting(&c, &events, all(), "answer", None);
        let ev = next_event(&mut r);
        assert_eq!((ev["event"].as_str(), ev["action"].as_str()), (Some(ACTION_EVENT), Some("answer")));
        assert!(ev.get("number").is_none());
        assert_eq!(c.act(&events, "dev", &all(), "decline", None, ACTION_WAIT), Err(Refusal::Busy));
        let done = json!({"cmd": "call_done", "id": ev["id"], "ok": true});
        assert_eq!(reply(c.control("dev", "Pixel", 7, Some(&all()), &d.m, done.to_string().as_bytes()))["reply"], "ok");
        assert_eq!(h.join().unwrap(), Ok(()));
        c.control("dev", "Pixel", 7, Some(&all()), &d.m, &state_line("active", Some("5556"), None));
        assert_eq!(c.act(&events, "dev", &all(), "answer", None, ACTION_WAIT), Err(Refusal::NoCall));
        let h = acting(&c, &events, all(), "hangup", None);
        let ev = next_event(&mut r);
        assert_eq!(ev["action"], "hangup");
        let done = json!({"cmd": "call_done", "id": ev["id"], "ok": false, "error": "no_call"});
        c.control("dev", "Pixel", 7, Some(&all()), &d.m, done.to_string().as_bytes());
        assert_eq!(h.join().unwrap(), Err(Refusal::Phone("no_call".into())));
        let last = json!({"cmd": "call_history_reply", "id": hist_id, "calls": []});
        c.control("dev", "Pixel", 7, Some(&all()), &d.m, last.to_string().as_bytes());
        assert_eq!(hist.join().unwrap(), Ok(vec![]));
    }

    #[test]
    fn a_companion_session_never_sees_a_dial() {
        let (c, events) = (Arc::new(Calls::default()), Arc::new(Events::default()));
        let mut r = phone(&events, "dev", 7, false);
        assert_eq!(c.act(&events, "dev", &all(), "dial", Some("5556"), ACTION_WAIT), Err(Refusal::Locked));
        quiet(&mut r);
    }

    #[test]
    fn dial_goes_only_when_allowed_idle_and_to_a_number() {
        let d = Desk::new("dial");
        let (c, events) = (Arc::new(Calls::default()), Arc::new(Events::default()));
        let mut r = phone(&events, "dev", 7, true);
        // Not allowed by the phone, or calls off: nothing is written.
        assert_eq!(
            c.act(&events, "dev", &report(&["calls", "calls_answer"]), "dial", Some("5556"), ACTION_WAIT),
            Err(Refusal::Unsupported("dial"))
        );
        assert_eq!(
            c.act(&events, "dev", &report(&["calls_dial"]), "dial", Some("5556"), ACTION_WAIT),
            Err(Refusal::Phone("off".into()))
        );
        assert_eq!(
            c.act(&events, "dev", &report(&["calls", "calls_dial"]), "answer", None, ACTION_WAIT),
            Err(Refusal::Unsupported("answer or end calls"))
        );
        assert!(matches!(c.act(&events, "dev", &all(), "dial", Some("5556;1"), ACTION_WAIT), Err(Refusal::BadReply(_))));
        quiet(&mut r);
        let h = acting(&c, &events, all(), "dial", Some("+1 555 521-5556"));
        let ev = next_event(&mut r);
        assert_eq!((ev["action"].as_str(), ev["number"].as_str()), (Some("dial"), Some("+15555215556")));
        let done = json!({"cmd": "call_done", "id": ev["id"], "ok": false, "error": "emergency"});
        c.control("dev", "Pixel", 7, Some(&all()), &d.m, done.to_string().as_bytes());
        let why = h.join().unwrap().unwrap_err();
        assert!(why.sentence("Pixel").contains("emergency"), "{why:?}");
        // On a call already: no second dial.
        c.control("dev", "Pixel", 7, Some(&all()), &d.m, &state_line("active", Some("5556"), None));
        assert_eq!(c.act(&events, "dev", &all(), "dial", Some("5556"), ACTION_WAIT), Err(Refusal::InCall));
        quiet(&mut r);
    }

    #[test]
    fn an_action_waiting_fails_when_its_connection_ends() {
        let d = Desk::new("actend");
        let (c, events) = (Arc::new(Calls::default()), Arc::new(Events::default()));
        let mut r = phone(&events, "dev", 7, true);
        let h = acting(&c, &events, all(), "dial", Some("5556"));
        next_event(&mut r);
        c.ended("dev", 7, &d.m);
        assert_eq!(h.join().unwrap(), Err(Refusal::Disconnected));
        // No events channel at all: refused before anything waits.
        events.ended("dev", 7);
        assert_eq!(c.act(&events, "dev", &all(), "dial", Some("5556"), ACTION_WAIT), Err(Refusal::NotConnected));
    }

    #[test]
    fn no_refusal_sentence_carries_a_number() {
        for r in [
            Refusal::NotConnected,
            Refusal::Locked,
            Refusal::Unsupported("dial"),
            Refusal::NoCall,
            Refusal::InCall,
            Refusal::Busy,
            Refusal::Disconnected,
            Refusal::Revoked,
            Refusal::Timeout,
            Refusal::Phone("emergency".into()),
            Refusal::Phone("x".into()),
        ] {
            let s = r.sentence("Pixel");
            assert!(s.contains("Pixel") && !s.chars().any(|c| c.is_ascii_digit()), "{s}");
        }
    }
}
