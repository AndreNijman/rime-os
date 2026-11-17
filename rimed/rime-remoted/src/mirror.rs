//! A paired phone's notifications, shown on this machine's desktop
//! (`docs/remote-live-contract.md` §2.7).
//!
//! The phone sends `phone_notification` for a notification from an app the
//! person switched on in Rime Remote, and `phone_notification_removed` when it
//! goes away there. Both are answered here and never forwarded.
//!
//! ## The text is somebody else's
//!
//! What arrives was written by whoever sent the phone a message, so it is
//! treated as hostile before it reaches the notification server:
//!
//! - control characters, and the bidi controls that can make a line read
//!   backwards, are removed; app and title lose their newlines;
//! - each field is capped ([`APP_CAP`], [`TITLE_CAP`], [`TEXT_CAP`] characters);
//! - `<` and `>` become `‹` and `›` in every field, so no field can form a
//!   tag. Rime Shell renders a body as Qt `StyledText` (which loads `<img>`
//!   and follows `<a>`) and a summary as `AutoText` (rich text when it looks
//!   like markup); entity-escaping a summary would show a literal `&lt;`, so
//!   the tags are made impossible instead. The body also has `&` escaped,
//!   because `StyledText` reads entities;
//! - the app name is always `<phone> · <app>`, so it cannot pose as an app on
//!   this machine, and it is posted with a fixed icon, no actions, no sound,
//!   no image and normal urgency;
//! - a phone gets [`BURST`] notifications at once and one more every
//!   [`REFILL`]; past that they are dropped, not queued.
//!
//! ## Posting never holds the link
//!
//! The control worker answers a connection's requests in order, so a stalled
//! session bus must not stall the phone. Requests go to one worker thread
//! through a bounded queue; a full queue drops the notification. The worker
//! posts with `busctl --user` (`org.freedesktop.Notifications.Notify`), the
//! same tool `machine.rs` reads rimed with, and keeps the id the server
//! returns per `(device, key)`, so an update replaces its notification and a
//! removal closes it.
//!
//! ## Tests never reach the real desktop
//!
//! [`STUB_ENV`] points the worker at a directory: each post and close is a
//! JSON line in `<dir>/notifications.jsonl`. A unit-test build never gets the
//! real backend from [`mirror`], whatever the environment says.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// The verbs this module answers. `serve::answered_here` routes by this list.
pub const VERBS: [&str; 2] = ["phone_notification", "phone_notification_removed"];

/// What `remote_hello` gains.
pub const FEATURES: [&str; 1] = ["phone_notifications"];

/// The environment variable that selects [`Stub`].
pub const STUB_ENV: &str = "RIME_REMOTED_NOTIFY_STUB";

/// The longest app name, title and text shown, in characters.
pub const APP_CAP: usize = 60;
pub const TITLE_CAP: usize = 120;
pub const TEXT_CAP: usize = 1000;

/// The most newlines a body keeps; the rest become spaces.
pub const TEXT_LINES: usize = 6;

/// A notification's key: the phone's own hash of Android's key, never the
/// key itself (which names the app and its tag).
pub const KEY_CAP: usize = 64;

/// Notifications a phone may post at once, and how often one more is allowed.
pub const BURST: u32 = 10;
pub const REFILL: Duration = Duration::from_secs(3);

/// Ids kept per phone, newest last; the oldest is forgotten past this.
pub const KEPT_IDS: usize = 64;

/// Requests waiting for the worker; a full queue drops the newest.
pub const QUEUE: usize = 32;

/// What one post or close may spend on the bus.
pub const POST_BUDGET: Duration = Duration::from_secs(3);

/// The icon every mirrored notification carries.
pub const ICON: &str = "phone";

