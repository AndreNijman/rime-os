//! The phone's half of the pairing: what it tells this machine about itself,
//! and what this machine may ask of it (ring) or hand it (send).
//!
//! ## Three pieces
//!
//! - `phone_report` (a verb the phone sends, answered here): its battery,
//!   whether it is charging, and what it can do. The last report per device
//!   is kept in `phones.json` beside the device store, so `rime remote
//!   devices` and the Shell can show it after the phone has gone.
//! - `device_events` (a channel the phone opens, read-only): the one way
//!   this machine can say something to a connected phone without being asked.
//!   One end of a socketpair goes to the connection's pump, which already
//!   forwards whatever arrives as `Data` frames; the other end is kept here,
//!   per device, and a ring is a JSON line written into it.
//! - `ring_ack` (a verb the phone sends back): proof the line reached the
//!   app. A write into the socketpair succeeds whether or not anybody is at
//!   the other end of a half-dead link, so "rang" is only ever reported on an
//!   ack. Without one the ring goes through push, and the reply says the
//!   phone was ASKED, not that it rang.
//!
//! - `send` and `take_offer` (contract §2.6): a file or some text somebody
//!   here sent the phone waits in the [`Outbox`]; an `offer` line on the
//!   events channel tells the phone, and the phone takes it on a channel of
//!   its own, by id, under its own device id.
//!
//! ## What a ring may not do
//!
//! Ring is a control-socket request, so only somebody at this machine can
//! send it (`control.rs`'s `authorized`). A paired phone has no verb that
//! rings anything: a `ring` arriving over the link is not one of this
//! module's verbs and goes to `rime-agentd`, which does not know it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Verbs a phone sends that this module answers.
pub const VERBS: [&str; 4] = ["phone_report", "ring_ack", "offer_ack", "offer_done"];

/// The channel a phone opens to hear from this machine.
pub const CHANNEL: &str = "device_events";

/// What `remote_hello` adds, so a phone talks to a machine that has these and
/// leaves an older one alone.
pub const FEATURES: [&str; 3] = ["device_events", "phone_report", TAKE];

/// The channel a phone opens to take what was sent to it.
pub const TAKE: &str = "take_offer";

/// The capability a phone declares when it can ring. Anything else a phone
/// claims is dropped, so the store holds only words this machine acts on.
pub const CAN_RING: &str = "ring";

/// The capability a phone declares when it takes what this machine sends.
pub const CAN_RECEIVE: &str = "receive";

/// The capability a phone declares when it cancels a notification it
/// mirrored here once the person dismisses it here (contract §2.7).
pub const CAN_DISMISS: &str = "dismiss";

/// Every capability word kept from a report.
const KEPT: [&str; 3] = [CAN_RING, CAN_RECEIVE, CAN_DISMISS];

/// The largest file `send` hands a phone: the same cap as `save_file`.
pub const MAX_SEND_BYTES: u64 = 256 * 1024 * 1024;

/// The most text `send` puts on a phone's clipboard: the same cap as
/// `clipboard_write`.
pub const MAX_TEXT_BYTES: usize = 8192;

/// How long something sent waits for its phone.
pub const OFFER_LIFE: Duration = Duration::from_secs(15 * 60);

/// How long a ring sent over the link waits for the phone's ack before it
/// goes through push instead. A round trip over the relay is well under a
/// second; four leaves room for a phone that is busy, and is short enough
/// that the person at the computer is not left wondering.
pub const ACK_WAIT: Duration = Duration::from_secs(4);

/// How many capability words one report may list.
const MAX_CAN: usize = 8;

/// One phone's last report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhoneReport {
    /// Percent, 0 to 100, when the phone said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery: Option<u8>,
    /// Whether it was charging, when the phone said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charging: Option<bool>,
    /// What it can do: [`CAN_RING`] and [`CAN_RECEIVE`] mean something.
    #[serde(default)]
    pub can: Vec<String>,
    /// Unix milliseconds at which this machine received it.
    pub reported_ms: u64,
}

impl PhoneReport {
    pub fn can_ring(&self) -> bool {
        self.can.iter().any(|c| c == CAN_RING)
    }

    pub fn can_receive(&self) -> bool {
        self.can.iter().any(|c| c == CAN_RECEIVE)
    }

    pub fn can_dismiss(&self) -> bool {
        self.can.iter().any(|c| c == CAN_DISMISS)
    }

