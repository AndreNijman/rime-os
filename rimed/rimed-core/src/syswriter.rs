//! The one and only path from an [`Action`] to a real hardware effect.
//!
//! `rimed-core` never writes sysfs or spawns a process directly; it emits
//! [`Action`]s and hands them to a [`SysWriter`]. Production uses
//! [`RealWriter`] (which also honours dry-run); tests use [`MockWriter`], which
//! records intended actions and touches nothing. This is what lets every logic
//! path be unit-tested without writing real sysfs.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;

use crate::tier::Action;

/// What applying one [`Action`] actually did to the machine.
///
/// This exists because `Ok(())` was not enough to tell the truth with. Most of
/// what this writer touches is *tolerated* — a kernel-managed interrupt refuses
/// an affinity write with `-EIO`, a driver refuses an EPP write while the
/// performance governor is selected — and tolerating a refusal is right: it
/// must never abort the rest of a plan, least of all a restore plan. But
/// tolerating it and *reporting success for it* are different things, and the
/// second is what let `rime game status` say "12 IRQs steered" on a machine
/// that had steered none.
///
/// ── The rule for actions that drive more than one write ─────────────────────
///
/// Only [`Action::IrqAffinity`] is one action to exactly one write. `Governor`
/// fans out across every cpufreq policy, `CgroupEnsure` writes `cpuset.mems`
/// and `cpuset.cpus`, `FanSafeRestore` walks a ladder until a rung sticks. For
/// all of those the outcome is **any-landed**: `Landed` means at least one
/// write reached the machine. That is the honest reading for a ladder (one
/// rung landing is the whole point) and the only defensible one for a fan-out
/// without inventing a per-write count no caller asked for.
///
/// So a count derived from these is only a measurement for the 1:1 action.
/// Anything wanting a per-write count of a fan-out action has to plan one
/// action per write first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The value reached the machine — or would have, under dry-run.
    Landed,
    /// Nothing reached the machine. Carries the reason, in the same words the
    /// skip was logged with, so a caller can report *why* rather than only
    /// that something did not happen.
    Refused(String),
    /// The action was issued and the machine could not be read back to say
    /// whether it took.
    ///
    /// The third answer, and it exists because two were not enough. A failed
    /// call, an absent feature and an unchecked assumption are three different
    /// facts, and this crate has now collapsed them into one cheerful one
    /// often enough to name the class: see the `Landed` docs below, and the
    /// sched-ext switch that reported a scheduler it had never loaded.
    ///
    /// `Unknown` is NOT a success: [`Outcome::landed`] is false for it, so
    /// nothing that counts landings counts one. It is also not a refusal —
    /// reporting it as one would assert the opposite falsehood.
    Unknown(String),
}

impl Outcome {
    /// True when the action had an effect. Named rather than matched inline
    /// because the fan restore ladder branches on it three times.
    ///
    /// False for [`Outcome::Unknown`], deliberately: "I could not tell" must
    /// never be counted as a landing.
    pub fn landed(&self) -> bool {
        matches!(self, Outcome::Landed)
    }

    /// The reason carried by a non-landing outcome, if any.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Outcome::Landed => None,
            Outcome::Refused(why) | Outcome::Unknown(why) => Some(why),
        }
    }
}

/// What sched-ext looks like to the KERNEL right now — read from sysfs, never
/// inferred from what a command said it did.
///
/// This is the trichotomy `rime game status` was missing. `scxctl` exiting 0
/// is a statement about `scxctl`; whether a BPF scheduler is attached is a
/// statement about the kernel, and only the second one is the thing Gaming
/// Mode claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScxState {
    /// A BPF scheduler is attached. `ops` is the `struct_ops` name the kernel
    /// publishes at `root/ops`, when it publishes one.
    ///
    /// **`ops` is not the scheduler's command name.** The kernel prints the
    /// struct_ops name, which drops the `scx_` prefix: `scx_lavd` attaches as
    /// `lavd`, `scx_rusty` as `rusty`. Anything comparing the two has to
    /// account for that or it will report a working scheduler as a failure.
    Enabled { ops: Option<String> },
    /// sched_ext exists in this kernel and nothing is attached.
    Disabled,
    /// Mid-transition (`enabling`, `disabling`) or a word this code does not
    /// know. Carries the string verbatim rather than rounding it to one of the
    /// two states it is not.
    InFlux(String),
    /// `CONFIG_SCHED_CLASS_EXT` is not in this kernel. A definite "no
    /// scheduler, and none is possible" — not an unknown.
    Unsupported,
    /// `/sys/kernel/sched_ext` is there and the read failed. NOT absence: a
    /// refused read is its own answer, and this program has mistaken one for
    /// the other about fourteen times.
    Unreadable(String),
}

impl ScxState {
    /// One word for a status surface: `loaded`, `not loaded` or `unknown`.
    pub fn verdict(&self) -> &'static str {
        match self {
            ScxState::Enabled { .. } => "loaded",
            ScxState::Disabled | ScxState::Unsupported => "not loaded",
            ScxState::InFlux(_) | ScxState::Unreadable(_) => "unknown",
        }
    }

    /// A sentence saying what was actually read, and from where.
    pub fn describe(&self) -> String {
        match self {
            ScxState::Enabled { ops: Some(o) } => {
                format!("sched_ext/state is enabled, root/ops reads '{o}'")
            }
            ScxState::Enabled { ops: None } => {
                "sched_ext/state is enabled; this kernel publishes no root/ops, \
                 so WHICH scheduler is attached cannot be read here"
                    .to_string()
            }
            ScxState::Disabled => "sched_ext/state is disabled — nothing is attached".to_string(),
            ScxState::InFlux(s) => {
                format!("sched_ext/state reads '{s}' — mid-transition, so this is not an answer yet")
            }
            ScxState::Unsupported => {
                "this kernel has no sched_ext (CONFIG_SCHED_CLASS_EXT) — no scheduler can attach"
                    .to_string()
            }
            ScxState::Unreadable(why) => {
                format!("sched_ext is present and could not be read: {why}")
            }
        }
    }
}

/// Read [`ScxState`] out of a sysfs root (`/sys`, or a fixture).
///
/// Rooted rather than hardcoded so all five answers are reachable headlessly.
/// The old code hardcoded `/sys/kernel/sched_ext` even on a writer built with
/// an explicit `sys_root`, which meant the only machine that could exercise
/// this path was the one running the tests.
pub fn read_scx_state(sys_root: &Path) -> ScxState {
    let base = sys_root.join("kernel/sched_ext");
    if !base.exists() {
        return ScxState::Unsupported;
    }
    let state_path = base.join("state");
    let raw = match std::fs::read_to_string(&state_path) {
        Ok(s) => s.trim().to_ascii_lowercase(),
        // `state` is 0444 on every kernel that ships sched_ext, so ENOENT here
        // is a genuinely odd shape — but it is still not the same fact as a
        // refused read, and it does not get to borrow its reason.
        Err(e) => {
            return ScxState::Unreadable(format!("{}: {e}", state_path.display()));
        }
    };
    match raw.as_str() {
        "enabled" => ScxState::Enabled {
            ops: std::fs::read_to_string(base.join("root/ops"))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        },
        "disabled" => ScxState::Disabled,
        other => ScxState::InFlux(other.to_string()),
    }
}

/// Whether `ops` (the kernel's struct_ops name) plausibly names `sched` (the
/// scheduler's command name).
///
/// `scx_lavd` attaches as `lavd`. Comparing the two verbatim reports a working
/// scheduler as a failure, which is this unit's own defect inverted, so the
/// comparison strips the prefix from either side and is deliberately a SOFT
/// check: a mismatch is reported, never treated as "not loaded".
///
/// The Rust schedulers also append a build tag: katana's kernel reported
/// `lavd_1.1.3_x86_64_unknown_linux_gnu` for scx-scheds 1.1.3, and rimed
/// logged "not the scheduler that was asked for" on every entry. A suffix
/// counts only when it starts `_<digit>`, so `lavd` never matches some other
/// scheduler whose name merely begins with it.
pub fn scx_ops_matches(ops: &str, sched: &str) -> bool {
    let strip = |s: &str| s.trim().trim_start_matches("scx_").to_ascii_lowercase();
    let (ops, sched) = (strip(ops), strip(sched));
    if ops.is_empty() || sched.is_empty() {
        return false;
    }
    match ops.strip_prefix(&sched) {
        Some("") => true,
        Some(tag) => tag
            .strip_prefix('_')
            .and_then(|v| v.chars().next())
            .is_some_and(|c| c.is_ascii_digit()),
        None => false,
    }
}

/// Turns intended [`Action`]s into effects.
pub trait SysWriter: Send + Sync {
    /// Apply one action, reporting whether it actually landed.
    ///
    /// `Err` is reserved for a *hard* failure that should stop a plan. A knob
    /// the hardware refuses is `Ok(Outcome::Refused)`, not an error.
    fn apply(&self, action: &Action) -> Result<Outcome>;

    /// Apply a whole plan in order, stopping on the first hard error.
    ///
    /// Deliberately discards the per-action outcome: a caller that needs to
    /// know what landed has to look at each action, because "some of this plan
    /// landed" is not a fact anything can act on.
    fn apply_all(&self, actions: &[Action]) -> Result<()> {
        for a in actions {
            self.apply(a)?;
        }
        Ok(())
    }

    /// Whether this writer will actually mutate hardware. `false` for dry-run
    /// and for the mock.
    fn is_live(&self) -> bool {
        false
    }

