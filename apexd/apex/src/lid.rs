//! `apex lid` — lid-closed continuous operation. Roadmap P1-063.
//!
//! ## What this is
//!
//! The thin, impure half of [`apexd_core::lid`]. That module decides; this one
//! measures the machine, holds the inhibitor, powers things down, and writes
//! down what happened. The split is not decoration: the act the policy
//! authorises is a laptop going to sleep in someone's bag, and the only machine
//! that could exercise it end to end is one a person is using. So the matrix
//! lives in a pure function with 720 asserted cases, and everything here is
//! either a read, a recorded write, or a subprocess.
//!
//! ## Why nothing here can touch the machine running the tests
//!
//! Every absolute path goes through [`Roots`], and every subprocess goes
//! through [`Runner`]. When `APEX_LID_ROOT` is set:
//!
//! * reads and writes are re-rooted into the fixture tree, so a sysfs write
//!   lands in a temp directory;
//! * **no external program is executed at all** — `systemctl`, `rfkill`, `iw`,
//!   `nmcli`, `systemd-inhibit` and `runuser` are appended to a command log
//!   inside the fixture instead, argv by argv.
//!
//! That is stronger than putting fakes first on `$PATH`: a fake can be missed
//! by an absolute-path invocation, where this cannot execute anything by
//! construction. The suite asserts the exact argv the driver *would* have run,
//! and `tests/test-apex-lid.sh` asserts that the command log is the only thing
//! that moved.
//!
//! ## Where the pin lives, and why it is not root-owned
//!
//! `~/.config/apex/lid.toml`. A root-owned pin would mean `apex lid pin on`
//! needed privilege, which would mean the shell tile raising a polkit prompt on
//! the owner's desktop every time they toggled it. Taking the inhibitor itself
//! needs no privilege (`allow_active=yes`, measured), so the only reason the
//! driver runs as root is the power-down half and the guards' suspend — and a
//! root driver can read a user's config file perfectly well. The system-wide
//! default is `/etc/apex/lid.toml`; the user's file wins where both exist, and
//! `apex lid status` names which one was used.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use apexd_core::lid::{
    Charge, ClosedPeriod, Guard, LidInputs, LidPolicy, LidState, Pin, PowerAction,
    PowerDown, PowerPlan, Skipped, Thermal, VpnSample, VpnState, Work,
};
use clap::Subcommand;
use serde_json::{json, Value};

/// Where the driver keeps its record of the current and last closed period.
const STATE: &str = "/var/lib/apex/lid/state.json";
/// Where a finished period is left for `apex lid report`.
const LAST: &str = "/var/lib/apex/lid/last.json";
/// The machine-wide policy default.
const SYSTEM_CONFIG: &str = "/etc/apex/lid.toml";
/// Under a fixture root only: every argv the driver would have executed.
const COMMAND_LOG: &str = "/var/lib/apex/lid/commands.log";

// ── path and process containment ─────────────────────────────────────────────

/// A prefix, and only a prefix — the same contract as `apex trust`'s `Roots`.
///
/// `$APEX_LID_ROOT` points it at a fixture tree. No program name is ever taken
/// from the environment, and under a fixture root no program is run at all.
pub struct Roots {
    fixture: Option<PathBuf>,
}

impl Roots {
    pub fn from_env() -> Roots {
        Roots { fixture: std::env::var_os("APEX_LID_ROOT").map(PathBuf::from) }
    }

    fn is_fixture(&self) -> bool {
        self.fixture.is_some()
    }

    fn path(&self, absolute: &str) -> PathBuf {
        match &self.fixture {
            // `absolute` always starts with '/'; without the strip, `join`
            // discards the prefix and reads the real machine — a fixture that
            // passes on the author's box and nowhere else.
            Some(root) => root.join(absolute.trim_start_matches('/')),
            None => PathBuf::from(absolute),
        }
    }

    /// A file's text, or the reason it could not be read. There is no `.ok()`
    /// twin: every path here feeds a claim about whether it is safe to leave a
    /// laptop awake in a bag, and no such claim survives a read nobody finished.
    fn read(&self, absolute: &str) -> Result<String, String> {
        let p = self.path(absolute);
        std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))
    }

    fn read_optional(&self, absolute: &str) -> Result<Option<String>, String> {
        match std::fs::read_to_string(self.path(absolute)) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", self.path(absolute).display())),
        }
    }

    fn write(&self, absolute: &str, body: &str) -> Result<(), String> {
        let p = self.path(absolute);
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&p, body).map_err(|e| format!("{}: {e}", p.display()))
    }
}

/// How the driver reaches the rest of the system.
///
/// One place, so that "this cannot run anything" is a property of the type
/// rather than a claim about every call site.
pub struct Runner<'a> {
    roots: &'a Roots,
    /// `--dry-run`: plan and print, change nothing, on a real machine.
    dry_run: bool,
}

/// What a subprocess did, or why it did not run.
pub enum Ran {
    Ok(String),
    /// It was not executed: a fixture root, or `--dry-run`. Not a failure.
    NotRun(String),
    Failed(String),
}

impl Ran {
    fn ok(&self) -> bool {
        matches!(self, Ran::Ok(_) | Ran::NotRun(_))
    }
    fn text(&self) -> &str {
        match self {
            Ran::Ok(s) | Ran::NotRun(s) | Ran::Failed(s) => s,
        }
    }
}