    /// A report from a `phone_report` line, or why it was refused.
    pub fn parse(v: &Value, now_ms: u64) -> Result<PhoneReport, String> {
        let battery = match &v["battery"] {
            Value::Null => None,
            b => match b.as_u64() {
                Some(n) if n <= 100 => Some(n as u8),
                _ => return Err("battery must be a whole number from 0 to 100".into()),
            },
        };
        let charging = match &v["charging"] {
            Value::Null => None,
            c => Some(c.as_bool().ok_or("charging must be true or false")?),
        };
        let can = match &v["can"] {
            Value::Null => Vec::new(),
            Value::Array(a) if a.len() <= MAX_CAN => {
                let mut can: Vec<String> = Vec::new();
                for c in a {
                    let c = c.as_str().ok_or("can must be a list of words")?;
                    if KEPT.contains(&c) && !can.iter().any(|x| x == c) {
                        can.push(c.to_string());
                    }
                }
                can
            }
            _ => return Err(format!("can must be a list of at most {MAX_CAN} words")),
        };
        Ok(PhoneReport {
            battery,
            charging,
            can,
            reported_ms: now_ms,
        })
    }
}

/// Every phone's last report, as written to `phones.json`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PhoneStore {
    #[serde(default)]
    pub phones: BTreeMap<String, PhoneReport>,
}

impl PhoneStore {
    pub fn load(path: &Path) -> std::io::Result<PhoneStore> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PhoneStore::default()),
            Err(e) => Err(e),
        }
    }

    /// Written whole, 0600, through a rename, like `push.json`.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let dir = path.parent().unwrap_or(Path::new("."));
        rime_agent_core::paths::ensure_private_dir(dir)?;
        let tmp = dir.join(format!(".phones.json.{}", std::process::id()));
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?.as_bytes())?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&tmp, path)
    }
}

/// The open `device_events` channels, one per device, and the acks rings are
/// waiting for.
#[derive(Default)]
pub struct Events {
    /// Device id to (generation, this machine's end of the socketpair).
    writers: Mutex<HashMap<String, (u64, UnixStream)>>,
    generation: AtomicU64,
    ring_id: AtomicU64,
    acked: Mutex<HashSet<u64>>,
    ack: Condvar,
}

impl Events {
    /// A new channel for `device_id`: the end the pump reads, and its
    /// generation. A channel the device already had is replaced, and its
    /// end dropped, so its pump reads end-of-file and closes it.
    pub fn open(&self, device_id: &str) -> std::io::Result<(UnixStream, u64)> {
        let (theirs, ours) = UnixStream::pair()?;
        // A ring is one short line into an empty buffer, so a write that
        // would block means nobody has read the last one for a long time. It
        // fails instead of holding the control socket's thread.
        ours.set_nonblocking(true)?;
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        lock(&self.writers).insert(device_id.to_string(), (generation, ours));
        Ok((theirs, generation))
    }

    /// Whether `device_id` has a channel open, as far as this machine knows.
    pub fn is_open(&self, device_id: &str) -> bool {
        lock(&self.writers).contains_key(device_id)
    }

    /// Write one event line to `device_id`. On a failed write the channel is
    /// forgotten, but only if it is still the one written to: a reconnect
    /// that opened a new channel in between keeps it.
    pub fn send(&self, device_id: &str, line: &Value) -> bool {
        let mut writers = lock(&self.writers);
        let Some((generation, stream)) = writers.get_mut(device_id) else {
            return false;
        };
        let generation = *generation;
        let mut bytes = line.to_string().into_bytes();
        bytes.push(b'\n');
        let ok = stream.write_all(&bytes).is_ok();
        if !ok {
            self.forget_generation(&mut writers, device_id, generation);
        }
        ok
    }

    fn forget_generation(
        &self,
        writers: &mut HashMap<String, (u64, UnixStream)>,
        device_id: &str,
        generation: u64,
    ) {
        if writers.get(device_id).map(|(g, _)| *g) == Some(generation) {
            writers.remove(device_id);
        }
    }