    /// What the kernel says about sched-ext, read independently of whatever
    /// the last `scxctl` call claimed.
    ///
    /// On the trait rather than on the daemon so that (a) the daemon needs no
    /// sysfs knowledge of its own and (b) a test can present all five answers
    /// through [`MockWriter`]. The default is deliberately
    /// [`ScxState::Unreadable`] and NOT `Disabled`: a writer that touches no
    /// machine cannot answer, and answering "nothing is attached" would be
    /// precisely the guess this exists to delete.
    fn scx_state(&self) -> ScxState {
        ScxState::Unreadable("this writer cannot read sched-ext".into())
    }
}

/// Writes real sysfs and runs `nvidia-smi`. When `dry_run` is set, it logs the
/// intended effect and does nothing — the same switch `RIMED_DRY_RUN=1` flips.
///
/// Every write is capability-checked first: absent attributes are skipped, and
/// values the running kernel does not advertise are substituted from a ladder
/// of near-equivalents (see [`governor_ladder`], [`epp_ladder`],
/// [`platform_profile_ladder`]) rather than pushed at a driver that will refuse
/// them.
pub struct RealWriter {
    dry_run: bool,
    sys_root: PathBuf,
    /// Whether actions that run a HOST COMMAND may actually run it.
    ///
    /// Off by default, and that default is the whole point. Most actions are
    /// sysfs writes, which `sys_root` redirects into a fixture — so a test can
    /// use a live writer safely. Two actions are not writes at all:
    /// `ScxSwitch`/`ScxStop` shell out to `scxctl`, and the NVIDIA clock locks
    /// shell out to `nvidia-smi`. No fixture root can redirect a process
    /// spawn, so a test applying those reaches the real machine.
    ///
    /// It did. `scxctl` is a D-Bus client for `scx_loader`, whose polkit action
    /// is not passwordless, so running the game-mode tests raised a burst of
    /// "Authentication is required to start, stop, or switch sched-ext
    /// schedulers" prompts on the developer's desktop — and, once
    /// authenticated, would have swapped the scheduler of the machine running
    /// the tests.
    ///
    /// So the daemon opts in explicitly ([`RealWriter::for_daemon`]) and
    /// everything else, tests included, gets a writer that logs and skips.
    host_commands: bool,
    /// An explicit `scxctl` to run instead of searching the standard
    /// locations. `None` in production; a test's recording stub otherwise.
    scxctl_bin: Option<PathBuf>,
    /// How long to wait for a started scheduler to actually attach before
    /// giving up and saying so.
    ///
    /// Attaching a BPF scheduler is asynchronous — `scx_loader` spawns the
    /// scheduler and returns, and `state` does not flip to `enabled` until the
    /// program has loaded — so reading `state` the instant `scxctl` exits
    /// would report a working scheduler as a failure. Injected rather than
    /// hardcoded so the tests for the refusal paths do not each burn this
    /// budget waiting for a state that is never coming.
    scx_settle: std::time::Duration,
}

/// How long the daemon waits for a started sched-ext scheduler to attach.
///
/// Two seconds, and the number is REASONED rather than measured: BPF load plus
/// attach is expected to be in the hundreds of milliseconds, and nothing here
/// has ever loaded a scheduler on hardware to time it. Game entry is a rare,
/// user-initiated operation on a multi-thread tokio runtime, so a bounded
/// stall is cheaper than the alternative — which is reporting a scheduler as
/// loaded because nobody waited to look.
///
/// `docs/gaming-and-sessions.md` §6.8 says what to record if this turns out to
/// be too short (a status of `unknown` over a `state` of `enabling`), and says
/// to record the number rather than re-run until it passes. Writing "measured"
/// here would be this unit's own defect, one file over.
const SCX_SETTLE: std::time::Duration = std::time::Duration::from_secs(2);

impl RealWriter {
    /// A writer rooted at real `/sys` that will NOT run host commands.
    ///
    /// This is the constructor for anything that is not the daemon.
    pub fn new(dry_run: bool) -> RealWriter {
        RealWriter {
            dry_run,
            sys_root: PathBuf::from("/sys"),
            host_commands: false,
            scxctl_bin: None,
            scx_settle: SCX_SETTLE,
        }
    }

    /// The daemon's writer: real `/sys`, and permitted to run `scxctl` and
    /// `nvidia-smi`.
    ///
    /// Separate from [`RealWriter::new`] so that opting into host commands is
    /// one visible call in one place, rather than the default that every
    /// caller silently inherits.
    pub fn for_daemon(dry_run: bool) -> RealWriter {
        RealWriter {
            dry_run,
            sys_root: PathBuf::from("/sys"),
            host_commands: true,
            scxctl_bin: None,
            scx_settle: SCX_SETTLE,
        }
    }

    /// A writer rooted at an explicit sysfs path (for a sandbox/fixture). Still
    /// gated by `dry_run`, and never runs host commands.
    pub fn with_root(dry_run: bool, sys_root: impl Into<PathBuf>) -> RealWriter {
        RealWriter {
            dry_run,
            sys_root: sys_root.into(),
            host_commands: false,
            scxctl_bin: None,
            scx_settle: SCX_SETTLE,
        }
    }

    /// A fixture-rooted writer that MAY run host commands — but only the
    /// `scxctl` handed to it, and only against the sysfs tree handed to it.
    ///
    /// `#[cfg(test)]` on purpose. The whole guard above exists because a live
    /// writer in a test reached the developer's real scheduler through a
    /// process spawn no fixture root can redirect; a production constructor
    /// that re-opens that door would undo it. Tests for the sched-ext paths
    /// live in this module's own `mod tests` so they can reach this.
    #[cfg(test)]
    fn for_scx_test(
        sys_root: impl Into<PathBuf>,
        scxctl_bin: impl Into<PathBuf>,
        scx_settle: std::time::Duration,
    ) -> RealWriter {
        RealWriter {
            dry_run: false,
            sys_root: sys_root.into(),
            host_commands: true,
            scxctl_bin: Some(scxctl_bin.into()),
            scx_settle,
        }
    }

    /// Whether this writer may run host commands.
    pub fn runs_host_commands(&self) -> bool {
        self.host_commands
    }

    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    /// Write a value to an absolute path, treating both a missing attribute and
    /// a rejected write as a *skip* rather than an error. M6 touches knobs the
    /// kernel routinely refuses (kernel-managed IRQ affinity, cpuset attributes
    /// on a delegated cgroup); a refusal must never abort the rest of a plan —
    /// least of all a restore plan.
    ///
    /// Returns [`Outcome::Landed`] when the value was actually written, and
    /// [`Outcome::Refused`] carrying the reason when it was not.
    fn write_tolerant(&self, path: &Path, value: &str, what: &str) -> Outcome {
        if !path.exists() {
            eprintln!("rimed: skip (absent) {} <- {value}", path.display());
            return Outcome::Refused(format!("{what}: attribute absent"));
        }
        if self.dry_run {
            eprintln!("rimed: [dry-run] {what}: {} <- {value}", path.display());
            // Report success so callers that ladder down through fallbacks
            // (the fan restore) show what they *would* have done, not every
            // rung of a ladder no real write ever descended.
            return Outcome::Landed;
        }
        match std::fs::write(path, value) {
            Ok(()) => Outcome::Landed,
            Err(e) => {
                eprintln!("rimed: skip ({what} rejected) {} <- {value}: {e}", path.display());
                Outcome::Refused(format!("{what}: {e}"))
            }
        }
    }

    /// Like [`RealWriter::write_tolerant`] but without the existence check.
    ///
    /// cgroup-v2 attributes (`cpuset.cpus`, `cgroup.procs`, ...) are
    /// materialised by the kernel the moment the directory is created, so
    /// "absent" is not a meaningful state to test for there — and on a plain
    /// filesystem (a test fixture) the write simply creates the file, which is
    /// the behaviour the kernel presents anyway.
    fn write_forced(&self, path: &Path, value: &str, what: &str) -> Outcome {
        if self.dry_run {
            eprintln!("rimed: [dry-run] {what}: {} <- {value}", path.display());
            // Report success so callers that ladder down through fallbacks
            // (the fan restore) show what they *would* have done, not every
            // rung of a ladder no real write ever descended.
            return Outcome::Landed;
        }
        match std::fs::write(path, value) {
            Ok(()) => Outcome::Landed,
            Err(e) => {
                eprintln!("rimed: skip ({what} rejected) {} <- {value}: {e}", path.display());
                Outcome::Refused(format!("{what}: {e}"))
            }
        }
    }

    /// Write a value to a sysfs attribute if it exists. A missing attribute is
    /// not an error (the profile expresses full intent; hardware may not have
    /// every knob), and neither is a driver that rejects the write.
    ///
    /// A rejection used to be fatal, which made the whole tier plan abort
    /// part-applied on perfectly ordinary hardware — `intel_pstate` in active
    /// mode refuses an `energy_performance_preference` write while the
    /// `performance` governor is selected, for instance. Tolerating it is what
    /// lets one plan run everywhere.
    fn write_if_present(&self, path: &Path, value: &str) -> Outcome {
        self.write_tolerant(path, value, "sysfs")
    }