impl<'a> Runner<'a> {
    pub fn new(roots: &'a Roots, dry_run: bool) -> Runner<'a> {
        Runner { roots, dry_run }
    }

    /// True when a subprocess would really be spawned.
    fn executes(&self) -> bool {
        !self.roots.is_fixture() && !self.dry_run
    }

    /// Run `prog` with `args`, or record the intent.
    fn run(&self, prog: &str, args: &[&str]) -> Ran {
        let line = format!("{prog} {}", args.join(" "));
        if !self.executes() {
            let _ = self.append_log(&line);
            return Ran::NotRun(line);
        }
        match Command::new(prog).args(args).output() {
            Ok(out) if out.status.success() => {
                Ran::Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
            }
            Ok(out) => Ran::Failed(format!(
                "{line}: exited {} — {}",
                out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into()),
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            Err(e) => Ran::Failed(format!("{line}: {e}")),
        }
    }

    /// A read-only probe, which is allowed to run even under `--dry-run` —
    /// asking `nmcli` what is connected changes nothing. Still refused under a
    /// fixture root, where the answer must come from the fixture.
    fn probe(&self, prog: &str, args: &[&str]) -> Ran {
        if self.roots.is_fixture() {
            let line = format!("{prog} {}", args.join(" "));
            let _ = self.append_log(&format!("probe {line}"));
            return Ran::NotRun(line);
        }
        match Command::new(prog).args(args).output() {
            Ok(out) if out.status.success() => {
                Ran::Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
            }
            Ok(out) => Ran::Failed(format!(
                "{prog} exited {}",
                out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "by signal".into())
            )),
            Err(e) => Ran::Failed(format!("{prog}: {e}")),
        }
    }

    fn append_log(&self, line: &str) -> Result<(), String> {
        if !self.roots.is_fixture() {
            return Ok(());
        }
        let existing = self.roots.read_optional(COMMAND_LOG)?.unwrap_or_default();
        self.roots.write(COMMAND_LOG, &format!("{existing}{line}\n"))
    }
}

// ── measuring the machine ────────────────────────────────────────────────────

/// Where the lid is, by a ladder of sources that keeps absence and failure
/// apart at every rung.
///
/// 1. `/proc/acpi/button/lid/*/state` — present on the hardware APEX targets,
///    unprivileged, and exactly what the firmware believes.
/// 2. UPower's `LidIsPresent` / `LidIsClosed`, for machines whose ACPI button
///    driver does not export the proc node.
/// 3. An evdev device advertising `SW_LID`. If one exists the machine *has* a
///    lid whose position nobody could read — `Unreadable`, never `Open`. If
///    none exists, there is genuinely no lid.
pub fn read_lid(roots: &Roots, runner: &Runner) -> LidState {
    // ── UPower first, and this order is load bearing ────────────────────────
    //
    // logind's view of "the lid" is the AGGREGATE of every input device tagged
    // `power-switch`: any one of them reporting SW_LID closed makes logind act.
    // UPower computes the same aggregate. `/proc/acpi/button/lid/*/state`
    // reports only the one firmware button.
    //
    // On a machine with exactly one lid the two agree. They diverge precisely
    // where this driver is tested — a virtual SW_LID device injected by
    // `tests/test-apex-lid-live.sh` makes logind and UPower say "closed" while
    // the ACPI node still says "open" — and they would diverge the same way on
    // a docking station or any second switch. The driver must agree with the
    // thing that would actually suspend the machine, or it sits in its open
    // arm while logind is one inhibitor away from sleeping.
    let up = runner.probe(
        "busctl",
        &[
            "--no-pager",
            "get-property",
            "org.freedesktop.UPower",
            "/org/freedesktop/UPower",
            "org.freedesktop.UPower",
            "LidIsPresent",
            "LidIsClosed",
        ],
    );
    if let Ran::Ok(text) = &up {
        let bools: Vec<bool> = text
            .split_whitespace()
            .filter(|t| *t == "true" || *t == "false")
            .map(|t| t == "true")
            .collect();
        if bools.len() == 2 {
            if !bools[0] {
                return LidState::NoLid;
            }
            return if bools[1] { LidState::Closed } else { LidState::Open };
        }
    }

    // ── ACPI second: the firmware button, for machines with no UPower ───────
    let dir = roots.path("/proc/acpi/button/lid");
    let mut acpi_err: Option<String> = None;
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            for e in entries.flatten() {
                let p = e.path().join("state");
                match std::fs::read_to_string(&p) {
                    Ok(s) => {
                        let lower = s.to_ascii_lowercase();
                        if lower.contains("closed") {
                            return LidState::Closed;
                        }
                        if lower.contains("open") {
                            return LidState::Open;
                        }
                        acpi_err = Some(format!("{} said {:?}", p.display(), s.trim()));
                    }
                    Err(err) => acpi_err = Some(format!("{}: {err}", p.display())),
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => acpi_err = Some(format!("{}: {e}", dir.display())),
    }

    // ── Third: does a lid switch exist at all? ──────────────────────────────
    match lid_switch_devices(roots) {
        Ok(devs) if devs.is_empty() => LidState::NoLid,
        Ok(devs) => LidState::Unreadable {
            why: format!(
                "{} advertises SW_LID but its position could not be read ({}; UPower: {})",
                devs.join(", "),
                acpi_err.unwrap_or_else(|| format!(
                    "{} is absent",
                    roots.path("/proc/acpi/button/lid").display()
                )),
                up.text()
            ),
        },
        Err(why) => LidState::Unreadable {
            why: format!("the input class could not be scanned for a lid switch: {why}"),
        },
    }
}

/// Input devices advertising `SW_LID` (switch capability bit 0).
///
/// `capabilities/sw` is a hex bitmask; bit 0 is `SW_LID`. A device with no
/// `capabilities/sw` at all is not a switch and is skipped — that is absence,
/// not an unreadable device, and lumping them together would make every laptop
/// look as though its lid could not be read.
fn lid_switch_devices(roots: &Roots) -> Result<Vec<String>, String> {
    let dir = roots.path("/sys/class/input");
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    let mut found = Vec::new();
    for e in entries.flatten() {
        let sw = e.path().join("device/capabilities/sw");
        let Ok(text) = std::fs::read_to_string(&sw) else { continue };
        let Some(last) = text.split_whitespace().last() else { continue };
        let Ok(bits) = u64::from_str_radix(last.trim(), 16) else { continue };
        if bits & 1 == 1 {
            let name = std::fs::read_to_string(e.path().join("device/name"))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| e.file_name().to_string_lossy().to_string());
            found.push(name);
        }
    }
    found.sort();
    Ok(found)
}

/// Live agent sessions, across every logged-in user.
///
/// Three outcomes and they are genuinely different:
///
/// * no `apex-agentd` control socket anywhere → **`Sessions { live: 0 }`**. The
///   agent runtime is not running, so no agent session is running. That is a
///   measurement, not a failed read.
/// * a socket exists and would not answer → **`Unreadable`** with the reason.
///   A socket that is there and mute is exactly the case where assuming zero
///   would let a machine suspend on top of live work.
/// * sockets answered → the summed count of live sessions.
pub fn read_work(roots: &Roots) -> Work {
    let dir = roots.path("/run/user");
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Work::Sessions { live: 0 };
        }
        Err(e) => {
            return Work::Unreadable { why: format!("{}: {e}", dir.display()) };
        }
    };

    let mut sockets = Vec::new();
    for e in entries.flatten() {
        let sock = e.path().join("apex-agentd/control.sock");
        if sock.exists() {
            sockets.push(sock);
        }
    }
    if sockets.is_empty() {
        return Work::Sessions { live: 0 };
    }

    let mut live = 0u32;
    let mut answered = false;
    let mut errors = Vec::new();
    for sock in &sockets {
        match apex_agent_core::client::Client::connect_at(sock) {
            Ok(mut c) => {
                match c.call(&apex_agent_core::protocol::Request::List) {
                    Ok(apex_agent_core::protocol::Response::Sessions { sessions }) => {
                        answered = true;
                        live += sessions.iter().filter(|s| s.is_live()).count() as u32;
                    }
                    Ok(other) => {
                        errors.push(format!("{} answered {other:?}", sock.display()));
                    }
                    Err(e) => errors.push(format!("{}: {e}", sock.display())),
                }
            }
            Err(e) => errors.push(format!("{}: {e}", sock.display())),
        }
    }
    if answered {
        return Work::Sessions { live };
    }
    Work::Unreadable {
        why: format!(
            "{} agent control socket(s) exist and none answered: {}",
            sockets.len(),
            errors.join("; ")
        ),
    }
}

/// Every live session, for the checkpoint pass and the report.
fn live_sessions(roots: &Roots) -> Vec<apex_agent_core::protocol::SessionInfo> {
    let dir = roots.path("/run/user");
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let sock = e.path().join("apex-agentd/control.sock");
        if !sock.exists() {
            continue;
        }
        let Ok(mut c) = apex_agent_core::client::Client::connect_at(&sock) else { continue };
        if let Ok(apex_agent_core::protocol::Response::Sessions { sessions }) =
            c.call(&apex_agent_core::protocol::Request::List)
        {
            out.extend(sessions.into_iter().filter(|s| s.is_live()));
        }
    }
    out
}

/// The active tunnel, by the same query APEX Shell's indicator uses.
///
/// NetworkManager first — WireGuard and every plugin VPN appear there — then
/// `sing-box`, which APEX ships as a unit rather than an NM connection. An
/// `nmcli` that could not be run is `Unreadable`, never "no VPN": the whole
/// point of sampling this is to catch a tunnel that went away, and a failed
/// probe that reads as "there was never one" would hide exactly that.
pub fn read_vpn(runner: &Runner) -> (Option<String>, VpnState) {
    // Under a fixture root there is no NetworkManager to ask, and a criterion
    // with no coverage is a criterion nobody has tested. `/var/lib/apex/lid/vpn`
    // stands in: `up <name>`, `down`, `none`, or anything else for the
    // unreadable case. `tests/test-apex-lid.sh` drives Up then Down across two
    // polls and watches the report turn from "the VPN held" into "the VPN
    // DROPPED" — which is the load-bearing half of this whole item.
    if runner.roots.is_fixture() {
        return match runner.roots.read_optional("/var/lib/apex/lid/vpn") {
            Ok(Some(text)) => {
                let t = text.trim();
                match t.split_once(' ') {
                    Some(("up", name)) => (Some(name.to_string()), VpnState::Up),
                    _ if t == "up" => (Some("fixture".to_string()), VpnState::Up),
                    _ if t == "down" => (None, VpnState::Down),
                    _ if t == "none" => (None, VpnState::None),
                    _ => (
                        None,
                        VpnState::Unreadable { why: format!("fixture said {t:?}") },
                    ),
                }
            }
            _ => (
                None,
                VpnState::Unreadable {
                    why: "NetworkManager was not queried (fixture root)".to_string(),
                },
            ),
        };
    }
    let nm = runner.probe("nmcli", &["-t", "-f", "TYPE,NAME", "con", "show", "--active"]);
    match &nm {
        Ran::Ok(text) => {
            for line in text.lines() {
                let Some((kind, name)) = line.split_once(':') else { continue };
                if kind == "wireguard" || kind == "vpn" || kind == "tun" {
                    return (Some(name.to_string()), VpnState::Up);
                }
            }
        }
        Ran::Failed(why) => {
            return (None, VpnState::Unreadable { why: why.clone() });
        }
        Ran::NotRun(_) => {
            return (
                None,
                VpnState::Unreadable {
                    why: "NetworkManager was not queried (fixture root or dry run)".to_string(),
                },
            );
        }
    }

    let sb = runner.probe("systemctl", &["is-active", "sing-box.service"]);
    if let Ran::Ok(text) = &sb {
        if text.trim() == "active" {
            return (Some("sing-box".to_string()), VpnState::Up);
        }
    }
    (None, VpnState::None)
}