    /// Forget a device's channel, whatever its generation: revoke.
    pub fn forget(&self, device_id: &str) {
        if let Some((_, s)) = lock(&self.writers).remove(device_id) {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }

    /// A fresh id for a ring or an offer. One counter for both, so an ack
    /// names exactly one thing.
    pub fn next_ring(&self) -> u64 {
        self.ring_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// The phone saw ring `id`.
    pub fn acknowledge(&self, id: u64) {
        lock(&self.acked).insert(id);
        self.ack.notify_all();
    }

    /// Wait up to `limit` for ring `id`'s ack.
    pub fn wait_ack(&self, id: u64, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        let mut acked = lock(&self.acked);
        loop {
            if acked.remove(&id) {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            acked = self
                .ack
                .wait_timeout(acked, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// What one ring did, for the person who asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rang {
    /// The app said it is ringing.
    Link,
    /// The push service accepted the request. Whether the phone rings depends
    /// on it being on and online, which nobody here can see.
    Push,
}

/// Why a ring could not be sent at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotRung {
    /// The phone has never said it can ring: an older app, or one that has
    /// not connected since it was updated.
    CannotRing,
    /// Not connected, and nothing to wake it with.
    NoPush,
    /// Push was tried and failed.
    PushFailed(String),
}

impl NotRung {
    pub fn sentence(&self, name: &str) -> String {
        match self {
            NotRung::CannotRing => format!(
                "{name} has not said it can ring. Open an up-to-date Rime Remote on it once, with its notifications allowed, then try again"
            ),
            NotRung::NoPush => format!(
                "{name} is not connected, and it has no push service to wake it. Open Rime Remote on it, \
                 or install a UnifiedPush distributor such as ntfy on it"
            ),
            NotRung::PushFailed(why) => format!("could not reach {name} through its push service: {why}"),
        }
    }
}

/// What this machine knows about reaching a phone other than its channel.
#[derive(Debug, Clone, Copy)]
pub struct Reach<'a> {
    /// Its last `phone_report`, if it ever sent one.
    pub report: Option<&'a PhoneReport>,
    /// Whether it holds a push registration.
    pub has_push: bool,
}

/// Ring `device_id`: over its open channel when the app acks, else through
/// push when the phone can ring and has a registration.
///
/// `push` delivers ONE envelope of [`rime_remote_core::push::Kind::Ring`] to
/// this one device with `started` = `now_s`; passed in so the decision is
/// tested without a push server.
pub fn ring(
    events: &Events,
    device_id: &str,
    machine: &str,
    reach: Reach<'_>,
    now_s: u64,
    ack_wait: Duration,
    push: impl FnOnce(u64) -> Result<(), String>,
) -> Result<Rang, NotRung> {
    if events.is_open(device_id) {
        let id = events.next_ring();
        let line = json!({"event": "ring", "id": id, "at": now_s, "machine": machine});
        if events.send(device_id, &line) && events.wait_ack(id, ack_wait) {
            return Ok(Rang::Link);
        }
    }
    if !reach.report.is_some_and(PhoneReport::can_ring) {
        return Err(NotRung::CannotRing);
    }
    if !reach.has_push {
        return Err(NotRung::NoPush);
    }
    push(now_s).map(|()| Rang::Push).map_err(NotRung::PushFailed)
}

/// Answer `phone_report`, the acks and `offer_done`. `None` for a line this module does
/// not own. The device is the handshake's, never a body field.
pub fn control(state: &crate::state::State, device_id: &str, line: &[u8]) -> Option<Vec<u8>> {
    let v: Value = serde_json::from_slice(line).ok()?;
    let cmd = v.get("cmd")?.as_str()?;
    if !VERBS.contains(&cmd) {
        return None;
    }
    Some(match cmd {
        "phone_report" => match PhoneReport::parse(&v, rime_remote_core::now_ms()) {
            Ok(report) => {
                state.phone_report(device_id, report);
                ok()
            }
            Err(why) => error(&why),
        },
        "ring_ack" | "offer_ack" => match v["id"].as_u64() {
            Some(id) => {
                state.events.acknowledge(id);
                ok()
            }
            None => error("id must be a whole number"),
        },
        // Idempotent: a second `offer_done`, or one for an offer that has
        // expired, has nothing left to drop and is not an error.
        "offer_done" => match v["id"].as_u64() {
            Some(id) => {
                state.outbox.done(device_id, id);
                ok()
            }
            None => error("id must be a whole number"),
        },
        _ => return None,
    })
}

/// Something somebody here sent a phone (contract §2.6).
#[derive(Debug)]
pub enum Outgoing {
    /// A regular file, open, and the name the phone is offered.
    File { file: std::fs::File, name: String, len: u64 },
    /// Text for the phone's clipboard.
    Text(Vec<u8>),
}

impl Outgoing {
    /// A file to send: regular, 1 byte to [`MAX_SEND_BYTES`], its name
    /// reduced as `save_file` reduces one.
    pub fn file(file: std::fs::File, raw_name: &str) -> Result<Outgoing, String> {
        let meta = file.metadata().map_err(|e| format!("could not read that file: {e}"))?;
        if !meta.is_file() {
            return Err("only a regular file can be sent".into());
        }
        let len = meta.len();
        if len == 0 {
            return Err("that file is empty; there is nothing to send".into());
        }
        if len > MAX_SEND_BYTES {
            return Err(format!(
                "that file is {len} bytes, and a phone is sent at most {MAX_SEND_BYTES} ({} MiB)",
                MAX_SEND_BYTES / 1024 / 1024
            ));
        }
        let name = rime_agent_core::filename::file_name(raw_name)?;
        Ok(Outgoing::File { file, name, len })
    }

    /// Text to send: 1 to [`MAX_TEXT_BYTES`] bytes.
    pub fn text(text: String) -> Result<Outgoing, String> {
        if text.is_empty() {
            return Err("there is no text to send".into());
        }
        if text.len() > MAX_TEXT_BYTES {
            return Err(format!(
                "that text is {} bytes, and a phone's clipboard is sent at most {MAX_TEXT_BYTES}",
                text.len()
            ));
        }
        Ok(Outgoing::Text(text.into_bytes()))
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Outgoing::File { .. } => "file",
            Outgoing::Text(_) => "text",
        }
    }

    pub fn len(&self) -> u64 {
        match self {
            Outgoing::File { len, .. } => *len,
            Outgoing::Text(t) => t.len() as u64,
        }
    }

    /// The offered name; empty for text.
    pub fn name(&self) -> &str {
        match self {
            Outgoing::File { name, .. } => name,
            Outgoing::Text(_) => "",
        }
    }

    /// Write every byte, from the start, into `out`. Reads the file by
    /// offset, so a second take (after a dropped link) does not share a
    /// position with the first.
    pub fn write_to(&self, out: &mut impl Write) -> std::io::Result<()> {
        match self {
            Outgoing::Text(t) => out.write_all(t),
            Outgoing::File { file, len, .. } => {
                let mut buf = vec![0u8; 64 * 1024];
                let mut at = 0u64;
                while at < *len {
                    let want = buf.len().min((*len - at) as usize);
                    let n = file.read_at(&mut buf[..want], at)?;
                    if n == 0 {
                        // Shorter than it was when it was sent: the phone
                        // sees a channel that closed early and keeps nothing.
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "the file got shorter after it was sent",
                        ));
                    }
                    out.write_all(&buf[..n])?;
                    at += n as u64;
                }
                Ok(())
            }
        }
    }
}

/// The `offer` event for something waiting.
pub fn offer_line(id: u64, at_s: u64, machine: &str, body: &Outgoing) -> Value {
    json!({
        "event": "offer",
        "id": id,
        "at": at_s,
        "machine": machine,
        "kind": body.kind(),
        "name": body.name(),
        "len": body.len(),
    })
}

struct Offer {
    id: u64,
    device_id: String,
    at_s: u64,
    expires: Instant,
    body: Arc<Outgoing>,
}

/// What was sent and not yet taken, in memory, oldest first.
#[derive(Default)]
pub struct Outbox {
    offers: Mutex<Vec<Offer>>,
}

impl Outbox {
    fn live(offers: &mut Vec<Offer>) {
        let now = Instant::now();
        offers.retain(|o| o.expires > now);
    }

