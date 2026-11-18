//! What a paired phone is playing, shown on this machine as a media player
//! (`docs/remote-live-contract.md` §2.8).
//!
//! The phone sends `phone_media` with what one of its apps is playing, or
//! with `"playing": null` when nothing it may show is playing. rime-remoted
//! answers it here (never forwarded) and keeps one MPRIS player per phone on
//! the session bus, so Rime Shell's player card and anything else that reads
//! MPRIS shows it beside the computer's own players.
//!
//! ## Only the computer's players are the computer's
//!
//! Every player is named `org.mpris.MediaPlayer2.rime_phone.d<hex>`, which
//! `playerctl` lists as `rime_phone.d<hex>`, so one `--ignore-player=rime_phone`
//! leaves all of them out. The `media` verb (machine.rs) and the media keys
//! use it: the phone is never told its own song is the computer's, and a
//! media key acts on the computer.
//!
//! ## Display only
//!
//! This player says `CanControl: false` and refuses every method: controlling
//! the phone from here is a separate step.
//!
//! ## The text is somebody else's
//!
//! A title is whatever the app (or the person who uploaded the track) wrote,
//! so every field is cleaned like a notification's title ([`crate::mirror::clean`]):
//! one line, no control or bidi characters, `<` and `>` replaced, capped.
//!
//! ## Position without a ticking stream
//!
//! The phone sends where playback was when it sent, and only when something
//! changes. The computer stamps the moment it read the line and works the
//! position out from that and the rate whenever it is asked, as MPRIS
//! expects. A position that does not follow from the last one (a seek on the
//! phone) is announced with `Seeked`.
//!
//! ## Lifetime
//!
//! A player lives while the connection that last described it is open: its
//! end, revoke, or `"playing": null` removes it. A reconnect that describes it
//! again before the old connection is noticed gone keeps it.
//!
//! ## Never holds the link
//!
//! Updates are kept per phone, latest wins, and one worker applies them at
//! most every [`SPACING`] per phone, so a phone that sends a hundred updates a
//! second costs a hundred replaced map entries and a few bus signals.
//!
//! ## Tests never reach the real desktop
//!
//! [`STUB_ENV`] points the worker at a directory: each change is a JSON line
//! in `<dir>/media.jsonl`. A unit-test build never gets the real backend from
//! [`media`], whatever the environment says.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// The verbs this module answers. `serve::answered_here` routes by this list.
pub const VERBS: [&str; 1] = ["phone_media"];

/// Added to `remote_hello.features`.
pub const FEATURES: [&str; 1] = ["phone_media"];

/// A directory standing in for the session bus (tests).
pub const STUB_ENV: &str = "RIME_REMOTED_MEDIA_STUB";

/// Every phone's player is this, a dot, and `d` + the hex of its device id.
pub const PLAYER_PREFIX: &str = "org.mpris.MediaPlayer2.rime_phone";

pub const APP_CAP: usize = 60;
pub const TITLE_CAP: usize = 120;
pub const ARTIST_CAP: usize = 120;
pub const ALBUM_CAP: usize = 120;

/// Longer than any track; a bigger number is refused, not clamped.
pub const MAX_DURATION_MS: u64 = 7 * 24 * 3600 * 1000;

/// The fastest rate a phone may report.
pub const MAX_RATE: f64 = 8.0;

/// How soon after one change to a phone's player the next is applied.
pub const SPACING: Duration = Duration::from_millis(200);

/// Each bus call's budget, connecting included.
pub const BUDGET: Duration = Duration::from_secs(3);

/// How far the reported position may stray from the one worked out before it
/// counts as a seek.
pub const SEEK_SLACK_MS: i64 = 1500;

const OBJECT: &str = "/org/mpris/MediaPlayer2";

/// What the phone is playing, cleaned.
#[derive(Debug, Clone, PartialEq)]
pub struct Playing {
    /// `"<phone> · <app>"`: the player's identity.
    pub identity: String,
    pub status: Status,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// 0 when the app did not say.
    pub duration_ms: u64,
    pub position_ms: u64,
    pub rate: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Playing,
    Paused,
    Stopped,
}

impl Status {
    /// The MPRIS `PlaybackStatus`.
    pub fn mpris(self) -> &'static str {
        match self {
            Status::Playing => "Playing",
            Status::Paused => "Paused",
            Status::Stopped => "Stopped",
        }
    }
}

/// [`Playing`] and the moment this machine read it.
#[derive(Debug, Clone, PartialEq)]
pub struct Now {
    pub playing: Playing,
    pub at: Instant,
}

