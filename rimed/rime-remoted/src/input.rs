//! A phone as this computer's touchpad and keyboard (contract §2.16).
//!
//! The phone opens one `input` channel and sends one batch of events per
//! display frame as a `Data` frame. This process injects them into the
//! person's graphical session as a Wayland virtual pointer and two virtual
//! keyboards, as the user, through the compositor's own protocols
//! (`zwlr_virtual_pointer_v1`, `zwp_virtual_keyboard_v1`).
//!
//! ## What keeps it safe
//!
//! Injected input reaches the lock screen on every compositor Rime ships
//! (measured), so the lock gate here is the only thing between a phone and a
//! password field. It is the union of every source this process can read,
//! and anything it cannot read counts as locked ([`locked`]):
//!
//! 1. the compositor's own lock notifier on the injecting connection
//!    (`hyprland_lock_notifier_v1`, when offered);
//! 2. logind's `LockedHint` for the user's graphical session;
//! 3. logind's `Lock` / `Unlock` signals for that session;
//! 4. the Shell's `$XDG_RUNTIME_DIR/rime-remote/lock.json`, written when the
//!    Shell starts locking and before the compositor confirms it.
//!
//! Only logind can say "unlocked", and only positively (`LockedHint` false);
//! every other source can only veto. A missing Shell file is no opinion
//! either way. The gate is read before every event, and a session thread
//! that holds anything re-reads it every [`HELD_TICK`] whether or not the
//! phone sends anything, so a key held when the screen locks is let go
//! before it can auto-repeat into the password field.
//!
//! The grant (`share.input` on the phone's record, set only by a person
//! here) is checked on every frame, not only when the channel opens, and
//! lowering it or revoking the phone ends the live channel at once. A
//! companion key or another computer can never open the channel.
//!
//! What is held (keys, buttons) is written to a file before every press, so
//! a crash leaves something to release from: a held key on Hyprland keeps
//! auto-repeating after the process that pressed it died (measured).

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rime_remote_core::device::Scope;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::state::State;

/// What `remote_hello` lists when this build has the channel.
pub const FEATURES: [&str; 1] = ["phone_input"];

/// The channel's verb.
pub const CMD: &str = "input";

/// A directory to use instead of the desktop: actions go to
/// `actions.jsonl`, the lock sources come from `sources.json`. Every harness
/// that starts this daemon sets it, or a test would type into the
/// developer's own desktop.
pub const STUB_ENV: &str = "RIME_REMOTED_INPUT_STUB";

/// Frames a phone may send in one second.
pub const MAX_FRAMES_PER_SEC: u32 = 250;
/// Events in one frame.
pub const MAX_EVENTS: usize = 64;
/// Characters in one `text` event.
pub const MAX_TEXT_CHARS: usize = 1_000;
/// Characters of text in one second, over every frame.
pub const MAX_TEXT_PER_SEC: usize = 4_000;
/// The largest motion or scroll delta, either axis.
pub const MAX_DELTA: f64 = 2_000.0;
/// One frame's bytes. 64 events of the longest kind are well under it.
pub const MAX_FRAME_BYTES: usize = 16 * 1024;
/// Nothing from the phone for this long while something is held: everything
/// is released. The phone sends `hold` every 250 ms while it holds anything.
pub const LEASE: Duration = Duration::from_millis(750);
/// How often a session thread that holds something reads the gate and the
/// lease when the phone sends nothing.
pub const HELD_TICK: Duration = Duration::from_millis(20);
/// The same, when nothing is held: only the paused/resumed news and the
/// grant wait on it.
pub const IDLE_TICK: Duration = Duration::from_millis(250);
/// Distinct characters per text keymap. Below 247 so every keycode fits the
/// 8..255 range an X11 client under Xwayland can receive.
const TEXT_CHUNK: usize = 200;

/// Keys a phone may never press: a touchpad needs none of them, and a
/// keyboard grant must not be a way to turn the computer off or suspend it.
pub const FORBIDDEN_KEYS: [u32; 4] = [
    116, /* POWER */
    142, /* SLEEP */
    143, /* WAKEUP */
    205, /* SUSPEND */
];

const CAPSLOCK: u32 = 58;

/// Whether `request` asks for this channel.
pub fn is_channel(request: &[u8]) -> bool {
    crate::companion::verb_of(request).as_deref() == Some(CMD)
}

// ── events ─────────────────────────────────────────────────────────────────

/// A pointer button, by its wire word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

impl Button {
    /// The evdev code (`BTN_LEFT` …).
    pub fn code(self) -> u32 {
        match self {
            Button::Left => 0x110,
            Button::Right => 0x111,
            Button::Middle => 0x112,
        }
    }
}

/// Where a scroll came from: a finger (kinetic, ends with `stop`) or a wheel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Src {
    Finger,
    Wheel,
}

impl Src {
    fn word(self) -> &'static str {
        match self {
            Src::Finger => "finger",
            Src::Wheel => "wheel",
        }
    }
}

/// A modifier a `tap` may hold around its key, as the evdev key it presses.
const MODS: [(&str, u32); 5] = [
    ("ctrl", 29),
    ("shift", 42),
    ("alt", 56),
    ("super", 125),
    ("altgr", 100),
];

/// One event of a batch, validated.
#[derive(Debug, Clone, PartialEq)]
pub enum Ev {
    Motion {
        dx: f64,
        dy: f64,
    },
    Button {
        b: Button,
        p: bool,
    },
    Scroll {
        dx: f64,
        dy: f64,
        src: Src,
        stop: bool,
    },
    Key {
        code: u32,
        p: bool,
    },
    Tap {
        code: u32,
        mods: Vec<u32>,
    },
    Text(String),
    Hold,
}

/// A batch the contract does not allow, and why, in words for the phone.
#[derive(Debug, Clone, PartialEq)]
pub struct Refused(pub String);

fn delta(v: &Value, field: &str) -> Result<f64, Refused> {
    let d = match v.get(field) {
        None => 0.0,
        Some(x) => x
            .as_f64()
            .ok_or_else(|| Refused(format!("{field} is not a number")))?,
    };
    if !d.is_finite() || d.abs() > MAX_DELTA {
        return Err(Refused(format!(
            "{field} must be a number no larger than {MAX_DELTA} either way"
        )));
    }
    Ok(d)
}

fn flag(v: &Value, field: &str) -> Result<bool, Refused> {
    v.get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| Refused(format!("{field} must be true or false")))
}

/// Whether a phone may press this key: a keyboard key (1..=255) that is not
/// one of [`FORBIDDEN_KEYS`].
pub fn key_allowed(code: u32) -> bool {
    (1..=0xff).contains(&code) && !FORBIDDEN_KEYS.contains(&code)
}

fn key_code(v: &Value) -> Result<u32, Refused> {
    let code = v
        .get("code")
        .and_then(Value::as_u64)
        .and_then(|c| u32::try_from(c).ok())
        .ok_or_else(|| Refused("code must be an evdev key code".into()))?;
    if !key_allowed(code) {
        return Err(Refused(format!(
            "key {code} may not be pressed from a phone"
        )));
    }
    Ok(code)
}

/// Text as it will be typed: line breaks made `\n`, and refused when it is
/// too long or carries a control character other than a line break or tab.
pub fn clean_text(s: &str) -> Result<String, Refused> {
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    let n = s.chars().count();
    if n > MAX_TEXT_CHARS {
        return Err(Refused(format!(
            "text is limited to {MAX_TEXT_CHARS} characters at a time"
        )));
    }
    if s.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err(Refused("text may not carry control characters".into()));
    }
    Ok(s)
}

fn event(v: &Value) -> Result<Ev, Refused> {
    let t = v
        .get("t")
        .and_then(Value::as_str)
        .ok_or_else(|| Refused("every event needs a t".into()))?;
    Ok(match t {
        "m" => Ev::Motion {
            dx: delta(v, "dx")?,
            dy: delta(v, "dy")?,
        },
        "b" => {
            let b = match v.get("b").and_then(Value::as_str) {
                Some("left") => Button::Left,
                Some("right") => Button::Right,
                Some("middle") => Button::Middle,
                _ => return Err(Refused("b must be left, right or middle".into())),
            };
            Ev::Button {
                b,
                p: flag(v, "p")?,
            }
        }
        "s" => {
            let src = match v.get("src").and_then(Value::as_str) {
                Some("finger") | None => Src::Finger,
                Some("wheel") => Src::Wheel,
                _ => return Err(Refused("src must be finger or wheel".into())),
            };
            let stop = v
                .get("stop")
                .map(|_| flag(v, "stop"))
                .transpose()?
                .unwrap_or(false);
            Ev::Scroll {
                dx: delta(v, "dx")?,
                dy: delta(v, "dy")?,
                src,
                stop,
            }
        }
        "k" => Ev::Key {
            code: key_code(v)?,
            p: flag(v, "p")?,
        },
        "tap" => {
            let code = key_code(v)?;
            let mut mods = Vec::new();
            if let Some(list) = v.get("mods") {
                let list = list
                    .as_array()
                    .ok_or_else(|| Refused("mods must be a list".into()))?;
                for m in list {
                    let word = m.as_str().unwrap_or_default();
                    let Some((_, key)) = MODS.iter().find(|(w, _)| *w == word) else {
                        return Err(Refused(
                            "mods may be ctrl, shift, alt, super and altgr".into(),
                        ));
                    };
                    if !mods.contains(key) {
                        mods.push(*key);
                    }
                }
            }
            Ev::Tap { code, mods }
        }
        "text" => {
            let s = v
                .get("s")
                .and_then(Value::as_str)
                .ok_or_else(|| Refused("text needs s".into()))?;
            Ev::Text(clean_text(s)?)
        }
        "hold" => Ev::Hold,
        _ => return Err(Refused("an event the contract does not have".into())),
    })
}

/// One `Data` frame: `{"seq":N,"ev":[...]}`, validated whole before any of
/// it is applied, so a bad event late in a batch cannot leave the earlier
/// ones half-done.
pub fn parse_batch(bytes: &[u8]) -> Result<(u64, Vec<Ev>), Refused> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(Refused(format!(
            "a frame is limited to {MAX_FRAME_BYTES} bytes"
        )));
    }
    let v: Value = serde_json::from_slice(bytes)
        .map_err(|_| Refused("a frame must be one JSON object".into()))?;
    let seq = v
        .get("seq")
        .and_then(Value::as_u64)
        .ok_or_else(|| Refused("a frame needs a seq".into()))?;
    let list = v
        .get("ev")
        .and_then(Value::as_array)
        .ok_or_else(|| Refused("a frame needs an ev list".into()))?;
    if list.len() > MAX_EVENTS {
        return Err(Refused(format!(
            "a frame is limited to {MAX_EVENTS} events"
        )));
    }
    let events = list.iter().map(event).collect::<Result<Vec<_>, _>>()?;
    Ok((seq, events))
}

// ── the lock gate ──────────────────────────────────────────────────────────

/// What the Shell's `lock.json` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShellFile {
    /// No file: no opinion. A Shell that never wrote one cannot prove the
    /// screen is unlocked by its absence.
    #[default]
    Absent,
    Locked,
    Unlocked,
    /// There, and not `{"locked": <bool>}`: counted as locked.
    Unreadable,
}

/// Every lock source, as last read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sources {
    /// logind's `LockedHint` for the graphical session; `None` when logind
    /// did not answer, there is no graphical session, or the bus is gone.
    pub locked_hint: Option<bool>,
    /// A logind `Lock` signal arrived and no `Unlock`, `LockedHint`
    /// true→false or compositor `unlocked` has cleared it since.
    pub lock_signal: bool,
    /// The compositor's own lock notifier, when it has one. `None` (not
    /// offered, or nothing said yet) is no veto: it cannot prove "unlocked".
    pub notifier: Option<bool>,
    /// The Shell's file.
    pub shell: ShellFile,
}

/// Whether input must stop: the union of every source, and unknown is
/// locked. Only logind saying `LockedHint` false, with no other source
/// objecting, lets anything through.
pub fn locked(s: &Sources) -> bool {
    crate::share::counts_as_locked(s.locked_hint)
        || s.lock_signal
        || s.notifier == Some(true)
        || matches!(s.shell, ShellFile::Locked | ShellFile::Unreadable)
}

/// Read the Shell's lock file.
pub fn read_shell_file(path: &Path) -> ShellFile {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v.get("locked")?.as_bool())
        {
            Some(true) => ShellFile::Locked,
            Some(false) => ShellFile::Unlocked,
            None => ShellFile::Unreadable,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ShellFile::Absent,
        Err(_) => ShellFile::Unreadable,
    }
}

/// Where the gate's sources come from: logind (or a stand-in for it) and the
/// Shell's file. The compositor's notifier comes from the [`Backend`].
pub struct Gate {
    /// `(LockedHint, lock signal outstanding)`.
    pub logind: Box<dyn Fn() -> (Option<bool>, bool) + Send>,
    pub shell: Option<PathBuf>,
}

impl Gate {
    fn sources(&self, notifier: Option<bool>, shell: ShellFile) -> Sources {
        let (locked_hint, lock_signal) = (self.logind)();
        Sources {
            locked_hint,
            lock_signal,
            notifier,
            shell,
        }
    }

    fn shell_file(&self) -> ShellFile {
        self.shell
            .as_deref()
            .map_or(ShellFile::Absent, read_shell_file)
    }
}

// ── what is held, and the crash file ───────────────────────────────────────

/// Everything pressed and not released yet, as evdev codes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    #[serde(default)]
    pub keys: BTreeSet<u32>,
    #[serde(default)]
    pub buttons: BTreeSet<u32>,
    /// Keys of a text keymap being typed right now (released from the fixed
    /// keyboard after a crash: a bare release from any device stops a
    /// repeat).
    #[serde(default)]
    pub text_keys: BTreeSet<u32>,
}

