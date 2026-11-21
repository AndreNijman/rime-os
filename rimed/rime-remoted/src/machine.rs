//! The machine itself, for a paired phone: its status card, the lock button,
//! and the media and volume controls (`docs/remote-live-contract.md` §2.4).
//!
//! Answered in this process and never forwarded: `rime-agentd` is about agent
//! sessions, and these are about the computer. None of them needs a grant,
//! because each is something the person could do at the keyboard, and none of
//! them can unlock anything. There is no unlock verb.
//!
//! ## Two backends, chosen once
//!
//! [`Real`] asks the machine (sysfs, rimed over `busctl`, `loginctl`,
//! `playerctl`, `wpctl`). [`Stub`] answers from a file and records what it
//! was asked to do. The choice is made once per process from
//! `RIME_REMOTED_MACHINE_STUB`, because every session of one user shares one
//! D-Bus user bus and one graphical session: a test daemon running as the
//! developer with the real backend would pause their music and lock their
//! screen. There is no third path a test could fall into.
//!
//! ## One budget per request
//!
//! The control worker answers requests in order, so a hung `wpctl` would
//! stall every request behind it. Every verb gets [`BUDGET`] for all it asks
//! the machine, the status reads run side by side, and a reader that runs out
//! of time gives `null`, not an error. The one exception is the lock, which
//! gets [`LOCK_BUDGET`]: its confirmation goes through hypridle, the Shell and
//! the compositor before logind's `LockedHint` says so, and a person pressed a
//! button and is waiting for exactly that answer.

use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The verbs this module answers. `serve::answered_here` routes by this list.
pub const VERBS: [&str; 5] = ["machine_status", "machine_lock", "media", "media_control", "volume"];

/// What `remote_hello` gains: one name per capability, always offered.
pub const FEATURES: [&str; 3] = ["machine_status", "machine_lock", "media"];

/// Everything one request may spend asking the machine.
pub const BUDGET: Duration = Duration::from_secs(2);

/// What `machine_lock` may spend, the request and its confirmation together.
pub const LOCK_BUDGET: Duration = Duration::from_secs(5);

/// The longest title or artist a reply carries, in characters.
pub const TEXT_CAP: usize = 200;

/// The environment variable that selects [`Stub`].
pub const STUB_ENV: &str = "RIME_REMOTED_MACHINE_STUB";