    pub fn put(&self, id: u64, device_id: &str, at_s: u64, body: Outgoing, life: Duration) {
        let mut offers = lock(&self.offers);
        Self::live(&mut offers);
        offers.push(Offer {
            id,
            device_id: device_id.to_string(),
            at_s,
            expires: Instant::now() + life,
            body: Arc::new(body),
        });
    }

    /// The `offer` line of everything still waiting for `device_id`.
    pub fn pending(&self, device_id: &str, machine: &str) -> Vec<Value> {
        let mut offers = lock(&self.offers);
        Self::live(&mut offers);
        offers
            .iter()
            .filter(|o| o.device_id == device_id)
            .map(|o| offer_line(o.id, o.at_s, machine, &o.body))
            .collect()
    }

    /// Offer `id`, if it is still waiting and was sent to `device_id`.
    pub fn take(&self, device_id: &str, id: u64) -> Option<Arc<Outgoing>> {
        let mut offers = lock(&self.offers);
        Self::live(&mut offers);
        offers
            .iter()
            .find(|o| o.id == id && o.device_id == device_id)
            .map(|o| Arc::clone(&o.body))
    }

    /// The phone has it: drop it. Only `device_id`'s own.
    pub fn done(&self, device_id: &str, id: u64) {
        lock(&self.offers).retain(|o| !(o.id == id && o.device_id == device_id));
    }

    /// Drop everything waiting for `device_id`: revoke.
    pub fn forget(&self, device_id: &str) {
        lock(&self.offers).retain(|o| o.device_id != device_id);
    }
}

/// How something sent went out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SentPath {
    /// The app said it is taking it.
    Link,
    /// The push service accepted the request to tell the phone.
    Push,
    /// Not connected and not reachable by push: it waits, and is offered when
    /// the phone next opens its events channel. `push` is why push failed,
    /// when it was tried.
    Waiting { push: Option<String> },
}

/// Why nothing was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotSent {
    /// The phone has never said it takes what is sent.
    CannotReceive,
}