    /// Every cpufreq policy directory under the sysfs root.
    ///
    /// Prefers the per-policy directories (`cpufreq/policy*`), which every
    /// modern driver registers, and falls back to the per-CPU `cpuN/cpufreq`
    /// links that older kernels and some ARM `cpufreq-dt` setups present
    /// instead. A machine with no cpufreq at all (a VM with no scaling driver)
    /// simply gets an empty list and a logged skip.
    fn cpufreq_policies(&self) -> Vec<PathBuf> {
        let base = self.sys_root.join("devices/system/cpu/cpufreq");
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&base) {
            for e in entries.flatten() {
                let p = e.path();
                if p.file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.starts_with("policy"))
                    .unwrap_or(false)
                {
                    out.push(p);
                }
            }
        }
        if out.is_empty() {
            for cpu in crate::topology::online_cpus(&self.sys_root) {
                let p = self
                    .sys_root
                    .join(format!("devices/system/cpu/cpu{cpu}/cpufreq"));
                if p.is_dir() {
                    out.push(p);
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Write a per-policy attribute, choosing the closest value the policy says
    /// it accepts.
    ///
    /// `choices_attr` names the sibling attribute that lists the legal values
    /// (`scaling_available_governors`,
    /// `energy_performance_available_preferences`). When it is absent the value
    /// is attempted as-is; when it is present the ladder is walked and the
    /// first advertised candidate wins. This is the whole reason a
    /// `performance`/`powersave` table works on `acpi-cpufreq`, `intel_pstate`,
    /// `amd-pstate` and ARM `cpufreq-dt` without per-driver special cases.
    ///
    /// Fans out across every policy, so the outcome is any-landed per
    /// [`Outcome`]'s documented rule: one policy accepting the value is an
    /// effect on the machine.
    fn write_policy_attr(
        &self,
        attr: &str,
        choices_attr: &str,
        value: &str,
        ladder: &[&str],
    ) -> Outcome {
        let policies = self.cpufreq_policies();
        if policies.is_empty() {
            eprintln!("rimed: no cpufreq policies found; skip {attr} <- {value}");
            return Outcome::Refused(format!("{attr}: no cpufreq policies on this machine"));
        }
        // Every policy is still visited even once one has landed: this loop is
        // what applies the value, and short-circuiting it would leave the other
        // policies untouched. Only the *report* is an aggregate.
        let mut landed = false;
        let mut last_refusal = None;
        for p in policies {
            let target = p.join(attr);
            if !target.exists() {
                eprintln!("rimed: skip (absent) {} <- {value}", target.display());
                last_refusal = Some(format!("{attr}: attribute absent"));
                continue;
            }
            let choices = read_tokens(&p.join(choices_attr));
            let chosen = match &choices {
                // No list published: the driver takes whatever it takes.
                None => Some(value.to_string()),
                Some(list) => pick_supported(value, ladder, list),
            };
            match chosen {
                Some(v) => {
                    if v != value {
                        eprintln!(
                            "rimed: {} does not offer '{value}'; using '{v}' instead",
                            target.display()
                        );
                    }
                    match self.write_tolerant(&target, &v, attr) {
                        Outcome::Landed => landed = true,
                        // A sysfs write never produces `Unknown` — it either
                        // returned an error or it did not. Matched explicitly
                        // rather than with `_` so that if some future writer
                        // path DOES start reading its work back, the compiler
                        // makes whoever added it decide what a fan-out of
                        // unconfirmed writes means instead of silently
                        // counting it as a refusal.
                        Outcome::Refused(why) | Outcome::Unknown(why) => last_refusal = Some(why),
                    }
                }
                None => {
                    eprintln!(
                        "rimed: skip ({attr} offers none of {value}/{}) {}",
                        ladder.join("/"),
                        target.display()
                    );
                    last_refusal = Some(format!(
                        "{attr}: offers none of {value}/{}",
                        ladder.join("/")
                    ));
                }
            }
        }
        if landed {
            return Outcome::Landed;
        }
        Outcome::Refused(last_refusal.unwrap_or_else(|| format!("{attr}: nothing accepted it")))
    }

    /// Write the ACPI platform profile, mapped onto what the firmware offers.
    /// `platform_profile_choices` is wildly vendor-specific — `low-power
    /// balanced performance` on one machine, `quiet balanced balanced-
    /// performance performance` on the next, `cool quiet performance` on an
    /// older ThinkPad — so the requested value is matched through a ladder of
    /// synonyms rather than written blind.
    fn write_platform_profile(&self, value: &str) -> Outcome {
        let path = self.sys_root.join("firmware/acpi/platform_profile");
        if !path.exists() {
            eprintln!("rimed: skip (absent) {} <- {value}", path.display());
            return Outcome::Refused("platform_profile: attribute absent".into());
        }
        let choices = read_tokens(&self.sys_root.join("firmware/acpi/platform_profile_choices"));
        let chosen = match &choices {
            None => Some(value.to_string()),
            Some(list) => pick_supported(value, platform_profile_ladder(value), list),
        };
        match chosen {
            Some(v) => {
                if v != value {
                    eprintln!("rimed: platform_profile has no '{value}'; using '{v}' instead");
                }
                self.write_tolerant(&path, &v, "platform_profile")
            }
            None => {
                eprintln!(
                    "rimed: skip (platform_profile offers none of the '{value}' synonyms) {}",
                    path.display()
                );
                Outcome::Refused(format!(
                    "platform_profile: firmware offers no '{value}' synonym"
                ))
            }
        }
    }

    /// Run `nvidia-smi` with `args`. A missing binary or a non-zero exit is a
    /// logged skip, never an error: a machine with no NVIDIA GPU must still be
    /// able to enter game mode.
    /// The `scxctl` to run, or `None` when there is none to run.
    ///
    /// scx-tools installs into `/usr/sbin`, which is not always on PATH for a
    /// service; both are tried rather than depending on the unit's
    /// environment. A test hands in its own.
    fn scxctl_path(&self) -> Option<PathBuf> {
        if let Some(p) = &self.scxctl_bin {
            return p.exists().then(|| p.clone());
        }
        ["/usr/sbin/scxctl", "/usr/bin/scxctl"]
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.exists())
    }

    /// The guards every `scxctl` call shares. `Err` here is the refusal to
    /// report; `Ok` carries the binary to run.
    fn scx_preflight(&self, what: &str) -> std::result::Result<PathBuf, Outcome> {
        // Checked BEFORE anything else, because this is the guard that keeps a
        // test off the host's scheduler. `sys_root` cannot redirect a process
        // spawn, so `dry_run` is not enough on its own.
        if !self.host_commands {
            eprintln!("rimed: skip (host commands not enabled for this writer) scxctl {what}");
            return Err(Outcome::Refused(
                "scxctl: host commands not enabled for this writer".into(),
            ));
        }
        // sched_ext has to exist in the kernel. On a kernel without it scxctl
        // would fail confusingly, so say the useful thing instead.
        if matches!(read_scx_state(&self.sys_root), ScxState::Unsupported) {
            eprintln!("rimed: skip (kernel has no sched_ext support) scxctl {what}");
            return Err(Outcome::Refused(
                "scxctl: this kernel has no sched_ext support (CONFIG_SCHED_CLASS_EXT)".into(),
            ));
        }
        self.scxctl_path().ok_or_else(|| {
            eprintln!("rimed: skip (scxctl absent) scxctl {what}");
            Outcome::Refused("scxctl: not installed".into())
        })
    }

    /// Run `scxctl <args>` once, returning (success, stderr).
    fn scxctl_once(&self, bin: &Path, args: &[&str]) -> std::result::Result<(bool, String), String> {
        match std::process::Command::new(bin).args(args).output() {
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                if !out.status.success() {
                    eprintln!(
                        "rimed: scxctl {} failed ({}): {stderr}",
                        args.join(" "),
                        out.status
                    );
                }
                Ok((out.status.success(), stderr))
            }
            Err(e) => {
                eprintln!("rimed: cannot run scxctl {}: {e}", args.join(" "));
                Err(format!("scxctl: {e}"))
            }
        }
    }

    /// Poll `state` until it reaches `want`, or the settle budget runs out.
    /// Returns whatever it last read, so the caller reports an observation and
    /// never an assumption.
    fn scx_settle_until(&self, want: fn(&ScxState) -> bool) -> ScxState {
        let deadline = std::time::Instant::now() + self.scx_settle;
        loop {
            let st = read_scx_state(&self.sys_root);
            if want(&st) || std::time::Instant::now() >= deadline {
                return st;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    /// Load `sched` as the running sched-ext scheduler, and then READ THE
    /// KERNEL to find out whether that happened.
    ///
    /// ── The verb ────────────────────────────────────────────────────────────
    ///
    /// `scxctl` has two verbs for this and they are not interchangeable:
    /// `start` attaches a scheduler when none is running, `switch` replaces one
    /// that is. Each refuses in the other's state, in as many words:
    ///
    /// ```text
    /// error: no scx scheduler running, use 'start' instead of 'switch'
    /// error: scx scheduler already running, use 'switch' instead of 'start'
    /// ```
    ///
    /// Rime loads nothing at boot, so the first entry into Gaming Mode always
    /// finds none — and this code hardcoded `switch`, so Gaming Mode had NEVER
    /// once loaded a scheduler, on any boot, since the feature landed. It was
    /// found on hardware months later only because somebody read the journal.
    ///
    /// So the verb is chosen from the state the kernel reports, and a single
    /// retry on the other verb covers the race (and the case where the state
    /// could not be read at all) rather than guessing twice.
    ///
    /// ── The read-back ───────────────────────────────────────────────────────
    ///
    /// `scxctl` exiting 0 is a statement about `scxctl`. It is not the claim
    /// Gaming Mode makes, which is that a scheduler is attached — so after a
    /// successful call this waits, bounded, for `sched_ext/state` to say
    /// `enabled` and reports what it actually saw. A command that succeeds and
    /// changes nothing comes back [`Outcome::Refused`]; a machine that cannot
    /// be read back comes back [`Outcome::Unknown`], which is neither.
    ///
    /// Deliberately never fatal, and for the same reason as nvidia-smi: a
    /// scheduler swap is a performance nicety, and a machine without sched-ext
    /// support, without scxctl, or whose scheduler refuses to load must still
    /// enter game mode with its cpuset, IRQ and clock work applied.
    fn scx_load(&self, sched: &str) -> Outcome {
        if self.dry_run {
            eprintln!("rimed: [dry-run] scxctl start -s {sched}");
            return Outcome::Landed;
        }
        let bin = match self.scx_preflight(&format!("start -s {sched}")) {
            Ok(b) => b,
            Err(o) => return o,
        };

        let before = read_scx_state(&self.sys_root);
        let first = match &before {
            ScxState::Enabled { .. } => "switch",
            // `disabled`, mid-transition, or unreadable. `start` is the right
            // opening guess for all three: it is correct for the only state
            // Rime machines are ever in at this point, and the retry below
            // covers being wrong without a second guess.
            _ => "start",
        };
        let other = if first == "start" { "switch" } else { "start" };

        let (mut ok, mut stderr) = match self.scxctl_once(&bin, &[first, "-s", sched]) {
            Ok(r) => r,
            Err(e) => return Outcome::Refused(e),
        };
        let mut verb = first;
        // scx_loader's own error names the verb it wanted. Taking it at its
        // word is not a guess — and one retry is the cap, so a loader that
        // ping-pongs cannot spin here.
        if !ok && (stderr.contains("already running") || stderr.contains("no scx scheduler running"))
        {
            eprintln!("rimed: scxctl asked for '{other}' instead of '{first}' — retrying once");
            match self.scxctl_once(&bin, &[other, "-s", sched]) {
                Ok(r) => {
                    ok = r.0;
                    stderr = r.1;
                    verb = other;
                }
                Err(e) => return Outcome::Refused(e),
            }
        }
        if !ok {
            return Outcome::Refused(format!("scxctl {verb} -s {sched}: {stderr}"));
        }

        // It said yes. Now ask the kernel.
        match self.scx_settle_until(|s| matches!(s, ScxState::Enabled { .. })) {
            ScxState::Enabled { ops } => {
                match &ops {
                    Some(o) if !scx_ops_matches(o, sched) => eprintln!(
                        "rimed: scx: asked for {sched}, kernel reports root/ops '{o}' \
                         — attached, but not the scheduler that was asked for"
                    ),
                    _ => eprintln!("rimed: scx: {sched} attached (scxctl {verb})"),
                }
                Outcome::Landed
            }
            // The exact defect this function exists for, in its general form:
            // the command reported success and the machine did not move.
            ScxState::Disabled => Outcome::Refused(format!(
                "scxctl {verb} -s {sched} reported success, but sched_ext/state is still \
                 disabled after {:?} — no scheduler attached",
                self.scx_settle
            )),
            // Neither a yes nor a no. Saying either would be the lie.
            other => Outcome::Unknown(format!(
                "scxctl {verb} -s {sched} reported success and the result could not be \
                 confirmed: {}",
                other.describe()
            )),
        }
    }

    /// Hand scheduling back to the kernel's own class, and read back that it
    /// happened.
    ///
    /// The mirror of [`RealWriter::scx_load`], and newly load-bearing: until
    /// the verb above was fixed, nothing was ever attached, so this never had
    /// anything to stop.
    fn scx_stop(&self) -> Outcome {
        if self.dry_run {
            eprintln!("rimed: [dry-run] scxctl stop");
            return Outcome::Landed;
        }
        let bin = match self.scx_preflight("stop") {
            Ok(b) => b,
            Err(o) => return o,
        };
        // Nothing attached is not a failure to stop it — it is the state the
        // stop was for. Saying "refused" here would make every ordinary exit
        // on a machine whose scheduler never loaded look like a fault.
        if matches!(read_scx_state(&self.sys_root), ScxState::Disabled) {
            return Outcome::Landed;
        }
        let (ok, stderr) = match self.scxctl_once(&bin, &["stop"]) {
            Ok(r) => r,
            Err(e) => return Outcome::Refused(e),
        };
        if !ok {
            return Outcome::Refused(format!("scxctl stop: {stderr}"));
        }
        match self.scx_settle_until(|s| matches!(s, ScxState::Disabled)) {
            ScxState::Disabled => Outcome::Landed,
            ScxState::Enabled { ops } => Outcome::Refused(format!(
                "scxctl stop reported success, but a scheduler is still attached ({}) \
                 after {:?}",
                ops.unwrap_or_else(|| "name not published".into()),
                self.scx_settle
            )),
            other => Outcome::Unknown(format!(
                "scxctl stop reported success and the result could not be confirmed: {}",
                other.describe()
            )),
        }
    }

    fn run_nvidia_smi(&self, args: &[String]) -> Result<Outcome> {
        if self.dry_run {
            eprintln!("rimed: [dry-run] nvidia-smi {}", args.join(" "));
            return Ok(Outcome::Landed);
        }
        // Same guard as scxctl: a process spawn is not redirected by
        // `sys_root`, so a test with a live writer would lock the clocks of the
        // GPU it is running on.
        if !self.host_commands {
            eprintln!(
                "rimed: skip (host commands not enabled for this writer) nvidia-smi {}",
                args.join(" ")
            );
            return Ok(Outcome::Refused(
                "nvidia-smi: host commands not enabled for this writer".into(),
            ));
        }
        if !crate::gpu::nvidia_smi_available() {
            eprintln!("rimed: skip (nvidia-smi absent) nvidia-smi {}", args.join(" "));
            return Ok(Outcome::Refused("nvidia-smi: not installed".into()));
        }
        match std::process::Command::new("nvidia-smi").args(args).output() {
            Ok(out) if out.status.success() => Ok(Outcome::Landed),
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                eprintln!(
                    "rimed: nvidia-smi {} failed ({}): {stderr}",
                    args.join(" "),
                    out.status,
                );
                Ok(Outcome::Refused(format!(
                    "nvidia-smi: {} — {stderr}",
                    out.status
                )))
            }
            Err(e) => {
                eprintln!("rimed: nvidia-smi {} could not run: {e}", args.join(" "));
                Ok(Outcome::Refused(format!("nvidia-smi: {e}")))
            }
        }
    }

    /// Hand a fan back to firmware control. The ladder is the safety guarantee:
    /// prior `pwm*_enable` -> `2` (firmware automatic) -> `0` (no control, which
    /// the hwmon ABI defines as *full speed*), and if a manual mode is all the
    /// hardware offers, the duty cycle is driven to 255 rather than left low.
    /// No path through this function can leave a fan stopped.
    ///
    /// The outcome is any-landed, per [`Outcome`]: whichever rung of the ladder
    /// sticks, control was handed back.
    fn fan_safe_restore(
        &self,
        enable_path: Option<&str>,
        pwm_path: Option<&str>,
        prior_enable: Option<u8>,
        prior_pwm: Option<u8>,
    ) -> Result<Outcome> {
        let Some(enable) = enable_path else {
            // No enable attribute: the only lever is the duty cycle. Restore the
            // recorded value, or go to full speed if we never recorded one.
            let Some(pwm) = pwm_path else {
                return Ok(Outcome::Refused(
                    "fan restore: this fan exposes neither pwm_enable nor pwm".into(),
                ));
            };
            let v = prior_pwm.unwrap_or(255);
            return Ok(self.write_tolerant(Path::new(pwm), &v.to_string(), "fan restore pwm"));
        };
        let enable = Path::new(enable);

        // 1. The value the fan had before we touched it (usually 2 = firmware).
        if let Some(prior) = prior_enable {
            if self
                .write_tolerant(enable, &prior.to_string(), "fan restore enable")
                .landed()
            {
                // Manual mode was the *prior* state; put its duty cycle back too,
                // and never below full speed if we do not know what it was.
                if prior == 1 {
                    if let Some(pwm) = pwm_path {
                        let v = prior_pwm.unwrap_or(255);
                        self.write_tolerant(Path::new(pwm), &v.to_string(), "fan restore pwm");
                    }
                }
                return Ok(Outcome::Landed);
            }
        }
        // 2. Firmware automatic.
        if self.write_tolerant(enable, "2", "fan restore auto").landed() {
            return Ok(Outcome::Landed);
        }
        // 3. Last resort: full speed. Push the duty cycle up *first* so that a
        //    driver treating `0` as "manual, keep current pwm" still ends up
        //    with the fan spinning flat out.
        let mut landed = false;
        if let Some(pwm) = pwm_path {
            landed |= self
                .write_tolerant(Path::new(pwm), "255", "fan restore full-speed pwm")
                .landed();
        }
        let last = self.write_tolerant(enable, "0", "fan restore full-speed");
        if landed || last.landed() {
            return Ok(Outcome::Landed);
        }
        Ok(last)
    }

    /// Create a cgroup-v2 directory (if needed) and apply a cpuset to it.
    /// Enabling the `cpuset` controller in the parent's `subtree_control` is
    /// best-effort: on a systemd host the root cgroup may already delegate it.
    ///
    /// Any-landed, per [`Outcome`]: the cpuset that confines the game is
    /// `cpuset.cpus`, so a run where `cpuset.mems` alone stuck still changed
    /// the machine. Callers wanting the two separately have to plan them
    /// separately.
    fn cgroup_ensure(&self, path: &str, cpus: &str, mems: &str) -> Result<Outcome> {
        let dir = Path::new(path);
        if self.dry_run {
            eprintln!("rimed: [dry-run] cgroup {path}: cpuset.cpus={cpus} cpuset.mems={mems}");
            return Ok(Outcome::Landed);
        }
        if let Some(parent) = dir.parent() {
            let sc = parent.join("cgroup.subtree_control");
            if sc.exists() {
                // Only meaningful if cpuset is not already enabled; a duplicate
                // write is harmless and a rejection is tolerated.
                self.write_tolerant(&sc, "+cpuset", "cgroup subtree_control");
            }
        }
        if !dir.exists() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                eprintln!("rimed: cgroup {path}: create failed: {e}");
                return Ok(Outcome::Refused(format!("cgroup {path}: create failed: {e}")));
            }
        }
        let mems = self.write_forced(&dir.join("cpuset.mems"), mems, "cpuset.mems");
        let cpus = self.write_forced(&dir.join("cpuset.cpus"), cpus, "cpuset.cpus");
        if mems.landed() || cpus.landed() {
            return Ok(Outcome::Landed);
        }
        Ok(cpus)
    }

    /// Remove a cgroup directory. `rmdir` is all the kernel needs (its
    /// auto-populated attribute files do not block it); the `remove_dir_all`
    /// fallback exists for plain filesystems — test fixtures — and is only
    /// attempted when the directory holds no sub-directories, so a cgroup with
    /// children is never blown away.
    fn cgroup_remove(&self, path: &str) -> Result<Outcome> {
        let dir = Path::new(path);
        if self.dry_run {
            eprintln!("rimed: [dry-run] cgroup {path}: remove");
            return Ok(Outcome::Landed);
        }
        if !dir.exists() {
            // Already gone is the state this action asks for, so it landed.
            // Exit is idempotent by design and must not report a refusal for
            // running twice.
            return Ok(Outcome::Landed);
        }
        match std::fs::remove_dir(dir) {
            Ok(()) => return Ok(Outcome::Landed),
            Err(e) => {
                let has_subdirs = std::fs::read_dir(dir)
                    .map(|it| it.flatten().any(|e| e.path().is_dir()))
                    .unwrap_or(true);
                if has_subdirs {
                    eprintln!("rimed: cgroup {path}: remove skipped ({e}); it has child cgroups");
                    return Ok(Outcome::Refused(format!(
                        "cgroup {path}: has child cgroups ({e})"
                    )));
                }
                if let Err(e2) = std::fs::remove_dir_all(dir) {
                    eprintln!("rimed: cgroup {path}: remove skipped: {e} / {e2}");
                    return Ok(Outcome::Refused(format!("cgroup {path}: {e} / {e2}")));
                }
            }
        }
        Ok(Outcome::Landed)
    }
}

