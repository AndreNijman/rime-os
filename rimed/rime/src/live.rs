//! `rime live` and the live half of `rime update`: stage a release, verify the
//! deployment that was actually staged, measure what it changes, and activate
//! the Rime-owned parts of it without a reboot.
//!
//! The rules live in `rimed_core::live` (classification, the plan, the
//! transaction state machine). This module measures and acts:
//!
//! * **Staging is verified twice.** The update path's existing gate verifies
//!   the digest the registry would serve; this stages with `bootc upgrade
//!   --download-only` (a deployment that will not boot), reads the digest that
//!   was really staged, and verifies THAT before `--from-downloaded` queues it.
//!   A refusal leaves a locked deployment nothing boots.
//! * **The live layer** is a systemd-sysext directory extension at
//!   `/run/extensions/rime-live`: copies of exactly the changed files the plan
//!   activates, taken from the verified staged deployment with their owners,
//!   modes and SELinux labels, plus overlay whiteouts for removed files. It is
//!   in `/run`, so it is gone at the next boot, where the staged deployment
//!   carries the same files. Canonical paths keep working, so
//!   `/usr/share/rime-shell`, `/usr/bin/rimed` and every keybind still point
//!   at the right thing.
//! * **Nothing disruptive is ever run.** Services in the restart-safe list are
//!   restarted; the shell is replaced only in unlocked sessions; everything
//!   else is reported with what it needs.
//!
//! State: `/var/lib/rime/live/txn.json` (the current transaction, written
//! before each step), `history.jsonl` (the audit log), and
//! `/run/rime-live/status.json` (world-readable, what the shell and
//! `rime live status` read).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::CString;
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use rimed_core::live::classify::{classify, Activator, Classified};
use rimed_core::live::diff::{
    diff_dumps, diff_packages, parse_ostree_diff, parse_rpm_list, Change, ChangeKind, PackageChange,
};
use rimed_core::live::plan::{self, Decision, Inputs, Plan};
use rimed_core::live::txn::{self, Recovery, State, Txn};
use rimed_core::live::caps::{self, NvidiaFacts, Verdict};
use rimed_core::live::{elf, Component, DeferReason, MachineState, Outcome, Requirement, SessionState};
use serde_json::{json, Value};

pub const LAYER_NAME: &str = "rime-live";
const SESSION_HELPER: &str = "/usr/libexec/rime-live-session";
const PKG_LOCK: &str = "/var/lib/rime/pkg/.lock";

// ── paths ────────────────────────────────────────────────────────────────────

/// Every path the engine touches, under one optional prefix.
///
/// `RIME_LIVE_ROOT` exists for the test suite, which runs the real binary in a
/// user namespace against a fake sysroot. `sudo` resets the environment, so a
/// real `sudo rime update` never sees it.
#[derive(Debug, Clone)]
pub struct Paths {
    pub root: Option<PathBuf>,
}