// ── Text ────────────────────────────────────────────────────────────────────

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// One field, made safe to hand a notification server. `lines` is how many
/// newlines may stay (0 for a one-line field). `body` also escapes `&`.
pub fn clean(raw: &str, cap: usize, lines: usize, body: bool) -> String {
    let mut out = String::new();
    let mut count = 0;
    let mut kept_lines = 0;
    let mut space = false;
    for c in raw.chars() {
        let c = match c {
            '\n' | '\r' if kept_lines < lines && c == '\n' => {
                kept_lines += 1;
                '\n'
            }
            '\n' | '\r' | '\t' => ' ',
            c if c.is_control() || is_bidi_control(c) || c == '\u{2028}' || c == '\u{2029}' => continue,
            '<' => '‹',
            '>' => '›',
            c => c,
        };
        // Runs of spaces collapse: a wall of blanks is not text.
        if c == ' ' {
            if space {
                continue;
            }
            space = true;
        } else {
            space = false;
        }
        if count == cap {
            out = out.trim_end().to_string();
            out.push('…');
            return out;
        }
        count += 1;
        if body && c == '&' {
            out.push_str("&amp;");
        } else {
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// A key the phone sent, or why it is refused.
fn key(v: &Value) -> Result<String, String> {
    let k = v["key"].as_str().ok_or("key must be a string")?;
    if k.is_empty() || k.len() > KEY_CAP || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(format!("key must be 1 to {KEY_CAP} letters, digits, - or _"));
    }
    Ok(k.to_string())
}

fn field<'a>(v: &'a Value, name: &str) -> Result<&'a str, String> {
    match &v[name] {
        Value::Null => Ok(""),
        Value::String(s) => Ok(s),
        _ => Err(format!("{name} must be a string")),
    }
}

/// A notification as it will be posted.
#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    pub app_name: String,
    pub summary: String,
    pub body: String,
}

/// Turn a `phone_notification` body into what is posted, or refuse it.
pub fn shown(phone: &str, v: &Value) -> Result<(String, Shown), String> {
    let key = key(v)?;
    let app = clean(field(v, "app")?, APP_CAP, 0, false);
    let title = clean(field(v, "title")?, TITLE_CAP, 0, false);
    let text = clean(field(v, "text")?, TEXT_CAP, TEXT_LINES, true);
    if title.is_empty() && text.is_empty() {
        return Err("a notification needs a title or some text".into());
    }
    let phone = clean(phone, APP_CAP, 0, false);
    let phone = if phone.is_empty() { "Phone".to_string() } else { phone };
    let app_name = if app.is_empty() { phone } else { format!("{phone} · {app}") };
    let summary = if title.is_empty() { app.clone() } else { title };
    Ok((key, Shown { app_name, summary, body: text }))
}

// ── Rate ────────────────────────────────────────────────────────────────────

/// [`BURST`] at once, one more every [`REFILL`].
#[derive(Debug)]
pub struct Bucket {
    tokens: u32,
    at: Instant,
}

impl Bucket {
    pub fn new(now: Instant) -> Bucket {
        Bucket { tokens: BURST, at: now }
    }

    pub fn take(&mut self, now: Instant) -> bool {
        let earned = (now.saturating_duration_since(self.at).as_millis() / REFILL.as_millis()) as u32;
        if earned > 0 {
            self.tokens = (self.tokens + earned).min(BURST);
            self.at += REFILL * earned;
            if self.tokens == BURST {
                self.at = now;
            }
        }
        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
    }
}

// ── Backends ────────────────────────────────────────────────────────────────

/// What the worker asks of the desktop.
pub trait Backend: Send + Sync {
    /// Post, replacing `replaces` when it is not 0. Returns the server's id.
    fn notify(&self, shown: &Shown, replaces: u32) -> Result<u32, String>;
    fn close(&self, id: u32) -> Result<(), String>;
    /// Who answers for `org.freedesktop.Notifications` now, if anyone.
    fn owner(&self) -> Option<String>;
}

/// The desktop's notification server, through `busctl --user`.
pub struct Real;

impl Real {
    fn call(args: &[&str]) -> Result<String, String> {
        let mut all = vec!["--user", "--json=short", "--", "call", "org.freedesktop.Notifications",
                           "/org/freedesktop/Notifications", "org.freedesktop.Notifications"];
        all.extend_from_slice(args);
        match crate::machine::run_until("busctl", &all, Instant::now() + POST_BUDGET) {
            Some((Some(0), out, _)) => Ok(out),
            Some((_, _, err)) => Err(err.trim().lines().last().unwrap_or("busctl failed").to_string()),
            None => Err("the notification server did not answer in time".into()),
        }
    }
}