impl SysWriter for RealWriter {
    fn apply(&self, action: &Action) -> Result<Outcome> {
        match action {
            Action::Governor(g) => Ok(self.write_policy_attr(
                "scaling_governor",
                "scaling_available_governors",
                g,
                governor_ladder(g),
            )),
            Action::Epp(e) => Ok(self.write_policy_attr(
                "energy_performance_preference",
                "energy_performance_available_preferences",
                e,
                epp_ladder(e),
            )),
            Action::PlatformProfile(p) => Ok(self.write_platform_profile(p)),
            Action::ChargeThresholds {
                start,
                stop,
                start_path,
                end_path,
            } => {
                // The stop threshold is written first AND last. First, because
                // some ECs refuse a start at or above the current stop, so a
                // window that moves up must widen before its start moves.
                // Last, because on msi-ec both attributes are ONE EC byte (the
                // start always reads stop - 10): whichever is written last
                // decides, and the stop is the one that protects the battery.
                // Written stop-then-start, a 60/80 window landed as 60/70 on
                // an MSI Katana.
                let mut last = Outcome::Refused(
                    "charge thresholds: this battery exposes neither attribute".into(),
                );
                let mut landed = false;
                let stop_s = stop.to_string();
                if let Some(end_path) = end_path {
                    last = self.write_if_present(Path::new(end_path), &stop_s);
                    landed |= last.landed();
                }
                if let Some(start_path) = start_path {
                    last = self.write_if_present(Path::new(start_path), &start.to_string());
                    landed |= last.landed();
                    if let Some(end_path) = end_path {
                        last = self.write_if_present(Path::new(end_path), &stop_s);
                        landed |= last.landed();
                    }
                }
                // Say what the battery actually holds when it is not what was
                // asked for (msi-ec: 60/80 becomes 70/80).
                if landed && !self.dry_run {
                    let read = |p: &Option<String>| {
                        p.as_deref()
                            .and_then(|p| std::fs::read_to_string(p).ok())
                            .map(|v| v.trim().to_string())
                    };
                    let (got_start, got_stop) = (read(start_path), read(end_path));
                    let differs = got_stop.as_deref().is_some_and(|v| v != stop_s)
                        || got_start.as_deref().is_some_and(|v| v != start.to_string());
                    if differs {
                        eprintln!(
                            "rimed: charge thresholds: asked {start}/{stop}, the battery holds {}/{} \
                             (this driver cannot hold that window)",
                            got_start.as_deref().unwrap_or("-"),
                            got_stop.as_deref().unwrap_or("-"),
                        );
                    }
                }
                Ok(if landed { Outcome::Landed } else { last })
            }

            // ── M6 ───────────────────────────────────────────────────────────
            Action::FanPwmEnable { path, value } => Ok(self.write_tolerant(
                Path::new(path),
                &value.to_string(),
                "pwm_enable",
            )),
            Action::FanPwm { path, value } => {
                Ok(self.write_tolerant(Path::new(path), &value.to_string(), "pwm"))
            }
            Action::FanVendorAttr { path, value, what } => {
                Ok(self.write_tolerant(Path::new(path), value, what))
            }
            // Tolerant like the fan attributes, and for the same reason: a
            // driver that refuses a GPU knob (an amdgpu built without the
            // manual DPM feature mask, an i915 that clamps a floor differently
            // than its own published limits) must not abort the rest of a game
            // plan, least of all the plan that puts the machine back.
            Action::GpuSysfsAttr { path, value, what } => {
                Ok(self.write_tolerant(Path::new(path), value, what))
            }
            Action::FanSafeRestore {
                enable_path,
                pwm_path,
                prior_enable,
                prior_pwm,
            } => self.fan_safe_restore(
                enable_path.as_deref(),
                pwm_path.as_deref(),
                *prior_enable,
                *prior_pwm,
            ),
            Action::NvidiaPersistence { gpu, enabled } => self.run_nvidia_smi(&[
                "-i".into(),
                gpu.to_string(),
                "-pm".into(),
                u8::from(*enabled).to_string(),
            ]),
            Action::NvidiaLockGraphics {
                gpu,
                min_mhz,
                max_mhz,
            } => self.run_nvidia_smi(&[
                "-i".into(),
                gpu.to_string(),
                "-lgc".into(),
                format!("{min_mhz},{max_mhz}"),
            ]),
            Action::NvidiaLockMemory {
                gpu,
                min_mhz,
                max_mhz,
            } => self.run_nvidia_smi(&[
                "-i".into(),
                gpu.to_string(),
                "-lmc".into(),
                format!("{min_mhz},{max_mhz}"),
            ]),
            Action::NvidiaResetGraphics { gpu } => {
                self.run_nvidia_smi(&["-i".into(), gpu.to_string(), "-rgc".into()])
            }
            Action::NvidiaResetMemory { gpu } => {
                self.run_nvidia_smi(&["-i".into(), gpu.to_string(), "-rmc".into()])
            }
            // The ONE action that is exactly one write, which is why the
            // game-mode report counts these and nothing else.
            Action::IrqAffinity { path, cpus } => {
                Ok(self.write_tolerant(Path::new(path), cpus, "irq affinity"))
            }
            Action::CgroupEnsure { path, cpus, mems } => self.cgroup_ensure(path, cpus, mems),
            Action::CgroupAttach { path, pid } => Ok(self.write_forced(
                &Path::new(path).join("cgroup.procs"),
                &pid.to_string(),
                "cgroup attach",
            )),
            Action::CgroupRemove { path } => self.cgroup_remove(path),
            Action::ScxSwitch { sched } => Ok(self.scx_load(sched)),
            Action::ScxStop => Ok(self.scx_stop()),
        }
    }

