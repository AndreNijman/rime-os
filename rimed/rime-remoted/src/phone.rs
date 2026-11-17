//! The phone's half of the pairing: what it tells this machine about itself,
//! and the one thing this machine may ask of it (ring).
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
//! ## What a ring may not do
//!
//! Ring is a control-socket request, so only somebody at this machine can
//! send it (`control.rs`'s `authorized`). A paired phone has no verb that
//! rings anything: a `ring` arriving over the link is not one of this
//! module's verbs and goes to `rime-agentd`, which does not know it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Verbs a phone sends that this module answers.
pub const VERBS: [&str; 2] = ["phone_report", "ring_ack"];

/// The channel a phone opens to hear from this machine.
pub const CHANNEL: &str = "device_events";

/// What `remote_hello` adds, so a phone talks to a machine that has these and
/// leaves an older one alone.
pub const FEATURES: [&str; 2] = ["device_events", "phone_report"];

/// The capability a phone declares when it can ring. Anything else a phone
/// claims is dropped, so the store holds only words this machine acts on.
pub const CAN_RING: &str = "ring";

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
    /// What it can do. Only [`CAN_RING`] means anything today.
    #[serde(default)]
    pub can: Vec<String>,
    /// Unix milliseconds at which this machine received it.
    pub reported_ms: u64,
}

impl PhoneReport {
    pub fn can_ring(&self) -> bool {
        self.can.iter().any(|c| c == CAN_RING)
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
                    if c == CAN_RING && !can.iter().any(|x| x == c) {
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

    /// A fresh ring id.
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
                "{name} has not said it can ring. Update Rime Remote on it and open it once, then try again"
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

/// Answer `phone_report` and `ring_ack`. `None` for a line this module does
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
        "ring_ack" => match v["id"].as_u64() {
            Some(id) => {
                state.events.acknowledge(id);
                ok()
            }
            None => error("id must be a whole number"),
        },
        _ => return None,
    })
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
        let r = PhoneReport::parse(&json!({"battery": 62, "charging": true, "can": ["ring", "fly", "ring"]}), 5)
            .expect("parse");
        assert_eq!(r.battery, Some(62));
        assert_eq!(r.charging, Some(true));
        assert_eq!(r.can, vec!["ring".to_string()]);
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
        assert!(NotRung::CannotRing.sentence("Pixel").contains("Update Rime Remote"));
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
}
