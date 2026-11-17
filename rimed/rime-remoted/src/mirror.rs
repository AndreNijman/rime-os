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
//! keeps the id the server returns per `(device, key)`, so an update replaces
//! its notification and a removal closes it.
//!
//! [`Bus`] is the desktop: one thread of its own with a single-threaded tokio
//! runtime owns the session-bus connection (made when first needed, made again
//! after it breaks). It answers the worker's calls, each within
//! [`POST_BUDGET`], and hears `NotificationClosed` from the current owner of
//! `org.freedesktop.Notifications` only. The text goes over the bus in the
//! message, never on a command line.
//!
//! ## Dismissed here, gone there
//!
//! A close with reason 2 ("dismissed by the user") for an id this process
//! posted goes back to the phone as `notification_dismissed` on its events
//! channel, if the phone said it `can` "dismiss" (contract §2.7). Expired
//! (1), closed by a call (3, which includes this process's own closes) and
//! anything else only forget the id. The signal is queued behind the job that
//! posted the id, so a notification closed the moment it was shown is still
//! known when its close is read.
//!
//! ## Tests never reach the real desktop
//!
//! [`STUB_ENV`] points the worker at a directory: each post and close is a
//! JSON line in `<dir>/notifications.jsonl`, and a line `{"id":N,"reason":R}`
//! appended to `<dir>/closed.jsonl` stands for the server's
//! `NotificationClosed`. A unit-test build never gets the real backend from
//! [`mirror`], whatever the environment says.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Seek, Write};
use std::path::PathBuf;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
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

/// `NotificationClosed`'s reason when the person dismissed it.
pub const DISMISSED: u32 = 2;

/// What a phone is told when the person dismissed one of its notifications here.
pub const DISMISSED_EVENT: &str = "notification_dismissed";

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

/// Called with `(id, reason)` for each `NotificationClosed` the server sends.
pub type Closer = Box<dyn Fn(u32, u32) + Send + Sync>;

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

enum Call {
    Notify(Shown, u32, SyncSender<Result<u32, String>>),
    Close(u32, SyncSender<Result<(), String>>),
    Owner(SyncSender<Option<String>>),
}

/// The desktop's notification server, over the session bus.
pub struct Bus {
    calls: tokio::sync::mpsc::Sender<Call>,
}

struct Link {
    notifications: zbus::Proxy<'static>,
    dbus: zbus::fdo::DBusProxy<'static>,
    closed: zbus::proxy::SignalStream<'static>,
}

/// Whether an error means the connection is gone (so the next call makes a
/// new one) rather than a refusal from the other end.
fn broken(e: &zbus::Error) -> bool {
    !matches!(e, zbus::Error::MethodError(..) | zbus::Error::FDO(..))
}

impl Bus {
    /// Start the bus thread. Nothing touches the bus until the first call.
    pub fn start(closer: Closer) -> std::io::Result<Bus> {
        let (calls, rx) = tokio::sync::mpsc::channel(4);
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        std::thread::Builder::new()
            .name("phone-notifications-bus".into())
            .spawn(move || runtime.block_on(Bus::serve(rx, closer)))?;
        Ok(Bus { calls })
    }

    /// One call, answered within the budget or not at all.
    fn ask<T>(&self, call: impl FnOnce(SyncSender<T>) -> Call) -> Option<T> {
        let (tx, rx) = sync_channel(1);
        self.calls.try_send(call(tx)).ok()?;
        rx.recv_timeout(POST_BUDGET + Duration::from_millis(500)).ok()
    }

    async fn connect() -> zbus::Result<Link> {
        let conn = zbus::Connection::session().await?;
        let notifications: zbus::Proxy<'static> = zbus::ProxyBuilder::new(&conn)
            .destination(NAME)?
            .path(PATH)?
            .interface(NAME)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await?;
        let dbus = zbus::fdo::DBusProxy::builder(&conn)
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await?;
        // A proxy for a well-known name hears that name's owner only, so a
        // client that sends its own NotificationClosed is not believed.
        let closed = notifications.receive_signal("NotificationClosed").await?;
        Ok(Link { notifications, dbus, closed })
    }

    async fn serve(mut calls: tokio::sync::mpsc::Receiver<Call>, closer: Closer) {
        use zbus::export::futures_util::StreamExt;
        enum Next {
            Call(Option<Call>),
            Closed(Option<zbus::Message>),
        }
        let mut link: Option<Link> = None;
        loop {
            let next = match link.as_mut() {
                Some(l) => tokio::select! {
                    c = calls.recv() => Next::Call(c),
                    m = l.closed.next() => Next::Closed(m),
                },
                None => Next::Call(calls.recv().await),
            };
            match next {
                Next::Call(None) => return,
                Next::Call(Some(call)) => {
                    if link.is_none() {
                        match tokio::time::timeout(POST_BUDGET, Bus::connect()).await {
                            Ok(Ok(l)) => link = Some(l),
                            Ok(Err(e)) => eprintln!("rime-remoted: no session bus for phone notifications: {e}"),
                            Err(_) => eprintln!("rime-remoted: the session bus did not answer in time"),
                        }
                    }
                    if Bus::answer(call, link.as_ref()).await {
                        link = None;
                    }
                }
                Next::Closed(Some(m)) => {
                    if let Ok((id, reason)) = m.body().deserialize::<(u32, u32)>() {
                        closer(id, reason);
                    }
                }
                // The stream ends with the connection.
                Next::Closed(None) => link = None,
            }
        }
    }