impl Now {
    /// Where playback is at `t`, worked out from the report.
    pub fn position_at(&self, t: Instant) -> u64 {
        let p = &self.playing;
        let mut ms = p.position_ms;
        if p.status == Status::Playing {
            let gone = t.saturating_duration_since(self.at).as_millis() as f64;
            ms = ms.saturating_add((gone * p.rate) as u64);
        }
        if p.duration_ms > 0 {
            ms = ms.min(p.duration_ms);
        }
        ms
    }
}

/// The player's bus name for a device: the id's bytes as hex, so two ids can
/// never share a name and every name is a valid bus-name element.
pub fn player_name(device_id: &str) -> String {
    let hex: String = device_id.bytes().map(|b| format!("{b:02x}")).collect();
    format!("{PLAYER_PREFIX}.d{hex}")
}

fn text<'a>(v: &'a Value, name: &str) -> Result<&'a str, String> {
    match &v[name] {
        Value::Null => Ok(""),
        Value::String(s) => Ok(s),
        _ => Err(format!("{name} must be a string")),
    }
}

fn millis(v: &Value, name: &str) -> Result<u64, String> {
    match &v[name] {
        Value::Null => Ok(0),
        n => n
            .as_u64()
            .filter(|ms| *ms <= MAX_DURATION_MS)
            .ok_or_else(|| format!("{name} must be a whole number of milliseconds, at most {MAX_DURATION_MS}")),
    }
}

/// The body's `playing`, cleaned, or why it is refused. `None` is "nothing".
pub fn playing(phone_name: &str, v: &Value) -> Result<Option<Playing>, String> {
    let p = match &v["playing"] {
        Value::Null => return Ok(None),
        p @ Value::Object(_) => p,
        _ => return Err("playing must be an object or null".into()),
    };
    let status = match p["state"].as_str() {
        Some("playing") => Status::Playing,
        Some("paused") => Status::Paused,
        Some("stopped") => Status::Stopped,
        _ => return Err("state must be playing, paused or stopped".into()),
    };
    let clean = |s: &str, cap| crate::mirror::clean(s, cap, 0, false);
    let app = clean(text(p, "app")?, APP_CAP);
    let title = clean(text(p, "title")?, TITLE_CAP);
    if title.is_empty() {
        return Err("title must not be empty".into());
    }
    let duration_ms = millis(p, "duration_ms")?;
    let mut position_ms = millis(p, "position_ms")?;
    if duration_ms > 0 {
        position_ms = position_ms.min(duration_ms);
    }
    let rate = match &p["rate"] {
        Value::Null => 1.0,
        r => r
            .as_f64()
            .filter(|r| r.is_finite() && (0.0..=MAX_RATE).contains(r))
            .ok_or_else(|| format!("rate must be a number from 0 to {MAX_RATE}"))?,
    };
    let phone = clean(phone_name, crate::mirror::APP_CAP);
    let identity = match (phone.is_empty(), app.is_empty()) {
        (false, false) => format!("{phone} · {app}"),
        (false, true) => phone,
        (true, false) => app,
        (true, true) => "Phone".into(),
    };
    Ok(Some(Playing {
        identity,
        status,
        title,
        artist: clean(text(p, "artist")?, ARTIST_CAP),
        album: clean(text(p, "album")?, ALBUM_CAP),
        duration_ms,
        position_ms,
        rate,
    }))
}

// ── Backends ────────────────────────────────────────────────────────────────

/// Where players are shown.
pub trait Backend: Send + Sync {
    /// Show or update `player` (a full bus name).
    fn show(&self, player: &str, now: &Now) -> Result<(), String>;
    /// Remove `player`; removing one not shown is fine.
    fn gone(&self, player: &str) -> Result<(), String>;
}

/// A directory standing in for the session bus.
pub struct Stub {
    dir: PathBuf,
}

impl Stub {
    pub fn new(dir: PathBuf) -> Stub {
        Stub { dir }
    }

    fn record(&self, line: Value) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("media.jsonl"))
            .map_err(|e| e.to_string())?;
        f.write_all(format!("{line}\n").as_bytes()).map_err(|e| e.to_string())
    }
}

impl Backend for Stub {
    fn show(&self, player: &str, now: &Now) -> Result<(), String> {
        let p = &now.playing;
        self.record(json!({"op": "show", "player": player, "identity": p.identity, "status": p.status.mpris(),
                           "title": p.title, "artist": p.artist, "album": p.album,
                           "duration_ms": p.duration_ms, "position_ms": p.position_ms, "rate": p.rate}))
    }

    fn gone(&self, player: &str) -> Result<(), String> {
        self.record(json!({"op": "gone", "player": player}))
    }
}

/// The session bus: one connection per phone, each owning its player's name.
pub struct Bus {
    calls: tokio::sync::mpsc::Sender<(Call, Instant)>,
}

enum Call {
    Show(String, Box<Now>, std::sync::mpsc::SyncSender<Result<(), String>>),
    Gone(String, std::sync::mpsc::SyncSender<Result<(), String>>),
}

