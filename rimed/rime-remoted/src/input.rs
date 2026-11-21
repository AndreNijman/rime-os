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
//! every other source can only veto. On a compositor with a lock notifier a
//! missing Shell file is no opinion either way; on one without (labwc) a
//! stale `LockedHint` false could not be told apart from a true one, so
//! there the Shell's file must be present and say unlocked. The file counts
//! only when this user alone could have written it ([`read_shell_file`]).
//! The gate is read before every event, and a session thread
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
/// New text keymaps in one second. Each is compiled by the compositor, so
/// text that needs more than this (every word in characters the keymap has
/// not seen) ends the channel. Typing reuses the keymap it has.
pub const MAX_KEYMAPS_PER_SEC: u32 = 20;

/// The file the Shell watches to show "in use as a touchpad", in the runtime
/// directory's `rime-remote` (§2.16).
pub const STATUS_FILE: &str = "input.json";

/// The most characters of a phone's name the status file carries. The name
/// is the phone's own choice, so it is bounded where it is written.
pub const STATUS_NAME_MAX: usize = 64;

/// How long a phone stopped at the computer is refused a new channel, unless
/// a person raises its grant again before then.
pub const COOL_DOWN: Duration = Duration::from_secs(30);

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
    /// No file. On a compositor with a lock notifier, no opinion; on one
    /// without (labwc), locked: logind's `LockedHint` alone cannot be told
    /// apart from a stale one there.
    #[default]
    Absent,
    Locked,
    Unlocked,
    /// There, and not a file this user alone could have written saying
    /// `{"locked": <bool>}`: counted as locked.
    Unreadable,
}

/// The most the Shell's file may hold.
const SHELL_FILE_MAX: u64 = 4096;

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
    /// Whether the compositor offers a lock notifier at all. Without one, the
    /// Shell's file is required: absent counts as locked.
    pub notifier_offered: bool,
    /// The Shell's file.
    pub shell: ShellFile,
}

/// Whether input must stop: the union of every source, and unknown is
/// locked. Only logind saying `LockedHint` false, with no other source
/// objecting, lets anything through; on a compositor with no lock notifier
/// the Shell's file must also be there and say unlocked.
pub fn locked(s: &Sources) -> bool {
    crate::share::counts_as_locked(s.locked_hint)
        || s.lock_signal
        || s.notifier == Some(true)
        || matches!(s.shell, ShellFile::Locked | ShellFile::Unreadable)
        || (s.shell == ShellFile::Absent && !s.notifier_offered)
}

/// Whether a Shell lock file with these properties could only have been
/// written by `me`: a regular file, owned by `me`, with no group or other
/// write bit, of at most [`SHELL_FILE_MAX`] bytes.
fn shell_file_trusted(is_file: bool, owner: u32, me: u32, mode: u32, len: u64) -> bool {
    is_file && owner == me && mode & 0o022 == 0 && len <= SHELL_FILE_MAX
}