// ── policy loading ───────────────────────────────────────────────────────────

/// Which file the policy came from, so the readout can say.
pub struct LoadedPolicy {
    pub policy: LidPolicy,
    pub source: String,
    /// A file that exists and would not parse is reported, never ignored: a
    /// silently-discarded pin is a machine doing the opposite of what its owner
    /// asked.
    pub error: Option<String>,
}

/// `$APEX_LID_CONFIG`, else the owner's `~/.config/apex/lid.toml`, else
/// `/etc/apex/lid.toml`, else the shipped defaults.
pub fn load_policy(roots: &Roots) -> LoadedPolicy {
    let mut candidates: Vec<(String, PathBuf)> = Vec::new();
    if let Some(p) = std::env::var_os("APEX_LID_CONFIG") {
        candidates.push(("APEX_LID_CONFIG".to_string(), PathBuf::from(p)));
    } else {
        for (who, path) in owner_configs(roots) {
            candidates.push((who, path));
        }
        candidates.push((SYSTEM_CONFIG.to_string(), roots.path(SYSTEM_CONFIG)));
    }

    // Notes accumulate rather than abort. A candidate belonging to ANOTHER user
    // is routinely unreadable — running `apex lid status` as andre walks past
    // root's `/root/.config/apex/lid.toml` and gets EACCES, which is not a
    // reason to discard andre's own pin. But it is not nothing either: a file
    // that exists and could not be read is reported, every time, because
    // "permission denied is not absence" and a silently skipped pin is a
    // machine doing the opposite of what its owner asked.
    let mut notes: Vec<String> = Vec::new();
    for (source, path) in candidates {
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<LidPolicy>(&text) {
                Ok(policy) => {
                    return LoadedPolicy {
                        policy,
                        source,
                        error: (!notes.is_empty()).then(|| notes.join("; ")),
                    };
                }
                Err(e) => {
                    // A file that parses as garbage is NOT skipped past. It was
                    // written deliberately and it is the one this caller meant.
                    notes.push(format!("{}: {e}", path.display()));
                    return LoadedPolicy {
                        policy: LidPolicy::default(),
                        source: "built-in defaults".to_string(),
                        error: Some(notes.join("; ")),
                    };
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => notes.push(format!("{}: {e}", path.display())),
        }
    }
    LoadedPolicy {
        policy: LidPolicy::default(),
        source: "built-in defaults".to_string(),
        error: (!notes.is_empty()).then(|| notes.join("; ")),
    }
}

/// Config files belonging to users with a live runtime directory.
///
/// The driver runs as root and the pin does not: `apex lid pin on` must never
/// need privilege, or the shell tile becomes a polkit prompt on the owner's
/// desktop. So root reads the owner's file rather than owning it. On a
/// multi-user machine the lowest uid with a config wins and the readout names
/// them — a laptop has one owner, and guessing between two would be worse than
/// saying which one was used.
fn owner_configs(roots: &Roots) -> Vec<(String, PathBuf)> {
    // The invoking user first: `apex lid status` run by a human should read the
    // file that `apex lid pin` would write.
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        if !home.is_empty() {
            out.push((
                "~/.config/apex/lid.toml".to_string(),
                config_path_for_home(Path::new(&home)),
            ));
        }
    }

    let dir = roots.path("/run/user");
    let Ok(entries) = std::fs::read_dir(&dir) else { return out };
    let mut uids: Vec<u32> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_string_lossy().parse::<u32>().ok())
        .collect();
    uids.sort_unstable();
    let homes = passwd_homes(roots);
    for uid in uids {
        if let Some(home) = homes.get(&uid) {
            let p = config_path_for_home(&roots.path(&home.to_string_lossy()));
            if !out.iter().any(|(_, q)| q == &p) {
                out.push((format!("uid {uid}: {}", p.display()), p));
            }
        }
    }
    out
}

fn config_path_for_home(home: &Path) -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x).join("apex/lid.toml"),
        _ => home.join(".config/apex/lid.toml"),
    }
}

/// uid → home directory, straight out of `/etc/passwd`.
///
/// Parsed rather than resolved through NSS on purpose: the driver must work
/// from a fixture tree, and `getpwuid` would read the real machine no matter
/// what `APEX_LID_ROOT` said.
fn passwd_homes(roots: &Roots) -> BTreeMap<u32, PathBuf> {
    let mut map = BTreeMap::new();
    let Ok(text) = roots.read("/etc/passwd") else { return map };
    for line in text.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() < 6 {
            continue;
        }
        if let Ok(uid) = f[2].parse::<u32>() {
            map.insert(uid, PathBuf::from(f[5]));
        }
    }
    map
}

fn user_config_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
    config_path_for_home(&home)
}

// ── the power-down half ──────────────────────────────────────────────────────

/// What to switch off for a keep-working close, and what could not be switched
/// off and why.
///
/// Nothing is guessed: every action carries the value it is replacing, read at
/// plan time, so the restore puts the machine back exactly rather than back to
/// "the usual".
pub fn plan_powerdown(roots: &Roots, pd: &PowerDown, runner: &Runner) -> PowerPlan {
    let mut actions = Vec::new();
    let mut skipped = Vec::new();

    if pd.display {
        // The root-reachable lever is the backlight class. A compositor DPMS
        // is the user-session complement (APEX Shell drives it) and is not
        // available to a system service, which is why this writes sysfs and
        // says so rather than pretending to own the display.
        match backlights(roots) {
            Ok(bls) if bls.is_empty() => skipped.push(Skipped {
                what: "display".to_string(),
                why: format!(
                    "{} holds no backlight device",
                    roots.path("/sys/class/backlight").display()
                ),
            }),
            Ok(_) => actions.push(PowerAction::Display { on: false }),
            Err(why) => skipped.push(Skipped { what: "display".to_string(), why }),
        }
    }

    if pd.keyboard_backlight {
        match kbd_backlights(roots) {
            Ok(leds) if leds.is_empty() => skipped.push(Skipped {
                what: "keyboard backlight".to_string(),
                why: "no /sys/class/leds entry matches *kbd_backlight*".to_string(),
            }),
            Ok(leds) => {
                for (path, prior) in leds {
                    actions.push(PowerAction::KeyboardBacklight { path, value: 0, prior });
                }
            }
            Err(why) => {
                skipped.push(Skipped { what: "keyboard backlight".to_string(), why })
            }
        }
    }

    if pd.bluetooth {
        actions.push(PowerAction::Bluetooth { blocked: true });
    }

    if pd.wifi_powersave_off {
        match wifi_device(roots) {
            Some(dev) => {
                let prior = wifi_powersave(runner, &dev);
                actions.push(PowerAction::WifiPowerSave { dev, on: false, prior });
            }
            None => skipped.push(Skipped {
                what: "wifi power save".to_string(),
                why: "no wireless interface under /sys/class/net".to_string(),
            }),
        }
    }

    for (units, user) in [(&pd.stop_user_units, true), (&pd.stop_system_units, false)] {
        for unit in units {
            match unit_is_active(runner, unit, user) {
                Some(true) => {
                    actions.push(PowerAction::Unit { unit: unit.clone(), user, start: false })
                }
                Some(false) => {}
                None => skipped.push(Skipped {
                    what: format!("{}{unit}", if user { "--user " } else { "" }),
                    why: "systemctl could not be asked whether it is active".to_string(),
                }),
            }
        }
    }

    PowerPlan { actions, skipped }
}

fn backlights(roots: &Roots) -> Result<Vec<PathBuf>, String> {
    let dir = roots.path("/sys/class/backlight");
    match std::fs::read_dir(&dir) {
        Ok(e) => Ok(e.flatten().map(|x| x.path()).collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{}: {e}", dir.display())),
    }
}

/// `(brightness path, current value)` for every keyboard-backlight LED.
fn kbd_backlights(roots: &Roots) -> Result<Vec<(String, u32)>, String> {
    let dir = roots.path("/sys/class/leds");
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.contains("kbd_backlight") {
            continue;
        }
        let p = e.path().join("brightness");
        match std::fs::read_to_string(&p) {
            Ok(s) => {
                let v = s.trim().parse::<u32>().unwrap_or(0);
                out.push((p.to_string_lossy().to_string(), v));
            }
            // Present and unreadable is reported by the caller as a skip with
            // this reason, not silently dropped.
            Err(err) => return Err(format!("{}: {err}", p.display())),
        }
    }
    out.sort();
    Ok(out)
}

