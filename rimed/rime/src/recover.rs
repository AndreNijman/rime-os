//! `rime recover` and `rime doctor --json` — roadmap §19's recovery surface,
//! automatic repair, and the scoped reset.
//!
//! ## `status` spawns nothing, and that is a property rather than an accident
//!
//! Every fact on the recovery surface is a file read. No `bootc`, no `ostree`,
//! no `nvidia-smi`, no `systemctl`, no D-Bus, no packet. Three things follow,
//! and each of them is asserted in `tests/test-rime-recover.sh`:
//!
//! * it cannot raise an authentication prompt, because nothing it does needs
//!   authorising;
//! * it cannot be slow or hang, so Rime Settings can poll it;
//! * it is exercisable against a fixture tree, so the states the developer's
//!   own machine does not have — a missing rollback target, an extension built
//!   for the previous release, `/usr` mounted read-write — are covered by
//!   assertions instead of by reasoning.
//!
//! The deliberate cost is that the deployment row reports the ostree checksum
//! rather than the image reference: the reference lives in `bootc status`, and
//! parsing another tool's JSON schema to duplicate what `rime changelog`
//! already prints would buy a second thing to keep in sync. The row names
//! `rime changelog` instead.
//!
//! ## `repair` converges the domain it is in, and reports the other
//!
//! The same split `rime apply` uses. A repair verb that demanded root would
//! make the half that fixes a broken desktop reachable only by running it as
//! the user who has no desktop, and nothing here ever calls `sudo` — so
//! `rime recover repair` is structurally incapable of producing the
//! authentication prompt this project has asked twice never to see.
//!
//! The steps themselves are in [`rimed_core::recover::REPAIRS`], where a test
//! asserts the invariant that makes a single button defensible: every step is
//! idempotent and removes no data. §19 lists repair, "boot previous
//! deployment" and "factory reset" as three separate actions precisely because
//! they carry three different consequences, and collapsing them would be the
//! whole point missed.
//!
//! ## `reset` is the destructive verb, so it is built to be refused
//!
//! Dry run is the default and cannot be turned off by a flag alone. Performing
//! it needs `--commit` **and** `--confirm <token>`, where the token is derived
//! from the scope *and the exact set of paths the plan found*. A caller cannot
//! construct it from the scope; it has to run the plan, which is the step that
//! prints the loss list. A machine that changed between plan and commit
//! produces a different token and the commit is refused with nothing touched.
//!
//! On top of that: it refuses to run as root, it validates `$HOME` before
//! resolving anything under it, every target is re-resolved and asserted to be
//! inside the home before removal, everything it removes is copied to a
//! backup directory outside every target first, and after the commit it
//! re-checks every preserved landmark that existed beforehand. Grepping for
//! what you deleted cannot detect what you deleted as well; the landmark check
//! is what can.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use rimed_core::recover::{
    confirm_token, preserved, targets, Disposition, Domain, Health, Kind, RepairStep, ResetScope,
    Target, PRESERVED_LANDMARKS, REPAIRS,
};
use clap::{Args, Subcommand};
use serde_json::{json, Value};

use crate::ops::LocalView;

#[derive(Subcommand)]
pub enum RecoverCmd {
    /// The recovery surface: every component §19 lists, with its state and the
    /// action that addresses it.
    ///
    /// Read-only, root-free, and it spawns no subprocess at all — so it is
    /// safe to poll from Rime Settings and can never raise an authentication
    /// prompt. Exits non-zero when any component needs attention, so it is
    /// usable as a check.
    Status {
        /// Emit machine-readable JSON instead of a report.
        #[arg(long)]
        json: bool,
    },
    /// Run the repairs that are idempotent and remove nothing.
    ///
    /// A DRY RUN unless `--commit` is given. Converges only the privilege
    /// domain it is already running in and reports the other, exactly as
    /// `rime apply` does: `rime recover repair` re-seeds your desktop,
    /// `sudo rime recover repair` rebuilds the package extension. Nothing here
    /// calls sudo itself.
    ///
    /// Rollback and factory reset are deliberately NOT repairs. §19 lists them
    /// as separate actions because they carry consequences the user has to see
    /// first, and `rime recover repair` will never perform either.
    Repair(RepairArgs),
    /// Reset Rime-owned state for this account, back to what the image
    /// provisions.
    ///
    /// A DRY RUN unless BOTH `--commit` and a matching `--confirm` are given.
    /// The dry run prints, per path, exactly what is removed and exactly what
    /// is preserved, and then the one command line that performs it — carrying
    /// a token derived from that plan, so a confirmation cannot be constructed
    /// without having seen the list.
    ///
    /// `--scope desktop` is settings, keybinds and caches. `--scope user` adds
    /// your blueprint, per-game profiles, trusted devices, local-model
    /// settings and recorded agent sessions. Neither touches a document, a
    /// checkout, a credential, a capsule, an installed package or the booted
    /// deployment — `rime recover reset --scope user` prints the full list.
    ///
    /// A full factory reset — accounts removed, `/etc` restored, disks
    /// repartitioned — is the installer's job and not a verb on a running
    /// system. `docs/recovery.md` says why.
    Reset(ResetArgs),
}

#[derive(Args)]
pub struct RepairArgs {
    /// Actually run the applicable steps. Without it, nothing is changed.
    #[arg(long)]
    pub commit: bool,
    /// Emit the plan as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct ResetArgs {
    /// How far the reset reaches: desktop or user.
    ///
    /// Required, and there is no default. A destructive verb whose blast
    /// radius came from a default is one nobody can review.
    #[arg(long, value_name = "SCOPE")]
    pub scope: String,
    /// Actually perform it. Requires a matching --confirm.
    #[arg(long)]
    pub commit: bool,
    /// The token the dry run printed. Bound to the plan, not to the scope.
    #[arg(long, value_name = "TOKEN")]
    pub confirm: Option<String>,
    /// Skip re-seeding the desktop afterwards.
    ///
    /// Without this, a reset refuses to start when the provisioner is missing:
    /// removing the files Rime Shell needs and having no way to put them back
    /// is a worse state than the one being left. Use it only if you want the
    /// deletion alone.
    #[arg(long)]
    pub no_reprovision: bool,
    /// Emit the plan as JSON.
    #[arg(long)]
    pub json: bool,
}

// ── the fixture root ─────────────────────────────────────────────────────────

/// Where the system half of the surface reads from.
///
/// A prefix, and only a prefix — the same shape `rime boot status` uses, for
/// the same reason: the interesting states are ones a healthy machine does not
/// have, so they have to be presentable as a tree. It also maps the
/// provisioner's path, so no environment variable ever names a *program*: a
/// caller-controlled program name is a hole even in an unprivileged command,
/// because nothing stops root from running it.
struct Sys {
    fixture: Option<PathBuf>,
}

impl Sys {
    fn from_env() -> Sys {
        Sys {
            fixture: std::env::var_os("RIME_RECOVER_ROOT")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
        }
    }

    fn path(&self, absolute: &str) -> PathBuf {
        match &self.fixture {
            // `absolute` always starts with '/', so strip it before joining, or
            // Path::join discards the prefix and silently reads the real
            // system — a fixture that reads /sys is worse than no fixture at
            // all, because the test then passes on the author's machine only.
            Some(root) => root.join(absolute.trim_start_matches('/')),
            None => PathBuf::from(absolute),
        }
    }

    fn read(&self, absolute: &str) -> Option<String> {
        std::fs::read_to_string(self.path(absolute)).ok()
    }

    /// Like [`Sys::read`], but keeps the reason the read failed.
    ///
    /// `read` answers `None` for "absent" and for "present, and you may not
    /// look at it", which are different answers to a user. The `/proc` and
    /// `/sys` callers do not care, because a file they cannot read is a file
    /// they cannot use either way. A caller that reports the *state of the
    /// machine* does care: saying "no packages installed" when the truth is
    /// "0700, ask root" tells the user something false about their own system.
    fn read_result(&self, absolute: &str) -> std::io::Result<String> {
        std::fs::read_to_string(self.path(absolute))
    }

    fn exists(&self, absolute: &str) -> bool {
        self.path(absolute).exists()
    }
}

/// The per-user provisioner. Re-seeds everything a reset removes, is idempotent
/// by design, runs at every login already, and needs no network.
const PROVISIONER: &str = "/usr/libexec/rime-shell-firstrun";

// ── doctor, made machine-readable ────────────────────────────────────────────

/// One line of `rime doctor`.
///
/// §19 asks for "`rime doctor` results graphically", which from the OS side
/// means the same checks in a shape a UI can render — not a second set of
/// checks that can disagree with the text one. So `doctor` builds this list
/// once and renders it either way.
pub struct Check {
    pub ok: bool,
    pub what: String,
}

/// Render the doctor's checks as text or JSON.
///
/// ## Why there is no severity field
///
/// `rime doctor` reports what this machine has, and its own comment says a
/// WARN is information rather than a fault: a laptop with no ACPI
/// `platform_profile` is not broken. Adding a severity would mean inventing a
/// judgement the checks do not make, and a UI painting an invented judgement
/// red is worse than one showing two states. So the JSON carries exactly what
/// the text carries — a boolean and a sentence — plus the counts, so a summary
/// badge needs no client-side arithmetic.
pub fn render_doctor(checks: &[Check], json: bool) -> String {
    if !json {
        let mut out = String::new();
        for c in checks {
            let _ = writeln!(out, "[{}] {}", if c.ok { "PASS" } else { "WARN" }, c.what);
        }
        return out;
    }
    let passed = checks.iter().filter(|c| c.ok).count();
    let doc = json!({
        "checks": checks
            .iter()
            .map(|c| json!({"ok": c.ok, "check": c.what}))
            .collect::<Vec<_>>(),
        "passed": passed,
        "warned": checks.len() - passed,
        "total": checks.len(),
    });
    format!("{}\n", serde_json::to_string_pretty(&doc).unwrap_or_default())
}

/// Every check `rime doctor` performs.
///
/// `daemon_running` is a parameter rather than something read here, so this is
/// a synchronous function that needs no bus — which is also what makes it
/// callable from a test.
pub fn doctor_checks(v: &LocalView, daemon_running: bool) -> Vec<Check> {
    let mut out: Vec<Check> = Vec::new();
    let mut line = |ok: bool, what: String| out.push(Check { ok, what });

    line(
        daemon_running,
        "rimed running (owns org.rimeos.Rimed1)".to_string(),
    );
    line(
        true,
        format!(
            "profile resolved: active={} class={} device={}",
            v.selection.active,
            v.selection.class_or_empty(),
            v.selection.device_or_empty()
        ),
    );

    // Every check below reports what this machine has; a WARN is information,
    // not a fault. Nothing here is required for rimed to work.
    let driver = v.fingerprint.cpu.scaling_driver.as_deref().unwrap_or("");
    let driver_name = if driver.is_empty() { "none" } else { driver };
    line(
        !driver.is_empty(),
        format!("cpufreq scaling driver present ({driver_name})"),
    );
    line(
        v.fingerprint.cpu.amd_pstate() || v.fingerprint.cpu.intel_pstate(),
        format!(
            "EPP-capable scaling driver ({driver_name}) — without it, tiers use the governor alone"
        ),
    );
    line(
        Path::new("/sys/firmware/acpi/platform_profile").exists(),
        format!(
            "ACPI platform_profile present (choices: {})",
            crate::read_sys("firmware/acpi/platform_profile_choices")
                .unwrap_or_else(|| "none".into())
        ),
    );

    let inv = rimed_core::BatteryInventory::detect();
    line(!inv.is_empty(), format!("battery discovery: {}", inv.summary()));
    if !inv.is_empty() {
        line(
            inv.supports_thresholds(),
            format!(
                "charge threshold control present ({})",
                inv.threshold_support().as_str()
            ),
        );
    }

    for (ok, what) in crate::touchpad::doctor_lines() {
        line(ok, what);
    }

    let s2idle = crate::read_sys("power/mem_sleep")
        .map(|s| s.contains("[s2idle]"))
        .unwrap_or(false);
    line(s2idle, "s2idle is the active suspend mode".to_string());

    // ── M6: fan control and game orchestration ──────────────────────────────
    let fan_cfg = v.active_profile().fan_config();
    let fans = rimed_core::fan::FanInventory::discover(Path::new("/sys"), &fan_cfg);
    let fan_names = if fans.controls.is_empty() && fans.msi_ec.is_none() {
        "none".to_string()
    } else {
        let mut s: Vec<String> = fans.controls.iter().map(|c| c.id.clone()).collect();
        if fans.msi_ec.is_some() {
            s.push("msi-ec".into());
        }
        s.join(", ")
    };
    line(
        fans.controllable(),
        format!("fan control channel present, write access unverified ({fan_names})"),
    );
    let topo = rimed_core::CoreTopology::detect_from(Path::new("/sys"));
    if v.fingerprint.cpu.hybrid {
        line(
            topo.is_hybrid(),
            format!(
                "P/E split detected via {} (P={} E={})",
                topo.source.as_str(),
                topo.pcore_list(),
                topo.ecore_list()
            ),
        );
    }
    if v.fingerprint
        .gpus
        .iter()
        .any(|g| g.vendor == rimed_core::GpuVendor::Nvidia)
    {
        line(
            rimed_core::gpu::nvidia_smi_available(),
            "nvidia-smi on PATH (needed for game-mode clock locks)".to_string(),
        );
    }
    line(
        Path::new("/sys/fs/cgroup/cgroup.controllers").exists(),
        "cgroup v2 present (needed for game-mode cpuset pinning)".to_string(),
    );
    out
}