impl NotSent {
    pub fn sentence(&self, name: &str) -> String {
        match self {
            NotSent::CannotReceive => format!(
                "{name} has not said it can receive files or text. Open an up-to-date Rime Remote on it once, \
                 with its notifications allowed, then try again"
            ),
        }
    }
}

/// Send `body` to `device_id`: an offer on its open channel when the app
/// acks, else a push asking the person to open the app, else it waits. It
/// waits in every case until the phone says `offer_done` or
/// [`OFFER_LIFE`] passes. Returns the path and the offer's id.
#[allow(clippy::too_many_arguments)]
pub fn send(
    events: &Events,
    outbox: &Outbox,
    device_id: &str,
    machine: &str,
    reach: Reach<'_>,
    body: Outgoing,
    now_s: u64,
    ack_wait: Duration,
    push: impl FnOnce(u64) -> Result<(), String>,
) -> Result<(SentPath, u64), NotSent> {
    // Asked first, channel or not: an older app drops an `offer` line it does
    // not know, and the person would be told it was delivered.
    if !reach.report.is_some_and(PhoneReport::can_receive) {
        return Err(NotSent::CannotReceive);
    }
    let id = events.next_ring();
    let line = offer_line(id, now_s, machine, &body);
    outbox.put(id, device_id, now_s, body, OFFER_LIFE);
    if events.is_open(device_id) && events.send(device_id, &line) && events.wait_ack(id, ack_wait) {
        return Ok((SentPath::Link, id));
    }
    if !reach.has_push {
        return Ok((SentPath::Waiting { push: None }, id));
    }
    match push(now_s) {
        Ok(()) => Ok((SentPath::Push, id)),
        Err(why) => Ok((SentPath::Waiting { push: Some(why) }, id)),
    }
}

/// Whether a channel request is `take_offer`.
pub fn is_take(request_line: &[u8]) -> bool {
    serde_json::from_slice::<Value>(request_line)
        .ok()
        .and_then(|v| v["cmd"].as_str().map(|c| c == TAKE))
        .unwrap_or(false)
}

/// Open a `take_offer` channel: the end the pump reads, and the reply. The
/// bytes are written into the other end by a thread of their own, which ends
/// when they are all out or the channel closes. `Err` is the reply that
/// refuses it.
pub fn open_take(outbox: &Outbox, device_id: &str, request_line: &[u8]) -> Result<(UnixStream, Vec<u8>), Vec<u8>> {
    let id = serde_json::from_slice::<Value>(request_line)
        .ok()
        .and_then(|v| v["id"].as_u64())
        .ok_or_else(|| error("id must be a whole number"))?;
    let body = outbox.take(device_id, id).ok_or_else(|| {
        json!({"reply": "error", "code": "not_found",
               "message": format!("nothing is waiting under id {id}: it was taken, it expired, or it was never sent to this phone")})
            .to_string()
            .into_bytes()
    })?;
    let (theirs, mut ours) = UnixStream::pair().map_err(|e| {
        json!({"reply": "error", "code": "unavailable", "message": e.to_string()})
            .to_string()
            .into_bytes()
    })?;
    let reply = json!({"reply": "offer", "kind": body.kind(), "name": body.name(), "len": body.len()})
        .to_string()
        .into_bytes();
    std::thread::spawn(move || {
        // A failed write is the channel closing under it: nothing to report
        // to anybody, and dropping `ours` ends the pump.
        let _ = body.write_to(&mut ours);
    });
    Ok((theirs, reply))
}

/// Whether a channel request is `device_events`.
pub fn is_channel(request_line: &[u8]) -> bool {
    serde_json::from_slice::<Value>(request_line)
        .ok()
        .and_then(|v| v["cmd"].as_str().map(|c| c == CHANNEL))
        .unwrap_or(false)
}

/// The reply that opens a `device_events` channel.
pub fn opened_reply() -> Vec<u8> {
    json!({"reply": "events"}).to_string().into_bytes()
}

fn ok() -> Vec<u8> {
    json!({"reply": "ok"}).to_string().into_bytes()
}