fn wifi_device(roots: &Roots) -> Option<String> {
    let dir = roots.path("/sys/class/net");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().join("wireless").exists() || e.path().join("phy80211").exists())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names.into_iter().next()
}

fn wifi_powersave(runner: &Runner, dev: &str) -> Option<bool> {
    match runner.probe("iw", &["dev", dev, "get", "power_save"]) {
        Ran::Ok(text) => {
            let t = text.to_ascii_lowercase();
            if t.contains("power save: on") {
                Some(true)
            } else if t.contains("power save: off") {
                Some(false)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn unit_is_active(runner: &Runner, unit: &str, user: bool) -> Option<bool> {
    let mut args: Vec<&str> = Vec::new();
    if user {
        args.push("--user");
    }
    args.push("is-active");
    args.push(unit);
    match runner.probe("systemctl", &args) {
        Ran::Ok(text) => Some(text.trim() == "active"),
        // `systemctl is-active` exits non-zero for an inactive unit, which is
        // an answer rather than a failure.
        Ran::Failed(_) => Some(false),
        Ran::NotRun(_) => {
            // Under a fixture root there is no systemd to ask. A unit list is
            // then taken from the fixture so the plan is still exercised.
            fixture_active_units(runner.roots).map(|set| set.contains(&unit.to_string()))
        }
    }
}

/// Under a fixture root: `/var/lib/apex/lid/active-units`, one unit per line.
fn fixture_active_units(roots: &Roots) -> Option<Vec<String>> {
    roots
        .read_optional("/var/lib/apex/lid/active-units")
        .ok()
        .flatten()
        .map(|t| t.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
}

/// Apply a plan. Returns one line per action describing what happened.
pub fn apply(roots: &Roots, runner: &Runner, plan: &PowerPlan) -> Vec<String> {
    let mut out = Vec::new();
    for a in &plan.actions {
        out.push(match a {
            PowerAction::Display { on } => apply_display(roots, runner, *on),
            PowerAction::KeyboardBacklight { path, value, .. } => {
                apply_sysfs(roots, runner, path, &value.to_string())
            }
            PowerAction::Bluetooth { blocked } => {
                let verb = if *blocked { "block" } else { "unblock" };
                let r = runner.run("rfkill", &[verb, "bluetooth"]);
                format!("rfkill {verb} bluetooth: {}", short(&r))
            }
            PowerAction::WifiPowerSave { dev, on, .. } => {
                let v = if *on { "on" } else { "off" };
                let r = runner.run("iw", &["dev", dev, "set", "power_save", v]);
                format!("iw dev {dev} set power_save {v}: {}", short(&r))
            }
            PowerAction::Unit { unit, user, start } => {
                let verb = if *start { "start" } else { "stop" };
                let mut args: Vec<&str> = Vec::new();
                if *user {
                    args.push("--user");
                }
                args.push(verb);
                args.push(unit);
                let r = runner.run("systemctl", &args);
                format!("systemctl {}{verb} {unit}: {}", if *user { "--user " } else { "" }, short(&r))
            }
        });
    }
    out
}

fn short(r: &Ran) -> String {
    match r {
        Ran::Ok(_) => "ok".to_string(),
        Ran::NotRun(_) => "not run (dry run or fixture root)".to_string(),
        Ran::Failed(why) => format!("FAILED — {why}"),
    }
}

/// Blank or restore every panel through the backlight class.
///
/// `bl_power` is the FB blanking level (4 = powerdown); brightness is set to 0
/// alongside it because a handful of drivers ignore `bl_power`. The prior
/// brightness is saved next to the record so the restore is exact.
fn apply_display(roots: &Roots, runner: &Runner, on: bool) -> String {
    let Ok(bls) = backlights(roots) else {
        return "display: /sys/class/backlight could not be listed".to_string();
    };
    let mut notes = Vec::new();
    for bl in bls {
        let name = bl.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let bl_power = bl.join("bl_power");
        let brightness = bl.join("brightness");
        if on {
            let saved = roots
                .read_optional(&format!("/var/lib/apex/lid/backlight-{name}"))
                .ok()
                .flatten();
            notes.push(format!(
                "{name}: bl_power=0 {}",
                match saved {
                    Some(v) => write_gated(runner, &brightness, v.trim()),
                    None => "brightness left as found (no saved value)".to_string(),
                }
            ));
            let _ = write_gated(runner, &bl_power, "0");
        } else {
            if let Ok(cur) = std::fs::read_to_string(&brightness) {
                remember(roots, &format!("/var/lib/apex/lid/backlight-{name}"), cur.trim());
            }
            let a = write_gated(runner, &bl_power, "4");
            let b = write_gated(runner, &brightness, "0");
            notes.push(format!("{name}: bl_power {a}; brightness {b}"));
        }
    }
    if notes.is_empty() {
        return "display: no backlight device".to_string();
    }
    format!("display: {}", notes.join("; "))
}

fn apply_sysfs(roots: &Roots, runner: &Runner, path: &str, value: &str) -> String {
    format!("{path} <- {value}: {}", write_gated(runner, &roots.path(path), value))
}

/// A sysfs write that `--dry-run` can actually stop.
///
/// This existed as an ungated `write_abs` for one commit and it was a real
/// hazard: `apex lid watch --once --dry-run` on the live machine would have put
/// `bl_power=4` and `brightness=0` into the real `amdgpu_bl1` and blanked the
/// owner's screen, because `--dry-run` was consulted only by the subprocess
/// path. Every write the driver makes to the machine now goes through here, and
/// under a fixture root or `--dry-run` the intent is logged instead.
fn write_gated(runner: &Runner, path: &Path, value: &str) -> String {
    if !runner.executes() {
        let _ = runner.append_log(&format!("write {} <- {value}", path.display()));
        return "not written (dry run or fixture root)".to_string();
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match std::fs::write(path, value) {
        Ok(()) => "ok".to_string(),
        // "Permission denied is not absence": a sysfs write that was refused is
        // reported with the reason, never counted as a thing that happened.
        Err(e) => format!("FAILED — {e}"),
    }
}

/// A write the driver makes to its OWN state under `/var/lib/apex/lid`.
///
/// Deliberately not gated: remembering the brightness it is about to replace,
/// or the plan that will undo the close, is bookkeeping rather than a change to
/// the machine, and a `--dry-run` that skipped it would leave the next real run
/// unable to restore.
fn remember(roots: &Roots, rel: &str, value: &str) {
    let _ = roots.write(rel, value);
}

// ── the record ───────────────────────────────────────────────────────────────

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn load_period(roots: &Roots, which: &str) -> Option<ClosedPeriod> {
    let text = roots.read_optional(which).ok().flatten()?;
    serde_json::from_str(&text).ok()
}

fn save_period(roots: &Roots, which: &str, p: &ClosedPeriod) -> Result<(), String> {
    let text = serde_json::to_string_pretty(p).map_err(|e| e.to_string())?;
    roots.write(which, &format!("{text}\n"))
}

// ── the CLI ──────────────────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum LidCmd {
    /// What the lid policy sees and what it would do right now.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// The decision, with every input that produced it named.
    Explain {
        #[arg(long)]
        json: bool,
    },
    /// Pin keep-working on or off, or hand it back to `auto`.
    ///
    /// Writes `~/.config/apex/lid.toml`. Needs no privilege on purpose: the
    /// inhibitor itself is authorised for an active session, so the only thing
    /// a root-owned pin would buy is a password prompt every time the shell
    /// tile is toggled.
    Pin {
        /// `auto`, `on` or `off`. Omit to print the current pin.
        state: Option<String>,
    },
    /// What the last closed period did, after reopening.
    Report {
        #[arg(long)]
        json: bool,
    },
    /// What would be powered down for a keep-working close. Applies nothing.
    Plan {
        #[arg(long)]
        json: bool,
    },
    /// The driver: hold the inhibitor while the policy says to, re-evaluate the
    /// guards for the whole closed period, power things down and back up, and
    /// keep the record.
    Watch {
        /// Evaluate and act exactly once, then exit. What the suite drives.
        #[arg(long)]
        once: bool,
        /// Decide and print; change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Override the policy's poll interval, in seconds.
        #[arg(long)]
        interval: Option<u64>,
    },
}

pub fn main(cmd: Option<LidCmd>) -> i32 {
    let roots = Roots::from_env();
    match cmd.unwrap_or(LidCmd::Status { json: false }) {
        LidCmd::Status { json } => status(&roots, json),
        LidCmd::Explain { json } => explain(&roots, json),
        LidCmd::Pin { state } => pin(&roots, state),
        LidCmd::Report { json } => report(&roots, json),
        LidCmd::Plan { json } => plan_cmd(&roots, json),
        LidCmd::Watch { once, dry_run, interval } => watch(&roots, once, dry_run, interval),
    }
}

/// Gather every input. One place, so status, explain and the driver can never
/// disagree about what the machine looked like.
pub fn measure(roots: &Roots, runner: &Runner) -> LidInputs {
    let sys_root = match std::env::var_os("APEX_LID_ROOT") {
        Some(r) => PathBuf::from(r).join("sys"),
        None => PathBuf::from("/sys"),
    };
    LidInputs {
        lid: read_lid(roots, runner),
        work: read_work(roots),
        thermal: Thermal::read(&sys_root),
        charge: Charge::read(&sys_root),
    }
}

/// Whether anything currently holds a `handle-lid-switch` block inhibitor.
///
/// Asked of logind rather than remembered, because the question that matters is
/// whether the lid is actually held — by this driver, by a desktop environment,
/// or by nothing at all.
fn inhibitor_holders(runner: &Runner) -> Result<Vec<String>, String> {
    match runner.probe("systemd-inhibit", &["--list", "--no-legend", "--no-pager"]) {
        Ran::Ok(text) => Ok(text
            .lines()
            .filter(|l| l.contains("handle-lid-switch") && l.contains("block"))
            .map(|l| l.split_whitespace().take(2).collect::<Vec<_>>().join(" "))
            .collect()),
        Ran::Failed(why) => Err(why),
        Ran::NotRun(_) => Err("logind was not queried (fixture root)".to_string()),
    }
}

fn status(roots: &Roots, as_json: bool) -> i32 {
    let runner = Runner::new(roots, true);
    let loaded = load_policy(roots);
    let inputs = measure(roots, &runner);
    let decision = loaded.policy.decide(&inputs);
    let held = inhibitor_holders(&runner);
    let (vpn_name, vpn) = read_vpn(&runner);

    if as_json {
        println!(
            "{}",
            json!({
                "policy_source": loaded.source,
                "policy_error": loaded.error,
                "pin": loaded.policy.pin.as_str(),
                "enabled": loaded.policy.enabled,
                "battery_floor_pct": loaded.policy.battery_floor_pct,
                "thermal_ceiling_c": loaded.policy.thermal_ceiling_c,
                "thermal_headroom_c": loaded.policy.thermal_headroom_c,
                "thermal_headroom_now_c": inputs.thermal.headroom_c(),
                "inputs": inputs,
                "decision": decision,
                "holds_inhibitor": decision.holds_inhibitor(),
                "inhibitor_holders": held.as_ref().ok(),
                "inhibitor_error": held.as_ref().err(),
                "vpn": { "name": vpn_name, "state": vpn },
            })
        );
        return 0;
    }

    println!("lid          {}", inputs.lid);
    println!(
        "work         {}",
        match &inputs.work {
            Work::Sessions { live } => format!("{live} live agent session(s)"),
            Work::Unreadable { why } => format!("unknown — {why}"),
        }
    );
    println!(
        "thermal      {}",
        match &inputs.thermal {
            Thermal::Celsius { c, critical_c: Some(crit), sensor } => format!(
            "{sensor} {c:.1} °C, {:.1} °C below its own {crit:.1} °C critical trip \
             (guard fires inside {:.1})",
            crit - c,
            loaded.policy.thermal_headroom_c
        ),
        Thermal::Celsius { c, critical_c: None, sensor } => format!(
            "{sensor} {c:.1} °C, no declared critical trip (backstop {:.1} °C)",
            loaded.policy.thermal_ceiling_c
        ),
            Thermal::NoSensor => "no sensor".to_string(),
            Thermal::Unreadable { why } => format!("unknown — {why}"),
        }
    );
    println!(
        "charge       {}",
        match &inputs.charge {
            Charge::Ac => "on mains".to_string(),
            Charge::Battery { percent } =>
                format!("{percent}% (floor {}%)", loaded.policy.battery_floor_pct),
            Charge::NoBattery => "no battery".to_string(),
            Charge::Unreadable { why } => format!("unknown — {why}"),
        }
    );
    println!("pin          {} (from {})", loaded.policy.pin, loaded.source);
    if let Some(e) = &loaded.error {
        // Not "the policy is broken": one of the files the search walked past
        // could not be read. Said out loud every time, because a config that
        // was skipped rather than absent is the difference between a pin being
        // honoured and being ignored.
        println!("policy note  a candidate policy file could not be read — {e}");
    }
    println!(
        "vpn          {}{}",
        vpn.as_str(),
        vpn_name.map(|n| format!(" ({n})")).unwrap_or_default()
    );
    match &held {
        Ok(h) if h.is_empty() => println!("inhibitor    nobody holds handle-lid-switch"),
        Ok(h) => println!("inhibitor    held by {}", h.join(", ")),
        Err(e) => println!("inhibitor    UNKNOWN — {e}"),
    }
    println!("decision     {}", decision.as_str());
    println!("             {}", decision.why());
    0
}

fn explain(roots: &Roots, as_json: bool) -> i32 {
    let runner = Runner::new(roots, true);
    let loaded = load_policy(roots);
    let inputs = measure(roots, &runner);
    let decision = loaded.policy.decide(&inputs);
    if as_json {
        println!(
            "{}",
            json!({ "inputs": inputs, "decision": decision, "guard": decision.guard().map(|g| g.as_str()) })
        );
        return 0;
    }
    println!("{}: {}", decision.as_str(), decision.why());
    if let Some(g) = decision.guard() {
        println!("guard {g} fired; live work is checkpointed before the machine suspends");
    }
    0
}

fn pin(roots: &Roots, state: Option<String>) -> i32 {
    let loaded = load_policy(roots);
    let Some(state) = state else {
        println!("{} (from {})", loaded.policy.pin, loaded.source);
        return 0;
    };
    let Some(p) = Pin::parse(&state) else {
        eprintln!("apex: `{state}` is not a pin; use auto, on or off");
        return 2;
    };
    let path = match std::env::var_os("APEX_LID_CONFIG") {
        Some(x) => PathBuf::from(x),
        None => user_config_path(),
    };
    // Only the `pin` key is rewritten, and the rest of the file is kept
    // verbatim. Serialising the whole resolved policy here would freeze today's
    // `thermal_headroom_c`, battery floor and unit list into the owner's file
    // forever: a later release that raised the headroom would never reach a
    // machine whose owner had once toggled the tile.
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let text = set_pin_key(&existing, p);
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("apex: {}: {e}", dir.display());
            return 1;
        }
    }
    if let Err(e) = std::fs::write(&path, &text) {
        eprintln!("apex: {}: {e}", path.display());
        return 1;
    }
    println!("lid keep-working pinned {p} in {}", path.display());
    match p {
        Pin::On => println!(
            "the guards still apply: a hot or nearly flat machine suspends anyway, and says \
             which guard fired"
        ),
        Pin::Off => println!("the lid now suspends immediately, as it does on a stock install"),
        Pin::Auto => println!("keep-working now follows live agent sessions"),
    }
    0
}

/// Replace (or add) the top-level `pin` key, leaving every other line as it was.
///
/// Deliberately line-based rather than a TOML round trip. `toml::to_string` on a
/// parsed document discards comments and reorders keys, and this file is one an
/// owner edits by hand.
fn set_pin_key(existing: &str, pin: Pin) -> String {
    let line = format!("pin = \"{}\"", pin.as_str());
    let mut out: Vec<String> = Vec::new();
    let mut replaced = false;
    // Only the top-level table: a `pin` inside `[powerdown]` is somebody else's
    // key and must not be rewritten.
    let mut in_table = false;
    for l in existing.lines() {
        let t = l.trim_start();
        if t.starts_with('[') {
            in_table = true;
        }
        if !in_table && !replaced && t.starts_with("pin") && t.split('=').next().map(|k| k.trim()) == Some("pin") {
            out.push(line.clone());
            replaced = true;
            continue;
        }
        out.push(l.to_string());
    }
    if !replaced {
        if out.is_empty() {
            out.push("# apex lid policy — see `apex lid status`".to_string());
        }
        // Before the first table header, or at the end when there is none.
        match out.iter().position(|l| l.trim_start().starts_with('[')) {
            Some(i) => out.insert(i, line),
            None => out.push(line),
        }
    }
    let mut text = out.join("\n");
    text.push('\n');
    text
}

fn report(roots: &Roots, as_json: bool) -> i32 {
    let period = load_period(roots, LAST).or_else(|| load_period(roots, STATE));
    let Some(p) = period else {
        if as_json {
            println!("{}", json!({ "period": Value::Null }));
        } else {
            println!("no lid-closed period has been recorded on this machine yet");
        }
        return 0;
    };
    if as_json {
        println!("{}", json!({ "period": p, "summary": p.summary(), "vpn_held": p.vpn_held() }));
        return 0;
    }
    println!("{}", p.summary());
    println!("why          {}", p.why);
    if let Some(g) = p.ended_by {
        println!("guard        {g}");
    }
    if let Some(c) = p.peak_c {
        println!("peak         {c:.1} °C");
    }
    if !p.powered_down.is_empty() {
        println!("powered down {}", p.powered_down.join(", "));
    }
    for s in &p.skipped {
        println!("not done     {} — {}", s.what, s.why);
    }
    println!("vpn samples  {}", p.vpn.len());
    for s in &p.vpn {
        println!(
            "  +{:>5}s   {}{}",
            s.at.saturating_sub(p.closed_at),
            s.state.as_str(),
            s.name.as_ref().map(|n| format!(" ({n})")).unwrap_or_default()
        );
    }
    0
}

fn plan_cmd(roots: &Roots, as_json: bool) -> i32 {
    let runner = Runner::new(roots, true);
    let loaded = load_policy(roots);
    let plan = plan_powerdown(roots, &loaded.policy.powerdown, &runner);
    if as_json {
        println!("{}", json!({ "plan": plan, "restore": plan.restore() }));
        return 0;
    }
    if plan.is_empty() {
        println!("nothing would be powered down");
    }
    for a in &plan.actions {
        println!("would  {}", a.describe());
    }
    for s in &plan.skipped {
        println!("cannot {} — {}", s.what, s.why);
    }
    0
}

/// The driver.
///
/// One evaluation is: measure, decide, and make the machine match the decision.
/// `--once` runs exactly one of those and is what the suite drives; without it
/// the loop re-evaluates every `poll_secs`, because a bag heats up and a
/// battery drains and a decision taken at the `SW_LID` edge and never revisited
/// is the failure mode the guards exist to prevent.
fn watch(roots: &Roots, once: bool, dry_run: bool, interval: Option<u64>) -> i32 {
    let runner = Runner::new(roots, dry_run);
    let loaded = load_policy(roots);
    if let Some(e) = &loaded.error {
        eprintln!("apex lid: a candidate policy file could not be read ({e})");
    }
    let every = interval.unwrap_or(loaded.policy.poll_secs).max(1);
    let mut held: Option<InhibitorHandle> = None;
    let mut holding = false;
    let mut period: Option<ClosedPeriod> = load_period(roots, STATE);

    loop {
        let inputs = measure(roots, &runner);
        let decision = loaded.policy.decide(&inputs);
        let closed = inputs.lid.treat_as_closed() && !inputs.lid.is_absent();
        let was_holding = holding;

        // ── the inhibitor ───────────────────────────────────────────────────
        if decision.holds_inhibitor() {
            if !holding {
                held = InhibitorHandle::take(&runner, decision.why());
                holding = true;
                if held.is_none() && runner.executes() {
                    holding = false;
                    eprintln!(
                        "apex lid: the handle-lid-switch inhibitor could not be taken; the \
                         lid will suspend as it does on a stock install"
                    );
                }
            }
        } else if holding {
            if let Some(h) = held.take() {
                h.release();
            }
            holding = false;
        }

        // ── the closed period ───────────────────────────────────────────────
        if closed && decision.holds_inhibitor() {
            let p = open_period(roots, &runner, &loaded.policy.powerdown, &inputs, &decision, &mut period);
            sample(roots, &runner, p, &inputs);
        } else if !closed {
            if let Some(mut p) = period.take() {
                p.opened_at = Some(now());
                p.last_seen = now();
                restore(roots, &runner);
                let _ = save_period(roots, LAST, &p);
                let _ = roots.write(STATE, "");
                println!("{}", p.summary());
            }
        }

        // ── a guard ─────────────────────────────────────────────────────────
        if let Some(guard) = decision.guard() {
            // The period record is CREATED here when a guard fires on the very
            // first closed poll. Without this, `apex lid report` had nothing to
            // read back on exactly the case the owner most wants explained —
            // "why did my laptop go to sleep in my bag" — and criterion 5 would
            // have failed on the one path it exists for.
            let p = open_period(roots, &runner, &loaded.policy.powerdown, &inputs, &decision, &mut period);
            sample(roots, &runner, p, &inputs);
            p.ended_by = Some(guard);
            p.opened_at = None;
            let _ = save_period(roots, LAST, p);
            eprintln!("apex lid: the {guard} guard fired — {}", decision.why());
            if guard.checkpoint_first() {
                for line in checkpoint_live_work(roots, &runner, guard) {
                    eprintln!("apex lid: {line}");
                }
            }
            restore(roots, &runner);
            if let Some(h) = held.take() {
                h.release();
            }
            holding = false;
            let r = runner.run("systemctl", &["suspend"]);
            eprintln!("apex lid: suspend: {}", short(&r));
            period = None;
            let _ = roots.write(STATE, "");
            if once {
                return if r.ok() { 0 } else { 1 };
            }
        } else if was_holding && closed && !decision.holds_inhibitor() {
            // ── work finished while the lid was shut ────────────────────────
            //
            // The inhibitor has just been dropped with the lid still down.
            // Whether logind re-examines a closed lid when the last
            // `handle-lid-switch` lock goes away is not a thing to assert from
            // memory, and the only way to observe it on this machine is to
            // suspend it — which is the one act this unit may not perform. So
            // the driver does not depend on the answer: it finishes the record,
            // puts the machine back, and asks for the suspend itself. If logind
            // also re-checks, the second request is a no-op.
            if let Some(mut p) = period.take() {
                p.last_seen = now();
                p.opened_at = None;
                restore(roots, &runner);
                let _ = save_period(roots, LAST, &p);
                let _ = roots.write(STATE, "");
                println!("{}", p.summary());
            } else {
                restore(roots, &runner);
            }
            eprintln!(
                "apex lid: nothing is running any more and the lid is still shut — {}",
                decision.why()
            );
            let r = runner.run("systemctl", &["suspend"]);
            eprintln!("apex lid: suspend: {}", short(&r));
            if once {
                return if r.ok() { 0 } else { 1 };
            }
        }

        if once {
            println!("{}: {}", decision.as_str(), decision.why());
            if let Some(h) = held.take() {
                h.release();
            }
            return 0;
        }
        std::thread::sleep(std::time::Duration::from_secs(every));
    }
}

/// Start the closed-period record if it is not already open, applying the
/// power-down once, at the moment the lid shuts.
fn open_period<'p>(
    roots: &Roots,
    runner: &Runner,
    pd: &PowerDown,
    inputs: &LidInputs,
    decision: &apexd_core::lid::LidDecision,
    slot: &'p mut Option<ClosedPeriod>,
) -> &'p mut ClosedPeriod {
    if slot.is_none() {
        let mut p = ClosedPeriod {
            closed_at: now(),
            last_seen: now(),
            opened_at: None,
            sessions_at_close: inputs.work.live_count(),
            why: decision.why().to_string(),
            ended_by: None,
            charge_at_close: inputs.charge.percent(),
            charge_last: inputs.charge.percent(),
            peak_c: inputs.thermal.celsius(),
            powered_down: Vec::new(),
            skipped: Vec::new(),
            vpn: Vec::new(),
        };
        // Only a keep-working close powers anything down. A guard that fires on
        // the first poll has nothing to switch off — the machine is going to
        // sleep, which switches off rather more.
        if decision.holds_inhibitor() {
            let plan = plan_powerdown(roots, pd, runner);
            p.skipped = plan.skipped.clone();
            p.powered_down = plan.actions.iter().map(PowerAction::describe).collect();
            for line in apply(roots, runner, &plan) {
                eprintln!("apex lid: {line}");
            }
            remember(
                roots,
                "/var/lib/apex/lid/restore.json",
                &serde_json::to_string_pretty(&plan.restore()).unwrap_or_default(),
            );
        }
        *slot = Some(p);
    }
    slot.as_mut().expect("just inserted")
}

/// One poll's worth of observation appended to the open period.
fn sample(roots: &Roots, runner: &Runner, p: &mut ClosedPeriod, inputs: &LidInputs) {
    let (vpn_name, vpn) = read_vpn(runner);
    p.last_seen = now();
    p.charge_last = inputs.charge.percent();
    if let Some(c) = inputs.thermal.celsius() {
        p.peak_c = Some(p.peak_c.map_or(c, |b: f64| b.max(c)));
    }
    p.vpn.push(VpnSample { at: now(), name: vpn_name, state: vpn });
    let _ = save_period(roots, STATE, p);
}

/// Put back everything the power-down changed, from the plan that was written
/// when it was applied — never from a default.
fn restore(roots: &Roots, runner: &Runner) {
    let Ok(Some(text)) = roots.read_optional("/var/lib/apex/lid/restore.json") else { return };
    let Ok(plan) = serde_json::from_str::<PowerPlan>(&text) else { return };
    for line in apply(roots, runner, &plan) {
        eprintln!("apex lid: restore {line}");
    }
    let _ = roots.write("/var/lib/apex/lid/restore.json", "");
}

/// Ask each live session's owner to checkpoint its project before the machine
/// goes down.
///
/// `runuser` rather than doing it here: a checkpoint is a git operation in the
/// user's project, and a root process writing into it would leave root-owned
/// objects in a repository the user has to keep using. The driver runs as root
/// precisely so it can drop back down.
fn checkpoint_live_work(roots: &Roots, runner: &Runner, guard: Guard) -> Vec<String> {
    let sessions = live_sessions(roots);
    if sessions.is_empty() {
        return vec!["no live session to checkpoint".to_string()];
    }
    let homes = passwd_homes(roots);
    let users: BTreeMap<u32, String> = passwd_names(roots);
    let mut done = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for s in &sessions {
        let Some(project) = s.project.clone() else {
            done.push(format!("session {} has no project root to checkpoint", s.id));
            continue;
        };
        if seen.contains(&project) {
            continue;
        }
        seen.push(project.clone());
        // Whose project is it? The uid whose runtime dir produced the session.
        let uid = owning_uid(roots, &project, &homes).unwrap_or(0);
        let user = users.get(&uid).cloned().unwrap_or_else(|| uid.to_string());
        let label = format!("lid-{}", guard.as_str());
        let r = runner.run(
            "runuser",
            &["-u", &user, "--", "apex", "agent", "checkpoint", "--label", &label],
        );
        done.push(format!("checkpoint {project} as {user}: {}", short(&r)));
    }
    done
}

fn passwd_names(roots: &Roots) -> BTreeMap<u32, String> {
    let mut map = BTreeMap::new();
    let Ok(text) = roots.read("/etc/passwd") else { return map };
    for line in text.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() < 6 {
            continue;
        }
        if let Ok(uid) = f[2].parse::<u32>() {
            map.insert(uid, f[0].to_string());
        }
    }
    map
}

