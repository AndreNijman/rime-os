//! The NixOS backend of `rime update`, `rime rollback`, `rime pin`, `rime gc` and `rime trust --gate`
//! (docs/nixos/updates-and-release.md, D6; implementation notes in docs/nixos/update-client.md).
//!
//! Selected at runtime by [`is_nixos`]; the bootc path in `ops.rs` is untouched when it says no. The decisions are
//! pure and live in `rimed_core::nixrel`; this module fetches, runs `nix` and measures the machine.
//!
//! Every program is run by the absolute path the NixOS module wrote into `/etc/rime/update.toml` (store paths), or
//! from a generation's own `bin/switch-to-configuration`; never by a PATH lookup.

pub mod fetch;
pub mod generations;
pub mod store;
pub mod trust;
pub mod update;

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const CONFIG: &str = "/etc/rime/update.toml";
pub const STATE_DIR: &str = "/var/lib/rime/nix";
/// The live engine's lock: update, live apply/rollback, rollback and gc never overlap (§8.2 step 1).
pub const LOCK: &str = "/var/lib/rime/live/lock";
pub const STATUS: &str = "/run/rime-live/status.json";
/// The checker's state file the Shell's update notice reads (`state=available|none`, `digest=…`).
pub const CHECK_STATE: &str = "/run/rime-update/state";
pub const GCROOTS: &str = "/nix/var/nix/gcroots/rime";
pub const PROFILE: &str = rimed_core::nixrel::SYSTEM_PROFILE;

/// NixOS when `/etc/NIXOS` exists and `/run/current-system` points into the store (§8.1).
pub fn is_nixos() -> bool {
    Path::new("/etc/NIXOS").exists()
        && fs::read_link("/run/current-system").map(|p| p.starts_with("/nix/store")).unwrap_or(false)
}

/// `/etc/rime/update.toml`, written by `nix/modules/update.nix`. Nothing here comes from the network.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub repository: String,
    pub channel: String,
    pub variant: String,
    pub cache_url: String,
    /// The compiled-in narinfo keys: used only under `--allow-unverified` (§6.4); otherwise the verified
    /// manifest's `cache.keys` are the only keys a download is checked against.
    pub cache_keys: Vec<String>,
    pub signers: Vec<String>,
    pub issuer: String,
    #[serde(default)]
    pub fulcio_root: Option<String>,
    pub nix: String,
    pub nix_env: String,
    pub nix_store: String,
    #[serde(default = "three")]
    pub keep_generations: usize,
}

fn three() -> usize {
    3
}

impl Config {
    pub fn load() -> Result<Config, String> {
        let text = fs::read_to_string(CONFIG).map_err(|e| format!("{CONFIG}: {e}"))?;
        let mut c: Config = toml::from_str(&text).map_err(|e| format!("{CONFIG}: {e}"))?;
        // `rime channel set` on NixOS writes this file (§12); the module's value is the default.
        if let Ok(ch) = fs::read_to_string(Path::new(STATE_DIR).join("channel")) {
            let ch = ch.trim();
            if !ch.is_empty() {
                c.channel = ch.to_string();
            }
        }
        Ok(c)
    }

    pub fn expect(&self) -> crate::verify::Expect {
        crate::verify::Expect {
            signers: self.signers.clone(),
            issuer: self.issuer.clone(),
            fulcio_root: self.fulcio_root.clone(),
        }
    }
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ── state under /var/lib/rime/nix ────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppliedRelease {
    pub release: String,
    /// The manifest digest the channel named.
    pub digest: String,
    pub toplevel: String,
    #[serde(default)]
    pub generation: Option<u32>,
    pub at: u64,
}

/// `applied.json`: release ↔ generation ↔ toplevel, and the downgrade high-water mark (§8.2 step 6).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Applied {
    #[serde(default)]
    pub releases: Vec<AppliedRelease>,
    /// Set when a slower-channel consent reset the bar; otherwise the newest release in `releases`.
    #[serde(default)]
    pub high_water: Option<String>,
}