/// Read the Shell's lock file. Only a regular file (not a link, a FIFO or a
/// directory) owned by this user, writable by nobody else, of at most
/// [`SHELL_FILE_MAX`] bytes, is read at all; anything else there is
/// [`ShellFile::Unreadable`], so locked. Opened without following a link and
/// without blocking, so nothing put there can stall the session thread.
pub fn read_shell_file(path: &Path) -> ShellFile {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path);
    let file = match file {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ShellFile::Absent,
        Err(_) => return ShellFile::Unreadable,
    };
    let Ok(meta) = file.metadata() else {
        return ShellFile::Unreadable;
    };
    // SAFETY: getuid has no failure mode.
    let uid = unsafe { libc::getuid() };
    if !shell_file_trusted(
        meta.file_type().is_file(),
        meta.uid(),
        uid,
        meta.mode(),
        meta.len(),
    ) {
        return ShellFile::Unreadable;
    }
    let mut bytes = Vec::new();
    if std::io::Read::read_to_end(
        &mut std::io::Read::take(&file, SHELL_FILE_MAX + 1),
        &mut bytes,
    )
    .is_err()
    {
        return ShellFile::Unreadable;
    }
    match serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|v| v.get("locked")?.as_bool())
    {
        Some(true) => ShellFile::Locked,
        Some(false) => ShellFile::Unlocked,
        None => ShellFile::Unreadable,
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
    fn sources(&self, notifier: Option<bool>, notifier_offered: bool, shell: ShellFile) -> Sources {
        let (locked_hint, lock_signal) = (self.logind)();
        Sources {
            locked_hint,
            lock_signal,
            notifier,
            notifier_offered,
            shell,
        }
    }

    /// The gate against `backend`: its notifier, and whether it has one.
    fn locked_for(&self, backend: &dyn Backend) -> bool {
        locked(&self.sources(
            backend.compositor_locked(),
            backend.offers_lock_notifier(),
            self.shell_file(),
        ))
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
    /// Make the new pointer take clicks. Sent only once the gate has said
    /// unlocked: even a motion there and back must not reach a lock screen.
    fn warm_up(&mut self) -> std::io::Result<()>;
    /// The compositor's own lock notifier: `Some(true)` locked.
    fn compositor_locked(&self) -> Option<bool>;
    /// Whether the compositor offers a lock notifier at all.
    fn offers_lock_notifier(&self) -> bool;
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
        // A compositor slow to answer: `slow` holds the milliseconds.
        if let Some(ms) = std::fs::read_to_string(dir.join("slow"))
            .ok()
            .and_then(|t| t.trim().parse::<u64>().ok())
        {
            std::thread::sleep(Duration::from_millis(ms.min(10_000)));
        }
        let b = StubBackend {
            dir: dir.to_path_buf(),
        };
        b.write(json!({"a": "open"}))?;
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
    fn warm_up(&mut self) -> std::io::Result<()> {
        self.write(json!({"a": "warm"}))
    }
    fn compositor_locked(&self) -> Option<bool> {
        StubBackend::sources(&self.dir)
            .get("notifier")
            .and_then(Value::as_bool)
    }
    /// A `notifier` that is a bool in `sources.json` (not null, not
    /// missing) stands for a compositor that offers one.
    fn offers_lock_notifier(&self) -> bool {
        StubBackend::sources(&self.dir)
            .get("notifier")
            .is_some_and(Value::is_boolean)
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
    /// Somebody at the computer pressed Stop (`input_stop`).
    StoppedHere = 5,
}

impl Why {
    fn word(self) -> &'static str {
        match self {
            Why::NotAllowed => "not_allowed",
            Why::Unavailable => "unavailable",
            Why::Refused => "refused",
            Why::Stopping => "stopping",
            Why::StoppedHere => "stopped",
        }
    }

    fn from_u8(v: u8) -> Option<Why> {
        [
            Why::NotAllowed,
            Why::Unavailable,
            Why::Refused,
            Why::Stopping,
            Why::StoppedHere,
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

/// What a session did, by kind, for the journal line at its end: counts
/// only, never a key code, a character or a position.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub frames: u64,
    pub motion: u64,
    pub button: u64,
    pub scroll: u64,
    pub key: u64,
    pub tap: u64,
    pub text: u64,
    pub hold: u64,
    /// Events that arrived while locked, and were dropped.
    pub dropped: u64,
}

impl Counts {
    fn add(&mut self, ev: &Ev) {
        let n = match ev {
            Ev::Motion { .. } => &mut self.motion,
            Ev::Button { .. } => &mut self.button,
            Ev::Scroll { .. } => &mut self.scroll,
            Ev::Key { .. } => &mut self.key,
            Ev::Tap { .. } => &mut self.tap,
            Ev::Text(_) => &mut self.text,
            Ev::Hold => &mut self.hold,
        };
        *n += 1;
    }
}

/// The journal line when a phone starts using this computer as a touchpad.
fn audit_open(device: &str, name: &str) -> String {
    format!("rime-remoted: touchpad: {name:?} ({device}) started using this computer as a touchpad and keyboard")
}

/// The journal line when it stops: who, why, for how long, and how many
/// events of each kind. Never what was typed or pressed.
fn audit_close(device: &str, name: &str, why: &str, lasted: Duration, c: &Counts) -> String {
    format!(
        "rime-remoted: touchpad: {name:?} ({device}) stopped ({why}) after {}s; frames={} motion={} \
         button={} scroll={} key={} tap={} text={} hold={} dropped_while_locked={}",
        lasted.as_secs(),
        c.frames,
        c.motion,
        c.button,
        c.scroll,
        c.key,
        c.tap,
        c.text,
        c.hold,
        c.dropped
    )
}

/// Frames, text and text keymaps in the current second.
struct Rate {
    start: Instant,
    frames: u32,
    text: usize,
    keymaps: u32,
}

/// How `chars` is typed when the text keyboard has the keymap `map`: each
/// run with the keymap it needs. A run's keymap is the one before it with
/// the run's new characters added while there is room, so text in
/// characters it already has needs no new one; when it is full of others,
/// a keymap of the run's own characters.
fn text_runs(map: &[char], chars: &[char]) -> Vec<(Vec<char>, std::ops::Range<usize>)> {
    let mut runs = Vec::new();
    let mut cur = map.to_vec();
    let mut i = 0;
    while i < chars.len() {
        let mut next = cur.clone();
        let mut end = i;
        while end < chars.len() {
            if !next.contains(&chars[end]) {
                if next.len() == TEXT_CHUNK {
                    break;
                }
                next.push(chars[end]);
            }
            end += 1;
        }
        if end == i {
            cur.clear();
            continue;
        }
        runs.push((next.clone(), i..end));
        cur = next;
        i = end;
    }
    runs
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
    /// When the tick's checks last ran, by a tick or after a frame.
    last_tick: Instant,
    rate: Rate,
    /// The text keyboard's keymap: key `1 + i` types `text_map[i]`.
    text_map: Vec<char>,
    crash: Option<PathBuf>,
    counts: Counts,
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
            last_tick: now,
            rate: Rate {
                start: now,
                frames: 0,
                text: 0,
                keymaps: 0,
            },
            text_map: Vec::new(),
            crash,
            counts: Counts::default(),
        }
    }

    /// What it has done so far, by kind.
    pub fn counts(&self) -> Counts {
        self.counts
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
        locked(&self.gate.sources(
            self.backend.compositor_locked(),
            self.backend.offers_lock_notifier(),
            self.shell,
        ))
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
    ///
    /// A code is forgotten only once its release was sent, and none is when
    /// the compositor does not confirm having them: what is left stays held
    /// here and in the crash file, for the next channel (or the next start)
    /// to release. The first failure is returned; the channel then ends.
    pub fn release_all(&mut self) -> std::io::Result<()> {
        let before = self.held.clone();
        let had_keys = !self.held.keys.is_empty() || !self.held.text_keys.is_empty();
        let mut failed: Option<std::io::Error> = None;
        for code in before.keys.iter().copied() {
            match self.backend.key(code, false) {
                Ok(()) => {
                    self.held.keys.remove(&code);
                }
                Err(e) => {
                    failed.get_or_insert(e);
                }
            }
        }
        for code in before.text_keys.iter().copied() {
            match self.backend.text_key(code, false) {
                Ok(()) => {
                    self.held.text_keys.remove(&code);
                }
                Err(e) => {
                    failed.get_or_insert(e);
                }
            }
        }
        if had_keys {
            if let Err(e) = self.backend.modifiers(0, self.locked_mask()) {
                failed.get_or_insert(e);
            }
        }
        for code in before.buttons.iter().copied() {
            match self.backend.button(code, false) {
                Ok(()) => {
                    self.held.buttons.remove(&code);
                }
                Err(e) => {
                    failed.get_or_insert(e);
                }
            }
        }
        if let Err(e) = self.backend.sync() {
            // Sent, perhaps, but not known to have arrived.
            self.held = before;
            failed.get_or_insert(e);
        }
        self.record();
        failed.map_or(Ok(()), Err)
    }

    fn locked_mask(&self) -> u32 {
        if self.caps {
            0x2
        } else {
            0
        }
    }

    fn pause(&mut self) -> std::io::Result<()> {
        self.release_all()?;
        if !self.paused {
            self.paused = true;
            (self.out)(json!({"event": "input_paused", "why": "locked"}));
        }
        Ok(())
    }

    fn rate_ok(&mut self, now: Instant, text: usize, keymaps: u32) -> Result<(), String> {
        if now.duration_since(self.rate.start) >= Duration::from_secs(1) {
            self.rate = Rate {
                start: now,
                frames: 0,
                text: 0,
                keymaps: 0,
            };
        }
        self.rate.frames += 1;
        self.rate.text += text;
        self.rate.keymaps += keymaps;
        if self.rate.keymaps > MAX_KEYMAPS_PER_SEC {
            return Err(format!(
                "text needing more than {MAX_KEYMAPS_PER_SEC} new keymaps in a second"
            ));
        }
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

    /// One frame from the phone, then the tick's checks when they are due.
    /// A phone that keeps sending frames (empty ones too) never lets the
    /// session thread's read time out, so without this it could keep a key
    /// held across a lock, or keep typing into Gaming Mode.
    pub fn frame(&mut self, bytes: &[u8], now: Instant) -> Step {
        match self.take_frame(bytes, now) {
            Step::Go if now.duration_since(self.last_tick) >= self.tick_every() => self.tick(now),
            step => step,
        }
    }

    fn take_frame(&mut self, bytes: &[u8], now: Instant) -> Step {
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
        // The keymaps the batch's text would need, counted before any of it
        // is typed: a batch is taken or refused whole.
        let mut map = self.text_map.clone();
        let mut keymaps = 0u32;
        for e in &events {
            if let Ev::Text(t) = e {
                let chars: Vec<char> = t.chars().collect();
                for (m, _) in text_runs(&map, &chars) {
                    if m != map {
                        keymaps += 1;
                        map = m;
                    }
                }
            }
        }
        if let Err(why) = self.rate_ok(now, text, keymaps) {
            return Step::End(Why::Refused, why);
        }
        // A frame sent twice (a resend after a lost write) is not applied twice.
        if self.last_seq.is_some_and(|last| seq <= last) {
            return Step::Go;
        }
        self.last_seq = Some(seq);
        self.counts.frames += 1;
        self.shell = self.gate.shell_file();
        if self.paused {
            if self.locked_now() {
                self.counts.dropped += events.len() as u64;
                return Step::Go;
            }
            self.paused = false;
            (self.out)(json!({"event": "input_resumed"}));
        }
        let total = events.len();
        for (n, ev) in events.into_iter().enumerate() {
            // Before EVERY event: a frame that started unlocked can end
            // after the screen locked.
            if self.locked_now() {
                self.counts.dropped += (total - n) as u64;
                return match self.pause() {
                    Ok(()) => Step::Go,
                    Err(e) => gone(e),
                };
            }
            self.counts.add(&ev);
            if let Err(e) = self.apply(ev) {
                return gone(e);
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
        for (map, run) in text_runs(&self.text_map, &chars) {
            if map != self.text_map {
                // Forgotten first: if the upload fails, what the compositor
                // has is unknown.
                self.text_map.clear();
                self.backend.text_keymap(&map)?;
                self.text_map = map;
            }
            self.held.text_keys = (1..=self.text_map.len() as u32).collect();
            self.record();
            for c in &chars[run] {
                if self.locked_now() {
                    self.held.text_keys.clear();
                    return self.pause();
                }
                let code = self
                    .text_map
                    .iter()
                    .position(|d| d == c)
                    .expect("in the keymap") as u32
                    + 1;
                self.backend.text_key(code, true)?;
                self.backend.text_key(code, false)?;
            }
            self.held.text_keys.clear();
            self.record();
        }
        Ok(())
    }

    /// Time passing with nothing from the phone: the grant, the compositor,
    /// the gate and the lease.
    pub fn tick(&mut self, now: Instant) -> Step {
        self.last_tick = now;
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
            if let Err(e) = self.pause() {
                return gone(e);
            }
        } else if !locked && self.paused {
            self.paused = false;
            (self.out)(json!({"event": "input_resumed"}));
        }
        if !self.held.is_empty() && now.duration_since(self.last_frame) > LEASE {
            if let Err(e) = self.release_all() {
                return gone(e);
            }
            (self.out)(json!({"event": "input_released", "why": "lease"}));
        }
        Step::Go
    }

    /// The channel is over: let go of everything, and say why when there is
    /// a reason the phone did not give itself.
    pub fn end(&mut self, why: Option<(Why, String)>) {
        if let Err(e) = self.release_all() {
            eprintln!(
                "rime-remoted: touchpad: could not let go of everything ({e}); \
                 the rest is released by the next channel or start"
            );
        }
        if let Some((why, message)) = why {
            (self.out)(json!({"event": "input_closed", "why": why.word(), "message": message}));
        }
    }
}

impl Drop for Session {
    /// A session dropped without [`Session::end`] (a panic on its thread)
    /// still lets go of what it holds; what it cannot stays in the crash
    /// file.
    fn drop(&mut self) {
        if !self.held.is_empty() {
            let _ = self.release_all();
        }
    }
}

/// The channel's end when the compositor stops taking input.
fn gone(e: std::io::Error) -> Step {
    Step::End(
        Why::Unavailable,
        format!("the compositor stopped taking input: {e}"),
    )
}

/// What to do about keys and buttons an earlier run left held. Keys get a
/// bare release from the new keyboard; a button needs a whole click to come
/// up on Hyprland, which is only given while the screen is unlocked (a click
/// on a lock screen is input on it). What could not be done, or was not
/// confirmed by the compositor, is returned, to be kept for next time.
pub fn recover(backend: &mut dyn Backend, held: &Held, unlocked: bool) -> Held {
    let mut left = Held::default();
    for code in &held.keys {
        if backend.key(*code, false).is_err() {
            left.keys.insert(*code);
        }
    }
    for code in &held.text_keys {
        if backend.key(*code, false).is_err() {
            left.text_keys.insert(*code);
        }
    }
    if !held.keys.is_empty() || !held.text_keys.is_empty() {
        let _ = backend.modifiers(0, 0);
    }
    for code in &held.buttons {
        if !unlocked
            || backend.button(*code, true).is_err()
            || backend.button(*code, false).is_err()
        {
            left.buttons.insert(*code);
        }
    }
    if backend.sync().is_err() {
        return held.clone();
    }
    left
}

// ── the live channel ───────────────────────────────────────────────────────

/// The one input channel this machine has open, if any: from the moment
/// [`open`] reserves it (before the slow connect), or while the start's
/// recovery releases what a crashed run left held (`device` empty).
struct Live {
    token: u64,
    device: String,
    /// A [`Why`] set by somebody else to end it; 0 for none.
    stop: Arc<AtomicU8>,
    /// The session thread's end, to wake it (none for the recovery).
    wake: Option<UnixStream>,
    done: Arc<(Mutex<bool>, Condvar)>,
}

/// The slot a [`Live`] holds, given back when this is dropped (also by a
/// panic): the entry goes if it is still this one, and whoever waits for
/// the end is woken.
struct Slot {
    token: u64,
    done: Arc<(Mutex<bool>, Condvar)>,
}

impl Drop for Slot {
    fn drop(&mut self) {
        {
            let mut guard = lock_live();
            if guard.as_ref().is_some_and(|l| l.token == self.token) {
                *guard = None;
            }
        }
        let (m, cv) = &*self.done;
        *m.lock().unwrap_or_else(|e| e.into_inner()) = true;
        cv.notify_all();
    }
}

/// Take the slot for `device` (empty: the start's recovery), unless one is
/// held. The global lock is held only for this, never across a connect.
fn reserve(device: &str, wake: Option<UnixStream>) -> Result<(Slot, Arc<AtomicU8>), ()> {
    let mut guard = lock_live();
    if guard.is_some() {
        return Err(());
    }
    let token = TOKENS.fetch_add(1, Ordering::Relaxed);
    let stop = Arc::new(AtomicU8::new(0));
    let done = Arc::new((Mutex::new(false), Condvar::new()));
    *guard = Some(Live {
        token,
        device: device.to_string(),
        stop: Arc::clone(&stop),
        wake,
        done: Arc::clone(&done),
    });
    Ok((Slot { token, done }, stop))
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
        Some(l) if l.device == device && !device.is_empty() => {
            l.stop.store(why as u8, Ordering::SeqCst);
            if let Some(w) = &l.wake {
                let _ = w.shutdown(std::net::Shutdown::Read);
            }
            true
        }
        _ => false,
    }
}

/// The words the phone is told when somebody at the computer stopped it.
pub const STOPPED_HERE: &str = "stopped at the computer";

/// End the live input channel (only `device`'s, when given) because somebody
/// at this computer asked: everything it holds is released, the phone gets
/// `input_closed` with `stopped`, and that phone is refused a new channel for
/// [`COOL_DOWN`], unless a person raises its grant again. The grant itself is
/// not changed. The id of the phone stopped, if one was.
///
/// The start's recovery (an empty device) is never stopped this way: what it
/// releases is what a crashed run left held.
pub fn stop_here(device: Option<&str>, now: Instant) -> Option<String> {
    let guard = lock_live();
    let l = guard
        .as_ref()
        .filter(|l| !l.device.is_empty() && device.map_or(true, |d| d == l.device))?;
    // The cool-down first, while the slot is still held: the phone can open
    // again only once this channel has given the slot back, and by then it
    // is refused.
    lock_cooling().insert(l.device.clone(), now + COOL_DOWN);
    l.stop.store(Why::StoppedHere as u8, Ordering::SeqCst);
    if let Some(w) = &l.wake {
        let _ = w.shutdown(std::net::Shutdown::Read);
    }
    Some(l.device.clone())
}

fn lock_cooling() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, Instant>> {
    static COOLING: OnceLock<Mutex<std::collections::HashMap<String, Instant>>> = OnceLock::new();
    COOLING
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// How long `device` is still refused after a stop at the computer.
pub fn cool_down_left(device: &str, now: Instant) -> Option<Duration> {
    let mut cooling = lock_cooling();
    match cooling.get(device) {
        Some(until) if *until > now => Some(*until - now),
        Some(_) => {
            cooling.remove(device);
            None
        }
        None => None,
    }
}

/// A person raised `device`'s grant again: it may open a channel at once.
pub fn lift_cool_down(device: &str) {
    lock_cooling().remove(device);
}

/// A phone's name as the status file and `input_status` carry it: no
/// control, format or line-breaking characters (a name could otherwise
/// reorder or break the Shell's line), trimmed, at most [`STATUS_NAME_MAX`]
/// characters, and never empty.
pub fn shown_name(name: &str) -> String {
    let kept: String = name
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(
                    c,
                    '\u{200b}'..='\u{200f}'
                        | '\u{2028}'..='\u{202e}'
                        | '\u{2060}'..='\u{2069}'
                        | '\u{feff}'
                        | '\u{061c}'
                )
        })
        .collect();
    let shown: String = kept.trim().chars().take(STATUS_NAME_MAX).collect();
    let shown = shown.trim_end().to_string();
    if shown.is_empty() {
        "a phone".to_string()
    } else {
        shown
    }
}

/// The phone using this computer as a touchpad, as `input_status` answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub device: String,
    pub name: String,
    pub since_ms: u64,
}

fn lock_shown() -> std::sync::MutexGuard<'static, Option<Shown>> {
    static SHOWN: Mutex<Option<Shown>> = Mutex::new(None);
    SHOWN.lock().unwrap_or_else(|e| e.into_inner())
}