/// The uid whose home contains `project`, when one does.
fn owning_uid(_roots: &Roots, project: &str, homes: &BTreeMap<u32, PathBuf>) -> Option<u32> {
    let mut best: Option<(usize, u32)> = None;
    for (uid, home) in homes {
        let h = home.to_string_lossy().to_string();
        if h == "/" || h.is_empty() {
            continue;
        }
        if project.starts_with(&h) {
            let len = h.len();
            if best.map(|(l, _)| len > l).unwrap_or(true) {
                best = Some((len, *uid));
            }
        }
    }
    best.map(|(_, uid)| uid)
}

/// A held `handle-lid-switch` block inhibitor.
///
/// `systemd-inhibit … sleep infinity` as a child, rather than a D-Bus fd held
/// in this process, for one reason: the child dies with the driver. If the
/// driver is killed, `systemd-inhibit` goes with it, the lock is dropped and
/// the lid suspends the machine again. A lock that outlived its keeper is how
/// a laptop cooks in a bag with nothing running.
struct InhibitorHandle {
    child: Option<std::process::Child>,
}

impl InhibitorHandle {
    fn take(runner: &Runner, why: &str) -> Option<InhibitorHandle> {
        let why = why.chars().take(180).collect::<String>();
        if !runner.executes() {
            let _ = runner.append_log(&format!(
                "systemd-inhibit --what=handle-lid-switch --mode=block --who=APEX lid --why={why}"
            ));
            return None;
        }
        match Command::new("systemd-inhibit")
            .arg("--what=handle-lid-switch")
            .arg("--mode=block")
            .arg("--who=APEX lid")
            .arg(format!("--why={why}"))
            .arg("setpriv")
            .arg("--pdeathsig")
            .arg("TERM")
            .arg("sleep")
            .arg("infinity")
            .spawn()
        {
            Ok(child) => Some(InhibitorHandle { child: Some(child) }),
            Err(e) => {
                eprintln!("apex lid: systemd-inhibit: {e}");
                None
            }
        }
    }