#[derive(Clone, Debug, PartialEq)]
pub struct Battery {
    pub percent: u8,
    pub state: String,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Status {
    pub battery: Option<Battery>,
    pub on_ac: Option<bool>,
    pub power_mode: Option<String>,
    pub locked: Option<bool>,
    pub update: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Player {
    pub name: String,
    pub status: String,
    pub title: String,
    pub artist: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Volume {
    pub percent: u8,
    pub muted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    PlayPause,
    Next,
    Previous,
}

impl Action {
    fn parse(s: &str) -> Option<Action> {
        match s {
            "play_pause" => Some(Action::PlayPause),
            "next" => Some(Action::Next),
            "previous" => Some(Action::Previous),
            _ => None,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Action::PlayPause => "play_pause",
            Action::Next => "next",
            Action::Previous => "previous",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VolumeChange {
    Percent(u8),
    Muted(bool),
}

/// Why a lock did not happen.
#[derive(Debug, PartialEq)]
pub enum LockError {
    /// No graphical session: there is no screen to lock.
    NoScreen,
    /// The lock was asked for and the tool refused, in its own words.
    Failed(String),
}

/// What the verbs ask of the machine. Every method finishes by `deadline`.
pub trait Backend: Send + Sync {
    fn status(&self, deadline: Instant) -> Status;
    /// Ask for the lock and return the lock state read back afterwards.
    fn lock(&self, deadline: Instant) -> Result<bool, LockError>;
    fn player(&self, deadline: Instant) -> Option<Player>;
    fn volume(&self, deadline: Instant) -> Option<Volume>;
    fn act(&self, action: Action, deadline: Instant) -> Result<(), String>;
    fn set_volume(&self, change: VolumeChange, deadline: Instant) -> Result<(), String>;
    /// Battery, AC and power mode, and nothing slower: what the presence
    /// refresh reads before it decides whether to dial (`presence.rs`).
    /// `locked` and `update` may be left `None`.
    fn power(&self, deadline: Instant) -> Status {
        self.status(deadline)
    }
    /// Whether the screen is locked, and nothing else: what sharing with
    /// another computer asks before each copy (`share.rs`). `None` when it
    /// cannot be told.
    fn locked(&self, deadline: Instant) -> Option<bool> {
        self.status(deadline).locked
    }
    /// How this computer is online: `wired`, `wifi` or `none`.
    fn network(&self) -> Option<String> {
        None
    }
    /// Seconds since boot.
    fn uptime_s(&self) -> Option<u64> {
        None
    }
}

/// The backend this process uses, chosen on first use from [`STUB_ENV`].
///
/// A unit-test build never gets [`Real`], whatever the environment says:
/// `cargo test` runs as the developer, in their session.
pub fn backend() -> &'static dyn Backend {
    static CHOSEN: OnceLock<Box<dyn Backend>> = OnceLock::new();
    CHOSEN
        .get_or_init(|| {
            if cfg!(test) {
                let dir = std::env::temp_dir().join(format!("rime-remoted-machine-unit-{}", std::process::id()));
                let _ = std::fs::create_dir_all(&dir);
                return Box::new(Stub::new(dir));
            }
            match std::env::var_os(STUB_ENV) {
                Some(dir) if !dir.is_empty() => Box::new(Stub::new(PathBuf::from(dir))),
                _ => Box::new(Real::new()),
            }
        })
        .as_ref()
}

/// Answer one of [`VERBS`].
pub fn control(backend: &dyn Backend, line: &[u8]) -> Vec<u8> {
    let deadline = Instant::now() + BUDGET;
    let v: Value = match serde_json::from_slice(line) {
        Ok(v) => v,
        Err(_) => return error("bad_request", "not a JSON object"),
    };
    let reply = match v.get("cmd").and_then(Value::as_str) {
        Some("machine_status") => status_reply(&backend.status(deadline)),
        Some("machine_lock") => match backend.lock(Instant::now() + LOCK_BUDGET) {
            Ok(locked) => json!({"reply": "machine_lock", "locked": locked}),
            Err(LockError::NoScreen) => return error("unavailable", "there is no screen to lock on this computer"),
            Err(LockError::Failed(why)) => return error("unavailable", &why),
        },
        Some("media") => media_reply(backend, deadline),
        Some("media_control") => {
            let Some(action) = v.get("action").and_then(Value::as_str).and_then(Action::parse) else {
                return error("bad_request", "action must be play_pause, next or previous");
            };
            if let Err(why) = backend.act(action, deadline) {
                return error("unavailable", &why);
            }
            media_reply(backend, deadline)
        }
        Some("volume") => {
            let change = match (v.get("percent"), v.get("muted")) {
                (Some(p), None) => match p.as_u64() {
                    Some(n) if n <= 100 => VolumeChange::Percent(n as u8),
                    _ => return error("bad_request", "percent must be a whole number from 0 to 100"),
                },
                (None, Some(m)) => match m.as_bool() {
                    Some(b) => VolumeChange::Muted(b),
                    None => return error("bad_request", "muted must be true or false"),
                },
                _ => return error("bad_request", "volume takes either percent or muted"),
            };
            if let Err(why) = backend.set_volume(change, deadline) {
                return error("unavailable", &why);
            }
            media_reply(backend, deadline)
        }
        _ => return error("bad_request", "not a machine verb"),
    };
    reply.to_string().into_bytes()
}

fn error(kind: &str, message: &str) -> Vec<u8> {
    json!({"reply": "error", "kind": kind, "message": message}).to_string().into_bytes()
}

fn status_reply(s: &Status) -> Value {
    json!({
        "reply": "machine_status",
        "battery": s.battery.as_ref().map(|b| json!({"percent": b.percent, "state": b.state})),
        "on_ac": s.on_ac,
        "power_mode": s.power_mode,
        "locked": s.locked,
        "update": s.update,
    })
}

fn media_reply(backend: &dyn Backend, deadline: Instant) -> Value {
    let (player, volume) = std::thread::scope(|scope| {
        let p = scope.spawn(|| backend.player(deadline));
        let v = backend.volume(deadline);
        (p.join().ok().flatten(), v)
    });
    json!({
        "reply": "media",
        "player": player.map(|p| json!({
            "name": cap(&p.name), "status": p.status, "title": cap(&p.title), "artist": cap(&p.artist),
        })),
        "volume": volume.map(|v| json!({"percent": v.percent, "muted": v.muted})),
    })
}

fn cap(s: &str) -> String {
    s.chars().take(TEXT_CAP).collect()
}

// ── The real machine ────────────────────────────────────────────────────────

/// Run a program and return its exit code and stdout, or `None` when it could
/// not start or did not finish by `deadline` (it is killed then).
pub fn run_until(program: &str, args: &[&str], deadline: Instant) -> Option<(Option<i32>, String, String)> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    // Read both pipes on their own threads, so a chatty child cannot block on
    // a full pipe while this waits for it to exit.
    let mut out = child.stdout.take()?;
    let mut err = child.stderr.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = out.read_to_end(&mut s);
        s
    });
    let ereader = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = err.read_to_end(&mut s);
        s
    });
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let stdout = String::from_utf8_lossy(&reader.join().ok()?).into_owned();
    let stderr = String::from_utf8_lossy(&ereader.join().ok()?).into_owned();
    Some((code, stdout, stderr))
}

/// The machine, through the tools Rime ships on both bootc and NixOS.
pub struct Real {
    power_supply: PathBuf,
    uid: u32,
}

impl Real {
    pub fn new() -> Real {
        // SAFETY: getuid cannot fail.
        let uid = unsafe { libc::getuid() };
        Real { power_supply: PathBuf::from("/sys/class/power_supply"), uid }
    }