/// The state both interfaces read.
type Shared = std::sync::Arc<Mutex<Now>>;

fn read(s: &Shared) -> Now {
    s.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

struct Root {
    now: Shared,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported("a phone's player cannot be raised here".into()))
    }
    fn quit(&self) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported("a phone's player cannot be quit from here".into()))
    }
    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn identity(&self) -> String {
        read(&self.now).playing.identity
    }
    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        vec![]
    }
    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        vec![]
    }
}

struct Player {
    now: Shared,
    /// Changes with each new track, as `mpris:trackid` must.
    track: u64,
}

fn refused() -> zbus::fdo::Error {
    zbus::fdo::Error::NotSupported("this computer shows the phone's media but does not control it".into())
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn next(&self) -> zbus::fdo::Result<()> {
        Err(refused())
    }
    fn previous(&self) -> zbus::fdo::Result<()> {
        Err(refused())
    }
    fn pause(&self) -> zbus::fdo::Result<()> {
        Err(refused())
    }
    fn play_pause(&self) -> zbus::fdo::Result<()> {
        Err(refused())
    }
    fn stop(&self) -> zbus::fdo::Result<()> {
        Err(refused())
    }
    fn play(&self) -> zbus::fdo::Result<()> {
        Err(refused())
    }
    fn seek(&self, _offset: i64) -> zbus::fdo::Result<()> {
        Err(refused())
    }
    fn set_position(&self, _track: zbus::zvariant::ObjectPath<'_>, _position: i64) -> zbus::fdo::Result<()> {
        Err(refused())
    }
    fn open_uri(&self, _uri: &str) -> zbus::fdo::Result<()> {
        Err(refused())
    }

    #[zbus(signal)]
    async fn seeked(ctxt: &zbus::SignalContext<'_>, position: i64) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> String {
        read(&self.now).playing.status.mpris().into()
    }
    #[zbus(property)]
    fn rate(&self) -> f64 {
        read(&self.now).playing.rate
    }
    #[zbus(property)]
    fn set_rate(&self, _rate: f64) -> zbus::Result<()> {
        Err(refused().into())
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, zbus::zvariant::OwnedValue> {
        metadata(&read(&self.now).playing, self.track)
    }
    #[zbus(property)]
    fn volume(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn set_volume(&self, _volume: f64) -> zbus::Result<()> {
        Err(refused().into())
    }
    /// Not announced on change, as MPRIS says: readers ask, or hear `Seeked`.
    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        let now = read(&self.now);
        (now.position_at(Instant::now()) as i64).saturating_mul(1000)
    }
    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        0.0
    }
    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        MAX_RATE
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        false
    }
    #[zbus(property(emits_changed_signal = "const"))]
    fn can_control(&self) -> bool {
        false
    }
}

/// `Metadata` for a track.
pub fn metadata(p: &Playing, track: u64) -> HashMap<String, zbus::zvariant::OwnedValue> {
    use zbus::zvariant::{ObjectPath, OwnedValue, Value as V};
    let mut m: HashMap<String, OwnedValue> = HashMap::new();
    let mut put = |k: &str, v: V<'_>| {
        if let Ok(v) = OwnedValue::try_from(v) {
            m.insert(k.into(), v);
        }
    };
    if let Ok(path) = ObjectPath::try_from(format!("/org/rimeos/RimeRemote/track/{track}")) {
        put("mpris:trackid", V::from(path));
    }
    put("xesam:title", V::from(p.title.as_str()));
    if !p.artist.is_empty() {
        put("xesam:artist", V::from(vec![p.artist.as_str()]));
    }
    if !p.album.is_empty() {
        put("xesam:album", V::from(p.album.as_str()));
    }
    if p.duration_ms > 0 {
        put("mpris:length", V::from((p.duration_ms as i64).saturating_mul(1000)));
    }
    m
}

/// Whether the reported position is not where the last report said it would be.
pub fn seeked(before: &Now, after: &Now) -> bool {
    let expected = before.position_at(after.at) as i64;
    (after.playing.position_ms as i64 - expected).abs() > SEEK_SLACK_MS
}

/// Whether two reports are the same track (so `mpris:trackid` stays).
fn same_track(a: &Playing, b: &Playing) -> bool {
    a.title == b.title && a.artist == b.artist && a.album == b.album && a.duration_ms == b.duration_ms
}

struct Shown {
    conn: zbus::Connection,
    now: Shared,
    track: u64,
}

impl Bus {
    /// Start the bus thread. Nothing touches the bus until the first call.
    pub fn start() -> std::io::Result<Bus> {
        let (calls, rx) = tokio::sync::mpsc::channel(4);
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        std::thread::Builder::new()
            .name("phone-media-bus".into())
            .spawn(move || runtime.block_on(Bus::serve(rx)))?;
        Ok(Bus { calls })
    }