/// Who is using this computer as a touchpad right now, if anybody.
pub fn shown() -> Option<Shown> {
    lock_shown().clone()
}

/// Write the status file whole, through a rename, 0600 in a 0700 directory,
/// as `focus.json` is: `{"v":1,"live":false}`, or with `live` true the
/// phone's id, its [`shown_name`] and when it started. Nothing about what is
/// typed. Called with the [`lock_shown`] lock held, so two writes never race.
fn write_status(path: &Path, live: Option<&Shown>) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    rime_agent_core::paths::ensure_private_dir(dir)?;
    let tmp = dir.join(format!(".{STATUS_FILE}.{}", std::process::id()));
    let body = match live {
        Some(s) => {
            json!({"v": 1, "live": true, "device": s.device, "name": s.name, "since_ms": s.since_ms})
        }
        None => json!({"v": 1, "live": false}),
    };
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(body.to_string().as_bytes())?;
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, path)
}

/// Say `live` (or not) in memory and in the status file. A file that could
/// not be written is said in the journal; the channel goes on.
fn set_shown(path: &Path, live: Option<Shown>) {
    let mut shown = lock_shown();
    if let Err(e) = write_status(path, live.as_ref()) {
        eprintln!(
            "rime-remoted: touchpad: could not write {}: {e}",
            path.display()
        );
    }
    *shown = live;
}

/// While this lives, the status file says the phone is using the computer;
/// when it is dropped (also by a panic) it says nobody is.
struct Showing {
    path: PathBuf,
}

impl Showing {
    fn new(path: PathBuf, device: &str, name: &str, since_ms: u64) -> Showing {
        set_shown(
            &path,
            Some(Shown {
                device: device.to_string(),
                name: shown_name(name),
                since_ms,
            }),
        );
        Showing { path }
    }
}

impl Drop for Showing {
    fn drop(&mut self) {
        set_shown(&self.path, None);
    }
}

/// End whatever channel is live and wait up to `within` for it to have let
/// go of everything: this process is stopping.
pub fn shutdown(within: Duration) {
    let done = {
        let guard = lock_live();
        let Some(l) = guard.as_ref() else { return };
        l.stop.store(Why::Stopping as u8, Ordering::SeqCst);
        if let Some(w) = &l.wake {
            let _ = w.shutdown(std::net::Shutdown::Read);
        }
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
    if let Some(left) = cool_down_left(device, Instant::now()) {
        return Err(error(
            "stopped",
            &format!(
                "this touchpad was stopped at the computer; try again in {} s, or allow it there at \
                 once with `rime remote devices grant {} input on`",
                left.as_secs() + u64::from(left.subsec_nanos() > 0),
                name.as_deref().map_or(device.to_string(), shown_name)
            ),
        ));
    }
    let env = env();
    if let Err(why) = env.available() {
        return Err(error("unavailable", &why));
    }
    let (theirs, mine) = seqpacket_pair().map_err(|e| error("unavailable", &e.to_string()))?;
    let wake = mine
        .try_clone()
        .map_err(|e| error("unavailable", &e.to_string()))?;
    let writer = mine
        .try_clone()
        .map_err(|e| error("unavailable", &e.to_string()))?;
    // The slot first, briefly under the global lock; everything slow after
    // it without, so revoking, lowering the grant or stopping this service
    // never waits behind a compositor that is slow to answer. They find the
    // slot and mark it, and it is looked at after each slow step.
    let (slot, stop) = match reserve(device, Some(wake)) {
        Ok(x) => x,
        Err(()) => {
            // The start's recovery is quick: wait for it once.
            let recovering = lock_live()
                .as_ref()
                .filter(|l| l.device.is_empty())
                .map(|l| Arc::clone(&l.done));
            if let Some(done) = recovering {
                let (m, cv) = &*done;
                let g = m.lock().unwrap_or_else(|e| e.into_inner());
                let _ = cv.wait_timeout_while(g, Duration::from_secs(5), |finished| !*finished);
            }
            let wake = mine
                .try_clone()
                .map_err(|e| error("unavailable", &e.to_string()))?;
            reserve(device, Some(wake))
                .map_err(|()| error("busy", "another phone is using this computer as a touchpad"))?
        }
    };
    let stopped = || -> Result<(), Vec<u8>> {
        match Why::from_u8(stop.load(Ordering::SeqCst)) {
            None => Ok(()),
            Some(Why::NotAllowed) => Err(error(
                "not_allowed",
                "this phone is no longer allowed to use this computer as a touchpad",
            )),
            Some(Why::Stopping) => Err(error(
                "unavailable",
                "Rime Remote on this computer is stopping",
            )),
            Some(Why::StoppedHere) => Err(error("stopped", STOPPED_HERE)),
            Some(_) => Err(error("unavailable", "the touchpad was stopped")),
        }
    };
    let locked_refusal = || {
        error(
            "locked",
            "this computer is locked; unlock it at the keyboard first",
        )
    };
    let gate = env.gate();
    let shell = gate.shell_file();
    // Before the connect the compositor's notifier is unknown, so this early
    // answer is lenient about an absent Shell file; the check after the
    // round trip below is the one that decides.
    if locked(&gate.sources(None, true, shell)) {
        return Err(locked_refusal());
    }
    let (mut backend, bound) = env.backend().map_err(|why| error("unavailable", &why))?;
    stopped()?;
    // The compositor's notifier is known only now, after the round trip.
    let unlocked = !gate.locked_for(backend.as_ref());
    if unlocked {
        backend
            .warm_up()
            .map_err(|e| error("unavailable", &e.to_string()))?;
    }
    // Keys or buttons an earlier run left held go first, on the new devices.
    let crash = env.crash_file();
    if let Some(left) = read_held(&crash) {
        write_held(&crash, &recover(backend.as_mut(), &left, unlocked));
    }
    if !unlocked || gate.locked_for(backend.as_ref()) {
        return Err(locked_refusal());
    }
    stopped()?;
    let st = Arc::clone(state);
    let dev = device.to_string();
    let session = Session::new(
        backend,
        gate,
        Box::new(move || granted(&st, &dev)),
        Box::new(move || {
            env.available()?;
            env.same_session(bound.as_deref())
        }),
        // One JSON line per event, as on `device_events`.
        Box::new(move |line: Value| {
            let _ = (&writer).write_all(format!("{line}\n").as_bytes());
        }),
        Some(crash),
        Instant::now(),
    );
    let who = (
        device.to_string(),
        name.unwrap_or_else(|| device.to_string()),
    );
    let opened_line = audit_open(&who.0, &who.1);
    let showing = Showing::new(
        env.runtime_file(STATUS_FILE),
        &who.0,
        &who.1,
        rime_remote_core::now_ms(),
    );
    // The slot goes with the thread, which gives it back however it ends,
    // and the status file with it, which says the end first.
    std::thread::Builder::new()
        .name("rime-input".into())
        .spawn(move || run(session, mine, stop, slot, showing, who))
        .map_err(|e| error("unavailable", &format!("no thread for the touchpad: {e}")))?;
    eprintln!("{opened_line}");
    Ok((
        theirs,
        json!({"reply": "input_ready"}).to_string().into_bytes(),
    ))
}

/// The session thread: frames in, ticks between, everything released at the
/// end however it ends.
fn run(
    session: Session,
    sock: UnixStream,
    stop: Arc<AtomicU8>,
    slot: Slot,
    showing: Showing,
    who: (String, String),
) {
    // Dropped last, even by a panic: the slot is given back after the
    // session (whose drop lets go of what it holds) is gone, and after the
    // status file says nobody is using the computer, so it never says that
    // over the next phone's channel.
    let _slot = slot;
    let _showing = showing;
    // Bound after both, so a panic drops it (letting go of what it holds)
    // before them: parameters would be dropped after every local.
    let mut session = session;
    let opened = Instant::now();
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
                Why::StoppedHere => STOPPED_HERE.into(),
                _ => String::new(),
            }
        } else {
            message
        };
        (why, message)
    });
    let word = ended.as_ref().map_or("disconnected", |(why, _)| why.word());
    session.end(ended);
    eprintln!(
        "{}",
        audit_close(&who.0, &who.1, word, opened.elapsed(), &session.counts())
    );
    drop(session);
    drop(sock);
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

    /// The devices, on the compositor of the user's graphical session,
    /// with that session's id (`None` for the stub).
    fn backend(&self) -> Result<(Box<dyn Backend>, Option<String>), String> {
        match self {
            Env::Stub(dir) => StubBackend::open(dir)
                .map(|b| (Box::new(b) as Box<dyn Backend>, None))
                .map_err(|e| e.to_string()),
            Env::Real => {
                let display = logind::watch()
                    .display()
                    .ok_or("nobody is logged in to a desktop on this computer")?;
                let stream = wayland::discover(&rime_agent_core::paths::runtime_dir(), &display)?;
                let layout = wayland::Layout::of_session();
                wayland::Wayland::over(stream, &layout)
                    .map(|b| (Box::new(b) as Box<dyn Backend>, Some(display)))
            }
        }
    }

    /// Whether the session the devices were made in is still the user's
    /// graphical one: after a log out and in, the old socket's successor
    /// may be someone else's to answer.
    fn same_session(&self, bound: Option<&str>) -> Result<(), String> {
        match self {
            Env::Stub(_) => Ok(()),
            Env::Real => {
                if logind::watch().display().as_deref() == bound {
                    Ok(())
                } else {
                    Err("the desktop session this touchpad was opened in has ended".into())
                }
            }
        }
    }
}