    fn rimed(&self, interface: &str, deadline: Instant) -> Option<Value> {
        let (code, out, _) = run_until(
            "busctl",
            &["--system", "--json=short", "call", "org.rimeos.Rimed1", "/org/rimeos/Rimed1",
              "org.freedesktop.DBus.Properties", "GetAll", "s", interface],
            deadline,
        )?;
        if code != Some(0) {
            return None;
        }
        properties(&out)
    }

    /// The user's graphical session, as `loginctl show-user -p Display` names it.
    fn display(&self, deadline: Instant) -> Result<String, LockError> {
        let uid = self.uid.to_string();
        let Some((code, out, err)) = run_until("loginctl", &["show-user", &uid, "-p", "Display", "--value"], deadline) else {
            return Err(LockError::Failed("loginctl did not answer".into()));
        };
        rime_agent_core::lock::parse_display(code, &out, &err).map_err(|state| match state {
            rime_agent_core::lock::LockState::NoDisplaySession => LockError::NoScreen,
            other => LockError::Failed(other.reason().unwrap_or("the screen's session could not be found").to_string()),
        })
    }

    fn locked_hint(&self, session: &str, deadline: Instant) -> Option<bool> {
        let (code, out, err) = run_until("loginctl", &["show-session", session, "-p", "LockedHint", "--value"], deadline)?;
        match rime_agent_core::lock::parse_locked_hint(session, code, &out, &err) {
            rime_agent_core::lock::LockState::Locked => Some(true),
            rime_agent_core::lock::LockState::Unlocked => Some(false),
            _ => None,
        }
    }
}

impl Default for Real {
    fn default() -> Self {
        Real::new()
    }
}

impl Backend for Real {
    fn status(&self, deadline: Instant) -> Status {
        std::thread::scope(|scope| {
            let power = scope.spawn(|| self.rimed("org.rimeos.Rimed1.Power", deadline));
            let updates = scope.spawn(|| self.rimed("org.rimeos.Rimed1.Updates", deadline));
            let locked = scope.spawn(|| {
                let session = self.display(deadline).ok()?;
                self.locked_hint(&session, deadline)
            });
            let battery = battery_in(&self.power_supply);
            let power = power.join().ok().flatten();
            let updates = updates.join().ok().flatten();
            Status {
                battery,
                on_ac: power.as_ref().and_then(|p| p.get("OnAcPower")).and_then(Value::as_bool),
                power_mode: power.as_ref().and_then(|p| p.get("Tier")).and_then(Value::as_str).map(str::to_string),
                locked: locked.join().ok().flatten(),
                update: updates.as_ref().and_then(|u| u.get("State")).and_then(Value::as_str).map(str::to_string),
            }
        })
    }

    fn locked(&self, deadline: Instant) -> Option<bool> {
        let session = self.display(deadline).ok()?;
        self.locked_hint(&session, deadline)
    }

    fn power(&self, deadline: Instant) -> Status {
        let power = self.rimed("org.rimeos.Rimed1.Power", deadline);
        Status {
            battery: battery_in(&self.power_supply),
            on_ac: power.as_ref().and_then(|p| p.get("OnAcPower")).and_then(Value::as_bool),
            power_mode: power.as_ref().and_then(|p| p.get("Tier")).and_then(Value::as_str).map(str::to_string),
            locked: None,
            update: None,
        }
    }

    fn network(&self) -> Option<String> {
        let route = std::fs::read_to_string("/proc/net/route").unwrap_or_default();
        Some(network_kind(Path::new("/sys/class/net"), &route).to_string())
    }

    fn uptime_s(&self) -> Option<u64> {
        parse_uptime(&std::fs::read_to_string("/proc/uptime").ok()?)
    }