impl Backend for Real {
    fn notify(&self, s: &Shown, replaces: u32) -> Result<u32, String> {
        let replaces = replaces.to_string();
        let out = Real::call(&[
            "Notify", "susssasa{sv}i", &s.app_name, &replaces, ICON, &s.summary, &s.body,
            "0", "1", "urgency", "y", "1", "-1",
        ])?;
        let v: Value = serde_json::from_str(out.trim()).map_err(|e| format!("busctl printed {out:?}: {e}"))?;
        v["data"][0].as_u64().map(|id| id as u32).ok_or_else(|| format!("busctl printed no id: {out}"))
    }

    fn close(&self, id: u32) -> Result<(), String> {
        Real::call(&["CloseNotification", "u", &id.to_string()]).map(|_| ())
    }

    fn owner(&self) -> Option<String> {
        let args = ["--user", "--json=short", "--", "call", "org.freedesktop.DBus", "/org/freedesktop/DBus",
                    "org.freedesktop.DBus", "GetNameOwner", "s", "org.freedesktop.Notifications"];
        match crate::machine::run_until("busctl", &args, Instant::now() + POST_BUDGET) {
            Some((Some(0), out, _)) => serde_json::from_str::<Value>(out.trim())
                .ok()
                .and_then(|v| v["data"][0].as_str().map(str::to_string)),
            _ => None,
        }
    }
}

/// A directory standing in for the desktop.
pub struct Stub {
    dir: PathBuf,
    next: Mutex<u32>,
}

impl Stub {
    pub fn new(dir: PathBuf) -> Stub {
        Stub { dir, next: Mutex::new(0) }
    }

    fn record(&self, line: Value) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("notifications.jsonl"))
            .map_err(|e| e.to_string())?;
        // One write, so a reader never sees half a line.
        f.write_all(format!("{line}\n").as_bytes()).map_err(|e| e.to_string())
    }
}

impl Backend for Stub {
    fn notify(&self, s: &Shown, replaces: u32) -> Result<u32, String> {
        let id = if replaces != 0 {
            replaces
        } else {
            let mut n = self.next.lock().unwrap_or_else(|e| e.into_inner());
            *n += 1;
            *n
        };
        self.record(json!({"op": "notify", "id": id, "replaces": replaces, "app": s.app_name,
                           "icon": ICON, "summary": s.summary, "body": s.body}))?;
        Ok(id)
    }

    fn close(&self, id: u32) -> Result<(), String> {
        self.record(json!({"op": "close", "id": id}))
    }

    /// `<dir>/owner` when a test wrote one (a restarted server), else "stub".
    fn owner(&self) -> Option<String> {
        Some(std::fs::read_to_string(self.dir.join("owner")).map(|s| s.trim().to_string()).unwrap_or("stub".into()))
    }
}

// ── The mirror ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum Job {
    Post { device: String, key: String, shown: Shown },
    Remove { device: String, key: String },
    /// Revoke: close what the phone posted and forget it.
    Forget { device: String },
}

#[derive(Default)]
struct Phone {
    bucket: Option<Bucket>,
    /// `(key, server id)`, oldest first.
    ids: VecDeque<(String, u32)>,
}

pub struct Mirror {
    backend: Box<dyn Backend>,
    phones: Mutex<HashMap<String, Phone>>,
    queue: Mutex<Option<SyncSender<Job>>>,
    /// The notification server the ids in [`Phone::ids`] came from.
    server: Mutex<Option<String>>,
}

impl Mirror {
    pub fn new(backend: Box<dyn Backend>) -> Mirror {
        Mirror { backend, phones: Mutex::new(HashMap::new()), queue: Mutex::new(None), server: Mutex::new(None) }
    }

    fn phones(&self) -> std::sync::MutexGuard<'_, HashMap<String, Phone>> {
        self.phones.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether this phone may post one more now. Removals are not charged.
    pub fn allow(&self, device: &str, now: Instant) -> bool {
        let mut phones = self.phones();
        let p = phones.entry(device.to_string()).or_default();
        p.bucket.get_or_insert_with(|| Bucket::new(now)).take(now)
    }