impl Held {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.buttons.is_empty() && self.text_keys.is_empty()
    }

    /// The depressed-modifier mask for the keys held, in the real modifier
    /// bits every Rime keymap (`pc` symbols) uses.
    pub fn depressed(&self) -> u32 {
        let mut m = 0;
        for k in &self.keys {
            m |= match k {
                42 | 54 => 0x1,    // Shift
                29 | 97 => 0x4,    // Control
                56 => 0x8,         // Mod1 (Alt)
                125 | 126 => 0x40, // Mod4 (Super)
                100 => 0x80,       // Mod5 (AltGr, ISO_Level3_Shift)
                _ => 0,
            };
        }
        m
    }
}

fn is_modifier(code: u32) -> bool {
    matches!(code, 29 | 97 | 42 | 54 | 56 | 100 | 125 | 126 | CAPSLOCK)
}

/// Write the held set where a later start of this process finds it: whole,
/// through a rename. Empty removes the file.
pub fn write_held(path: &Path, held: &Held) {
    if held.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    let Ok(text) = serde_json::to_vec(held) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    let wrote = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = wrote {
        eprintln!("rime-remoted: could not record what the phone holds ({e}); a crash now would leave it held");
    }
}

/// What an earlier run left held, if anything.
pub fn read_held(path: &Path) -> Option<Held> {
    let bytes = std::fs::read(path).ok()?;
    let held: Held = serde_json::from_slice(&bytes).ok()?;
    (!held.is_empty()).then_some(held)
}

// ── backends ───────────────────────────────────────────────────────────────

/// One virtual pointer and two virtual keyboards in the graphical session:
/// `key` is the keyboard with the session's layout, `text_*` the one whose
/// keymap is replaced for each piece of text.
pub trait Backend: Send {
    fn motion(&mut self, dx: f64, dy: f64) -> std::io::Result<()>;
    fn button(&mut self, code: u32, pressed: bool) -> std::io::Result<()>;
    fn scroll(&mut self, dx: f64, dy: f64, src: Src, stop: bool) -> std::io::Result<()>;
    fn key(&mut self, code: u32, pressed: bool) -> std::io::Result<()>;
    fn modifiers(&mut self, depressed: u32, locked: u32) -> std::io::Result<()>;
    /// Give the text keyboard a keymap whose key `1 + i` types `chars[i]`,
    /// and wait until the compositor has it (Hyprland loses the first
    /// character otherwise).
    fn text_keymap(&mut self, chars: &[char]) -> std::io::Result<()>;
    fn text_key(&mut self, code: u32, pressed: bool) -> std::io::Result<()>;
    /// Wait until the compositor has processed everything sent.
    fn sync(&mut self) -> std::io::Result<()>;
    /// The compositor's own lock notifier: `Some(true)` locked.
    fn compositor_locked(&self) -> Option<bool>;
    /// Why the compositor is gone, once it is.
    fn gone(&self) -> Option<String>;
}

/// The stub: one JSON line per action in `<dir>/actions.jsonl`.
pub struct StubBackend {
    dir: PathBuf,
}

impl StubBackend {
    pub fn open(dir: &Path) -> std::io::Result<StubBackend> {
        std::fs::create_dir_all(dir)?;
        let b = StubBackend {
            dir: dir.to_path_buf(),
        };
        b.write(json!({"a": "open"}))?;
        b.write(json!({"a": "warm"}))?;
        Ok(b)
    }

    fn write(&self, line: Value) -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("actions.jsonl"))?;
        f.write_all(format!("{line}\n").as_bytes())
    }

    fn sources(dir: &Path) -> Value {
        std::fs::read(dir.join("sources.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null)
    }
}

impl Backend for StubBackend {
    fn motion(&mut self, dx: f64, dy: f64) -> std::io::Result<()> {
        self.write(json!({"a": "motion", "dx": dx, "dy": dy}))
    }
    fn button(&mut self, code: u32, pressed: bool) -> std::io::Result<()> {
        self.write(json!({"a": "button", "code": code, "p": pressed}))
    }
    fn scroll(&mut self, dx: f64, dy: f64, src: Src, stop: bool) -> std::io::Result<()> {
        self.write(json!({"a": "scroll", "dx": dx, "dy": dy, "src": src.word(), "stop": stop}))
    }
    fn key(&mut self, code: u32, pressed: bool) -> std::io::Result<()> {
        self.write(json!({"a": "key", "code": code, "p": pressed}))
    }
    fn modifiers(&mut self, depressed: u32, locked: u32) -> std::io::Result<()> {
        self.write(json!({"a": "mods", "depressed": depressed, "locked": locked}))
    }
    fn text_keymap(&mut self, chars: &[char]) -> std::io::Result<()> {
        self.write(json!({"a": "text_keymap", "chars": chars.iter().collect::<String>()}))
    }
    fn text_key(&mut self, code: u32, pressed: bool) -> std::io::Result<()> {
        self.write(json!({"a": "text_key", "code": code, "p": pressed}))
    }
    fn sync(&mut self) -> std::io::Result<()> {
        if self.dir.join("gone").exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the stub compositor is gone",
            ));
        }
        Ok(())
    }
    fn compositor_locked(&self) -> Option<bool> {
        StubBackend::sources(&self.dir)
            .get("notifier")
            .and_then(Value::as_bool)
    }
    fn gone(&self) -> Option<String> {
        self.dir
            .join("gone")
            .exists()
            .then(|| "the stub compositor is gone".to_string())
    }
}

impl Drop for StubBackend {
    fn drop(&mut self) {
        let _ = self.write(json!({"a": "close"}));
    }
}

// ── the session ────────────────────────────────────────────────────────────

/// Why a channel ends, as the phone is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Why {
    /// The grant was lowered, or the phone revoked.
    NotAllowed = 1,
    /// The compositor went away, or Gaming Mode started.
    Unavailable = 2,
    /// The phone sent something the contract does not allow, or too much.
    Refused = 3,
    /// This process is stopping.
    Stopping = 4,
}

impl Why {
    fn word(self) -> &'static str {
        match self {
            Why::NotAllowed => "not_allowed",
            Why::Unavailable => "unavailable",
            Why::Refused => "refused",
            Why::Stopping => "stopping",
        }
    }

    fn from_u8(v: u8) -> Option<Why> {
        [
            Why::NotAllowed,
            Why::Unavailable,
            Why::Refused,
            Why::Stopping,
        ]
        .into_iter()
        .find(|w| *w as u8 == v)
    }
}

/// What a frame or a tick decided.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Go,
    End(Why, String),
}

/// Frames and text in the current second.
struct Rate {
    start: Instant,
    frames: u32,
    text: usize,
}

/// One phone driving this computer: what it holds and what it may do. Pure
/// apart from the backend and the gate, so every rule here is tested
/// without a compositor.
pub struct Session {
    backend: Box<dyn Backend>,
    gate: Gate,
    granted: Box<dyn Fn() -> bool + Send>,
    /// Whether the compositor is still one this may type into (not Gaming
    /// Mode), asked once a second.
    available: Box<dyn Fn() -> Result<(), String> + Send>,
    out: Box<dyn FnMut(Value) + Send>,
    held: Held,
    caps: bool,
    paused: bool,
    shell: ShellFile,
    last_frame: Instant,
    last_seq: Option<u64>,
    last_available: Instant,
    rate: Rate,
    crash: Option<PathBuf>,
}

impl Session {
    /// A session that writes to `backend`, asks `gate` and `granted`, and
    /// tells the phone through `out`.
    pub fn new(
        backend: Box<dyn Backend>,
        gate: Gate,
        granted: Box<dyn Fn() -> bool + Send>,
        available: Box<dyn Fn() -> Result<(), String> + Send>,
        out: Box<dyn FnMut(Value) + Send>,
        crash: Option<PathBuf>,
        now: Instant,
    ) -> Session {
        let shell = gate.shell_file();
        Session {
            backend,
            gate,
            granted,
            available,
            out,
            held: Held::default(),
            caps: false,
            paused: false,
            shell,
            last_frame: now,
            last_seq: None,
            last_available: now,
            rate: Rate {
                start: now,
                frames: 0,
                text: 0,
            },
            crash,
        }
    }

    #[cfg(test)]
    pub fn held(&self) -> &Held {
        &self.held
    }

    #[cfg(test)]
    pub fn paused(&self) -> bool {
        self.paused
    }

    /// The gate, now: the Shell file as last read, logind and the
    /// compositor as they are.
    fn locked_now(&self) -> bool {
        locked(
            &self
                .gate
                .sources(self.backend.compositor_locked(), self.shell),
        )
    }

    /// How long the session thread may wait for the next frame.
    pub fn tick_every(&self) -> Duration {
        if self.held.is_empty() {
            IDLE_TICK
        } else {
            HELD_TICK
        }
    }

    fn record(&self) {
        if let Some(path) = &self.crash {
            write_held(path, &self.held);
        }
    }

    /// Let go of everything: keys from the fixed keyboard, buttons, and any
    /// text key mid-press, then wait for the compositor to have it.
    pub fn release_all(&mut self) {
        let had_keys = !self.held.keys.is_empty() || !self.held.text_keys.is_empty();
        for code in std::mem::take(&mut self.held.keys) {
            let _ = self.backend.key(code, false);
        }
        for code in std::mem::take(&mut self.held.text_keys) {
            let _ = self.backend.text_key(code, false);
        }
        if had_keys {
            let _ = self.backend.modifiers(0, self.locked_mask());
        }
        for code in std::mem::take(&mut self.held.buttons) {
            let _ = self.backend.button(code, false);
        }
        let _ = self.backend.sync();
        self.record();
    }

    fn locked_mask(&self) -> u32 {
        if self.caps {
            0x2
        } else {
            0
        }
    }

    fn pause(&mut self) {
        self.release_all();
        if !self.paused {
            self.paused = true;
            (self.out)(json!({"event": "input_paused", "why": "locked"}));
        }
    }

    fn rate_ok(&mut self, now: Instant, text: usize) -> Result<(), String> {
        if now.duration_since(self.rate.start) >= Duration::from_secs(1) {
            self.rate = Rate {
                start: now,
                frames: 0,
                text: 0,
            };
        }
        self.rate.frames += 1;
        self.rate.text += text;
        if self.rate.frames > MAX_FRAMES_PER_SEC {
            return Err(format!("more than {MAX_FRAMES_PER_SEC} frames in a second"));
        }
        if self.rate.text > MAX_TEXT_PER_SEC {
            return Err(format!(
                "more than {MAX_TEXT_PER_SEC} characters of text in a second"
            ));
        }
        Ok(())
    }

    /// One frame from the phone.
    pub fn frame(&mut self, bytes: &[u8], now: Instant) -> Step {
        // The grant on EVERY frame: lowering it ends the channel even if the
        // hook that stops a live channel never ran.
        if !(self.granted)() {
            return Step::End(
                Why::NotAllowed,
                "this phone is no longer allowed to use this computer as a touchpad".into(),
            );
        }
        self.last_frame = now;
        let (seq, events) = match parse_batch(bytes) {
            Ok(x) => x,
            Err(Refused(why)) => return Step::End(Why::Refused, why),
        };
        let text: usize = events
            .iter()
            .map(|e| {
                if let Ev::Text(s) = e {
                    s.chars().count()
                } else {
                    0
                }
            })
            .sum();
        if let Err(why) = self.rate_ok(now, text) {
            return Step::End(Why::Refused, why);
        }
        // A frame sent twice (a resend after a lost write) is not applied twice.
        if self.last_seq.is_some_and(|last| seq <= last) {
            return Step::Go;
        }
        self.last_seq = Some(seq);
        self.shell = self.gate.shell_file();
        if self.paused {
            if self.locked_now() {
                return Step::Go;
            }
            self.paused = false;
            (self.out)(json!({"event": "input_resumed"}));
        }
        for ev in events {
            // Before EVERY event: a frame that started unlocked can end
            // after the screen locked.
            if self.locked_now() {
                self.pause();
                return Step::Go;
            }
            if let Err(e) = self.apply(ev) {
                return Step::End(
                    Why::Unavailable,
                    format!("the compositor stopped taking input: {e}"),
                );
            }
        }
        Step::Go
    }

    fn press_key(&mut self, code: u32) -> std::io::Result<()> {
        if !self.held.keys.insert(code) {
            return Ok(());
        }
        self.record();
        self.backend.key(code, true)?;
        if code == CAPSLOCK {
            self.caps = !self.caps;
        }
        if is_modifier(code) {
            self.backend
                .modifiers(self.held.depressed(), self.locked_mask())?;
        }
        Ok(())
    }

    fn release_key(&mut self, code: u32) -> std::io::Result<()> {
        if !self.held.keys.contains(&code) {
            return Ok(());
        }
        self.backend.key(code, false)?;
        self.held.keys.remove(&code);
        if is_modifier(code) {
            self.backend
                .modifiers(self.held.depressed(), self.locked_mask())?;
        }
        self.record();
        Ok(())
    }

    fn apply(&mut self, ev: Ev) -> std::io::Result<()> {
        match ev {
            Ev::Motion { dx, dy } => self.backend.motion(dx, dy),
            Ev::Scroll { dx, dy, src, stop } => self.backend.scroll(dx, dy, src, stop),
            Ev::Button { b, p } => {
                let code = b.code();
                if p {
                    if self.held.buttons.insert(code) {
                        self.record();
                        self.backend.button(code, true)?;
                    }
                } else if self.held.buttons.contains(&code) {
                    self.backend.button(code, false)?;
                    self.held.buttons.remove(&code);
                    self.record();
                }
                Ok(())
            }
            Ev::Key { code, p } => {
                if p {
                    self.press_key(code)
                } else {
                    self.release_key(code)
                }
            }
            Ev::Tap { code, mods } => {
                // Only what this tap pressed is let go by it: a modifier the
                // phone already holds stays held.
                let pressed: Vec<u32> = mods
                    .into_iter()
                    .filter(|m| !self.held.keys.contains(m))
                    .collect();
                for m in &pressed {
                    self.press_key(*m)?;
                }
                let was_held = self.held.keys.contains(&code);
                if !was_held {
                    self.press_key(code)?;
                    self.release_key(code)?;
                }
                for m in pressed.iter().rev() {
                    self.release_key(*m)?;
                }
                Ok(())
            }
            Ev::Text(s) => self.text(&s),
            Ev::Hold => Ok(()),
        }
    }