// ── the recovery surface ─────────────────────────────────────────────────────

/// One rendered row.
struct Row {
    id: &'static str,
    label: &'static str,
    state: Health,
    detail: String,
    /// The command that addresses this row, when one exists. Not every row has
    /// an action — "no default route" is not something a CLI verb fixes.
    action: Option<String>,
}

/// A recovery route: a way back into a working system.
struct Route {
    id: &'static str,
    /// `None` means "cannot be determined from a running system" — which is
    /// the honest answer for installer media and is not the same as "no".
    available: Option<bool>,
    how: String,
}

/// The whole surface, probed.
struct Surface {
    bootloader: &'static str,
    /// Why `bootloader` above is a guess rather than a reading, when it is one.
    ///
    /// Carried onto the surface, not just consulted while building the routes,
    /// because the LABEL is a second place the same fact reaches the user. A
    /// whole-directory efivarfs refusal takes `LoaderInfo` down, `bootloader`
    /// falls back to the kernel command line, and that fallback is "grub" on
    /// every Rime image — UKI or not. `rime boot status` already says so
    /// (boot.rs); this surface printed the bare word and the two disagreed
    /// about the same machine.
    bootloader_unavailable: Option<String>,
    rows: Vec<Row>,
    routes: Vec<Route>,
}

/// The recovery surface's rows as `(id, health, detail)`, for §26's post-update
/// health verdict.
///
/// One prober, two callers — the same rule `boot::chain_facts` follows. A
/// second implementation of "is the GPU driver bound" would be the one nobody
/// keeps correct, and it would be the one deciding whether to refuse somebody's
/// update.
pub(crate) fn health_rows() -> Vec<(String, Health, String)> {
    probe(&Sys::from_env())
        .rows
        .into_iter()
        .map(|r| (r.id.to_string(), r.state, r.detail))
        .collect()
}

/// Deployments present under `/ostree/deploy/*/deploy`.
///
/// Counted from the filesystem rather than asked of `bootc status`, because a
/// directory count is a fact with no schema: it cannot break when another
/// tool changes its JSON, it needs no subprocess, and it is presentable as a
/// fixture. A deployment is `<checksum>.<serial>`; the sibling
/// `<checksum>.<serial>.origin` file is not one.
///
/// Every failed read propagates instead of shrinking the count. A partial
/// enumeration reported as complete is the answer that hides a rollback
/// target: one stateroot we cannot list turns "2 deployments present, there is
/// one to go back to" into "only the booted deployment exists". The `Err` side
/// carries the path and the errno so the row can say which read failed.
fn deployment_count(sys: &Sys) -> Result<usize, String> {
    let root = sys.path("/ostree/deploy");
    let at = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let stateroots = std::fs::read_dir(&root).map_err(|e| at(&root, e))?;
    let mut n = 0usize;
    for sr in stateroots {
        let sr = sr.map_err(|e| at(&root, e))?;
        let deploy = sr.path().join("deploy");
        let entries = match std::fs::read_dir(&deploy) {
            Ok(entries) => entries,
            // A stateroot with no `deploy/` yet holds no deployments. That is
            // a read that succeeded in saying "nothing here", not one we were
            // refused.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(at(&deploy, e)),
        };
        for e in entries {
            let e = e.map_err(|e| at(&deploy, e))?;
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.ends_with(".origin") {
                continue;
            }
            // `Path::is_dir` answers false for every failed stat, so a
            // deployment we may not look at would simply not be counted.
            match std::fs::metadata(e.path()) {
                Ok(m) if m.is_dir() => n += 1,
                Ok(_) => {}
                // Removed between the readdir and the stat, or a dangling
                // link: not a deployment either way.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(at(&e.path(), err)),
            }
        }
    }
    Ok(n)
}

/// The booted deployment's ostree checksum, from the kernel command line.
///
/// `ostree=/ostree/boot.1/rime/<hash>/0` on a GRUB/BLS machine. The checksum
/// in that path is the deployment identity, and it is the value `ostree admin
/// pin` and `bootc status` both key on.
fn booted_deployment(cmdline: &str) -> Option<String> {
    let arg = cmdline
        .split_whitespace()
        .find_map(|w| w.strip_prefix("ostree="))?;
    // The last two components are `<hash>/<serial>` for boot.N paths and
    // `<csum>.<serial>` for deploy paths. Take the longest hex-looking
    // component, which is the identity in both layouts.
    arg.split('/')
        .map(|c| c.split('.').next().unwrap_or(c))
        .filter(|c| c.len() >= 32 && c.chars().all(|ch| ch.is_ascii_hexdigit()))
        .max_by_key(|c| c.len())
        .map(str::to_string)
}

/// Read `KEY=value` pairs out of an os-release file, unquoted.
fn os_release(sys: &Sys) -> BTreeMap<String, String> {
    let text = sys
        .read("/etc/os-release")
        .or_else(|| sys.read("/usr/lib/os-release"))
        .unwrap_or_default();
    let mut map = BTreeMap::new();
    for l in text.lines() {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = l.split_once('=') {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            map.insert(k.trim().to_string(), v.to_string());
        }
    }
    map
}

/// Whether `/usr` is mounted read-only, from `/proc/mounts`.
///
/// `None` when no mount covers `/usr`, which on an ostree machine means it is
/// covered by the root mount instead — so the root mount's flags are checked
/// as a fallback. Returning "read-write" for a machine whose `/proc/mounts`
/// simply looks different would report the drift AGENTS.md prohibits on a
/// machine that has none.
fn usr_readonly(mounts: &str) -> (Option<bool>, Option<String>) {
    let mut root: Option<(bool, String)> = None;
    for l in mounts.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 4 {
            continue;
        }
        let (target, fstype, opts) = (f[1], f[2], f[3]);
        let ro = opts.split(',').any(|o| o == "ro");
        if target == "/usr" || target == "/sysroot/usr" {
            return (Some(ro), Some(fstype.to_string()));
        }
        if target == "/" {
            root = Some((ro, fstype.to_string()));
        }
    }
    match root {
        Some((ro, fs)) => (Some(ro), Some(fs)),
        None => (None, None),
    }
}

/// Does the machine have a default route? Read, never probed.
///
/// `/proc/net/route` lists a destination of `00000000` for the default. This
/// deliberately contacts nothing: a recovery surface that resolved a name or
/// opened a socket would be slow on exactly the machine whose network is the
/// problem, and "the internet is reachable" is not a fact Rime needs to assert
/// to tell the user whether their machine has a route.
fn has_default_route(route: &str) -> bool {
    route
        .lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() >= 2).then_some(f[1])
        })
        .any(|dest| dest == "00000000")
}

/// The kernel module a GPU vendor needs, and whether one of them is loaded.
fn gpu_modules(vendor: &rimed_core::GpuVendor) -> &'static [&'static str] {
    use rimed_core::GpuVendor as V;
    match vendor {
        V::Nvidia => &["nvidia", "nouveau"],
        V::Amd => &["amdgpu", "radeon"],
        V::Intel => &["i915", "xe"],
        _ => &[],
    }
}