    /// An id means something only to the server that gave it out, and a
    /// restarted server (a Shell update restarts it) counts from 1 again: an
    /// old id would replace or close some other app's notification. So every
    /// id is dropped when the server's bus owner changes.
    fn same_server(&self) {
        let now = self.backend.owner();
        let mut server = self.server.lock().unwrap_or_else(|e| e.into_inner());
        if *server != now {
            for p in self.phones().values_mut() {
                p.ids.clear();
            }
            *server = now;
        }
    }

    /// Do one job, on the worker's thread (or a test's).
    pub fn handle(&self, job: Job) {
        self.same_server();
        match job {
            Job::Post { device, key, shown } => {
                let replaces = self.id_of(&device, &key).unwrap_or(0);
                match self.backend.notify(&shown, replaces) {
                    Ok(id) => self.remember(&device, &key, id),
                    Err(why) => eprintln!("rime-remoted: a phone notification was not shown: {why}"),
                }
            }
            Job::Remove { device, key } => {
                let id = {
                    let mut phones = self.phones();
                    phones.get_mut(&device).and_then(|p| {
                        let at = p.ids.iter().position(|(k, _)| *k == key)?;
                        p.ids.remove(at).map(|(_, id)| id)
                    })
                };
                if let Some(id) = id {
                    if let Err(why) = self.backend.close(id) {
                        eprintln!("rime-remoted: a phone notification was not closed: {why}");
                    }
                }
            }
            Job::Forget { device } => {
                let gone = self.phones().remove(&device);
                for (_, id) in gone.map(|p| p.ids).unwrap_or_default() {
                    let _ = self.backend.close(id);
                }
            }
        }
    }

    fn id_of(&self, device: &str, key: &str) -> Option<u32> {
        self.phones().get(device)?.ids.iter().find(|(k, _)| k == key).map(|(_, id)| *id)
    }

    fn remember(&self, device: &str, key: &str, id: u32) {
        let mut phones = self.phones();
        let p = phones.entry(device.to_string()).or_default();
        p.ids.retain(|(k, _)| k != key);
        p.ids.push_back((key.to_string(), id));
        while p.ids.len() > KEPT_IDS {
            p.ids.pop_front();
        }
    }

    /// Revoke: what this phone posted is closed, on the worker, and it is
    /// forgotten. With the queue full it is forgotten at once and its
    /// notifications stay until dismissed: never a wait on the bus here.
    pub fn forget(&self, device: &str) {
        if !self.submit(Job::Forget { device: device.to_string() }) {
            self.phones().remove(device);
        }
    }

    /// Hand a job to the worker. False when the queue is full or gone.
    pub fn submit(&self, job: Job) -> bool {
        let queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        // Full or gone, the job is dropped: never a wait here.
        queue.as_ref().is_some_and(|q| q.try_send(job).is_ok())
    }

    /// Give this mirror its queue and return the end the worker reads.
    pub fn open_queue(&self) -> Receiver<Job> {
        let (tx, rx) = sync_channel(QUEUE);
        *self.queue.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        rx
    }
}

/// The mirror this process uses, with its worker running.
pub fn mirror() -> &'static Mirror {
    static CHOSEN: OnceLock<&'static Mirror> = OnceLock::new();
    CHOSEN.get_or_init(|| {
        let backend: Box<dyn Backend> = match std::env::var_os(STUB_ENV) {
            Some(dir) if !dir.is_empty() => Box::new(Stub::new(PathBuf::from(dir))),
            _ if cfg!(test) => Box::new(Stub::new(
                std::env::temp_dir().join(format!("rime-remoted-mirror-unit-{}", std::process::id())),
            )),
            _ => Box::new(Real),
        };
        let m: &'static Mirror = Box::leak(Box::new(Mirror::new(backend)));
        let rx = m.open_queue();
        let spawned = std::thread::Builder::new().name("phone-notifications".into()).spawn(move || {
            for job in rx {
                m.handle(job);
            }
        });
        if let Err(e) = spawned {
            eprintln!("rime-remoted: phone notifications are off: no worker thread ({e})");
        }
        m
    })
}