fn error(message: &str) -> Vec<u8> {
    json!({"reply": "error", "code": "bad_request", "message": message})
        .to_string()
        .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    fn report(can: &[&str]) -> PhoneReport {
        PhoneReport {
            battery: Some(40),
            charging: Some(false),
            can: can.iter().map(|s| s.to_string()).collect(),
            reported_ms: 1,
        }
    }

    #[test]
    fn a_report_keeps_only_what_this_machine_acts_on() {
        let r = PhoneReport::parse(&json!({"battery": 62, "charging": true, "can": ["ring", "fly", "receive", "ring"]}), 5)
            .expect("parse");
        assert_eq!(r.battery, Some(62));
        assert_eq!(r.charging, Some(true));
        assert_eq!(r.can, vec!["ring".to_string(), "receive".to_string()]);
        assert!(r.can_ring() && r.can_receive());
        assert_eq!(r.reported_ms, 5);
        let empty = PhoneReport::parse(&json!({}), 5).expect("an empty report is a report");
        assert_eq!((empty.battery, empty.charging, empty.can.len()), (None, None, 0));
        for bad in [
            json!({"battery": 101}),
            json!({"battery": -1}),
            json!({"battery": "62"}),
            json!({"charging": "yes"}),
            json!({"can": "ring"}),
            json!({"can": [1]}),
            json!({"can": ["a","b","c","d","e","f","g","h","i"]}),
        ] {
            assert!(PhoneReport::parse(&bad, 5).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn a_ring_over_the_link_counts_only_once_the_phone_acks_it() {
        let events = std::sync::Arc::new(Events::default());
        let (theirs, _) = events.open("dev-a").expect("open");
        let phone = std::sync::Arc::clone(&events);
        // The phone: read the line, ack its id.
        let t = std::thread::spawn(move || {
            let mut line = String::new();
            BufReader::new(theirs).read_line(&mut line).expect("read");
            let v: Value = serde_json::from_str(&line).expect("json");
            assert_eq!(v["event"], "ring");
            assert_eq!(v["at"], 1000);
            assert_eq!(v["machine"], "l16");
            phone.acknowledge(v["id"].as_u64().expect("id"));
        });
        let pushed = std::cell::Cell::new(false);
        let got = ring(&events, "dev-a", "l16", Reach { report: None, has_push: true }, 1000, ACK_WAIT, |_| {
            pushed.set(true);
            Ok(())
        });
        t.join().expect("phone");
        assert_eq!(got, Ok(Rang::Link));
        assert!(!pushed.get(), "an acked ring must not also go through push");
    }

    #[test]
    fn a_ring_nobody_acks_goes_through_push_with_the_same_time() {
        let events = Events::default();
        // The channel is open and the write succeeds, but nobody reads it:
        // the half-dead link this ack exists for.
        let (_theirs, _) = events.open("dev-a").expect("open");
        let at = std::cell::Cell::new(0);
        let got = ring(&events, "dev-a", "l16", Reach { report: Some(&report(&["ring"])), has_push: true }, 1234, Duration::from_millis(50), |s| {
            at.set(s);
            Ok(())
        });
        assert_eq!(got, Ok(Rang::Push));
        assert_eq!(at.get(), 1234, "push must carry the moment the ring was asked for");
    }

    #[test]
    fn push_is_refused_to_a_phone_that_never_said_it_can_ring_or_cannot_be_woken() {
        let events = Events::default();
        let never = |_| -> Result<(), String> { panic!("must not push") };
        assert_eq!(ring(&events, "d", "l16", Reach { report: None, has_push: true }, 1, ACK_WAIT, never), Err(NotRung::CannotRing));
        assert_eq!(ring(&events, "d", "l16", Reach { report: Some(&report(&[])), has_push: true }, 1, ACK_WAIT, never), Err(NotRung::CannotRing));
        assert_eq!(ring(&events, "d", "l16", Reach { report: Some(&report(&["ring"])), has_push: false }, 1, ACK_WAIT, never), Err(NotRung::NoPush));
        assert_eq!(
            ring(&events, "d", "l16", Reach { report: Some(&report(&["ring"])), has_push: true }, 1, ACK_WAIT, |_| Err("503".into())),
            Err(NotRung::PushFailed("503".into()))
        );
        assert!(NotRung::CannotRing.sentence("Pixel").contains("notifications allowed"));
        assert!(NotRung::NoPush.sentence("Pixel").contains("ntfy"));
    }

    #[test]
    fn a_closed_channel_is_forgotten_but_a_newer_one_is_kept() {
        let events = Events::default();
        let (old, g1) = events.open("dev-a").expect("open");
        drop(old);
        assert!(!events.send("dev-a", &json!({"event": "x"})), "a write to a dropped reader must fail");
        assert!(!events.is_open("dev-a"), "the dead channel stays listed");
        // A reconnect: the new channel replaces the old; a stale failure for
        // the old generation does not remove it.
        let (_new, g2) = events.open("dev-a").expect("open");
        assert!(g2 > g1);
        {
            let mut w = lock(&events.writers);
            events.forget_generation(&mut w, "dev-a", g1);
        }
        assert!(events.is_open("dev-a"), "the old connection's teardown removed the new channel");
        events.forget("dev-a");
        assert!(!events.is_open("dev-a"));
    }

    #[test]
    fn the_store_round_trips_and_is_private() {
        let dir = std::env::temp_dir().join(format!("rime-remoted-phones-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("remote").join("phones.json");
        assert!(PhoneStore::load(&path).expect("missing is empty").phones.is_empty());
        let mut s = PhoneStore::default();
        s.phones.insert("dev-a".into(), report(&["ring"]));
        s.save(&path).expect("save");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777, 0o600);
        assert_eq!(PhoneStore::load(&path).expect("load").phones["dev-a"], report(&["ring"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A scratch file holding `bytes`, opened for reading, already unlinked.
    fn scratch(bytes: &[u8]) -> std::fs::File {
        let path = std::env::temp_dir().join(format!(
            "phone-send-{}-{:?}-{}",
            std::process::id(),
            std::thread::current().id(),
            bytes.len()
        ));
        std::fs::write(&path, bytes).expect("write");
        let f = std::fs::File::open(&path).expect("open");
        std::fs::remove_file(&path).expect("unlink");
        f
    }

    fn read_all(mut s: UnixStream) -> Vec<u8> {
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut s, &mut out).expect("read");
        out
    }

    #[test]
    fn what_can_be_sent_is_a_nonempty_regular_file_or_text_within_the_caps() {
        let ok = Outgoing::file(scratch(b"pdf"), "../../.report.pdf").expect("a small file");
        assert_eq!((ok.kind(), ok.name(), ok.len()), ("file", "report.pdf", 3));
        assert!(Outgoing::file(scratch(b""), "empty.txt").unwrap_err().contains("empty"));
        let dir = std::fs::File::open(std::env::temp_dir()).expect("open dir");
        assert!(Outgoing::file(dir, "dir").unwrap_err().contains("regular file"));
        // One byte over the cap, sparse, so the test writes nothing.
        let path = std::env::temp_dir().join(format!("phone-send-big-{}", std::process::id()));
        let w = std::fs::File::create(&path).expect("create");
        w.set_len(MAX_SEND_BYTES + 1).expect("sparse");
        let r = std::fs::File::open(&path).expect("open");
        std::fs::remove_file(&path).expect("unlink");
        assert!(Outgoing::file(r, "big.bin").unwrap_err().contains("at most"));
        assert!(Outgoing::file(scratch(b"x"), "a\nb").is_err(), "a control character in a name is refused");

        let t = Outgoing::text("hello".into()).expect("text");
        assert_eq!((t.kind(), t.name(), t.len()), ("text", "", 5));
        assert!(Outgoing::text(String::new()).is_err());
        assert!(Outgoing::text("x".repeat(MAX_TEXT_BYTES)).is_ok());
        assert!(Outgoing::text("x".repeat(MAX_TEXT_BYTES + 1)).is_err());
    }

    #[test]
    fn nothing_is_sent_to_a_phone_that_never_said_it_receives_even_with_its_channel_open() {
        let events = Events::default();
        let outbox = Outbox::default();
        let (_theirs, _) = events.open("dev-a").expect("open");
        let never = |_| -> Result<(), String> { panic!("must not push") };
        for reach in [Reach { report: None, has_push: true }, Reach { report: Some(&report(&["ring"])), has_push: true }] {
            let got = send(&events, &outbox, "dev-a", "l16", reach, Outgoing::text("x".into()).unwrap(), 1, ACK_WAIT, never);
            assert_eq!(got, Err(NotSent::CannotReceive));
        }
        assert!(outbox.pending("dev-a", "l16").is_empty(), "a refused send must leave nothing waiting");
        assert!(NotSent::CannotReceive.sentence("Pixel 8").contains("Pixel 8"));
    }

    #[test]
    fn a_send_over_the_link_counts_once_the_phone_acks_and_waits_until_done() {
        let events = Arc::new(Events::default());
        let outbox = Outbox::default();
        let (theirs, _) = events.open("dev-a").expect("open");
        let phone = Arc::clone(&events);
        let t = std::thread::spawn(move || {
            let mut line = String::new();
            BufReader::new(theirs).read_line(&mut line).expect("read");
            let v: Value = serde_json::from_str(&line).expect("json");
            phone.acknowledge(v["id"].as_u64().expect("id"));
            v
        });
        let receives = report(&["receive"]);
        let got = send(&events, &outbox, "dev-a", "l16", Reach { report: Some(&receives), has_push: true },
            Outgoing::file(scratch(b"0123456789"), "notes.txt").unwrap(), 777, ACK_WAIT,
            |_| -> Result<(), String> { panic!("an acked send must not push") });
        let v = t.join().expect("phone");
        let (path, id) = got.expect("sent");
        assert_eq!(path, SentPath::Link);
        assert_eq!(v, json!({"event":"offer","id":id,"at":777,"machine":"l16","kind":"file","name":"notes.txt","len":10}));
        // Waiting until the phone says it has it, and only for this phone.
        assert_eq!(outbox.pending("dev-a", "l16"), vec![v]);
        assert!(outbox.pending("dev-b", "l16").is_empty());
        assert!(outbox.take("dev-b", id).is_none(), "another phone took an offer that was not sent to it");
        outbox.done("dev-b", id);
        assert!(outbox.take("dev-a", id).is_some(), "another phone's done dropped this one's offer");
        outbox.done("dev-a", id);
        assert!(outbox.take("dev-a", id).is_none());
    }

    #[test]
    fn with_no_ack_a_send_pushes_and_otherwise_waits_and_says_why() {
        let events = Events::default();
        let outbox = Outbox::default();
        let receives = report(&["receive"]);
        let at = std::cell::Cell::new(0);
        let got = send(&events, &outbox, "dev-a", "l16", Reach { report: Some(&receives), has_push: true },
            Outgoing::text("a".into()).unwrap(), 55, Duration::from_millis(20), |s| { at.set(s); Ok(()) });
        assert_eq!(got.expect("sent").0, SentPath::Push);
        assert_eq!(at.get(), 55, "push carries the moment it was sent");
        let got = send(&events, &outbox, "dev-a", "l16", Reach { report: Some(&receives), has_push: true },
            Outgoing::text("b".into()).unwrap(), 56, Duration::from_millis(20), |_| Err("503".into()));
        assert_eq!(got.expect("sent").0, SentPath::Waiting { push: Some("503".into()) });
        let got = send(&events, &outbox, "dev-a", "l16", Reach { report: Some(&receives), has_push: false },
            Outgoing::text("c".into()).unwrap(), 57, Duration::from_millis(20), |_| -> Result<(), String> { panic!("no push to try") });
        assert_eq!(got.expect("sent").0, SentPath::Waiting { push: None });
        // All three wait, oldest first, for the next time the phone opens its channel.
        let at: Vec<u64> = outbox.pending("dev-a", "l16").iter().map(|v| v["at"].as_u64().unwrap()).collect();
        assert_eq!(at, vec![55, 56, 57]);
    }

    #[test]
    fn an_offer_expires_and_revoke_drops_what_was_waiting() {
        let outbox = Outbox::default();
        outbox.put(1, "dev-a", 1, Outgoing::text("x".into()).unwrap(), Duration::ZERO);
        assert!(outbox.pending("dev-a", "l16").is_empty(), "an expired offer was offered");
        assert!(outbox.take("dev-a", 1).is_none());
        outbox.put(2, "dev-a", 1, Outgoing::text("x".into()).unwrap(), OFFER_LIFE);
        outbox.put(3, "dev-b", 1, Outgoing::text("x".into()).unwrap(), OFFER_LIFE);
        outbox.forget("dev-a");
        assert!(outbox.pending("dev-a", "l16").is_empty());
        assert_eq!(outbox.pending("dev-b", "l16").len(), 1);
    }

    #[test]
    fn take_offer_streams_every_byte_from_the_start_each_time_and_only_to_its_phone() {
        let outbox = Outbox::default();
        let body: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        outbox.put(9, "dev-a", 1, Outgoing::file(scratch(&body), "data.bin").unwrap(), OFFER_LIFE);
        for _ in 0..2 {
            let (stream, reply) = open_take(&outbox, "dev-a", br#"{"cmd":"take_offer","id":9}"#).expect("take");
            let r: Value = serde_json::from_slice(&reply).expect("reply");
            assert_eq!(r, json!({"reply":"offer","kind":"file","name":"data.bin","len":200000}));
            assert_eq!(read_all(stream), body, "a take must carry the whole file from byte 0");
        }
        let refused = open_take(&outbox, "dev-b", br#"{"cmd":"take_offer","id":9}"#).expect_err("another phone");
        let r: Value = serde_json::from_slice(&refused).expect("reply");
        assert_eq!(r["code"], "not_found");
        assert!(open_take(&outbox, "dev-a", br#"{"cmd":"take_offer"}"#).is_err());
        assert!(is_take(br#"{"cmd":"take_offer","id":9}"#) && !is_take(br#"{"cmd":"device_events"}"#));

        outbox.put(10, "dev-a", 1, Outgoing::text("clip".into()).unwrap(), OFFER_LIFE);
        let (stream, reply) = open_take(&outbox, "dev-a", br#"{"cmd":"take_offer","id":10}"#).expect("take text");
        assert_eq!(serde_json::from_slice::<Value>(&reply).unwrap()["kind"], "text");
        assert_eq!(read_all(stream), b"clip");
    }

}