    fn lock(&self, deadline: Instant) -> Result<bool, LockError> {
        let session = self.display(deadline)?;
        match run_until("loginctl", &["lock-session", &session], deadline) {
            Some((Some(0), _, _)) => {}
            Some((_, _, err)) => return Err(LockError::Failed(first_line(&err, "loginctl refused to lock the screen"))),
            None => return Err(LockError::Failed("loginctl did not answer".into())),
        }
        // The Shell sets LockedHint once the compositor has confirmed the
        // lock, a moment after the request. Read it until it says locked or
        // the budget is spent; what is returned is what was read.
        loop {
            if self.locked_hint(&session, deadline) == Some(true) {
                return Ok(true);
            }
            if Instant::now() + Duration::from_millis(100) >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn player(&self, deadline: Instant) -> Option<Player> {
        // A phone's player (media.rs) is never the computer's own.
        let (code, out, _) = run_until(
            "playerctl",
            &[IGNORE_PHONES, "metadata", "--format", "{{playerName}}\u{1f}{{lc(status)}}\u{1f}{{xesam:title}}\u{1f}{{xesam:artist}}"],
            deadline,
        )?;
        if code != Some(0) {
            return None;
        }
        parse_player(&out)
    }

    fn volume(&self, deadline: Instant) -> Option<Volume> {
        let (code, out, _) = run_until("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"], deadline)?;
        if code != Some(0) {
            return None;
        }
        parse_volume(&out)
    }

    fn act(&self, action: Action, deadline: Instant) -> Result<(), String> {
        let verb = match action {
            Action::PlayPause => "play-pause",
            Action::Next => "next",
            Action::Previous => "previous",
        };
        match run_until("playerctl", &[IGNORE_PHONES, verb], deadline) {
            Some((Some(0), _, _)) => Ok(()),
            Some((_, _, err)) => Err(first_line(&err, "no media player is running")),
            None => Err("playerctl did not answer".into()),
        }
    }

    fn set_volume(&self, change: VolumeChange, deadline: Instant) -> Result<(), String> {
        let level;
        let args: Vec<&str> = match change {
            VolumeChange::Percent(p) => {
                level = format!("{:.2}", f64::from(p) / 100.0);
                vec!["set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", &level]
            }
            VolumeChange::Muted(m) => vec!["set-mute", "@DEFAULT_AUDIO_SINK@", if m { "1" } else { "0" }],
        };
        match run_until("wpctl", &args, deadline) {
            Some((Some(0), _, _)) => Ok(()),
            Some((_, _, err)) => Err(first_line(&err, "the volume could not be changed")),
            None => Err("wpctl did not answer".into()),
        }
    }
}

fn first_line(s: &str, otherwise: &str) -> String {
    s.lines().map(str::trim).find(|l| !l.is_empty()).map(cap).unwrap_or_else(|| otherwise.to_string())
}

/// `busctl --json=short call … GetAll` → `{name: value}`.
pub fn properties(out: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(out.trim()).ok()?;
    let all = v.get("data")?.get(0)?.as_object()?;
    Some(Value::Object(all.iter().filter_map(|(k, p)| Some((k.clone(), p.get("data")?.clone()))).collect()))
}

/// The first battery under `/sys/class/power_supply`, or `None` without one.
pub fn battery_in(dir: &Path) -> Option<Battery> {
    let mut names: Vec<_> = std::fs::read_dir(dir).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    names.sort();
    names.into_iter().find_map(|p| {
        let read = |f: &str| std::fs::read_to_string(p.join(f)).ok().map(|s| s.trim().to_string());
        if read("type")? != "Battery" || read("present").is_some_and(|v| v == "0") {
            return None;
        }
        let percent = read("capacity")?.parse::<u8>().ok()?.min(100);
        let state = read("status").unwrap_or_else(|| "Unknown".into()).to_lowercase();
        Some(Battery { percent, state })
    })
}

/// `/proc/uptime` → whole seconds since boot.
pub fn parse_uptime(text: &str) -> Option<u64> {
    let secs: f64 = text.split_whitespace().next()?.parse().ok()?;
    (secs.is_finite() && secs >= 0.0).then_some(secs as u64)
}

/// How this computer is online, from sysfs (`/sys/class/net`) and the IPv4
/// routing table (`/proc/net/route`): the interface the default route uses
/// when it is a real device, else any real device that is up, wired before
/// Wi-Fi. A default route through a tunnel (a VPN) says nothing about the
/// link under it, so tunnels and other virtual interfaces are never the
/// answer. `none` when no real device is up.
pub fn network_kind(class_net: &Path, route: &str) -> &'static str {
    let real = |name: &str| class_net.join(name).join("device").exists();
    let wifi = |name: &str| class_net.join(name).join("wireless").exists() || class_net.join(name).join("phy80211").exists();
    let up = |name: &str| {
        std::fs::read_to_string(class_net.join(name).join("operstate")).is_ok_and(|s| matches!(s.trim(), "up" | "unknown"))
    };
    let kind = |name: &str| if wifi(name) { "wifi" } else { "wired" };
    // Default routes, lowest metric first.
    let mut defaults: Vec<(u64, &str)> = route
        .lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let flags = u32::from_str_radix(f.get(3)?, 16).ok()?;
            if f.get(1) != Some(&"00000000") || f.get(7) != Some(&"00000000") || flags & 1 == 0 {
                return None;
            }
            Some((f.get(6)?.parse::<u64>().ok()?, f[0]))
        })
        .collect();
    defaults.sort();
    if let Some((_, name)) = defaults.iter().find(|(_, n)| real(n) && up(n)) {
        return kind(name);
    }
    let mut names: Vec<String> = std::fs::read_dir(class_net)
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.sort();
    let live: Vec<&String> = names.iter().filter(|n| n.as_str() != "lo" && real(n) && up(n)).collect();
    if live.iter().any(|n| !wifi(n)) {
        "wired"
    } else if !live.is_empty() {
        "wifi"
    } else {
        "none"
    }
}

/// `wpctl get-volume` → "Volume: 0.40" or "Volume: 0.40 [MUTED]".
pub fn parse_volume(out: &str) -> Option<Volume> {
    let rest = out.trim().strip_prefix("Volume:")?.trim();
    let level: f64 = rest.split_whitespace().next()?.parse().ok()?;
    Some(Volume { percent: (level * 100.0).round().clamp(0.0, 100.0) as u8, muted: rest.contains("[MUTED]") })
}