    /// Type `s` on the text keyboard, a keymap at a time. The gate is read
    /// before every character; a lock mid-text stops it there.
    fn text(&mut self, s: &str) -> std::io::Result<()> {
        let chars: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            // The longest run with at most TEXT_CHUNK distinct characters.
            let mut distinct: Vec<char> = Vec::new();
            let mut end = i;
            while end < chars.len() {
                if !distinct.contains(&chars[end]) {
                    if distinct.len() == TEXT_CHUNK {
                        break;
                    }
                    distinct.push(chars[end]);
                }
                end += 1;
            }
            self.backend.text_keymap(&distinct)?;
            self.held.text_keys = (1..=distinct.len() as u32).collect();
            self.record();
            for c in &chars[i..end] {
                if self.locked_now() {
                    self.held.text_keys.clear();
                    self.pause();
                    return Ok(());
                }
                let code = distinct.iter().position(|d| d == c).expect("in the keymap") as u32 + 1;
                self.backend.text_key(code, true)?;
                self.backend.text_key(code, false)?;
            }
            self.held.text_keys.clear();
            self.record();
            i = end;
        }
        Ok(())
    }

    /// Time passing with nothing from the phone: the grant, the compositor,
    /// the gate and the lease.
    pub fn tick(&mut self, now: Instant) -> Step {
        if !(self.granted)() {
            return Step::End(
                Why::NotAllowed,
                "this phone is no longer allowed to use this computer as a touchpad".into(),
            );
        }
        if let Some(why) = self.backend.gone() {
            return Step::End(Why::Unavailable, why);
        }
        if now.duration_since(self.last_available) >= Duration::from_secs(1) {
            self.last_available = now;
            if let Err(why) = (self.available)() {
                return Step::End(Why::Unavailable, why);
            }
        }
        self.shell = self.gate.shell_file();
        let locked = self.locked_now();
        if locked && (!self.paused || !self.held.is_empty()) {
            self.pause();
        } else if !locked && self.paused {
            self.paused = false;
            (self.out)(json!({"event": "input_resumed"}));
        }
        if !self.held.is_empty() && now.duration_since(self.last_frame) > LEASE {
            self.release_all();
            (self.out)(json!({"event": "input_released", "why": "lease"}));
        }
        Step::Go
    }

    /// The channel is over: let go of everything, and say why when there is
    /// a reason the phone did not give itself.
    pub fn end(&mut self, why: Option<(Why, String)>) {
        self.release_all();
        if let Some((why, message)) = why {
            (self.out)(json!({"event": "input_closed", "why": why.word(), "message": message}));
        }
    }
}

/// What to do about keys and buttons an earlier run left held. Keys get a
/// bare release from the new keyboard; a button needs a whole click to come
/// up on Hyprland, which is only given while the screen is unlocked (a click
/// on a lock screen is input on it). What could not be done is returned, to
/// be kept for next time.
pub fn recover(backend: &mut dyn Backend, held: &Held, unlocked: bool) -> Held {
    for code in held.keys.iter().chain(held.text_keys.iter()) {
        let _ = backend.key(*code, false);
    }
    if !held.keys.is_empty() || !held.text_keys.is_empty() {
        let _ = backend.modifiers(0, 0);
    }
    let mut left = Held::default();
    for code in &held.buttons {
        if unlocked {
            let _ = backend.button(*code, true);
            let _ = backend.button(*code, false);
        } else {
            left.buttons.insert(*code);
        }
    }
    let _ = backend.sync();
    left
}

// ── the live channel ───────────────────────────────────────────────────────

/// The one input channel this machine has open, if any.
struct Live {
    token: u64,
    device: String,
    /// A [`Why`] set by somebody else to end it; 0 for none.
    stop: Arc<AtomicU8>,
    /// The session thread's end, to wake it.
    wake: UnixStream,
    done: Arc<(Mutex<bool>, Condvar)>,
}

fn live() -> &'static Mutex<Option<Live>> {
    static LIVE: OnceLock<Mutex<Option<Live>>> = OnceLock::new();
    LIVE.get_or_init(|| Mutex::new(None))
}

fn lock_live() -> std::sync::MutexGuard<'static, Option<Live>> {
    live().lock().unwrap_or_else(|e| e.into_inner())
}

static TOKENS: AtomicU64 = AtomicU64::new(1);

/// End `device`'s live input channel, if it has one: everything it holds is
/// released and the channel closes, telling the phone `why`. Whether there
/// was one.
pub fn stop_for(device: &str, why: Why) -> bool {
    let guard = lock_live();
    match guard.as_ref() {
        Some(l) if l.device == device => {
            l.stop.store(why as u8, Ordering::SeqCst);
            let _ = l.wake.shutdown(std::net::Shutdown::Read);
            true
        }
        _ => false,
    }
}

/// End whatever channel is live and wait up to `within` for it to have let
/// go of everything: this process is stopping.
pub fn shutdown(within: Duration) {
    let done = {
        let guard = lock_live();
        let Some(l) = guard.as_ref() else { return };
        l.stop.store(Why::Stopping as u8, Ordering::SeqCst);
        let _ = l.wake.shutdown(std::net::Shutdown::Read);
        Arc::clone(&l.done)
    };
    let (m, cv) = &*done;
    let g = m.lock().unwrap_or_else(|e| e.into_inner());
    let _ = cv.wait_timeout_while(g, within, |finished| !*finished);
}

fn error(kind: &str, message: &str) -> Vec<u8> {
    json!({"reply": "error", "kind": kind, "message": message})
        .to_string()
        .into_bytes()
}

fn seqpacket_pair() -> std::io::Result<(UnixStream, UnixStream)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: socketpair writes two descriptors into the array on success.
    let r = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    };
    if r != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both descriptors are new and owned by nobody else.
    Ok(unsafe {
        (
            UnixStream::from_raw_fd(fds[0]),
            UnixStream::from_raw_fd(fds[1]),
        )
    })
}

/// Whether this phone may use this computer as a touchpad, as the store says
/// now.
fn granted(state: &State, device: &str) -> bool {
    state
        .devices()
        .ok()
        .and_then(|s| s.find(device).ok().map(|d| d.input_grant()))
        .unwrap_or(false)
}

/// Open the input channel for `device` on a session of `scope`: the reply,
/// and the end the frame loop writes the phone's frames into. Refused (the
/// reply only) when the phone may not, the screen is locked, there is no
/// session to type into, or another phone has it.
pub fn open(
    state: &Arc<State>,
    device: &str,
    scope: Scope,
    request: &[u8],
) -> Result<(UnixStream, Vec<u8>), Vec<u8>> {
    // `may_open` already keeps companion and peer sessions out; this says it
    // again where the channel is made, so neither check alone is the wall.
    if scope != Scope::Full {
        return Err(error(
            "not_allowed",
            "a touchpad needs the phone's own key; unlock the phone to use it",
        ));
    }
    let v: Value = serde_json::from_slice(request).unwrap_or(Value::Null);
    if v.get("v").and_then(Value::as_u64) != Some(1) {
        return Err(error("bad_request", "this computer speaks input version 1"));
    }
    let name = state
        .devices()
        .ok()
        .and_then(|s| s.find(device).ok().map(|d| d.name.clone()));
    if !granted(state, device) {
        return Err(error(
            "not_allowed",
            &format!(
                "this phone may not use this computer as a touchpad and keyboard; at the computer run \
                 `rime remote devices grant {} input on`",
                name.as_deref().unwrap_or(device)
            ),
        ));
    }
    let env = env();
    if let Err(why) = env.available() {
        return Err(error("unavailable", &why));
    }
    let mut guard = lock_live();
    if guard.is_some() {
        return Err(error(
            "busy",
            "another phone is using this computer as a touchpad",
        ));
    }
    let gate = env.gate();
    let shell = gate.shell_file();
    if locked(&gate.sources(None, shell)) {
        return Err(error(
            "locked",
            "this computer is locked; unlock it at the keyboard first",
        ));
    }
    let mut backend = env.backend().map_err(|why| error("unavailable", &why))?;
    // Keys or buttons an earlier run left held go first, on the new devices.
    let crash = env.crash_file();
    if let Some(left) = read_held(&crash) {
        let unlocked = !locked(&gate.sources(backend.compositor_locked(), gate.shell_file()));
        write_held(&crash, &recover(backend.as_mut(), &left, unlocked));
    }
    // The compositor's notifier is known only now, after the round trip.
    if locked(&gate.sources(backend.compositor_locked(), gate.shell_file())) {
        return Err(error(
            "locked",
            "this computer is locked; unlock it at the keyboard first",
        ));
    }
    let (theirs, mine) = seqpacket_pair().map_err(|e| error("unavailable", &e.to_string()))?;
    let wake = mine
        .try_clone()
        .map_err(|e| error("unavailable", &e.to_string()))?;
    let writer = mine
        .try_clone()
        .map_err(|e| error("unavailable", &e.to_string()))?;
    let token = TOKENS.fetch_add(1, Ordering::Relaxed);
    let stop = Arc::new(AtomicU8::new(0));
    let done = Arc::new((Mutex::new(false), Condvar::new()));
    let st = Arc::clone(state);
    let dev = device.to_string();
    let session = Session::new(
        backend,
        gate,
        Box::new(move || granted(&st, &dev)),
        Box::new(move || env.available()),
        // One JSON line per event, as on `device_events`.
        Box::new(move |line: Value| {
            let _ = (&writer).write_all(format!("{line}\n").as_bytes());
        }),
        Some(crash),
        Instant::now(),
    );
    let (stop2, done2) = (Arc::clone(&stop), Arc::clone(&done));
    std::thread::Builder::new()
        .name("rime-input".into())
        .spawn(move || run(session, mine, stop2, done2, token))
        .map_err(|e| error("unavailable", &format!("no thread for the touchpad: {e}")))?;
    *guard = Some(Live {
        token,
        device: device.to_string(),
        stop,
        wake,
        done,
    });
    Ok((
        theirs,
        json!({"reply": "input_ready"}).to_string().into_bytes(),
    ))
}

/// The session thread: frames in, ticks between, everything released at the
/// end however it ends.
fn run(
    mut session: Session,
    sock: UnixStream,
    stop: Arc<AtomicU8>,
    done: Arc<(Mutex<bool>, Condvar)>,
    token: u64,
) {
    let mut buf = vec![0u8; MAX_FRAME_BYTES + 1];
    let ended = loop {
        if let Some(why) = Why::from_u8(stop.load(Ordering::SeqCst)) {
            break Some((why, String::new()));
        }
        let _ = sock.set_read_timeout(Some(session.tick_every()));
        let step = match (&sock).read(&mut buf) {
            // The phone closed the channel, the connection ended, or
            // somebody stopped it: `stop` says which.
            Ok(0) => match Why::from_u8(stop.load(Ordering::SeqCst)) {
                Some(why) => break Some((why, String::new())),
                None => break None,
            },
            Ok(n) => session.frame(&buf[..n], Instant::now()),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                session.tick(Instant::now())
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Step::Go,
            Err(_) => break None,
        };
        if let Step::End(why, message) = step {
            break Some((why, message));
        }
    };
    let ended = ended.map(|(why, message)| {
        let message = if message.is_empty() {
            match why {
                Why::NotAllowed => {
                    "this phone is no longer allowed to use this computer as a touchpad".into()
                }
                Why::Stopping => "Rime Remote on this computer is stopping".into(),
                _ => String::new(),
            }
        } else {
            message
        };
        (why, message)
    });
    session.end(ended);
    drop(session);
    drop(sock);
    {
        let mut guard = lock_live();
        if guard.as_ref().is_some_and(|l| l.token == token) {
            *guard = None;
        }
    }
    let (m, cv) = &*done;
    *m.lock().unwrap_or_else(|e| e.into_inner()) = true;
    cv.notify_all();
}

// ── where input goes ───────────────────────────────────────────────────────

/// The stub, or the person's graphical session.
enum Env {
    Stub(PathBuf),
    Real,
}

fn env() -> &'static Env {
    static CHOSEN: OnceLock<Env> = OnceLock::new();
    CHOSEN.get_or_init(|| match std::env::var_os(STUB_ENV) {
        Some(dir) if !dir.is_empty() => Env::Stub(PathBuf::from(dir)),
        // A unit test never reaches a real desktop, whatever its environment.
        _ if cfg!(test) => Env::Stub(
            std::env::temp_dir().join(format!("rime-remoted-input-unit-{}", std::process::id())),
        ),
        _ => Env::Real,
    })
}

impl Env {
    fn runtime_file(&self, name: &str) -> PathBuf {
        match self {
            Env::Stub(dir) if cfg!(test) => dir.join(name),
            _ => rime_agent_core::paths::runtime_dir()
                .join("rime-remote")
                .join(name),
        }
    }

    fn crash_file(&self) -> PathBuf {
        self.runtime_file("input-held.json")
    }

    fn gate(&self) -> Gate {
        let shell = Some(self.runtime_file("lock.json"));
        match self {
            Env::Stub(dir) => {
                let dir = dir.clone();
                Gate {
                    logind: Box::new(move || {
                        let v = StubBackend::sources(&dir);
                        (
                            v.get("locked_hint").and_then(Value::as_bool),
                            v.get("lock_signal")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        )
                    }),
                    shell,
                }
            }
            Env::Real => {
                let watch = logind::watch();
                Gate {
                    logind: Box::new(move || watch.read()),
                    shell,
                }
            }
        }
    }