fn probe(sys: &Sys) -> Surface {
    // A `/proc/cmdline` that could not be read is not a `/proc/cmdline`
    // without an `ostree=` argument. Collapsing the two put an `Attention` row
    // on a machine nobody had looked at.
    let (cmdline, cmdline_error) = match sys.read_result("/proc/cmdline") {
        Ok(text) => (text, None),
        Err(e) => (String::new(), Some(e.to_string())),
    };
    let osr = os_release(sys);
    let chain = crate::boot::chain_facts(sys.fixture.clone());
    let ostree_booted = sys.exists("/run/ostree-booted");
    let mut rows: Vec<Row> = Vec::new();

    // ── current deployment ──────────────────────────────────────────────────
    let deployment = booted_deployment(&cmdline);
    let variant = osr.get("VARIANT_ID").cloned().unwrap_or_default();
    let version = osr.get("VERSION_ID").cloned().unwrap_or_default();
    rows.push(match (ostree_booted, deployment.as_deref(), &cmdline_error) {
        (true, Some(csum), _) => Row {
            id: "current-deployment",
            label: "Current deployment",
            state: Health::Verified,
            detail: format!(
                "ostree {} — Rime OS {} {} (the image reference and its source \
                 revision are `rime changelog`)",
                &csum[..csum.len().min(12)],
                if version.is_empty() { "?" } else { &version },
                if variant.is_empty() { "?" } else { &variant },
            ),
            action: None,
        },
        (true, None, Some(why)) => Row {
            id: "current-deployment",
            label: "Current deployment",
            state: Health::Unavailable,
            detail: format!(
                "/proc/cmdline could not be read ({why}), so the booted \
                 deployment was never looked for. Reporting a missing ostree= \
                 argument here would be a claim about a file nobody read."
            ),
            action: None,
        },
        (true, None, None) => Row {
            id: "current-deployment",
            label: "Current deployment",
            state: Health::Attention,
            detail: "the machine booted an ostree deployment but the kernel \
                     command line carries no ostree= argument, so the booted \
                     deployment cannot be identified"
                .to_string(),
            action: Some("rime changelog".to_string()),
        },
        (false, ..) => Row {
            id: "current-deployment",
            label: "Current deployment",
            state: Health::Unavailable,
            detail: "not an ostree/bootc boot (/run/ostree-booted is absent) — \
                     this is what a container or a CI runner looks like, not a \
                     fault on an installed Rime machine"
                .to_string(),
            action: None,
        },
    });

    // ── previous deployment: §19's [Boot previous deployment] ───────────────
    //
    // There is deliberately no `rime recover previous` verb. `rime rollback`
    // already swaps the default and the previous deployment, and a second name
    // for it would be a second thing to keep correct. What §19 actually asks
    // for is that the action be *visible from Settings rather than only the
    // CLI*, so the row reports whether there is anything to roll back to and
    // names the command a button runs.
    let deployments = deployment_count(sys);
    rows.push(match &deployments {
        Ok(n) if *n >= 2 => Row {
            id: "previous-deployment",
            label: "Previous deployment",
            state: Health::Available,
            detail: format!(
                "{n} deployments present, so there is one to go back to. Nothing \
                 has verified that it boots — that is what the boot counter does \
                 on the opt-in systemd-boot path. `sudo rime pin` before a risky \
                 change, or two bad updates in a row can evict it."
            ),
            action: Some("sudo rime rollback".to_string()),
        },
        Ok(1) => Row {
            id: "previous-deployment",
            label: "Previous deployment",
            state: Health::Attention,
            detail: "only the booted deployment exists, so there is nothing to \
                     roll back to yet. The next `rime update` creates one."
                .to_string(),
            action: None,
        },
        Ok(n) => Row {
            id: "previous-deployment",
            label: "Previous deployment",
            state: Health::Unavailable,
            detail: format!("/ostree/deploy holds {n} deployments, which should be impossible"),
            action: None,
        },
        Err(why) => Row {
            id: "previous-deployment",
            label: "Previous deployment",
            state: Health::Unavailable,
            detail: format!(
                "/ostree/deploy could not be read ({why}), so the deployment \
                 count is unknown. An empty answer here would be \
                 indistinguishable from 'nothing to roll back to', which is the \
                 answer that would hide a rollback."
            ),
            action: None,
        },
    });

    // ── Secure Boot ─────────────────────────────────────────────────────────
    rows.push(match chain.secure_boot {
        Some(true) => Row {
            id: "secure-boot",
            label: "Secure Boot",
            state: Health::Verified,
            detail: format!(
                "firmware reports Secure Boot enabled{}",
                match chain.setup_mode {
                    Some(true) => ", and the firmware is in Setup Mode",
                    _ => "",
                }
            ),
            action: None,
        },
        Some(false) => Row {
            id: "secure-boot",
            label: "Secure Boot",
            state: Health::Attention,
            detail: "firmware reports Secure Boot disabled. It is a product \
                     invariant for published images, and enabling it writes \
                     your firmware — an explicitly user-initiated procedure, \
                     never a script in this repository. See docs/boot-v2.md."
                .to_string(),
            action: None,
        },
        None => Row {
            id: "secure-boot",
            label: "Secure Boot",
            state: Health::Unavailable,
            detail: "no SecureBoot EFI variable: this is not a UEFI boot. \
                     Reporting 'disabled' would claim a measurement nobody took."
                .to_string(),
            action: None,
        },
    });

    // ── filesystem ──────────────────────────────────────────────────────────
    let mounts = sys.read("/proc/mounts").unwrap_or_default();
    let (usr_ro, usr_fs) = usr_readonly(&mounts);
    let fs = usr_fs.unwrap_or_else(|| "unknown".into());
    rows.push(match usr_ro {
        Some(true) => Row {
            id: "filesystem",
            label: "Filesystem",
            state: Health::Verified,
            detail: format!(
                "/usr is read-only on a {fs} root{}",
                if ostree_booted { ", ostree-booted" } else { "" }
            ),
            action: None,
        },
        Some(false) => Row {
            id: "filesystem",
            label: "Filesystem",
            state: Health::Attention,
            detail: format!(
                "/usr is mounted READ-WRITE on a {fs} root. /usr is image-owned \
                 and read-only at runtime; a writable one is machine drift, and \
                 anything written there is lost at the next update. Reboot to \
                 restore it."
            ),
            action: None,
        },
        None => Row {
            id: "filesystem",
            label: "Filesystem",
            state: Health::Unavailable,
            detail: "no mount covering /usr or / was found in /proc/mounts".to_string(),
            action: None,
        },
    });

    // ── GPU driver ──────────────────────────────────────────────────────────
    //
    // The module list is read, and a read that failed is not a list with
    // nothing in it. This row is the one that recommends `sudo rime rollback`,
    // so an empty list here proposed reverting the machine's operating system
    // on the strength of a file nobody managed to open. Absence is not carved
    // out: a kernel built with CONFIG_MODULES=n has no /proc/modules and every
    // driver compiled in, so "no module loaded" would be just as wrong there.
    let modules = sys.read_result("/proc/modules");
    let fp = rimed_core::Fingerprint::detect_from(&sys.path("/proc"), &sys.path("/sys"));
    rows.push(if fp.gpus.is_empty() {
        Row {
            id: "gpu-driver",
            label: "GPU driver",
            state: Health::Unavailable,
            detail: "no PCI display device was found, so there is no driver to \
                     check. This is what a headless machine or a VM without a \
                     virtual GPU looks like."
                .to_string(),
            action: None,
        }
    } else {
        match &modules {
            Err(e) => Row {
                id: "gpu-driver",
                label: "GPU driver",
                state: Health::Unavailable,
                detail: format!(
                    "/proc/modules could not be read ({e}), so the loaded \
                     modules were never listed. This row recommends a rollback \
                     when a driver is missing, and it must not do that off a \
                     file it did not read."
                ),
                action: None,
            },
            Ok(text) => {
                let loaded: Vec<&str> = text
                    .lines()
                    .filter_map(|l| l.split_whitespace().next())
                    .collect();
                let mut missing: Vec<String> = Vec::new();
                let mut present: Vec<String> = Vec::new();
                // An NVIDIA GPU older than the image's driver branch has no
                // driver on ANY Rime image, so it is not a regression and
                // `sudo rime rollback` cannot fix it. It gets its own sentence
                // and no action, instead of the rollback below.
                let mut too_old = false;
                for g in &fp.gpus {
                    let want = gpu_modules(&g.vendor);
                    if want.is_empty() {
                        continue;
                    }
                    match want.iter().find(|m| loaded.contains(m)) {
                        Some(m) => present.push(format!("{} via {}", g.vendor.as_str(), m)),
                        None if g.vendor == rimed_core::GpuVendor::Nvidia
                            && rimed_core::nvidia_support::Branch::for_device(g.pci_device)
                                != rimed_core::nvidia_support::Branch::Current =>
                        {
                            too_old = true
                        }
                        None => missing.push(format!(
                            "{} (wanted one of {})",
                            g.vendor.as_str(),
                            want.join("/")
                        )),
                    }
                }
                let old_gpu = too_old
                    .then(|| rimed_core::nvidia_support::scan(&sys.path("/sys")).explain())
                    .flatten();
                if let (true, Some(why)) = (missing.is_empty(), &old_gpu) {
                    Row {
                        id: "gpu-driver",
                        label: "GPU driver",
                        state: Health::Attention,
                        detail: if present.is_empty() {
                            why.clone()
                        } else {
                            format!("working: {}. {why}", present.join(", "))
                        },
                        action: None,
                    }
                } else if missing.is_empty() {
                    Row {
                        id: "gpu-driver",
                        label: "GPU driver",
                        state: Health::Verified,
                        detail: format!("{} — {}", present.len(), present.join(", ")),
                        action: None,
                    }
                } else {
                    Row {
                        id: "gpu-driver",
                        label: "GPU driver",
                        state: Health::Attention,
                        detail: format!(
                            "no kernel module loaded for {}{}{}",
                            missing.join(", "),
                            if present.is_empty() {
                                String::new()
                            } else {
                                format!("; working: {}", present.join(", "))
                            },
                            old_gpu.as_deref().map(|w| format!(". {w}")).unwrap_or_default()
                        ),
                        action: Some("sudo rime rollback".to_string()),
                    }
                }
            }
        }
    });

    // ── Rime Shell ──────────────────────────────────────────────────────────
    //
    // The same two facts `rime-shell-firstrun` checks, in the same order: the
    // image has to carry the shell (otherwise it is an image-build defect and
    // no user action helps), and this account has to be provisioned.
    //
    // Both halves keep the reason a stat failed. `(false, _)` tells the user to
    // roll their machine back, and `(true, false)` tells them their account was
    // never set up; neither is something to conclude from a stat that returned
    // an error nobody looked at. On a stock image `/usr/share/rime-shell` is
    // 0755 and the home is the caller's own, so this is the shape of the defect
    // rather than a refusal users are hitting today.
    let shell_qml = sys.path("/usr/share/rime-shell/shell.qml");
    let shipped = match std::fs::metadata(&shell_qml) {
        Ok(m) => Ok(m.len() > 0),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("{}: {e}", shell_qml.display())),
    };
    let provisioned = match user_home() {
        None => Err("$HOME is unset, or does not name a usable home directory".to_string()),
        Some(h) => {
            let dir = h.join(".config/rime-shell");
            match std::fs::metadata(&dir) {
                Ok(m) => Ok(m.is_dir()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(format!("{}: {e}", dir.display())),
            }
        }
    };
    rows.push(match (&shipped, &provisioned) {
        (Err(why), _) => Row {
            id: "rime-shell",
            label: "Rime Shell",
            state: Health::Unavailable,
            detail: format!(
                "{why} — so whether the image carries Rime Shell is unknown. \
                 Reporting it as missing here would recommend a rollback off a \
                 stat that failed."
            ),
            action: None,
        },
        (Ok(true), Err(why)) => Row {
            id: "rime-shell",
            label: "Rime Shell",
            state: Health::Unavailable,
            detail: format!(
                "the image carries the shell, but whether this account is \
                 provisioned could not be established: {why}"
            ),
            action: None,
        },
        (Ok(true), Ok(true)) => Row {
            id: "rime-shell",
            label: "Rime Shell",
            state: Health::Verified,
            detail: "vendored in the image at /usr/share/rime-shell, and this \
                     account is provisioned"
                .to_string(),
            action: None,
        },
        (Ok(true), Ok(false)) => Row {
            id: "rime-shell",
            label: "Rime Shell",
            state: Health::Attention,
            detail: "the image carries the shell but this account has no \
                     ~/.config/rime-shell, so the desktop has never been \
                     provisioned"
                .to_string(),
            action: Some("rime recover repair --commit".to_string()),
        },
        (Ok(false), _) => Row {
            id: "rime-shell",
            label: "Rime Shell",
            state: Health::Unavailable,
            detail: "/usr/share/rime-shell/shell.qml is missing or empty — the \
                     image did not ship Rime Shell. That is an image-build \
                     defect, not something a reset or a repair can fix; roll \
                     back to the previous deployment."
                .to_string(),
            action: Some("sudo rime rollback".to_string()),
        },
    });

    // ── network ─────────────────────────────────────────────────────────────
    //
    // `Available` is the ceiling here on purpose. Nothing was contacted, so
    // nothing was verified — claiming `verified` would be claiming a
    // reachability test this row deliberately does not perform.
    //
    // And a routing table that could not be read is not a routing table with no
    // default in it. The unread case sent the user looking at their network on
    // a machine whose network was fine.
    rows.push(match sys.read_result("/proc/net/route") {
        Ok(route) if has_default_route(&route) => Row {
            id: "network",
            label: "Network",
            state: Health::Available,
            detail: "a default route exists. Nothing was contacted, so nothing \
                     about reachability is claimed."
                .to_string(),
            action: None,
        },
        Ok(_) => Row {
            id: "network",
            label: "Network",
            state: Health::Attention,
            detail: "no default route. `rime update` and `rime install` need \
                     one; every verb on this surface does not."
                .to_string(),
            action: None,
        },
        Err(e) => Row {
            id: "network",
            label: "Network",
            state: Health::Unavailable,
            detail: format!(
                "/proc/net/route could not be read ({e}), so the routing table \
                 was never looked at. That is not the same as having no default \
                 route."
            ),
            action: None,
        },
    });

    // ── package extensions ──────────────────────────────────────────────────
    rows.push(package_row(sys, &version));

    // ── recovery routes ─────────────────────────────────────────────────────
    let mut routes: Vec<Route> = Vec::new();
    routes.push(Route {
        id: "previous-deployment",
        // `None` when the count could not be read, which is the route's own
        // "cannot be determined" and not a claim that there is nowhere to go.
        available: deployments.as_ref().ok().map(|n| *n >= 2),
        how: "`sudo rime rollback` then reboot, or select the previous entry in \
              the boot menu. /etc and /var — including /var/home — are preserved."
            .to_string(),
    });
    // The rescue route is NOT uniform, and saying so is the point.
    //
    // Reaching `rescue.target` means editing the kernel command line at the
    // boot menu. GRUB lets you. A Unified Kernel Image does not: its command
    // line is inside the signed image, which is what makes the signature worth
    // having, so on the opt-in systemd-boot+UKI path this route does not exist
    // — on exactly the machines that are hardest to get into. Reporting it
    // uniformly would be a false claim there, so the condition is the UKI, not
    // the loader's name: systemd-boot booting a type #1 entry still has an
    // editable command line.
    let rescue_present = sys.exists("/usr/lib/systemd/system/rescue.target");
    // Whether the command line is editable needs `booted_from_uki` only on the
    // systemd-boot path — GRUB's menu is editable regardless of what booted.
    // A refused StubInfo read only matters there, so it is the one case that
    // gets its own arm rather than picking a side.
    routes.push(if let Some(why) = &chain.bootloader_unavailable {
        // A directory-wide efivarfs refusal takes LoaderInfo down with
        // StubInfo, and `chain.bootloader` then fell back to the cmdline
        // heuristic without ever having a chance to see "systemd-boot". That
        // fallback is "grub" on every Rime image's cmdline, and trusting it
        // here would un-gate this route on exactly the systemd-boot+UKI
        // machine it must not exist on — so bootloader identity itself being
        // unavailable outranks everything below.
        Route {
            id: "rescue-target",
            available: None,
            how: format!(
                "cannot be determined: which bootloader is in use could not be \
                 established ({why}), and that decides whether the kernel command \
                 line at the boot menu is editable at all."
            ),
        }
    } else {
        match (&chain.booted_from_uki, chain.bootloader) {
            (Err(why), "systemd-boot") => Route {
                id: "rescue-target",
                available: None,
                how: format!(
                    "cannot be determined: whether this boot used a Unified Kernel \
                     Image could not be read ({why}), and that decides whether the \
                     kernel command line at the boot menu is editable."
                ),
            },
            (uki, bootloader) => {
                let uki = uki.clone().unwrap_or(false);
                let cmdline_editable =
                    bootloader == "grub" || (bootloader == "systemd-boot" && !uki);
                Route {
                    id: "rescue-target",
                    available: Some(rescue_present && cmdline_editable),
                    how: if cmdline_editable {
                        format!(
                            "at the {bootloader} menu, edit the entry ({}) and append \
                             `systemd.unit=rescue.target` to the kernel command line. It \
                             asks for the root password.",
                            if bootloader == "grub" { "`e`, then Ctrl-X" } else { "`e`" }
                        )
                    } else if uki {
                        "not available: this machine booted a Unified Kernel Image, whose \
                         command line is inside the signed image and cannot be edited at \
                         the menu. Use the boot counter or the previous deployment."
                            .to_string()
                    } else {
                        format!(
                            "not available: the bootloader is {bootloader} and rescue.target \
                             is {}.",
                            if rescue_present { "present" } else { "absent from this image" }
                        )
                    },
                }
            }
        }
    });
    routes.push(match &chain.boot_counting {
        Ok(true) => Route {
            id: "boot-counting",
            available: Some(true),
            how: "three boots that do not reach boot-complete.target and \
                  systemd-boot selects the previous blessed entry by itself. \
                  `rime boot status` shows the tally."
                .to_string(),
        },
        Ok(false) => Route {
            id: "boot-counting",
            available: Some(false),
            how: "not in effect: this machine boots through GRUB, which is the \
                  default for every published Rime image. Automatic boot counting \
                  is the opt-in systemd-boot path — see docs/boot-v2.md."
                .to_string(),
        },
        Err(why) => Route {
            id: "boot-counting",
            available: None,
            how: format!(
                "cannot be determined: the LoaderBootCountPath EFI variable could \
                 not be read ({why}), so whether boot counting is in effect is \
                 unknown."
            ),
        },
    });
    routes.push(Route {
        id: "disposable-environment",
        available: Some(sys.exists("/usr/libexec/rime-disposable")),
        how: "`rime disposable run` gives you a throwaway userspace on a machine \
              that still boots — a whole environment that is deleted when you \
              close it. It is not a repair environment for a machine that will \
              not boot."
            .to_string(),
    });
    routes.push(Route {
        id: "recovery-boot-entry",
        // Deliberately `false`, not `null`. Rime ships no recovery boot entry,
        // and that is a decision rather than a gap: creating one means writing
        // an ESP or an EFI variable, and there is no rollback for either —
        // the thing that would perform the rollback is what you broke.
        // docs/recovery.md carries the operator procedure.
        available: Some(false),
        how: "Rime ships no recovery boot entry, and nothing in this repository \
              writes an ESP or an EFI variable. Adding one is an operator \
              procedure — see docs/recovery.md — because there is no rollback \
              for a boot path you overwrote."
            .to_string(),
    });
    routes.push(Route {
        id: "installer-media",
        // `null`: a running system cannot tell whether the user has a USB
        // stick in a drawer. `false` would be a claim; `true` would be a lie.
        available: None,
        how: "the route for a machine that will not boot at all. Cannot be \
              determined from a running system."
            .to_string(),
    });

    Surface {
        bootloader: chain.bootloader,
        bootloader_unavailable: chain.bootloader_unavailable,
        rows,
        routes,
    }
}