impl Applied {
    pub fn load() -> Applied {
        read_json(&state("applied.json")).unwrap_or_default()
    }
    pub fn save(&self) -> Result<(), String> {
        write_json(&state("applied.json"), self, 0o644)
    }
    pub fn high_water(&self) -> Option<String> {
        self.high_water.clone().or_else(|| {
            rimed_core::nixrel::release::newest(self.releases.iter().map(|r| r.release.as_str())).map(str::to_string)
        })
    }
    pub fn by_toplevel(&self, toplevel: &str) -> Option<&AppliedRelease> {
        self.releases.iter().rev().find(|r| r.toplevel == toplevel)
    }
    pub fn record(&mut self, r: AppliedRelease) {
        self.releases.retain(|x| !(x.release == r.release && x.toplevel == r.toplevel));
        self.releases.push(r);
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Pins {
    #[serde(default)]
    pub generations: BTreeSet<u32>,
    /// `rime update --pin <release>`: while set, `rime update` installs nothing newer.
    #[serde(default)]
    pub hold: Option<String>,
}

impl Pins {
    pub fn load() -> Pins {
        read_json(&state("pins.json")).unwrap_or_default()
    }
    pub fn save(&self) -> Result<(), String> {
        write_json(&state("pins.json"), self, 0o644)
    }
}

pub fn state(name: &str) -> PathBuf {
    Path::new(STATE_DIR).join(name)
}

pub fn read_json<T: for<'de> Deserialize<'de>>(p: &Path) -> Option<T> {
    serde_json::from_str(&fs::read_to_string(p).ok()?).ok()
}

pub fn write_json<T: Serialize>(p: &Path, v: &T, mode: u32) -> Result<(), String> {
    let text = serde_json::to_vec_pretty(v).map_err(|e| e.to_string())?;
    write_atomic(p, &text, mode)
}

pub fn write_atomic(path: &Path, data: &[u8], mode: u32) -> Result<(), String> {
    let dir = path.parent().ok_or("no parent directory")?;
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".{}.tmp", path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()));
    {
        let mut f = fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        f.write_all(data).map_err(|e| e.to_string())?;
        f.set_permissions(fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The live engine's lock file, held for the life of the value.
pub struct Lock(#[allow(dead_code)] fs::File);

pub fn lock(what: &str) -> Result<Lock, String> {
    let p = Path::new(LOCK);
    if let Some(d) = p.parent() {
        fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let f = fs::OpenOptions::new().create(true).truncate(false).write(true).open(p).map_err(|e| format!("{LOCK}: {e}"))?;
    use std::os::unix::io::AsRawFd;
    // SAFETY: flock on a descriptor we own.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        Ok(Lock(f))
    } else {
        Err(format!("rime: another update, rollback or gc is in progress; {what} was not started"))
    }
}

// ── running programs ─────────────────────────────────────────────────────────

/// A finished program: exit status, stdout, stderr.
pub struct Ran {
    pub ok: bool,
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Ran {
    /// The last few stderr lines, for a message.
    pub fn why(&self) -> String {
        let lines: Vec<&str> = self.stderr.lines().filter(|l| !l.trim().is_empty()).collect();
        let tail = lines[lines.len().saturating_sub(3)..].join("; ");
        if tail.is_empty() {
            format!("exit {}", self.code)
        } else {
            tail
        }
    }
}

pub fn run(program: &str, args: &[&str], stdin: Option<&str>) -> Result<Ran, String> {
    let mut cmd = Command::new(program);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child = cmd.spawn().map_err(|e| format!("could not run {program}: {e}"))?;
    if let (Some(text), Some(mut w)) = (stdin, child.stdin.take()) {
        w.write_all(text.as_bytes()).map_err(|e| format!("{program}: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("{program}: {e}"))?;
    Ok(Ran {
        ok: out.status.success(),
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Like [`run`], with stderr passed through to the user (long-running steps: the download, activation).
pub fn run_visible(program: &str, args: &[&str], stdin: Option<&str>) -> Result<Ran, String> {
    let mut cmd = Command::new(program);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child = cmd.spawn().map_err(|e| format!("could not run {program}: {e}"))?;
    if let (Some(text), Some(mut w)) = (stdin, child.stdin.take()) {
        w.write_all(text.as_bytes()).map_err(|e| format!("{program}: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("{program}: {e}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    eprint!("{stderr}");
    Ok(Ran {
        ok: out.status.success(),
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr,
    })
}

// ── the status document ──────────────────────────────────────────────────────

/// `/run/rime-live/status.json`, schema 1 (docs/live-update.md), plus the NixOS fields of §10.1: `product`,
/// `booted.toplevel`, `target.toplevel` and `generations`. Rime Shell's `liveupdate.js` reads the schema-1 keys
/// and drops the rest.
pub struct Status<'a> {
    pub state: &'a str,
    pub booted_release: Option<String>,
    pub booted_digest: Option<String>,
    pub booted_toplevel: Option<String>,
    pub target: Option<(&'a str, &'a str, &'a str)>,
    pub staged_for_boot: bool,
    pub verified: bool,
    pub components: Vec<Value>,
    pub remaining: &'a str,
    pub recommendation: &'a str,
    pub summary: String,
}

pub fn write_status(s: &Status) {
    let g = generations::list().ok();
    let v = json!({
        "schema": 1,
        "product": "nixos",
        "updated": now(),
        "txn": format!("nix-{}", now()),
        "state": s.state,
        "booted": {"digest": s.booted_digest, "release": s.booted_release, "toplevel": s.booted_toplevel},
        "from": {"digest": s.booted_digest},
        "target": match s.target {
            Some((digest, release, toplevel)) => json!({"digest": digest, "release": release, "toplevel": toplevel}),
            None => Value::Null,
        },
        "staged_for_boot": s.staged_for_boot,
        "verified": s.verified,
        "components": s.components,
        "remaining": s.remaining,
        "recommendation": s.recommendation,
        "summary": s.summary,
        "generations": g.map(|g| json!({
            "current": g.current, "booted": g.booted, "previous": g.previous(), "pinned": Pins::load().generations,
        })),
    });
    if let Err(e) = write_json(Path::new(STATUS), &v, 0o644) {
        eprintln!("rime: could not write {STATUS}: {e}");
    }
}

/// The release the running (`/run/booted-system`) generation was installed as, from `applied.json`.
pub fn release_of(toplevel: &str) -> Option<AppliedRelease> {
    Applied::load().by_toplevel(toplevel).cloned()
}

pub fn resolve_link(p: &str) -> Option<String> {
    fs::canonicalize(p).ok().map(|p| p.display().to_string())
}
