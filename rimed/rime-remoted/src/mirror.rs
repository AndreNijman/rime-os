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
//! [`POST_BUDGET`] counted from the ask, connecting included, and hears `NotificationClosed` from the current owner of
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
pub const FEATURES: [&str; 2] = ["phone_notifications", REPLY_FEATURE];

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

/// What a phone is told when the person replied to one of its notifications here.
pub const REPLY_EVENT: &str = "notification_reply";

/// The `remote_hello` feature that says `phone_notification` takes `reply`.
pub const REPLY_FEATURE: &str = "phone_notification_reply";

/// The action a server that lists the `inline-reply` capability shows as a
/// text field (KDE's extension, which Quickshell implements).
pub const INLINE_REPLY: &str = "inline-reply";

/// The longest reply label shown, and the longest reply text sent on.
pub const REPLY_LABEL_CAP: usize = 40;
pub const REPLY_TEXT_CAP: usize = 2000;

/// How many ids replied to here are remembered until their close arrives.
const REPLIED_KEPT: usize = 16;

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
    /// The reply field's label when the phone can take a reply to this one.
    pub reply: Option<String>,
    /// `(key, label)` buttons. Only notifications this process makes itself
    /// carry any (a ringing call's Answer and Decline, contract §2.12); a
    /// phone's own notifications never do.
    pub actions: Vec<(String, String)>,
    /// Shown until closed (expire timeout 0) rather than for the server's
    /// own time: a call rings until the phone says it stopped.
    pub sticky: bool,
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
    let reply = clean(field(v, "reply")?, REPLY_LABEL_CAP, 0, false);
    let reply = (!reply.is_empty()).then_some(reply);
    Ok((key, Shown { app_name, summary, body: text, reply, actions: Vec::new(), sticky: false }))
}