/// At start: begin following logind, so the first channel a phone opens
/// finds the lock state already known instead of unknown (and refused).
pub fn prepare() {
    let env = env();
    // A run that was killed outright may have left "live" in the file: the
    // Shell must never show a touchpad in use that is not.
    set_shown(&env.runtime_file(STATUS_FILE), None);
    if let Env::Real = env {
        let _ = logind::watch();
    }
}

/// At start: release what a run that crashed left held, as far as can be
/// done now. On its own thread, since the compositor may not be up yet; what
/// is left waits for the next channel.
pub fn recover_at_start() {
    let env = env();
    if read_held(&env.crash_file()).is_none() {
        return;
    }
    std::thread::spawn(move || recover_held(env, Duration::from_secs(3)));
}

/// Release what the crash file holds, under the same slot a live channel
/// takes, so the two never write the file (or the devices) at once. Skipped
/// when a channel holds the slot: its open releases it instead. Waits up to
/// `settle` for logind's first answer (unknown counts as locked).
fn recover_held(env: &'static Env, settle: Duration) {
    let Ok((_slot, stop)) = reserve("", None) else {
        return;
    };
    let crash = env.crash_file();
    let Some(left) = read_held(&crash) else {
        return;
    };
    let gate = env.gate();
    let deadline = Instant::now() + settle;
    while (gate.logind)().0.is_none()
        && Instant::now() < deadline
        && stop.load(Ordering::SeqCst) == 0
    {
        std::thread::sleep(Duration::from_millis(100));
    }
    let Ok((mut backend, _)) = env.backend() else {
        return;
    };
    let unlocked = !gate.locked_for(backend.as_ref());
    if unlocked && backend.warm_up().is_err() {
        return;
    }
    write_held(&crash, &recover(backend.as_mut(), &left, unlocked));
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
        /// The id of the user's graphical (`Display`) session, as logind
        /// last said; `None` when there is none or nothing is known.
        display: Option<String>,
    }

    impl Seen {
        /// The watcher lost logind and starts again. `LockedHint` is unknown
        /// (locked) until it is read again; an outstanding `Lock` stays, as
        /// only an `Unlock` or `LockedHint` going true→false clears it, and an
        /// `Unlock` sent while nobody listened is not known to have been.
        fn restarted(&mut self) {
            self.hint = None;
        }

        /// logind names the user's graphical session (`None`: there is none).
        /// A `Lock` heard for another session is not this one's.
        fn session(&mut self, id: Option<String>) {
            if self.display != id {
                self.signal = false;
            }
            self.no_session = id.is_none();
            self.display = id;
        }
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

        /// The id of the user's graphical session (`5` for `session-5.scope`).
        pub fn display(&self) -> Option<String> {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .display
                .clone()
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
                        w2.set(Seen::restarted);
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
        let (display_id, display): (String, zbus::zvariant::OwnedObjectPath) =
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
                s.session(None);
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
            s.session(Some(display_id.clone()));
            s.hint = Some(hint);
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

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_lock_outlives_the_watcher_starting_again_and_only_an_unlock_clears_it() {
            let w = Watch(Arc::new(Mutex::new(Seen::default())));
            w.set(|s| s.session(Some("3".into())));
            apply_hint(&w, Some(false));
            // Lock heard; then the bus goes and the watcher starts again.
            w.set(|s| s.signal = true);
            w.set(Seen::restarted);
            assert_eq!(w.read(), (None, true));
            // The same session again, LockedHint false: no true→false was
            // seen, so the Lock stands.
            w.set(|s| s.session(Some("3".into())));
            apply_hint(&w, Some(false));
            assert_eq!(w.read(), (Some(false), true));
            assert_eq!(w.display().as_deref(), Some("3"));
            // A true→false clears it.
            apply_hint(&w, Some(true));
            apply_hint(&w, Some(false));
            assert_eq!(w.read(), (Some(false), false));
            // A Lock for a session that is no longer the graphical one is
            // not the new one's.
            w.set(|s| s.signal = true);
            w.set(|s| s.session(Some("7".into())));
            assert!(!w.read().1);
            w.set(|s| s.session(None));
            assert!(w.no_session());
            assert_eq!(w.display(), None);
        }
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

    /// Every socket in `runtime` that may be the compositor's:
    /// `WAYLAND_DISPLAY` when it is a name, then each `wayland-<digits>`,
    /// lowest first, once each.
    pub fn candidates(runtime: &Path) -> Vec<PathBuf> {
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
        let mut seen = BTreeSet::new();
        names
            .into_iter()
            .filter(|n| seen.insert(n.clone()))
            .map(|n| runtime.join(n))
            .collect()
    }

    /// The compositor of the user's graphical session `display`, connected.
    ///
    /// Not whoever answers first: any program running as this user can
    /// listen on a `wayland-N` of its own (or be what `WAYLAND_DISPLAY`
    /// names), and input sent there would be its to read. So each candidate
    /// that accepts is asked who listens on it ([`peer_session`]), and only
    /// a listener in `display`'s own session scope counts. None, or more
    /// than one process, is refused: there is no telling which is real.
    pub fn discover(runtime: &Path, display: &str) -> Result<UnixStream, String> {
        if cfg!(test) {
            return Err("unit tests never look for a desktop".into());
        }
        pick(&candidates(runtime), display, |_, stream| {
            peer_session(stream)
        })
    }

    /// [`discover`]'s choice, with who listens on a socket asked of
    /// `classify` (`(session id, pid)`), so a test can stand in for the
    /// kernel and logind.
    pub fn pick(
        candidates: &[PathBuf],
        display: &str,
        classify: impl Fn(&Path, &UnixStream) -> Option<(String, i32)>,
    ) -> Result<UnixStream, String> {
        let mut chosen: Vec<(i32, UnixStream)> = Vec::new();
        for path in candidates {
            let Ok(stream) = UnixStream::connect(path) else {
                continue;
            };
            if let Some((session, pid)) = classify(path, &stream) {
                if session == display && !chosen.iter().any(|(p, _)| *p == pid) {
                    chosen.push((pid, stream));
                }
            }
        }
        match chosen.len() {
            1 => Ok(chosen.remove(0).1),
            0 => Err("no desktop of this user's graphical session is listening".into()),
            _ => Err(
                "more than one program in this user's graphical session is listening as a desktop; \
                 not guessing which is real"
                    .into(),
            ),
        }
    }

    /// The uid and pid of whoever listens on `stream`'s other end
    /// (`SO_PEERCRED`: the process that called listen(2)).
    pub fn peer_creds(stream: &UnixStream) -> Option<(u32, i32)> {
        // SAFETY: a zeroed ucred is valid, and the length matches it.
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        };
        (rc == 0 && cred.pid > 0).then_some((cred.uid, cred.pid))
    }

    /// The logind session whose scope the listener on `stream` runs in,
    /// with its pid: only when it is this user, and only from the session
    /// scope itself.
    pub fn peer_session(stream: &UnixStream) -> Option<(String, i32)> {
        let (uid, pid) = peer_creds(stream)?;
        // SAFETY: getuid has no failure mode.
        if uid != unsafe { libc::getuid() } {
            return None;
        }
        let cgroup = rime_agent_core::origin::read_cgroup(pid).ok()?;
        session_of_cgroup(&cgroup, uid).map(|id| (id, pid))
    }

    /// `5` for exactly `/user.slice/user-<uid>.slice/session-5.scope`.
    ///
    /// Nothing under `user@<uid>.service` counts, though a user unit may
    /// call itself `session-5.scope` there (`systemd-run --user --scope
    /// --unit=session-5.scope`): those are the user's to make, while a
    /// session scope is made by logind and only root may move a process
    /// into it.
    pub fn session_of_cgroup(cgroup: &str, uid: u32) -> Option<String> {
        let id = cgroup
            .strip_prefix(&format!("/user.slice/user-{uid}.slice/session-"))?
            .strip_suffix(".scope")?;
        (!id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric())).then(|| id.to_string())
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
        /// Connect to the compositor at `socket` and make the three devices
        /// (the real-compositor test's way in; the daemon goes through
        /// [`discover`], which checks who listens first).
        #[cfg(test)]
        pub fn connect(socket: &Path, layout: &Layout) -> Result<Wayland, String> {
            let sock = UnixStream::connect(socket)
                .map_err(|e| format!("cannot reach the desktop: {e}"))?;
            Wayland::over(sock, layout)
        }

        /// Make the three devices over a connected socket. Refused when the
        /// compositor offers no virtual pointer or keyboard (gamescope).
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

        fn warm_up(&mut self) -> std::io::Result<()> {
            // Hyprland drops a new pointer's first clicks until it has seen
            // a relative motion (measured): one there and back.
            self.motion(1.0, 0.0)?;
            self.motion(-1.0, 0.0)?;
            self.sync()
        }

        fn compositor_locked(&self) -> Option<bool> {
            self.shared
                .heard
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .locked
        }

        fn offers_lock_notifier(&self) -> bool {
            self.shared.note.load(Ordering::SeqCst) != 0
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
        offers_notifier: Arc<AtomicBool>,
        /// Requests (as logged) the compositor refuses.
        fail: Arc<Mutex<Vec<String>>>,
    }

    impl Fake {
        fn take(&self) -> Vec<String> {
            std::mem::take(&mut *self.log.lock().expect("log"))
        }
        fn push(&self, s: String) -> std::io::Result<()> {
            if self.fail.lock().expect("fail").contains(&s) {
                self.log.lock().expect("log").push(format!("FAILED {s}"));
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "the fake compositor refused it",
                ));
            }
            self.log.lock().expect("log").push(s);
            Ok(())
        }
        fn refuse(&self, what: &[&str]) {
            *self.fail.lock().expect("fail") = what.iter().map(|s| s.to_string()).collect();
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
        fn warm_up(&mut self) -> std::io::Result<()> {
            self.push("warm".into())
        }
        fn compositor_locked(&self) -> Option<bool> {
            *self.compositor.lock().expect("c")
        }
        fn offers_lock_notifier(&self) -> bool {
            self.offers_notifier.load(Ordering::SeqCst)
        }
        fn gone(&self) -> Option<String> {
            None
        }
    }

    /// Write the Shell's file the way the Shell must: whole (a rename), mode
    /// 0600, whatever this process's umask.
    pub(super) fn write_shell_file(path: &Path, locked: bool) {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;
        let tmp = path.with_extension("tmp");
        let _ = std::fs::remove_file(&tmp);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .expect("create");
        f.write_all(json!({"locked": locked}).to_string().as_bytes())
            .expect("write");
        drop(f);
        std::fs::rename(&tmp, path).expect("rename");
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
        // labwc's case, the stricter one: no notifier, so the Shell's file
        // must be there and say unlocked.
        write_shell_file(&dir.join("lock.json"), false);
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
            notifier_offered: true,
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
        // And what cannot veto does not: an absent or "unlocked" Shell file
        // where the compositor has a notifier, a notifier that says unlocked
        // or nothing.
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
        // With no notifier (labwc) a stale `LockedHint` false cannot be told
        // from a true one: the Shell's file must be there and say unlocked.
        let labwc = Sources {
            notifier_offered: false,
            ..open
        };
        assert!(locked(&Sources {
            shell: ShellFile::Absent,
            ..labwc
        }));
        assert!(!locked(&Sources {
            shell: ShellFile::Unlocked,
            ..labwc
        }));
        assert!(locked(&Sources {
            shell: ShellFile::Unlocked,
            locked_hint: None,
            ..labwc
        }));
    }

    #[test]
    fn the_shell_file_is_read_only_when_this_user_alone_could_have_written_it() {
        let dir = std::env::temp_dir().join(format!("rime-input-shellfile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let f = dir.join("lock.json");
        let _ = std::fs::remove_file(&f);
        assert_eq!(read_shell_file(&f), ShellFile::Absent);
        use std::os::unix::fs::PermissionsExt;
        let set_mode = |m: u32| {
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(m)).expect("chmod");
        };
        for (text, want) in [
            (r#"{"locked":true}"#.to_string(), ShellFile::Locked),
            (
                r#"{"locked":false,"at":1}"#.to_string(),
                ShellFile::Unlocked,
            ),
            (r#"{"locked":"no"}"#.to_string(), ShellFile::Unreadable),
            (String::new(), ShellFile::Unreadable),
            ("{".to_string(), ShellFile::Unreadable),
            // Bigger than any file the Shell writes, even if it parses.
            (
                format!(r#"{{"locked":false,"pad":"{}"}}"#, "x".repeat(5000)),
                ShellFile::Unreadable,
            ),
        ] {
            std::fs::write(&f, &text).expect("write");
            set_mode(0o600);
            assert_eq!(read_shell_file(&f), want, "{text:.40}");
        }
        // Writable by the group or by others: anyone could have said
        // "unlocked", so it says nothing but locked.
        write_shell_file(&f, false);
        assert_eq!(read_shell_file(&f), ShellFile::Unlocked);
        for mode in [0o620, 0o602, 0o666] {
            set_mode(mode);
            assert_eq!(read_shell_file(&f), ShellFile::Unreadable, "{mode:o}");
        }
        set_mode(0o644);
        assert_eq!(read_shell_file(&f), ShellFile::Unlocked);
        // A link to a file that says unlocked is not the file.
        let real = dir.join("elsewhere.json");
        write_shell_file(&real, false);
        std::fs::remove_file(&f).expect("rm");
        std::os::unix::fs::symlink(&real, &f).expect("symlink");
        assert_eq!(read_shell_file(&f), ShellFile::Unreadable);
        // A FIFO is opened without blocking and refused.
        std::fs::remove_file(&f).expect("rm");
        let c = std::ffi::CString::new(f.as_os_str().as_encoded_bytes()).expect("c");
        // SAFETY: a valid C string; mkfifo only creates a node.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo");
        assert_eq!(read_shell_file(&f), ShellFile::Unreadable);
        // A directory where the file should be cannot be read: locked.
        std::fs::remove_file(&f).expect("rm");
        std::fs::create_dir(&f).expect("mkdir");
        assert_eq!(read_shell_file(&f), ShellFile::Unreadable);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_shell_file_owned_by_someone_else_says_nothing_but_locked() {
        // The rule itself, so the owner check is proven alone: a user
        // cannot make a file another user owns that says unlocked.
        let me = 1000;
        assert!(shell_file_trusted(true, me, me, 0o100600, 16));
        assert!(shell_file_trusted(true, me, me, 0o100644, SHELL_FILE_MAX));
        assert!(
            !shell_file_trusted(true, 0, me, 0o100600, 16),
            "root's file"
        );
        assert!(
            !shell_file_trusted(true, 1001, me, 0o100600, 16),
            "another user's file"
        );
        assert!(!shell_file_trusted(true, me, me, 0o100620, 16));
        assert!(!shell_file_trusted(true, me, me, 0o100602, 16));
        assert!(!shell_file_trusted(false, me, me, 0o100600, 16));
        assert!(!shell_file_trusted(
            true,
            me,
            me,
            0o100600,
            SHELL_FILE_MAX + 1
        ));
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
        fn warm_up(&mut self) -> std::io::Result<()> {
            self.inner.warm_up()
        }
        fn compositor_locked(&self) -> Option<bool> {
            self.inner.compositor_locked()
        }
        fn offers_lock_notifier(&self) -> bool {
            self.inner.offers_lock_notifier()
        }
        fn gone(&self) -> Option<String> {
            None
        }
    }

    #[test]
    fn without_a_lock_notifier_the_shell_file_going_away_pauses_and_lets_go() {
        let mut r = rig("shellgone");
        assert_eq!(
            r.frame(0, 1, json!([{"t": "k", "code": 30, "p": true}])),
            Step::Go
        );
        r.fake.take();
        // logind still says unlocked, but on labwc that may be stale: the
        // Shell's file is gone (the Shell died, or never ran), so locked.
        std::fs::remove_file(r.dir.join("lock.json")).expect("rm");
        assert_eq!(r.session.tick(r.at(20)), Step::Go);
        assert!(r.session.paused());
        assert!(r.fake.take().contains(&"key 30 false".to_string()));
        assert!(r.said().iter().any(|v| v["event"] == "input_paused"));
        // On a compositor with a notifier, the same missing file is no
        // opinion: logind's "unlocked" stands.
        r.fake.offers_notifier.store(true, Ordering::SeqCst);
        assert_eq!(r.session.tick(r.at(40)), Step::Go);
        assert!(!r.session.paused());
        assert!(r.said().iter().any(|v| v["event"] == "input_resumed"));
    }

    #[test]
    fn a_code_is_forgotten_only_once_its_release_was_sent_and_confirmed() {
        let mut r = rig("releasefail");
        let held_file = r.dir.join("input-held.json");
        assert_eq!(
            r.frame(
                0,
                1,
                json!([{"t": "k", "code": 30, "p": true}, {"t": "k", "code": 31, "p": true},
                       {"t": "b", "b": "left", "p": true}])
            ),
            Step::Go
        );
        // The release of 31 fails as the screen locks: the channel ends, and
        // 31 (only) is still held, here and on disk.
        r.fake.refuse(&["key 31 false"]);
        r.lock(Some(true), false);
        assert!(matches!(
            r.session.tick(r.at(20)),
            Step::End(Why::Unavailable, _)
        ));
        assert_eq!(r.session.held().keys, BTreeSet::from([31]));
        assert!(r.session.held().buttons.is_empty());
        assert_eq!(
            read_held(&held_file).expect("kept").keys,
            BTreeSet::from([31])
        );
        // Ending tries again; still refused, so the file stays for the next
        // channel or start.
        r.session.end(Some((Why::Unavailable, String::new())));
        assert_eq!(
            read_held(&held_file).expect("kept").keys,
            BTreeSet::from([31])
        );
        // Once it goes through, nothing is left.
        r.fake.refuse(&[]);
        r.session.end(None);
        assert!(r.session.held().is_empty());
        assert!(!held_file.exists());

        // Sent but never confirmed (the sync fails): everything is kept.
        let mut r = rig("syncfail");
        let held_file = r.dir.join("input-held.json");
        assert_eq!(
            r.frame(0, 1, json!([{"t": "k", "code": 30, "p": true}])),
            Step::Go
        );
        r.fake.refuse(&["sync"]);
        r.session.end(None);
        assert_eq!(r.session.held().keys, BTreeSet::from([30]));
        assert_eq!(
            read_held(&held_file).expect("kept").keys,
            BTreeSet::from([30])
        );

        // Recovery keeps what it could not release, and everything when
        // the compositor does not confirm.
        let held = Held {
            keys: BTreeSet::from([30, 31]),
            buttons: BTreeSet::from([0x110]),
            text_keys: BTreeSet::new(),
        };
        let f = Fake::default();
        f.refuse(&["key 31 false", "button 0x110 true"]);
        let left = recover(&mut f.clone(), &held, true);
        assert_eq!(
            (left.keys, left.buttons),
            (BTreeSet::from([31]), BTreeSet::from([0x110]))
        );
        f.refuse(&["sync"]);
        assert_eq!(recover(&mut f.clone(), &held, true), held);
    }

    #[test]
    fn the_journal_hears_who_why_how_long_and_how_many_never_what() {
        let mut r = rig("audit");
        r.frame(
            0,
            1,
            json!([{"t": "m", "dx": 1, "dy": 2}, {"t": "k", "code": 47, "p": true},
                   {"t": "k", "code": 47, "p": false}, {"t": "text", "s": "hunter2"},
                   {"t": "tap", "code": 46, "mods": ["ctrl"]}, {"t": "hold"}]),
        );
        // A resend is not counted twice.
        r.frame(5, 1, json!([{"t": "m", "dx": 1, "dy": 2}]));
        r.lock(Some(true), false);
        r.frame(
            10,
            2,
            json!([{"t": "text", "s": "secret"}, {"t": "b", "b": "left", "p": true}]),
        );
        let c = r.session.counts();
        assert_eq!(
            c,
            Counts {
                frames: 2,
                motion: 1,
                key: 2,
                text: 1,
                tap: 1,
                hold: 1,
                dropped: 2,
                ..Counts::default()
            }
        );
        let line = audit_close(
            "d1f2",
            "Andre\u{1b}[31m's Pixel",
            "lease",
            Duration::from_secs(75),
            &c,
        );
        assert!(
            line.contains("\"Andre\\u{1b}[31m's Pixel\" (d1f2) stopped (lease) after 75s"),
            "{line}"
        );
        assert!(
            line.contains("key=2")
                && line.contains("text=1")
                && line.contains("dropped_while_locked=2"),
            "{line}"
        );
        for secret in ["hunter2", "secret", "47", "46", "dx", "\u{1b}"] {
            assert!(!line.contains(secret), "{secret} in {line}");
        }
        let line = audit_open("d1f2", "Pixel");
        assert!(line.contains("\"Pixel\" (d1f2) started"), "{line}");
    }

    /// Tests that use the process's one live slot or its stub desktop, one
    /// at a time.
    fn read_status(path: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(path).expect("status file")).expect("json")
    }

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static M: Mutex<()> = Mutex::new(());
        M.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The unit tests' stub desktop, emptied, unlocked (a compositor with
    /// a notifier, so no Shell file is needed).
    fn fresh_stub() -> PathBuf {
        let Env::Stub(dir) = env() else {
            panic!("unit tests run on the stub")
        };
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).expect("mkdir");
        std::fs::write(
            dir.join("sources.json"),
            json!({"locked_hint": false, "notifier": false}).to_string(),
        )
        .expect("sources");
        dir.clone()
    }

    fn stub_actions(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("actions.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| {
                let v: Value = serde_json::from_str(l).expect("json");
                format!(
                    "{} {} {}",
                    v["a"].as_str().unwrap_or_default(),
                    v["code"],
                    v["p"]
                )
            })
            .collect()
    }

    #[test]
    fn revoking_or_stopping_never_waits_behind_a_slow_compositor() {
        let _s = serial();
        let dir = fresh_stub();
        std::fs::write(dir.join("slow"), "1500").expect("slow");
        let (state, sdir) = crate::serve::tests::a_state("input-slow");
        let id = {
            let mut store = state.devices().expect("store");
            let id = store
                .pair(&rime_remote_core::b64_encode(&[9; 32]), "Pixel", 1, false)
                .expect("pair")
                .id;
            store.set_phone_input(&id, true).expect("grant");
            id
        };
        let req = br#"{"cmd":"input","v":1}"#;
        let (st, id2) = (Arc::clone(&state), id.clone());
        let opener = std::thread::spawn(move || open(&st, &id2, Scope::Full, req));
        let t = Instant::now();
        while lock_live().is_none() {
            assert!(t.elapsed() < Duration::from_secs(2), "never reserved");
            std::thread::sleep(Duration::from_millis(5));
        }
        // Mid-connect: another open is told "busy", and lowering the grant
        // finds the channel, both at once.
        let t = Instant::now();
        let busy: Value =
            serde_json::from_slice(&open(&state, &id, Scope::Full, req).expect_err("opened twice"))
                .expect("json");
        assert_eq!(busy["kind"], "busy", "{busy}");
        assert!(stop_for(&id, Why::NotAllowed));
        assert!(
            t.elapsed() < Duration::from_millis(300),
            "waited {:?} behind the connect",
            t.elapsed()
        );
        // The open, once the compositor answers, gives up without sending
        // the devices anything, and gives the slot back.
        let v: Value = serde_json::from_slice(&opener.join().expect("join").expect_err("opened"))
            .expect("json");
        assert_eq!(v["kind"], "not_allowed", "{v}");
        assert!(lock_live().is_none());
        assert_eq!(stub_actions(&dir), ["open null null", "close null null"]);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(sdir);
    }

    #[test]
    fn a_panic_on_the_session_thread_still_lets_go_and_gives_the_slot_back() {
        let _s = serial();
        let dir = std::env::temp_dir().join(format!("rime-input-panic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        write_shell_file(&dir.join("lock.json"), false);
        let fake = Fake::default();
        // The second call into the compositor code panics.
        let hook = Hook {
            inner: fake.clone(),
            after: 2,
            n: 0,
            then: Box::new(|| panic!("a bug in the compositor code")),
        };
        let held = dir.join("input-held.json");
        let session = Session::new(
            Box::new(hook),
            Gate {
                logind: Box::new(|| (Some(false), false)),
                shell: Some(dir.join("lock.json")),
            },
            Box::new(|| true),
            Box::new(|| Ok(())),
            Box::new(|_| {}),
            Some(held.clone()),
            Instant::now(),
        );
        let (slot, stop) = reserve("pixel", None).expect("free");
        let (theirs, mine) = seqpacket_pair().expect("pair");
        let status = dir.join(STATUS_FILE);
        let showing = Showing::new(status.clone(), "pixel", "Pixel", 1);
        assert_eq!(read_status(&status)["live"], true);
        let t = std::thread::spawn(move || {
            run(
                session,
                mine,
                stop,
                slot,
                showing,
                ("pixel".into(), "Pixel".into()),
            )
        });
        for (seq, ev) in [
            (1, json!([{"t": "k", "code": 30, "p": true}])),
            (2, json!([{"t": "m", "dx": 1, "dy": 1}])),
        ] {
            (&theirs)
                .write_all(json!({"seq": seq, "ev": ev}).to_string().as_bytes())
                .expect("send");
        }
        assert!(t.join().is_err(), "the thread did not panic");
        assert!(lock_live().is_none(), "the slot was kept");
        assert!(fake.take().contains(&"key 30 false".to_string()));
        assert!(read_held(&held).is_none());
        // And the Shell is not left showing a touchpad in use.
        assert_eq!(read_status(&status), json!({"v": 1, "live": false}));
        assert_eq!(shown(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A paired phone with the grant, in a fresh state.
    fn granted_phone(tag: &str) -> (Arc<State>, PathBuf, String) {
        let (state, sdir) = crate::serve::tests::a_state(tag);
        let id = {
            let mut store = state.devices().expect("store");
            let id = store
                .pair(&rime_remote_core::b64_encode(&[9; 32]), "Pixel", 1, false)
                .expect("pair")
                .id;
            store.set_phone_input(&id, true).expect("grant");
            id
        };
        (state, sdir, id)
    }

    /// Lines the session thread wrote to the phone, until one has `event`
    /// `until` (or 5 s pass).
    fn lines_until(sock: &UnixStream, until: &str) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = Vec::new();
        let mut buf = vec![0u8; 4096];
        sock.set_read_timeout(Some(Duration::from_millis(50)))
            .expect("timeout");
        while Instant::now() < deadline {
            match (&*sock).read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    for l in String::from_utf8_lossy(&buf[..n]).lines() {
                        let v: Value = serde_json::from_str(l).expect("json line");
                        let last = v["event"] == until;
                        got.push(v);
                        if last {
                            return got;
                        }
                    }
                }
                Err(_) => {}
            }
        }
        got
    }

    fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ok() {
            assert!(Instant::now() < deadline, "waited 5 s for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn stopping_at_the_computer_lets_go_tells_the_phone_and_holds_it_off() {
        let _s = serial();
        let dir = fresh_stub();
        let (state, sdir, id) = granted_phone("input-stop");
        let status = env().runtime_file(STATUS_FILE);
        let req = br#"{"cmd":"input","v":1}"#;
        let (theirs, ready) = open(&state, &id, Scope::Full, req).expect("opened");
        assert_eq!(
            serde_json::from_slice::<Value>(&ready).expect("json")["reply"],
            "input_ready"
        );
        // The status file says who, from the moment it is live.
        let live = read_status(&status);
        assert_eq!(live["v"], 1);
        assert_eq!(live["live"], true);
        assert_eq!(live["device"], id.as_str());
        assert_eq!(live["name"], "Pixel");
        assert!(live["since_ms"].as_u64().is_some_and(|ms| ms > 0), "{live}");
        assert_eq!(
            live.as_object().expect("object").len(),
            5,
            "nothing else in it: {live}"
        );
        assert_eq!(shown().map(|s| s.device), Some(id.clone()));
        (&theirs)
            .write_all(br#"{"seq":1,"ev":[{"t":"k","code":30,"p":true}]}"#)
            .expect("send");
        wait_until("the key press", || {
            stub_actions(&dir).contains(&"key 30 true".to_string())
        });
        // A stop for another phone stops nothing.
        assert_eq!(stop_here(Some("someone-else"), Instant::now()), None);
        let at = Instant::now();
        assert_eq!(stop_here(None, at).as_deref(), Some(id.as_str()));
        let said = lines_until(&theirs, "input_closed");
        let closed = said.last().expect("a line");
        assert_eq!(closed["event"], "input_closed", "{said:?}");
        assert_eq!(closed["why"], "stopped");
        assert_eq!(closed["message"], STOPPED_HERE);
        wait_until("the slot back", || lock_live().is_none());
        // Everything held was let go, and the Shell is told nobody is on it.
        assert!(stub_actions(&dir).contains(&"key 30 false".to_string()));
        assert!(read_held(&env().crash_file()).is_none());
        assert_eq!(read_status(&status), json!({"v": 1, "live": false}));
        assert_eq!(shown(), None);
        // The grant is as it was.
        assert!(granted(&state, &id));
        // The same phone is held off, with its own kind...
        let again: Value =
            serde_json::from_slice(&open(&state, &id, Scope::Full, req).expect_err("reopened"))
                .expect("json");
        assert_eq!(again["kind"], "stopped", "{again}");
        assert!(
            again["message"]
                .as_str()
                .unwrap_or_default()
                .contains("grant Pixel input on"),
            "{again}"
        );
        // ...for the cool-down, and not after it.
        assert!(cool_down_left(&id, at + COOL_DOWN - Duration::from_secs(1)).is_some());
        assert_eq!(cool_down_left(&id, at + COOL_DOWN), None);
        // Nothing is live now: a stop says so.
        assert_eq!(stop_here(None, Instant::now()), None);
        lift_cool_down(&id);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(sdir);
    }

    #[test]
    fn a_person_raising_the_grant_again_lifts_the_cool_down() {
        let _s = serial();
        let dir = fresh_stub();
        let (state, sdir, id) = granted_phone("input-lift");
        let req = br#"{"cmd":"input","v":1}"#;
        let (theirs, _) = open(&state, &id, Scope::Full, req).expect("opened");
        assert!(stop_here(Some(&id), Instant::now()).is_some());
        let _ = lines_until(&theirs, "input_closed");
        wait_until("the slot back", || lock_live().is_none());
        assert!(open(&state, &id, Scope::Full, req).is_err(), "cooling down");
        // Through the control socket's own handler, as `rime remote devices
        // grant <phone> input on` does.
        let r = crate::control::tests::grant(&state, &id, "on");
        assert!(r.contains("device_granted"), "{r}");
        let (theirs, _) = open(&state, &id, Scope::Full, req).expect("allowed again at once");
        drop(theirs);
        wait_until("the slot back", || lock_live().is_none());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(sdir);
    }

    #[test]
    fn the_starts_recovery_is_never_stopped_from_the_computer() {
        let _s = serial();
        let (slot, stop) = reserve("", None).expect("free");
        assert_eq!(stop_here(None, Instant::now()), None);
        assert_eq!(stop.load(Ordering::SeqCst), 0);
        drop(slot);
    }

    #[test]
    fn the_status_file_says_nobody_at_start_and_is_private() {
        let _s = serial();
        let dir = fresh_stub();
        let status = env().runtime_file(STATUS_FILE);
        // What a run killed outright would leave.
        std::fs::write(
            &status,
            r#"{"v":1,"live":true,"device":"x","name":"x","since_ms":1}"#,
        )
        .expect("stale");
        prepare();
        assert_eq!(read_status(&status), json!({"v": 1, "live": false}));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&status)
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
        // In its own 0700 directory, written whole (no temporary left).
        let private =
            std::env::temp_dir().join(format!("rime-input-status-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&private);
        let path = private.join("rime-remote").join(STATUS_FILE);
        let s = Shown {
            device: "d".into(),
            name: "n".into(),
            since_ms: 7,
        };
        write_status(&path, Some(&s)).expect("write");
        let dmode = std::fs::metadata(path.parent().expect("dir"))
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(dmode & 0o777, 0o700, "{dmode:o}");
        let names: Vec<_> = std::fs::read_dir(path.parent().expect("dir"))
            .expect("ls")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from(STATUS_FILE)]);
        assert_eq!(
            read_status(&path),
            json!({"v": 1, "live": true, "device": "d", "name": "n", "since_ms": 7})
        );
        let _ = std::fs::remove_dir_all(&private);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_phones_name_reaches_the_shell_bounded_and_without_control_characters() {
        assert_eq!(shown_name("Pixel 8"), "Pixel 8");
        assert_eq!(shown_name("Pix\nel\r\t\u{1b}[2J8"), "Pixel[2J8");
        // Bidi overrides and isolates would reorder the Shell's line.
        assert_eq!(shown_name("\u{202e}lexip\u{2066}x\u{2069}"), "lexipx");
        assert_eq!(shown_name("a\u{200b}b\u{feff}c\u{2028}d"), "abcd");
        assert_eq!(shown_name("   "), "a phone");
        assert_eq!(shown_name("\u{7}\u{0}"), "a phone");
        let long = "é".repeat(500);
        assert_eq!(shown_name(&long).chars().count(), STATUS_NAME_MAX);
        assert_eq!(shown_name("  Pixel  "), "Pixel");
    }

    #[test]
    fn the_starts_recovery_takes_the_same_slot_as_a_channel() {
        let _s = serial();
        let dir = fresh_stub();
        let held = env().crash_file();
        write_held(
            &held,
            &Held {
                keys: BTreeSet::from([50]),
                ..Held::default()
            },
        );
        // A channel holds the slot: the recovery leaves the file to it.
        let (slot, _) = reserve("pixel", None).expect("free");
        recover_held(env(), Duration::ZERO);
        assert!(stub_actions(&dir).is_empty());
        assert_eq!(read_held(&held).expect("kept").keys, BTreeSet::from([50]));
        drop(slot);
        // Free: it releases, holding the slot meanwhile, and gives it back.
        std::fs::write(dir.join("slow"), "300").expect("slow");
        let t = std::thread::spawn(|| recover_held(env(), Duration::ZERO));
        let start = Instant::now();
        while lock_live().is_none() {
            assert!(start.elapsed() < Duration::from_secs(2), "never reserved");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(lock_live().as_ref().is_some_and(|l| l.device.is_empty()));
        t.join().expect("join");
        assert!(lock_live().is_none());
        assert_eq!(
            stub_actions(&dir),
            [
                "open null null",
                "warm null null",
                "key 50 false",
                "mods null null",
                "close null null"
            ]
        );
        assert!(read_held(&held).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn frames_that_never_stop_coming_still_let_go_of_a_held_key_when_it_locks() {
        let mut r = rig("flood");
        assert_eq!(
            r.frame(0, 1, json!([{"t":"k","code":30,"p":true}])),
            Step::Go
        );
        r.fake.take();
        r.lock(Some(true), false);
        // Empty batches every 5 ms: no event to check the gate before, and
        // the session thread's read never times out, so no tick() either.
        for i in 0..10 {
            assert_eq!(r.frame(5 + 5 * i, 2 + i, json!([])), Step::Go);
        }
        assert!(
            r.session.held().is_empty(),
            "a key stayed held across the lock"
        );
        let got = r.fake.take();
        assert!(got.contains(&"key 30 false".to_string()), "{got:?}");
        assert_eq!(
            r.said(),
            [json!({"event": "input_paused", "why": "locked"})]
        );
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
        // The keymap has room for two more: they are added to it, so the 50
        // it has keep their keys.
        r.frame(10, 2, json!([{"t":"text","s":"aba"}]));
        assert_eq!(
            r.fake.take(),
            [
                "keymap 52",
                "text 51 true",
                "text 51 false",
                "text 52 true",
                "text 52 false",
                "text 51 true",
                "text 51 false"
            ]
        );
    }

    #[test]
    fn text_in_characters_the_keymap_has_reuses_it_and_new_keymaps_are_limited() {
        let mut r = rig("keymaps");
        assert_eq!(r.frame(0, 1, json!([{"t":"text","s":"hello"}])), Step::Go);
        assert_eq!(
            r.fake
                .take()
                .iter()
                .filter(|l| l.starts_with("keymap"))
                .count(),
            1
        );
        // Characters it already has: no new keymap, the same keys.
        assert_eq!(r.frame(10, 2, json!([{"t":"text","s":"hole"}])), Step::Go);
        let log = r.fake.take();
        assert!(!log.iter().any(|l| l.starts_with("keymap")), "{log:?}");
        assert_eq!(log[0], "text 1 true", "h is still key 1: {log:?}");
        // Text that needs a new keymap every time (five characters it has
        // not seen in each frame) ends the channel past the limit, and the
        // batch over it is not typed at all.
        let full = |k: u32| -> String {
            (0..5)
                .map(|i| char::from_u32(0x4e00 + k * 5 + i).expect("char"))
                .collect()
        };
        let mut ended = None;
        for k in 0..MAX_KEYMAPS_PER_SEC + 1 {
            r.fake.take();
            let seq = 3 + u64::from(k);
            match r.frame(20 + u64::from(k), seq, json!([{"t":"text","s":full(k)}])) {
                Step::Go => {}
                end => {
                    ended = Some((k, end));
                    break;
                }
            }
        }
        let (k, end) = ended.expect("never limited");
        assert!(
            matches!(end, Step::End(Why::Refused, ref m) if m.contains("keymaps")),
            "{end:?}"
        );
        // "hello" took one, so the 20th frame's keymap is one too many.
        assert_eq!(k, MAX_KEYMAPS_PER_SEC - 1);
        assert!(r.fake.take().is_empty(), "the refused batch was typed");
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
    fn the_desktop_is_the_listener_in_the_graphical_session_not_whoever_answers() {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!("rime-input-pick-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        // wayland-0: what WAYLAND_DISPLAY names, a listener outside the
        // session (a user unit). wayland-1: the session's compositor.
        // wayland-2: a second socket of that same compositor.
        let listen = |n: &str| {
            let l = UnixListener::bind(dir.join(n)).expect("bind");
            l.set_nonblocking(true).expect("nonblocking");
            l
        };
        let (outside, real, real2) = (
            listen("wayland-0"),
            listen("wayland-1"),
            listen("wayland-2"),
        );
        let who = |p: &Path, _: &UnixStream| -> Option<(String, i32)> {
            match p.file_name()?.to_str()? {
                "wayland-0" => Some(("user@".into(), 10)),
                "wayland-1" | "wayland-2" => Some(("3".into(), 20)),
                "wayland-3" => Some(("3".into(), 30)),
                _ => None,
            }
        };
        let paths = |ns: &[&str]| ns.iter().map(|n| dir.join(n)).collect::<Vec<_>>();
        let got = wayland::pick(&paths(&["wayland-0", "wayland-1", "wayland-2"]), "3", who)
            .expect("the session's compositor");
        // It is the session's listener whose connection is kept open, not
        // the one first in line: the others were asked and hung up.
        let open = |l: &UnixListener| {
            let mut c = l.accept().expect("was connected").0;
            c.set_nonblocking(true).expect("nonblocking");
            match std::io::Read::read(&mut c, &mut [0u8; 1]) {
                Ok(0) => false,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => true,
                other => panic!("{other:?}"),
            }
        };
        assert!(open(&real), "the session's compositor was let go");
        assert!(!open(&outside), "the outside listener kept a connection");
        assert!(
            !open(&real2),
            "a second socket of the same compositor was kept too"
        );
        drop(got);
        // Only the outside one: refused, though it answers.
        let err = wayland::pick(&paths(&["wayland-0"]), "3", who).expect_err("chose it");
        assert!(err.contains("no desktop"), "{err}");
        // Another session's listener is not this one's.
        assert!(wayland::pick(&paths(&["wayland-1"]), "4", who).is_err());
        // Two processes in the session: no guessing.
        let _other = listen("wayland-3");
        let err =
            wayland::pick(&paths(&["wayland-1", "wayland-3"]), "3", who).expect_err("guessed");
        assert!(err.contains("more than one"), "{err}");
        // A socket nobody listens on is passed over.
        drop(outside);
        assert!(wayland::pick(&paths(&["wayland-0", "wayland-1"]), "3", who).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_logind_session_scope_of_this_user_is_a_session() {
        use wayland::session_of_cgroup as of;
        assert_eq!(
            of("/user.slice/user-1000.slice/session-3.scope", 1000).as_deref(),
            Some("3")
        );
        assert_eq!(
            of("/user.slice/user-1000.slice/session-c12.scope", 1000).as_deref(),
            Some("c12")
        );
        for cg in [
            // A user unit naming itself after a session.
            "/user.slice/user-1000.slice/user@1000.service/app.slice/session-3.scope",
            "/user.slice/user-1000.slice/user@1000.service/session-3.scope",
            // Another user's session, and inside a session scope.
            "/user.slice/user-1001.slice/session-3.scope",
            "/user.slice/user-1000.slice/session-3.scope/sub",
            "/user.slice/user-1000.slice/session-.scope",
            "/user.slice/user-1000.slice/session-3.service",
            "/system.slice/session-3.scope",
            "/",
            "",
        ] {
            assert_eq!(of(cg, 1000), None, "{cg}");
        }
    }

    #[test]
    fn the_listener_is_known_by_the_kernel_not_by_what_it_says() {
        use std::os::unix::net::UnixListener;
        let path = std::env::temp_dir().join(format!("rime-input-peer-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let _l = UnixListener::bind(&path).expect("bind");
        let s = UnixStream::connect(&path).expect("connect");
        // SAFETY: getuid has no failure mode.
        let me = (unsafe { libc::getuid() }, std::process::id() as i32);
        assert_eq!(wayland::peer_creds(&s), Some(me));
        // And this test's own cgroup decides whether it is a session.
        // Without a unified hierarchy the placement is unknown: no session.
        match rime_agent_core::origin::read_cgroup(me.1) {
            Ok(cg) => assert_eq!(
                wayland::peer_session(&s).map(|(id, _)| id),
                wayland::session_of_cgroup(&cg, me.0),
                "{cg}"
            ),
            Err(_) => assert_eq!(wayland::peer_session(&s), None),
        }
        let _ = std::fs::remove_file(&path);
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

    /// Every request a scripted compositor got, as (object, opcode, body).
    type Requests = Arc<Mutex<Vec<(u32, u16, Vec<u8>)>>>;

    /// A scripted compositor on the other end of a socket pair: answers the
    /// registry with `globals`, every `sync` with `done`, and records every
    /// request as (object, opcode, body).
    fn compositor(globals: &'static [&'static str]) -> (UnixStream, Requests) {
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
    fn the_devices_are_made_and_warmed_up_apart_with_one_motion_there_and_back() {
        let (sock, seen) = compositor(ALL);
        let mut w =
            wayland::Wayland::over(sock, &wayland::Layout::from_names("us", "")).expect("connect");
        // Nothing reaches the pointer until the gate has said unlocked.
        let made = seen.lock().expect("log").len();
        assert_eq!(made, 10, "{:?}", seen.lock().expect("log"));
        w.warm_up().expect("warm up");
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
        // The daemon's choice of socket, against a real kernel: the
        // listener is labwc itself, and it is in a logind session exactly
        // when this test is (in-login-session.sh puts it in one).
        let probe = UnixStream::connect(&socket).expect("labwc");
        let labwc_pid = bed.kids.last().expect("labwc").id() as i32;
        assert_eq!(
            wayland::peer_creds(&probe).map(|(_, pid)| pid),
            Some(labwc_pid)
        );
        // SAFETY: getuid has no failure mode.
        let uid = unsafe { libc::getuid() };
        let own = rime_agent_core::origin::read_cgroup(std::process::id() as i32)
            .ok()
            .and_then(|cg| wayland::session_of_cgroup(&cg, uid));
        assert_eq!(
            wayland::peer_session(&probe),
            own.clone().map(|id| (id, labwc_pid))
        );
        eprintln!("labwc's session as the daemon reads it: {own:?}");
        drop(probe);

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
        let mut backend =
            wayland::Wayland::connect(&socket, &wayland::Layout::from_names("us", ""))
                .expect("virtual devices");
        backend.warm_up().expect("warm up");
        assert_eq!(
            backend.compositor_locked(),
            None,
            "labwc has no lock notifier"
        );
        assert!(
            !backend.offers_lock_notifier(),
            "labwc has no lock notifier"
        );
        // So the Shell's file is required: written here as the Shell would.
        let shell_file = rt.join("lock.json");
        write_shell_file(&shell_file, false);
        let mut session = Session::new(
            Box::new(backend),
            Gate {
                logind: Box::new(move || *h.lock().expect("h")),
                shell: Some(shell_file),
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