    fn ask(&self, call: impl FnOnce(std::sync::mpsc::SyncSender<Result<(), String>>) -> Call) -> Result<(), String> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.calls
            .try_send((call(tx), Instant::now() + BUDGET))
            .map_err(|_| "the session bus is behind".to_string())?;
        rx.recv_timeout(BUDGET + Duration::from_millis(500))
            .unwrap_or_else(|_| Err("the session bus did not answer in time".into()))
    }

    async fn serve(mut calls: tokio::sync::mpsc::Receiver<(Call, Instant)>) {
        let mut shown: HashMap<String, Shown> = HashMap::new();
        let mut next_track: u64 = 0;
        while let Some((call, by)) = calls.recv().await {
            let by = tokio::time::Instant::from_std(by);
            match call {
                Call::Show(name, now, reply) => {
                    let got = match tokio::time::timeout_at(by, Bus::show(&mut shown, &mut next_track, &name, *now)).await {
                        Ok(r) => r,
                        Err(_) => Err("the session bus did not answer in time".into()),
                    };
                    let _ = reply.send(got);
                }
                Call::Gone(name, reply) => {
                    if let Some(s) = shown.remove(&name) {
                        let _ = tokio::time::timeout_at(by, s.conn.release_name(name.as_str())).await;
                    }
                    let _ = reply.send(Ok(()));
                }
            }
        }
    }

    async fn show(shown: &mut HashMap<String, Shown>, next_track: &mut u64, name: &str, now: Now) -> Result<(), String> {
        let err = |e: zbus::Error| e.to_string();
        if let Some(s) = shown.get_mut(name) {
            let before = read(&s.now);
            let new_track = !same_track(&before.playing, &now.playing);
            let jumped = seeked(&before, &now);
            *s.now.lock().unwrap_or_else(|e| e.into_inner()) = now.clone();
            let server = s.conn.object_server();
            if before.playing.identity != now.playing.identity {
                let root = server.interface::<_, Root>(OBJECT).await.map_err(err)?;
                root.get().await.identity_changed(root.signal_context()).await.map_err(err)?;
            }
            let player = server.interface::<_, Player>(OBJECT).await.map_err(err)?;
            if new_track {
                *next_track += 1;
                s.track = *next_track;
                player.get_mut().await.track = s.track;
            }
            let p = player.get().await;
            let ctxt = player.signal_context();
            if before.playing.status != now.playing.status {
                p.playback_status_changed(ctxt).await.map_err(err)?;
            }
            if before.playing.rate != now.playing.rate {
                p.rate_changed(ctxt).await.map_err(err)?;
            }
            if new_track || before.playing != now.playing {
                p.metadata_changed(ctxt).await.map_err(err)?;
            }
            if new_track || jumped {
                let at = (now.position_at(Instant::now()) as i64).saturating_mul(1000);
                Player::seeked(ctxt, at).await.map_err(err)?;
            }
            return Ok(());
        }
        *next_track += 1;
        let track = *next_track;
        let shared: Shared = std::sync::Arc::new(Mutex::new(now));
        let conn = zbus::connection::Builder::session()
            .map_err(err)?
            .serve_at(OBJECT, Root { now: shared.clone() })
            .map_err(err)?
            .serve_at(OBJECT, Player { now: shared.clone(), track })
            .map_err(err)?
            .name(name)
            .map_err(err)?
            .build()
            .await
            .map_err(err)?;
        shown.insert(name.to_string(), Shown { conn, now: shared, track });
        Ok(())
    }
}

impl Backend for Bus {
    fn show(&self, player: &str, now: &Now) -> Result<(), String> {
        self.ask(|r| Call::Show(player.to_string(), Box::new(now.clone()), r))
    }

    fn gone(&self, player: &str) -> Result<(), String> {
        self.ask(|r| Call::Gone(player.to_string(), r))
    }
}

// ── The worker ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Pending {
    Show(Now),
    Gone,
}

#[derive(Default)]
struct Book {
    /// The latest change per phone, not yet applied.
    pending: HashMap<String, Pending>,
    /// The connection that last described each phone's player.
    owner: HashMap<String, u64>,
    /// When each phone's player last changed here.
    applied: HashMap<String, Instant>,
}

pub struct Media {
    backend: Box<dyn Backend>,
    book: Mutex<Book>,
    wake: Condvar,
}

impl Media {
    pub fn new(backend: Box<dyn Backend>) -> Media {
        Media { backend, book: Mutex::new(Book::default()), wake: Condvar::new() }
    }