    fn is_live(&self) -> bool {
        !self.dry_run
    }

    fn scx_state(&self) -> ScxState {
        read_scx_state(&self.sys_root)
    }
}

// ── capability probing: what does this kernel actually accept? ───────────────

/// Read a whitespace-separated sysfs list (`scaling_available_governors` and
/// friends). `None` when the attribute does not exist — which means "the driver
/// publishes no list", not "the list is empty".
fn read_tokens(path: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(
        text.split_whitespace()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    )
}

/// The first of `value` then `ladder` that appears in `available`.
fn pick_supported(value: &str, ladder: &[&str], available: &[String]) -> Option<String> {
    let has = |c: &str| available.iter().any(|a| a.eq_ignore_ascii_case(c));
    if has(value) {
        return Some(value.to_string());
    }
    ladder
        .iter()
        .find(|c| has(c))
        .map(|c| (*c).to_string())
}

/// Fallbacks for a `scaling_governor` value.
///
/// `performance` and `powersave` are near-universal, but they are not
/// guaranteed: a kernel can be built without `CPU_FREQ_GOV_POWERSAVE`, and some
/// ARM defconfigs ship only `schedutil` plus `performance`. Substituting the
/// nearest governor in the same direction beats writing `EINVAL` at the driver.
fn governor_ladder(value: &str) -> &'static [&'static str] {
    match value.to_ascii_lowercase().as_str() {
        "performance" => &["performance", "schedutil", "ondemand"],
        "powersave" => &["powersave", "schedutil", "conservative", "ondemand"],
        "schedutil" => &["schedutil", "ondemand", "powersave"],
        "ondemand" => &["ondemand", "schedutil", "conservative"],
        "conservative" => &["conservative", "ondemand", "schedutil", "powersave"],
        _ => &["schedutil", "ondemand", "powersave"],
    }
}