    /// Answer one call. True when the connection broke doing it.
    async fn answer(call: Call, link: Option<&Link>) -> bool {
        async fn within<T, E: Into<zbus::Error>>(
            f: impl std::future::Future<Output = Result<T, E>>,
        ) -> Result<T, (String, bool)> {
            match tokio::time::timeout(POST_BUDGET, f).await {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => {
                    let e: zbus::Error = e.into();
                    Err((e.to_string(), broken(&e)))
                }
                Err(_) => Err(("the notification server did not answer in time".into(), false)),
            }
        }
        let Some(l) = link else {
            match call {
                Call::Notify(_, _, r) => drop(r.send(Err("no session bus".into()))),
                Call::Close(_, r) => drop(r.send(Err("no session bus".into()))),
                Call::Owner(r) => drop(r.send(None)),
            }
            return false;
        };
        match call {
            Call::Notify(s, replaces, reply) => {
                let actions: Vec<&str> = Vec::new();
                let hints: HashMap<&str, zbus::zvariant::Value<'_>> =
                    HashMap::from([("urgency", zbus::zvariant::Value::U8(1))]);
                let body = (&s.app_name, replaces, ICON, &s.summary, &s.body, actions, hints, -1i32);
                let got = within(l.notifications.call::<_, _, u32>("Notify", &body)).await;
                let gone = matches!(got, Err((_, true)));
                let _ = reply.send(got.map_err(|(e, _)| e));
                gone
            }
            Call::Close(id, reply) => {
                let got = within(l.notifications.call::<_, _, ()>("CloseNotification", &(id,))).await;
                let gone = matches!(got, Err((_, true)));
                let _ = reply.send(got.map_err(|(e, _)| e));
                gone
            }
            Call::Owner(reply) => {
                let Ok(name) = zbus::names::BusName::from_static_str(NAME) else { return false };
                let got = within(l.dbus.get_name_owner(name)).await;
                let gone = matches!(got, Err((_, true)));
                let _ = reply.send(got.ok().map(|o| o.to_string()));
                gone
            }
        }
    }
}

impl Backend for Bus {
    fn notify(&self, s: &Shown, replaces: u32) -> Result<u32, String> {
        self.ask(|r| Call::Notify(s.clone(), replaces, r))
            .unwrap_or_else(|| Err("the notification server did not answer in time".into()))
    }

    fn close(&self, id: u32) -> Result<(), String> {
        self.ask(|r| Call::Close(id, r)).unwrap_or_else(|| Err("the notification server did not answer in time".into()))
    }

    fn owner(&self) -> Option<String> {
        self.ask(Call::Owner).flatten()
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
    /// The server's `NotificationClosed`.
    Closed { id: u32, reason: u32 },
}

/// Told `(device, key)` when the person dismissed a phone's notification here.
pub type Dismissed = Box<dyn Fn(&str, &str) + Send + Sync>;

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
    dismissed: OnceLock<Dismissed>,
}

impl Mirror {
    pub fn new(backend: Box<dyn Backend>) -> Mirror {
        Mirror {
            backend,
            phones: Mutex::new(HashMap::new()),
            queue: Mutex::new(None),
            server: Mutex::new(None),
            dismissed: OnceLock::new(),
        }
    }

