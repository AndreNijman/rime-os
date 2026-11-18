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
//! ## Controlled only as far as the phone allows
//!
//! A report may carry `can`: what the phone's media session lets anybody do
//! (play, pause, next, previous, seek), sent only while the person has
//! "Control from your computer" on. With an empty `can` the player says
//! `CanControl: false` and refuses every method, as the display-only player
//! did. Otherwise each `Can*` follows `can`, and a method turns into one
//! `media_control` line on the phone's `device_events` channel
//! ([`Media::call`], the one place that decides, for the bus and the stub
//! alike). A seek always crosses as an absolute position the phone can apply
//! without knowing this computer's clock. Nothing is sent to a phone that has
//! no events channel open: the caller gets an error, not a silent success.
//! `Raise`, `Quit`, `OpenUri`, `Rate` and `Volume` stay refused.
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
//! in `<dir>/media.jsonl`, and each complete line appended to
//! `<dir>/control.jsonl` is a method call on a player (`{"player":…,
//! "method":"PlayPause"}`), answered through [`Media::call`] with its outcome
//! written back to `media.jsonl` as `{"op":"call",…,"result":…}`. A
//! unit-test build never gets the real backend from [`media`], whatever the
//! environment says.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// The verbs this module answers. `serve::answered_here` routes by this list.
pub const VERBS: [&str; 1] = ["phone_media"];

/// Added to `remote_hello.features`: the player, and controlling it.
pub const FEATURES: [&str; 2] = ["phone_media", CONTROL_FEATURE];

/// A phone sends `can` only to a machine whose hello lists this. This
/// machine keeps no per-connection record of what it offered (every build
/// that has this code offers it), so a `can` on the wire IS the negotiation,
/// as a notification's `reply` label is.
pub const CONTROL_FEATURE: &str = "phone_media_control";

/// The event a method call becomes, on the phone's `device_events` channel.
pub const CONTROL_EVENT: &str = "media_control";

/// The most words a report's `can` may list.
pub const MAX_CAN: usize = 8;

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
    /// What the phone lets this computer do; empty is display only.
    pub can: Can,
}

/// What the phone's session lets this computer do (a report's `can`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Can {
    pub play: bool,
    pub pause: bool,
    pub next: bool,
    pub previous: bool,
    pub seek: bool,
}

impl Can {
    /// Whether anything at all may be done: the player's `CanControl`.
    pub fn any(self) -> bool {
        self.play || self.pause || self.next || self.previous || self.seek
    }

    /// The words, in a fixed order (the stub's record).
    pub fn words(self) -> Vec<&'static str> {
        [(self.play, "play"), (self.pause, "pause"), (self.next, "next"), (self.previous, "previous"), (self.seek, "seek")]
            .into_iter()
            .filter_map(|(on, w)| on.then_some(w))
            .collect()
    }

    /// A report's `can`. Words this build does not know are ignored, so a
    /// newer phone does not lose control of an older computer.
    fn of(v: &Value) -> Result<Can, String> {
        let words = match v {
            Value::Null => return Ok(Can::default()),
            Value::Array(w) if w.len() <= MAX_CAN => w,
            _ => return Err(format!("can must be a list of at most {MAX_CAN} words")),
        };
        let mut c = Can::default();
        for w in words {
            match w.as_str() {
                Some("play") => c.play = true,
                Some("pause") => c.pause = true,
                Some("next") => c.next = true,
                Some("previous") => c.previous = true,
                Some("seek") => c.seek = true,
                Some(_) => {}
                None => return Err("can must be a list of words".into()),
            }
        }
        Ok(c)
    }
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
        can: Can::of(&p["can"])?,
    }))
}

/// `mpris:trackid` for track number `track`.
pub fn track_path(track: u64) -> String {
    format!("/org/rimeos/RimeRemote/track/{track}")
}

/// An MPRIS method on a phone's player.
#[derive(Debug, Clone, PartialEq)]
pub enum Method {
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    /// By this many microseconds, either way.
    Seek(i64),
    /// To this many microseconds into the track with this `mpris:trackid`.
    SetPosition(String, i64),
}

impl Method {
    /// The MPRIS name (the stub's record).
    pub fn name(&self) -> &'static str {
        match self {
            Method::Play => "Play",
            Method::Pause => "Pause",
            Method::PlayPause => "PlayPause",
            Method::Stop => "Stop",
            Method::Next => "Next",
            Method::Previous => "Previous",
            Method::Seek(_) => "Seek",
            Method::SetPosition(..) => "SetPosition",
        }
    }

    /// A stub `control.jsonl` line: `method`, and `offset_us` for Seek,
    /// `track` and `position_us` for SetPosition.
    pub fn of_stub(v: &Value) -> Option<Method> {
        Some(match v["method"].as_str()? {
            "Play" => Method::Play,
            "Pause" => Method::Pause,
            "PlayPause" => Method::PlayPause,
            "Stop" => Method::Stop,
            "Next" => Method::Next,
            "Previous" => Method::Previous,
            "Seek" => Method::Seek(v["offset_us"].as_i64()?),
            "SetPosition" => Method::SetPosition(v["track"].as_str()?.to_string(), v["position_us"].as_i64()?),
            _ => return None,
        })
    }
}

/// Why a method was not passed on.
#[derive(Debug, Clone, PartialEq)]
pub enum Refused {
    /// The phone does not allow it (MPRIS `NotSupported`).
    NotSupported(String),
    /// Allowed, but it could not reach the phone.
    Failed(String),
}