/// Answer one of [`VERBS`]. The device is the handshake's; its name is the
/// store's, never a body field.
pub fn control(m: &Mirror, device_id: &str, phone_name: &str, line: &[u8]) -> Vec<u8> {
    let v: Value = match serde_json::from_slice(line) {
        Ok(v) => v,
        Err(_) => return error("bad_request", "that request is not JSON"),
    };
    match v["cmd"].as_str() {
        Some("phone_notification") => {
            let (key, shown) = match shown(phone_name, &v) {
                Ok(x) => x,
                Err(why) => return error("bad_request", &why),
            };
            if !m.allow(device_id, Instant::now()) {
                return error("rate_limited", "this phone sent too many notifications at once; this one was not shown");
            }
            if !m.submit(Job::Post { device: device_id.to_string(), key, shown }) {
                return error("busy", "this machine is behind on notifications; this one was not shown");
            }
            ok()
        }
        Some("phone_notification_removed") => match key(&v) {
            // Not refused when the queue is full: a removal that never
            // happens leaves a stale notification, which is untidy, not wrong.
            Ok(key) => {
                m.submit(Job::Remove { device: device_id.to_string(), key });
                ok()
            }
            Err(why) => error("bad_request", &why),
        },
        _ => error("bad_request", "not a notification verb"),
    }
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

    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(tag: &str) -> Scratch {
        let d = std::env::temp_dir().join(format!("rime-mirror-{tag}-{}-{}", std::process::id(), rime_remote_core::now_ms()));
        Scratch(d)
    }

    fn lines(dir: &std::path::Path) -> Vec<Value> {
        std::fs::read_to_string(dir.join("notifications.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn no_field_can_form_a_tag_and_the_body_keeps_its_ampersands() {
        let (_, s) = shown(
            "Pixel <8>",
            &json!({"key": "k1", "app": "Sig<b>nal</b>", "title": "<img src=http://x/p.png>Hi",
                    "text": "a < b && <a href='x'>c</a> &lt;"}),
        )
        .unwrap();
        for f in [&s.app_name, &s.summary, &s.body] {
            assert!(!f.contains('<') && !f.contains('>'), "{f}");
        }
        assert_eq!(s.app_name, "Pixel ‹8› · Sig‹b›nal‹/b›");
        assert_eq!(s.summary, "‹img src=http://x/p.png›Hi");
        assert_eq!(s.body, "a ‹ b &amp;&amp; ‹a href='x'›c‹/a› &amp;lt;");
    }

    #[test]
    fn control_and_bidi_characters_go_and_one_line_fields_stay_one_line() {
        let (_, s) = shown(
            "P",
            &json!({"key": "k", "app": "A\u{202E}pp", "title": "one\ntwo\u{0007}\r\nthree",
                    "text": "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\u{2066}\u{200F}\u{0000}"}),
        )
        .unwrap();
        assert_eq!(s.app_name, "P · App");
        assert_eq!(s.summary, "one two three");
        assert_eq!(s.body.matches('\n').count(), TEXT_LINES, "{:?}", s.body);
        assert!(s.body.ends_with("l7 l8"), "{:?}", s.body);
    }

    #[test]
    fn fields_are_capped_in_characters_with_an_ellipsis() {
        let long = "é".repeat(5000);
        let (_, s) = shown("P", &json!({"key": "k", "app": long, "title": long, "text": long})).unwrap();
        assert_eq!(s.summary.chars().count(), TITLE_CAP + 1);
        assert!(s.summary.ends_with('…'));
        assert_eq!(s.body.chars().count(), TEXT_CAP + 1);
        assert_eq!(s.app_name.chars().count(), "P · ".chars().count() + APP_CAP + 1);
    }

    #[test]
    fn a_bad_key_or_an_empty_notification_is_refused() {
        for bad in [json!({"title": "t"}), json!({"key": "", "title": "t"}), json!({"key": "a/b", "title": "t"}),
                    json!({"key": "k".repeat(65), "title": "t"}), json!({"key": "k", "title": 5}),
                    json!({"key": "k", "title": " \n ", "text": "\u{202E}"})] {
            assert!(shown("P", &bad).is_err(), "{bad}");
        }
        let (_, s) = shown("", &json!({"key": "k", "app": "Mail", "text": "body"})).unwrap();
        assert_eq!((s.app_name.as_str(), s.summary.as_str()), ("Phone · Mail", "Mail"));
    }

    #[test]
    fn a_burst_of_ten_then_one_every_three_seconds() {
        let t = Instant::now();
        let mut b = Bucket::new(t);
        assert_eq!((0..BURST).filter(|_| b.take(t)).count() as u32, BURST);
        assert!(!b.take(t));
        assert!(!b.take(t + Duration::from_millis(2999)));
        assert!(b.take(t + REFILL));
        assert!(!b.take(t + REFILL));
        // A long quiet spell refills to the burst, never past it.
        let later = t + Duration::from_secs(3600);
        assert_eq!((0..50).filter(|_| b.take(later)).count() as u32, BURST);
    }

    #[test]
    fn an_update_replaces_its_notification_and_a_removal_closes_it() {
        let s = scratch("replace");
        let m = Mirror::new(Box::new(Stub::new(s.0.clone())));
        let post = |key: &str, title: &str| Job::Post {
            device: "dev".into(),
            key: key.into(),
            shown: Shown { app_name: "P · A".into(), summary: title.into(), body: String::new() },
        };
        m.handle(post("k1", "first"));
        m.handle(post("k2", "other"));
        m.handle(post("k1", "first, edited"));
        m.handle(Job::Remove { device: "dev".into(), key: "k1".into() });
        m.handle(Job::Remove { device: "dev".into(), key: "k1".into() });
        m.handle(Job::Remove { device: "other-phone".into(), key: "k2".into() });
        let got: Vec<(String, u64, u64)> = lines(&s.0)
            .iter()
            .map(|l| (l["op"].as_str().unwrap().to_string(), l["id"].as_u64().unwrap(), l["replaces"].as_u64().unwrap_or(0)))
            .collect();
        assert_eq!(got, vec![("notify".into(), 1, 0), ("notify".into(), 2, 0), ("notify".into(), 1, 1), ("close".into(), 1, 0)]);
    }

    #[test]
    fn revoking_closes_what_the_phone_posted_and_only_its() {
        let s = scratch("revoke");
        let m = Mirror::new(Box::new(Stub::new(s.0.clone())));
        for (dev, key) in [("a", "k1"), ("b", "k1"), ("a", "k2")] {
            m.handle(Job::Post { device: dev.into(), key: key.into(),
                                 shown: Shown { app_name: "x".into(), summary: "y".into(), body: String::new() } });
        }
        m.handle(Job::Forget { device: "a".into() });
        assert!(m.phones().get("a").is_none());
        let closed: Vec<u64> = lines(&s.0).iter().filter(|l| l["op"] == "close").map(|l| l["id"].as_u64().unwrap()).collect();
        assert_eq!(closed, vec![1, 3]);
    }

    #[test]
    fn a_restarted_server_gets_new_notifications_and_no_closes_for_old_ids() {
        let s = scratch("owner");
        let m = Mirror::new(Box::new(Stub::new(s.0.clone())));
        let post = |key: &str| Job::Post {
            device: "dev".into(),
            key: key.into(),
            shown: Shown { app_name: "x".into(), summary: "y".into(), body: String::new() },
        };
        m.handle(post("k1"));
        m.handle(post("k2"));
        std::fs::write(s.0.join("owner"), ":1.99").unwrap();
        m.handle(post("k1"));
        m.handle(Job::Remove { device: "dev".into(), key: "k2".into() });
        m.handle(Job::Forget { device: "dev".into() });
        let got: Vec<(String, u64)> = lines(&s.0)
            .iter()
            .map(|l| (l["op"].as_str().unwrap().to_string(), l["replaces"].as_u64().unwrap_or(0)))
            .collect();
        // k1 is posted fresh, k2's removal closes nothing, and revoke closes
        // only what the new server was given.
        assert_eq!(got, vec![("notify".into(), 0), ("notify".into(), 0), ("notify".into(), 0), ("close".into(), 0)]);
        assert_eq!(lines(&s.0)[3]["id"], lines(&s.0)[2]["id"]);
    }

    #[test]
    fn only_the_newest_ids_are_kept() {
        let s = scratch("kept");
        let m = Mirror::new(Box::new(Stub::new(s.0.clone())));
        for i in 0..(KEPT_IDS + 5) {
            m.handle(Job::Post { device: "d".into(), key: format!("k{i}"),
                                 shown: Shown { app_name: "x".into(), summary: "y".into(), body: String::new() } });
        }
        assert_eq!(m.phones().get("d").unwrap().ids.len(), KEPT_IDS);
        assert_eq!(m.id_of("d", "k0"), None);
        assert_eq!(m.id_of("d", &format!("k{}", KEPT_IDS + 4)), Some((KEPT_IDS + 5) as u32));
    }

    #[test]
    fn the_verbs_reply_at_once_and_drop_past_the_rate() {
        let s = scratch("control");
        let m = Mirror::new(Box::new(Stub::new(s.0.clone())));
        let rx = m.open_queue();
        let line = |k: usize| json!({"cmd": "phone_notification", "key": format!("k{k}"), "app": "Signal", "title": "t"}).to_string();
        let replies: Vec<String> = (0..12)
            .map(|k| serde_json::from_slice::<Value>(&control(&m, "dev", "Pixel 8", line(k).as_bytes())).unwrap()["reply"]
                .as_str().unwrap().to_string() + "")
            .collect();
        assert_eq!(replies.iter().filter(|r| *r == "ok").count() as u32, BURST, "{replies:?}");
        let refused: Value = serde_json::from_slice(&control(&m, "dev", "Pixel 8", line(99).as_bytes())).unwrap();
        assert_eq!(refused["code"], "rate_limited");
        // Another phone has its own allowance.
        let other: Value = serde_json::from_slice(&control(&m, "dev2", "Tab", line(1).as_bytes())).unwrap();
        assert_eq!(other["reply"], "ok");
        // Removals are never charged.
        let rm = json!({"cmd": "phone_notification_removed", "key": "k1"}).to_string();
        let rmr: Value = serde_json::from_slice(&control(&m, "dev", "Pixel 8", rm.as_bytes())).unwrap();
        assert_eq!(rmr["reply"], "ok");
        let jobs: Vec<Job> = rx.try_iter().collect();
        assert_eq!(jobs.len(), BURST as usize + 2);
        match &jobs[0] {
            Job::Post { shown, .. } => assert_eq!(shown.app_name, "Pixel 8 · Signal"),
            j => panic!("{j:?}"),
        }
    }

    #[test]
    fn a_full_queue_drops_rather_than_blocks() {
        let m = Mirror::new(Box::new(Stub::new(std::env::temp_dir().join("rime-mirror-never-written"))));
        let _rx = m.open_queue();
        let job = || Job::Remove { device: "d".into(), key: "k".into() };
        assert_eq!((0..QUEUE + 3).filter(|_| m.submit(job())).count(), QUEUE);
        let unopened = Mirror::new(Box::new(Stub::new(std::env::temp_dir().join("rime-mirror-never-written"))));
        assert!(!unopened.submit(job()));
    }

    /// The real backend against a fake notification server on a PRIVATE bus.
    /// Runs only when `RIME_REMOTED_FAKE_NOTIFY_BUS=1` says the session bus in
    /// the environment is a throwaway one (see `tests/test-remoted-notify-busctl.sh`),
    /// because it posts through whatever `DBUS_SESSION_BUS_ADDRESS` names.
    #[test]
    fn the_real_backend_posts_replaces_and_closes_on_a_fake_server() {
        if std::env::var_os("RIME_REMOTED_FAKE_NOTIFY_BUS").as_deref() != Some(std::ffi::OsStr::new("1")) {
            eprintln!("skipped: RIME_REMOTED_FAKE_NOTIFY_BUS is not set");
            return;
        }
        let (_, s) = shown("Pixel 8", &json!({"key": "k", "app": "Signal", "title": "-x <b>", "text": "a & b"})).unwrap();
        assert!(Real.owner().is_some_and(|o| o.starts_with(":1.")), "{:?}", Real.owner());
        let id = Real.notify(&s, 0).expect("posted");
        assert!(id > 0);
        assert_eq!(Real.notify(&s, id).expect("replaced"), id);
        Real.close(id).expect("closed");
    }
}