/// The package-extension row.
///
/// Split out because it is the one row that reads another program's state file,
/// and every branch of it is a state the developer's machine does not have at
/// the same time.
fn package_row(sys: &Sys, running_version: &str) -> Row {
    const STATE: &str = "/var/lib/rime/pkg/state.json";
    let id = "package-extensions";
    let label = "Package extensions";
    let text = match sys.read_result(STATE) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Row {
                id,
                label,
                state: Health::Verified,
                // Absence is a checked fact, and it is the common case: most
                // machines install nothing with `rime install`.
                detail: "no user packages on this machine".to_string(),
                action: None,
            };
        }
        Err(e) => {
            // `/var/lib/rime/pkg` is 0700 root:root by a tmpfiles.d rule
            // (Containerfile.base), and `rime recover status` is something a
            // desktop session runs as itself. Treating that refusal as absence
            // told every non-root user on every machine with packages
            // installed that they had none, and marked the lie `Verified`.
            // `rime boot status` already gets this right for the ESP, which is
            // 0700 for the same reason, by reporting the read as unavailable
            // with the reason rather than inventing an empty answer.
            return Row {
                id,
                label,
                state: Health::Unavailable,
                detail: format!("{STATE} could not be read: {e}"),
                action: Some("sudo rime recover status".to_string()),
            };
        }
    };
    let doc: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            return Row {
                id,
                label,
                state: Health::Attention,
                detail: format!("{STATE} could not be parsed: {e}"),
                action: Some("sudo rime pkg rebuild".to_string()),
            }
        }
    };
    let built_for = doc
        .get("os_version_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let resolved = doc
        .get("resolved")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let unsigned = doc
        .get("unsigned_accepted")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);

    if !built_for.is_empty() && !running_version.is_empty() && built_for != running_version {
        return Row {
            id,
            label,
            state: Health::Attention,
            detail: format!(
                "{resolved} packages, built for OS {built_for} but this machine \
                 runs OS {running_version} — the extension needs rebuilding \
                 against the booted image"
            ),
            action: Some("sudo rime pkg rebuild --if-needed".to_string()),
        };
    }
    // Available, not Attention: a package the owner chose to install with
    // --allow-unsigned is present and usable with nothing verifying it, which
    // is what Available means. Attention made it a "regression an update could
    // have caused" and held every later update for a decision the owner made
    // on purpose (opencode on the L16, 2026-09-23) — and offered a rebuild,
    // which does nothing for a signature.
    if unsigned > 0 {
        return Row {
            id,
            label,
            state: Health::Available,
            detail: format!(
                "{resolved} packages built for OS {built_for}, of which \
                 {unsigned} were installed with --allow-unsigned and are \
                 covered by no trusted key"
            ),
            action: Some("rime pkg verify".to_string()),
        };
    }
    Row {
        id,
        label,
        state: Health::Verified,
        detail: format!("{resolved} packages, built for OS {built_for} (running {running_version})"),
        action: None,
    }
}

/// §19's four action buttons, and the command each one runs.
///
/// The command strings are bounded at 66 characters, because the text renderer
/// prints them after a 28-column prefix and the report as a whole is held to
/// 96. A test asserts the rendered width rather than this constant: the first
/// version of this table produced a 102-column line while every `wrap` unit
/// test passed, because the fixed-width action rows do not go through `wrap`
/// at all.
fn actions() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "repair",
            "Repair automatically",
            "rime recover repair                  (dry run; --commit runs it)",
        ),
        (
            "bootPrevious",
            "Boot previous deployment",
            "sudo rime rollback                   then reboot",
        ),
        (
            "factoryReset",
            "Factory reset",
            "rime recover reset --scope desktop|user   (a dry run)",
        ),
        (
            "diagnostics",
            "Hardware diagnostics",
            "rime doctor                          (--json for a UI)",
        ),
    ]
}