/// Every `playerctl` call leaves the phones' players out: the `media` verb
/// describes and controls this computer, and a phone's player shown here is
/// the phone's (`media.rs`).
pub const IGNORE_PHONES: &str = "--ignore-player=rime_phone";

/// The `playerctl metadata --format` line [`Real::player`] asks for.
pub fn parse_player(out: &str) -> Option<Player> {
    let line = out.strip_suffix('\n').unwrap_or(out);
    let mut f = line.split('\u{1f}');
    let name = f.next()?.trim().to_string();
    let status = f.next()?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    Some(Player { name, status, title: f.next().unwrap_or("").to_string(), artist: f.next().unwrap_or("").to_string() })
}

// ── The stub ────────────────────────────────────────────────────────────────

/// A machine made of one file, for tests.
///
/// `<dir>/state.json` holds `{"machine_status": {…}, "media": {…}}` in the
/// replies' own shapes (missing means `null`; `"no_screen": true` inside
/// `machine_status` makes a lock refuse). Every action is appended to
/// `<dir>/actions.jsonl` and applied to the file, so a read after a change
/// sees it, as it would on a machine.
pub struct Stub {
    dir: PathBuf,
    lock: std::sync::Mutex<()>,
}

impl Stub {
    pub fn new(dir: PathBuf) -> Stub {
        Stub { dir, lock: std::sync::Mutex::new(()) }
    }

    fn state(&self) -> Value {
        std::fs::read(self.dir.join("state.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_else(|| json!({}))
    }

    fn change(&self, record: Value, apply: impl FnOnce(&mut Value)) {
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.state();
        apply(&mut state);
        let _ = std::fs::write(self.dir.join("state.json"), state.to_string());
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.dir.join("actions.jsonl")) {
            let _ = writeln!(f, "{record}");
        }
    }
}

impl Backend for Stub {
    fn status(&self, _: Instant) -> Status {
        let s = self.state();
        let m = &s["machine_status"];
        Status {
            battery: m.get("battery").and_then(|b| {
                Some(Battery { percent: b["percent"].as_u64()?.min(100) as u8, state: b["state"].as_str()?.to_string() })
            }),
            on_ac: m["on_ac"].as_bool(),
            power_mode: m["power_mode"].as_str().map(str::to_string),
            locked: m["locked"].as_bool(),
            update: m["update"].as_str().map(str::to_string),
        }
    }

    fn network(&self) -> Option<String> {
        self.state()["machine_status"]["network"].as_str().map(str::to_string)
    }

    fn uptime_s(&self) -> Option<u64> {
        self.state()["machine_status"]["uptime_s"].as_u64()
    }

    fn lock(&self, _: Instant) -> Result<bool, LockError> {
        if self.state()["machine_status"]["no_screen"].as_bool() == Some(true) {
            return Err(LockError::NoScreen);
        }
        self.change(json!({"action": "lock"}), |s| s["machine_status"]["locked"] = json!(true));
        Ok(true)
    }

    fn player(&self, _: Instant) -> Option<Player> {
        let p = self.state()["media"]["player"].clone();
        Some(Player {
            name: p["name"].as_str()?.to_string(),
            status: p["status"].as_str().unwrap_or("stopped").to_string(),
            title: p["title"].as_str().unwrap_or("").to_string(),
            artist: p["artist"].as_str().unwrap_or("").to_string(),
        })
    }

    fn volume(&self, _: Instant) -> Option<Volume> {
        let v = self.state()["media"]["volume"].clone();
        Some(Volume { percent: v["percent"].as_u64()?.min(100) as u8, muted: v["muted"].as_bool().unwrap_or(false) })
    }

    fn act(&self, action: Action, _: Instant) -> Result<(), String> {
        if self.state()["media"]["player"].is_null() {
            return Err("No players found".into());
        }
        self.change(json!({"action": action.word()}), |s| {
            if action == Action::PlayPause {
                let playing = s["media"]["player"]["status"] == "playing";
                s["media"]["player"]["status"] = json!(if playing { "paused" } else { "playing" });
            }
        });
        Ok(())
    }