/// A reply typed here, as the phone gets it: the person's own words, so
/// line breaks stay; control and bidi characters go, and it is capped.
pub fn reply_text(raw: &str) -> String {
    let kept: String = raw
        .chars()
        .filter(|&c| c == '\n' || !(c.is_control() || is_bidi_control(c) || c == '\u{2028}' || c == '\u{2029}'))
        .take(REPLY_TEXT_CAP)
        .collect();
    kept.trim().to_string()
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

/// A signal from the notification server's current owner.
#[derive(Debug, Clone, PartialEq)]
pub enum Heard {
    /// `NotificationClosed(id, reason)`.
    Closed { id: u32, reason: u32 },
    /// `NotificationReplied(id, text)`: the person typed into the inline-reply field.
    Replied { id: u32, text: String },
    /// `ActionInvoked(id, key)`: the person pressed one of the notification's buttons.
    Action { id: u32, key: String },
}

/// Called with each signal the server sends, in the order it sent them.
pub type Hear = Box<dyn Fn(Heard) + Send + Sync>;

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

enum Call {
    Notify(Shown, u32, SyncSender<Result<u32, String>>),
    Close(u32, SyncSender<Result<(), String>>),
    Owner(SyncSender<Option<String>>),
}

/// The desktop's notification server, over the session bus.
pub struct Bus {
    /// Each call with the moment its asker stops waiting.
    calls: tokio::sync::mpsc::Sender<(Call, std::time::Instant)>,
}

struct Link {
    notifications: zbus::Proxy<'static>,
    dbus: zbus::fdo::DBusProxy<'static>,
    /// Every signal of the interface, in one stream: a reply and the close
    /// that follows it must be handled in the order the server sent them.
    signals: zbus::proxy::SignalStream<'static>,
    /// The owner whose capabilities [`Link::inline_reply`] describes.
    caps_for: Option<String>,
    inline_reply: bool,
    /// Whether that owner draws action buttons (capability `actions`).
    actions: bool,
}

/// Whether an error means the connection is gone (so the next call makes a
/// new one) rather than a refusal from the other end.
fn broken(e: &zbus::Error) -> bool {
    !matches!(e, zbus::Error::MethodError(..) | zbus::Error::FDO(..))
}

impl Bus {
    /// Start the bus thread. Nothing touches the bus until the first call.
    pub fn start(closer: Hear) -> std::io::Result<Bus> {
        let (calls, rx) = tokio::sync::mpsc::channel(4);
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        std::thread::Builder::new()
            .name("phone-notifications-bus".into())
            .spawn(move || runtime.block_on(Bus::serve(rx, closer)))?;
        Ok(Bus { calls })
    }

    /// One call, answered within the budget or not at all. Connecting and
    /// calling share the one deadline, so nothing reaches the server after
    /// the asker has given up: a post it gave up on would show with no id
    /// kept, and never be replaced or closed.
    fn ask<T>(&self, call: impl FnOnce(SyncSender<T>) -> Call) -> Option<T> {
        let (tx, rx) = sync_channel(1);
        let by = std::time::Instant::now() + POST_BUDGET;
        self.calls.try_send((call(tx), by)).ok()?;
        // The slack is for the reply's trip back, not for more work.
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
        let signals = notifications.receive_all_signals().await?;
        Ok(Link { notifications, dbus, signals, caps_for: None, inline_reply: false, actions: false })
    }

    async fn serve(mut calls: tokio::sync::mpsc::Receiver<(Call, std::time::Instant)>, closer: Hear) {
        use zbus::export::futures_util::StreamExt;
        enum Next {
            Call(Option<(Call, std::time::Instant)>),
            Heard(Option<zbus::Message>),
        }
        let mut link: Option<Link> = None;
        loop {
            let next = match link.as_mut() {
                Some(l) => tokio::select! {
                    c = calls.recv() => Next::Call(c),
                    m = l.signals.next() => Next::Heard(m),
                },
                None => Next::Call(calls.recv().await),
            };
            match next {
                Next::Call(None) => return,
                Next::Call(Some((call, by))) => {
                    let by = tokio::time::Instant::from_std(by);
                    if link.is_none() && by > tokio::time::Instant::now() {
                        match tokio::time::timeout_at(by, Bus::connect()).await {
                            Ok(Ok(l)) => link = Some(l),
                            Ok(Err(e)) => eprintln!("rime-remoted: no session bus for phone notifications: {e}"),
                            Err(_) => eprintln!("rime-remoted: the session bus did not answer in time"),
                        }
                    }
                    if Bus::answer(call, by, link.as_mut()).await {
                        link = None;
                    }
                }
                Next::Heard(Some(m)) => {
                    let header = m.header();
                    match header.member().map(|n| n.as_str()) {
                        Some("NotificationClosed") => {
                            if let Ok((id, reason)) = m.body().deserialize::<(u32, u32)>() {
                                closer(Heard::Closed { id, reason });
                            }
                        }
                        Some("NotificationReplied") => {
                            if let Ok((id, text)) = m.body().deserialize::<(u32, String)>() {
                                closer(Heard::Replied { id, text });
                            }
                        }
                        Some("ActionInvoked") => {
                            if let Ok((id, key)) = m.body().deserialize::<(u32, String)>() {
                                closer(Heard::Action { id, key });
                            }
                        }
                        _ => {}
                    }
                }
                // The stream ends with the connection.
                Next::Heard(None) => link = None,
            }
        }
    }

    /// `f` by `by`; the error says whether the connection broke.
    async fn within<T, E: Into<zbus::Error>>(
        by: tokio::time::Instant,
        f: impl std::future::Future<Output = Result<T, E>>,
    ) -> Result<T, (String, bool)> {
        match tokio::time::timeout_at(by, f).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => {
                let e: zbus::Error = e.into();
                Err((e.to_string(), broken(&e)))
            }
            Err(_) => Err(("the notification server did not answer in time".into(), false)),
        }
    }

    /// Answer one call. True when the connection broke doing it.
    async fn answer(call: Call, by: tokio::time::Instant, link: Option<&mut Link>) -> bool {
        let late = by <= tokio::time::Instant::now();
        let Some(l) = link.filter(|_| !late) else {
            let why = if late { "the notification server did not answer in time" } else { "no session bus" };
            match call {
                Call::Notify(_, _, r) => drop(r.send(Err(why.into()))),
                Call::Close(_, r) => drop(r.send(Err(why.into()))),
                Call::Owner(r) => drop(r.send(None)),
            }
            return false;
        };
        match call {
            Call::Notify(s, replaces, reply) => {
                // A reply field only where the server draws one: anywhere
                // else "inline-reply" would be a button that does nothing.
                let label = s.reply.as_deref().filter(|_| l.inline_reply);
                let mut actions: Vec<&str> = label.map(|label| vec![INLINE_REPLY, label]).unwrap_or_default();
                // Buttons only where the server draws them, as for the reply field.
                if l.actions {
                    for (key, label) in &s.actions {
                        actions.push(key);
                        actions.push(label);
                    }
                }
                let mut hints: HashMap<&str, zbus::zvariant::Value<'_>> =
                    HashMap::from([("urgency", zbus::zvariant::Value::U8(1))]);
                if let Some(label) = label {
                    hints.insert("x-kde-reply-placeholder-text", zbus::zvariant::Value::from(label));
                }
                let expire = if s.sticky { 0i32 } else { -1i32 };
                let body = (&s.app_name, replaces, ICON, &s.summary, &s.body, actions, hints, expire);
                let got = Bus::within(by, l.notifications.call::<_, _, u32>("Notify", &body)).await;
                let gone = matches!(got, Err((_, true)));
                let _ = reply.send(got.map_err(|(e, _)| e));
                gone
            }
            Call::Close(id, reply) => {
                let got = Bus::within(by, l.notifications.call::<_, _, ()>("CloseNotification", &(id,))).await;
                let gone = matches!(got, Err((_, true)));
                let _ = reply.send(got.map_err(|(e, _)| e));
                gone
            }
            Call::Owner(reply) => {
                let Ok(name) = zbus::names::BusName::from_static_str(NAME) else { return false };
                let got = Bus::within(by, l.dbus.get_name_owner(name)).await;
                let gone = matches!(got, Err((_, true)));
                let owner = got.ok().map(|o| o.to_string());
                // Asked once per owner: the worker asks for the owner before
                // every job, so a new Shell is asked before its first post.
                if owner.is_some() && owner != l.caps_for {
                    let caps = Bus::within(by, l.notifications.call::<_, _, Vec<String>>("GetCapabilities", &())).await;
                    l.inline_reply = caps.as_ref().is_ok_and(|c| c.iter().any(|c| c == INLINE_REPLY));
                    l.actions = caps.as_ref().is_ok_and(|c| c.iter().any(|c| c == "actions"));
                    l.caps_for = if caps.is_ok() { owner.clone() } else { None };
                }
                let _ = reply.send(owner);
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
        let mut line = json!({"op": "notify", "id": id, "replaces": replaces, "app": s.app_name,
                              "icon": ICON, "summary": s.summary, "body": s.body,
                              // A stand-in for a server that draws reply fields.
                              "reply": s.reply});
        // Only when there are some, so a phone's notification reads as before.
        if !s.actions.is_empty() {
            line["actions"] = json!(s.actions.iter().map(|(k, l)| [k, l]).collect::<Vec<_>>());
        }
        if s.sticky {
            line["sticky"] = json!(true);
        }
        self.record(line)?;
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
    /// A notice of this computer's own (a call action that did not go):
    /// kept by no phone, so it is never replaced, closed or reported.
    Notice(Shown),
    /// The server's `NotificationClosed`, `NotificationReplied` or `ActionInvoked`.
    Heard(Heard),
}

/// Told `(device, key, action)` when the person pressed a button on a
/// notification this process made for that phone (only keys with a `:`,
/// which a phone's own keys never carry: contract §2.12).
pub type Acted = Box<dyn Fn(&str, &str, &str) + Send + Sync>;

/// Whether a key names a notification this process made itself rather than
/// one a phone posted. A phone's keys are checked never to hold a `:`.
pub fn own_key(key: &str) -> bool {
    key.contains(':')
}

/// Told `(device, key)` when the person dismissed a phone's notification here.
pub type Dismissed = Box<dyn Fn(&str, &str) + Send + Sync>;

/// Told `(device, key, text)` when the person replied to one here; answers
/// whether it went to the phone.
pub type Replied = Box<dyn Fn(&str, &str, &str) -> Result<(), NotSent> + Send + Sync>;

/// Why a reply typed here did not go to the phone. The card it was typed
/// into is already closed, so the computer says so and gives the words back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotSent {
    /// The phone has no live connection to this computer.
    Offline,
    /// The phone's latest report does not list `"reply"`: replying is off.
    Off,
}

/// The notice for a reply that did not go: posted here, kept by no phone,
/// so closing it tells nobody, and it has no reply field of its own.
pub fn not_sent(why: NotSent, text: &str) -> Shown {
    let reason = match why {
        NotSent::Offline => "Your phone is not connected, so it could not send this reply.",
        NotSent::Off => "Replying from this computer is off on your phone, so it did not send this reply.",
    };
    Shown {
        app_name: "Rime Remote".into(),
        summary: "Reply not sent".into(),
        // The words go back as a body, so they are made safe like one: a
        // reply of "a < b & c" must not become markup or vanish in it.
        body: format!("{reason} What you typed:\n{}", clean(text, REPLY_TEXT_CAP, usize::MAX, true)),
        reply: None,
        actions: Vec::new(),
        sticky: false,
    }
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
    dismissed: OnceLock<Dismissed>,
    replied: OnceLock<Replied>,
    acted: OnceLock<Acted>,
    /// Ids replied to here whose close has not come yet: the server closes a
    /// notification once it is answered, and that close is not a dismissal.
    answered: Mutex<VecDeque<u32>>,
}

impl Mirror {
    pub fn new(backend: Box<dyn Backend>) -> Mirror {
        Mirror {
            backend,
            phones: Mutex::new(HashMap::new()),
            queue: Mutex::new(None),
            server: Mutex::new(None),
            dismissed: OnceLock::new(),
            replied: OnceLock::new(),
            acted: OnceLock::new(),
            answered: Mutex::new(VecDeque::new()),
        }
    }

    /// Where dismissals go (once; later calls are ignored).
    pub fn on_dismissed(&self, f: Dismissed) {
        let _ = self.dismissed.set(f);
    }

    /// Where replies go (once; later calls are ignored).
    pub fn on_replied(&self, f: Replied) {
        let _ = self.replied.set(f);
    }

    /// Where button presses go (once; later calls are ignored).
    pub fn on_action(&self, f: Acted) {
        let _ = self.acted.set(f);
    }

    fn answered(&self) -> std::sync::MutexGuard<'_, VecDeque<u32>> {
        self.answered.lock().unwrap_or_else(|e| e.into_inner())
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
            self.answered().clear();
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
            Job::Notice(shown) => {
                if let Err(why) = self.backend.notify(&shown, 0) {
                    eprintln!("rime-remoted: a notice was not shown: {why}");
                }
            }
            Job::Heard(Heard::Action { id, key: action }) => {
                // Only this process's own notifications have buttons; a
                // stray signal for any other id is nobody's business here.
                let owner = self.phones().iter().find_map(|(device, p)| {
                    p.ids.iter().find(|(k, i)| *i == id && own_key(k)).map(|(key, _)| (device.clone(), key.clone()))
                });
                if let (Some((device, key)), Some(tell)) = (owner, self.acted.get()) {
                    tell(&device, &key, &action);
                }
            }
            Job::Forget { device } => {
                let gone = self.phones().remove(&device);
                for (_, id) in gone.map(|p| p.ids).unwrap_or_default() {
                    let _ = self.backend.close(id);
                }
            }
            Job::Heard(Heard::Replied { id, text }) => {
                let owner = self.phones().iter().find_map(|(device, p)| {
                    p.ids.iter().find(|(_, i)| *i == id).map(|(key, _)| (device.clone(), key.clone()))
                });
                let Some((device, key)) = owner else { return };
                {
                    let mut answered = self.answered();
                    answered.retain(|i| *i != id);
                    answered.push_back(id);
                    while answered.len() > REPLIED_KEPT {
                        answered.pop_front();
                    }
                }
                let text = reply_text(&text);
                if text.is_empty() {
                    return;
                }
                if let Some(Err(why)) = self.replied.get().map(|tell| tell(&device, &key, &text)) {
                    if let Err(e) = self.backend.notify(&not_sent(why, &text), 0) {
                        eprintln!("rime-remoted: a reply was not sent and saying so failed: {e}");
                    }
                }
            }
            Job::Heard(Heard::Closed { id, reason }) => {
                // Answered here, then closed by the server: the reply went to
                // the phone, whose app updates or clears it there.
                let answered = {
                    let mut answered = self.answered();
                    let had = answered.contains(&id);
                    answered.retain(|i| *i != id);
                    had
                };
                // Gone from the desktop, whatever the reason: the id is dead.
                let owner = {
                    let mut phones = self.phones();
                    phones.iter_mut().find_map(|(device, p)| {
                        let at = p.ids.iter().position(|(_, i)| *i == id)?;
                        p.ids.remove(at).map(|(key, _)| (device.clone(), key))
                    })
                };
                // A notification of this process's own was never the phone's:
                // closing it tells the phone nothing.
                let owner = owner.filter(|(_, key)| !own_key(key));
                if let (Some((device, key)), DISMISSED, false) = (owner, reason, answered) {
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
            // A burst of the phone's own must not push out a ringing call,
            // which then could never be closed when the ringing stops.
            match p.ids.iter().position(|(k, _)| !own_key(k)) {
                Some(at) => {
                    p.ids.remove(at);
                }
                None => {
                    p.ids.pop_front();
                }
            }
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
fn running(make: impl FnOnce(Hear) -> std::io::Result<Box<dyn Backend>>) -> std::io::Result<&'static Mirror> {
    let cell: Arc<OnceLock<&'static Mirror>> = Arc::new(OnceLock::new());
    let to = cell.clone();
    // Through the queue, never handled here: the worker owns the order, so a
    // close always comes after the post that gave out its id.
    let closer: Hear = Box::new(move |heard| {
        if let Some(m) = to.get() {
            m.submit(Job::Heard(heard));
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
fn watch_closed(dir: PathBuf, closer: Hear) -> std::io::Result<()> {
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
                    let Some(id) = v["id"].as_u64() else { continue };
                    if let Some(reason) = v["reason"].as_u64() {
                        closer(Heard::Closed { id: id as u32, reason: reason as u32 });
                    } else if let Some(text) = v["reply"].as_str() {
                        closer(Heard::Replied { id: id as u32, text: text.to_string() });
                    } else if let Some(key) = v["action"].as_str() {
                        closer(Heard::Action { id: id as u32, key: key.to_string() });
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
    fn a_button_is_heard_only_on_this_processs_own_notification_and_its_close_is_no_dismissal() {
        let s = scratch("action");
        let m = Mirror::new(Box::new(Stub::new(s.0.clone())));
        let (atx, arx) = std::sync::mpsc::channel();
        m.on_action(Box::new(move |d, k, a| atx.send((d.to_string(), k.to_string(), a.to_string())).unwrap()));
        let (dtx, drx) = std::sync::mpsc::channel();
        m.on_dismissed(Box::new(move |d, k| dtx.send((d.to_string(), k.to_string())).unwrap()));
        let shown = |actions: Vec<(String, String)>| Shown {
            app_name: "P · Calls".into(),
            summary: "Call from 5556".into(),
            body: String::new(),
            reply: None,
            actions,
            sticky: true,
        };
        let buttons = vec![("answer".to_string(), "Answer".to_string())];
        m.handle(Job::Post { device: "dev".into(), key: "call:ringing".into(), shown: shown(buttons) });
        m.handle(Job::Post { device: "dev".into(), key: "k1".into(), shown: shown(vec![]) });
        // A press on the phone's own notification (some other app's button,
        // as far as this process knows) and on an id nobody owns: nothing.
        m.handle(Job::Heard(Heard::Action { id: 2, key: "answer".into() }));
        m.handle(Job::Heard(Heard::Action { id: 99, key: "answer".into() }));
        m.handle(Job::Heard(Heard::Action { id: 1, key: "answer".into() }));
        assert_eq!(arx.try_iter().collect::<Vec<_>>(), vec![("dev".into(), "call:ringing".into(), "answer".into())]);
        // The server closes it after the press: never a dismissal to the phone.
        m.handle(Job::Heard(Heard::Closed { id: 1, reason: DISMISSED }));
        m.handle(Job::Heard(Heard::Closed { id: 2, reason: DISMISSED }));
        assert_eq!(drx.try_iter().collect::<Vec<_>>(), vec![("dev".into(), "k1".into())]);
        // And a notice is posted fresh each time, kept by nobody.
        m.handle(Job::Notice(shown(vec![])));
        let l = lines(&s.0);
        assert_eq!(l[0]["actions"], json!([["answer", "Answer"]]));
        assert_eq!(l[0]["sticky"], true);
        assert!(l[1].get("actions").is_none() && l[1].get("sticky") == Some(&json!(true)));
        assert_eq!((l[2]["id"].as_u64(), l[2]["replaces"].as_u64()), (Some(3), Some(0)));
    }

    #[test]
    fn a_burst_of_phone_notifications_never_pushes_out_a_ringing_call() {
        let s = scratch("evict");
        let m = Mirror::new(Box::new(Stub::new(s.0.clone())));
        let post = |key: String| Job::Post {
            device: "dev".into(),
            key,
            shown: Shown { app_name: "P".into(), summary: "t".into(), body: String::new(), reply: None, actions: vec![], sticky: false },
        };
        m.handle(post("call:ringing".into()));
        for k in 0..(KEPT_IDS * 2) {
            m.handle(post(format!("k{k}")));
        }
        m.handle(Job::Remove { device: "dev".into(), key: "call:ringing".into() });
        assert_eq!(lines(&s.0).last().unwrap(), &json!({"op": "close", "id": 1}));
    }

    #[test]
    fn an_update_replaces_its_notification_and_a_removal_closes_it() {
        let s = scratch("replace");
        let m = Mirror::new(Box::new(Stub::new(s.0.clone())));
        let post = |key: &str, title: &str| Job::Post {
            device: "dev".into(),
            key: key.into(),
            shown: Shown { app_name: "P · A".into(), summary: title.into(), body: String::new(), reply: None, actions: vec![], sticky: false },
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
                                 shown: Shown { app_name: "x".into(), summary: "y".into(), body: String::new(), reply: None, actions: vec![], sticky: false } });
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
            shown: Shown { app_name: "x".into(), summary: "y".into(), body: String::new(), reply: None, actions: vec![], sticky: false },
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
                                 shown: Shown { app_name: "x".into(), summary: "y".into(), body: String::new(), reply: None, actions: vec![], sticky: false } });
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
    fn a_reply_reaches_the_phone_and_the_close_after_it_is_not_a_dismissal() {
        let s = scratch("replied");
        let m = Mirror::new(Box::new(Stub::new(s.0.clone())));
        let (dtx, drx) = std::sync::mpsc::channel();
        m.on_dismissed(Box::new(move |d, k| dtx.send((d.to_string(), k.to_string())).unwrap()));
        let (rtx, rrx) = std::sync::mpsc::channel();
        m.on_replied(Box::new(move |d, k, t| {
            rtx.send((d.to_string(), k.to_string(), t.to_string())).unwrap();
            Ok(())
        }));
        let post = |dev: &str, key: &str| {
            let (_, shown) = shown("P", &json!({"key": key, "app": "A", "title": "t", "reply": "Reply"})).unwrap();
            Job::Post { device: dev.into(), key: key.into(), shown }
        };
        m.handle(post("a", "chat")); // id 1
        m.handle(post("a", "other")); // id 2
        assert_eq!(lines(&s.0)[0]["reply"], "Reply");

        // Answered: the phone gets the text, cleaned; then the server closes
        // it as dismissed, which tells the phone nothing.
        m.handle(Job::Heard(Heard::Replied { id: 1, text: "on my way\u{202E}\u{7}\nsoon".into() }));
        m.handle(Job::Heard(Heard::Closed { id: 1, reason: DISMISSED }));
        assert_eq!(rrx.try_iter().collect::<Vec<_>>(), vec![("a".into(), "chat".into(), "on my way\nsoon".into())]);
        assert!(drx.try_recv().is_err(), "the close after a reply was told as a dismissal");
        assert_eq!(m.id_of("a", "chat"), None, "the closed id was kept");

        // A reply to an id not posted here, or one of nothing but control
        // characters, goes nowhere; and only the answered id is spared.
        m.handle(Job::Heard(Heard::Replied { id: 77, text: "hi".into() }));
        m.handle(Job::Heard(Heard::Replied { id: 2, text: "\u{7}".into() }));
        m.handle(Job::Heard(Heard::Closed { id: 2, reason: DISMISSED }));
        assert!(rrx.try_recv().is_err());
        assert!(drx.try_recv().is_err(), "an empty reply still counts as answered: the person used the field");

        // A new server forgets what was answered on the old one.
        m.handle(post("a", "x")); // id 3
        m.handle(Job::Heard(Heard::Replied { id: 3, text: "ok".into() }));
        std::fs::write(s.0.join("owner"), ":1.99").unwrap();
        m.handle(post("a", "y")); // new server: id 4
        assert!(m.answered().is_empty(), "{:?}", m.answered());
    }

    #[test]
    fn a_reply_that_did_not_go_is_said_here_with_the_words_and_closing_that_tells_nobody() {
        let s = scratch("notsent");
        let m = Mirror::new(Box::new(Stub::new(s.0.clone())));
        let (dtx, drx) = std::sync::mpsc::channel();
        m.on_dismissed(Box::new(move |d, k| dtx.send((d.to_string(), k.to_string())).unwrap()));
        let outcome = std::sync::Arc::new(Mutex::new(Err(NotSent::Offline)));
        let o = std::sync::Arc::clone(&outcome);
        m.on_replied(Box::new(move |_, _, _| *o.lock().unwrap()));
        let post = |key: &str| {
            let (_, shown) = shown("P", &json!({"key": key, "app": "A", "title": "t", "reply": "Reply"})).unwrap();
            Job::Post { device: "a".into(), key: key.into(), shown }
        };
        m.handle(post("k1")); // id 1
        m.handle(Job::Heard(Heard::Replied { id: 1, text: "on my way\u{7}\nsoon".into() }));
        m.handle(Job::Heard(Heard::Closed { id: 1, reason: DISMISSED }));
        let l = lines(&s.0);
        assert_eq!(l.len(), 2, "{l:?}");
        assert_eq!((l[1]["op"].as_str(), l[1]["id"].as_u64(), l[1]["replaces"].as_u64()), (Some("notify"), Some(2), Some(0)));
        assert_eq!(l[1]["summary"], "Reply not sent");
        assert_eq!(l[1]["reply"], Value::Null, "the notice has a reply field");
        let body = l[1]["body"].as_str().unwrap();
        assert!(body.contains("not connected") && body.ends_with("What you typed:\non my way\nsoon"), "{body}");

        // The notice belongs to no phone: closing it tells nobody.
        m.handle(Job::Heard(Heard::Closed { id: 2, reason: DISMISSED }));
        assert!(drx.try_recv().is_err());

        // Replying turned off says so; a reply that went posts nothing.
        *outcome.lock().unwrap() = Err(NotSent::Off);
        m.handle(post("k2")); // id 3
        m.handle(Job::Heard(Heard::Replied { id: 3, text: "x".into() }));
        assert!(lines(&s.0)[3]["body"].as_str().unwrap().contains("is off on your phone"));
        m.handle(post("k4")); // id 5
        m.handle(Job::Heard(Heard::Replied { id: 5, text: "a < b & <b>c</b>".into() }));
        let markup = lines(&s.0)[5]["body"].as_str().unwrap().to_string();
        assert!(markup.ends_with("What you typed:\na ‹ b &amp; ‹b›c‹/b›"), "{markup}");
        *outcome.lock().unwrap() = Ok(());
        m.handle(post("k3")); // id 7
        m.handle(Job::Heard(Heard::Replied { id: 7, text: "y".into() }));
        assert_eq!(lines(&s.0).len(), 7, "a reply that went was also said not to have");
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
            shown: Shown { app_name: "x".into(), summary: "y".into(), body: String::new(), reply: None, actions: vec![], sticky: false },
        };
        m.handle(post("a", "expired")); // id 1
        m.handle(post("a", "closed-by-call")); // id 2
        m.handle(post("b", "dismissed")); // id 3
        m.handle(Job::Heard(Heard::Closed { id: 1, reason: 1 }));
        m.handle(Job::Heard(Heard::Closed { id: 2, reason: 3 }));
        m.handle(Job::Heard(Heard::Closed { id: 99, reason: DISMISSED }));
        m.handle(Job::Heard(Heard::Closed { id: 3, reason: DISMISSED }));
        // A second close of the same id finds nothing.
        m.handle(Job::Heard(Heard::Closed { id: 3, reason: DISMISSED }));
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
            shown: Shown { app_name: "x".into(), summary: "y".into(), body: String::new(), reply: None, actions: vec![], sticky: false },
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

        // Answered in the reply field: the server sends NotificationReplied
        // and NotificationClosed(id, 2) back to back. The reply reaches the
        // phone and the close is not a dismissal. Ten in a row, so a reply
        // and its close handled out of order (two streams) would show.
        let (rtx, rrx) = std::sync::mpsc::channel();
        m.on_replied(Box::new(move |d, k, t| {
            rtx.send((d.to_string(), k.to_string(), t.to_string())).unwrap();
            Ok(())
        }));
        for i in 0..10 {
            let key = format!("r{i}");
            let (_, s) = shown("Pixel 8", &json!({"key": key, "app": "Signal", "title": "R", "reply": "Reply"})).unwrap();
            assert!(m.submit(Job::Post { device: "dev".into(), key: key.clone(), shown: s }));
            let deadline = Instant::now() + Duration::from_secs(5);
            let id = loop {
                if let Some(id) = m.id_of("dev", &key) {
                    break id;
                }
                assert!(Instant::now() < deadline, "{key} was never posted");
                std::thread::sleep(Duration::from_millis(10));
            };
            let text = format!("on my way {i}");
            rt.block_on(conn.call_method(Some(NAME), "/test", Some("org.rimeos.Test"), "Reply", &(id, &text))).unwrap();
            assert_eq!(rrx.recv_timeout(Duration::from_secs(5)).unwrap(), ("dev".to_string(), key.clone(), text));
            gone(&key);
        }
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err(), "an answered notification was told to the phone as dismissed");
    }
}