    fn book(&self) -> std::sync::MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// What `connection` says `device` is playing now (`None`: nothing).
    pub fn set(&self, device: &str, connection: u64, playing: Option<Playing>, at: Instant) {
        let mut b = self.book();
        b.owner.insert(device.to_string(), connection);
        let next = match playing {
            Some(playing) => Pending::Show(Now { playing, at }),
            None => Pending::Gone,
        };
        b.pending.insert(device.to_string(), next);
        drop(b);
        self.wake.notify_all();
    }

    /// `connection` ended: its phone's player goes, unless a newer connection
    /// has described it since.
    pub fn ended(&self, device: &str, connection: u64) {
        let mut b = self.book();
        if b.owner.get(device) == Some(&connection) {
            b.owner.remove(device);
            b.pending.insert(device.to_string(), Pending::Gone);
            drop(b);
            self.wake.notify_all();
        }
    }

    /// Revoke: the player goes whoever described it.
    pub fn forget(&self, device: &str) {
        let mut b = self.book();
        b.owner.remove(device);
        b.pending.insert(device.to_string(), Pending::Gone);
        drop(b);
        self.wake.notify_all();
    }

    /// The next change due, waiting for one; `None` once `stop` says so.
    fn next(&self, stop: &dyn Fn() -> bool) -> Option<(String, Pending)> {
        let mut b = self.book();
        loop {
            if stop() {
                return None;
            }
            let now = Instant::now();
            let mut soonest: Option<Instant> = None;
            let mut due: Option<String> = None;
            for device in b.pending.keys() {
                let at = b.applied.get(device).map(|t| *t + SPACING).unwrap_or(now);
                if at <= now {
                    due = Some(device.clone());
                    break;
                }
                soonest = Some(soonest.map_or(at, |s: Instant| s.min(at)));
            }
            if let Some(device) = due {
                let p = b.pending.remove(&device)?;
                b.applied.insert(device.clone(), now);
                return Some((device, p));
            }
            let wait = soonest.map(|s| s.saturating_duration_since(now)).unwrap_or(Duration::from_secs(60));
            b = self.wake.wait_timeout(b, wait).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    fn apply(&self, device: &str, p: Pending) {
        let name = player_name(device);
        let got = match &p {
            Pending::Show(now) => self.backend.show(&name, now),
            Pending::Gone => self.backend.gone(&name),
        };
        if let Err(e) = got {
            eprintln!("rime-remoted: a phone's media could not be shown: {e}");
        }
    }

    /// Apply every change due now (tests).
    #[cfg(test)]
    fn drain(&self) {
        loop {
            let ready = {
                let b = self.book();
                !b.pending.is_empty()
            };
            if !ready {
                return;
            }
            match self.next(&|| false) {
                Some((d, p)) => self.apply(&d, p),
                None => return,
            }
        }
    }
}

/// The media this process uses, with its worker running.
pub fn media() -> &'static Media {
    static CHOSEN: OnceLock<&'static Media> = OnceLock::new();
    CHOSEN.get_or_init(|| {
        let stub = match std::env::var_os(STUB_ENV) {
            Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
            _ if cfg!(test) => Some(std::env::temp_dir().join(format!("rime-remoted-media-unit-{}", std::process::id()))),
            _ => None,
        };
        let backend: std::io::Result<Box<dyn Backend>> = match stub {
            Some(dir) => Ok(Box::new(Stub::new(dir))),
            None => Bus::start().map(|b| Box::new(b) as Box<dyn Backend>),
        };
        let m: &'static Media = match backend {
            Ok(b) => Box::leak(Box::new(Media::new(b))),
            Err(e) => {
                eprintln!("rime-remoted: a phone's media will not show: no bus thread ({e})");
                Box::leak(Box::new(Media::new(Box::new(Stub::new(PathBuf::from("/nonexistent"))))))
            }
        };
        let started = std::thread::Builder::new().name("phone-media".into()).spawn(move || {
            while let Some((d, p)) = m.next(&|| false) {
                m.apply(&d, p);
            }
        });
        if let Err(e) = started {
            eprintln!("rime-remoted: a phone's media will not show: no worker thread ({e})");
        }
        m
    })
}

/// Answer one of [`VERBS`]. The device is the handshake's, `connection` the
/// one it came on; the name is the store's, never a body field.
pub fn control(m: &Media, device_id: &str, connection: u64, phone_name: &str, line: &[u8]) -> Vec<u8> {
    let v: Value = match serde_json::from_slice(line) {
        Ok(v) => v,
        Err(_) => return error("bad_request", "that request is not JSON"),
    };
    if v["cmd"].as_str() != Some("phone_media") {
        return error("bad_request", "not a media verb");
    }
    match playing(phone_name, &v) {
        Ok(p) => {
            m.set(device_id, connection, p, Instant::now());
            json!({"reply": "ok"}).to_string().into_bytes()
        }
        Err(why) => error("bad_request", &why),
    }
}