/// Fallbacks for an `energy_performance_preference` value.
///
/// The four canonical strings (`performance`, `balance_performance`,
/// `balance_power`, `power`) are what `intel_pstate` and `amd-pstate` publish,
/// but a driver in a different operating mode may offer only a subset, and
/// `default` is always a safe landing spot.
fn epp_ladder(value: &str) -> &'static [&'static str] {
    match value.to_ascii_lowercase().as_str() {
        "performance" => &["performance", "balance_performance", "default"],
        "balance_performance" => &["balance_performance", "performance", "default"],
        "balance_power" => &["balance_power", "balance_performance", "default"],
        "power" => &["power", "balance_power", "default"],
        _ => &["default", "balance_performance"],
    }
}

/// Synonyms for an ACPI `platform_profile` value, ordered by how close they are
/// to the intent. The vocabulary differs per vendor: `low-power` on one
/// machine, `quiet` or `cool` on another, and `balanced-performance` sits
/// between `balanced` and `performance` on newer firmware.
fn platform_profile_ladder(value: &str) -> &'static [&'static str] {
    match value.to_ascii_lowercase().as_str() {
        "performance" => &["performance", "balanced-performance", "balanced"],
        "balanced-performance" => &["balanced-performance", "performance", "balanced"],
        "balanced" => &["balanced", "balanced-performance", "quiet", "performance"],
        "low-power" => &["low-power", "quiet", "cool", "balanced"],
        "quiet" => &["quiet", "low-power", "cool", "balanced"],
        "cool" => &["cool", "quiet", "low-power", "balanced"],
        _ => &["balanced"],
    }
}

/// Records intended actions without touching anything. The backbone of the
/// unit tests.
#[derive(Default)]
pub struct MockWriter {
    actions: Mutex<Vec<Action>>,
}

impl MockWriter {
    pub fn new() -> MockWriter {
        MockWriter::default()
    }

    /// A snapshot of every action applied so far, in order.
    pub fn recorded(&self) -> Vec<Action> {
        self.actions.lock().unwrap().clone()
    }

    /// Clear the record.
    pub fn clear(&self) {
        self.actions.lock().unwrap().clear();
    }
}

impl SysWriter for MockWriter {
    /// Always [`Outcome::Landed`]: the mock's contract is that the plan is
    /// recorded exactly as issued. A test that needs a *refusing* writer builds
    /// one — see the game-status tests in `rimed/src/game.rs`, which is the
    /// case a mock that could refuse would have made ambiguous.
    fn apply(&self, action: &Action) -> Result<Outcome> {
        self.actions.lock().unwrap().push(action.clone());
        Ok(Outcome::Landed)
    }

    // `scx_state` is left at the trait default, which is
    // `ScxState::Unreadable`. That is the truthful answer for a writer that
    // touches nothing, and it means a plan applied through the mock reports
    // sched-ext as `unknown` rather than as loaded — a recorded intention is
    // not a scheduler. A test that wants a definite state implements the trait
    // itself; `rimed/src/game.rs` does exactly that for all three.
}

#[cfg(test)]
mod host_command_tests {
    use super::*;

    // The guard these assert exists because running the game-mode tests raised
    // a burst of polkit prompts on the developer's desktop: `scxctl` is a D-Bus
    // client for `scx_loader`, whose action is not passwordless, and a test
    // applying `ScxSwitch` through a live writer invoked it for real. `sys_root`
    // could not prevent it — a process spawn has no root to redirect.

    #[test]
    fn a_writer_does_not_run_host_commands_unless_it_is_the_daemons() {
        assert!(
            !RealWriter::new(false).runs_host_commands(),
            "the default must be OFF: this is the constructor tests reach for"
        );
        assert!(!RealWriter::new(true).runs_host_commands());
        assert!(!RealWriter::with_root(false, "/tmp/fixture").runs_host_commands());
        assert!(!RealWriter::with_root(true, "/tmp/fixture").runs_host_commands());
    }

    #[test]
    fn the_daemons_writer_does_run_them() {
        // Otherwise the guard has quietly disabled game mode in production,
        // which is the failure mode of fixing this the lazy way.
        assert!(RealWriter::for_daemon(false).runs_host_commands());
        assert!(RealWriter::for_daemon(true).runs_host_commands());
    }

    #[test]
    fn dry_run_is_still_independent_of_host_commands() {
        // Two separate axes: dry-run says "plan only", host-commands says "you
        // may leave this process". A daemon in dry-run must do neither.
        let d = RealWriter::for_daemon(true);
        assert!(d.is_dry_run());
        assert!(d.runs_host_commands());
        let live = RealWriter::for_daemon(false);
        assert!(!live.is_dry_run());
    }

    #[test]
    fn scx_and_nvidia_actions_are_accepted_and_skipped_rather_than_failing() {
        // A skipped host command must not abort a plan: the rest of game mode
        // (cpuset, IRQ affinity) is the part that matters, and a restore plan
        // that aborts half way is worse than one that logs a skip.
        //
        // This runs on the test host with host commands OFF, so it is also the
        // assertion that these two actions cannot touch it.
        let w = RealWriter::new(false);
        assert!(w
            .apply(&Action::ScxSwitch {
                sched: "scx_lavd".into()
            })
            .is_ok());
        assert!(w.apply(&Action::ScxStop).is_ok());
    }
}

#[cfg(test)]
mod outcome_tests {
    use super::*;