    fn available(&self) -> Result<(), String> {
        match self {
            Env::Stub(dir) => match std::fs::read_to_string(dir.join("unavailable")) {
                Ok(why) => Err(why.trim().to_string()),
                Err(_) => Ok(()),
            },
            Env::Real => {
                let runtime = rime_agent_core::paths::runtime_dir();
                if wayland::gaming(&runtime) {
                    return Err(
                        "Gaming Mode is running; the touchpad works on the desktop only".into(),
                    );
                }
                if logind::watch().no_session() {
                    return Err("nobody is logged in to a desktop on this computer".into());
                }
                Ok(())
            }
        }
    }

    fn backend(&self) -> Result<Box<dyn Backend>, String> {
        match self {
            Env::Stub(dir) => StubBackend::open(dir)
                .map(|b| Box::new(b) as Box<dyn Backend>)
                .map_err(|e| e.to_string()),
            Env::Real => {
                let socket = wayland::discover(&rime_agent_core::paths::runtime_dir())?;
                let layout = wayland::Layout::of_session();
                wayland::Wayland::connect(&socket, &layout).map(|b| Box::new(b) as Box<dyn Backend>)
            }
        }
    }
}

/// At start: begin following logind, so the first channel a phone opens
/// finds the lock state already known instead of unknown (and refused).
pub fn prepare() {
    if let Env::Real = env() {
        let _ = logind::watch();
    }
}

/// At start: release what a run that crashed left held, as far as can be
/// done now. On its own thread, since the compositor may not be up yet; what
/// is left waits for the next channel.
pub fn recover_at_start() {
    let env = env();
    let crash = env.crash_file();
    let Some(left) = read_held(&crash) else {
        return;
    };
    std::thread::spawn(move || {
        // logind's first answer, or unknown (locked) after a moment.
        let gate = env.gate();
        let deadline = Instant::now() + Duration::from_secs(3);
        while (gate.logind)().0.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        let Ok(mut backend) = env.backend() else {
            return;
        };
        let unlocked = !locked(&gate.sources(backend.compositor_locked(), gate.shell_file()));
        write_held(&crash, &recover(backend.as_mut(), &left, unlocked));
    });
}

// ── logind ─────────────────────────────────────────────────────────────────

mod logind {
    //! Sources 2 and 3: the graphical session's `LockedHint` and its `Lock`
    //! and `Unlock` signals, from a system-bus connection on a thread of its
    //! own (a `loginctl` per event would be far too slow).

    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Duration;

    use zbus::export::futures_util::StreamExt;

    #[derive(Default)]
    struct Seen {
        hint: Option<bool>,
        signal: bool,
        no_session: bool,
    }

    /// What the watcher last saw.
    #[derive(Clone)]
    pub struct Watch(Arc<Mutex<Seen>>);

    impl Watch {
        /// `(LockedHint, lock signal outstanding)`.
        pub fn read(&self) -> (Option<bool>, bool) {
            let s = self.0.lock().unwrap_or_else(|e| e.into_inner());
            (s.hint, s.signal)
        }

        pub fn no_session(&self) -> bool {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).no_session
        }

        fn set(&self, f: impl FnOnce(&mut Seen)) {
            f(&mut self.0.lock().unwrap_or_else(|e| e.into_inner()));
        }
    }

    /// The process's watcher, started on first use.
    pub fn watch() -> Watch {
        static W: OnceLock<Watch> = OnceLock::new();
        W.get_or_init(|| {
            let w = Watch(Arc::new(Mutex::new(Seen::default())));
            let w2 = w.clone();
            let started = std::thread::Builder::new().name("rime-input-logind".into()).spawn(move || {
                let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
                rt.block_on(async move {
                    loop {
                        if let Err(e) = follow(&w2).await {
                            eprintln!("rime-remoted: touchpad lock watch: {e}; counting the screen as locked");
                        }
                        // Whatever happened, nothing is known until the next
                        // connection says so.
                        w2.set(|s| {
                            s.hint = None;
                            s.signal = false;
                        });
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                });
            });
            if let Err(e) = started {
                eprintln!("rime-remoted: no thread to watch the lock state ({e}); the touchpad stays off");
            }
            w
        })
        .clone()
    }

    const DEST: &str = "org.freedesktop.login1";

    async fn proxy<'a>(
        conn: &zbus::Connection,
        path: &str,
        iface: &'static str,
    ) -> zbus::Result<zbus::Proxy<'a>> {
        zbus::ProxyBuilder::new(conn)
            .destination(DEST)?
            .path(path.to_string())?
            .interface(iface)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
    }

    /// Follow the user's graphical session until something changes that
    /// needs a fresh start (the bus went, the session changed).
    async fn follow(w: &Watch) -> zbus::Result<()> {
        let conn = zbus::Connection::system().await?;
        let manager = proxy(
            &conn,
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        )
        .await?;
        let uid = unsafe { libc::getuid() };
        let user: zbus::zvariant::OwnedObjectPath = manager.call("GetUser", &(uid,)).await?;
        let user = proxy(&conn, user.as_str(), "org.freedesktop.login1.User").await?;
        let (_, display): (String, zbus::zvariant::OwnedObjectPath) =
            user.get_property("Display").await?;
        let mut user_changes = zbus::fdo::PropertiesProxy::builder(&conn)
            .destination(DEST)?
            .path(user.path().to_owned())?
            .build()
            .await?
            .receive_properties_changed()
            .await?;
        if display.as_str() == "/" {
            w.set(|s| {
                s.hint = None;
                s.no_session = true;
            });
            // Wait for a login: the user's Display changes.
            let _ = user_changes.next().await;
            w.set(|s| s.no_session = false);
            return Ok(());
        }
        let session = proxy(&conn, display.as_str(), "org.freedesktop.login1.Session").await?;
        let mut locks = session.receive_signal("Lock").await?;
        let mut unlocks = session.receive_signal("Unlock").await?;
        let mut changes = zbus::fdo::PropertiesProxy::builder(&conn)
            .destination(DEST)?
            .path(display.as_str().to_string())?
            .build()
            .await?
            .receive_properties_changed()
            .await?;
        let hint: bool = session.get_property("LockedHint").await?;
        w.set(|s| {
            s.hint = Some(hint);
            s.no_session = false;
        });
        // Signals can be missed; the property is read again now and then.
        let mut again = tokio::time::interval(Duration::from_secs(5));
        again.tick().await;
        loop {
            tokio::select! {
                m = locks.next() => {
                    m.ok_or_else(|| zbus::Error::Failure("the Lock signal stream ended".into()))?;
                    w.set(|s| s.signal = true);
                }
                m = unlocks.next() => {
                    m.ok_or_else(|| zbus::Error::Failure("the Unlock signal stream ended".into()))?;
                    w.set(|s| s.signal = false);
                }
                c = changes.next() => {
                    let c = c.ok_or_else(|| zbus::Error::Failure("the session's property stream ended".into()))?;
                    if let Ok(args) = c.args() {
                        if let Some(v) = args.changed_properties().get("LockedHint") {
                            let now = bool::try_from(v).ok();
                            apply_hint(w, now);
                        }
                    }
                }
                c = user_changes.next() => {
                    let c = c.ok_or_else(|| zbus::Error::Failure("the user's property stream ended".into()))?;
                    if c.args().is_ok_and(|a| a.changed_properties().contains_key("Display")) {
                        return Ok(());
                    }
                }
                _ = again.tick() => {
                    let now: bool = session.get_property("LockedHint").await?;
                    apply_hint(w, Some(now));
                }
            }
        }
    }

    /// A new `LockedHint`: true→false also clears an outstanding `Lock`
    /// signal (the session was locked and is unlocked again).
    fn apply_hint(w: &Watch, now: Option<bool>) {
        w.set(|s| {
            if s.hint == Some(true) && now == Some(false) {
                s.signal = false;
            }
            s.hint = now;
        });
    }
}

// ── Wayland ────────────────────────────────────────────────────────────────

pub mod wayland {
    //! The person's compositor, spoken to over its socket directly: a
    //! virtual pointer, a keyboard with the session's layout, and a keyboard
    //! for text. No libwayland and no libxkbcommon in this process: keymaps
    //! are sent as XKB text with includes, which the compositor compiles
    //! against the system's own XKB data.

    use super::*;