impl Refused {
    /// The stub's record.
    pub fn word(&self) -> &'static str {
        match self {
            Refused::NotSupported(_) => "not_supported",
            Refused::Failed(_) => "failed",
        }
    }
}

impl From<Refused> for zbus::fdo::Error {
    fn from(r: Refused) -> Self {
        match r {
            Refused::NotSupported(m) => zbus::fdo::Error::NotSupported(m),
            Refused::Failed(m) => zbus::fdo::Error::Failed(m),
        }
    }
}

/// What `method` on the player showing `now` (track number `track`) sends
/// the phone at `at`: a `media_control` line, nothing (MPRIS says to ignore
/// it: a stale `SetPosition`, `Stop` while not playing), or a refusal.
///
/// The phone's `can` decides; `PlayPause` needs whichever of play and pause
/// the current status calls for, and the phone resolves it by its own,
/// fresher state. A seek crosses as an absolute `position_ms`: `Seek` is the
/// position worked out for `at` plus the offset, clamped to the track (or to
/// [`MAX_DURATION_MS`] when its length is unknown). `SetPosition` outside the
/// track is ignored, as MPRIS says.
pub fn decide(now: &Now, track: u64, method: &Method, at: Instant) -> Result<Option<Value>, Refused> {
    let p = &now.playing;
    let c = p.can;
    let send = |action: &str| Ok(Some(json!({"event": CONTROL_EVENT, "action": action})));
    let seek = |ms: u64| Ok(Some(json!({"event": CONTROL_EVENT, "action": "seek", "position_ms": ms})));
    let refuse = |what: &str| Err(Refused::NotSupported(format!("the phone does not let this computer {what}")));
    let end = if p.duration_ms > 0 { p.duration_ms } else { MAX_DURATION_MS };
    match method {
        Method::Play if c.play => send("play"),
        Method::Play => refuse("play"),
        Method::Pause if c.pause => send("pause"),
        Method::Pause => refuse("pause"),
        Method::PlayPause => {
            let allowed = if p.status == Status::Playing { c.pause } else { c.play };
            if allowed { send("play_pause") } else { refuse("play or pause") }
        }
        Method::Stop if p.status == Status::Playing => {
            if c.pause { send("pause") } else { refuse("stop") }
        }
        Method::Stop => {
            if c.any() { Ok(None) } else { refuse("stop") }
        }
        Method::Next if c.next => send("next"),
        Method::Next => refuse("skip"),
        Method::Previous if c.previous => send("previous"),
        Method::Previous => refuse("go back"),
        Method::Seek(_) | Method::SetPosition(..) if !c.seek => refuse("seek"),
        Method::Seek(offset_us) => {
            let target = (now.position_at(at) as i64).saturating_add(offset_us / 1000);
            seek((target.max(0) as u64).min(end))
        }
        Method::SetPosition(id, us) => {
            let ms = us / 1000;
            if *id != track_path(track) || *us < 0 || ms as u64 > end {
                return Ok(None);
            }
            seek(ms as u64)
        }
    }
}

// ── Backends ────────────────────────────────────────────────────────────────

/// Where players are shown.
pub trait Backend: Send + Sync {
    /// Show or update `player` (a full bus name), playing track number
    /// `track` (a new number is a new track, for `mpris:trackid`).
    fn show(&self, player: &str, now: &Now, track: u64) -> Result<(), String>;
    /// Remove `player`; removing one not shown is fine.
    fn gone(&self, player: &str) -> Result<(), String>;
}

/// Who answers a method call on a player: [`Media::call`] in the daemon.
pub type Caller = Arc<dyn Fn(&str, Method) -> Result<(), Refused> + Send + Sync>;

/// A directory standing in for the session bus.
pub struct Stub {
    dir: PathBuf,
}

/// One JSON line appended to `<dir>/media.jsonl`, in one write.
fn record_in(dir: &std::path::Path, line: Value) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("media.jsonl"))
        .map_err(|e| e.to_string())?;
    f.write_all(format!("{line}\n").as_bytes()).map_err(|e| e.to_string())
}

impl Stub {
    pub fn new(dir: PathBuf) -> Stub {
        Stub { dir }
    }

    fn record(&self, line: Value) -> Result<(), String> {
        record_in(&self.dir, line)
    }

    /// The stub's stand-in for method calls: complete lines appended to
    /// `<dir>/control.jsonl` (`{"player":…,"method":…}`, see
    /// [`Method::of_stub`]), read every 100 ms and handed to `caller`; each
    /// outcome goes to `media.jsonl` as `{"op":"call","player","method",
    /// "result":"ok"|"not_supported"|"failed"|"bad_line"}`.
    pub fn watch_control(dir: PathBuf, caller: Caller) -> std::io::Result<()> {
        use std::io::{Read, Seek};
        std::thread::Builder::new().name("phone-media-stub".into()).spawn(move || {
            let path = dir.join("control.jsonl");
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
                    let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                    let player = v["player"].as_str().unwrap_or("").to_string();
                    let (method, result) = match Method::of_stub(&v) {
                        Some(m) => (m.name(), match caller(&player, m) {
                            Ok(()) => "ok",
                            Err(r) => r.word(),
                        }),
                        None => ("", "bad_line"),
                    };
                    let _ = record_in(&dir, json!({"op": "call", "player": player, "method": method, "result": result}));
                }
            }
        })?;
        Ok(())
    }
}