fn cmd_status(json: bool) -> i32 {
    let sys = Sys::from_env();
    let s = probe(&sys);
    let attention = s.rows.iter().filter(|r| r.state == Health::Attention).count();

    if json {
        let doc = json!({
            "bootloader": s.bootloader,
            // A sibling key, never a change to the value above: rime-shell's
            // RecoveryService.qml and RecoveryPage.qml read `bootloader` and
            // two shell suites pin the bare strings. Named as `rime boot
            // status --json` names it, so a consumer learns one word for it.
            "bootloaderUnavailable": s.bootloader_unavailable,
            "rows": s.rows.iter().map(|r| json!({
                "id": r.id,
                "label": r.label,
                "state": r.state.as_str(),
                "detail": r.detail,
                "action": r.action,
            })).collect::<Vec<_>>(),
            "actions": actions().iter().map(|(id, label, cmd)| json!({
                "id": id, "label": label, "command": cmd.trim_end(),
            })).collect::<Vec<_>>(),
            "routes": s.routes.iter().map(|r| json!({
                "id": r.id, "available": r.available, "how": r.how,
            })).collect::<Vec<_>>(),
            "needsAttention": attention,
            "resetScopes": ResetScope::ALL.iter().map(|sc| json!({
                "id": sc.as_str(), "summary": sc.summary(),
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
        return if attention > 0 { 1 } else { 0 };
    }

    println!("Rime recovery");
    println!("  bootloader : {}", s.bootloader);
    if s.bootloader_unavailable.is_some() {
        // 15, because "  bootloader : " is what precedes the label and `wrap`
        // bounds a line INCLUDING the indent it is told about.
        const CAVEAT_COL: usize = 15;
        // The reason itself is an efivarfs path plus an OS error, one token of
        // which is longer than the whole 96-column budget — so it goes in the
        // JSON key and in `rime boot status`, and what is said here is the part
        // that changes what the user should believe: the word above was not
        // read off the machine.
        println!(
            "{:<15}{}",
            "",
            wrap(
                "not confirmed: LoaderInfo could not be read, so this is the kernel \
                 command line's guess rather than a measurement. `rime boot status` \
                 prints the reason.",
                CAVEAT_COL,
            )
        );
    }
    println!();
    println!("{:<22}  {:<12}  DETAIL", "COMPONENT", "STATE");
    // 38, not 24: the prefix printed before the first line of the detail is
    // 22 + 2 + 12 + 2 columns, and `wrap` bounds a line INCLUDING the indent it
    // is given. Told 24 it left the first line free to reach 110 columns on a
    // 96-column budget — invisible to a test that checks `wrap`'s contract
    // rather than the rendered row.
    const DETAIL_COL: usize = 38;
    for r in &s.rows {
        println!("{:<22}  {:<12}  {}", r.label, r.state.as_str(), wrap(&r.detail, DETAIL_COL));
        if let Some(a) = &r.action {
            println!("{:<22}  {:<12}  -> {a}", "", "");
        }
    }
    println!("\nActions");
    for (_, label, cmd) in actions() {
        println!("  {label:<26} {cmd}");
    }
    println!("\nRecovery routes on this machine");
    for r in &s.routes {
        let mark = match r.available {
            Some(true) => "yes",
            Some(false) => "no",
            None => "unknown",
        };
        println!("  {:<24} {:<8} {}", r.id, mark, wrap(&r.how, 36));
    }
    if attention > 0 {
        println!("\n{attention} component(s) need attention.");
    }
    if attention > 0 {
        1
    } else {
        0
    }
}

/// Re-flow a detail string so a long sentence does not run off the terminal.
///
/// Whitespace-collapsing, because the details are written as multi-line Rust
/// string literals and would otherwise carry their source indentation into the
/// report.
fn wrap(text: &str, indent: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut out = String::new();
    let mut col = indent;
    for w in words {
        if col + w.len() + 1 > 96 && col > indent {
            out.push('\n');
            for _ in 0..indent {
                out.push(' ');
            }
            col = indent;
        } else if !out.is_empty() {
            out.push(' ');
            col += 1;
        }
        out.push_str(w);
        col += w.len();
    }
    out
}

// ── repair ───────────────────────────────────────────────────────────────────

/// Which repair steps this machine currently needs.
///
/// Diagnosed from the same surface `status` renders, so the button and the
/// report cannot disagree. A step with no diagnosis is not offered: a
/// `[Repair automatically]` that proposes something on every healthy machine
/// trains people to ignore it.
fn applicable_repairs(s: &Surface) -> Vec<&'static RepairStep> {
    let state = |id: &str| s.rows.iter().find(|r| r.id == id).map(|r| r.state);
    REPAIRS
        .iter()
        .filter(|step| match step.id {
            "reprovision-desktop" => state("rime-shell") == Some(Health::Attention),
            "rebuild-package-extension" => {
                state("package-extensions") == Some(Health::Attention)
            }
            // An unrecognised step is never offered. A new entry in the table
            // with no diagnosis here would otherwise be silently proposed
            // always, which is the failure mode this filter exists to avoid.
            _ => false,
        })
        .collect()
}

fn cmd_repair(args: RepairArgs) -> i32 {
    let sys = Sys::from_env();
    let s = probe(&sys);
    let steps = applicable_repairs(&s);
    let root = crate::ops::effective_uid() == Some(0);
    let here = if root { Domain::System } else { Domain::User };

    let mine: Vec<&&RepairStep> = steps.iter().filter(|s| s.domain == here).collect();
    let theirs: Vec<&&RepairStep> = steps.iter().filter(|s| s.domain != here).collect();

    if args.json {
        let doc = json!({
            "domain": here.as_str(),
            "committed": args.commit,
            "steps": steps.iter().map(|s| json!({
                "id": s.id,
                "domain": s.domain.as_str(),
                "what": s.what,
                "whySafe": s.why_safe,
                "command": s.argv,
                "runnableHere": s.domain == here,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
        if !args.commit {
            return 0;
        }
    } else {
        println!(
            "Automatic repair — {}",
            if args.commit {
                "COMMITTING"
            } else {
                "DRY RUN, nothing has been changed"
            }
        );
        println!("  privilege domain: {} (this run converges only this one)", here.as_str());
        if steps.is_empty() {
            println!("\nNothing to repair: every component this verb can fix reports fine.");
            println!("`rime recover status` shows the full surface.");
            return 0;
        }
        println!();
        for st in &steps {
            println!(
                "  [{}] {:<28} {}",
                st.domain.as_str(),
                st.id,
                wrap(st.what, 40)
            );
            println!("       command : {}", st.argv.join(" "));
            println!("       safe    : {}", wrap(st.why_safe, 17));
        }
        if !theirs.is_empty() {
            println!(
                "\n{} step(s) belong to the other privilege domain and were NOT run.",
                theirs.len()
            );
            println!(
                "  run: {}rime recover repair{}",
                if root { "" } else { "sudo " },
                if args.commit { " --commit" } else { "" }
            );
        }
        if !args.commit {
            println!("\nTo perform the {} step(s) above: rime recover repair --commit", mine.len());
            return 0;
        }
    }

    if steps.is_empty() {
        return 0;
    }

    let mut worst = 0;
    for st in &mine {
        // The program is spelled by the table, absolute, and under a fixture
        // root it is remapped like every other path — so no environment
        // variable ever names the program that runs here.
        let program = sys.path(st.argv[0]);
        eprintln!("rime: running: {} {}", program.display(), st.argv[1..].join(" "));
        match Command::new(&program).args(&st.argv[1..]).status() {
            Ok(status) => {
                let code = status.code().unwrap_or(-1);
                if code != 0 {
                    eprintln!("rime: {} exited {code}", st.id);
                }
                worst = worst.max(code);
            }
            Err(e) => {
                eprintln!("rime: {} could not run ({}): {e}", st.id, program.display());
                worst = worst.max(1);
            }
        }
    }
    worst
}

// ── reset ────────────────────────────────────────────────────────────────────

/// The invoking user's home, validated.
///
/// Validation, not convenience. Everything a reset removes is resolved under
/// this path, so a `$HOME` of `/` would turn `.config/rime-shell/input.json`
/// into `/.config/rime-shell/input.json` and, worse, would make the
/// "is the resolved path inside the home" check pass for anything at all. So:
/// absolute, an existing directory, at least two components deep, and not one
/// of the shared parents that are never anybody's home.
fn user_home() -> Option<PathBuf> {
    // Under sudo the account being asked about is the invoking one, not root.
    // `sudo rime update` runs this check with $HOME=/root, and root has no
    // ~/.config/rime-shell, so every update on every machine was held as "the
    // desktop has never been provisioned" (measured on the L16, 2026-09-23).
    // SUDO_UID is set by sudo itself; only root can forge it, and root already
    // holds everything it could select.
    let p = match sudo_invoker_home() {
        Some(home) => home,
        None => PathBuf::from(std::env::var_os("HOME").filter(|v| !v.is_empty())?),
    };
    if !p.is_absolute() || !p.is_dir() {
        return None;
    }
    let real = std::fs::canonicalize(&p).ok()?;
    let s = real.to_string_lossy();
    if matches!(s.as_ref(), "/" | "/home" | "/var/home" | "/usr" | "/etc" | "/var") {
        return None;
    }
    if real.components().count() < 3 {
        // `/x` is one root plus one component. A real home is at least
        // `/home/<user>` or `/var/home/<user>`.
        return None;
    }
    Some(real)
}

/// The invoking account's home when this runs as root under sudo, else nothing.
fn sudo_invoker_home() -> Option<PathBuf> {
    // Safe: geteuid cannot fail.
    if unsafe { libc::geteuid() } != 0 {
        return None;
    }
    let uid = std::env::var("SUDO_UID").ok()?.trim().parse::<libc::uid_t>().ok()?;
    if uid == 0 {
        return None;
    }
    rime_agent_core::paths::passwd_home_of(uid)
}

/// One resolved reset target.
struct Planned {
    target: &'static Target,
    path: PathBuf,
    exists: bool,
}

/// Why a path was refused. Every one of these means nothing is deleted.
fn safe_to_touch(home: &Path, t: &Target, path: &Path) -> Result<(), String> {
    // The final component must not be a symlink. A symlink at the target path
    // pointing somewhere else is the one input a naive prefix check passes and
    // a recursive delete then follows out of the tree. `symlink_metadata` does
    // not follow, which is the whole reason it is used here.
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            return Err(format!("{} is a symlink; refusing to remove it", path.display()))
        }
        Ok(m) => {
            // The declared kind is enforced, so a directory sitting where the
            // table declares a file can never be removed recursively.
            if t.kind == Kind::Dir && !m.is_dir() {
                return Err(format!("{} is not a directory but the table says it is", path.display()));
            }
            if t.kind == Kind::File && !m.is_file() {
                return Err(format!("{} is not a regular file but the table says it is", path.display()));
            }
        }
        Err(_) => return Ok(()), // absent: nothing to do, nothing to refuse
    }
    // Resolve the PARENT and rebuild the path, then compare. Canonicalising
    // the target itself would follow a symlink and hide the very thing the
    // check above refuses; canonicalising the parent catches a symlinked
    // directory higher up.
    let parent = path.parent().ok_or_else(|| format!("{} has no parent", path.display()))?;
    let real_parent = std::fs::canonicalize(parent)
        .map_err(|e| format!("cannot resolve {}: {e}", parent.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| format!("{} has no final component", path.display()))?;
    let resolved = real_parent.join(name);
    if !real_parent.starts_with(home) {
        return Err(format!(
            "{} resolves to {}, which is outside {}",
            path.display(),
            resolved.display(),
            home.display()
        ));
    }
    if resolved == home {
        return Err(format!("{} resolves to the home directory itself", path.display()));
    }
    Ok(())
}

fn plan(home: &Path, scope: ResetScope) -> Vec<Planned> {
    targets(scope)
        .into_iter()
        .map(|t| {
            let path = home.join(t.rel);
            let exists = std::fs::symlink_metadata(&path).is_ok();
            Planned { target: t, path, exists }
        })
        .collect()
}

/// Paths a plan would actually change — the set the confirmation token binds
/// to.
fn token_paths(planned: &[Planned]) -> Vec<String> {
    planned
        .iter()
        .filter(|p| p.exists)
        .map(|p| p.path.to_string_lossy().to_string())
        .collect()
}

fn cmd_reset(args: ResetArgs) -> i32 {
    // Root is refused outright, and not as a formality. Root's home is not the
    // user's, so a `sudo rime recover reset` would reset root's desktop and
    // leave the user's untouched while reporting success — a destructive verb
    // that acts on the wrong account is worse than one that refuses.
    if crate::ops::effective_uid() == Some(0) {
        eprintln!(
            "rime: `recover reset` is per-account and must not run as root.\n\
             \x20      Root's home is not yours, so this would reset root's desktop\n\
             \x20      and leave yours exactly as it is — while reporting success.\n\
             \x20      Run it as yourself, without sudo."
        );
        return 1;
    }

    let scope: ResetScope = match args.scope.parse() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("rime: {e}");
            return 2;
        }
    };
    let Some(home) = user_home() else {
        eprintln!(
            "rime: $HOME is unset, is not an existing directory, or names a\n\
             \x20      shared parent (/, /home, /var/home). Everything this verb\n\
             \x20      removes is resolved under $HOME, so it refuses rather than\n\
             \x20      guessing."
        );
        return 1;
    };

    let planned = plan(&home, scope);
    let paths = token_paths(&planned);
    let token = confirm_token(scope, &paths);
    let provisioner = Sys::from_env().path(PROVISIONER);

    // ── the plan, printed the same way whether or not this run commits ──────
    if args.json {
        let doc = json!({
            "scope": scope.as_str(),
            "summary": scope.summary(),
            "confirmToken": token,
            "committed": args.commit,
            "targets": planned.iter().map(|p| json!({
                "path": p.path.to_string_lossy(),
                "relative": p.target.rel,
                "disposition": p.target.how.as_str(),
                "kind": if p.target.kind == Kind::Dir { "dir" } else { "file" },
                "exists": p.exists,
                "backedUp": p.target.worth_backing_up(),
                "what": p.target.what,
            })).collect::<Vec<_>>(),
            "preserved": preserved(scope),
            "preservedLandmarks": PRESERVED_LANDMARKS,
            "reprovision": !args.no_reprovision,
            "provisioner": provisioner.to_string_lossy(),
        });
        println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
    } else {
        print!("{}", render_reset_plan(scope, &planned, &token, args.commit, args.no_reprovision));
    }

    if !args.commit {
        return 0;
    }

    // ── the confirmation ───────────────────────────────────────────────────
    let Some(given) = args.confirm.as_deref() else {
        eprintln!(
            "\nrime: --commit needs --confirm. Nothing has been changed.\n\
             \x20      The token is derived from this scope AND the exact set of\n\
             \x20      paths above, so it cannot be constructed without running\n\
             \x20      the plan — which is the step that prints what is lost.\n\
             \x20      run: rime recover reset --scope {} --commit --confirm {}",
            scope.as_str(),
            token
        );
        return 2;
    };
    if given != token {
        eprintln!(
            "\nrime: the confirmation does not match this plan. Nothing has been changed.\n\
             \x20      given    : {given}\n\
             \x20      expected : {token}\n\
             \x20      A mismatch means the machine changed since the plan was\n\
             \x20      printed, or the token was constructed rather than read.\n\
             \x20      Re-run without --commit and use the token it prints."
        );
        return 2;
    }

    if !args.no_reprovision && !provisioner.is_file() {
        eprintln!(
            "\nrime: {} is missing, so this reset could not put back the files it\n\
             \x20      removes. Refusing rather than leaving the desktop with no\n\
             \x20      configuration and no way to regenerate it.\n\
             \x20      On a Rime machine this is an image defect — `sudo rime update`.\n\
             \x20      To take the deletion alone anyway: --no-reprovision",
            provisioner.display()
        );
        return 1;
    }

    // ── landmarks, before ──────────────────────────────────────────────────
    // Snapshotted rather than assumed present: a landmark that never existed
    // cannot be asserted afterwards, and asserting it would fail every reset
    // on a machine with no ~/.gnupg.
    let landmarks_before: Vec<&&str> = PRESERVED_LANDMARKS
        .iter()
        .filter(|l| std::fs::symlink_metadata(home.join(l)).is_ok())
        .collect();

    // ── safety pass: every target, before anything is touched ──────────────
    // All-or-nothing on purpose. A reset that removed four paths and then
    // refused the fifth would leave a state nobody planned.
    for p in planned.iter().filter(|p| p.exists) {
        if let Err(why) = safe_to_touch(&home, p.target, &p.path) {
            eprintln!("\nrime: refusing this reset — {why}");
            eprintln!("rime: nothing has been changed.");
            return 1;
        }
    }

    // ── backup ─────────────────────────────────────────────────────────────
    // Outside every target, and named so it is obvious. `~/.local/state/rime`
    // is itself a target at user scope, so the backup cannot live under it.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = home.join(format!("rime-reset-backup-{stamp}"));
    let mut backed_up = 0usize;
    let wants_backup = planned
        .iter()
        .any(|p| p.exists && p.target.worth_backing_up());
    if wants_backup {
        if let Err(e) = std::fs::create_dir_all(&backup) {
            eprintln!(
                "\nrime: cannot create the backup directory {}: {e}\n\
                 rime: nothing has been changed.",
                backup.display()
            );
            return 1;
        }
    }
    for p in planned.iter().filter(|p| p.exists && p.target.worth_backing_up()) {
        let dest = backup.join(p.target.rel);
        if let Some(parent) = dest.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("\nrime: cannot prepare {}: {e}", parent.display());
                eprintln!("rime: nothing has been changed.");
                return 1;
            }
        }
        if let Err(e) = copy_tree(&p.path, &dest) {
            eprintln!("\nrime: cannot back up {}: {e}", p.path.display());
            eprintln!("rime: nothing has been changed.");
            return 1;
        }
        backed_up += 1;
    }
    if backed_up > 0 {
        println!("\nbacked up {backed_up} path(s) to {}", backup.display());
    }

    // ── perform ────────────────────────────────────────────────────────────
    let mut removed = 0usize;
    let mut emptied = 0usize;
    for p in planned.iter().filter(|p| p.exists) {
        let r = match (p.target.how, p.target.kind) {
            (Disposition::Delete, Kind::Dir) => std::fs::remove_dir_all(&p.path),
            (Disposition::Delete, Kind::File) => std::fs::remove_file(&p.path),
            // Emptied in place. Nothing under ~/.config/hypr is ever deleted:
            // empty is the "no overrides" state the compositor understands, and
            // it is reachable without this code deciding which of the user's
            // files it may remove.
            (Disposition::Truncate, _) => std::fs::write(&p.path, b""),
        };
        match r {
            Ok(()) => {
                if p.target.how == Disposition::Truncate {
                    emptied += 1;
                    println!("emptied {}", p.path.display());
                } else {
                    removed += 1;
                    println!("removed {}", p.path.display());
                }
            }
            Err(e) => eprintln!("rime: could not change {}: {e}", p.path.display()),
        }
    }
    println!("\nremoved {removed} path(s), emptied {emptied}.");

    // ── landmarks, after ───────────────────────────────────────────────────
    // The postcondition that catches a table entry which somehow widened.
    // Grepping for what you deleted cannot detect what you deleted as well.
    let mut lost: Vec<String> = Vec::new();
    for l in &landmarks_before {
        if std::fs::symlink_metadata(home.join(**l)).is_err() {
            lost.push((**l).to_string());
        }
    }
    if !lost.is_empty() {
        eprintln!(
            "\nrime: FAILURE — this reset removed something it promised to preserve:\n\
             \x20       {}\n\
             \x20     This is a defect in the reset table, not a normal outcome.\n\
             \x20     {}",
            lost.join(", "),
            if backed_up > 0 {
                format!("A backup of what was removed is in {}.", backup.display())
            } else {
                "Nothing was backed up, because no target asked for it.".to_string()
            }
        );
        return 1;
    }
    println!("preserved {} landmark(s), re-checked after the fact.", landmarks_before.len());

    // ── re-seed ────────────────────────────────────────────────────────────
    if args.no_reprovision {
        println!(
            "\n--no-reprovision: the files Rime Shell needs were NOT put back.\n\
             Log in again, or run {}, before starting a session.",
            provisioner.display()
        );
        return 0;
    }
    eprintln!("rime: running: {}", provisioner.display());
    let rc = match Command::new(&provisioner).status() {
        Ok(s) => s.code().unwrap_or(-1),
        Err(e) => {
            eprintln!("rime: the provisioner could not run: {e}");
            -1
        }
    };
    // Postcondition on the reseed, not just its exit status: the provisioner
    // is `set -e` but writes several files best-effort, so "exited 0" and
    // "the files are back" are two different claims.
    //
    // The two generated fragments are NOT checked here any more. They used to
    // be, because the provisioner pre-created them and a `source =` with no
    // match was fatal — so "still absent" really did mean a broken session.
    // Under the Lua layout the provisioner deliberately creates neither:
    // hyprland.lua skips a module that is not there, and an empty
    // rime/monitors.lua would suppress nothing while looking like a generator
    // that ran. Asserting their presence would now fail every correct reset.
    //
    // What is worth asserting is what the provisioner really does seed: the
    // entry point and the module directory it requires from.
    let mut absent: Vec<String> = Vec::new();
    for rel in [
        ".config/rime-shell",
        ".config/hypr/hyprland.lua",
        ".config/hypr/rime",
    ] {
        if !home.join(rel).exists() {
            absent.push(rel.to_string());
        }
    }
    if rc != 0 || !absent.is_empty() {
        eprintln!(
            "\nrime: the reset completed but the desktop was not fully re-seeded\n\
             \x20      (provisioner exit {rc}{}).\n\
             \x20      Log out and back in — the provisioner runs at every login and\n\
             \x20      self-heals. If it still does not, `sudo rime update`.",
            if absent.is_empty() {
                String::new()
            } else {
                format!(", still absent: {}", absent.join(", "))
            }
        );
        return 1;
    }
    println!("re-seeded the desktop. Log out and back in for the compositor to re-read its configuration.");
    0
}

/// Copy a file or a whole directory. Used only for the pre-reset backup.
fn copy_tree(src: &Path, dest: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir_all(dest)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            copy_tree(&e.path(), &dest.join(e.file_name()))?;
        }
        return Ok(());
    }
    if meta.file_type().is_symlink() {
        // Not followed, and not recreated. A symlink in the backup would point
        // at the same place the original did, which is not a copy of anything.
        return Ok(());
    }
    std::fs::copy(src, dest).map(|_| ())
}

fn render_reset_plan(
    scope: ResetScope,
    planned: &[Planned],
    token: &str,
    committing: bool,
    no_reprovision: bool,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Factory reset — {}",
        if committing {
            "COMMITTING"
        } else {
            "DRY RUN. Nothing has been changed."
        }
    );
    let _ = writeln!(out, "\nscope: {} — {}", scope.as_str(), wrap(scope.summary(), 7));

    let del: Vec<&Planned> = planned
        .iter()
        .filter(|p| p.exists && p.target.how == Disposition::Delete)
        .collect();
    let trunc: Vec<&Planned> = planned
        .iter()
        .filter(|p| p.exists && p.target.how == Disposition::Truncate)
        .collect();
    let absent: Vec<&Planned> = planned.iter().filter(|p| !p.exists).collect();

    let _ = writeln!(out, "\nWILL BE REMOVED ({}):", del.len());
    if del.is_empty() {
        let _ = writeln!(out, "  (nothing — none of these paths exists)");
    }
    for p in &del {
        let _ = writeln!(out, "  {}", p.path.display());
        let _ = writeln!(out, "      {}", wrap(p.target.what, 6));
    }
    let _ = writeln!(out, "\nWILL BE EMPTIED, NOT REMOVED ({}):", trunc.len());
    if trunc.is_empty() {
        let _ = writeln!(out, "  (nothing)");
    }
    for p in &trunc {
        let _ = writeln!(out, "  {}", p.path.display());
        let _ = writeln!(out, "      {}", wrap(p.target.what, 6));
    }
    if !absent.is_empty() {
        let _ = writeln!(out, "\nNOT PRESENT, NOTHING TO DO ({}):", absent.len());
        for p in &absent {
            let _ = writeln!(out, "  {}", p.path.display());
        }
    }

    let _ = writeln!(out, "\nPRESERVED:");
    for k in preserved(scope) {
        let _ = writeln!(out, "  - {}", wrap(k, 4));
    }
    let _ = writeln!(
        out,
        "\nEverything removed is copied to ~/rime-reset-backup-<timestamp> first,\n\
         except caches. Delete that directory yourself once you are sure."
    );
    if no_reprovision {
        let _ = writeln!(
            out,
            "\n--no-reprovision: the desktop will NOT be re-seeded afterwards."
        );
    } else {
        let _ = writeln!(
            out,
            "\nAfterwards {PROVISIONER} re-seeds the files Rime Shell needs, and this\n\
             verb re-checks that they came back. Log out and back in for the\n\
             compositor to re-read its configuration."
        );
    }
    if !committing {
        let _ = writeln!(
            out,
            "\nTo perform it, run exactly:\n  rime recover reset --scope {} --commit --confirm {}",
            scope.as_str(),
            token
        );
    }
    out
}

pub fn main(cmd: RecoverCmd) -> i32 {
    match cmd {
        RecoverCmd::Status { json } => cmd_status(json),
        RecoverCmd::Repair(args) => cmd_repair(args),
        RecoverCmd::Reset(args) => cmd_reset(args),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────
// The parsers here decide what the surface says about a machine, and each one
// reads a file format this suite can present exactly. `tests/test-rime-recover
// .sh` drives the shipped binary against whole fixture trees; these pin the
// individual readers, where a wrong answer would be a plausible-looking
// sentence rather than a crash.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_booted_deployment_is_read_out_of_the_ostree_argument() {
        // A real Rime kernel command line, GRUB/BLS shape.
        let cmdline = "BOOT_IMAGE=(hd0,gpt2)/ostree/rime-1f0d/vmlinuz-7.1.5 \
                       root=UUID=abc ostree=/ostree/boot.1/rime/\
                       8f14e45fceea167a5a36dedd4bea2543f14e45fceea167a5a36dedd4bea25431/0 \
                       rw quiet";
        assert_eq!(
            booted_deployment(cmdline).as_deref(),
            Some("8f14e45fceea167a5a36dedd4bea2543f14e45fceea167a5a36dedd4bea25431")
        );
        // The `.0` serial suffix of a deploy-path layout must be stripped, not
        // included: the checksum is the identity `ostree admin pin` keys on.
        let deploy = "ostree=/ostree/deploy/rime/deploy/\
                      8f14e45fceea167a5a36dedd4bea2543f14e45fceea167a5a36dedd4bea25431.0";
        assert_eq!(
            booted_deployment(deploy).as_deref(),
            Some("8f14e45fceea167a5a36dedd4bea2543f14e45fceea167a5a36dedd4bea25431")
        );
        // No ostree argument at all: an honest None, never a guess.
        assert_eq!(booted_deployment("root=UUID=abc rw quiet"), None);
        // Short non-hex components must not be mistaken for a checksum.
        assert_eq!(booted_deployment("ostree=/ostree/boot.1/rime/short/0"), None);
    }

    #[test]
    fn usr_readonly_prefers_the_usr_mount_and_falls_back_to_root() {
        // An ostree machine: / is the composefs overlay, /usr has its own ro
        // mount.
        let m = "overlay / overlay ro,relatime,lowerdir=x 0 0\n\
                 none /usr overlay ro,relatime 0 0\n\
                 tmpfs /run tmpfs rw,nosuid 0 0\n";
        assert_eq!(usr_readonly(m), (Some(true), Some("overlay".to_string())));
        // No /usr line: the root mount's flags answer instead of the row
        // reporting drift on a machine that has none.
        let m2 = "overlay / overlay ro,relatime 0 0\ntmpfs /run tmpfs rw 0 0\n";
        assert_eq!(usr_readonly(m2), (Some(true), Some("overlay".to_string())));
        // A genuinely writable /usr is the drift the row exists to report.
        let m3 = "overlay / overlay ro 0 0\nnone /usr overlay rw,relatime 0 0\n";
        assert_eq!(usr_readonly(m3), (Some(false), Some("overlay".to_string())));
        // `ro` must be matched as a whole option. `rootcontext=` and
        // `errors=remount-ro` both contain the two letters.
        let m4 = "/dev/sda2 /usr ext4 rw,errors=remount-ro,rootcontext=x 0 0\n";
        assert_eq!(usr_readonly(m4).0, Some(false));
        // Nothing at all: unavailable, not "read-write".
        assert_eq!(usr_readonly(""), (None, None));
    }

    #[test]
    fn a_default_route_is_read_never_probed() {
        let table = "Iface\tDestination\tGateway \tFlags\n\
                     wlan0\t00000000\t0101A8C0\t0003\n\
                     wlan0\t0001A8C0\t00000000\t0001\n";
        assert!(has_default_route(table));
        // Only non-default routes: a machine on a LAN with no gateway.
        let no_default = "Iface\tDestination\tGateway\n\
                          wlan0\t0001A8C0\t00000000\t0001\n";
        assert!(!has_default_route(no_default));
        // The header alone, and an empty file.
        assert!(!has_default_route("Iface\tDestination\tGateway\n"));
        assert!(!has_default_route(""));
    }

    #[test]
    fn os_release_values_are_unquoted() {
        let dir = std::env::temp_dir().join(format!("rime-recover-osr-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("etc")).unwrap();
        std::fs::write(
            dir.join("etc/os-release"),
            "NAME=\"Rime OS\"\n# a comment\nVERSION_ID=43\nVARIANT_ID='gaming'\n\n",
        )
        .unwrap();
        let sys = Sys { fixture: Some(dir.clone()) };
        let m = os_release(&sys);
        assert_eq!(m.get("NAME").map(String::as_str), Some("Rime OS"));
        assert_eq!(m.get("VERSION_ID").map(String::as_str), Some("43"));
        assert_eq!(m.get("VARIANT_ID").map(String::as_str), Some("gaming"));
        assert!(!m.contains_key("# a comment"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_package_state_we_may_not_read_is_unavailable_rather_than_none() {
        // The live bug: /var/lib/rime/pkg is 0700 root:root, `rime recover
        // status` runs as the desktop user, and the row asserted "no user
        // packages on this machine" as Verified on every machine that had
        // some. The suite missed it because its fixtures are owned by whoever
        // runs the tests, so nothing ever hit EACCES.
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("rime-recover-eacces-{}", std::process::id()));
        let pkg = dir.join("var/lib/rime/pkg");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("state.json"), "{\"requested\":[\"ncdu\"]}").unwrap();

        let mut perms = std::fs::metadata(&pkg).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&pkg, perms).unwrap();

        let sys = Sys { fixture: Some(dir.clone()) };
        // Require the exact error, so an unrelated failure cannot masquerade as
        // a successful seal and leave the assertion asserting nothing.
        let sealed = match sys.read_result("/var/lib/rime/pkg/state.json") {
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => true,
            Ok(_) => false, // root, or CAP_DAC_OVERRIDE
            Err(e) => panic!("expected PermissionDenied while sealing, got {e:?}"),
        };
        let row = package_row(&sys, "43");

        let mut perms = std::fs::metadata(&pkg).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&pkg, perms).ok();
        std::fs::remove_dir_all(&dir).ok();

        if !sealed {
            return; // root, or CAP_DAC_OVERRIDE: the mode bit proves nothing
        }
        assert_eq!(
            row.state,
            Health::Unavailable,
            "a refused read must not be reported as a verified absence"
        );
        assert!(
            !row.detail.contains("no user packages"),
            "the row claimed absence it never established: {}",
            row.detail
        );
        assert!(row.action.is_some(), "the user needs to be told how to see it");
    }

    #[test]
    fn a_genuinely_absent_package_state_is_still_verified_none() {
        // The other half: ENOENT must stay a checked fact, or the fix would
        // turn the common case (nothing installed) into a permanent warning.
        let dir = std::env::temp_dir().join(format!("rime-recover-noenoent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Sys { fixture: Some(dir.clone()) };
        let row = package_row(&sys, "43");
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(row.state, Health::Verified);
        assert!(row.detail.contains("no user packages"));
    }

    // ── reads that were refused, and reads that found nothing ────────────────
    //
    // The whole surface is file reads, and `unwrap_or_default()` answered both
    // questions with the same empty string. Every row below then reported the
    // empty string as a measurement of the machine — and the GPU one attached
    // `sudo rime rollback` to it.

    /// A whole machine, presented as a tree. The same shape
    /// `tests/test-rime-recover.sh` builds, small enough to keep in the crate
    /// so the individual rows can be pinned without spawning the binary.
    struct Machine(PathBuf);

    impl Machine {
        fn new(name: &str) -> Machine {
            let dir = std::env::temp_dir()
                .join(format!("rime-recover-{name}-{}", std::process::id()));
            std::fs::remove_dir_all(&dir).ok();
            let w = |rel: &str, body: &str| {
                let p = dir.join(rel);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, body).unwrap();
            };
            let csum = "8f14e45fceea167a5a36dedd4bea2543f14e45fceea167a5a36dedd4bea25431";
            w("run/ostree-booted", "");
            w(
                "proc/cmdline",
                &format!("BOOT_IMAGE=/vmlinuz root=UUID=x ostree=/ostree/boot.1/rime/{csum}/0 rw\n"),
            );
            w("proc/mounts", "overlay / overlay ro,relatime 0 0\n");
            w("proc/modules", "amdgpu 1 0 - Live 0x0\ndrm 1 0 - Live 0x0\n");
            w(
                "proc/net/route",
                "Iface\tDestination\tGateway\nwlan0\t00000000\t0101A8C0\t0003\n",
            );
            w("etc/os-release", "NAME=\"Rime OS\"\nVERSION_ID=43\nVARIANT_ID=gaming\n");
            w("usr/share/rime-shell/shell.qml", "shell\n");
            // An AMD display controller, so the GPU row has something to check.
            w("sys/bus/pci/devices/0000:03:00.0/class", "0x030000\n");
            w("sys/bus/pci/devices/0000:03:00.0/vendor", "0x1002\n");
            w("sys/bus/pci/devices/0000:03:00.0/device", "0x1636\n");
            std::fs::create_dir_all(dir.join(format!("ostree/deploy/rime/deploy/{csum}.0")))
                .unwrap();
            std::fs::create_dir_all(dir.join("ostree/deploy/rime/deploy/aaaa.0")).unwrap();
            Machine(dir)
        }

        fn sys(&self) -> Sys {
            Sys { fixture: Some(self.0.clone()) }
        }

        fn row(&self, id: &str) -> Row {
            probe(&self.sys())
                .rows
                .into_iter()
                .find(|r| r.id == id)
                .unwrap_or_else(|| panic!("no row {id}"))
        }
    }

    impl Drop for Machine {
        fn drop(&mut self) {
            unseal(&self.0);
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    /// Take every mode bit off `rel`, then check with `probe` that the access
    /// the row depends on really is refused now.
    ///
    /// `probe` is spelled out per call site because the mode bit that stops one
    /// access does not stop another: a 0000 *file* still stats fine, since stat
    /// needs only search permission on the parent. Sealing a file and then
    /// asserting about a `metadata` call would assert nothing.
    ///
    /// Root and anything holding `CAP_DAC_OVERRIDE` walk through 0000, so the
    /// question asked is whether the access now fails with `PermissionDenied` —
    /// not whether the caller looks like root, which would be a guess. Any
    /// other error is a broken fixture rather than a seal, and panics instead
    /// of quietly leaving an assertion with nothing to assert.
    fn seal(root: &Path, rel: &str, probe: impl FnOnce(&Path) -> std::io::Result<()>) -> bool {
        use std::os::unix::fs::PermissionsExt;
        let p = root.join(rel);
        let mut perms = std::fs::metadata(&p).expect("stat").permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&p, perms).expect("chmod");
        match probe(root) {
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => true,
            Ok(()) => false, // root, or CAP_DAC_OVERRIDE
            Err(e) => panic!("expected PermissionDenied while sealing {rel}, got {e:?}"),
        }
    }

    fn read(rel: &'static str) -> impl FnOnce(&Path) -> std::io::Result<()> {
        move |root| std::fs::read(root.join(rel)).map(|_| ())
    }

    fn list(rel: &'static str) -> impl FnOnce(&Path) -> std::io::Result<()> {
        move |root| std::fs::read_dir(root.join(rel)).map(|_| ())
    }

    fn stat(rel: &'static str) -> impl FnOnce(&Path) -> std::io::Result<()> {
        move |root| std::fs::metadata(root.join(rel)).map(|_| ())
    }

    /// Put the mode bits back on everything under `root`, so the fixture can be
    /// removed and a failing assertion does not leave a 0000 directory behind.
    fn unseal(root: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let Ok(entries) = std::fs::read_dir(root) else { return };
        for e in entries.flatten() {
            let p = e.path();
            let mut perms = match std::fs::symlink_metadata(&p) {
                Ok(m) => m.permissions(),
                Err(_) => continue,
            };
            perms.set_mode(if p.is_dir() { 0o755 } else { 0o644 });
            let _ = std::fs::set_permissions(&p, perms);
            if p.is_dir() {
                unseal(&p);
            }
        }
    }

    impl Machine {
        fn route(&self, id: &str) -> Route {
            probe(&self.sys())
                .routes
                .into_iter()
                .find(|r| r.id == id)
                .unwrap_or_else(|| panic!("no route {id}"))
        }
    }

    /// `rimed/rime/src/boot.rs`'s vendor GUID, duplicated here rather than
    /// imported: it is a private constant of that module, and the fixture
    /// byte layout below is the one `tests/test-boot-v2.sh` already uses.
    const TEST_LOADER_GUID: &str = "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";

    #[test]
    fn an_unreadable_stubinfo_does_not_settle_the_rescue_route_either_way() {
        // `chain.booted_from_uki` gates the rescue-target route only on the
        // systemd-boot path — GRUB's menu is editable regardless of what
        // booted. Force systemd-boot via LoaderInfo, then refuse the
        // StubInfo read: the route must say "cannot be determined", not pick
        // "UKI" or "no UKI" off a read that never happened.
        let m = Machine::new("route-stubinfo-eacces");
        let efivars = m.0.join("sys/firmware/efi/efivars");
        std::fs::create_dir_all(&efivars).unwrap();
        std::fs::write(
            efivars.join(format!("LoaderInfo-{TEST_LOADER_GUID}")),
            b"\x07\x00\x00\x00s\x00y\x00s\x00t\x00e\x00m\x00d\x00-\x00b\x00o\x00o\x00t\x00",
        )
        .unwrap();
        std::fs::write(efivars.join(format!("StubInfo-{TEST_LOADER_GUID}")), b"\x07\x00\x00\x00")
            .unwrap();
        // Seal the StubInfo file itself, not its directory: chmod 0000 on the
        // file blocks the read (open() needs the file's own read bit) without
        // also blocking the LoaderInfo read next to it that bootloader
        // detection depends on.
        let target = format!("sys/firmware/efi/efivars/StubInfo-{TEST_LOADER_GUID}");
        let sealed = seal(&m.0, &target, |root| std::fs::read(root.join(&target)).map(|_| ()));
        if !sealed {
            return; // the caller overrides the mode bit; it proves nothing here
        }
        let route = m.route("rescue-target");
        assert!(
            route.available.is_none(),
            "a refused StubInfo read must not settle the rescue route either way, got {:?}",
            route.available
        );
        assert!(
            route.how.contains("cannot be determined"),
            "the route should say it could not be determined, got: {}",
            route.how
        );
    }

    #[test]
    fn an_unreadable_loaderbootcountpath_does_not_settle_boot_counting_either_way() {
        let m = Machine::new("route-bootcount-eacces");
        let efivars = m.0.join("sys/firmware/efi/efivars");
        std::fs::create_dir_all(&efivars).unwrap();
        std::fs::write(
            efivars.join(format!("LoaderBootCountPath-{TEST_LOADER_GUID}")),
            b"\x07\x00\x00\x00",
        )
        .unwrap();
        let target = format!("sys/firmware/efi/efivars/LoaderBootCountPath-{TEST_LOADER_GUID}");
        let sealed = seal(&m.0, &target, |root| std::fs::read(root.join(&target)).map(|_| ()));
        if !sealed {
            return;
        }
        let route = m.route("boot-counting");
        assert!(
            route.available.is_none(),
            "a refused LoaderBootCountPath read must not settle boot counting \
             either way, got {:?}",
            route.available
        );
        assert!(
            route.how.contains("cannot be determined"),
            "the route should say it could not be determined, got: {}",
            route.how
        );
    }

    #[test]
    fn a_directory_wide_efivarfs_refusal_does_not_default_the_rescue_route_to_grub() {
        // The gap the single-file seal above cannot reach: a refusal on the
        // *directory* takes LoaderInfo down together with StubInfo, so
        // `chain.bootloader` falls back to the cmdline heuristic and reports
        // "grub" — every Rime cmdline carries ostree=, UKI or not. Trusting
        // that here would offer the rescue route on exactly the
        // systemd-boot+UKI machine it must not exist on.
        let m = Machine::new("route-efivars-eacces");
        let efivars = m.0.join("sys/firmware/efi/efivars");
        std::fs::create_dir_all(&efivars).unwrap();
        std::fs::write(
            efivars.join(format!("LoaderInfo-{TEST_LOADER_GUID}")),
            b"\x07\x00\x00\x00s\x00y\x00s\x00t\x00e\x00m\x00d\x00-\x00b\x00o\x00o\x00t\x00",
        )
        .unwrap();
        std::fs::write(efivars.join(format!("StubInfo-{TEST_LOADER_GUID}")), b"\x07\x00\x00\x00")
            .unwrap();
        let target = format!("sys/firmware/efi/efivars/LoaderInfo-{TEST_LOADER_GUID}");
        let sealed =
            seal(&m.0, "sys/firmware/efi/efivars", |root| std::fs::read(root.join(&target)).map(|_| ()));
        if !sealed {
            return; // the caller overrides the mode bit; it proves nothing here
        }
        let route = m.route("rescue-target");
        assert!(
            route.available.is_none(),
            "bootloader identity being unreadable must not settle the rescue \
             route either way, got {:?}",
            route.available
        );
        assert!(
            !route.how.contains("grub"),
            "must not fall back to the cmdline guess and call it a bootloader, got: {}",
            route.how
        );
        assert!(
            route.how.contains("cannot be determined"),
            "got: {}",
            route.how
        );
    }

    #[test]
    fn the_bootloader_label_says_so_when_the_identity_could_not_be_read() {
        // The route above was fixed and the LABEL was not. `probe` consulted
        // `bootloader_unavailable` while building the routes and then dropped
        // it, so `Surface` could not carry it and both renderings of
        // `rime recover status` printed a bare "grub" — the kernel command
        // line's guess — about a machine whose bootloader nobody could read.
        // `rime boot status` printed the caveat for the same fixture, so the
        // two surfaces contradicted each other on the same machine.
        let m = Machine::new("label-efivars-eacces");
        let efivars = m.0.join("sys/firmware/efi/efivars");
        std::fs::create_dir_all(&efivars).unwrap();
        std::fs::write(
            efivars.join(format!("LoaderInfo-{TEST_LOADER_GUID}")),
            b"\x07\x00\x00\x00s\x00y\x00s\x00t\x00e\x00m\x00d\x00-\x00b\x00o\x00o\x00t\x00",
        )
        .unwrap();
        let target = format!("sys/firmware/efi/efivars/LoaderInfo-{TEST_LOADER_GUID}");
        let sealed = seal(&m.0, "sys/firmware/efi/efivars", |root| {
            std::fs::read(root.join(&target)).map(|_| ())
        });
        if !sealed {
            return; // the caller overrides the mode bit; it proves nothing here
        }
        let s = probe(&m.sys());
        // The label itself is deliberately unchanged: rime-shell reads this
        // value and two shell suites pin the bare strings.
        assert_eq!(
            s.bootloader, "grub",
            "the cmdline fallback is still the label; only the caveat is new"
        );
        let why = s
            .bootloader_unavailable
            .as_deref()
            .expect("an unreadable LoaderInfo must reach the surface, not stop at the routes");
        assert!(
            why.contains("LoaderInfo") && why.contains("Permission denied"),
            "the caveat must carry the reason it could not be read, got: {why}"
        );
    }

    #[test]
    fn a_readable_efivarfs_leaves_the_bootloader_label_uncaveated() {
        // The other half, and the one that stops the caveat from being
        // unconditional prose that always prints and therefore says nothing.
        let m = Machine::new("label-efivars-readable");
        let efivars = m.0.join("sys/firmware/efi/efivars");
        std::fs::create_dir_all(&efivars).unwrap();
        let s = probe(&m.sys());
        assert_eq!(s.bootloader, "grub");
        assert!(
            s.bootloader_unavailable.is_none(),
            "a LoaderInfo that is merely absent is a reading, not a refusal, got {:?}",
            s.bootloader_unavailable
        );
    }

    #[test]
    fn a_genuinely_absent_stubinfo_still_reports_the_rescue_route_on_grub() {
        // The other half: on a plain GRUB machine (Machine::new's default),
        // StubInfo and LoaderInfo are both genuinely absent, and the route
        // must still be reported rather than turned into a permanent
        // "cannot be determined" by an over-broad fix.
        let m = Machine::new("route-stubinfo-absent");
        let route = m.route("rescue-target");
        assert!(route.available.is_some());
        assert!(route.how.contains("grub"));
    }

    #[test]
    fn a_genuinely_absent_loaderbootcountpath_still_reports_boot_counting_on_grub() {
        let m = Machine::new("route-bootcount-absent");
        let route = m.route("boot-counting");
        assert_eq!(route.available, Some(false));
        assert!(route.how.contains("GRUB"));
    }

    #[test]
    fn a_module_list_we_may_not_read_never_recommends_a_rollback() {
        // The worst outcome in the class: `/proc/modules` read into an empty
        // string, every GPU therefore missing its driver, and the row telling
        // the user to roll their operating system back.
        let m = Machine::new("modules-eacces");
        if !seal(&m.0, "proc/modules", read("proc/modules")) {
            return; // the caller overrides the mode bit; it proves nothing here
        }
        let row = m.row("gpu-driver");
        assert_eq!(
            row.state,
            Health::Unavailable,
            "a refused read is not a measurement that no module is loaded"
        );
        assert!(
            !row.detail.contains("no kernel module loaded"),
            "the row claimed a module list it never read: {}",
            row.detail
        );
        assert_eq!(
            row.action, None,
            "nothing read the file, so nothing may propose a rollback"
        );
    }

    #[test]
    fn a_module_list_that_is_absent_is_also_not_a_missing_driver() {
        // CONFIG_MODULES=n: no /proc/modules at all, and every driver compiled
        // into the kernel. "No module loaded" is exactly as false there as it
        // is under EACCES, so absence is not carved out.
        let m = Machine::new("modules-enoent");
        std::fs::remove_file(m.0.join("proc/modules")).unwrap();
        let row = m.row("gpu-driver");
        assert_eq!(row.state, Health::Unavailable);
        assert_eq!(row.action, None);
    }

    #[test]
    fn a_gpu_with_no_module_in_a_readable_list_is_still_attention() {
        // The other half. A module list that WAS read and does not carry the
        // driver is the real fault this row exists to report, and it must keep
        // naming the rollback.
        let m = Machine::new("modules-missing");
        std::fs::write(m.0.join("proc/modules"), "drm 1 0 - Live 0x0\n").unwrap();
        let row = m.row("gpu-driver");
        assert_eq!(row.state, Health::Attention);
        assert!(row.detail.contains("no kernel module loaded"), "{}", row.detail);
        assert_eq!(row.action.as_deref(), Some("sudo rime rollback"));
    }

    #[test]
    fn a_routing_table_we_may_not_read_is_not_a_machine_without_a_route() {
        let m = Machine::new("route-eacces");
        if !seal(&m.0, "proc/net/route", read("proc/net/route")) {
            return;
        }
        let row = m.row("network");
        assert_eq!(
            row.state,
            Health::Unavailable,
            "a refused read is not a measurement that there is no route"
        );
        assert!(
            row.detail.contains("could not be read"),
            "the row must say the read failed, not describe the network: {}",
            row.detail
        );
    }

    #[test]
    fn a_routing_table_with_no_default_route_is_still_attention() {
        let m = Machine::new("route-none");
        std::fs::write(m.0.join("proc/net/route"), "Iface\tDestination\tGateway\n").unwrap();
        let row = m.row("network");
        assert_eq!(row.state, Health::Attention);
        assert!(row.detail.contains("no default route"), "{}", row.detail);
    }

    #[test]
    fn a_command_line_we_may_not_read_is_not_a_command_line_without_ostree() {
        // `/run/ostree-booted` exists, so the machine demonstrably booted a
        // deployment. Saying the command line carries no `ostree=` argument
        // when the command line was never read puts an Attention on a machine
        // nobody looked at.
        let m = Machine::new("cmdline-eacces");
        if !seal(&m.0, "proc/cmdline", read("proc/cmdline")) {
            return;
        }
        let row = m.row("current-deployment");
        assert_eq!(row.state, Health::Unavailable);
        assert!(
            !row.detail.contains("no ostree= argument"),
            "the row claimed a command line it never read: {}",
            row.detail
        );
    }

    #[test]
    fn a_command_line_that_really_carries_no_ostree_argument_is_still_attention() {
        let m = Machine::new("cmdline-no-ostree");
        std::fs::write(m.0.join("proc/cmdline"), "root=UUID=x rw quiet\n").unwrap();
        let row = m.row("current-deployment");
        assert_eq!(row.state, Health::Attention);
        assert!(row.detail.contains("no ostree= argument"), "{}", row.detail);
    }

    #[test]
    fn a_stateroot_we_may_not_list_is_not_a_smaller_deployment_count() {
        // A partial count reported as complete is what hides a rollback target.
        // One stateroot is unreadable and another holds a single deployment, so
        // skipping the unreadable one and reporting the rest told the user
        // "only the booted deployment exists, so there is nothing to roll back
        // to yet" on a machine that had somewhere to go.
        let m = Machine::new("deploy-eacces");
        std::fs::create_dir_all(m.0.join("ostree/deploy/spare/deploy/cccc.0")).unwrap();
        if !seal(&m.0, "ostree/deploy/rime/deploy", list("ostree/deploy/rime/deploy")) {
            return;
        }
        assert!(
            deployment_count(&m.sys()).is_err(),
            "a directory we may not list must not shrink the count"
        );
        let row = m.row("previous-deployment");
        assert_eq!(
            row.state,
            Health::Unavailable,
            "a partial enumeration must not be reported as a complete count"
        );
        assert!(
            row.detail.contains("could not be read"),
            "the row must say the read failed: {}",
            row.detail
        );
    }

    #[test]
    fn deployments_we_may_not_stat_are_not_deployments_that_are_absent() {
        // The second half of the enumeration. Mode 0400 on the deploy directory
        // lists the names and refuses every stat, and `Path::is_dir` answers
        // false for a refused stat exactly as it does for a missing path — so
        // both deployments vanished from the count.
        use std::os::unix::fs::PermissionsExt;
        let m = Machine::new("deploy-nostat");
        let d = m.0.join("ostree/deploy/rime/deploy");
        let mut perms = std::fs::metadata(&d).unwrap().permissions();
        perms.set_mode(0o400);
        std::fs::set_permissions(&d, perms).unwrap();
        assert!(
            std::fs::read_dir(&d).is_ok(),
            "0400 must still list: the whole point is a stat that fails after a \
             readdir that did not"
        );
        match std::fs::metadata(d.join("aaaa.0")) {
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {}
            Ok(_) => return, // root, or CAP_DAC_OVERRIDE
            Err(e) => panic!("expected PermissionDenied, got {e:?}"),
        }
        let row = m.row("previous-deployment");
        assert_eq!(row.state, Health::Unavailable);
        assert!(
            row.detail.contains("could not be read"),
            "the row must name the refused stat rather than report a count of 0: {}",
            row.detail
        );
    }

    #[test]
    fn a_stateroot_with_no_deploy_directory_yet_still_counts_the_others() {
        // Absence stays absence: a stateroot mid-creation contributes nothing
        // and must not turn the whole count into a shrug.
        let m = Machine::new("deploy-partial");
        std::fs::create_dir_all(m.0.join("ostree/deploy/fresh")).unwrap();
        assert_eq!(deployment_count(&m.sys()), Ok(2));
        assert_eq!(m.row("previous-deployment").state, Health::Available);
    }

    #[test]
    fn a_shell_we_may_not_stat_does_not_become_an_image_build_defect() {
        // The `(false, _)` arm tells the user the image did not ship Rime Shell
        // and to roll back. On a stock image /usr/share/rime-shell is 0755, so
        // this is the shape of the defect rather than a refusal users hit — but
        // the recommendation is a rollback, and a failed stat must not produce
        // one.
        let m = Machine::new("shell-eacces");
        // The parent, not the file: a 0000 file still stats, and `shipped` is
        // a stat.
        if !seal(&m.0, "usr/share/rime-shell", stat("usr/share/rime-shell/shell.qml")) {
            return;
        }
        let row = m.row("rime-shell");
        assert_eq!(row.state, Health::Unavailable);
        assert!(
            !row.detail.contains("did not ship Rime Shell"),
            "the row claimed a stat it never made: {}",
            row.detail
        );
        assert_eq!(row.action, None);
    }

    #[test]
    fn a_shell_that_is_really_missing_is_still_an_image_build_defect() {
        let m = Machine::new("shell-enoent");
        std::fs::remove_file(m.0.join("usr/share/rime-shell/shell.qml")).unwrap();
        let row = m.row("rime-shell");
        assert_eq!(row.state, Health::Unavailable);
        assert!(row.detail.contains("did not ship Rime Shell"), "{}", row.detail);
        assert_eq!(row.action.as_deref(), Some("sudo rime rollback"));
    }

    #[test]
    fn the_fixture_root_is_a_prefix_and_never_falls_through_to_the_real_system() {
        // The mistake this guards: `Path::join` on an absolute argument
        // discards the prefix and reads the real machine, so a test would pass
        // on the author's laptop and assert nothing.
        let sys = Sys { fixture: Some(PathBuf::from("/tmp/fixture")) };
        assert_eq!(sys.path("/proc/cmdline"), PathBuf::from("/tmp/fixture/proc/cmdline"));
        assert_eq!(
            sys.path("/usr/libexec/rime-shell-firstrun"),
            PathBuf::from("/tmp/fixture/usr/libexec/rime-shell-firstrun")
        );
        let real = Sys { fixture: None };
        assert_eq!(real.path("/proc/cmdline"), PathBuf::from("/proc/cmdline"));
    }

    #[test]
    fn every_repair_step_has_a_diagnosis() {
        // A step in the table with no branch in `applicable_repairs` would be
        // silently never offered — or, with a permissive default, offered
        // always. Both are wrong, and this is the check that keeps the two
        // lists in step.
        let known = ["reprovision-desktop", "rebuild-package-extension"];
        for step in REPAIRS {
            assert!(
                known.contains(&step.id),
                "{} has no diagnosis in applicable_repairs()",
                step.id
            );
        }
        assert_eq!(known.len(), REPAIRS.len());
    }

    #[test]
    fn the_doctor_json_carries_every_check_and_the_counts() {
        let checks = vec![
            Check { ok: true, what: "one".into() },
            Check { ok: false, what: "two \"quoted\"".into() },
        ];
        let out = render_doctor(&checks, true);
        let v: Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(v["total"], 2);
        assert_eq!(v["passed"], 1);
        assert_eq!(v["warned"], 1);
        assert_eq!(v["checks"][1]["check"], "two \"quoted\"");
        assert_eq!(v["checks"][1]["ok"], false);
        // And the text form is unchanged, because scripts already read it.
        let text = render_doctor(&checks, false);
        assert!(text.contains("[PASS] one"));
        assert!(text.contains("[WARN] two"));
    }

    #[test]
    fn the_surface_reports_every_row_even_on_an_empty_fixture() {
        // A machine that is nothing like a Rime install: no /proc, no
        // /ostree, no os-release. Every row must still be present and say
        // something, because a UI keyed on the row ids must not lose a row
        // when a read fails.
        let dir = std::env::temp_dir().join(format!("rime-recover-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = probe(&Sys { fixture: Some(dir.clone()) });
        let ids: Vec<&str> = s.rows.iter().map(|r| r.id).collect();
        let want: Vec<&str> = rimed_core::recover::ROWS.iter().map(|r| r.id).collect();
        assert_eq!(ids, want);
        for r in &s.rows {
            assert!(!r.detail.is_empty(), "{} has no detail", r.id);
        }
        assert_eq!(s.bootloader, "unknown");
        // And a route list that always names the boot-entry decision.
        assert!(s.routes.iter().any(|r| r.id == "recovery-boot-entry"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_symlinked_target_is_refused_rather_than_followed() {
        let base = std::env::temp_dir().join(format!("rime-recover-sym-{}", std::process::id()));
        let home = base.join("home");
        let outside = base.join("outside");
        std::fs::create_dir_all(home.join(".cache")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("precious"), b"do not delete").unwrap();
        let link = home.join(".cache/rime-shell");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let t = targets(ResetScope::Desktop)
            .into_iter()
            .find(|t| t.rel == ".cache/rime-shell")
            .unwrap();
        let home_real = std::fs::canonicalize(&home).unwrap();
        let err = safe_to_touch(&home_real, t, &link).expect_err("must refuse");
        assert!(err.contains("symlink"), "wrong refusal: {err}");
        // And the thing it pointed at is untouched — the assertion that makes
        // the refusal mean something.
        assert!(outside.join("precious").exists());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_target_whose_parent_escapes_the_home_is_refused() {
        let base = std::env::temp_dir().join(format!("rime-recover-esc-{}", std::process::id()));
        let home = base.join("home");
        let elsewhere = base.join("elsewhere/rime-shell");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("keep"), b"x").unwrap();
        // A symlinked PARENT: ~/.cache -> ../elsewhere. The final component is
        // a real directory, so the symlink check above does not fire and the
        // parent resolution is what has to catch it.
        std::os::unix::fs::symlink(base.join("elsewhere"), home.join(".cache")).unwrap();

        let t = targets(ResetScope::Desktop)
            .into_iter()
            .find(|t| t.rel == ".cache/rime-shell")
            .unwrap();
        let home_real = std::fs::canonicalize(&home).unwrap();
        let err = safe_to_touch(&home_real, t, &home_real.join(t.rel)).expect_err("must refuse");
        assert!(err.contains("outside"), "wrong refusal: {err}");
        assert!(elsewhere.join("keep").exists());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_file_target_that_is_really_a_directory_is_refused() {
        // Without this, a directory sitting where the table declares a file
        // would be passed to `remove_file`, which fails — but the check is
        // what makes the refusal explicit rather than an errno.
        let base = std::env::temp_dir().join(format!("rime-recover-kind-{}", std::process::id()));
        let home = base.join("home");
        std::fs::create_dir_all(home.join(".config/rime-shell/input.json")).unwrap();
        let t = targets(ResetScope::Desktop)
            .into_iter()
            .find(|t| t.rel == ".config/rime-shell/input.json")
            .unwrap();
        let home_real = std::fs::canonicalize(&home).unwrap();
        let err = safe_to_touch(&home_real, t, &home_real.join(t.rel)).expect_err("must refuse");
        assert!(err.contains("not a regular file"), "wrong refusal: {err}");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_plan_finds_only_what_exists_and_the_token_follows_it() {
        let base = std::env::temp_dir().join(format!("rime-recover-plan-{}", std::process::id()));
        let home = base.join("home");
        std::fs::create_dir_all(home.join(".config/rime-shell")).unwrap();
        std::fs::write(home.join(".config/rime-shell/input.json"), b"{}").unwrap();
        let home_real = std::fs::canonicalize(&home).unwrap();

        let p1 = plan(&home_real, ResetScope::Desktop);
        assert_eq!(token_paths(&p1).len(), 1);
        let t1 = confirm_token(ResetScope::Desktop, &token_paths(&p1));

        // One more file appears: the token must change, so a confirmation
        // printed before it is refused afterwards.
        std::fs::write(home_real.join(".config/rime-shell/display.json"), b"{}").unwrap();
        let p2 = plan(&home_real, ResetScope::Desktop);
        assert_eq!(token_paths(&p2).len(), 2);
        assert_ne!(t1, confirm_token(ResetScope::Desktop, &token_paths(&p2)));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn the_rendered_plan_names_the_loss_and_the_exact_command() {
        let base = std::env::temp_dir().join(format!("rime-recover-render-{}", std::process::id()));
        let home = base.join("home");
        std::fs::create_dir_all(home.join(".config/rime")).unwrap();
        std::fs::write(home.join(".config/rime/blueprint.toml"), b"x").unwrap();
        let home_real = std::fs::canonicalize(&home).unwrap();
        let planned = plan(&home_real, ResetScope::User);
        let token = confirm_token(ResetScope::User, &token_paths(&planned));
        let text = render_reset_plan(ResetScope::User, &planned, &token, false, false);

        // The blueprint must be named, with its own loss line, not merely
        // counted. It is the one file in Rime whose contract is that no
        // program writes it.
        assert!(text.contains("blueprint.toml"));
        assert!(text.contains("rime sync export"));
        // The preserved list must be there, or "explicit about what is
        // preserved" is not satisfied.
        assert!(text.contains("PRESERVED:"));
        assert!(text.contains(".ssh"));
        // And the one command line that performs it, carrying this plan's
        // token.
        assert!(text.contains(&format!("--confirm {token}")));
        assert!(text.contains("DRY RUN"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn wrap_collapses_source_indentation_and_bounds_the_width() {
        // The details are multi-line Rust string literals, so they arrive
        // carrying their own source indentation. A report that printed that
        // verbatim would have ragged gaps mid-sentence.
        let ragged = "one two    three\n                     four five";
        assert_eq!(wrap(ragged, 0), "one two three four five");

        let long = "one two three four five six seven eight nine ten eleven twelve \
                    thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty \
                    twentyone twentytwo twentythree twentyfour twentyfive twentysix";
        let w = wrap(long, 10);
        let mut lines = w.lines();
        // The first line carries no indent of its own: the caller has already
        // printed a column before it.
        let first = lines.next().expect("at least one line");
        assert!(!first.starts_with(' '));
        // Continuations are indented by exactly the requested amount, and the
        // word after the indent is a word rather than more whitespace.
        let mut continuations = 0;
        for l in lines {
            assert_eq!(&l[..10], "          ", "continuation not indented by 10");
            assert!(!l[10..].starts_with(' '), "double indent on a continuation");
            continuations += 1;
        }
        assert!(continuations >= 1, "the long line was never wrapped");
        for l in w.lines() {
            assert!(l.len() <= 96, "line too long: {} chars", l.len());
        }
        assert_eq!(wrap("", 4), "");
    }
}