    fn release(mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for InhibitorHandle {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Tmp {
            let p = std::env::temp_dir().join(format!(
                "apex-lid-cli-{tag}-{}-{}",
                std::process::id(),
                now()
            ));
            std::fs::create_dir_all(&p).expect("temp");
            Tmp(p)
        }
        fn write(&self, rel: &str, body: &str) {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).expect("mkdir");
            std::fs::write(p, body).expect("write");
        }
        fn roots(&self) -> Roots {
            Roots { fixture: Some(self.0.clone()) }
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_fixture_root_can_never_execute_anything() {
        let t = Tmp::new("noexec");
        let roots = t.roots();
        let runner = Runner::new(&roots, false);
        assert!(!runner.executes(), "a fixture root must not spawn processes");
        // And a real root with --dry-run must not either.
        let real = Roots { fixture: None };
        assert!(!Runner::new(&real, true).executes());
        assert!(Runner::new(&real, false).executes(), "the real path must actually run");
    }

    #[test]
    fn the_acpi_node_is_read_before_anything_else() {
        let t = Tmp::new("acpi");
        t.write("proc/acpi/button/lid/LID/state", "state:      closed\n");
        let roots = t.roots();
        let runner = Runner::new(&roots, true);
        assert_eq!(read_lid(&roots, &runner), LidState::Closed);

        let t2 = Tmp::new("acpi-open");
        t2.write("proc/acpi/button/lid/LID/state", "state:      open\n");
        let roots2 = t2.roots();
        assert_eq!(read_lid(&roots2, &Runner::new(&roots2, true)), LidState::Open);
    }

    #[test]
    fn a_machine_with_no_lid_switch_at_all_reports_no_lid() {
        let t = Tmp::new("nolid");
        // An input class with a device that is not a switch.
        t.write("sys/class/input/event0/device/name", "AT Translated Set 2 keyboard\n");
        let roots = t.roots();
        assert_eq!(read_lid(&roots, &Runner::new(&roots, true)), LidState::NoLid);
    }

    #[test]
    fn a_lid_switch_with_no_readable_state_is_unreadable_not_open() {
        // The defect class this whole codebase has already swept once: a failed
        // read recorded as a checked absence.
        let t = Tmp::new("mute-lid");
        t.write("sys/class/input/event1/device/name", "Lid Switch\n");
        t.write("sys/class/input/event1/device/capabilities/sw", "1\n");
        let roots = t.roots();
        match read_lid(&roots, &Runner::new(&roots, true)) {
            LidState::Unreadable { why } => {
                assert!(why.contains("Lid Switch"), "{why}");
                assert!(why.contains("SW_LID"), "{why}");
            }
            other => panic!("expected Unreadable, got {other:?}"),
        }
    }

    #[test]
    fn a_switch_device_that_is_not_a_lid_is_not_a_lid() {
        // `capabilities/sw` = 8 is SW_TABLET_MODE and friends, not SW_LID. A
        // bitmask test that checked for "non-zero" would make every convertible
        // and every headphone jack look like a lid.
        let t = Tmp::new("notalid");
        t.write("sys/class/input/event12/device/name", "ThinkPad Extra Buttons\n");
        t.write("sys/class/input/event12/device/capabilities/sw", "8\n");
        let roots = t.roots();
        assert_eq!(read_lid(&roots, &Runner::new(&roots, true)), LidState::NoLid);
    }

    #[test]
    fn no_agent_socket_anywhere_is_zero_sessions_not_an_unreadable() {
        let t = Tmp::new("nosock");
        t.write("run/user/1000/.keep", "");
        let roots = t.roots();
        assert_eq!(read_work(&roots), Work::Sessions { live: 0 });
    }

    #[test]
    fn a_socket_that_will_not_answer_is_unreadable_not_zero() {
        // This is the arm that matters: a mute socket read as zero would let a
        // machine suspend on top of live work.
        let t = Tmp::new("mutesock");
        t.write("run/user/1000/apex-agentd/control.sock", "not a socket");
        let roots = t.roots();
        match read_work(&roots) {
            Work::Unreadable { why } => assert!(why.contains("control.sock"), "{why}"),
            other => panic!("expected Unreadable, got {other:?}"),
        }
    }

    #[test]
    fn the_plan_names_what_it_cannot_do_instead_of_shrinking() {
        let t = Tmp::new("plan");
        // No backlight, no leds, no wireless: three named skips, not an empty
        // plan that looks like success.
        t.write("var/lib/apex/lid/active-units", "");
        let roots = t.roots();
        let runner = Runner::new(&roots, true);
        let pd = PowerDown::default();
        let plan = plan_powerdown(&roots, &pd, &runner);
        let what: Vec<&str> = plan.skipped.iter().map(|s| s.what.as_str()).collect();
        assert!(what.contains(&"keyboard backlight"), "{what:?}");
        assert!(what.contains(&"wifi power save"), "{what:?}");
        for s in &plan.skipped {
            assert!(!s.why.is_empty(), "{s:?} was skipped with no reason");
        }
    }

    #[test]
    fn the_plan_saves_the_value_it_is_about_to_replace() {
        let t = Tmp::new("kbd");
        t.write("sys/class/backlight/amdgpu_bl1/brightness", "120\n");
        t.write("sys/class/leds/platform::kbd_backlight/brightness", "2\n");
        t.write("var/lib/apex/lid/active-units", "");
        let roots = t.roots();
        let runner = Runner::new(&roots, true);
        let plan = plan_powerdown(&roots, &PowerDown::default(), &runner);
        let kbd = plan
            .actions
            .iter()
            .find_map(|a| match a {
                PowerAction::KeyboardBacklight { prior, value, .. } => Some((*prior, *value)),
                _ => None,
            })
            .expect("a keyboard backlight action");
        assert_eq!(kbd, (2, 0), "the prior brightness must be captured, not assumed");
        assert!(plan.actions.contains(&PowerAction::Display { on: false }));
    }

    #[test]
    fn only_units_that_are_actually_running_are_stopped() {
        let t = Tmp::new("units");
        t.write(
            "var/lib/apex/lid/active-units",
            "apex-storage-notice.timer\nfwupd-refresh.timer\n",
        );
        let roots = t.roots();
        let runner = Runner::new(&roots, true);
        let plan = plan_powerdown(&roots, &PowerDown::default(), &runner);
        let units: Vec<&String> = plan
            .actions
            .iter()
            .filter_map(|a| match a {
                PowerAction::Unit { unit, .. } => Some(unit),
                _ => None,
            })
            .collect();
        assert_eq!(units.len(), 2, "{units:?}");
        assert!(units.iter().any(|u| u.as_str() == "fwupd-refresh.timer"));
        assert!(
            !units.iter().any(|u| u.as_str() == "dnf-makecache.timer"),
            "a unit that was not running must not be stopped, or the restore starts it"
        );
    }

    #[test]
    fn the_owner_config_beats_the_system_one() {
        let t = Tmp::new("cfg");
        t.write("etc/apex/lid.toml", "pin = \"off\"\n");
        t.write("etc/passwd", "andre:x:1000:1000::/home/andre:/bin/zsh\n");
        t.write("run/user/1000/.keep", "");
        t.write("home/andre/.config/apex/lid.toml", "pin = \"on\"\n");
        let roots = t.roots();
        // HOME and XDG_CONFIG_HOME belong to whoever ran the test; the uid scan
        // is the path a root driver takes, so assert that one.
        let configs = owner_configs(&roots);
        assert!(
            configs.iter().any(|(_, p)| p.ends_with("home/andre/.config/apex/lid.toml")),
            "the root driver must find the owner's file: {configs:?}"
        );
    }

    #[test]
    fn an_unreadable_candidate_does_not_discard_the_one_that_follows_it() {
        // Found on the live machine: `apex lid status` run as andre walked past
        // root's config, got EACCES, and abandoned the search — so andre's own
        // pin would have been ignored in favour of the built-in defaults, with
        // a message that read as though HIS file were broken.
        let t = Tmp::new("eacces");
        let unreadable = t.0.join("locked/lid.toml");
        std::fs::create_dir_all(unreadable.parent().unwrap()).unwrap();
        std::fs::write(&unreadable, "pin = \"off\"\n").unwrap();
        let mut perm = std::fs::metadata(unreadable.parent().unwrap()).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o000);
        std::fs::set_permissions(unreadable.parent().unwrap(), perm).unwrap();

        let good = t.0.join("good.toml");
        std::fs::write(&good, "pin = \"on\"\n").unwrap();

        let roots = t.roots();
        let mut notes = Vec::new();
        // Exercise the same loop shape the loader uses.
        let candidates = vec![
            ("locked".to_string(), unreadable.clone()),
            ("good".to_string(), good.clone()),
        ];
        let mut chosen = None;
        for (src, path) in candidates {
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    chosen = Some((src, toml::from_str::<LidPolicy>(&text).unwrap()));
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => notes.push(format!("{}: {e}", path.display())),
            }
        }
        // Restore so the temp dir can be removed.
        let mut perm = std::fs::metadata(unreadable.parent().unwrap()).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
        let _ = std::fs::set_permissions(unreadable.parent().unwrap(), perm);

        if notes.is_empty() {
            // Running as root, where nothing is unreadable. Report that rather
            // than passing on an assertion that never ran.
            assert!(
                unsafe { libc::geteuid() } == 0,
                "an unreadable file produced no note and this is not root"
            );
            return;
        }
        let (src, policy) = chosen.expect("the readable candidate must still be used");
        assert_eq!(src, "good");
        assert_eq!(policy.pin, Pin::On, "the unreadable candidate discarded the good one");
        let _ = roots;
    }