impl Backend for Stub {
    fn show(&self, player: &str, now: &Now, track: u64) -> Result<(), String> {
        let p = &now.playing;
        self.record(json!({"op": "show", "player": player, "identity": p.identity, "status": p.status.mpris(),
                           "title": p.title, "artist": p.artist, "album": p.album,
                           "duration_ms": p.duration_ms, "position_ms": p.position_ms, "rate": p.rate,
                           "track": track_path(track), "can": p.can.words(), "can_control": p.can.any()}))
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
    Show(String, Box<Now>, u64, std::sync::mpsc::SyncSender<Result<(), String>>),
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
    /// This player's bus name, which is how [`Media::call`] finds its phone.
    name: String,
    call: Caller,
}

fn refused() -> zbus::fdo::Error {
    zbus::fdo::Error::NotSupported("a phone's player does not do that from this computer".into())
}

impl Player {
    fn ask(&self, m: Method) -> zbus::fdo::Result<()> {
        (self.call)(&self.name, m).map_err(Into::into)
    }
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn next(&self) -> zbus::fdo::Result<()> {
        self.ask(Method::Next)
    }
    fn previous(&self) -> zbus::fdo::Result<()> {
        self.ask(Method::Previous)
    }
    fn pause(&self) -> zbus::fdo::Result<()> {
        self.ask(Method::Pause)
    }
    fn play_pause(&self) -> zbus::fdo::Result<()> {
        self.ask(Method::PlayPause)
    }
    fn stop(&self) -> zbus::fdo::Result<()> {
        self.ask(Method::Stop)
    }
    fn play(&self) -> zbus::fdo::Result<()> {
        self.ask(Method::Play)
    }
    fn seek(&self, offset: i64) -> zbus::fdo::Result<()> {
        self.ask(Method::Seek(offset))
    }
    fn set_position(&self, track: zbus::zvariant::ObjectPath<'_>, position: i64) -> zbus::fdo::Result<()> {
        self.ask(Method::SetPosition(track.as_str().to_string(), position))
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
        read(&self.now).playing.can.next
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        read(&self.now).playing.can.previous
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        read(&self.now).playing.can.play
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        read(&self.now).playing.can.pause
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        read(&self.now).playing.can.seek
    }
    /// Announced when it changes: the person turns "Control from your
    /// computer" on and off on the phone while the player lives.
    #[zbus(property)]
    fn can_control(&self) -> bool {
        read(&self.now).playing.can.any()
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
    if let Ok(path) = ObjectPath::try_from(track_path(track)) {
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
pub fn same_track(a: &Playing, b: &Playing) -> bool {
    a.title == b.title && a.artist == b.artist && a.album == b.album && a.duration_ms == b.duration_ms
}

struct Shown {
    conn: zbus::Connection,
    now: Shared,
    track: u64,
}

impl Bus {
    /// Start the bus thread. Nothing touches the bus until the first call.
    /// Method calls on the players go to `call`.
    pub fn start(call: Caller) -> std::io::Result<Bus> {
        let (calls, rx) = tokio::sync::mpsc::channel(4);
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        std::thread::Builder::new()
            .name("phone-media-bus".into())
            .spawn(move || runtime.block_on(Bus::serve(rx, call)))?;
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

    async fn serve(mut calls: tokio::sync::mpsc::Receiver<(Call, Instant)>, caller: Caller) {
        let mut shown: HashMap<String, Shown> = HashMap::new();
        while let Some((call, by)) = calls.recv().await {
            let by = tokio::time::Instant::from_std(by);
            match call {
                Call::Show(name, now, track, reply) => {
                    let got = match tokio::time::timeout_at(by, Bus::show(&mut shown, &caller, &name, *now, track)).await {
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

    async fn show(shown: &mut HashMap<String, Shown>, caller: &Caller, name: &str, now: Now, track: u64) -> Result<(), String> {
        let err = |e: zbus::Error| e.to_string();
        if let Some(s) = shown.get_mut(name) {
            let before = read(&s.now);
            let new_track = s.track != track;
            let jumped = seeked(&before, &now);
            *s.now.lock().unwrap_or_else(|e| e.into_inner()) = now.clone();
            let server = s.conn.object_server();
            if before.playing.identity != now.playing.identity {
                let root = server.interface::<_, Root>(OBJECT).await.map_err(err)?;
                root.get().await.identity_changed(root.signal_context()).await.map_err(err)?;
            }
            let player = server.interface::<_, Player>(OBJECT).await.map_err(err)?;
            if new_track {
                s.track = track;
                player.get_mut().await.track = track;
            }
            let p = player.get().await;
            let ctxt = player.signal_context();
            if before.playing.status != now.playing.status {
                p.playback_status_changed(ctxt).await.map_err(err)?;
            }
            if before.playing.rate != now.playing.rate {
                p.rate_changed(ctxt).await.map_err(err)?;
            }
            // `can` is not metadata: it has its own properties, below.
            let quiet = |p: &Playing| Playing { can: Can::default(), ..p.clone() };
            if new_track || quiet(&before.playing) != quiet(&now.playing) {
                p.metadata_changed(ctxt).await.map_err(err)?;
            }
            let (was, is) = (before.playing.can, now.playing.can);
            if was.any() != is.any() {
                p.can_control_changed(ctxt).await.map_err(err)?;
            }
            if was.play != is.play {
                p.can_play_changed(ctxt).await.map_err(err)?;
            }
            if was.pause != is.pause {
                p.can_pause_changed(ctxt).await.map_err(err)?;
            }
            if was.next != is.next {
                p.can_go_next_changed(ctxt).await.map_err(err)?;
            }
            if was.previous != is.previous {
                p.can_go_previous_changed(ctxt).await.map_err(err)?;
            }
            if was.seek != is.seek {
                p.can_seek_changed(ctxt).await.map_err(err)?;
            }
            if new_track || jumped {
                let at = (now.position_at(Instant::now()) as i64).saturating_mul(1000);
                Player::seeked(ctxt, at).await.map_err(err)?;
            }
            return Ok(());
        }
        let shared: Shared = std::sync::Arc::new(Mutex::new(now));
        let conn = zbus::connection::Builder::session()
            .map_err(err)?
            .serve_at(OBJECT, Root { now: shared.clone() })
            .map_err(err)?
            .serve_at(OBJECT, Player { now: shared.clone(), track, name: name.to_string(), call: caller.clone() })
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
    fn show(&self, player: &str, now: &Now, track: u64) -> Result<(), String> {
        self.ask(|r| Call::Show(player.to_string(), Box::new(now.clone()), track, r))
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

/// What a phone's player shows now, as last applied: what a method call is
/// decided against.
#[derive(Debug, Clone)]
struct Live {
    now: Now,
    track: u64,
}

/// Where a `media_control` line goes: the phone's `device_events` channel
/// (main.rs). False is "not written": no channel open.
pub type Sink = Box<dyn Fn(&str, &Value) -> bool + Send + Sync>;

pub struct Media {
    backend: Box<dyn Backend>,
    book: Mutex<Book>,
    wake: Condvar,
    /// By device id; only the worker writes it.
    live: Mutex<HashMap<String, Live>>,
    next_track: AtomicU64,
    sink: OnceLock<Sink>,
}

impl Media {
    pub fn new(backend: Box<dyn Backend>) -> Media {
        Media {
            backend,
            book: Mutex::new(Book::default()),
            wake: Condvar::new(),
            live: Mutex::new(HashMap::new()),
            next_track: AtomicU64::new(0),
            sink: OnceLock::new(),
        }
    }

    /// Where control lines go (once; later calls are ignored).
    pub fn on_control(&self, f: Sink) {
        let _ = self.sink.set(f);
    }

    fn live(&self) -> std::sync::MutexGuard<'_, HashMap<String, Live>> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `method` on `player` (a full bus name): decided by [`decide`] against
    /// what the player shows now, and the line written to the phone that
    /// owns it. A player nobody shows, or a phone with no events channel,
    /// is [`Refused::Failed`]: never a success that went nowhere.
    pub fn call(&self, player: &str, method: Method) -> Result<(), Refused> {
        let found = self.live().iter().find(|(d, _)| player_name(d) == player).map(|(d, l)| (d.clone(), l.clone()));
        let Some((device, live)) = found else {
            return Err(Refused::Failed("no phone shows this player any more".into()));
        };
        let Some(line) = decide(&live.now, live.track, &method, Instant::now())? else {
            return Ok(());
        };
        match self.sink.get() {
            Some(send) if send(&device, &line) => Ok(()),
            _ => Err(Refused::Failed("the phone is not connected to this computer now".into())),
        }
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
            Pending::Show(now) => {
                // A new number for a new track, kept for the same one.
                let track = {
                    let mut live = self.live();
                    let track = match live.get(device) {
                        Some(l) if same_track(&l.now.playing, &now.playing) => l.track,
                        _ => self.next_track.fetch_add(1, Ordering::Relaxed) + 1,
                    };
                    live.insert(device.to_string(), Live { now: now.clone(), track });
                    track
                };
                self.backend.show(&name, now, track)
            }
            Pending::Gone => {
                self.live().remove(device);
                self.backend.gone(&name)
            }
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
        // Method calls reach the Media through this cell, set the moment it
        // exists: a call before that has no player to act on anyway.
        let cell: Arc<OnceLock<&'static Media>> = Arc::new(OnceLock::new());
        let to = cell.clone();
        let caller: Caller = Arc::new(move |player, method| match to.get() {
            Some(m) => m.call(player, method),
            None => Err(Refused::Failed("not ready".into())),
        });
        let backend: std::io::Result<Box<dyn Backend>> = match stub {
            Some(dir) => Stub::watch_control(dir.clone(), caller).map(|()| Box::new(Stub::new(dir)) as Box<dyn Backend>),
            None => Bus::start(caller).map(|b| Box::new(b) as Box<dyn Backend>),
        };
        let m: &'static Media = match backend {
            Ok(b) => {
                let m: &'static Media = Box::leak(Box::new(Media::new(b)));
                let _ = cell.set(m);
                m
            }
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
        fn show(&self, player: &str, now: &Now, _track: u64) -> Result<(), String> {
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

    fn with_can(can: &[&str], status: Status) -> Now {
        let mut v = song();
        v["can"] = json!(can);
        let p = playing("P", &body(v)).unwrap().unwrap();
        Now { playing: Playing { status, ..p }, at: Instant::now() }
    }

    fn action(r: Result<Option<Value>, Refused>) -> Option<String> {
        r.unwrap().map(|v| {
            assert_eq!(v["event"], CONTROL_EVENT);
            v["action"].as_str().unwrap().to_string()
        })
    }

    #[test]
    fn can_is_parsed_strictly_and_unknown_words_are_ignored() {
        let c = |v: Value| playing("P", &body(json!({"state": "playing", "title": "t", "can": v}))).map(|p| p.unwrap().can);
        assert_eq!(c(Value::Null), Ok(Can::default()));
        assert_eq!(c(json!(["play", "seek", "shuffle"])), Ok(Can { play: true, seek: true, ..Can::default() }));
        assert!(!c(json!([])).unwrap().any());
        for bad in [json!("play"), json!([1]), json!(["a", "b", "c", "d", "e", "f", "g", "h", "i"]), json!({"play": true})] {
            assert!(c(bad.clone()).is_err(), "{bad}");
        }
        let all = c(json!(["previous", "next", "pause", "play", "seek"])).unwrap();
        assert_eq!(all.words(), ["play", "pause", "next", "previous", "seek"]);
    }

    #[test]
    fn each_method_needs_its_word_in_can_and_names_one_action() {
        let all = with_can(&["play", "pause", "next", "previous", "seek"], Status::Playing);
        let t = all.at;
        for (m, a) in [(Method::Play, "play"), (Method::Pause, "pause"), (Method::PlayPause, "play_pause"),
                       (Method::Stop, "pause"), (Method::Next, "next"), (Method::Previous, "previous")] {
            assert_eq!(action(decide(&all, 1, &m, t)).as_deref(), Some(a), "{m:?}");
        }
        let none = with_can(&[], Status::Playing);
        for m in [Method::Play, Method::Pause, Method::PlayPause, Method::Stop, Method::Next, Method::Previous,
                  Method::Seek(1), Method::SetPosition(track_path(1), 0)] {
            assert!(matches!(decide(&none, 1, &m, t), Err(Refused::NotSupported(_))), "{m:?}");
        }
        // One word does not open another.
        let only_next = with_can(&["next"], Status::Playing);
        assert_eq!(action(decide(&only_next, 1, &Method::Next, t)).as_deref(), Some("next"));
        for m in [Method::Previous, Method::Play, Method::Pause, Method::PlayPause, Method::Seek(0)] {
            assert!(matches!(decide(&only_next, 1, &m, t), Err(Refused::NotSupported(_))), "{m:?}");
        }
    }

    #[test]
    fn play_pause_asks_for_what_the_status_calls_for_and_stop_is_a_pause() {
        let t = Instant::now();
        let playing_pause_only = with_can(&["pause"], Status::Playing);
        assert_eq!(action(decide(&playing_pause_only, 1, &Method::PlayPause, t)).as_deref(), Some("play_pause"));
        let paused_pause_only = with_can(&["pause"], Status::Paused);
        assert!(matches!(decide(&paused_pause_only, 1, &Method::PlayPause, t), Err(Refused::NotSupported(_))));
        let paused_play = with_can(&["play"], Status::Paused);
        assert_eq!(action(decide(&paused_play, 1, &Method::PlayPause, t)).as_deref(), Some("play_pause"));
        // Stop while not playing does nothing and is not an error.
        assert_eq!(decide(&paused_play, 1, &Method::Stop, t), Ok(None));
        assert!(matches!(decide(&with_can(&["play"], Status::Playing), 1, &Method::Stop, t), Err(Refused::NotSupported(_))));
    }

    #[test]
    fn a_seek_crosses_as_an_absolute_position_clamped_to_the_track() {
        let now = with_can(&["seek"], Status::Paused);
        let at = now.at;
        let pos = |r: Result<Option<Value>, Refused>| r.unwrap().map(|v| {
            assert_eq!(v["action"], "seek");
            v["position_ms"].as_u64().unwrap()
        });
        // song(): 10 s into 200 s.
        assert_eq!(pos(decide(&now, 1, &Method::Seek(5_000_000), at)), Some(15_000));
        assert_eq!(pos(decide(&now, 1, &Method::Seek(-60_000_000), at)), Some(0));
        assert_eq!(pos(decide(&now, 1, &Method::Seek(900_000_000), at)), Some(200_000));
        // While playing, from where the clock has carried it.
        let running = with_can(&["seek"], Status::Playing);
        let later = running.at + Duration::from_secs(4);
        assert_eq!(pos(decide(&running, 1, &Method::Seek(1_000_000), later)), Some(15_000));
        // Unknown length: only the floor, and the 7-day ceiling.
        let open = Now { playing: Playing { duration_ms: 0, ..now.playing.clone() }, at };
        assert_eq!(pos(decide(&open, 1, &Method::Seek(500_000_000), at)), Some(510_000));
        assert_eq!(pos(decide(&open, 1, &Method::Seek(i64::MAX), at)), Some(MAX_DURATION_MS));
        // SetPosition: this track only, inside it; anything else is ignored.
        assert_eq!(pos(decide(&now, 7, &Method::SetPosition(track_path(7), 42_000_000), at)), Some(42_000));
        assert_eq!(decide(&now, 7, &Method::SetPosition(track_path(6), 42_000_000), at), Ok(None));
        assert_eq!(decide(&now, 7, &Method::SetPosition(track_path(7), -1), at), Ok(None));
        assert_eq!(decide(&now, 7, &Method::SetPosition(track_path(7), 200_001_000), at), Ok(None));
    }

    #[test]
    fn a_call_reaches_the_phone_that_owns_the_player_and_never_succeeds_into_nothing() {
        let (_seen, m) = seen();
        let sent: &'static Mutex<Vec<(String, Value)>> = Box::leak(Box::default());
        let connected: &'static std::sync::atomic::AtomicBool = Box::leak(Box::new(true.into()));
        // No sink yet: allowed, but it goes nowhere.
        let mut v = song();
        v["can"] = json!(["play", "pause", "next"]);
        let p = playing("P", &body(v)).unwrap().unwrap();
        m.set("dev", 1, Some(p.clone()), Instant::now());
        m.set("other", 2, Some(Playing { can: Can::default(), ..p.clone() }), Instant::now());
        m.drain();
        assert!(matches!(m.call(&player_name("dev"), Method::Next), Err(Refused::Failed(_))));
        m.on_control(Box::new(move |d, line| {
            sent.lock().unwrap().push((d.to_string(), line.clone()));
            connected.load(Ordering::Relaxed)
        }));
        assert_eq!(m.call(&player_name("dev"), Method::Next), Ok(()));
        assert_eq!(*sent.lock().unwrap(), vec![("dev".to_string(), json!({"event": "media_control", "action": "next"}))]);
        // Another phone's player, display only: refused, nothing sent.
        assert!(matches!(m.call(&player_name("other"), Method::Next), Err(Refused::NotSupported(_))));
        assert!(matches!(m.call(&player_name("nobody"), Method::Next), Err(Refused::Failed(_))));
        // No events channel: an error, not a success.
        connected.store(false, Ordering::Relaxed);
        assert!(matches!(m.call(&player_name("dev"), Method::Pause), Err(Refused::Failed(_))));
        // Gone with its connection: nothing to call.
        connected.store(true, Ordering::Relaxed);
        m.ended("dev", 1);
        m.drain();
        assert!(matches!(m.call(&player_name("dev"), Method::Pause), Err(Refused::Failed(_))));
        assert_eq!(sent.lock().unwrap().len(), 2, "only the allowed calls were offered to the sink (one found no channel)");
    }

    #[test]
    fn a_track_keeps_its_id_until_the_track_changes() {
        let dir = std::env::temp_dir().join(format!("rime-media-track-{}-{}", std::process::id(), rime_remote_core::now_ms()));
        let m = Media::new(Box::new(Stub::new(dir.clone())));
        let mut p = playing("P", &body(song())).unwrap().unwrap();
        m.set("dev", 1, Some(p.clone()), Instant::now());
        m.drain();
        p.position_ms = 90_000;
        std::thread::sleep(SPACING);
        m.set("dev", 1, Some(p.clone()), Instant::now());
        m.drain();
        p.title = "Next one".into();
        std::thread::sleep(SPACING);
        m.set("dev", 1, Some(p), Instant::now());
        m.drain();
        let tracks: Vec<String> = std::fs::read_to_string(dir.join("media.jsonl")).unwrap().lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap()["track"].as_str().unwrap().to_string()).collect();
        assert_eq!(tracks.len(), 3);
        assert_eq!(tracks[0], tracks[1], "a seek is the same track");
        assert_ne!(tracks[1], tracks[2], "a new title is a new track");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_stub_takes_method_calls_from_its_file_and_records_each_outcome() {
        let dir = std::env::temp_dir().join(format!("rime-media-ctl-{}-{}", std::process::id(), rime_remote_core::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let heard: &'static Mutex<Vec<(String, Method)>> = Box::leak(Box::default());
        let caller: Caller = Arc::new(move |player: &str, m: Method| {
            heard.lock().unwrap().push((player.to_string(), m.clone()));
            if m == Method::Next { Err(Refused::NotSupported("no".into())) } else { Ok(()) }
        });
        Stub::watch_control(dir.clone(), caller).unwrap();
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("control.jsonl")).unwrap();
        f.write_all(br#"{"player":"p1","method":"PlayPause"}
{"player":"p1","method":"Next"}
{"player":"p1","method":"Seek","offset_us":-5000000}
{"player":"p1","method":"SetPosition","track":"/x/1","position_us":9}
{"player":"p1","method":"Raise"}
{"player":"p1","method":"Pla"#).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let calls = loop {
            let text = std::fs::read_to_string(dir.join("media.jsonl")).unwrap_or_default();
            let calls: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
            if calls.len() >= 5 || Instant::now() > deadline { break calls; }
            std::thread::sleep(Duration::from_millis(50));
        };
        let results: Vec<(&str, &str)> = calls.iter().map(|c| (c["method"].as_str().unwrap(), c["result"].as_str().unwrap())).collect();
        assert_eq!(results, [("PlayPause", "ok"), ("Next", "not_supported"), ("Seek", "ok"), ("SetPosition", "ok"), ("", "bad_line")]);
        assert_eq!(heard.lock().unwrap()[2], ("p1".to_string(), Method::Seek(-5_000_000)));
        assert_eq!(heard.lock().unwrap()[3].1, Method::SetPosition("/x/1".into(), 9));
        // The half-written line waits for its end.
        f.write_all(b"y\"}\n").unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(heard.lock().unwrap().last().unwrap().1, Method::Play);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real backend on a private session bus (tests/test-remoted-media-bus.sh
    /// starts one and points DBUS_SESSION_BUS_ADDRESS at it). Fails without it,
    /// so a run that forgot the bus cannot pass by skipping. With
    /// RIME_MEDIA_KEY set (the script sets it) the media-key helper is run
    /// against the same players.
    #[test]
    #[ignore]
    fn the_bus_backend_exports_a_player_mpris_readers_read_and_playerctl_leaves_out() {
        use std::sync::atomic::AtomicBool;
        let addr = std::env::var("DBUS_SESSION_BUS_ADDRESS").expect("a private session bus");
        assert!(std::env::var("RIME_MEDIA_BUS_PRIVATE").is_ok_and(|v| v == addr), "refusing a bus the script did not start");
        let key = std::env::var("RIME_MEDIA_KEY").expect("RIME_MEDIA_KEY: the rime-media-key helper to test");
        let run = |cmd: &str, args: &[&str]| {
            let o = std::process::Command::new(cmd).args(args).output().unwrap();
            (o.status.success(), String::from_utf8_lossy(&o.stdout).trim().to_string(), String::from_utf8_lossy(&o.stderr).trim().to_string())
        };
        // The phone: a real Media on the bus backend, its control lines
        // recorded where the device_events channel would take them.
        let cell: Arc<OnceLock<&'static Media>> = Arc::new(OnceLock::new());
        let to = cell.clone();
        let caller: Caller = Arc::new(move |p: &str, m: Method| to.get().expect("media").call(p, m));
        let m: &'static Media = Box::leak(Box::new(Media::new(Box::new(Bus::start(caller).unwrap()))));
        let _ = cell.set(m);
        let sent: &'static Mutex<Vec<Value>> = Box::leak(Box::default());
        let connected: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(true)));
        m.on_control(Box::new(move |d, line| {
            assert_eq!(d, "dev-1", "a control goes to the phone that owns the player");
            if !connected.load(Ordering::Relaxed) {
                return false;
            }
            sent.lock().unwrap().push(line.clone());
            true
        }));
        let sent_now = || sent.lock().unwrap().clone();
        let show_phone = |p: &Playing| {
            m.set("dev-1", 1, Some(p.clone()), Instant::now());
            m.drain();
        };
        // A player of the computer's own (a second bus backend whose method
        // calls are recorded), so "leaves the phone out" is also "keeps the
        // computer's", and the media key can be seen to reach it.
        let heard: &'static Mutex<Vec<String>> = Box::leak(Box::default());
        let computer_bus = Bus::start(Arc::new(move |_: &str, m: Method| {
            heard.lock().unwrap().push(m.name().to_string());
            Ok(())
        }))
        .unwrap();
        let computer = "org.mpris.MediaPlayer2.computer";
        let name = player_name("dev-1");
        let short = name.trim_start_matches("org.mpris.MediaPlayer2.").to_string();
        let p = playing("Pixel", &body(song())).unwrap().unwrap();
        let all = Can { play: true, pause: true, next: true, previous: true, seek: true };
        computer_bus.show(computer, &Now { playing: Playing { can: all, ..p.clone() }, at: Instant::now() }, 1).unwrap();
        show_phone(&p);
        let get = |iface: &str, prop: &str| {
            let (ok, out, err) =
                run("busctl", &["--user", "--json=short", "get-property", &name, OBJECT, iface, prop]);
            assert!(ok, "{prop}: {err}");
            serde_json::from_str::<Value>(&out).unwrap()["data"].clone()
        };
        let call = |method: &str, sig_args: &[&str]| {
            let mut a = vec!["--user", "call", name.as_str(), OBJECT, "org.mpris.MediaPlayer2.Player", method];
            a.extend_from_slice(sig_args);
            run("busctl", &a)
        };
        let player = "org.mpris.MediaPlayer2.Player";
        assert_eq!(get("org.mpris.MediaPlayer2", "Identity"), "Pixel · Music");
        assert_eq!(get(player, "PlaybackStatus"), "Playing");
        let m0 = get(player, "Metadata");
        assert_eq!(m0["xesam:title"]["data"], "Song", "{m0}");
        assert_eq!(m0["xesam:artist"]["data"], json!(["Band"]), "{m0}");
        assert_eq!(m0["xesam:album"]["data"], "LP", "{m0}");
        assert_eq!(m0["mpris:length"]["data"], 200_000_000, "{m0}");
        let track = m0["mpris:trackid"]["data"].as_str().unwrap().to_string();
        let first = get(player, "Position").as_i64().unwrap();
        std::thread::sleep(Duration::from_millis(1200));
        let second = get(player, "Position").as_i64().unwrap();
        assert!(first >= 10_000_000 && (1_000_000..2_500_000).contains(&(second - first)), "{first} {second}");

        // Display only (no `can`): every control is refused, nothing crosses.
        for prop in ["CanControl", "CanPlay", "CanPause", "CanGoNext", "CanGoPrevious", "CanSeek"] {
            assert_eq!(get(player, prop), false, "{prop}");
        }
        let (ok, _, err) = call("PlayPause", &[]);
        assert!(!ok && err.contains("does not let this computer"), "{err}");
        let (ok, _, _) = call("Seek", &["x", "5000000"]);
        assert!(!ok);
        assert_eq!(get(player, "PlaybackStatus"), "Playing");
        let (_, all_players, _) = run("playerctl", &["-l"]);
        assert!(all_players.lines().any(|l| l == short) && all_players.lines().any(|l| l == "computer"), "{all_players}");
        let (_, kept, _) = run("playerctl", &["-l", crate::machine::IGNORE_PHONES]);
        assert_eq!(kept.lines().collect::<Vec<_>>(), ["computer"], "one name leaves every phone out and keeps the rest");
        let (acted, _, _) = run("playerctl", &["-p", &short, "play-pause"]);
        assert!(!acted, "playerctl does not act on a player that cannot be controlled");
        assert!(sent_now().is_empty(), "{:?}", sent_now());

        // The phone allows control: the properties change, announced.
        let mut monitor = std::process::Command::new("dbus-monitor")
            .args(["--session", &format!("type='signal',sender='{name}',interface='org.freedesktop.DBus.Properties'")])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(400));
        let controlled = Playing { can: all, ..p.clone() };
        show_phone(&controlled);
        std::thread::sleep(Duration::from_millis(400));
        monitor.kill().unwrap();
        let mut said = String::new();
        std::io::Read::read_to_string(monitor.stdout.as_mut().unwrap(), &mut said).unwrap();
        let _ = monitor.wait();
        for prop in ["CanControl", "CanPlay", "CanPause", "CanGoNext", "CanGoPrevious", "CanSeek"] {
            assert_eq!(get(player, prop), true, "{prop}");
            assert!(said.contains(&format!("\"{prop}\"")), "no PropertiesChanged for {prop}: {said}");
        }
        assert_eq!(get(player, "Metadata")["mpris:trackid"]["data"], track.as_str(), "allowing control is the same track");

        // Each method crosses as one media_control line.
        let (ok, _, err) = call("PlayPause", &[]);
        assert!(ok, "{err}");
        assert_eq!(sent_now().last().unwrap(), &json!({"event": "media_control", "action": "play_pause"}));
        let (ok, _, err) = call("Seek", &["x", "5000000"]);
        assert!(ok, "{err}");
        let at = sent_now().last().unwrap().clone();
        assert_eq!(at["action"], "seek", "{at}");
        let pos = at["position_ms"].as_u64().unwrap();
        assert!((15_000..20_000).contains(&pos), "relative seek crosses as an absolute position: {at}");
        let (ok, _, err) = call("SetPosition", &["ox", &track, "42000000"]);
        assert!(ok, "{err}");
        assert_eq!(sent_now().last().unwrap(), &json!({"event": "media_control", "action": "seek", "position_ms": 42_000}));
        let before = sent_now().len();
        let (ok, _, err) = call("SetPosition", &["ox", "/org/rimeos/RimeRemote/track/999999", "42000000"]);
        assert!(ok, "a stale track id is ignored, per MPRIS: {err}");
        assert_eq!(sent_now().len(), before, "a stale track id sends nothing");
        let (ok, _, _) = run("busctl", &["--user", "call", &name, OBJECT, "org.mpris.MediaPlayer2", "Raise"]);
        assert!(!ok, "Raise stays refused");
        let (ok, _, _) = run("playerctl", &["-p", &short, "next"]);
        assert!(ok, "playerctl acts on a phone that allows it");
        assert_eq!(sent_now().last().unwrap(), &json!({"event": "media_control", "action": "next"}));

        // The media key: the computer's player first, the phone untouched.
        let before = sent_now().len();
        let (ok, _, err) = run(&key, &["next"]);
        assert!(ok, "{err}");
        assert_eq!(*heard.lock().unwrap(), ["Next"], "the key went to the computer's player");
        assert_eq!(sent_now().len(), before, "and not to the phone");
        // A computer player that takes no keys still keeps them off the phone.
        computer_bus
            .show(computer, &Now { playing: Playing { can: Can::default(), ..p.clone() }, at: Instant::now() }, 1)
            .unwrap();
        let _ = run(&key, &["play-pause"]);
        assert_eq!(heard.lock().unwrap().len(), 1);
        assert_eq!(sent_now().len(), before, "a computer player that takes no keys does not hand them to the phone");
        // No computer player at all: the phone gets the key.
        computer_bus.gone(computer).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let (ok, _, err) = run(&key, &["previous"]);
        assert!(ok, "{err}");
        assert_eq!(sent_now().last().unwrap(), &json!({"event": "media_control", "action": "previous"}));
        assert_eq!(sent_now().len(), before + 1);
        let (ok, _, _) = run(&key, &["stop"]);
        assert!(!ok, "the helper takes only the three keys");

        // One word does not open another.
        show_phone(&Playing { can: Can { next: true, ..Can::default() }, ..p.clone() });
        assert_eq!(get(player, "CanControl"), true);
        assert_eq!(get(player, "CanGoPrevious"), false);
        let (ok, _, err) = call("Previous", &[]);
        assert!(!ok && err.contains("does not let this computer"), "{err}");

        // No events channel: an error, never a success that went nowhere.
        connected.store(false, Ordering::Relaxed);
        let before = sent_now().len();
        let (ok, _, err) = call("Next", &[]);
        assert!(!ok && err.contains("not connected"), "{err}");
        assert_eq!(sent_now().len(), before);
        connected.store(true, Ordering::Relaxed);

        // An update shows, as a new track.
        show_phone(&Playing { status: Status::Paused, title: "Other".into(), ..p.clone() });
        assert_eq!(get(player, "PlaybackStatus"), "Paused");
        let m1 = get(player, "Metadata");
        assert_eq!(m1["xesam:title"]["data"], "Other");
        assert_ne!(m1["mpris:trackid"]["data"], track.as_str(), "a new track, a new id");
        assert_eq!(get(player, "CanControl"), false, "the phone stopped allowing control");
        // Removal takes it off the bus.
        m.ended("dev-1", 1);
        m.drain();
        std::thread::sleep(Duration::from_millis(300));
        let (_, after, _) = run("playerctl", &["-l"]);
        assert!(!after.lines().any(|l| l == short), "{after}");
        let (ok, _, _) = run(&key, &["next"]);
        assert!(!ok, "no player anywhere: the key does nothing and says so");
    }
}