fn error(code: &str, message: &str) -> Vec<u8> {
    json!({"reply": "error", "code": code, "message": message}).to_string().into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Seen(Mutex<Vec<(String, Option<Now>)>>);
    impl Backend for &'static Seen {
        fn show(&self, player: &str, now: &Now) -> Result<(), String> {
            self.0.lock().unwrap().push((player.into(), Some(now.clone())));
            Ok(())
        }
        fn gone(&self, player: &str) -> Result<(), String> {
            self.0.lock().unwrap().push((player.into(), None));
            Ok(())
        }
    }

    fn seen() -> (&'static Seen, Media) {
        let s: &'static Seen = Box::leak(Box::default());
        (s, Media::new(Box::new(s)))
    }

    /// What `playerctl --ignore-player=` takes to leave out every phone: the
    /// prefix's last element, which playerctl reports as the player's name.
    fn ignore() -> &'static str {
        PLAYER_PREFIX.rsplit('.').next().unwrap()
    }

    fn body(p: Value) -> Value {
        json!({"cmd": "phone_media", "playing": p})
    }

    fn song() -> Value {
        json!({"app": "Music", "state": "playing", "title": "Song", "artist": "Band", "album": "LP",
               "duration_ms": 200_000, "position_ms": 10_000, "rate": 1.0})
    }

    #[test]
    fn every_field_is_cleaned_like_a_title_and_named_for_the_phone() {
        let p = playing(
            "Pixel <8>",
            &body(json!({"app": "Mu\u{202E}sic", "state": "paused", "title": "<b>A</b>\nB\u{0007}",
                         "artist": "x".repeat(300), "album": "<img src=x>"})),
        )
        .unwrap()
        .unwrap();
        assert_eq!(p.identity, "Pixel ‹8› · Music");
        assert_eq!(p.title, "‹b›A‹/b› B");
        assert_eq!(p.artist.chars().count(), ARTIST_CAP + 1, "capped with an ellipsis");
        assert!(p.artist.ends_with('…'));
        assert_eq!(p.album, "‹img src=x›");
        assert_eq!((p.status, p.duration_ms, p.position_ms, p.rate), (Status::Paused, 0, 0, 1.0));
    }

    #[test]
    fn a_bad_body_is_refused_and_null_means_nothing_playing() {
        assert_eq!(playing("P", &body(Value::Null)), Ok(None));
        for bad in [
            json!("x"),
            json!({"state": "playing"}),
            json!({"state": "loud", "title": "t"}),
            json!({"state": "playing", "title": "t", "duration_ms": -1}),
            json!({"state": "playing", "title": "t", "duration_ms": MAX_DURATION_MS + 1}),
            json!({"state": "playing", "title": "t", "position_ms": 1.5}),
            json!({"state": "playing", "title": "t", "rate": 9.0}),
            json!({"state": "playing", "title": "t", "rate": -1}),
            json!({"state": "playing", "title": "t", "artist": 3}),
            json!({"state": "playing", "title": "\u{0007}"}),
        ] {
            assert!(playing("P", &body(bad.clone())).is_err(), "{bad}");
        }
        let p = playing("P", &body(json!({"state": "playing", "title": "t", "duration_ms": 1000, "position_ms": 5000})))
            .unwrap()
            .unwrap();
        assert_eq!(p.position_ms, 1000, "a position past the end is the end");
    }

    #[test]
    fn the_position_follows_the_clock_only_while_playing_and_stops_at_the_end() {
        let at = Instant::now();
        let p = playing("P", &body(song())).unwrap().unwrap();
        let now = Now { playing: p.clone(), at };
        assert_eq!(now.position_at(at), 10_000);
        assert_eq!(now.position_at(at + Duration::from_secs(5)), 15_000);
        assert_eq!(now.position_at(at + Duration::from_secs(500)), 200_000);
        let paused = Now { playing: Playing { status: Status::Paused, ..p.clone() }, at };
        assert_eq!(paused.position_at(at + Duration::from_secs(5)), 10_000);
        let fast = Now { playing: Playing { rate: 2.0, ..p }, at };
        assert_eq!(fast.position_at(at + Duration::from_secs(5)), 20_000);
    }

    #[test]
    fn a_report_where_the_clock_says_is_not_a_seek_and_a_jump_is() {
        let at = Instant::now();
        let p = playing("P", &body(song())).unwrap().unwrap();
        let before = Now { playing: p.clone(), at };
        let later = at + Duration::from_secs(10);
        let on_time = Now { playing: Playing { position_ms: 20_400, ..p.clone() }, at: later };
        let jumped = Now { playing: Playing { position_ms: 90_000, ..p }, at: later };
        assert!(!seeked(&before, &on_time));
        assert!(seeked(&before, &jumped));
    }

    #[test]
    fn player_names_are_valid_bus_names_and_never_shared() {
        let a = player_name("AbC+/=_-x");
        let b = player_name("AbC/+=_-x");
        assert_ne!(a, b);
        for n in [&a, &b] {
            assert!(zbus::names::WellKnownName::try_from(n.as_str()).is_ok(), "{n}");
            assert!(n.starts_with("org.mpris.MediaPlayer2.rime_phone.d"));
        }
        assert_eq!(crate::machine::IGNORE_PHONES, format!("--ignore-player={}", ignore()), "the media verb leaves phones out");
    }

    #[test]
    fn metadata_says_what_mpris_readers_look_for() {
        let p = playing("P", &body(song())).unwrap().unwrap();
        let m = metadata(&p, 3);
        assert_eq!(String::try_from(m["xesam:title"].try_clone().unwrap()).unwrap(), "Song");
        assert_eq!(i64::try_from(m["mpris:length"].try_clone().unwrap()).unwrap(), 200_000_000);
        assert!(m.contains_key("mpris:trackid") && m.contains_key("xesam:artist") && m.contains_key("xesam:album"));
        let bare = metadata(&Playing { artist: String::new(), album: String::new(), duration_ms: 0, ..p }, 4);
        assert!(!bare.contains_key("xesam:artist") && !bare.contains_key("mpris:length"));
    }

    #[test]
    fn the_latest_report_wins_and_a_burst_costs_one_change() {
        let (seen, m) = seen();
        let mut p = playing("P", &body(song())).unwrap().unwrap();
        for i in 0..100 {
            p.position_ms = i;
            m.set("dev", 1, Some(p.clone()), Instant::now());
        }
        m.drain();
        let got = seen.0.lock().unwrap().clone();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].1.as_ref().unwrap().playing.position_ms, 99);
    }

    #[test]
    fn a_phones_changes_are_spaced_and_another_phone_does_not_wait() {
        let (seen, m) = seen();
        let p = playing("P", &body(song())).unwrap().unwrap();
        m.set("a", 1, Some(p.clone()), Instant::now());
        m.drain();
        let t = Instant::now();
        m.set("a", 1, None, Instant::now());
        m.set("b", 2, Some(p), Instant::now());
        m.drain();
        let waited = t.elapsed();
        let got: Vec<_> = seen.0.lock().unwrap().iter().map(|(n, s)| (n.clone(), s.is_some())).collect();
        assert_eq!(got, vec![(player_name("a"), true), (player_name("b"), true), (player_name("a"), false)]);
        assert!(waited >= SPACING - Duration::from_millis(20), "{waited:?}");
    }

    #[test]
    fn the_end_of_an_old_connection_leaves_a_reconnected_phones_player() {
        let (seen, m) = seen();
        let p = playing("P", &body(song())).unwrap().unwrap();
        m.set("dev", 1, Some(p.clone()), Instant::now());
        m.drain();
        m.set("dev", 2, Some(p), Instant::now());
        m.ended("dev", 1);
        m.drain();
        assert!(seen.0.lock().unwrap().iter().all(|(_, s)| s.is_some()), "still shown");
        m.ended("dev", 2);
        m.drain();
        assert!(seen.0.lock().unwrap().last().unwrap().1.is_none(), "gone with its connection");
    }

    #[test]
    fn revoke_removes_the_player_whoever_described_it() {
        let (seen, m) = seen();
        let p = playing("P", &body(song())).unwrap().unwrap();
        m.set("dev", 7, Some(p), Instant::now());
        m.drain();
        m.forget("dev");
        m.drain();
        assert_eq!(seen.0.lock().unwrap().last().unwrap(), &(player_name("dev"), None));
        m.ended("dev", 7);
        m.drain();
        assert_eq!(seen.0.lock().unwrap().len(), 2, "nothing more after revoke");
    }

    #[test]
    fn control_answers_ok_or_bad_request_and_records_through_the_stub() {
        let dir = std::env::temp_dir().join(format!("rime-media-stub-{}-{}", std::process::id(), rime_remote_core::now_ms()));
        let m = Media::new(Box::new(Stub::new(dir.clone())));
        let ok = control(&m, "dev", 1, "Pixel", body(song()).to_string().as_bytes());
        assert_eq!(serde_json::from_slice::<Value>(&ok).unwrap()["reply"], "ok");
        let bad = control(&m, "dev", 1, "Pixel", br#"{"cmd":"phone_media","playing":{"state":"x"}}"#);
        assert_eq!(serde_json::from_slice::<Value>(&bad).unwrap()["code"], "bad_request");
        let other = control(&m, "dev", 1, "Pixel", br#"{"cmd":"phone_notification"}"#);
        assert_eq!(serde_json::from_slice::<Value>(&other).unwrap()["code"], "bad_request");
        m.drain();
        let line: Value = serde_json::from_str(std::fs::read_to_string(dir.join("media.jsonl")).unwrap().lines().next().unwrap()).unwrap();
        assert_eq!(line["op"], "show");
        assert_eq!(line["player"], player_name("dev"));
        assert_eq!(line["identity"], "Pixel · Music");
        assert_eq!(line["status"], "Playing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real backend on a private session bus (tests/test-remoted-media-bus.sh
    /// starts one and points DBUS_SESSION_BUS_ADDRESS at it). Fails without it,
    /// so a run that forgot the bus cannot pass by skipping.
    #[test]
    #[ignore]
    fn the_bus_backend_exports_a_player_mpris_readers_read_and_playerctl_leaves_out() {
        let addr = std::env::var("DBUS_SESSION_BUS_ADDRESS").expect("a private session bus");
        assert!(std::env::var("RIME_MEDIA_BUS_PRIVATE").is_ok_and(|v| v == addr), "refusing a bus the script did not start");
        let run = |cmd: &str, args: &[&str]| {
            let o = std::process::Command::new(cmd).args(args).output().unwrap();
            (o.status.success(), String::from_utf8_lossy(&o.stdout).trim().to_string(), String::from_utf8_lossy(&o.stderr).trim().to_string())
        };
        let bus = Bus::start().unwrap();
        let name = player_name("dev-1");
        let short = name.trim_start_matches("org.mpris.MediaPlayer2.").to_string();
        // A player of the computer's own, so "leaves the phone out" is also
        // "keeps the computer's".
        let computer = "org.mpris.MediaPlayer2.computer";
        let p = playing("Pixel", &body(song())).unwrap().unwrap();
        bus.show(computer, &Now { playing: p.clone(), at: Instant::now() }).unwrap();
        bus.show(&name, &Now { playing: p.clone(), at: Instant::now() }).unwrap();
        let get = |iface: &str, prop: &str| {
            let (ok, out, err) =
                run("busctl", &["--user", "--json=short", "get-property", &name, OBJECT, iface, prop]);
            assert!(ok, "{prop}: {err}");
            serde_json::from_str::<Value>(&out).unwrap()["data"].clone()
        };
        let player = "org.mpris.MediaPlayer2.Player";
        assert_eq!(get("org.mpris.MediaPlayer2", "Identity"), "Pixel · Music");
        assert_eq!(get(player, "PlaybackStatus"), "Playing");
        assert_eq!(get(player, "CanControl"), false);
        let m = get(player, "Metadata");
        assert_eq!(m["xesam:title"]["data"], "Song", "{m}");
        assert_eq!(m["xesam:artist"]["data"], json!(["Band"]), "{m}");
        assert_eq!(m["xesam:album"]["data"], "LP", "{m}");
        assert_eq!(m["mpris:length"]["data"], 200_000_000, "{m}");
        let first = get(player, "Position").as_i64().unwrap();
        std::thread::sleep(Duration::from_millis(1200));
        let second = get(player, "Position").as_i64().unwrap();
        assert!(first >= 10_000_000 && (1_000_000..2_500_000).contains(&(second - first)), "{first} {second}");
        // Display only: every control is refused and nothing changes.
        let (ok, _, err) = run("busctl", &["--user", "call", &name, OBJECT, player, "PlayPause"]);
        assert!(!ok && (err.contains("NotSupported") || err.contains("not control")), "{err}");
        assert_eq!(get(player, "PlaybackStatus"), "Playing");
        // playerctl: listed by its short name, left out by one --ignore-player,
        // and never acted on (it skips a player that says it cannot be controlled).
        let (_, all, _) = run("playerctl", &["-l"]);
        assert!(all.lines().any(|l| l == short) && all.lines().any(|l| l == "computer"), "{all}");
        let (_, kept, _) = run("playerctl", &["-l", crate::machine::IGNORE_PHONES]);
        assert_eq!(kept.lines().collect::<Vec<_>>(), ["computer"], "one name leaves every phone out and keeps the rest");
        let (acted, _, _) = run("playerctl", &["-p", &short, "play-pause"]);
        assert!(!acted, "playerctl does not act on a player that cannot be controlled");
        // An update shows.
        bus.show(&name, &Now { playing: Playing { status: Status::Paused, title: "Other".into(), ..p }, at: Instant::now() })
            .unwrap();
        assert_eq!(get(player, "PlaybackStatus"), "Paused");
        assert_eq!(get(player, "Metadata")["xesam:title"]["data"], "Other");
        // Removal takes it off the bus.
        bus.gone(&name).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let (_, after, _) = run("playerctl", &["-l"]);
        assert!(!after.lines().any(|l| l == short) && after.lines().any(|l| l == "computer"), "{after}");
    }
}