    /// A scratch directory that cleans itself up.
    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Tmp {
            let p = std::env::temp_dir().join(format!(
                "rimed-outcome-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::remove_dir_all(&p).ok();
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    // ── the outcome of a write is reported, not assumed ─────────────────────
    //
    // These exist because `apply` used to return `Ok(())` whether or not the
    // value reached the machine, and `rime game status` then reported the
    // PLANNED number of steered interrupts as if it had measured them. On a
    // machine that refuses every affinity write — kernel-managed MSI-X queues
    // return -EIO — status said "N IRQs steered" having steered none.
    //
    // The refusal is produced by pointing the action at a DIRECTORY. `write(2)`
    // on a directory fails with EISDIR for every user including root, so this
    // asserts the same thing whether the suite runs as the developer or in a
    // root container. A `chmod 0444` fixture would not: root ignores the mode
    // bits, and the test would silently invert in CI.

    #[test]
    fn an_irq_write_that_lands_reports_that_it_landed() {
        let t = Tmp::new("irq-landed");
        let path = t.0.join("smp_affinity_list");
        std::fs::write(&path, "0-19\n").unwrap();
        let w = RealWriter::new(false);
        assert_eq!(
            w.apply(&Action::IrqAffinity {
                path: path.to_string_lossy().to_string(),
                cpus: "12-19".into(),
            })
            .unwrap(),
            Outcome::Landed
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "12-19");
    }

    #[test]
    fn an_irq_write_the_kernel_refuses_reports_the_refusal_and_the_reason() {
        let t = Tmp::new("irq-refused");
        // A directory: present, so the existence check passes, and unwritable
        // for anyone — which is exactly what a kernel-managed interrupt looks
        // like from here.
        let path = t.0.join("smp_affinity_list");
        std::fs::create_dir_all(&path).unwrap();
        let w = RealWriter::new(false);
        let outcome = w
            .apply(&Action::IrqAffinity {
                path: path.to_string_lossy().to_string(),
                cpus: "12-19".into(),
            })
            .unwrap();
        assert!(!outcome.landed(), "a refused write must not report landing");
        let Outcome::Refused(why) = outcome else {
            unreachable!("checked by the assertion above")
        };
        assert!(
            why.contains("irq affinity"),
            "the reason must name the knob: {why}"
        );
    }

    #[test]
    fn an_absent_attribute_is_a_refusal_rather_than_a_silent_success() {
        let t = Tmp::new("irq-absent");
        let w = RealWriter::new(false);
        let outcome = w
            .apply(&Action::IrqAffinity {
                path: t.0.join("no/such/smp_affinity_list").to_string_lossy().to_string(),
                cpus: "12-19".into(),
            })
            .unwrap();
        assert!(
            !outcome.landed(),
            "an attribute this machine does not have cannot have been written"
        );
    }

    #[test]
    fn a_refused_action_is_still_not_an_error() {
        // The tolerance this whole module is built on: a knob the hardware
        // refuses must never abort the rest of a plan, least of all a restore
        // plan. Reporting the refusal is a REPORTING change, not a change to
        // what aborts.
        let t = Tmp::new("irq-tolerant");
        let path = t.0.join("smp_affinity_list");
        std::fs::create_dir_all(&path).unwrap();
        let w = RealWriter::new(false);
        let plan = [
            Action::IrqAffinity {
                path: path.to_string_lossy().to_string(),
                cpus: "12-19".into(),
            },
            Action::ScxStop,
        ];
        assert!(
            w.apply_all(&plan).is_ok(),
            "a refused write must not abort the plan behind it"
        );
    }
}

// ── sched-ext: the verb, and the read-back ──────────────────────────────────
//
// The defect these close, in one sentence: Gaming Mode ran `scxctl switch`
// where the machine needed `scxctl start`, so it had NEVER loaded a scheduler
// on any boot since the feature landed — and the only surface that reported
// sched-ext said it had, because that sentence came out of the plan.
//
// Nothing here touches a real scheduler. The writer under test is rooted at a
// fixture sysfs AND handed a fake `scxctl` that writes into that fixture, so
// every one of the five states is reachable on any machine, including a
// kernel with no sched_ext at all. That matters beyond convenience: the
// production guard exists because a live writer in a test reached the
// developer's own scheduler through a process spawn, and `for_scx_test` is
// `#[cfg(test)]` so it cannot be the thing that reopens that door.
#[cfg(test)]
mod scx_tests {
    use super::*;
    use std::time::Duration;

    /// Serialises this module.
    ///
    /// These tests write a small executable and then run it. `fork` in ANOTHER
    /// thread duplicates every open fd, so a sibling test's still-open write
    /// handle to its own fake is enough to make `execve` here fail with
    /// ETXTBSY — a flake with nothing to do with what is being asserted. One
    /// lock across the module removes the overlap, and the module runs in
    /// about two tenths of a second either way.
    ///
    /// Poisoning is stepped over deliberately: one genuine failure must not
    /// turn into sixteen spurious ones that hide it.
    static SCX_LOCK: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SCX_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A fixture sysfs plus a fake `scxctl`, cleaned up on drop.
    struct Lab {
        root: PathBuf,
    }

    impl Lab {
        fn new(tag: &str) -> Lab {
            let root = std::env::temp_dir().join(format!(
                "rimed-scx-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::remove_dir_all(&root).ok();
            std::fs::create_dir_all(&root).unwrap();
            Lab { root }
        }

        fn sys(&self) -> PathBuf {
            self.root.join("sys")
        }

        fn scx_dir(&self) -> PathBuf {
            self.sys().join("kernel/sched_ext")
        }

        /// A kernel that has sched_ext, in the given `state`.
        fn with_sched_ext(&self, state: &str) -> &Lab {
            std::fs::create_dir_all(self.scx_dir()).unwrap();
            std::fs::write(self.scx_dir().join("state"), format!("{state}\n")).unwrap();
            self
        }

        /// `root/ops`, the struct_ops name — note it is `lavd`, not
        /// `scx_lavd`.
        fn with_ops(&self, ops: &str) -> &Lab {
            std::fs::create_dir_all(self.scx_dir().join("root")).unwrap();
            std::fs::write(self.scx_dir().join("root/ops"), format!("{ops}\n")).unwrap();
            self
        }

        /// Install a fake `scxctl`.
        ///
        /// `behaviour` is a shell fragment run with `$1` as the verb. The
        /// refusal texts are the REAL ones, lifted verbatim out of the shipped
        /// `scxctl` 1.1.2 binary (and the first of them out of katana's
        /// journal), because a test that invents an error string does not test
        /// the branch that reads it.
        fn scxctl(&self, behaviour: &str) -> PathBuf {
            let bin = self.root.join("scxctl");
            std::fs::write(&bin, format!("#!/bin/sh\n{behaviour}\n")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            bin
        }

        /// A writer over this lab. The settle budget is tiny so the refusal
        /// cases do not each wait two real seconds for a state that is never
        /// coming.
        fn writer(&self, scxctl: &Path) -> RealWriter {
            RealWriter::for_scx_test(self.sys(), scxctl, Duration::from_millis(120))
        }
    }

    impl Drop for Lab {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    /// The fake that behaves like the real loader: `start` works when nothing
    /// is running, `switch` works when something is, each refuses in the
    /// other's state, and a success actually changes the fixture's `state`.
    const HONEST: &str = r#"
sys="$RIME_TEST_SCX_DIR"
state=$(cat "$sys/state" 2>/dev/null || echo disabled)
case "$1" in
  start)
    if [ "$state" = enabled ]; then
      echo "error: scx scheduler already running, use 'switch' instead of 'start'" >&2
      exit 1
    fi
    echo enabled > "$sys/state"; mkdir -p "$sys/root"; echo lavd > "$sys/root/ops"; exit 0 ;;
  switch)
    if [ "$state" != enabled ]; then
      echo "error: no scx scheduler running, use 'start' instead of 'switch'" >&2
      exit 1
    fi
    echo enabled > "$sys/state"; mkdir -p "$sys/root"; echo lavd > "$sys/root/ops"; exit 0 ;;
  stop)
    echo disabled > "$sys/state"; rm -rf "$sys/root"; exit 0 ;;
esac
exit 64
"#;

    fn honest(lab: &Lab) -> PathBuf {
        let dir = lab.scx_dir();
        lab.scxctl(&format!(
            "RIME_TEST_SCX_DIR='{}'\n{HONEST}",
            dir.display()
        ))
    }

    /// Record the argv the writer chose, and do nothing else.
    fn recorder(lab: &Lab, log: &Path, exit: u8) -> PathBuf {
        lab.scxctl(&format!(
            "echo \"$@\" >> '{}'\nexit {exit}\n",
            log.display()
        ))
    }

    // ── case 1: nothing running. The whole defect. ──────────────────────────

    #[test]
    fn a_kernel_with_no_scheduler_running_gets_start_and_not_switch() {
        let _serial = serial();
        // THE bug: `switch` was hardcoded, and `switch` is precisely the verb
        // that cannot work from `disabled`. This asserts the argv, not just
        // the outcome, because "it worked" could be true for the wrong reason.
        let lab = Lab::new("verb-start");
        lab.with_sched_ext("disabled");
        let log = lab.root.join("argv");
        let w = lab.writer(&recorder(&lab, &log, 0));
        let _ = w.apply(&Action::ScxSwitch {
            sched: "scx_lavd".into(),
        });
        let argv = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            argv.starts_with("start -s scx_lavd"),
            "from `disabled` the verb must be `start`, got: {argv:?}"
        );
        assert!(
            !argv.starts_with("switch"),
            "`switch` from `disabled` is the shipped defect: {argv:?}"
        );
    }

    #[test]
    fn starting_a_scheduler_from_disabled_lands_and_the_kernel_agrees() {
        let _serial = serial();
        let lab = Lab::new("start-lands");
        lab.with_sched_ext("disabled");
        let w = lab.writer(&honest(&lab));
        assert_eq!(
            w.apply(&Action::ScxSwitch {
                sched: "scx_lavd".into()
            })
            .unwrap(),
            Outcome::Landed
        );
        assert_eq!(
            w.scx_state(),
            ScxState::Enabled {
                ops: Some("lavd".into())
            },
            "and the state the daemon reads back must be the kernel's, not the command's"
        );
    }

    // ── case 2: one already running ─────────────────────────────────────────

    #[test]
    fn a_scheduler_already_running_gets_switch_and_not_start() {
        let _serial = serial();
        let lab = Lab::new("verb-switch");
        lab.with_sched_ext("enabled");
        lab.with_ops("rusty");
        let log = lab.root.join("argv");
        let w = lab.writer(&recorder(&lab, &log, 0));
        let _ = w.apply(&Action::ScxSwitch {
            sched: "scx_lavd".into(),
        });
        let argv = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            argv.starts_with("switch -s scx_lavd"),
            "from `enabled` the verb must be `switch`, got: {argv:?}"
        );
    }

    #[test]
    fn the_wrong_verb_is_retried_on_the_one_the_loader_names_and_only_once() {
        let _serial = serial();
        // The state can move between the read and the call, and it can be
        // unreadable at the read. scx_loader's refusal names the verb it
        // wanted, so taking it at its word is not a second guess — but it is
        // capped at one retry so a loader that ping-pongs cannot spin.
        let lab = Lab::new("retry");
        // `state` says disabled, so the writer opens with `start` — and the
        // fake refuses it as though something were running.
        lab.with_sched_ext("disabled");
        let log = lab.root.join("argv");
        let bin = lab.scxctl(&format!(
            r#"echo "$@" >> '{}'
if [ "$1" = start ]; then
  echo "error: scx scheduler already running, use 'switch' instead of 'start'" >&2
  exit 1
fi
echo enabled > '{}/state'
exit 0
"#,
            log.display(),
            lab.scx_dir().display()
        ));
        let w = lab.writer(&bin);
        let out = w
            .apply(&Action::ScxSwitch {
                sched: "scx_lavd".into(),
            })
            .unwrap();
        let argv = std::fs::read_to_string(&log).unwrap_or_default();
        let calls: Vec<&str> = argv.lines().collect();
        assert_eq!(
            calls.len(),
            2,
            "exactly one retry, no more and no fewer: {calls:?}"
        );
        assert!(calls[0].starts_with("start"), "{calls:?}");
        assert!(calls[1].starts_with("switch"), "{calls:?}");
        assert_eq!(out, Outcome::Landed, "and the retry's result is the result");
    }

    #[test]
    fn a_refusal_the_loader_does_not_name_a_verb_for_is_not_retried() {
        let _serial = serial();
        // Only the two "use X instead of Y" messages justify a second call.
        // Retrying on any failure would turn one refusal into two, and hide
        // the real reason behind the second one's.
        let lab = Lab::new("no-retry");
        lab.with_sched_ext("disabled");
        let log = lab.root.join("argv");
        let bin = lab.scxctl(&format!(
            "echo \"$@\" >> '{}'\necho 'error: scheduler not found' >&2\nexit 1\n",
            log.display()
        ));
        let w = lab.writer(&bin);
        let out = w
            .apply(&Action::ScxSwitch {
                sched: "scx_nonesuch".into(),
            })
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&log).unwrap_or_default().lines().count(),
            1,
            "an unrelated refusal must not be retried"
        );
        let Outcome::Refused(why) = out else {
            panic!("expected Refused, got {out:?}");
        };
        assert!(why.contains("scheduler not found"), "{why}");
    }

    // ── case 3: the command lies. This is the one that matters. ─────────────