    /// Where dismissals go (once; later calls are ignored).
    pub fn on_dismissed(&self, f: Dismissed) {
        let _ = self.dismissed.set(f);
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
            Job::Closed { id, reason } => {
                // Gone from the desktop, whatever the reason: the id is dead.
                let owner = {
                    let mut phones = self.phones();
                    phones.iter_mut().find_map(|(device, p)| {
                        let at = p.ids.iter().position(|(_, i)| *i == id)?;
                        p.ids.remove(at).map(|(key, _)| (device.clone(), key))
                    })
                };
                if let (Some((device, key)), DISMISSED) = (owner, reason) {
                    if let Some(tell) = self.dismissed.get() {
                        tell(&device, &key);
                    }
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

/// A mirror with its worker running, and the closes `make`'s backend hears
/// queued for that worker.
fn running(make: impl FnOnce(Closer) -> std::io::Result<Box<dyn Backend>>) -> std::io::Result<&'static Mirror> {
    let cell: Arc<OnceLock<&'static Mirror>> = Arc::new(OnceLock::new());
    let to = cell.clone();
    // Through the queue, never handled here: the worker owns the order, so a
    // close always comes after the post that gave out its id.
    let closer: Closer = Box::new(move |id, reason| {
        if let Some(m) = to.get() {
            m.submit(Job::Closed { id, reason });
        }
    });
    let m: &'static Mirror = Box::leak(Box::new(Mirror::new(make(closer)?)));
    let _ = cell.set(m);
    let rx = m.open_queue();
    std::thread::Builder::new().name("phone-notifications".into()).spawn(move || {
        for job in rx {
            m.handle(job);
        }
    })?;
    Ok(m)
}

/// The stub's stand-in for `NotificationClosed`: complete lines appended to
/// `<dir>/closed.jsonl`, read every 100 ms.
fn watch_closed(dir: PathBuf, closer: Closer) -> std::io::Result<()> {
    std::thread::Builder::new().name("phone-notifications-stub".into()).spawn(move || {
        let path = dir.join("closed.jsonl");
        let mut at = 0u64;
        let mut partial = String::new();
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let Ok(mut f) = std::fs::File::open(&path) else { continue };
            if f.seek(std::io::SeekFrom::Start(at)).is_err() {
                continue;
            }
            let mut more = String::new();
            let Ok(n) = f.read_to_string(&mut more) else { continue };
            at += n as u64;
            partial.push_str(&more);
            while let Some(i) = partial.find('\n') {
                let line: String = partial.drain(..=i).collect();
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if let (Some(id), Some(reason)) = (v["id"].as_u64(), v["reason"].as_u64()) {
                        closer(id as u32, reason as u32);
                    }
                }
            }
        }
    })?;
    Ok(())
}

/// The mirror this process uses, with its worker running.
pub fn mirror() -> &'static Mirror {
    static CHOSEN: OnceLock<&'static Mirror> = OnceLock::new();
    CHOSEN.get_or_init(|| {
        let stub = match std::env::var_os(STUB_ENV) {
            Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
            _ if cfg!(test) => {
                Some(std::env::temp_dir().join(format!("rime-remoted-mirror-unit-{}", std::process::id())))
            }
            _ => None,
        };
        let started = running(|closer| -> std::io::Result<Box<dyn Backend>> {
            Ok(match stub {
                Some(dir) => {
                    watch_closed(dir.clone(), closer)?;
                    Box::new(Stub::new(dir))
                }
                None => Box::new(Bus::start(closer)?),
            })
        });
        started.unwrap_or_else(|e| {
            eprintln!("rime-remoted: phone notifications are off: no worker thread ({e})");
            // No queue: every post answers `busy`.
            Box::leak(Box::new(Mirror::new(Box::new(Stub::new(PathBuf::from("/nonexistent"))))))
        })
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

    #[test]
    fn only_a_dismissal_of_an_id_posted_here_reaches_the_phone_and_every_close_forgets_the_id() {
        let s = scratch("closed");
        let m = Mirror::new(Box::new(Stub::new(s.0.clone())));
        let (tx, rx) = std::sync::mpsc::channel();
        m.on_dismissed(Box::new(move |d, k| tx.send((d.to_string(), k.to_string())).unwrap()));
        let post = |dev: &str, key: &str| Job::Post {
            device: dev.into(),
            key: key.into(),
            shown: Shown { app_name: "x".into(), summary: "y".into(), body: String::new() },
        };
        m.handle(post("a", "expired")); // id 1
        m.handle(post("a", "closed-by-call")); // id 2
        m.handle(post("b", "dismissed")); // id 3
        m.handle(Job::Closed { id: 1, reason: 1 });
        m.handle(Job::Closed { id: 2, reason: 3 });
        m.handle(Job::Closed { id: 99, reason: DISMISSED });
        m.handle(Job::Closed { id: 3, reason: DISMISSED });
        // A second close of the same id finds nothing.
        m.handle(Job::Closed { id: 3, reason: DISMISSED });
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![("b".to_string(), "dismissed".to_string())]);
        for (dev, key) in [("a", "expired"), ("a", "closed-by-call"), ("b", "dismissed")] {
            assert_eq!(m.id_of(dev, key), None, "{dev}/{key} kept a dead id");
        }
        // So the next post of that key is a new notification, not a replace
        // of an id the server may have given someone else.
        m.handle(post("a", "expired"));
        assert_eq!(lines(&s.0).last().unwrap()["replaces"], 0);
    }

    #[test]
    fn the_stub_reads_closes_from_its_file_through_the_worker() {
        let s = scratch("stubclosed");
        std::fs::create_dir_all(&s.0).unwrap();
        let dir = s.0.clone();
        let m = running(|closer| -> std::io::Result<Box<dyn Backend>> {
            watch_closed(dir.clone(), closer)?;
            Ok(Box::new(Stub::new(dir)))
        })
        .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        m.on_dismissed(Box::new(move |d, k| tx.send((d.to_string(), k.to_string())).unwrap()));
        assert!(m.submit(Job::Post {
            device: "dev".into(),
            key: "k".into(),
            shown: Shown { app_name: "x".into(), summary: "y".into(), body: String::new() },
        }));
        let deadline = Instant::now() + Duration::from_secs(5);
        while m.id_of("dev", "k").is_none() {
            assert!(Instant::now() < deadline, "never posted");
            std::thread::sleep(Duration::from_millis(10));
        }
        // Written in two pieces: half a line is not read as a close.
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(s.0.join("closed.jsonl")).unwrap();
        f.write_all(br#"{"id":1,"#).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(m.id_of("dev", "k").is_some());
        f.write_all(b"\"reason\":2}\n").unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), ("dev".to_string(), "k".to_string()));
    }