    fn set_volume(&self, change: VolumeChange, _: Instant) -> Result<(), String> {
        match change {
            VolumeChange::Percent(p) => self.change(json!({"action": "volume", "percent": p}), |s| {
                s["media"]["volume"]["percent"] = json!(p)
            }),
            VolumeChange::Muted(m) => self.change(json!({"action": "volume", "muted": m}), |s| {
                s["media"]["volume"]["muted"] = json!(m)
            }),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(b: &dyn Backend, line: &str) -> Value {
        serde_json::from_slice(&control(b, line.as_bytes())).expect("json reply")
    }

    fn stub(state: Value) -> (tempdir::Dir, Stub) {
        let d = tempdir::Dir::new();
        std::fs::write(d.path().join("state.json"), state.to_string()).unwrap();
        let s = Stub::new(d.path().to_path_buf());
        (d, s)
    }

    fn actions(d: &tempdir::Dir) -> Vec<Value> {
        std::fs::read_to_string(d.path().join("actions.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// A small temporary directory, removed on drop, without a new dependency.
    mod tempdir {
        pub struct Dir(std::path::PathBuf);
        impl Dir {
            pub fn new() -> Dir {
                use std::sync::atomic::{AtomicU32, Ordering};
                static N: AtomicU32 = AtomicU32::new(0);
                let p = std::env::temp_dir().join(format!(
                    "rime-remoted-machine-{}-{}",
                    std::process::id(),
                    N.fetch_add(1, Ordering::SeqCst)
                ));
                std::fs::create_dir_all(&p).unwrap();
                Dir(p)
            }
            pub fn path(&self) -> &std::path::Path {
                &self.0
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn the_status_card_says_what_the_machine_said_and_null_for_what_it_did_not() {
        let (_d, s) = stub(json!({"machine_status": {
            "battery": {"percent": 81, "state": "charging"}, "on_ac": true, "power_mode": "balanced",
            "locked": false, "update": "available"}}));
        assert_eq!(
            ask(&s, r#"{"cmd":"machine_status"}"#),
            json!({"reply": "machine_status", "battery": {"percent": 81, "state": "charging"}, "on_ac": true,
                   "power_mode": "balanced", "locked": false, "update": "available"})
        );
        let (_d, empty) = stub(json!({}));
        assert_eq!(
            ask(&empty, r#"{"cmd":"machine_status"}"#),
            json!({"reply": "machine_status", "battery": null, "on_ac": null, "power_mode": null, "locked": null, "update": null})
        );
    }

    #[test]
    fn lock_is_asked_for_and_read_back_and_a_machine_without_a_screen_says_so() {
        let (d, s) = stub(json!({"machine_status": {"locked": false}}));
        assert_eq!(ask(&s, r#"{"cmd":"machine_lock"}"#), json!({"reply": "machine_lock", "locked": true}));
        assert_eq!(actions(&d), vec![json!({"action": "lock"})]);
        assert_eq!(ask(&s, r#"{"cmd":"machine_status"}"#)["locked"], json!(true));

        let (d, s) = stub(json!({"machine_status": {"no_screen": true}}));
        let r = ask(&s, r#"{"cmd":"machine_lock"}"#);
        assert_eq!(r["kind"], "unavailable");
        assert_eq!(r["message"], "there is no screen to lock on this computer");
        assert!(actions(&d).is_empty());
    }

    #[test]
    fn uptime_and_the_network_kind_are_read_from_proc_and_sysfs() {
        assert_eq!(parse_uptime("12345.67 99999.00\n"), Some(12345));
        assert_eq!(parse_uptime("garbage"), None);
        assert_eq!(parse_uptime("-5 1"), None);

        let d = tempdir::Dir::new();
        let net = d.path();
        let iface = |name: &str, real: bool, wifi: bool, state: &str| {
            let dir = net.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            if real {
                std::fs::create_dir_all(dir.join("device")).unwrap();
            }
            if wifi {
                std::fs::create_dir_all(dir.join("wireless")).unwrap();
            }
            std::fs::write(dir.join("operstate"), format!("{state}\n")).unwrap();
        };
        let head = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT\n";
        let route = |rows: &[(&str, u32)]| {
            let mut t = head.to_string();
            for (n, metric) in rows {
                t.push_str(&format!("{n}\t00000000\t0101A8C0\t0003\t0\t0\t{metric}\t00000000\t0\t0\t0\n"));
            }
            t
        };
        assert_eq!(network_kind(net, ""), "none", "nothing at all");
        iface("lo", false, false, "unknown");
        iface("wg0", false, false, "unknown");
        iface("wlan0", true, true, "up");
        iface("eth0", true, false, "down");
        // A VPN carries the default route: the Wi-Fi under it is the answer.
        assert_eq!(network_kind(net, &route(&[("wg0", 0), ("wlan0", 600)])), "wifi");
        assert_eq!(network_kind(net, &route(&[("wg0", 0)])), "wifi");
        iface("eth0", true, false, "up");
        // The default route's real device wins over another that is up.
        assert_eq!(network_kind(net, &route(&[("wlan0", 100), ("eth0", 600)])), "wifi");
        assert_eq!(network_kind(net, &route(&[("eth0", 100), ("wlan0", 600)])), "wired");
        // No route: wired before Wi-Fi.
        assert_eq!(network_kind(net, head), "wired");
        iface("eth0", true, false, "down");
        iface("wlan0", true, true, "down");
        assert_eq!(network_kind(net, &route(&[("wg0", 0)])), "none");
    }

    #[test]
    fn there_is_no_unlock() {
        let (_d, s) = stub(json!({}));
        for line in [r#"{"cmd":"machine_unlock"}"#, r#"{"cmd":"machine_lock","locked":false}"#] {
            let r = ask(&s, line);
            assert_ne!(r["locked"], json!(false), "{line} unlocked something: {r}");
        }
        assert!(!VERBS.iter().any(|v| v.contains("unlock")));
    }

    #[test]
    fn media_actions_and_volume_answer_with_what_is_true_afterwards() {
        let (d, s) = stub(json!({"media": {
            "player": {"name": "spotify", "status": "playing", "title": "Song", "artist": "Band"},
            "volume": {"percent": 40, "muted": false}}}));
        assert_eq!(
            ask(&s, r#"{"cmd":"media"}"#),
            json!({"reply": "media", "player": {"name": "spotify", "status": "playing", "title": "Song", "artist": "Band"},
                   "volume": {"percent": 40, "muted": false}})
        );
        assert_eq!(ask(&s, r#"{"cmd":"media_control","action":"play_pause"}"#)["player"]["status"], "paused");
        assert_eq!(ask(&s, r#"{"cmd":"media_control","action":"next"}"#)["reply"], "media");
        assert_eq!(ask(&s, r#"{"cmd":"volume","percent":55}"#)["volume"], json!({"percent": 55, "muted": false}));
        assert_eq!(ask(&s, r#"{"cmd":"volume","muted":true}"#)["volume"], json!({"percent": 55, "muted": true}));
        assert_eq!(
            actions(&d),
            vec![json!({"action": "play_pause"}), json!({"action": "next"}),
                 json!({"action": "volume", "percent": 55}), json!({"action": "volume", "muted": true})]
        );
    }

    #[test]
    fn a_bad_line_is_refused_before_anything_is_touched() {
        let (d, s) = stub(json!({"media": {"player": {"name": "x", "status": "playing"}, "volume": {"percent": 1}}}));
        for line in [
            r#"{"cmd":"media_control","action":"stop_everything"}"#,
            r#"{"cmd":"media_control"}"#,
            r#"{"cmd":"volume","percent":101}"#,
            r#"{"cmd":"volume","percent":-1}"#,
            r#"{"cmd":"volume","percent":50.5}"#,
            r#"{"cmd":"volume","muted":"yes"}"#,
            r#"{"cmd":"volume"}"#,
            r#"{"cmd":"volume","percent":5,"muted":true}"#,
            "not json",
        ] {
            assert_eq!(ask(&s, line)["kind"], "bad_request", "{line}");
        }
        assert!(actions(&d).is_empty(), "a refused line changed the machine: {:?}", actions(&d));
    }

    #[test]
    fn no_player_is_null_and_acting_on_none_says_so() {
        let (_d, s) = stub(json!({"media": {"volume": {"percent": 30, "muted": false}}}));
        assert_eq!(ask(&s, r#"{"cmd":"media"}"#)["player"], Value::Null);
        let r = ask(&s, r#"{"cmd":"media_control","action":"next"}"#);
        assert_eq!(r["kind"], "unavailable");
        assert_eq!(r["message"], "No players found");
    }

    #[test]
    fn long_titles_are_cut_to_the_cap() {
        let long = "é".repeat(TEXT_CAP + 50);
        let (_d, s) = stub(json!({"media": {"player": {"name": "mpv", "status": "playing", "title": long, "artist": ""}}}));
        let r = ask(&s, r#"{"cmd":"media"}"#);
        assert_eq!(r["player"]["title"].as_str().unwrap().chars().count(), TEXT_CAP);
    }

    #[test]
    fn busctl_wpctl_playerctl_and_sysfs_are_read_as_they_print() {
        let out = r#"{"type":"a{sv}","data":[{"Tier":{"type":"s","data":"balanced"},"OnAcPower":{"type":"b","data":true}}]}"#;
        assert_eq!(properties(out).unwrap(), json!({"Tier": "balanced", "OnAcPower": true}));
        assert_eq!(properties("not json"), None);

        assert_eq!(parse_volume("Volume: 0.40\n"), Some(Volume { percent: 40, muted: false }));
        assert_eq!(parse_volume("Volume: 0.07 [MUTED]\n"), Some(Volume { percent: 7, muted: true }));
        assert_eq!(parse_volume("Volume: 1.53\n"), Some(Volume { percent: 100, muted: false }));
        assert_eq!(parse_volume("wpctl: no default sink"), None);

        let p = parse_player("spotify\u{1f}playing\u{1f}A\tTitle\u{1f}An Artist\n").unwrap();
        assert_eq!(p, Player { name: "spotify".into(), status: "playing".into(), title: "A\tTitle".into(), artist: "An Artist".into() });
        assert_eq!(parse_player("\u{1f}\u{1f}\u{1f}\n"), None);

        let d = tempdir::Dir::new();
        let ac = d.path().join("AC");
        std::fs::create_dir(&ac).unwrap();
        std::fs::write(ac.join("type"), "Mains\n").unwrap();
        assert_eq!(battery_in(d.path()), None, "a machine with only a charger has no battery");
        let bat = d.path().join("BAT0");
        std::fs::create_dir(&bat).unwrap();
        std::fs::write(bat.join("type"), "Battery\n").unwrap();
        std::fs::write(bat.join("capacity"), "81\n").unwrap();
        std::fs::write(bat.join("status"), "Not charging\n").unwrap();
        assert_eq!(battery_in(d.path()), Some(Battery { percent: 81, state: "not charging".into() }));
    }

    #[test]
    fn a_tool_that_hangs_is_killed_at_the_deadline() {
        let start = Instant::now();
        assert_eq!(run_until("sleep", &["30"], start + Duration::from_millis(300)), None);
        assert!(start.elapsed() < Duration::from_secs(2), "took {:?}", start.elapsed());
        let (code, out, _) = run_until("sh", &["-c", "echo hi; exit 3"], Instant::now() + BUDGET).unwrap();
        assert_eq!((code, out.as_str()), (Some(3), "hi\n"));
        assert_eq!(run_until("/nonexistent/program", &[], Instant::now() + BUDGET), None);
    }

    /// A backend whose every read takes longer than the budget.
    struct Slow;
    impl Backend for Slow {
        fn status(&self, deadline: Instant) -> Status {
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            Status::default()
        }
        fn lock(&self, _: Instant) -> Result<bool, LockError> {
            Ok(false)
        }
        fn player(&self, deadline: Instant) -> Option<Player> {
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            None
        }
        fn volume(&self, deadline: Instant) -> Option<Volume> {
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            None
        }
        fn act(&self, _: Action, _: Instant) -> Result<(), String> {
            Ok(())
        }
        fn set_volume(&self, _: VolumeChange, _: Instant) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn the_media_reads_run_side_by_side_inside_one_budget() {
        let start = Instant::now();
        let r = ask(&Slow, r#"{"cmd":"media"}"#);
        assert_eq!(r, json!({"reply": "media", "player": null, "volume": null}));
        assert!(start.elapsed() < BUDGET + Duration::from_millis(500), "took {:?}", start.elapsed());
        // An action spends from the same budget as the read that follows it.
        let start = Instant::now();
        ask(&Slow, r#"{"cmd":"volume","percent":3}"#);
        assert!(start.elapsed() < BUDGET + Duration::from_millis(500), "took {:?}", start.elapsed());
    }

    #[test]
    fn a_line_this_module_does_not_take_is_refused() {
        let (_d, s) = stub(json!({}));
        assert_eq!(ask(&s, r#"{"cmd":"hello"}"#)["kind"], "bad_request");
    }

    /// The real backend's READ half against the machine running the test,
    /// compared with what the tools say when asked directly. Never the lock,
    /// never a media action, never a volume change. Run by hand on a Rime
    /// machine: `cargo test -p rime-remoted -- --ignored the_real_backend`.
    #[test]
    #[ignore = "reads this machine's real status, player and volume"]
    fn the_real_backend_reads_this_machine_as_its_tools_do() {
        fn raw(program: &str, args: &[&str]) -> Option<String> {
            let out = std::process::Command::new(program).args(args).output().ok()?;
            out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
        }
        let real = Real::new();
        let status = real.status(Instant::now() + BUDGET);
        eprintln!("status: {status:?}");

        let battery_dir = Path::new("/sys/class/power_supply");
        assert_eq!(status.battery, battery_in(battery_dir), "the battery is read from sysfs directly");

        match raw("busctl", &["--system", "get-property", "org.rimeos.Rimed1", "/org/rimeos/Rimed1", "org.rimeos.Rimed1.Power", "Tier"]) {
            Some(tier) => {
                let tier = tier.trim().strip_prefix("s ").map(|t| t.trim_matches('"').to_string());
                assert_eq!(status.power_mode, tier, "rimed's tier, as busctl reads it");
                assert!(status.on_ac.is_some(), "rimed answers, so OnAcPower is known");
            }
            None => assert_eq!(status.power_mode, None, "rimed does not answer, so the tier is unknown"),
        }
        match raw("busctl", &["--system", "get-property", "org.rimeos.Rimed1", "/org/rimeos/Rimed1", "org.rimeos.Rimed1.Updates", "State"]) {
            Some(state) => assert_eq!(status.update, state.trim().strip_prefix("s ").map(|t| t.trim_matches('"').to_string())),
            None => assert_eq!(status.update, None),
        }
        let uid = unsafe { libc::getuid() }.to_string();
        let display = raw("loginctl", &["show-user", &uid, "-p", "Display", "--value"]).map(|d| d.trim().to_string());
        if display.as_deref().is_some_and(|d| !d.is_empty()) {
            assert!(status.locked.is_some(), "a graphical session exists, so its LockedHint is known");
        }

        let player = real.player(Instant::now() + BUDGET);
        eprintln!("player: {player:?}");
        let direct = raw("playerctl", &[IGNORE_PHONES, "metadata", "--format", "{{playerName}}\u{1f}{{lc(status)}}\u{1f}{{xesam:title}}\u{1f}{{xesam:artist}}"])
            .and_then(|o| parse_player(&o));
        assert_eq!(player.map(|p| p.name), direct.map(|p| p.name), "the same player as playerctl picks");

        let volume = real.volume(Instant::now() + BUDGET);
        eprintln!("volume: {volume:?}");
        let direct = raw("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"]).and_then(|o| parse_volume(&o));
        assert_eq!(volume, direct, "the same volume as wpctl reads");
        if let Some(v) = &volume {
            assert!(v.percent <= 100);
        }
    }
}