    /// The compositor's `wayland-N` socket in `runtime`, found the way
    /// `rime-agentd`'s clipboard finds it: `WAYLAND_DISPLAY` first when it is
    /// a name, then every `wayland-<digits>` that accepts a connection (a
    /// stale socket refuses one).
    pub fn discover(runtime: &Path) -> Result<PathBuf, String> {
        if cfg!(test) {
            return Err("unit tests never look for a desktop".into());
        }
        let mut names = Vec::new();
        if let Some(v) = std::env::var_os("WAYLAND_DISPLAY") {
            let v = v.to_string_lossy().into_owned();
            if !v.is_empty() && !v.contains('/') {
                names.push(v);
            }
        }
        if let Ok(entries) = std::fs::read_dir(runtime) {
            let mut found: Vec<String> = entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| {
                    n.strip_prefix("wayland-")
                        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
                })
                .collect();
            found.sort_by_key(|n| n["wayland-".len()..].parse::<u32>().unwrap_or(u32::MAX));
            names.extend(found);
        }
        for n in names {
            let path = runtime.join(&n);
            if UnixStream::connect(&path).is_ok() {
                return Ok(path);
            }
        }
        Err("no desktop is running for this user on this computer".into())
    }

    /// Whether gamescope (Gaming Mode) is running: one of its `-ei` sockets
    /// accepts a connection. Asked with connect(2), not by the file being
    /// there, so a stale socket left by a crash does not turn the touchpad
    /// off for good.
    pub fn gaming(runtime: &Path) -> bool {
        let Ok(entries) = std::fs::read_dir(runtime) else {
            return false;
        };
        entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .any(|n| {
                n.strip_prefix("gamescope-")
                    .and_then(|r| r.strip_suffix("-ei"))
                    .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
                    && UnixStream::connect(runtime.join(&n)).is_ok()
            })
    }

    /// The session's keyboard layout, as XKB names.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Layout {
        pub layouts: Vec<String>,
        pub variants: Vec<String>,
    }

    fn safe(name: &str) -> bool {
        name.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }

    impl Layout {
        /// From `XKB_DEFAULT_LAYOUT` / `XKB_DEFAULT_VARIANT` (what Rime's
        /// keymap generator sets for the session), else the system's
        /// `00-keyboard.conf`, else `us`. A name with anything but letters,
        /// digits, `_` and `-` in it is not put into a keymap: `us` instead.
        pub fn of_session() -> Layout {
            let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
            if let Some(l) = env("XKB_DEFAULT_LAYOUT") {
                return Layout::from_names(&l, &env("XKB_DEFAULT_VARIANT").unwrap_or_default());
            }
            if let Ok(conf) = std::fs::read_to_string("/etc/X11/xorg.conf.d/00-keyboard.conf") {
                let option = |name: &str| {
                    conf.lines().find_map(|l| {
                        let l = l.trim();
                        let rest = l.strip_prefix("Option")?.trim();
                        let rest = rest.strip_prefix(&format!("\"{name}\""))?.trim();
                        Some(rest.trim_matches('"').to_string())
                    })
                };
                if let Some(l) = option("XkbLayout") {
                    return Layout::from_names(&l, &option("XkbVariant").unwrap_or_default());
                }
            }
            Layout::from_names("us", "")
        }

        pub fn from_names(layouts: &str, variants: &str) -> Layout {
            let layouts: Vec<String> = layouts.split(',').map(|s| s.trim().to_string()).collect();
            let variants: Vec<String> = variants.split(',').map(|s| s.trim().to_string()).collect();
            if layouts.is_empty()
                || layouts.len() > 4
                || layouts.iter().any(|l| l.is_empty() || !safe(l))
                || variants.iter().any(|v| !safe(v))
            {
                return Layout {
                    layouts: vec!["us".into()],
                    variants: vec![],
                };
            }
            Layout { layouts, variants }
        }

        /// The keymap for the fixed keyboard.
        pub fn keymap(&self) -> String {
            let mut symbols = String::from("pc");
            for (i, l) in self.layouts.iter().enumerate() {
                let v = self.variants.get(i).filter(|v| !v.is_empty());
                symbols.push('+');
                symbols.push_str(l);
                if let Some(v) = v {
                    symbols.push_str(&format!("({v})"));
                }
                if i > 0 {
                    symbols.push_str(&format!(":{}", i + 1));
                }
            }
            symbols.push_str("+inet(evdev)");
            format!(
                "xkb_keymap {{\n  xkb_keycodes {{ include \"evdev+aliases(qwerty)\" }};\n  \
                 xkb_types {{ include \"complete\" }};\n  xkb_compat {{ include \"complete\" }};\n  \
                 xkb_symbols {{ include \"{symbols}\" }};\n}};\n"
            )
        }
    }

    /// A keymap whose keycode `9 + i` (evdev `1 + i`) types `chars[i]`.
    pub fn text_keymap(chars: &[char]) -> String {
        let mut codes = String::new();
        let mut syms = String::new();
        for (i, c) in chars.iter().enumerate() {
            codes.push_str(&format!("    <I{}> = {};\n", i + 9, i + 9));
            let sym = match c {
                '\n' => "Return".to_string(),
                '\t' => "Tab".to_string(),
                c => format!("U{:04X}", *c as u32),
            };
            syms.push_str(&format!("    key <I{}> {{ [ {sym} ] }};\n", i + 9));
        }
        format!(
            "xkb_keymap {{\n  xkb_keycodes {{\n    minimum = 8;\n    maximum = {};\n{codes}  }};\n  \
             xkb_types {{ include \"complete\" }};\n  xkb_compat {{ include \"complete\" }};\n  \
             xkb_symbols {{\n{syms}  }};\n}};\n",
            chars.len() + 9
        )
    }

    fn put_u32(b: &mut Vec<u8>, v: u32) {
        b.extend_from_slice(&v.to_le_bytes());
    }

    fn put_i32(b: &mut Vec<u8>, v: i32) {
        b.extend_from_slice(&v.to_le_bytes());
    }

    fn put_fixed(b: &mut Vec<u8>, v: f64) {
        put_i32(b, (v * 256.0).round() as i32);
    }

    fn put_str(b: &mut Vec<u8>, s: &str) {
        let len = s.len() + 1;
        put_u32(b, len as u32);
        b.extend_from_slice(s.as_bytes());
        b.push(0);
        while b.len() % 4 != 0 {
            b.push(0);
        }
    }

    /// One request on the wire.
    pub fn message(obj: u32, op: u16, args: &[u8]) -> Vec<u8> {
        let mut m = Vec::with_capacity(8 + args.len());
        put_u32(&mut m, obj);
        put_u32(&mut m, (((8 + args.len()) as u32) << 16) | op as u32);
        m.extend_from_slice(args);
        m
    }

    #[derive(Default)]
    struct Heard {
        globals: Vec<(u32, String, u32)>,
        done: BTreeSet<u32>,
        locked: Option<bool>,
        gone: Option<String>,
    }

    struct Shared {
        heard: Mutex<Heard>,
        cv: Condvar,
        note: std::sync::atomic::AtomicU32,
    }

    const DISPLAY: u32 = 1;
    const REGISTRY: u32 = 2;
    const WAIT: Duration = Duration::from_secs(2);

    /// The connection, the devices on it, and the thread reading events.
    pub struct Wayland {
        sock: UnixStream,
        shared: Arc<Shared>,
        next: u32,
        pointer: u32,
        keyboard: u32,
        text: u32,
        epoch: Instant,
        text_chars: Vec<char>,
        closed: Arc<AtomicBool>,
    }

    fn read_u32(b: &[u8], at: usize) -> Option<u32> {
        b.get(at..at + 4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }

    fn read_str(b: &[u8], at: usize) -> Option<(String, usize)> {
        let len = read_u32(b, at)? as usize;
        let bytes = b.get(at + 4..at + 4 + len)?;
        let s = String::from_utf8_lossy(bytes.strip_suffix(&[0]).unwrap_or(bytes)).into_owned();
        Some((s, at + 4 + ((len + 3) & !3)))
    }

    fn reader(mut sock: UnixStream, shared: Arc<Shared>, closed: Arc<AtomicBool>) {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        let why = loop {
            let n = match sock.read(&mut chunk) {
                Ok(0) => break "the compositor closed the connection".to_string(),
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => break format!("reading from the compositor: {e}"),
            };
            buf.extend_from_slice(&chunk[..n]);
            let mut bad = None;
            while buf.len() >= 8 {
                let obj = read_u32(&buf, 0).unwrap_or(0);
                let so = read_u32(&buf, 4).unwrap_or(0);
                let (size, op) = ((so >> 16) as usize, so & 0xffff);
                if size < 8 {
                    bad = Some("the compositor sent a malformed message".to_string());
                    break;
                }
                if buf.len() < size {
                    break;
                }
                let body: Vec<u8> = buf.drain(..size).skip(8).collect();
                let mut h = shared.heard.lock().unwrap_or_else(|e| e.into_inner());
                let note = shared.note.load(Ordering::SeqCst);
                if obj == DISPLAY && op == 0 {
                    let code = read_u32(&body, 4).unwrap_or(0);
                    let text = read_str(&body, 8).map(|(s, _)| s).unwrap_or_default();
                    bad = Some(format!(
                        "the compositor refused a request (error {code}: {text})"
                    ));
                } else if obj == REGISTRY && op == 0 {
                    if let (Some(name), Some((iface, at))) =
                        (read_u32(&body, 0), read_str(&body, 4))
                    {
                        let version = read_u32(&body, at).unwrap_or(1);
                        h.globals.push((name, iface, version));
                    }
                } else if note != 0 && obj == note {
                    h.locked = Some(op == 0);
                } else if obj > REGISTRY && op == 0 {
                    // A wl_callback's `done` (and the first event of any
                    // other object, which is harmless to note).
                    h.done.insert(obj);
                }
                drop(h);
                shared.cv.notify_all();
                if bad.is_some() {
                    break;
                }
            }
            if let Some(why) = bad {
                break why;
            }
        };
        if !closed.load(Ordering::SeqCst) {
            eprintln!("rime-remoted: touchpad: {why}");
        }
        shared.heard.lock().unwrap_or_else(|e| e.into_inner()).gone = Some(why);
        shared.cv.notify_all();
    }

    impl Wayland {
        /// Connect to the compositor at `socket` and make the three devices.
        /// Refused when it offers no virtual pointer or keyboard (gamescope).
        pub fn connect(socket: &Path, layout: &Layout) -> Result<Wayland, String> {
            let sock = UnixStream::connect(socket)
                .map_err(|e| format!("cannot reach the desktop: {e}"))?;
            Wayland::over(sock, layout)
        }

        /// The same over a connected socket: what a test's scripted
        /// compositor gives it.
        pub fn over(sock: UnixStream, layout: &Layout) -> Result<Wayland, String> {
            sock.set_write_timeout(Some(WAIT))
                .map_err(|e| e.to_string())?;
            let shared = Arc::new(Shared {
                heard: Mutex::new(Heard::default()),
                cv: Condvar::new(),
                note: std::sync::atomic::AtomicU32::new(0),
            });
            let closed = Arc::new(AtomicBool::new(false));
            let read_half = sock.try_clone().map_err(|e| e.to_string())?;
            {
                let (shared, closed) = (Arc::clone(&shared), Arc::clone(&closed));
                std::thread::Builder::new()
                    .name("rime-input-wl".into())
                    .spawn(move || reader(read_half, shared, closed))
                    .map_err(|e| e.to_string())?;
            }
            let mut w = Wayland {
                sock,
                shared,
                next: REGISTRY + 1,
                pointer: 0,
                keyboard: 0,
                text: 0,
                epoch: Instant::now(),
                text_chars: Vec::new(),
                closed,
            };
            let fail = |e: std::io::Error| format!("the desktop stopped answering: {e}");
            let mut args = Vec::new();
            put_u32(&mut args, REGISTRY);
            w.send(DISPLAY, 1, &args, None).map_err(fail)?;
            w.sync().map_err(fail)?;
            let globals = w
                .shared
                .heard
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .globals
                .clone();
            let find = |iface: &str| globals.iter().find(|(_, i, _)| i == iface).cloned();
            let (Some(seat), Some(pointers), Some(keyboards)) = (
                find("wl_seat"),
                find("zwlr_virtual_pointer_manager_v1"),
                find("zwp_virtual_keyboard_manager_v1"),
            ) else {
                return Err("this desktop does not take virtual input (Gaming Mode?)".into());
            };
            let seat = w.bind(&seat, 1).map_err(fail)?;
            let pointers = w.bind(&pointers, 2).map_err(fail)?;
            let keyboards = w.bind(&keyboards, 1).map_err(fail)?;
            if let Some(notifier) = find("hyprland_lock_notifier_v1") {
                let notifier = w.bind(&notifier, 1).map_err(fail)?;
                let note = w.id();
                w.shared.note.store(note, Ordering::SeqCst);
                let mut a = Vec::new();
                put_u32(&mut a, note);
                w.send(notifier, 1, &a, None).map_err(fail)?;
            }
            w.pointer = w.id();
            let mut a = Vec::new();
            put_u32(&mut a, seat);
            put_u32(&mut a, w.pointer);
            w.send(pointers, 0, &a, None).map_err(fail)?;
            w.keyboard = w.id();
            let mut a = Vec::new();
            put_u32(&mut a, seat);
            put_u32(&mut a, w.keyboard);
            w.send(keyboards, 0, &a, None).map_err(fail)?;
            w.text = w.id();
            let mut a = Vec::new();
            put_u32(&mut a, seat);
            put_u32(&mut a, w.text);
            w.send(keyboards, 0, &a, None).map_err(fail)?;
            w.keymap(w.keyboard, &layout.keymap()).map_err(fail)?;
            w.sync().map_err(fail)?;
            // Hyprland drops a new pointer's first clicks until it has seen
            // a relative motion (measured): one there and back.
            w.motion(1.0, 0.0).map_err(fail)?;
            w.motion(-1.0, 0.0).map_err(fail)?;
            w.sync().map_err(fail)?;
            Ok(w)
        }

        fn id(&mut self) -> u32 {
            let i = self.next;
            self.next += 1;
            i
        }

        fn ms(&self) -> u32 {
            self.epoch.elapsed().as_millis() as u32
        }

        fn bind(&mut self, global: &(u32, String, u32), version: u32) -> std::io::Result<u32> {
            let id = self.id();
            let mut a = Vec::new();
            put_u32(&mut a, global.0);
            put_str(&mut a, &global.1);
            put_u32(&mut a, global.2.min(version));
            put_u32(&mut a, id);
            self.send(REGISTRY, 0, &a, None)?;
            Ok(id)
        }

        fn send(
            &mut self,
            obj: u32,
            op: u16,
            args: &[u8],
            fd: Option<RawFd>,
        ) -> std::io::Result<()> {
            if let Some(why) = self.gone() {
                return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, why));
            }
            let m = message(obj, op, args);
            match fd {
                None => (&self.sock).write_all(&m),
                Some(fd) => send_with_fd(&self.sock, &m, fd),
            }
        }

        fn keymap(&mut self, keyboard: u32, text: &str) -> std::io::Result<()> {
            let name = std::ffi::CString::new("rime-keymap").expect("no NUL");
            // SAFETY: memfd_create with a valid NUL-terminated name.
            let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: fd is new and owned here; File closes it.
            let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
            let mut bytes = text.as_bytes().to_vec();
            bytes.push(0);
            file.write_all(&bytes)?;
            let mut a = Vec::new();
            put_u32(&mut a, 1); // WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1
            put_u32(&mut a, bytes.len() as u32);
            self.send(keyboard, 0, &a, Some(file.as_raw_fd()))
        }

        fn pointer_frame(&mut self) -> std::io::Result<()> {
            self.send(self.pointer, 4, &[], None)
        }
    }

    fn send_with_fd(sock: &UnixStream, m: &[u8], fd: RawFd) -> std::io::Result<()> {
        // SAFETY: a single-descriptor SCM_RIGHTS control message in a buffer
        // CMSG_SPACE says is large enough, for the duration of the call.
        unsafe {
            let space = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as usize;
            let mut control = vec![0u8; space];
            let mut iov = libc::iovec {
                iov_base: m.as_ptr() as *mut libc::c_void,
                iov_len: m.len(),
            };
            let mut msg: libc::msghdr = std::mem::zeroed();
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
            msg.msg_controllen = space as _;
            let cmsg = libc::CMSG_FIRSTHDR(&msg);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
            std::ptr::copy_nonoverlapping(
                &fd as *const RawFd as *const u8,
                libc::CMSG_DATA(cmsg),
                std::mem::size_of::<RawFd>(),
            );
            let n = libc::sendmsg(sock.as_raw_fd(), &msg, libc::MSG_NOSIGNAL);
            if n < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if n as usize != m.len() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "a short write with a keymap",
                ));
            }
        }
        Ok(())
    }

    impl Backend for Wayland {
        fn motion(&mut self, dx: f64, dy: f64) -> std::io::Result<()> {
            let mut a = Vec::new();
            put_u32(&mut a, self.ms());
            put_fixed(&mut a, dx);
            put_fixed(&mut a, dy);
            self.send(self.pointer, 0, &a, None)?;
            self.pointer_frame()
        }

        fn button(&mut self, code: u32, pressed: bool) -> std::io::Result<()> {
            let mut a = Vec::new();
            put_u32(&mut a, self.ms());
            put_u32(&mut a, code);
            put_u32(&mut a, pressed as u32);
            self.send(self.pointer, 2, &a, None)?;
            self.pointer_frame()
        }

        fn scroll(&mut self, dx: f64, dy: f64, src: Src, stop: bool) -> std::io::Result<()> {
            let t = self.ms();
            let mut a = Vec::new();
            put_u32(&mut a, if src == Src::Wheel { 0 } else { 1 });
            self.send(self.pointer, 5, &a, None)?; // axis_source
            for (axis, v) in [(0u32, dy), (1u32, dx)] {
                if v != 0.0 {
                    let mut a = Vec::new();
                    put_u32(&mut a, t);
                    put_u32(&mut a, axis);
                    put_fixed(&mut a, v);
                    let discrete = (v / 15.0).round() as i32;
                    if src == Src::Wheel && discrete != 0 {
                        put_i32(&mut a, discrete);
                        self.send(self.pointer, 7, &a, None)?; // axis_discrete
                    } else {
                        self.send(self.pointer, 3, &a, None)?; // axis
                    }
                }
                if stop && src == Src::Finger {
                    let mut a = Vec::new();
                    put_u32(&mut a, t);
                    put_u32(&mut a, axis);
                    self.send(self.pointer, 6, &a, None)?; // axis_stop
                }
            }
            self.pointer_frame()
        }

        fn key(&mut self, code: u32, pressed: bool) -> std::io::Result<()> {
            let mut a = Vec::new();
            put_u32(&mut a, self.ms());
            put_u32(&mut a, code);
            put_u32(&mut a, pressed as u32);
            self.send(self.keyboard, 1, &a, None)
        }

        fn modifiers(&mut self, depressed: u32, locked: u32) -> std::io::Result<()> {
            let mut a = Vec::new();
            for v in [depressed, 0, locked, 0] {
                put_u32(&mut a, v);
            }
            self.send(self.keyboard, 2, &a, None)
        }

        fn text_keymap(&mut self, chars: &[char]) -> std::io::Result<()> {
            if self.text_chars != chars {
                self.keymap(self.text, &text_keymap(chars))?;
                self.text_chars = chars.to_vec();
                // No modifiers on the text keyboard, ever.
                let mut a = Vec::new();
                for _ in 0..4 {
                    put_u32(&mut a, 0);
                }
                self.send(self.text, 2, &a, None)?;
            }
            // Hyprland loses the first character unless the keymap has been
            // taken before the first key (measured).
            self.sync()
        }

        fn text_key(&mut self, code: u32, pressed: bool) -> std::io::Result<()> {
            let mut a = Vec::new();
            put_u32(&mut a, self.ms());
            put_u32(&mut a, code);
            put_u32(&mut a, pressed as u32);
            self.send(self.text, 1, &a, None)
        }

        fn sync(&mut self) -> std::io::Result<()> {
            let cb = self.id();
            let mut a = Vec::new();
            put_u32(&mut a, cb);
            self.send(DISPLAY, 0, &a, None)?;
            let deadline = Instant::now() + WAIT;
            let mut h = self.shared.heard.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if h.done.remove(&cb) {
                    return Ok(());
                }
                if let Some(why) = &h.gone {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        why.clone(),
                    ));
                }
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "the desktop did not answer in time",
                    ));
                }
                h = self
                    .shared
                    .cv
                    .wait_timeout(h, left)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        }

        fn compositor_locked(&self) -> Option<bool> {
            self.shared
                .heard
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .locked
        }

        fn gone(&self) -> Option<String> {
            self.shared
                .heard
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .gone
                .clone()
        }
    }

    impl Drop for Wayland {
        fn drop(&mut self) {
            // Closing the connection destroys every device on it.
            self.closed.store(true, Ordering::SeqCst);
            let _ = self.sock.shutdown(std::net::Shutdown::Both);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A backend that writes what it was asked into a shared list.
    #[derive(Clone, Default)]
    struct Fake {
        log: Arc<Mutex<Vec<String>>>,
        compositor: Arc<Mutex<Option<bool>>>,
    }

    impl Fake {
        fn take(&self) -> Vec<String> {
            std::mem::take(&mut *self.log.lock().expect("log"))
        }
        fn push(&self, s: String) -> std::io::Result<()> {
            self.log.lock().expect("log").push(s);
            Ok(())
        }
    }

    impl Backend for Fake {
        fn motion(&mut self, dx: f64, dy: f64) -> std::io::Result<()> {
            self.push(format!("motion {dx} {dy}"))
        }
        fn button(&mut self, code: u32, p: bool) -> std::io::Result<()> {
            self.push(format!("button {code:#x} {p}"))
        }
        fn scroll(&mut self, dx: f64, dy: f64, src: Src, stop: bool) -> std::io::Result<()> {
            self.push(format!("scroll {dx} {dy} {} {stop}", src.word()))
        }
        fn key(&mut self, code: u32, p: bool) -> std::io::Result<()> {
            self.push(format!("key {code} {p}"))
        }
        fn modifiers(&mut self, d: u32, l: u32) -> std::io::Result<()> {
            self.push(format!("mods {d:#x} {l:#x}"))
        }
        fn text_keymap(&mut self, chars: &[char]) -> std::io::Result<()> {
            self.push(format!("keymap {}", chars.len()))
        }
        fn text_key(&mut self, code: u32, p: bool) -> std::io::Result<()> {
            self.push(format!("text {code} {p}"))
        }
        fn sync(&mut self) -> std::io::Result<()> {
            self.push("sync".into())
        }
        fn compositor_locked(&self) -> Option<bool> {
            *self.compositor.lock().expect("c")
        }
        fn gone(&self) -> Option<String> {
            None
        }
    }

    /// A session on a fake backend, with switches for the gate and grant.
    struct Rig {
        fake: Fake,
        hint: Arc<Mutex<(Option<bool>, bool)>>,
        grant: Arc<AtomicBool>,
        said: Arc<Mutex<Vec<Value>>>,
        session: Session,
        t0: Instant,
        dir: PathBuf,
    }

    fn rig(tag: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!("rime-input-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let fake = Fake::default();
        let hint = Arc::new(Mutex::new((Some(false), false)));
        let grant = Arc::new(AtomicBool::new(true));
        let said = Arc::new(Mutex::new(Vec::new()));
        let (h, g, s) = (Arc::clone(&hint), Arc::clone(&grant), Arc::clone(&said));
        let t0 = Instant::now();
        let session = Session::new(
            Box::new(fake.clone()),
            Gate {
                logind: Box::new(move || *h.lock().expect("h")),
                shell: Some(dir.join("lock.json")),
            },
            Box::new(move || g.load(Ordering::SeqCst)),
            Box::new(|| Ok(())),
            Box::new(move |v| s.lock().expect("s").push(v)),
            Some(dir.join("input-held.json")),
            t0,
        );
        Rig {
            fake,
            hint,
            grant,
            said,
            session,
            t0,
            dir,
        }
    }

    impl Rig {
        fn at(&self, ms: u64) -> Instant {
            self.t0 + Duration::from_millis(ms)
        }
        fn frame(&mut self, ms: u64, seq: u64, ev: Value) -> Step {
            let bytes = json!({"seq": seq, "ev": ev}).to_string();
            let at = self.at(ms);
            self.session.frame(bytes.as_bytes(), at)
        }
        fn said(&self) -> Vec<Value> {
            std::mem::take(&mut *self.said.lock().expect("s"))
        }
        fn lock(&self, hint: Option<bool>, signal: bool) {
            *self.hint.lock().expect("h") = (hint, signal);
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn locked_is_the_union_of_every_source_and_only_logind_can_say_unlocked() {
        let open = Sources {
            locked_hint: Some(false),
            ..Sources::default()
        };
        assert!(!locked(&open));
        // Unknown is locked: no logind answer, whatever the others say.
        assert!(locked(&Sources::default()));
        assert!(locked(&Sources {
            notifier: Some(false),
            shell: ShellFile::Unlocked,
            ..Sources::default()
        }));
        assert!(locked(&Sources {
            locked_hint: Some(true),
            ..Sources::default()
        }));
        // Each other source alone vetoes an unlocked logind.
        assert!(locked(&Sources {
            lock_signal: true,
            ..open
        }));
        assert!(locked(&Sources {
            notifier: Some(true),
            ..open
        }));
        assert!(locked(&Sources {
            shell: ShellFile::Locked,
            ..open
        }));
        assert!(locked(&Sources {
            shell: ShellFile::Unreadable,
            ..open
        }));
        // And what cannot veto does not: an absent or "unlocked" Shell file,
        // a notifier that says unlocked or nothing.
        assert!(!locked(&Sources {
            shell: ShellFile::Absent,
            notifier: None,
            ..open
        }));
        assert!(!locked(&Sources {
            shell: ShellFile::Unlocked,
            notifier: Some(false),
            ..open
        }));
    }

    #[test]
    fn the_shell_file_is_no_opinion_when_absent_and_locked_when_unreadable() {
        let dir = std::env::temp_dir().join(format!("rime-input-shellfile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let f = dir.join("lock.json");
        let _ = std::fs::remove_file(&f);
        assert_eq!(read_shell_file(&f), ShellFile::Absent);
        for (text, want) in [
            (r#"{"locked":true}"#, ShellFile::Locked),
            (r#"{"locked":false,"at":1}"#, ShellFile::Unlocked),
            (r#"{"locked":"no"}"#, ShellFile::Unreadable),
            ("", ShellFile::Unreadable),
            ("{", ShellFile::Unreadable),
        ] {
            std::fs::write(&f, text).expect("write");
            assert_eq!(read_shell_file(&f), want, "{text}");
        }
        // A directory where the file should be cannot be read: locked.
        std::fs::remove_file(&f).expect("rm");
        std::fs::create_dir(&f).expect("mkdir");
        assert_eq!(read_shell_file(&f), ShellFile::Unreadable);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_batch_is_validated_whole_against_the_contracts_limits() {
        let ok = br#"{"seq":1,"ev":[{"t":"m","dx":12.5,"dy":-3},{"t":"b","b":"left","p":true},
            {"t":"s","dy":-40,"src":"wheel"},{"t":"k","code":30,"p":true},{"t":"tap","code":46,"mods":["ctrl","ctrl"]},
            {"t":"text","s":"a\r\nb\rc\td"},{"t":"hold"}]}"#;
        let (seq, ev) = parse_batch(ok).expect("valid");
        assert_eq!(seq, 1);
        assert_eq!(
            ev[2],
            Ev::Scroll {
                dx: 0.0,
                dy: -40.0,
                src: Src::Wheel,
                stop: false
            }
        );
        assert_eq!(
            ev[4],
            Ev::Tap {
                code: 46,
                mods: vec![29]
            }
        );
        assert_eq!(ev[5], Ev::Text("a\nb\nc\td".into()));
        let too_many =
            json!({"seq": 1, "ev": vec![json!({"t": "hold"}); MAX_EVENTS + 1]}).to_string();
        let long_text =
            json!({"seq": 1, "ev": [{"t": "text", "s": "x".repeat(MAX_TEXT_CHARS + 1)}]})
                .to_string();
        let fat = json!({"seq": 1, "ev": [{"t": "text", "s": "é".repeat(900)}], "pad": "y".repeat(MAX_FRAME_BYTES)}).to_string();
        for bad in [
            r#"{"ev":[]}"#.to_string(),
            r#"{"seq":1}"#.to_string(),
            r#"{"seq":-1,"ev":[]}"#.to_string(),
            r#"[1]"#.to_string(),
            too_many,
            long_text,
            fat,
            r#"{"seq":1,"ev":[{"t":"m","dx":2000.5,"dy":0}]}"#.to_string(),
            r#"{"seq":1,"ev":[{"t":"s","dx":0,"dy":"1"}]}"#.to_string(),
            r#"{"seq":1,"ev":[{"t":"b","b":"back","p":true}]}"#.to_string(),
            r#"{"seq":1,"ev":[{"t":"b","b":"left"}]}"#.to_string(),
            r#"{"seq":1,"ev":[{"t":"k","code":116,"p":true}]}"#.to_string(),
            r#"{"seq":1,"ev":[{"t":"k","code":205,"p":true}]}"#.to_string(),
            r#"{"seq":1,"ev":[{"t":"k","code":0,"p":true}]}"#.to_string(),
            r#"{"seq":1,"ev":[{"t":"k","code":272,"p":true}]}"#.to_string(),
            r#"{"seq":1,"ev":[{"t":"tap","code":30,"mods":["hyper"]}]}"#.to_string(),
            r#"{"seq":1,"ev":[{"t":"text","s":"a\u0007b"}]}"#.to_string(),
            r#"{"seq":1,"ev":[{"t":"text","s":"a\u001b[2~"}]}"#.to_string(),
            r#"{"seq":1,"ev":[{"t":"hold"},{"t":"warp","x":0}]}"#.to_string(),
        ] {
            assert!(
                parse_batch(bad.as_bytes()).is_err(),
                "accepted {}",
                &bad[..bad.len().min(80)]
            );
        }
    }

    #[test]
    fn events_reach_the_backend_and_what_is_held_is_tracked_with_its_modifiers() {
        let mut r = rig("apply");
        assert_eq!(r.frame(0, 1, json!([{"t":"m","dx":3,"dy":-2},{"t":"k","code":42,"p":true},{"t":"k","code":30,"p":true}])), Step::Go);
        assert_eq!(
            r.fake.take(),
            ["motion 3 -2", "key 42 true", "mods 0x1 0x0", "key 30 true"]
        );
        assert_eq!(r.session.held().keys, BTreeSet::from([30, 42]));
        // Held before it was pressed: a crash right after the press finds it.
        assert_eq!(
            read_held(&r.dir.join("input-held.json"))
                .expect("recorded")
                .keys,
            BTreeSet::from([30, 42])
        );
        r.frame(10, 2, json!([{"t":"k","code":30,"p":false},{"t":"k","code":42,"p":false},{"t":"k","code":42,"p":false}]));
        assert_eq!(
            r.fake.take(),
            ["key 30 false", "key 42 false", "mods 0x0 0x0"]
        );
        assert!(read_held(&r.dir.join("input-held.json")).is_none());
        // A tap holds its modifiers around the key and lets go of only those.
        r.frame(20, 3, json!([{"t":"k","code":29,"p":true}]));
        r.fake.take();
        r.frame(
            30,
            4,
            json!([{"t":"tap","code":46,"mods":["ctrl","shift"]}]),
        );
        assert_eq!(
            r.fake.take(),
            [
                "key 42 true",
                "mods 0x5 0x0",
                "key 46 true",
                "key 46 false",
                "key 42 false",
                "mods 0x4 0x0"
            ]
        );
        assert_eq!(r.session.held().keys, BTreeSet::from([29]));
        // A frame sent twice is applied once.
        assert_eq!(r.frame(40, 4, json!([{"t":"m","dx":1,"dy":1}])), Step::Go);
        assert!(r.fake.take().is_empty());
    }

    #[test]
    fn a_lock_releases_everything_says_paused_and_nothing_gets_through_until_unlocked() {
        let mut r = rig("lock");
        r.frame(
            0,
            1,
            json!([{"t":"k","code":30,"p":true},{"t":"b","b":"left","p":true}]),
        );
        r.fake.take();
        // Locked with nothing from the phone: the next tick lets go, so a
        // held key cannot auto-repeat into the password field.
        r.lock(Some(true), false);
        assert_eq!(r.session.tick(r.at(20)), Step::Go);
        assert_eq!(
            r.fake.take(),
            ["key 30 false", "mods 0x0 0x0", "button 0x110 false", "sync"]
        );
        assert_eq!(
            r.said(),
            [json!({"event": "input_paused", "why": "locked"})]
        );
        assert!(r.session.held().is_empty() && r.session.paused());
        // Everything while locked is dropped, not queued.
        r.frame(
            40,
            2,
            json!([{"t":"k","code":31,"p":true},{"t":"text","s":"hunter2"}]),
        );
        r.session.tick(r.at(60));
        assert!(r.fake.take().is_empty());
        assert!(r.said().is_empty());
        r.lock(Some(false), false);
        r.frame(80, 3, json!([{"t":"m","dx":1,"dy":0}]));
        assert_eq!(r.said(), [json!({"event": "input_resumed"})]);
        assert_eq!(r.fake.take(), ["motion 1 0"]);
    }

    #[test]
    fn a_lock_in_the_middle_of_a_batch_or_a_text_stops_it_there() {
        let mut r = rig("midlock");
        // The compositor's notifier says locked after the first event: the
        // gate is read before every event, not once per frame.
        let c = Arc::clone(&r.fake.compositor);
        let fake = r.fake.clone();
        r.session.backend = Box::new(Hook {
            inner: fake,
            after: 1,
            n: 0,
            then: Box::new(move || *c.lock().expect("c") = Some(true)),
        });
        r.frame(
            0,
            1,
            json!([{"t":"m","dx":1,"dy":0},{"t":"m","dx":2,"dy":0},{"t":"text","s":"secret"}]),
        );
        let log = r.fake.take();
        assert_eq!(
            log.first().map(String::as_str),
            Some("motion 1 0"),
            "{log:?}"
        );
        assert!(
            !log.iter()
                .any(|l| l == "motion 2 0" || l.starts_with("text")),
            "{log:?}"
        );
        assert_eq!(
            r.said(),
            [json!({"event": "input_paused", "why": "locked"})]
        );
    }

    /// A backend that runs `then` after its `after`-th call.
    struct Hook {
        inner: Fake,
        after: usize,
        n: usize,
        then: Box<dyn Fn() + Send>,
    }

    impl Hook {
        fn count(&mut self) {
            self.n += 1;
            if self.n == self.after {
                (self.then)();
            }
        }
    }

    impl Backend for Hook {
        fn motion(&mut self, dx: f64, dy: f64) -> std::io::Result<()> {
            self.count();
            self.inner.motion(dx, dy)
        }
        fn button(&mut self, c: u32, p: bool) -> std::io::Result<()> {
            self.count();
            self.inner.button(c, p)
        }
        fn scroll(&mut self, dx: f64, dy: f64, s: Src, st: bool) -> std::io::Result<()> {
            self.count();
            self.inner.scroll(dx, dy, s, st)
        }
        fn key(&mut self, c: u32, p: bool) -> std::io::Result<()> {
            self.count();
            self.inner.key(c, p)
        }
        fn modifiers(&mut self, d: u32, l: u32) -> std::io::Result<()> {
            self.inner.modifiers(d, l)
        }
        fn text_keymap(&mut self, c: &[char]) -> std::io::Result<()> {
            self.inner.text_keymap(c)
        }
        fn text_key(&mut self, c: u32, p: bool) -> std::io::Result<()> {
            self.count();
            self.inner.text_key(c, p)
        }
        fn sync(&mut self) -> std::io::Result<()> {
            self.inner.sync()
        }
        fn compositor_locked(&self) -> Option<bool> {
            self.inner.compositor_locked()
        }
        fn gone(&self) -> Option<String> {
            None
        }
    }

    #[test]
    fn the_grant_is_read_on_every_frame_and_every_tick() {
        let mut r = rig("grant");
        r.frame(0, 1, json!([{"t":"k","code":30,"p":true}]));
        r.fake.take();
        r.grant.store(false, Ordering::SeqCst);
        assert!(matches!(
            r.frame(10, 2, json!([{"t":"m","dx":5,"dy":5}])),
            Step::End(Why::NotAllowed, _)
        ));
        assert!(
            r.fake.take().is_empty(),
            "a frame after the grant was lowered was applied"
        );
        assert!(matches!(
            r.session.tick(r.at(20)),
            Step::End(Why::NotAllowed, _)
        ));
        r.session.end(Some((Why::NotAllowed, "x".into())));
        assert_eq!(r.fake.take(), ["key 30 false", "mods 0x0 0x0", "sync"]);
        assert_eq!(r.said()[0]["event"], "input_closed");
    }

    #[test]
    fn nothing_from_the_phone_for_the_lease_while_holding_releases_everything() {
        let mut r = rig("lease");
        r.frame(0, 1, json!([{"t":"b","b":"left","p":true}]));
        r.fake.take();
        assert_eq!(r.session.tick_every(), HELD_TICK);
        // `hold` keeps it.
        r.frame(500, 2, json!([{"t":"hold"}]));
        r.session.tick(r.at(1_000));
        assert!(r.fake.take().is_empty());
        r.session.tick(r.at(1_251));
        assert_eq!(r.fake.take(), ["button 0x110 false", "sync"]);
        assert_eq!(
            r.said(),
            [json!({"event": "input_released", "why": "lease"})]
        );
        assert_eq!(r.session.tick_every(), IDLE_TICK);
        // Nothing held: no lease to run out.
        r.session.tick(r.at(5_000));
        assert!(r.fake.take().is_empty() && r.said().is_empty());
    }

    #[test]
    fn ending_the_channel_releases_everything_held() {
        let mut r = rig("end");
        r.frame(
            0,
            1,
            json!([{"t":"k","code":125,"p":true},{"t":"b","b":"right","p":true}]),
        );
        r.fake.take();
        r.session.end(None);
        assert_eq!(
            r.fake.take(),
            [
                "key 125 false",
                "mods 0x0 0x0",
                "button 0x111 false",
                "sync"
            ]
        );
        assert!(
            r.said().is_empty(),
            "the phone closed it itself: nothing to explain"
        );
        assert!(read_held(&r.dir.join("input-held.json")).is_none());
    }

    #[test]
    fn too_many_frames_or_too_much_text_in_a_second_ends_the_channel() {
        let mut r = rig("rate");
        for i in 0..MAX_FRAMES_PER_SEC as u64 {
            assert_eq!(r.frame(i, i + 1, json!([{"t":"hold"}])), Step::Go);
        }
        assert!(matches!(
            r.frame(999, 9_999, json!([{"t":"hold"}])),
            Step::End(Why::Refused, _)
        ));
        let mut r = rig("textrate");
        for i in 0..4 {
            assert_eq!(
                r.frame(i, i + 1, json!([{"t":"text","s":"a".repeat(1_000)}])),
                Step::Go
            );
        }
        assert!(matches!(
            r.frame(5, 9, json!([{"t":"text","s":"b"}])),
            Step::End(Why::Refused, _)
        ));
        // A new second starts afresh.
        let mut r = rig("textrate2");
        for i in 0..4 {
            r.frame(i, i + 1, json!([{"t":"text","s":"a".repeat(1_000)}]));
        }
        assert_eq!(r.frame(1_000, 9, json!([{"t":"text","s":"b"}])), Step::Go);
    }

    #[test]
    fn text_with_more_distinct_characters_than_a_keymap_holds_is_typed_in_runs() {
        let mut r = rig("text");
        let s: String = (0..250u32)
            .map(|i| char::from_u32(0x4e00 + i).expect("char"))
            .collect();
        r.frame(0, 1, json!([{"t":"text","s":s}]));
        let log = r.fake.take();
        let maps: Vec<&String> = log.iter().filter(|l| l.starts_with("keymap")).collect();
        assert_eq!(maps, ["keymap 200", "keymap 50"]);
        assert_eq!(
            log.iter()
                .filter(|l| l.ends_with("true") && l.starts_with("text"))
                .count(),
            250
        );
        // Each character is pressed then released, never left down.
        assert!(r.session.held().is_empty());
        r.frame(10, 2, json!([{"t":"text","s":"aba"}]));
        assert_eq!(
            r.fake.take(),
            [
                "keymap 2",
                "text 1 true",
                "text 1 false",
                "text 2 true",
                "text 2 false",
                "text 1 true",
                "text 1 false"
            ]
        );
    }

    #[test]
    fn recovery_releases_keys_and_clicks_buttons_only_when_unlocked() {
        let held = Held {
            keys: BTreeSet::from([30]),
            buttons: BTreeSet::from([0x110]),
            text_keys: BTreeSet::from([3]),
        };
        let mut f = Fake::default();
        let left = recover(&mut f, &held, false);
        assert_eq!(
            f.take(),
            ["key 30 false", "key 3 false", "mods 0x0 0x0", "sync"]
        );
        assert_eq!(left.buttons, BTreeSet::from([0x110]));
        let left = recover(&mut f, &left, true);
        assert_eq!(
            f.take(),
            ["button 0x110 true", "button 0x110 false", "sync"]
        );
        assert!(left.is_empty());
    }

    #[test]
    fn only_a_phones_own_key_opens_the_channel() {
        // Neither a companion link nor another computer, at the allow list
        // and again where the channel is made.
        let req = br#"{"cmd":"input","v":1}"#;
        assert!(is_channel(req));
        assert!(!crate::companion::channel_allowed(Scope::Companion, req));
        assert!(!crate::companion::channel_allowed(Scope::Peer, req));
        let (state, dir) = crate::serve::tests::a_state("input-scope");
        for scope in [Scope::Companion, Scope::Peer] {
            let err = open(&state, "pixel", scope, req).expect_err("opened");
            let v: Value = serde_json::from_slice(&err).expect("json");
            assert_eq!(v["kind"], "not_allowed", "{scope:?}: {v}");
        }
        // And a phone with no grant, which every phone starts as.
        let id = {
            let mut store = state.devices().expect("store");
            store
                .pair(&rime_remote_core::b64_encode(&[7; 32]), "Pixel", 1, false)
                .expect("pair")
                .id
        };
        let v: Value =
            serde_json::from_slice(&open(&state, &id, Scope::Full, req).expect_err("opened"))
                .expect("json");
        assert_eq!(v["kind"], "not_allowed", "{v}");
        assert!(
            v["message"]
                .as_str()
                .unwrap_or_default()
                .contains("rime remote devices grant Pixel input on"),
            "{v}"
        );
        let v: Value = serde_json::from_slice(
            &open(&state, &id, Scope::Full, br#"{"cmd":"input","v":2}"#).expect_err("opened"),
        )
        .expect("json");
        assert_eq!(v["kind"], "bad_request", "{v}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn layouts_become_include_keymaps_and_nothing_else_gets_into_one() {
        use wayland::Layout;
        let de = Layout::from_names("de", "nodeadkeys").keymap();
        assert!(
            de.contains(r#"include "pc+de(nodeadkeys)+inet(evdev)""#),
            "{de}"
        );
        let two = Layout::from_names("us,ru", ",phonetic").keymap();
        assert!(
            two.contains(r#"include "pc+us+ru(phonetic):2+inet(evdev)""#),
            "{two}"
        );
        for bad in [
            ("us\"};", ""),
            ("us", "x) }; xkb_symbols { include \"evil"),
            ("", ""),
            ("a,b,c,d,e", ""),
        ] {
            assert_eq!(
                Layout::from_names(bad.0, bad.1),
                Layout {
                    layouts: vec!["us".into()],
                    variants: vec![]
                },
                "{bad:?}"
            );
        }
        let t = wayland::text_keymap(&['é', '\n', '"']);
        assert!(
            t.contains("maximum = 12;")
                && t.contains("<I9> = 9;")
                && t.contains("key <I9> { [ U00E9 ] };"),
            "{t}"
        );
        assert!(
            t.contains("key <I10> { [ Return ] };") && t.contains("key <I11> { [ U0022 ] };"),
            "{t}"
        );
    }

    /// A scripted compositor on the other end of a socket pair: answers the
    /// registry with `globals`, every `sync` with `done`, and records every
    /// request as (object, opcode, body).
    fn compositor(
        globals: &'static [&'static str],
    ) -> (UnixStream, Arc<Mutex<Vec<(u32, u16, Vec<u8>)>>>) {
        let (ours, theirs) = UnixStream::pair().expect("pair");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            let mut sock = theirs;
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = match sock.read(&mut chunk) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                buf.extend_from_slice(&chunk[..n]);
                while buf.len() >= 8 {
                    let obj = u32::from_le_bytes(buf[0..4].try_into().expect("4"));
                    let so = u32::from_le_bytes(buf[4..8].try_into().expect("4"));
                    let size = (so >> 16) as usize;
                    if buf.len() < size {
                        break;
                    }
                    let body: Vec<u8> = buf.drain(..size).skip(8).collect();
                    let op = (so & 0xffff) as u16;
                    let mut reply = Vec::new();
                    if obj == 1 && op == 1 {
                        for (i, g) in globals.iter().enumerate() {
                            let mut a = Vec::new();
                            a.extend_from_slice(&(i as u32 + 1).to_le_bytes());
                            let len = g.len() + 1;
                            a.extend_from_slice(&(len as u32).to_le_bytes());
                            a.extend_from_slice(g.as_bytes());
                            a.push(0);
                            while a.len() % 4 != 0 {
                                a.push(0);
                            }
                            a.extend_from_slice(&2u32.to_le_bytes());
                            reply.extend(wayland::message(2, 0, &a));
                        }
                    } else if obj == 1 && op == 0 {
                        let cb = u32::from_le_bytes(body[0..4].try_into().expect("4"));
                        reply.extend(wayland::message(cb, 0, &0u32.to_le_bytes()));
                    }
                    log.lock().expect("log").push((obj, op, body));
                    if !reply.is_empty() && sock.write_all(&reply).is_err() {
                        return;
                    }
                }
            }
        });
        (ours, seen)
    }

    const ALL: &[&str] = &[
        "wl_seat",
        "zwlr_virtual_pointer_manager_v1",
        "zwp_virtual_keyboard_manager_v1",
    ];

    #[test]
    fn the_devices_are_made_then_warmed_up_with_one_motion_there_and_back() {
        let (sock, seen) = compositor(ALL);
        let w =
            wayland::Wayland::over(sock, &wayland::Layout::from_names("us", "")).expect("connect");
        let log = seen.lock().expect("log").clone();
        let shape: Vec<(u32, u16)> = log.iter().map(|(o, op, _)| (*o, *op)).collect();
        // get_registry, sync (callback 3), bind ×3 (seat 4, pointers 5,
        // keyboards 6), pointer 7, keyboard 8, text keyboard 9, keymap on
        // 8, sync, then motion+frame, motion+frame, sync.
        assert_eq!(
            shape,
            [
                (1, 1),
                (1, 0),
                (2, 0),
                (2, 0),
                (2, 0),
                (5, 0),
                (6, 0),
                (6, 0),
                (8, 0),
                (1, 0),
                (7, 0),
                (7, 4),
                (7, 0),
                (7, 4),
                (1, 0)
            ],
        );
        let dx = |b: &Vec<u8>| i32::from_le_bytes(b[4..8].try_into().expect("4"));
        assert_eq!(
            (dx(&log[10].2), dx(&log[12].2)),
            (256, -256),
            "+1 then -1 in 24.8 fixed point"
        );
        assert!(w.gone().is_none());
    }

    #[test]
    fn a_text_keymap_is_taken_before_the_first_key_of_it() {
        let (sock, seen) = compositor(ALL);
        let mut w =
            wayland::Wayland::over(sock, &wayland::Layout::from_names("us", "")).expect("connect");
        seen.lock().expect("log").clear();
        w.text_keymap(&['é']).expect("keymap");
        w.text_key(1, true).expect("key");
        w.text_key(1, false).expect("key");
        w.sync().expect("sync");
        let shape: Vec<(u32, u16)> = seen
            .lock()
            .expect("log")
            .iter()
            .map(|(o, op, _)| (*o, *op))
            .collect();
        // keymap on the text keyboard (9), its modifiers zeroed, a sync the
        // compositor answered, and only then the key.
        assert_eq!(shape, [(9, 0), (9, 2), (1, 0), (9, 1), (9, 1), (1, 0)]);
        // The same keymap again is not uploaded again.
        seen.lock().expect("log").clear();
        w.text_keymap(&['é']).expect("keymap");
        let shape: Vec<(u32, u16)> = seen
            .lock()
            .expect("log")
            .iter()
            .map(|(o, op, _)| (*o, *op))
            .collect();
        assert_eq!(shape, [(1, 0)]);
    }

    #[test]
    fn a_compositor_without_virtual_input_is_refused() {
        let (sock, _) = compositor(&["wl_seat", "wl_compositor"]);
        let err = wayland::Wayland::over(sock, &wayland::Layout::from_names("us", ""))
            .err()
            .expect("refused");
        assert!(err.contains("does not take virtual input"), "{err}");
    }

    // ── a real compositor ────────────────────────────────────────────────

    /// Runs inside foot: every input chunk (keys, SGR mouse reports) as a
    /// Python bytes literal per line.
    const RECV_PY: &str = r#"import os, sys, termios, tty, select, time
log = open(sys.argv[1], "w", buffering=1)
fd = sys.stdin.fileno()
tty.setraw(fd)
sys.stdout.write("\x1b[?1003h\x1b[?1006h"); sys.stdout.flush()
log.write("READY\n")
end = time.monotonic() + 120
while time.monotonic() < end:
    r, _, _ = select.select([fd], [], [], 0.5)
    if r:
        log.write(repr(os.read(fd, 4096)) + "\n")
"#;

    /// A test-only lock screen (ext-session-lock, no PAM) that logs what it
    /// receives. The same protocol every real Rime lock screen uses.
    const LOCK_QML: &str = r#"import QtQuick
import Quickshell
import Quickshell.Wayland
ShellRoot {
    WlSessionLock {
        id: lock
        locked: true
        onSecureStateChanged: console.warn("LOCKTEST secure=" + lock.secure)
        WlSessionLockSurface {
            color: "black"
            Item {
                anchors.fill: parent
                focus: true
                Keys.onPressed: (e) => console.warn("LOCKTEST key text=" + JSON.stringify(e.text))
                MouseArea {
                    anchors.fill: parent
                    hoverEnabled: true
                    onPressed: console.warn("LOCKTEST press")
                }
            }
        }
    }
}
"#;

    fn need(bin: &str) {
        let found = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
            .unwrap_or(false);
        assert!(
            found,
            "{bin} is not installed: this test needs labwc, foot, quickshell and python3"
        );
    }

    fn wait_for(what: &str, within: Duration, log: &Path, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + within;
        while !done() {
            if Instant::now() >= deadline {
                let seen = std::fs::read_to_string(log).unwrap_or_default();
                panic!("never saw {what}; {} says:\n{seen}", log.display());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Children stopped by their own PIDs, and the private runtime dir
    /// removed, however the test ends.
    struct Bed {
        rt: PathBuf,
        kids: Vec<std::process::Child>,
    }

    impl Drop for Bed {
        fn drop(&mut self) {
            for k in self.kids.iter_mut().rev() {
                let _ = k.kill();
                let _ = k.wait();
            }
            let _ = std::fs::remove_dir_all(&self.rt);
        }
    }

    impl Bed {
        /// A client of the private compositor, with nothing of this
        /// session's environment.
        fn client(&self, program: &str) -> std::process::Command {
            let mut c = std::process::Command::new(program);
            c.env_clear()
                .env("HOME", self.rt.join("home"))
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("XDG_RUNTIME_DIR", &self.rt)
                .env("WAYLAND_DISPLAY", "wayland-0")
                .env("LANG", "C.UTF-8");
            c
        }
    }

    /// Keys, motion, clicks, scrolls and Unicode text reach a real window
    /// through a headless labwc started here, in its own runtime dir (never
    /// this machine's desktop). Then a real ext-session-lock screen: with
    /// the gate saying locked nothing reaches it, and the negative control,
    /// the gate saying unlocked while the compositor is locked, shows that
    /// injected input DOES reach a lock screen, so the gate is what stops it.
    ///
    /// Needs labwc, foot, quickshell and python3, and fails without them:
    /// `--ignored --exact input::tests::a_real_compositor_gets_the_input_and_a_lock_screen_gets_none_of_it`.
    #[test]
    #[ignore = "starts a headless labwc, foot and quickshell"]
    fn a_real_compositor_gets_the_input_and_a_lock_screen_gets_none_of_it() {
        for b in ["labwc", "foot", "quickshell", "python3"] {
            need(b);
        }
        // Short: a socket path is limited to 108 bytes.
        let rt = PathBuf::from(format!("/tmp/rime-in-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&rt);
        std::fs::create_dir_all(rt.join("home/.config/labwc")).expect("mkdir");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        std::fs::write(rt.join("home/.config/labwc/autostart"), "").expect("autostart");
        std::fs::write(
            rt.join("home/.config/labwc/rc.xml"),
            "<?xml version=\"1.0\"?>\n<labwc_config></labwc_config>\n",
        )
        .expect("rc");
        std::fs::write(rt.join("recv.py"), RECV_PY).expect("recv");
        std::fs::create_dir_all(rt.join("lockqs")).expect("mkdir");
        std::fs::write(rt.join("lockqs/shell.qml"), LOCK_QML).expect("qml");
        let mut bed = Bed {
            rt: rt.clone(),
            kids: Vec::new(),
        };

        let labwc = std::process::Command::new("labwc")
            .env_clear()
            .env("HOME", rt.join("home"))
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("XDG_RUNTIME_DIR", &rt)
            .env("WLR_BACKENDS", "headless")
            .env("WLR_RENDERER", "pixman")
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::fs::File::create(rt.join("labwc.log")).expect("log"))
            .spawn()
            .expect("labwc");
        bed.kids.push(labwc);
        let socket = rt.join("wayland-0");
        wait_for(
            "labwc's socket",
            Duration::from_secs(15),
            &rt.join("labwc.log"),
            || UnixStream::connect(&socket).is_ok(),
        );

        let recv_log = rt.join("recv.log");
        let foot = bed
            .client("foot")
            .args(["-o", "font=monospace:size=8", "python3"])
            .arg(rt.join("recv.py"))
            .arg(&recv_log)
            .stdout(std::process::Stdio::null())
            .stderr(std::fs::File::create(rt.join("foot.log")).expect("log"))
            .spawn()
            .expect("foot");
        bed.kids.push(foot);
        let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
        wait_for(
            "the receiver",
            Duration::from_secs(15),
            &rt.join("foot.log"),
            || read(&recv_log).contains("READY"),
        );
        std::thread::sleep(Duration::from_millis(300));

        let hint = Arc::new(Mutex::new((Some(false), false)));
        let said = Arc::new(Mutex::new(Vec::new()));
        let (h, sd) = (Arc::clone(&hint), Arc::clone(&said));
        let backend = wayland::Wayland::connect(&socket, &wayland::Layout::from_names("us", ""))
            .expect("virtual devices");
        assert_eq!(
            backend.compositor_locked(),
            None,
            "labwc has no lock notifier"
        );
        let mut session = Session::new(
            Box::new(backend),
            Gate {
                logind: Box::new(move || *h.lock().expect("h")),
                shell: None,
            },
            Box::new(|| true),
            Box::new(|| Ok(())),
            Box::new(move |v| sd.lock().expect("s").push(v)),
            Some(rt.join("input-held.json")),
            Instant::now(),
        );
        let mut seq = 0u64;
        let mut send = |session: &mut Session, ev: Value| {
            seq += 1;
            let step = session.frame(
                json!({"seq": seq, "ev": ev}).to_string().as_bytes(),
                Instant::now(),
            );
            assert_eq!(step, Step::Go);
            std::thread::sleep(Duration::from_millis(30));
        };

        // Into the window: over it, keys (with Shift), text, a click, a wheel.
        for _ in 0..4 {
            send(&mut session, json!([{"t": "m", "dx": 60, "dy": 40}]));
        }
        send(
            &mut session,
            json!([{"t": "tap", "code": 35}, {"t": "tap", "code": 23, "mods": ["shift"]}]),
        );
        send(&mut session, json!([{"t": "text", "s": "é✓"}]));
        send(&mut session, json!([{"t": "b", "b": "left", "p": true}]));
        send(&mut session, json!([{"t": "b", "b": "left", "p": false}]));
        send(
            &mut session,
            json!([{"t": "s", "dx": 0, "dy": 15, "src": "wheel"}]),
        );
        // Motion with no button held, now that the pointer is over the
        // window: a report foot sends only for a real move.
        for _ in 0..3 {
            send(&mut session, json!([{"t": "m", "dx": 12, "dy": 9}]));
        }
        // The receiver logs each read as a bytes literal; join them.
        let typed = |p: &Path| -> String {
            read(p)
                .lines()
                .filter_map(|l| l.strip_prefix("b'").and_then(|l| l.strip_suffix('\'')))
                .collect()
        };
        let want = [
            "hI",
            r"\xc3\xa9\xe2\x9c\x93",
            r"\x1b[<0;",
            r"\x1b[<65;",
            r"\x1b[<35;",
        ];
        wait_for(
            "every kind of input in the window",
            Duration::from_secs(5),
            &recv_log,
            || {
                let got = typed(&recv_log);
                want.iter().all(|w| got.contains(w))
            },
        );

        // Locking, as logind says: a key held at that moment is let go and
        // the phone is told. The gate turns before the lock screen exists,
        // as the Shell's file makes it do on a real machine.
        send(&mut session, json!([{"t": "k", "code": 31, "p": true}]));
        *hint.lock().expect("h") = (Some(true), false);
        assert_eq!(session.tick(Instant::now()), Step::Go);
        assert!(said
            .lock()
            .expect("s")
            .iter()
            .any(|v| v["event"] == "input_paused"));
        assert!(session.held().is_empty());

        // A real lock screen.
        let lock_log = rt.join("lock.log");
        let lock = bed
            .client("quickshell")
            .env("QT_QPA_PLATFORM", "wayland")
            // pixman labwc offers no GPU buffers: draw in software.
            .env("QT_QUICK_BACKEND", "software")
            .arg("-p")
            .arg(rt.join("lockqs/shell.qml"))
            // Quickshell writes its log (the QML warnings) to stdout.
            .stdout(std::fs::File::create(&lock_log).expect("log"))
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("quickshell");
        bed.kids.push(lock);
        wait_for(
            "the lock screen",
            Duration::from_secs(20),
            &lock_log,
            || read(&lock_log).contains("LOCKTEST secure=true"),
        );
        std::thread::sleep(Duration::from_millis(300));
        let before = read(&recv_log);

        // Locked: nothing gets through.
        send(
            &mut session,
            json!([{"t": "tap", "code": 30}, {"t": "text", "s": "secret"}]),
        );
        send(
            &mut session,
            json!([{"t": "b", "b": "left", "p": true}, {"t": "b", "b": "left", "p": false}]),
        );
        std::thread::sleep(Duration::from_millis(500));
        let locked = read(&lock_log);
        assert!(
            !locked.contains("LOCKTEST key") && !locked.contains("LOCKTEST press"),
            "input reached the lock screen while the gate said locked:\n{locked}"
        );

        // The negative control: the gate wrongly saying unlocked. The same
        // events now reach the lock screen, so it is the gate that kept them
        // out above.
        *hint.lock().expect("h") = (Some(false), false);
        send(&mut session, json!([{"t": "tap", "code": 30}]));
        send(
            &mut session,
            json!([{"t": "b", "b": "left", "p": true}, {"t": "b", "b": "left", "p": false}]),
        );
        wait_for(
            "input on the lock screen with the gate open",
            Duration::from_secs(5),
            &lock_log,
            || {
                let l = read(&lock_log);
                l.contains("LOCKTEST key text=\"a\"") && l.contains("LOCKTEST press")
            },
        );
        // And none of it reached the window behind the lock.
        assert_eq!(
            read(&recv_log),
            before,
            "input reached a window behind the lock screen"
        );
        session.end(None);
        drop(bed);
    }
}