    /// The real backend against a fake notification server on a PRIVATE bus,
    /// run by `tests/test-remoted-notify-bus.sh` with `--ignored`. It posts
    /// through whatever `DBUS_SESSION_BUS_ADDRESS` names, so it refuses to run
    /// unless `RIME_REMOTED_FAKE_NOTIFY_BUS=1` says that bus is a throwaway one:
    /// a failure, not a skip, so it never reads as a pass.
    #[test]
    #[ignore = "posts on the session bus; run by tests/test-remoted-notify-bus.sh on a private one"]
    fn the_real_backend_posts_replaces_closes_and_hears_dismissals_on_a_fake_server() {
        assert_eq!(
            std::env::var_os("RIME_REMOTED_FAKE_NOTIFY_BUS").as_deref(),
            Some(std::ffi::OsStr::new("1")),
            "refusing to post on a session bus that is not a throwaway one: run tests/test-remoted-notify-bus.sh"
        );
        let m = running(|closer| -> std::io::Result<Box<dyn Backend>> { Ok(Box::new(Bus::start(closer)?)) }).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        m.on_dismissed(Box::new(move |d, k| tx.send((d.to_string(), k.to_string())).unwrap()));
        let posted = |key: &str, title: &str, text: &str| -> u32 {
            let (_, shown) = shown("Pixel 8", &json!({"key": key, "app": "Signal", "title": title, "text": text})).unwrap();
            assert!(m.submit(Job::Post { device: "dev".into(), key: key.into(), shown }));
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(id) = m.id_of("dev", key) {
                    return id;
                }
                assert!(Instant::now() < deadline, "{key} was never posted");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let gone = |key: &str| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while m.id_of("dev", key).is_some() {
                assert!(Instant::now() < deadline, "{key} was never forgotten");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        // The test's own connection: it asks the fake server to emit, and it
        // emits as a stranger.
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let conn = rt.block_on(zbus::Connection::session()).unwrap();
        let emit = |id: u32, reason: u32| {
            rt.block_on(conn.call_method(Some(NAME), "/test", Some("org.rimeos.Test"), "Emit", &(id, reason))).unwrap();
        };

        // Posted, replaced, then closed by this process: a close with reason
        // 3 that is not a dismissal.
        let id = posted("k", "-x <b>", "a & b");
        assert!(m.backend.owner().is_some_and(|o| o.starts_with(":1.")), "{:?}", m.backend.owner());
        let (_, again) = shown("Pixel 8", &json!({"key": "k", "app": "Signal", "title": "-x <b>", "text": "a & b"})).unwrap();
        assert!(m.submit(Job::Post { device: "dev".into(), key: "k".into(), shown: again }));
        assert!(m.submit(Job::Remove { device: "dev".into(), key: "k".into() }));
        gone("k");
        assert!(id > 0);

        // A stranger's NotificationClosed is not believed; the server's
        // expiry (1) forgets the id and tells the phone nothing.
        let a = posted("a", "A", "");
        rt.block_on(conn.emit_signal(None::<&str>, PATH, NAME, "NotificationClosed", &(a, DISMISSED))).unwrap();
        emit(a, 1);
        gone("a");

        // The server's dismissal (2) reaches the phone.
        let b = posted("b", "B", "");
        emit(b, DISMISSED);
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), ("dev".to_string(), "b".to_string()));
        gone("b");

        // Closed before Notify even returned (the server emits first): still
        // known when the close is read, because the close waits behind the post.
        let (_, at_once) = shown("Pixel 8", &json!({"key": "c", "app": "Signal", "title": "close-at-once"})).unwrap();
        assert!(m.submit(Job::Post { device: "dev".into(), key: "c".into(), shown: at_once }));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), ("dev".to_string(), "c".to_string()));
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err(), "a close was told to the phone that should not be");
    }
}