impl Paths {
    pub fn from_env() -> Paths {
        Paths { root: std::env::var_os("RIME_LIVE_ROOT").map(PathBuf::from) }
    }
    pub fn at(&self, absolute: &str) -> PathBuf {
        match &self.root {
            Some(r) => r.join(absolute.trim_start_matches('/')),
            None => PathBuf::from(absolute),
        }
    }
    fn state_dir(&self) -> PathBuf {
        self.at("/var/lib/rime/live")
    }
    fn txn_file(&self) -> PathBuf {
        self.state_dir().join("txn.json")
    }
    fn history_file(&self) -> PathBuf {
        self.state_dir().join("history.jsonl")
    }
    fn run_dir(&self) -> PathBuf {
        self.at("/run/rime-live")
    }
    pub fn status_file(&self) -> PathBuf {
        self.run_dir().join("status.json")
    }
    fn layer(&self) -> PathBuf {
        self.at("/run/extensions").join(LAYER_NAME)
    }
    /// The previous layer, kept outside /run/extensions so sysext ignores it.
    fn prev_layer(&self) -> PathBuf {
        self.run_dir().join("previous-layer")
    }
    fn new_layer(&self) -> PathBuf {
        self.run_dir().join("next-layer")
    }
    fn sysroot(&self) -> PathBuf {
        self.at("/sysroot")
    }
    fn live_root(&self) -> PathBuf {
        self.at("/")
    }
    fn testing(&self) -> bool {
        self.root.is_some()
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn boot_id(paths: &Paths) -> String {
    fs::read_to_string(paths.at("/proc/sys/kernel/random/boot_id"))
        .or_else(|_| fs::read_to_string("/proc/sys/kernel/random/boot_id"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn new_txn_id() -> String {
    let uuid = fs::read_to_string("/proc/sys/kernel/random/uuid").unwrap_or_default();
    let secs = now();
    format!("{secs}-{}", uuid.trim().chars().take(8).collect::<String>())
}

// ── commands ─────────────────────────────────────────────────────────────────

/// stdout of a command, or a sentence with its stderr.
fn output(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        return Err(format!(
            "{program} {} exited {}{}",
            args.first().copied().unwrap_or(""),
            out.status.code().unwrap_or(-1),
            if err.is_empty() { String::new() } else { format!(": {}", last_lines(err, 3)) }
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn last_lines(s: &str, n: usize) -> String {
    let v: Vec<&str> = s.lines().collect();
    v[v.len().saturating_sub(n)..].join(" / ")
}

fn status_code(program: &str, args: &[&str]) -> Result<i32, String> {
    Command::new(program)
        .args(args)
        .status()
        .map(|s| s.code().unwrap_or(-1))
        .map_err(|e| format!("could not run {program}: {e}"))
}

// ── bootc status ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deployment {
    pub image: String,
    pub digest: String,
    /// The ostree commit, for the ostree backend.
    pub checksum: Option<String>,
    pub serial: u64,
    pub stateroot: String,
    pub download_only: bool,
    pub soft_reboot_capable: bool,
    /// "ostree" or "composefs".
    pub backend: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootcStatus {
    pub booted: Option<Deployment>,
    pub staged: Option<Deployment>,
}

fn parse_deployment(v: &Value) -> Option<Deployment> {
    if v.is_null() {
        return None;
    }
    let image = v.get("image")?;
    let ostree = v.get("ostree").filter(|o| !o.is_null());
    let backend = if ostree.is_some() {
        "ostree"
    } else if v.get("composefs").is_some_and(|c| !c.is_null()) {
        "composefs"
    } else {
        "unknown"
    };
    Some(Deployment {
        image: image.pointer("/image/image").and_then(Value::as_str).unwrap_or("").to_string(),
        digest: image.get("imageDigest").and_then(Value::as_str).unwrap_or("").to_string(),
        checksum: ostree.and_then(|o| o.get("checksum")).and_then(Value::as_str).map(str::to_string),
        serial: ostree.and_then(|o| o.get("deploySerial")).and_then(Value::as_u64).unwrap_or(0),
        stateroot: ostree
            .and_then(|o| o.get("stateroot"))
            .and_then(Value::as_str)
            .unwrap_or("default")
            .to_string(),
        download_only: v.get("downloadOnly").and_then(Value::as_bool).unwrap_or(false),
        soft_reboot_capable: v.get("softRebootCapable").and_then(Value::as_bool).unwrap_or(false),
        backend: backend.to_string(),
    })
}

pub fn parse_bootc_status(text: &str) -> Result<BootcStatus, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("bootc status: {e}"))?;
    let st = v.get("status").ok_or("bootc status has no `status`")?;
    Ok(BootcStatus {
        booted: st.get("booted").and_then(parse_deployment),
        staged: st.get("staged").and_then(parse_deployment),
    })
}

fn bootc_status() -> Result<BootcStatus, String> {
    parse_bootc_status(&output("bootc", &["status", "--format=json"])?)
}

/// The checked-out tree of an ostree deployment.
fn deploy_dir(paths: &Paths, d: &Deployment) -> Result<PathBuf, String> {
    let Some(c) = &d.checksum else {
        return Err(format!(
            "this machine uses the {} storage backend, and live activation reads staged files \
             from an ostree deployment",
            d.backend
        ));
    };
    if !c.chars().all(|ch| ch.is_ascii_hexdigit()) || d.stateroot.contains('/') {
        return Err("bootc reported an implausible deployment".into());
    }
    let p = paths
        .sysroot()
        .join("ostree/deploy")
        .join(&d.stateroot)
        .join("deploy")
        .join(format!("{c}.{}", d.serial));
    if p.join("usr").is_dir() {
        Ok(p)
    } else {
        Err(format!("the deployment tree {} is not there", p.display()))
    }
}

fn release_of(tree: &Path) -> Option<String> {
    let text = fs::read_to_string(tree.join("usr/share/rime/release.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("id").and_then(Value::as_str).map(str::to_string)
}

// ── measuring ────────────────────────────────────────────────────────────────

fn measure_changes(paths: &Paths, booted: &Deployment, staged: &Deployment, bdir: &Path, sdir: &Path) -> Result<Vec<Change>, String> {
    if let (Some(a), Some(b)) = (&booted.checksum, &staged.checksum) {
        let repo = paths.sysroot().join("ostree/repo");
        let repo_arg = format!("--repo={}", repo.display());
        match output("ostree", &[&repo_arg, "diff", a, b]) {
            Ok(text) => return parse_ostree_diff(&text),
            Err(e) => eprintln!("rime: ostree diff failed ({e}); comparing composefs images instead"),
        }
    }
    let dump = |dir: &Path| -> Result<String, String> {
        let cfs = dir.join(".ostree.cfs");
        output("composefs-info", &["dump", &cfs.to_string_lossy()])
    };
    diff_dumps(&dump(bdir)?, &dump(sdir)?)
}

fn rpm_list(tree: &Path) -> Result<BTreeMap<String, String>, String> {
    let db = tree.join("usr/lib/sysimage/rpm");
    let text = output(
        "rpm",
        &["-qa", "--dbpath", &db.to_string_lossy(), "--qf", "%{NAME} %{EPOCHNUM}:%{VERSION}-%{RELEASE} %{ARCH}\\n"],
    )?;
    parse_rpm_list(&text)
}

fn measure_packages(bdir: &Path, sdir: &Path) -> Result<Vec<PackageChange>, String> {
    Ok(diff_packages(&rpm_list(bdir)?, &rpm_list(sdir)?))
}

/// Resolve `rel` (absolute inside the tree) through symlinks that stay inside it.
fn resolve_in(tree: &Path, rel: &str) -> Option<String> {
    let mut cur = rel.to_string();
    for _ in 0..16 {
        let p = tree.join(cur.trim_start_matches('/'));
        let md = fs::symlink_metadata(&p).ok()?;
        if !md.file_type().is_symlink() {
            return Some(cur);
        }
        let target = fs::read_link(&p).ok()?;
        let t = target.to_string_lossy();
        cur = if t.starts_with('/') {
            t.to_string()
        } else {
            let parent = Path::new(&cur).parent()?.join(&*t);
            normalise(&parent.to_string_lossy())
        };
    }
    None
}

fn normalise(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            x => out.push(x),
        }
    }
    format!("/{}", out.join("/"))
}

const LIB_DIRS: &[&str] = &["/usr/lib64", "/usr/lib", "/usr/lib64/rime-shell"];

/// Every library `path` loads, transitively, is the same file in both trees.
pub fn elf_closure(bdir: &Path, sdir: &Path, path: &str, changed: &HashSet<String>) -> Result<(), String> {
    let mut todo = vec![(path.to_string(), sdir.to_path_buf())];
    let mut seen: HashSet<String> = HashSet::new();
    while let Some((p, tree)) = todo.pop() {
        if seen.len() > 512 {
            return Err("library closure is implausibly large".into());
        }
        if !seen.insert(p.clone()) {
            continue;
        }
        let bytes = fs::read(tree.join(p.trim_start_matches('/'))).map_err(|e| format!("{p}: {e}"))?;
        let Some(needs) = elf::needs(&bytes).map_err(|e| format!("{p}: {e}"))? else {
            continue; // a script: its interpreter is checked by classification
        };
        if let Some(i) = &needs.interp {
            let r = resolve_in(sdir, i).ok_or_else(|| format!("interpreter {i} is missing"))?;
            if changed.contains(i) || changed.contains(&r) {
                return Err(format!("its interpreter {i} changes in this release"));
            }
        }
        let own_dir = Path::new(&p).parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
        for lib in &needs.needed {
            let found = std::iter::once(own_dir.as_str())
                .chain(LIB_DIRS.iter().copied())
                .map(|d| format!("{d}/{lib}"))
                .find_map(|cand| resolve_in(sdir, &cand).filter(|r| sdir.join(r.trim_start_matches('/')).is_file()).map(|r| (cand, r)));
            let Some((cand, real)) = found else {
                return Err(format!("links {lib}, which the release does not ship"));
            };
            if changed.contains(&cand) || changed.contains(&real) {
                return Err(format!("links {lib}, which this release changes"));
            }
            // Unchanged: read it from the booted tree, which is what a process
            // started now will load.
            todo.push((real, bdir.to_path_buf()));
        }
    }
    Ok(())
}

fn running_units() -> BTreeSet<String> {
    output("systemctl", &["list-units", "--type=service", "--state=running", "--no-legend", "--plain", "--no-pager"])
        .map(|t| t.lines().filter_map(|l| l.split_whitespace().next().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn has_nvidia(paths: &Paths) -> Option<bool> {
    let dir = fs::read_dir(paths.at("/sys/bus/pci/devices")).ok()?;
    for e in dir.flatten() {
        let vendor = fs::read_to_string(e.path().join("vendor")).unwrap_or_default();
        let class = fs::read_to_string(e.path().join("class")).unwrap_or_default();
        if vendor.trim() == "0x10de" && class.trim().starts_with("0x03") {
            return Some(true);
        }
    }
    Some(false)
}

/// `loginctl show-session` output for one session.
pub fn parse_show_session(id: &str, text: &str) -> Option<SessionState> {
    let mut kv = BTreeMap::new();
    for l in text.lines() {
        if let Some((k, v)) = l.split_once('=') {
            kv.insert(k.trim(), v.trim());
        }
    }
    let ty = kv.get("Type").copied().unwrap_or("");
    if !matches!(ty, "wayland" | "x11") || kv.get("Class").copied() != Some("user") || kv.get("Remote").copied() == Some("yes") {
        return None;
    }
    if matches!(kv.get("State").copied(), Some("closing")) {
        return None;
    }
    let desktop = kv.get("Desktop").copied().unwrap_or("").to_ascii_lowercase();
    let service = kv.get("Service").copied().unwrap_or("").to_ascii_lowercase();
    let desktop = if desktop.contains("gamescope") || service.contains("gamescope") || desktop.contains("gaming") {
        "gamescope".to_string()
    } else {
        desktop
    };
    Some(SessionState {
        user: kv.get("Name").copied().unwrap_or("").to_string(),
        uid: kv.get("User").and_then(|u| u.parse().ok()).unwrap_or(u32::MAX),
        session_id: id.to_string(),
        desktop,
        locked: match kv.get("LockedHint").copied() {
            Some("no") => Some(false),
            Some("yes") => Some(true),
            _ => None,
        },
    })
}

fn sessions() -> Result<Vec<SessionState>, String> {
    let list = output("loginctl", &["list-sessions", "--no-legend", "--no-pager"])?;
    let mut out = Vec::new();
    for line in list.lines() {
        let Some(id) = line.split_whitespace().next() else { continue };
        if !id.chars().all(|c| c.is_ascii_alphanumeric()) {
            continue;
        }
        let text = output(
            "loginctl",
            &["show-session", id, "-p", "Type", "-p", "Class", "-p", "Remote", "-p", "State", "-p", "Desktop", "-p", "Service", "-p", "Name", "-p", "User", "-p", "LockedHint"],
        )?;
        if let Some(mut s) = parse_show_session(id, &text) {
            if s.desktop.is_empty() {
                // greetd sessions carry no Desktop: find the compositor among
                // the user's processes.
                s.desktop = compositor_process(s.uid).unwrap_or_default();
            }
            out.push(s);
        }
    }
    Ok(out)
}

/// The compositor or gamescope a user is running, by process name.
fn compositor_process(uid: u32) -> Option<String> {
    for e in fs::read_dir("/proc").ok()?.flatten() {
        let Ok(md) = e.metadata() else { continue };
        if md.uid() != uid {
            continue;
        }
        let comm = fs::read_to_string(e.path().join("comm")).unwrap_or_default();
        match comm.trim() {
            "gamescope" | "gamescope-wl" => return Some("gamescope".into()),
            "Hyprland" | ".Hyprland-wrapp" => return Some("hyprland".into()),
            "niri" => return Some("niri".into()),
            "labwc" => return Some("labwc".into()),
            _ => {}
        }
    }
    None
}

fn machine_state(paths: &Paths) -> MachineState {
    use rimed_core::workload;
    let roots = workload::Roots::live();
    let game = workload::read_game_session(&roots);
    let procs = workload::read_processes(&roots);
    let busy = match (game.value(), procs.value()) {
        (Some(n), _) if *n > 0 => Some(Some(format!("a game is running ({n} processes in its session)"))),
        (_, Some(h)) if !h.game.is_empty() => {
            Some(Some(format!("a game runtime is running ({})", h.game.iter().cloned().collect::<Vec<_>>().join(", "))))
        }
        (Some(_), Some(_)) => Some(None),
        _ => None,
    };
    let sessions = sessions().unwrap_or_else(|e| {
        // Without the sessions, nothing that depends on them is permitted: one
        // session of unknown lock state defers the shell.
        eprintln!("rime: could not list sessions: {e}");
        vec![SessionState {
            user: String::new(),
            uid: u32::MAX,
            session_id: "?".into(),
            desktop: String::new(),
            locked: None,
        }]
    });
    MachineState { busy, sessions, has_nvidia: has_nvidia(paths) }
}

// ── transaction store ────────────────────────────────────────────────────────

struct Lock(#[allow(dead_code)] fs::File);

fn flock(path: &Path, what: &str, wait_s: u64) -> Result<Lock, String> {
    if let Some(d) = path.parent() {
        fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let f = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    use std::os::unix::io::AsRawFd;
    for _ in 0..=wait_s * 10 {
        // SAFETY: flock on a descriptor we own.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Lock(f));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(format!("another {what} is in progress"))
}

fn write_atomic(path: &Path, data: &[u8], mode: u32) -> Result<(), String> {
    let dir = path.parent().ok_or("no parent")?;
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".{}.tmp", path.file_name().unwrap().to_string_lossy()));
    {
        let mut f = fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        f.write_all(data).map_err(|e| e.to_string())?;
        f.set_permissions(fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

fn load_txn(paths: &Paths) -> Result<Option<Txn>, String> {
    match fs::read_to_string(paths.txn_file()) {
        Ok(t) => Txn::parse(&t).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", paths.txn_file().display())),
    }
}

/// Persist the record, the audit line and the public status. The record is
/// written first: it is what recovery reads.
fn save(paths: &Paths, t: &Txn) -> Result<(), String> {
    let text = serde_json::to_string_pretty(t).map_err(|e| e.to_string())?;
    write_atomic(&paths.txn_file(), text.as_bytes(), 0o600)?;
    if let Some(step) = t.history.last() {
        let line = json!({"txn": t.id, "at": step.at, "state": step.state, "note": step.note,
                          "booted": t.booted_digest, "target": t.target_digest});
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(paths.history_file())
            .map_err(|e| format!("{}: {e}", paths.history_file().display()))?;
        let _ = writeln!(f, "{line}");
    }
    write_status(paths, t)
}

fn advance(paths: &Paths, t: &mut Txn, s: State, note: impl Into<String>) -> Result<(), String> {
    t.advance(s, now(), note)?;
    save(paths, t)
}

fn outcome_state(o: &Outcome) -> &'static str {
    match o {
        Outcome::Unchanged => "unchanged",
        Outcome::Active { .. } => "active",
        Outcome::ActiveAtNextUse { .. } => "active-at-next-use",
        Outcome::Deferred { .. } => "deferred",
        Outcome::Pending { .. } => "pending",
        Outcome::FailedRolledBack { .. } => "failed-rolled-back",
        Outcome::Failed { .. } => "failed",
        Outcome::NotApplicable { .. } => "not-applicable",
    }
}

pub fn outcome_detail(o: &Outcome) -> String {
    match o {
        Outcome::Unchanged => "not changed by this release".into(),
        Outcome::Active { evidence, .. } => format!("active now: {evidence}"),
        Outcome::ActiveAtNextUse { evidence } => format!("installed; used from its next start: {evidence}"),
        Outcome::Deferred { reason } => reason.describe(),
        Outcome::Pending { requirement } => format!("waiting: {}", requirement.describe()),
        Outcome::FailedRolledBack { error } => format!("activation failed and was undone: {error}"),
        Outcome::Failed { error } => format!("activation failed: {error}"),
        Outcome::NotApplicable { why } => why.clone(),
    }
}

fn state_name(s: State) -> String {
    serde_json::to_value(s).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

fn req_name(r: Requirement) -> String {
    serde_json::to_value(r).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

/// The public document; its schema is in docs/live-update.md.
pub fn status_json(t: &Txn, booted_release: Option<&str>) -> Value {
    let comps: Vec<Value> = t
        .outcomes
        .iter()
        .filter(|(c, _)| **c != Component::Metadata)
        .map(|(c, o)| {
            let cp = t.plan.as_ref().and_then(|p| p.component(*c));
            json!({
                "component": c.slug(),
                "label": c.label(),
                "state": outcome_state(o),
                "requirement": req_name(cp.map(|p| p.requirement).unwrap_or(Requirement::Nothing)),
                "detail": outcome_detail(o),
                "versions": cp.map(|p| p.versions.clone()).unwrap_or_default(),
            })
        })
        .collect();
    let remaining = remaining_after(t);
    let active = t.outcomes.values().filter(|o| matches!(o, Outcome::Active { .. } | Outcome::ActiveAtNextUse { .. })).count();
    let summary = match t.state {
        State::Active | State::Deferred if remaining <= Requirement::ServiceRestart => {
            if active > 0 { format!("{active} component(s) updated live; nothing else to do.") } else { "Up to date.".into() }
        }
        State::Active | State::Deferred => format!(
            "{active} component(s) updated live; the rest needs: {}.",
            remaining.describe()
        ),
        State::RolledBack => "Live activation failed and was undone; the update applies at the next restart.".into(),
        State::Failed => "Live activation failed; see `rime live doctor`.".into(),
        State::Superseded => "Superseded by a restart or a newer update.".into(),
        _ => format!("Update in progress ({}).", state_name(t.state)),
    };
    json!({
        "schema": 1,
        "updated": t.history.last().map(|s| s.at).unwrap_or(0),
        "txn": t.id,
        "state": state_name(t.state),
        "booted": {"digest": t.booted_digest, "release": booted_release},
        "target": if t.target_digest.is_empty() { Value::Null } else {
            json!({"digest": t.target_digest, "release": t.target_release})
        },
        "staged_for_boot": t.staged_for_boot,
        "components": comps,
        "remaining": req_name(remaining),
        "recommendation": plan::recommend(remaining, t.soft_reboot_capable),
        "summary": summary,
    })
}

fn remaining_after(t: &Txn) -> Requirement {
    t.outcomes
        .iter()
        .map(|(c, o)| match o {
            Outcome::Pending { requirement } => *requirement,
            Outcome::Deferred { .. } | Outcome::FailedRolledBack { .. } | Outcome::Failed { .. } => t
                .plan
                .as_ref()
                .and_then(|p| p.component(*c))
                .map(|p| boot_need(p.requirement))
                .unwrap_or(Requirement::Reboot),
            Outcome::Active { .. } | Outcome::ActiveAtNextUse { .. } => t
                .plan
                .as_ref()
                .and_then(|p| p.component(*c))
                .and_then(|p| match p.decision {
                    Decision::Activate { residual } => residual,
                    _ => None,
                })
                .unwrap_or(Requirement::Nothing),
            _ => Requirement::Nothing,
        })
        .max()
        .unwrap_or(Requirement::Nothing)
}

/// A component that did not activate live is carried by the staged
/// deployment, so it needs a restart of at least userspace.
fn boot_need(r: Requirement) -> Requirement {
    r.max(Requirement::SoftReboot)
}

fn write_status(paths: &Paths, t: &Txn) -> Result<(), String> {
    let booted = release_of(&paths.live_root());
    let v = status_json(t, booted.as_deref());
    let text = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    write_atomic(&paths.status_file(), text.as_bytes(), 0o644)
}

// ── the live layer ───────────────────────────────────────────────────────────

fn xattrs(p: &Path) -> Vec<(CString, Vec<u8>)> {
    let Ok(cp) = CString::new(p.as_os_str().as_bytes()) else { return vec![] };
    let mut names = vec![0u8; 4096];
    // SAFETY: buffers are sized and the lengths returned are checked.
    let n = unsafe { libc::llistxattr(cp.as_ptr(), names.as_mut_ptr().cast(), names.len()) };
    if n <= 0 {
        return vec![];
    }
    names.truncate(n as usize);
    let mut out = Vec::new();
    for name in names.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        let Ok(cn) = CString::new(name) else { continue };
        let mut val = vec![0u8; 65536];
        let m = unsafe { libc::lgetxattr(cp.as_ptr(), cn.as_ptr(), val.as_mut_ptr().cast(), val.len()) };
        if m >= 0 {
            val.truncate(m as usize);
            out.push((cn, val));
        }
    }
    out
}

/// Owner, mode and extended attributes (SELinux labels) from `src` onto `dst`.
fn copy_meta(src: &Path, dst: &Path, strict: bool) -> Result<(), String> {
    let md = fs::symlink_metadata(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let cd = CString::new(dst.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: plain syscalls on a path we just created.
    if unsafe { libc::lchown(cd.as_ptr(), md.uid(), md.gid()) } != 0 && strict {
        return Err(format!("{}: chown: {}", dst.display(), std::io::Error::last_os_error()));
    }
    if !md.file_type().is_symlink() {
        fs::set_permissions(dst, fs::Permissions::from_mode(md.mode() & 0o7777)).map_err(|e| format!("{}: {e}", dst.display()))?;
    }
    for (name, val) in xattrs(src) {
        let r = unsafe { libc::lsetxattr(cd.as_ptr(), name.as_ptr(), val.as_ptr().cast(), val.len(), 0) };
        if r != 0 && strict {
            return Err(format!(
                "{}: setting {}: {}",
                dst.display(),
                name.to_string_lossy(),
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

fn ensure_parents(layer: &Path, src_tree: &Path, rel: &str, strict: bool) -> Result<(), String> {
    let mut cur = String::new();
    let parts: Vec<&str> = rel.trim_start_matches('/').split('/').collect();
    for part in &parts[..parts.len().saturating_sub(1)] {
        cur.push('/');
        cur.push_str(part);
        let d = layer.join(cur.trim_start_matches('/'));
        if !d.exists() {
            fs::create_dir(&d).map_err(|e| format!("{}: {e}", d.display()))?;
            let s = src_tree.join(cur.trim_start_matches('/'));
            if s.is_dir() {
                copy_meta(&s, &d, strict)?;
            }
        }
    }
    Ok(())
}

fn whiteout(p: &Path) -> Result<(), String> {
    let cp = CString::new(p.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: mknod of an overlay whiteout (character device 0:0).
    if unsafe { libc::mknod(cp.as_ptr(), libc::S_IFCHR, 0) } != 0 {
        return Err(format!("{}: whiteout: {}", p.display(), std::io::Error::last_os_error()));
    }
    Ok(())
}

fn copy_entry(src: &Path, dst: &Path, strict: bool) -> Result<(), String> {
    let md = fs::symlink_metadata(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let ft = md.file_type();
    if ft.is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(src).map_err(|e| e.to_string())?, dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    } else if ft.is_dir() {
        fs::create_dir(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    } else if ft.is_file() {
        fs::copy(src, dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    } else {
        return Err(format!("{}: not a file, directory or symlink", src.display()));
    }
    copy_meta(src, dst, strict)
}

fn os_release_kv(root: &Path, key: &str) -> Option<String> {
    let text = fs::read_to_string(root.join("usr/lib/os-release")).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .map(|v| v.trim_matches('"').to_string())
}

/// Build the next layer: `keep` paths copied from the current layer (components
/// that stay as they are), `set` from the staged tree.
fn build_layer(paths: &Paths, staged: &Path, set: &[Change], keep: &[String]) -> Result<PathBuf, String> {
    let strict = !paths.testing();
    let next = paths.new_layer();
    if next.exists() {
        fs::remove_dir_all(&next).map_err(|e| format!("{}: {e}", next.display()))?;
    }
    fs::create_dir_all(&next).map_err(|e| format!("{}: {e}", next.display()))?;
    let current = paths.layer();
    for rel in keep {
        ensure_parents(&next, &current, rel, strict)?;
        let dst = next.join(rel.trim_start_matches('/'));
        if dst.symlink_metadata().is_ok() {
            continue;
        }
        copy_entry(&current.join(rel.trim_start_matches('/')), &dst, strict)?;
    }
    // Directories before their contents, removals of a directory as one whiteout.
    let mut ordered: Vec<&Change> = set.iter().collect();
    ordered.sort_by(|a, b| a.path.cmp(&b.path));
    for c in ordered {
        if !c.path.starts_with("/usr/") {
            return Err(format!("{} is outside /usr and cannot be activated live", c.path));
        }
        let dst = next.join(c.path.trim_start_matches('/'));
        if dst.symlink_metadata().is_ok() {
            continue; // inside a directory already copied or whited out
        }
        ensure_parents(&next, staged, &c.path, strict)?;
        match c.kind {
            ChangeKind::Removed => whiteout(&dst)?,
            _ => copy_entry(&staged.join(c.path.trim_start_matches('/')), &dst, strict)?,
        }
    }
    let rel_dir = next.join("usr/lib/extension-release.d");
    fs::create_dir_all(&rel_dir).map_err(|e| e.to_string())?;
    let booted = paths.live_root();
    let id = os_release_kv(&booted, "ID").unwrap_or_else(|| "fedora".into());
    let ver = os_release_kv(&booted, "VERSION_ID").unwrap_or_default();
    fs::write(
        rel_dir.join(format!("extension-release.{LAYER_NAME}")),
        format!("ID={id}\nVERSION_ID={ver}\nSYSEXT_SCOPE=system\nRIME_EXTENSION=live-update\n"),
    )
    .map_err(|e| e.to_string())?;
    Ok(next)
}

/// Paths (relative, absolute-style) a layer directory holds, files and
/// whiteouts only.
fn layer_entries(layer: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(md) = fs::symlink_metadata(&p) else { continue };
            if md.is_dir() {
                walk(base, &p, out);
            } else if let Ok(rel) = p.strip_prefix(base) {
                let rel = format!("/{}", rel.to_string_lossy());
                if !rel.starts_with("/usr/lib/extension-release.d/") {
                    out.push(rel);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(layer, layer, &mut out);
    out.sort();
    out
}

fn sysext_refresh(paths: &Paths) -> Result<(), String> {
    let _pkg = flock(&paths.at(PKG_LOCK), "rime package operation", 120)?;
    output("systemd-sysext", &["refresh"]).map(|_| ())
}

/// Put `next` in place, keeping the current layer to roll back to.
fn swap_in(paths: &Paths, next: &Path) -> Result<(), String> {
    let cur = paths.layer();
    let prev = paths.prev_layer();
    if prev.exists() {
        fs::remove_dir_all(&prev).map_err(|e| e.to_string())?;
    }
    if cur.exists() {
        fs::rename(&cur, &prev).map_err(|e| format!("{}: {e}", cur.display()))?;
    }
    fs::create_dir_all(cur.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::rename(next, &cur).map_err(|e| format!("{}: {e}", cur.display()))?;
    sysext_refresh(paths)
}

/// Put the previous layer (or none) back.
fn restore_prev(paths: &Paths) -> Result<(), String> {
    let cur = paths.layer();
    let prev = paths.prev_layer();
    if cur.exists() {
        fs::remove_dir_all(&cur).map_err(|e| format!("{}: {e}", cur.display()))?;
    }
    if prev.exists() {
        fs::rename(&prev, &cur).map_err(|e| format!("{}: {e}", prev.display()))?;
    }
    sysext_refresh(paths)
}

/// After a merge: every path in the set reads as the staged file (or is gone).
fn verify_files(paths: &Paths, staged: &Path, set: &[Change]) -> Result<(), String> {
    let live = paths.live_root();
    for c in set {
        let at = live.join(c.path.trim_start_matches('/'));
        match c.kind {
            ChangeKind::Removed => {
                if at.symlink_metadata().is_ok() {
                    return Err(format!("{} is still there after the merge", c.path));
                }
            }
            _ => {
                let src = staged.join(c.path.trim_start_matches('/'));
                let smd = fs::symlink_metadata(&src).map_err(|e| e.to_string())?;
                if smd.is_file() {
                    let a = fs::read(&at).map_err(|e| format!("{}: {e}", c.path))?;
                    let b = fs::read(&src).map_err(|e| format!("{}: {e}", c.path))?;
                    if a != b {
                        return Err(format!("{} does not read as the staged file after the merge", c.path));
                    }
                } else if smd.file_type().is_symlink() && fs::read_link(&at).ok() != fs::read_link(&src).ok() {
                    return Err(format!("{} does not point where the staged link does", c.path));
                }
            }
        }
    }
    Ok(())
}

// ── activators ───────────────────────────────────────────────────────────────

/// What one activator achieved, per component it served.
#[derive(Debug)]
enum ActResult {
    Done(String),
    /// Did not run, for a reason that is not a failure (a session locked
    /// between planning and acting).
    Deferred(DeferReason),
}

fn main_pid(unit: &str) -> Option<u32> {
    output("systemctl", &["show", "-p", "MainPID", "--value", unit]).ok()?.trim().parse().ok().filter(|p| *p > 0)
}

fn restart_unit(paths: &Paths, unit: &str) -> Result<ActResult, String> {
    output("systemctl", &["restart", unit])?;
    let active = output("systemctl", &["is-active", unit]).unwrap_or_default();
    if active.trim() != "active" {
        return Err(format!("{unit} is {} after the restart", active.trim()));
    }
    let pid = main_pid(unit).ok_or_else(|| format!("{unit} has no main process after the restart"))?;
    let exe = fs::read_link(paths.at(&format!("/proc/{pid}/exe"))).map_err(|e| format!("{unit}: {e}"))?;
    let exe_s = exe.to_string_lossy();
    if exe_s.ends_with(" (deleted)") {
        return Err(format!("{unit} is running a deleted binary"));
    }
    Ok(ActResult::Done(format!("{unit} restarted, pid {pid} runs {exe_s}")))
}

/// Run the per-session helper as `user` in their own service manager.
fn session_helper(sess: &SessionState, verb: &str, arg: &str) -> Result<ActResult, String> {
    if sess.user.is_empty() || !sess.user.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
        return Err(format!("implausible user name for session {}", sess.session_id));
    }
    let machine = format!("--machine={}@.host", sess.user);
    let out = Command::new("systemd-run")
        .args(["--user", &machine, "--wait", "--pipe", "--collect", "--quiet", "--service-type=exec"])
        .args([SESSION_HELPER, verb, arg, &sess.session_id])
        .output()
        .map_err(|e| format!("could not run systemd-run: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let line = text.lines().last().unwrap_or("").to_string();
    match out.status.code() {
        Some(0) => Ok(ActResult::Done(format!("{}: {line}", sess.user))),
        Some(3) => Ok(ActResult::Deferred(DeferReason::Locked(format!("{}: {line}", sess.user)))),
        Some(4) => Ok(ActResult::Done(format!("{}: not running in this session ({line})", sess.user))),
        c => Err(format!("{} {verb} in {}'s session exited {}: {}", SESSION_HELPER, sess.user, c.unwrap_or(-1), if line.is_empty() { last_lines(&err, 3) } else { line })),
    }
}

fn graphical(sessions: &[SessionState]) -> Vec<&SessionState> {
    let mut seen = HashSet::new();
    sessions
        .iter()
        .filter(|s| s.desktop != "gamescope")
        .filter(|s| seen.insert(s.uid)) // one shell per user
        .collect()
}

fn shell_revision(tree: &Path) -> String {
    fs::read_to_string(tree.join("usr/share/rime-shell/.rime-shell-commit")).map(|s| s.trim().to_string()).unwrap_or_default()
}

fn run_action(paths: &Paths, a: &Activator, sessions: &[SessionState], rev: &str) -> Result<ActResult, String> {
    match a {
        Activator::DaemonReload => output("systemctl", &["daemon-reload"]).map(|_| ActResult::Done("system manager reloaded".into())),
        Activator::UserDaemonReload => {
            for s in graphical(sessions) {
                let m = format!("--machine={}@.host", s.user);
                output("systemctl", &["--user", &m, "daemon-reload"])?;
            }
            Ok(ActResult::Done("user managers reloaded".into()))
        }
        Activator::RestartUnit(u) => {
            if !rimed_core::live::classify::RESTART_SAFE_UNITS.contains(&u.as_str()) {
                return Err(format!("{u} is not in the restart-safe list"));
            }
            restart_unit(paths, u)
        }
        Activator::Shell | Activator::HyprlandConfig => {
            let verb = if *a == Activator::Shell { "shell" } else { "hypr" };
            let mut done = Vec::new();
            for s in graphical(sessions) {
                match session_helper(s, verb, rev)? {
                    ActResult::Done(e) => done.push(e),
                    d @ ActResult::Deferred(_) => return Ok(d),
                }
            }
            if done.is_empty() {
                Ok(ActResult::Done("no graphical session is running; the next login uses it".into()))
            } else {
                Ok(ActResult::Done(done.join("; ")))
            }
        }
    }
}

fn components_of(a: &Activator, plan: &Plan, inputs: &[Classified]) -> BTreeSet<Component> {
    inputs
        .iter()
        .filter(|c| c.class.activator.as_ref() == Some(a) || matches!((a, &c.class.activator), (Activator::DaemonReload, Some(Activator::RestartUnit(_)))))
        .map(|c| c.class.component)
        .filter(|c| plan.component(*c).is_some_and(|p| matches!(p.decision, Decision::Activate { .. })))
        .collect()
}

// ── the update path ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default)]
pub struct LiveOptions {
    /// Stage, verify, plan, print; activate nothing, queue nothing.
    pub plan_only: bool,
    /// Activate live but leave the deployment download-only (not queued).
    pub live_only: bool,
    /// Stage and queue only; no live activation.
    pub no_live: bool,
    pub allow_unverified: bool,
}

/// Facts gathered for planning, kept for the activation that follows.
struct Measured {
    booted: Deployment,
    staged: Deployment,
    bdir: PathBuf,
    sdir: PathBuf,
    classified: Vec<Classified>,
    machine: MachineState,
}

fn measure(paths: &Paths, st: &BootcStatus) -> Result<(Measured, Inputs), String> {
    let booted = st.booted.clone().ok_or("bootc reports no booted deployment")?;
    let staged = st.staged.clone().ok_or("no staged deployment")?;
    let bdir = deploy_dir(paths, &booted)?;
    let sdir = deploy_dir(paths, &staged)?;
    let changes = measure_changes(paths, &booted, &staged, &bdir, &sdir)?;
    let changed: HashSet<String> = changes.iter().map(|c| c.path.clone()).collect();
    let classified = classify(&changes);
    let mut elf_res = BTreeMap::new();
    for c in classified.iter().filter(|c| c.class.elf_checked && c.change.kind != ChangeKind::Removed) {
        elf_res.insert(c.change.path.clone(), elf_closure(&bdir, &sdir, &c.change.path, &changed));
    }
    let packages = measure_packages(&bdir, &sdir).unwrap_or_else(|e| {
        eprintln!("rime: package versions not compared: {e}");
        Vec::new()
    });
    let machine = machine_state(paths);
    let inputs = Inputs {
        changes: classified.clone(),
        packages,
        elf: elf_res,
        running_units: running_units(),
        machine: machine.clone(),
        soft_reboot_capable: booted.soft_reboot_capable,
        no_live: false,
        only: None,
    };
    Ok((Measured { booted, staged, bdir, sdir, classified, machine }, inputs))
}

pub fn render_plan(p: &Plan) -> String {
    let mut s = String::new();
    for c in &p.components {
        if c.component == Component::Metadata {
            continue;
        }
        let what = match &c.decision {
            Decision::Activate { residual: None } => format!("live ({})", c.requirement.describe()),
            Decision::Activate { residual: Some(r) } => format!("live, then {}", r.describe()),
            Decision::Defer { reason } => reason.describe(),
            Decision::Pending { requirement } => format!("needs {}", requirement.describe()),
            Decision::NotApplicable { why } => why.clone(),
        };
        s.push_str(&format!("  {:<22} {:>4} file(s)  {what}\n", c.label, c.files));
        for v in &c.versions {
            s.push_str(&format!("  {:<22}            {v}\n", ""));
        }
        for n in &c.notes {
            s.push_str(&format!("  {:<22}            {n}\n", ""));
        }
    }
    if p.components.iter().all(|c| c.component == Component::Metadata) {
        s.push_str("  nothing this machine runs changes\n");
    }
    s.push_str(&format!("  afterwards: {}\n", p.recommendation));
    s
}

/// Close out an unfinished transaction from an earlier run.
fn recover(paths: &Paths) -> Result<(), String> {
    let Some(mut t) = load_txn(paths)? else { return Ok(()) };
    match txn::recover(&t, &boot_id(paths)) {
        Recovery::Nothing => Ok(()),
        // The record keeps the boot it was verified in: rewriting it here
        // would let `rime live apply` trust a verification from an earlier boot.
        Recovery::Supersede => advance(paths, &mut t, State::Superseded, "the machine restarted since"),
        Recovery::Abandon => advance(paths, &mut t, State::Failed, "interrupted before activation; nothing was changed live"),
        Recovery::RollBack => {
            eprintln!("rime: an earlier live update was interrupted mid-activation; undoing it");
            if t.state != State::RollingBack {
                advance(paths, &mut t, State::RollingBack, "recovering an interrupted activation")?;
            }
            let r = restore_prev(paths);
            let sessions = machine_state(paths).sessions;
            // The previous shell may predate the `shell` IPC target: accept any
            // shell that answers.
            let rev = "-".to_string();
            if let Some(p) = &t.plan {
                for a in &p.actions {
                    if let Err(e) = run_action(paths, a, &sessions, &rev) {
                        eprintln!("rime: re-running {a:?} during recovery: {e}");
                    }
                }
            }
            match r {
                Ok(()) => advance(paths, &mut t, State::RolledBack, "recovered"),
                Err(e) => advance(paths, &mut t, State::Failed, format!("recovery could not restore the layer: {e}")),
            }
        }
    }
}

/// Stage, verify, plan and activate. Called by `rime update` in place of
/// `bootc upgrade`, after the pre-pull gate. Returns an exit code.
pub fn update(opts: LiveOptions) -> i32 {
    let paths = Paths::from_env();
    let _lock = match flock(&paths.state_dir().join("lock"), "live update", 0) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("rime: {e}");
            return 1;
        }
    };
    if let Err(e) = recover(&paths) {
        eprintln!("rime: {e}");
    }

    match status_code("bootc", &["upgrade", "--download-only"]) {
        Ok(0) => {}
        Ok(c) => {
            eprintln!("rime: staging the update failed (bootc upgrade exited {c})");
            return c;
        }
        Err(e) => {
            eprintln!("rime: OS update failed: {e}");
            return 1;
        }
    }
    let st = match bootc_status() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("rime: {e}");
            return 1;
        }
    };
    let Some(staged) = st.staged.clone() else {
        println!("rime: the OS image is up to date");
        return 0;
    };
    let booted = st.booted.clone();
    let mut t = Txn::new(&new_txn_id(), &boot_id(&paths), booted.as_ref().map(|b| b.digest.as_str()).unwrap_or(""), now());
    t.target_digest = staged.digest.clone();
    t.soft_reboot_capable = booted.as_ref().is_some_and(|b| b.soft_reboot_capable);
    let fail = |paths: &Paths, t: &mut Txn, msg: String| -> i32 {
        eprintln!("rime: {msg}");
        let _ = advance(paths, t, State::Failed, msg);
        1
    };

    // The deployment that was really staged, verified by digest.
    if let Err(code) = crate::ops::verify_staged(opts.allow_unverified, &staged.image, &staged.digest) {
        let _ = advance(&paths, &mut t, State::Failed, "the staged image did not verify; it stays locked and will not boot");
        eprintln!("rime: the staged deployment is download-only and will not be booted.");
        return code;
    }
    if let Err(e) = advance(&paths, &mut t, State::Verified, format!("{} verified after staging", staged.digest)) {
        return fail(&paths, &mut t, e);
    }

    if !opts.plan_only && !opts.live_only && staged.download_only {
        match status_code("bootc", &["upgrade", "--from-downloaded"]) {
            Ok(0) => {}
            Ok(c) => return fail(&paths, &mut t, format!("queueing the verified deployment failed (bootc exited {c})")),
            Err(e) => return fail(&paths, &mut t, e),
        }
    }
    t.staged_for_boot = !staged.download_only || !(opts.plan_only || opts.live_only);
    let note = if t.staged_for_boot { "queued for the next boot" } else { "download-only" };
    if let Err(e) = advance(&paths, &mut t, State::Staged, note) {
        return fail(&paths, &mut t, e);
    }

    let (m, mut inputs) = match measure(&paths, &st) {
        Ok(x) => x,
        Err(e) => {
            // Nothing live; the staged deployment carries the whole release.
            eprintln!("rime: live activation skipped: {e}");
            t.outcomes.insert(Component::OtherSystem, Outcome::Deferred { reason: DeferReason::Unproven(e.clone()) });
            let _ = advance(&paths, &mut t, State::Planned, e);
            let _ = advance(&paths, &mut t, State::Deferred, "nothing could be measured");
            println!("rime: the update is staged and applies at the next restart");
            return 0;
        }
    };
    inputs.no_live = opts.no_live || opts.plan_only;
    let p = plan::plan(&inputs);
    t.target_release = release_of(&m.sdir);
    record_plan_outcomes(&mut t, &p);
    t.plan = Some(p.clone());
    if let Err(e) = advance(&paths, &mut t, State::Planned, "planned") {
        return fail(&paths, &mut t, e);
    }
    println!(
        "rime: {} → {}",
        release_of(&m.bdir).unwrap_or_else(|| short(&m.booted.digest)),
        t.target_release.clone().unwrap_or_else(|| short(&m.staged.digest))
    );
    print!("{}", render_plan(&p));

    if opts.plan_only {
        let _ = advance(&paths, &mut t, State::Deferred, "plan only");
        println!("rime: plan only; the update was downloaded and verified, not queued");
        return 0;
    }
    activate(&paths, &mut t, &m, &p)
}

fn short(d: &str) -> String {
    d.trim_start_matches("sha256:").chars().take(12).collect()
}

fn record_plan_outcomes(t: &mut Txn, p: &Plan) {
    t.outcomes.clear();
    for c in &p.components {
        let o = match &c.decision {
            Decision::Activate { .. } => Outcome::Deferred { reason: DeferReason::Unproven("not activated yet".into()) },
            Decision::Defer { reason } => Outcome::Deferred { reason: reason.clone() },
            Decision::Pending { requirement } => Outcome::Pending { requirement: *requirement },
            Decision::NotApplicable { why } => Outcome::NotApplicable { why: why.clone() },
        };
        t.outcomes.insert(c.component, o);
    }
}

fn activate(paths: &Paths, t: &mut Txn, m: &Measured, p: &Plan) -> i32 {
    if !p.activates_anything() {
        let final_state = if p.components.iter().any(|c| matches!(c.decision, Decision::Defer { .. })) { State::Deferred } else { State::Active };
        let _ = advance(paths, t, final_state, "nothing to activate live");
        finish_message(t);
        return 0;
    }
    if let Err(e) = advance(paths, t, State::Activating, format!("{} file(s) into the live layer", p.live_set.len())) {
        eprintln!("rime: {e}");
        return 1;
    }
    let activating: BTreeSet<Component> = p
        .components
        .iter()
        .filter(|c| matches!(c.decision, Decision::Activate { .. }))
        .map(|c| c.component)
        .collect();
    // Keep what an earlier live update put in place for components this one
    // does not activate: their running code is that version.
    let keep: Vec<String> = layer_entries(&paths.layer())
        .into_iter()
        .filter(|rel| {
            let comp = rimed_core::live::classify::classify_path(rel).component;
            !activating.contains(&comp)
        })
        .collect();
    type Evidence = Vec<(Component, String)>;
    let result = (|| -> Result<Evidence, (String, Option<Component>)> {
        let next = build_layer(paths, &m.sdir, &p.live_set, &keep).map_err(|e| (e, None))?;
        swap_in(paths, &next).map_err(|e| (format!("merging the live layer: {e}"), None))?;
        verify_files(paths, &m.sdir, &p.live_set).map_err(|e| (e, None))?;
        let rev = shell_revision(&m.sdir);
        let mut results = Vec::new();
        for a in &p.actions {
            let comps = components_of(a, p, &m.classified);
            match run_action(paths, a, &m.machine.sessions, &rev) {
                Ok(ActResult::Done(ev)) => {
                    for c in comps {
                        results.push((c, ev.clone()));
                    }
                }
                Ok(ActResult::Deferred(r)) => {
                    // A session locked between planning and acting: the shell
                    // must not keep new files under an old process.
                    return Err((r.describe(), comps.into_iter().next()));
                }
                Err(e) => return Err((e, comps.into_iter().next())),
            }
        }
        Ok(results)
    })();
    let _ = advance(paths, t, State::Verifying, "checking what runs");
    match result {
        Ok(results) => {
            for c in &activating {
                let cp = p.component(*c).unwrap();
                let ev = results
                    .iter()
                    .filter(|(rc, _)| rc == c)
                    .map(|(_, r)| r.clone())
                    .collect::<Vec<_>>()
                    .join("; ");
                let o = match cp.requirement {
                    Requirement::NextUse | Requirement::AppRestart => Outcome::ActiveAtNextUse {
                        evidence: if ev.is_empty() { format!("{} file(s) verified on disk", cp.files) } else { ev },
                    },
                    r => Outcome::Active { how: r, evidence: if ev.is_empty() { format!("{} file(s) verified on disk", cp.files) } else { ev } },
                };
                t.outcomes.insert(*c, o);
            }
            let _ = advance(paths, t, State::Active, "activated");
            finish_message(t);
            0
        }
        Err((err, culprit)) => {
            eprintln!("rime: live activation failed: {err}; undoing it");
            let _ = advance(paths, t, State::RollingBack, err.clone());
            let undo = restore_prev(paths);
            // The previous shell may predate the `shell` IPC target: accept any
            // shell that answers.
            let rev = "-".to_string();
            let mut undo_errors = Vec::new();
            if undo.is_ok() {
                for a in &p.actions {
                    if let Err(e) = run_action(paths, a, &m.machine.sessions, &rev) {
                        undo_errors.push(format!("{a:?}: {e}"));
                    }
                }
            }
            let locked = err.starts_with("deferred while the screen is locked");
            for c in &activating {
                let o = if locked {
                    Outcome::Deferred { reason: DeferReason::Locked(err.clone()) }
                } else if undo.is_err() || !undo_errors.is_empty() {
                    Outcome::Failed { error: err.clone() }
                } else if Some(*c) == culprit || culprit.is_none() {
                    Outcome::FailedRolledBack { error: err.clone() }
                } else {
                    Outcome::FailedRolledBack { error: format!("undone together with a failure elsewhere: {err}") }
                };
                t.outcomes.insert(*c, o);
            }
            match (&undo, undo_errors.is_empty()) {
                (Ok(()), true) => {
                    let _ = advance(paths, t, State::RolledBack, "undone; the update applies at the next restart");
                    finish_message(t);
                    if locked { 0 } else { 1 }
                }
                (Err(e), _) => {
                    let _ = advance(paths, t, State::Failed, format!("could not restore the previous layer: {e}"));
                    eprintln!("rime: could not undo the live layer: {e}. `rime live doctor` explains; a restart clears it.");
                    1
                }
                (Ok(()), false) => {
                    let _ = advance(paths, t, State::Failed, undo_errors.join("; "));
                    eprintln!("rime: the layer was removed, but re-activating the previous version failed: {}", undo_errors.join("; "));
                    1
                }
            }
        }
    }
}

fn finish_message(t: &Txn) {
    for (c, o) in &t.outcomes {
        if *c == Component::Metadata {
            continue;
        }
        println!("  {:<22} {}", c.label(), outcome_detail(o));
    }
    let r = remaining_after(t);
    println!("rime: {}", plan::recommend(r, t.soft_reboot_capable));
}

// ── `rime live …` ────────────────────────────────────────────────────────────

pub fn status(json_out: bool) -> i32 {
    let paths = Paths::from_env();
    match fs::read_to_string(paths.status_file()) {
        Ok(text) => {
            if json_out {
                println!("{}", text.trim());
                return 0;
            }
            let v: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("rime: {}: {e}", paths.status_file().display());
                    return 1;
                }
            };
            println!("{}", v["summary"].as_str().unwrap_or(""));
            println!(
                "  booted {}   target {}   state {}",
                v["booted"]["release"].as_str().unwrap_or("?"),
                v["target"]["release"].as_str().unwrap_or("none"),
                v["state"].as_str().unwrap_or("?")
            );
            for c in v["components"].as_array().into_iter().flatten() {
                println!(
                    "  {:<22} {:<20} {}",
                    c["label"].as_str().unwrap_or(""),
                    c["state"].as_str().unwrap_or(""),
                    c["detail"].as_str().unwrap_or("")
                );
            }
            println!("  next: {}", v["recommendation"].as_str().unwrap_or(""));
            0
        }
        Err(_) => {
            if json_out {
                println!("{}", json!({"schema": 1, "state": "idle", "components": [], "target": null}));
            } else {
                println!("no live update has run since this boot");
            }
            0
        }
    }
}

pub fn explain(component: Option<&str>) -> i32 {
    let paths = Paths::from_env();
    let t = match fs::read_to_string(paths.txn_file()).map_err(|e| e.to_string()).and_then(|s| Txn::parse(&s)) {
        Ok(t) => t,
        Err(_) => {
            println!("no live update transaction is recorded (run `sudo rime update --plan`)");
            return 0;
        }
    };
    let want = component.map(|c| Component::from_slug(c).ok_or(c));
    if let Some(Err(c)) = want {
        eprintln!("rime: unknown component {c}; one of: {}", Component::ALL.iter().map(|c| c.slug()).collect::<Vec<_>>().join(", "));
        return 2;
    }
    let want = want.map(|w| w.unwrap());
    let Some(p) = &t.plan else {
        println!("transaction {} has no plan ({})", t.id, state_name(t.state));
        return 0;
    };
    for c in &p.components {
        if want.is_some_and(|w| w != c.component) || c.component == Component::Metadata {
            continue;
        }
        println!("{} ({})", c.label, c.component.slug());
        println!("  changed files:  {}", c.files);
        println!("  needs:          {}", c.requirement.describe());
        println!("  live possible:  {}", if c.requirement.engine_may_apply() { "yes" } else { "no" });
        if let Some(o) = t.outcomes.get(&c.component) {
            println!("  outcome:        {}", outcome_detail(o));
        }
        for v in &c.versions {
            println!("  version:        {v}");
        }
        for n in &c.notes {
            println!("  note:           {n}");
        }
    }
    0
}

pub fn logs(json_out: bool, limit: usize) -> i32 {
    let paths = Paths::from_env();
    let text = fs::read_to_string(paths.history_file()).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    for l in &lines[lines.len().saturating_sub(limit)..] {
        if json_out {
            println!("{l}");
            continue;
        }
        let v: Value = serde_json::from_str(l).unwrap_or(Value::Null);
        println!(
            "{}  {:<12} {:<13} {}",
            v["at"].as_u64().unwrap_or(0),
            v["txn"].as_str().unwrap_or(""),
            v["state"].as_str().unwrap_or(""),
            v["note"].as_str().unwrap_or("")
        );
    }
    if lines.is_empty() && !json_out {
        println!("no live update history (the log is root-readable only: try sudo)");
    }
    0
}

/// `rime live apply [--only C…]`: activate what the current, verified
/// transaction left deferred.
pub fn apply(only: &[String]) -> i32 {
    let paths = Paths::from_env();
    let _lock = match flock(&paths.state_dir().join("lock"), "live update", 0) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("rime: {e}");
            return 1;
        }
    };
    if let Err(e) = recover(&paths) {
        eprintln!("rime: {e}");
    }
    let st = match bootc_status() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("rime: {e}");
            return 1;
        }
    };
    let Some(staged) = &st.staged else {
        println!("rime: nothing is staged; run `sudo rime update`");
        return 0;
    };
    // Only what this engine verified after staging is ever activated.
    let prev = load_txn(&paths).ok().flatten();
    let verified = prev.as_ref().is_some_and(|t| {
        t.target_digest == staged.digest
            && t.state != State::Superseded
            && t.history.iter().any(|s| s.state == State::Verified)
            && t.boot_id == boot_id(&paths)
    });
    if !verified {
        eprintln!("rime: the staged deployment was not verified by `rime update` in this boot; run `sudo rime update`");
        return 1;
    }
    let mut only_set = BTreeSet::new();
    for o in only {
        match Component::from_slug(o) {
            Some(c) => {
                only_set.insert(c);
            }
            None => {
                eprintln!("rime: unknown component {o}");
                return 2;
            }
        }
    }
    let (m, mut inputs) = match measure(&paths, &st) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("rime: {e}");
            return 1;
        }
    };
    if !only_set.is_empty() {
        inputs.only = Some(only_set);
    }
    let p = plan::plan(&inputs);
    let prev = prev.unwrap();
    let mut t = Txn::new(&new_txn_id(), &boot_id(&paths), &m.booted.digest, now());
    t.target_digest = staged.digest.clone();
    t.target_release = release_of(&m.sdir);
    t.staged_for_boot = !staged.download_only;
    t.soft_reboot_capable = m.booted.soft_reboot_capable;
    for (s, note) in [
        (State::Verified, format!("verified by transaction {}", prev.id)),
        (State::Staged, "already staged".to_string()),
    ] {
        if let Err(e) = advance(&paths, &mut t, s, note) {
            eprintln!("rime: {e}");
            return 1;
        }
    }
    record_plan_outcomes(&mut t, &p);
    t.plan = Some(p.clone());
    let _ = advance(&paths, &mut t, State::Planned, "planned for apply");
    print!("{}", render_plan(&p));
    activate(&paths, &mut t, &m, &p)
}

/// `rime live rollback`: remove the live layer and re-activate what the
/// deployment itself ships.
pub fn rollback() -> i32 {
    let paths = Paths::from_env();
    let _lock = match flock(&paths.state_dir().join("lock"), "live update", 0) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("rime: {e}");
            return 1;
        }
    };
    let Some(mut t) = load_txn(&paths).ok().flatten() else {
        println!("rime: no live update to roll back");
        return 0;
    };
    if t.state != State::Active || !paths.layer().exists() || t.boot_id != boot_id(&paths) {
        println!("rime: nothing is active live in this boot (last transaction: {})", state_name(t.state));
        return 0;
    }
    if let Err(e) = advance(&paths, &mut t, State::RollingBack, "requested") {
        eprintln!("rime: {e}");
        return 1;
    }
    let mut errors = Vec::new();
    let cur = paths.layer();
    if let Err(e) = fs::remove_dir_all(&cur).map_err(|e| e.to_string()).and_then(|_| sysext_refresh(&paths)) {
        errors.push(e);
    }
    let _ = fs::remove_dir_all(paths.prev_layer());
    let sessions = machine_state(&paths).sessions;
    // The previous shell may predate the `shell` IPC target: accept any
    // shell that answers.
    let rev = "-".to_string();
    if errors.is_empty() {
        if let Some(p) = t.plan.clone() {
            for a in &p.actions {
                if let Err(e) = run_action(&paths, a, &sessions, &rev) {
                    errors.push(format!("{a:?}: {e}"));
                }
            }
        }
    }
    let comps: Vec<Component> = t.outcomes.iter().filter(|(_, o)| matches!(o, Outcome::Active { .. } | Outcome::ActiveAtNextUse { .. })).map(|(c, _)| *c).collect();
    for c in comps {
        t.outcomes.insert(c, Outcome::Deferred { reason: DeferReason::NotRequested });
    }
    if errors.is_empty() {
        let _ = advance(&paths, &mut t, State::RolledBack, "live layer removed on request");
        println!("rime: the live layer is removed; this machine runs its booted deployment again");
        0
    } else {
        let _ = advance(&paths, &mut t, State::Failed, errors.join("; "));
        eprintln!("rime: rollback incomplete: {}", errors.join("; "));
        1
    }
}

// ── `rime live doctor` ───────────────────────────────────────────────────────

fn read_trim(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

fn kernel_config(paths: &Paths) -> Option<String> {
    let rel = read_trim(&paths.at("/proc/sys/kernel/osrelease"))?;
    fs::read_to_string(paths.at(&format!("/usr/lib/modules/{rel}/config"))).ok()
}

fn nvidia_facts(paths: &Paths) -> NvidiaFacts {
    let m = paths.at("/sys/module/nvidia");
    let holders = fs::read_dir(m.join("holders"))
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect())
        .unwrap_or_default();
    let modeset = read_trim(&paths.at("/sys/module/nvidia_drm/parameters/modeset")).as_deref() == Some("Y");
    let openers = if m.exists() { count_openers(paths) } else { Some(0) };
    let opted_in = fs::read_to_string(paths.at("/etc/rime/live.toml"))
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok())
        .and_then(|t| t.get("live")?.get("nvidia_reload")?.as_bool())
        .unwrap_or(false);
    NvidiaFacts {
        gpu_present: has_nvidia(paths).unwrap_or(false),
        module_loaded: m.exists(),
        refcnt: read_trim(&m.join("refcnt")).and_then(|s| s.parse().ok()),
        holders,
        openers,
        modeset,
        opted_in,
    }
}

/// Processes holding /dev/nvidia*. Needs root to see every process; None when
/// any process's descriptors could not be read.
fn count_openers(paths: &Paths) -> Option<usize> {
    let mut n = 0;
    for e in fs::read_dir(paths.at("/proc")).ok()?.flatten() {
        let name = e.file_name();
        if !name.to_string_lossy().chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let fds = match fs::read_dir(e.path().join("fd")) {
            Ok(f) => f,
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => return None,
            Err(_) => continue, // exited
        };
        if fds.flatten().any(|fd| fs::read_link(fd.path()).is_ok_and(|t| t.to_string_lossy().starts_with("/dev/nvidia"))) {
            n += 1;
        }
    }
    Some(n)
}

fn compositor_of(sessions: &[SessionState]) -> String {
    sessions.iter().map(|s| s.desktop.clone()).find(|d| !d.is_empty() && d != "gamescope").unwrap_or_default()
}

pub fn doctor(json_out: bool) -> i32 {
    let paths = Paths::from_env();
    let root = crate::ops::effective_uid() == Some(0);
    let mut checks: Vec<(&str, Verdict)> = Vec::new();

    match bootc_status() {
        Ok(st) => {
            let b = st.booted.clone();
            checks.push((
                "storage backend",
                match &b {
                    Some(d) if d.backend == "ostree" => Verdict::Available { evidence: "ostree deployment trees can be read".into() },
                    Some(d) => Verdict::Unavailable { why: format!("{} backend: live activation is not supported yet; updates apply at restart", d.backend) },
                    None => Verdict::Unknown { why: "no booted deployment reported".into() },
                },
            ));
            checks.push((
                "soft reboot",
                match &b {
                    Some(d) if d.soft_reboot_capable => Verdict::Available { evidence: "bootc reports softRebootCapable; only ever run on request".into() },
                    Some(_) => Verdict::Unavailable { why: "bootc reports this deployment cannot soft-reboot".into() },
                    None => Verdict::Unknown { why: "no booted deployment".into() },
                },
            ));
            checks.push((
                "staged update",
                match &st.staged {
                    Some(s) => Verdict::Available { evidence: format!("{}{}", short(&s.digest), if s.download_only { " (download-only, not queued)" } else { " (queued for the next boot)" }) },
                    None => Verdict::Unavailable { why: "nothing staged".into() },
                },
            ));
        }
        Err(e) => checks.push((
            "storage backend",
            Verdict::Unknown { why: if root { e } else { "bootc status needs root: run `sudo rime live doctor`".into() } },
        )),
    }
    checks.push((
        "live layer",
        if paths.layer().exists() {
            Verdict::Available { evidence: format!("{} ({} file(s))", paths.layer().display(), layer_entries(&paths.layer()).len()) }
        } else {
            Verdict::Unavailable { why: "no live layer in this boot".into() }
        },
    ));
    checks.push((
        "systemd-sysext",
        match output("systemd-sysext", &["--version"]) {
            Ok(v) => Verdict::Available { evidence: v.lines().next().unwrap_or("").to_string() },
            Err(e) => Verdict::Unavailable { why: e },
        },
    ));
    let txn = load_txn(&paths);
    checks.push((
        "last transaction",
        match txn {
            Ok(Some(t)) => match txn::recover(&t, &boot_id(&paths)) {
                Recovery::Nothing | Recovery::Supersede => Verdict::Available { evidence: format!("{} {}", t.id, state_name(t.state)) },
                r => Verdict::Unavailable { why: format!("{} was interrupted in {}; the next update runs {:?}", t.id, state_name(t.state), r) },
            },
            Ok(None) => Verdict::Available { evidence: "none recorded".into() },
            Err(e) => Verdict::Unknown { why: if root { e } else { "the record is root-only".into() } },
        },
    ));
    let cfg = kernel_config(&paths);
    let sig = read_trim(&paths.at("/sys/module/module/parameters/sig_enforce")).map(|s| s == "Y");
    let lockdown = read_trim(&paths.at("/sys/kernel/security/lockdown"));
    let sb = output("mokutil", &["--sb-state"]).ok().map(|s| s.contains("enabled"));
    checks.push(("module signatures", caps::module_signing(sig, lockdown.as_deref(), sb)));
    checks.push(("kernel livepatch", caps::livepatch(cfg.as_deref(), paths.at("/sys/kernel/livepatch").exists(), sig)));
    let cmdline = read_trim(&paths.at("/proc/cmdline")).unwrap_or_default();
    checks.push(("kexec handover (LUO/KHO)", caps::luo_kho(cfg.as_deref(), &cmdline, paths.at("/dev/liveupdate").exists())));
    checks.push(("NVIDIA driver reload", caps::nvidia_reload(&nvidia_facts(&paths))));
    let sessions = sessions().unwrap_or_default();
    checks.push(("compositor handover", caps::compositor_handover(&compositor_of(&sessions))));
    checks.push((
        "bootloader",
        match output("bootupctl", &["status", "--json"]).and_then(|t| caps::bootloader_update(&t)) {
            Ok(None) => Verdict::Available { evidence: "installed bootloader matches the image; changes apply at boot".into() },
            Ok(Some(u)) => Verdict::Unavailable { why: format!("an update is available ({u}); it applies at boot") },
            Err(e) => Verdict::Unknown { why: if root { e } else { "needs root".into() } },
        },
    ));
    checks.push((
        "firmware",
        match output("fwupdmgr", &["get-devices", "--json"]).and_then(|t| caps::firmware_needs_restart(&t)) {
            Ok(v) if v.is_empty() => Verdict::Available { evidence: "no device has firmware waiting on a restart".into() },
            Ok(v) => Verdict::Unavailable { why: format!("waiting on a restart: {}", v.join(", ")) },
            Err(e) => Verdict::Unknown { why: e },
        },
    ));

    if json_out {
        let v: Vec<Value> = checks.iter().map(|(k, v)| json!({"check": k, "result": v})).collect();
        println!("{}", serde_json::to_string_pretty(&json!({"schema": 1, "checks": v})).unwrap_or_default());
    } else {
        for (k, v) in &checks {
            println!("  {:<26} {}", k, v.sentence());
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOTC: &str = include_str!("../tests/fixtures/live/bootc-status-staged.json");

    #[test]
    fn parses_bootc_status() {
        let s = parse_bootc_status(BOOTC).unwrap();
        let b = s.booted.unwrap();
        assert_eq!(b.backend, "ostree");
        assert!(b.soft_reboot_capable);
        assert_eq!(b.checksum.as_deref().unwrap().len(), 64);
        let st = s.staged.unwrap();
        assert!(st.download_only);
        assert!(st.digest.starts_with("sha256:"));
    }

    #[test]
    fn session_parsing_is_conservative() {
        let s = parse_show_session("2", "Type=wayland\nClass=user\nRemote=no\nState=active\nDesktop=Hyprland\nName=andre\nUser=1000\nLockedHint=no\n").unwrap();
        assert_eq!(s.locked, Some(false));
        assert_eq!(s.desktop, "hyprland");
        let s = parse_show_session("3", "Type=wayland\nClass=user\nRemote=no\nState=active\nDesktop=gamescope\nName=andre\nUser=1000\nLockedHint=no\n").unwrap();
        assert_eq!(s.desktop, "gamescope");
        let s = parse_show_session("4", "Type=wayland\nClass=user\nRemote=no\nName=andre\nUser=1000\n").unwrap();
        assert_eq!(s.locked, None, "a missing hint is not 'unlocked'");
        assert!(parse_show_session("5", "Type=tty\nClass=user\n").is_none());
        assert!(parse_show_session("6", "Type=wayland\nClass=greeter\n").is_none());
    }

    #[test]
    fn normalise_and_resolve() {
        assert_eq!(normalise("/usr/lib64/../lib64/./libc.so.6"), "/usr/lib64/libc.so.6");
        let d = std::env::temp_dir().join(format!("rime-live-resolve-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("usr/lib64")).unwrap();
        fs::write(d.join("usr/lib64/libx.so.1.2"), b"x").unwrap();
        std::os::unix::fs::symlink("libx.so.1.2", d.join("usr/lib64/libx.so.1")).unwrap();
        std::os::unix::fs::symlink("/usr/lib64/libx.so.1", d.join("usr/lib64/libx.so")).unwrap();
        assert_eq!(resolve_in(&d, "/usr/lib64/libx.so").as_deref(), Some("/usr/lib64/libx.so.1.2"));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn status_document_follows_the_contract() {
        let mut t = Txn::new("1", "b", "sha256:aa", 0);
        t.target_digest = "sha256:bb".into();
        t.outcomes.insert(Component::Shell, Outcome::Active { how: Requirement::ServiceRestart, evidence: "rev x".into() });
        t.outcomes.insert(Component::Kernel, Outcome::Pending { requirement: Requirement::KernelTransition });
        t.outcomes.insert(Component::Metadata, Outcome::Pending { requirement: Requirement::Nothing });
        let v = status_json(&t, Some("2026.10.10"));
        assert_eq!(v["schema"], 1);
        assert_eq!(v["state"], "discovered");
        assert_eq!(v["booted"]["release"], "2026.10.10");
        assert_eq!(v["target"]["digest"], "sha256:bb");
        let comps = v["components"].as_array().unwrap();
        assert_eq!(comps.len(), 2, "metadata is not a component users see");
        assert!(comps.iter().any(|c| c["component"] == "shell" && c["state"] == "active"));
        assert_eq!(v["remaining"], "kernel-transition");
    }

    #[test]
    fn a_deferred_component_still_needs_a_restart() {
        let mut t = Txn::new("1", "b", "d", 0);
        t.outcomes.insert(Component::Shell, Outcome::Deferred { reason: DeferReason::Locked("x".into()) });
        assert_eq!(remaining_after(&t), Requirement::Reboot, "no plan: assume the worst");
    }
}