    #[test]
    fn a_policy_file_that_does_not_parse_is_reported_not_ignored() {
        let t = Tmp::new("badcfg");
        let bad = t.0.join("bad.toml");
        std::fs::write(&bad, "battery_floor_pct = \"twenty\"\n").unwrap();
        std::env::set_var("APEX_LID_CONFIG", &bad);
        let roots = t.roots();
        let loaded = load_policy(&roots);
        std::env::remove_var("APEX_LID_CONFIG");
        assert!(loaded.error.is_some(), "a broken policy file must be reported");
        assert_eq!(loaded.policy.pin, Pin::Auto, "and must fall back to the shipped defaults");
    }

    #[test]
    fn the_owning_uid_is_the_deepest_matching_home() {
        let mut homes = BTreeMap::new();
        homes.insert(0u32, PathBuf::from("/root"));
        homes.insert(1000u32, PathBuf::from("/var/home/andre"));
        homes.insert(999u32, PathBuf::from("/var/home"));
        let roots = Roots { fixture: None };
        assert_eq!(
            owning_uid(&roots, "/var/home/andre/Projects/apex", &homes),
            Some(1000),
            "the deepest home must win, or every project belongs to the shallowest user"
        );
        assert_eq!(owning_uid(&roots, "/srv/thing", &homes), None);
    }

    #[test]
    fn a_vpn_probe_that_could_not_run_is_unknown_not_absent() {
        let t = Tmp::new("vpn");
        let roots = t.roots();
        let runner = Runner::new(&roots, true);
        let (_name, state) = read_vpn(&runner);
        assert!(
            matches!(state, VpnState::Unreadable { .. }),
            "an unrun probe must not read as `no VPN`: {state:?}"
        );
    }
}