    #[test]
    fn a_command_that_exits_zero_and_changes_nothing_is_refused_not_landed() {
        let _serial = serial();
        // `scxctl` exiting 0 is a fact about `scxctl`. If the kernel still
        // reads `disabled` afterwards, no scheduler attached — and reporting
        // that as success is the entire defect this unit exists for, in its
        // general form.
        let lab = Lab::new("liar");
        lab.with_sched_ext("disabled");
        let w = lab.writer(&lab.scxctl("exit 0"));
        let out = w
            .apply(&Action::ScxSwitch {
                sched: "scx_lavd".into(),
            })
            .unwrap();
        let Outcome::Refused(why) = out else {
            panic!("a success that changed nothing must not be Landed: {out:?}");
        };
        assert!(
            why.contains("reported success") && why.contains("still") && why.contains("disabled"),
            "and the reason must say the command succeeded and the machine did not move: {why}"
        );
    }

    // ── case 4: cannot tell ─────────────────────────────────────────────────

    #[test]
    fn a_state_that_cannot_be_read_back_is_unknown_and_is_not_a_landing() {
        let _serial = serial();
        // sched_ext is present, `state` is not readable. That is neither
        // "loaded" nor "not loaded", and the third answer is the point.
        let lab = Lab::new("unknown");
        std::fs::create_dir_all(lab.scx_dir()).unwrap();
        // A directory where `state` should be: every read of it fails, and it
        // fails with something that is not NotFound.
        std::fs::create_dir_all(lab.scx_dir().join("state")).unwrap();
        let w = lab.writer(&lab.scxctl("exit 0"));
        let out = w
            .apply(&Action::ScxSwitch {
                sched: "scx_lavd".into(),
            })
            .unwrap();
        let Outcome::Unknown(why) = out.clone() else {
            panic!("an unreadable state must be Unknown, got {out:?}");
        };
        assert!(
            why.contains("could not be confirmed"),
            "and it must say so: {why}"
        );
        assert!(
            !out.landed(),
            "Unknown must never count as a landing — that is the collapse being fixed"
        );
    }

    #[test]
    fn a_scheduler_stuck_enabling_is_unknown_rather_than_rounded_to_a_yes_or_a_no() {
        let _serial = serial();
        let lab = Lab::new("influx");
        lab.with_sched_ext("disabled");
        let w = lab.writer(&lab.scxctl(&format!(
            "echo enabling > '{}/state'\nexit 0\n",
            lab.scx_dir().display()
        )));
        let out = w
            .apply(&Action::ScxSwitch {
                sched: "scx_lavd".into(),
            })
            .unwrap();
        assert!(
            matches!(out, Outcome::Unknown(_)),
            "`enabling` is mid-transition, not an answer: {out:?}"
        );
    }

    // ── case 5: no sched_ext in this kernel at all ──────────────────────────

    #[test]
    fn a_kernel_without_sched_ext_refuses_before_spawning_anything() {
        let _serial = serial();
        // No /sys/kernel/sched_ext. `scxctl` must not even be run: on such a
        // machine it fails confusingly, and the useful sentence is the one
        // naming the kernel config.
        let lab = Lab::new("nosupport");
        std::fs::create_dir_all(lab.sys()).unwrap();
        let log = lab.root.join("argv");
        let w = lab.writer(&recorder(&lab, &log, 0));
        let out = w
            .apply(&Action::ScxSwitch {
                sched: "scx_lavd".into(),
            })
            .unwrap();
        let Outcome::Refused(why) = out else {
            panic!("expected Refused, got {out:?}");
        };
        assert!(
            why.contains("CONFIG_SCHED_CLASS_EXT"),
            "the reason must name what is missing: {why}"
        );
        assert!(
            !log.exists(),
            "scxctl must not be spawned on a kernel that has no sched_ext"
        );
        assert_eq!(w.scx_state(), ScxState::Unsupported);
    }

    #[test]
    fn an_absent_scxctl_is_refused_and_named() {
        let _serial = serial();
        let lab = Lab::new("noscxctl");
        lab.with_sched_ext("disabled");
        let w = lab.writer(&lab.root.join("scxctl-that-is-not-there"));
        let out = w
            .apply(&Action::ScxSwitch {
                sched: "scx_lavd".into(),
            })
            .unwrap();
        assert_eq!(out, Outcome::Refused("scxctl: not installed".into()));
    }

    // ── the exit half, which only now does anything ─────────────────────────

    #[test]
    fn stopping_reads_back_that_the_scheduler_actually_went_away() {
        let _serial = serial();
        let lab = Lab::new("stop");
        lab.with_sched_ext("disabled");
        let bin = honest(&lab);
        let w = lab.writer(&bin);
        w.apply(&Action::ScxSwitch {
            sched: "scx_lavd".into(),
        })
        .unwrap();
        assert_eq!(w.apply(&Action::ScxStop).unwrap(), Outcome::Landed);
        assert_eq!(w.scx_state(), ScxState::Disabled);
    }

    #[test]
    fn a_stop_that_leaves_the_scheduler_attached_is_refused() {
        let _serial = serial();
        let lab = Lab::new("stop-liar");
        lab.with_sched_ext("enabled");
        lab.with_ops("lavd");
        let w = lab.writer(&lab.scxctl("exit 0"));
        let out = w.apply(&Action::ScxStop).unwrap();
        let Outcome::Refused(why) = out else {
            panic!("a stop that stopped nothing must not be Landed: {out:?}");
        };
        assert!(why.contains("still attached"), "{why}");
    }

    #[test]
    fn stopping_when_nothing_is_attached_lands_without_running_anything() {
        let _serial = serial();
        // The ordinary exit on a machine whose scheduler never loaded. It is
        // not a failure to stop something that is not running, and reporting
        // one would make every such exit look like a fault.
        let lab = Lab::new("stop-noop");
        lab.with_sched_ext("disabled");
        let log = lab.root.join("argv");
        let w = lab.writer(&recorder(&lab, &log, 1));
        assert_eq!(w.apply(&Action::ScxStop).unwrap(), Outcome::Landed);
        assert!(!log.exists(), "nothing to stop means nothing to run");
    }

    // ── reading the kernel, in its own right ────────────────────────────────

    #[test]
    fn read_scx_state_tells_absence_from_a_refused_read() {
        let _serial = serial();
        // The rule this program has broken about fourteen times: a failed stat
        // is not an absent feature. Both used to land on the same answer.
        let lab = Lab::new("read");
        std::fs::create_dir_all(lab.sys()).unwrap();
        assert_eq!(read_scx_state(&lab.sys()), ScxState::Unsupported);

        std::fs::create_dir_all(lab.scx_dir().join("state")).unwrap();
        let st = read_scx_state(&lab.sys());
        assert!(
            matches!(st, ScxState::Unreadable(_)),
            "sched_ext present and unreadable is its own answer, got {st:?}"
        );
        assert_ne!(st.verdict(), "not loaded", "and it must not read as absence");
        assert_eq!(st.verdict(), "unknown");
    }

    #[test]
    fn the_struct_ops_name_drops_the_scx_prefix_and_the_comparison_knows_it() {
        let _serial = serial();
        // `scx_lavd` attaches as `lavd`. A verbatim comparison would report a
        // perfectly good scheduler as the wrong one — this unit's own defect,
        // inverted.
        assert!(scx_ops_matches("lavd", "scx_lavd"));
        assert!(scx_ops_matches("scx_lavd", "scx_lavd"));
        assert!(scx_ops_matches("rusty", "scx_rusty"));
        assert!(!scx_ops_matches("rusty", "scx_lavd"));
        assert!(!scx_ops_matches("", "scx_lavd"));
    }

    #[test]
    fn the_build_tag_the_rust_schedulers_append_is_not_a_mismatch() {
        let _serial = serial();
        // Read verbatim off katana's /sys/kernel/sched_ext/root/ops, 2026-09-26.
        assert!(scx_ops_matches("lavd_1.1.3_x86_64_unknown_linux_gnu", "scx_lavd"));
        assert!(scx_ops_matches("rustland_1.1.3_x86_64_unknown_linux_gnu", "scx_rustland"));
        // …but a longer name is a different scheduler, tag or no tag.
        assert!(!scx_ops_matches("lavdx_1.1.3_x86_64_unknown_linux_gnu", "scx_lavd"));
        assert!(!scx_ops_matches("lavd_extra", "scx_lavd"));
        assert!(!scx_ops_matches("rusty_1.1.3_x86_64_unknown_linux_gnu", "scx_lavd"));
        assert!(!scx_ops_matches("lavd_1.1.3", ""));
    }

    #[test]
    fn the_three_verdicts_are_distinct_words() {
        let _serial = serial();
        assert_eq!(ScxState::Enabled { ops: None }.verdict(), "loaded");
        assert_eq!(ScxState::Disabled.verdict(), "not loaded");
        // A kernel that cannot run one is a definite no, not an unknown.
        assert_eq!(ScxState::Unsupported.verdict(), "not loaded");
        assert_eq!(ScxState::InFlux("enabling".into()).verdict(), "unknown");
        assert_eq!(ScxState::Unreadable("EACCES".into()).verdict(), "unknown");
    }

    #[test]
    fn a_dry_run_writer_still_never_spawns_scxctl() {
        let _serial = serial();
        let lab = Lab::new("dry");
        lab.with_sched_ext("disabled");
        let log = lab.root.join("argv");
        let bin = recorder(&lab, &log, 0);
        let mut w = lab.writer(&bin);
        w.dry_run = true;
        assert_eq!(
            w.apply(&Action::ScxSwitch {
                sched: "scx_lavd".into()
            })
            .unwrap(),
            Outcome::Landed
        );
        assert!(!log.exists(), "dry-run must not run the command");
    }
}
