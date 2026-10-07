//! System-tuning profiles: the data model, loading (embedded + on-disk
//! override), and the pure tier -> [`Action`] planner.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::tier::{Action, Tier};
use crate::topology::{CoreSource, CoreTopology};

/// Where the profile classifies in the layered selection hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileKind {
    /// Chassis-level fallback (`generic-desktop`, `generic-laptop`).
    Generic,
    /// CPU-class profile (`amd-zen`, `intel-hybrid`).
    Class,
    /// Exact-machine profile (`thinkpad-l16-g2`, `msi-katana-gf76`).
    Device,
}

/// The three-knob settings for one tier. Every field is optional so a profile
/// can express only the knobs its hardware class supports; the real
/// [`SysWriter`](crate::syswriter::SysWriter) additionally path-checks each one.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TierSettings {
    pub governor: Option<String>,
    pub epp: Option<String>,
    pub platform_profile: Option<String>,
}

/// Default tier for AC and battery, used by the daemon's auto-switch.
#[derive(Debug, Clone, Deserialize)]
pub struct Defaults {
    pub ac: Tier,
    pub battery: Tier,
}

/// Battery charge-threshold *policy* for a profile: the window, and nothing
/// else.
///
/// Deliberately carries no sysfs paths. Which battery (or batteries) can honour
/// a threshold, and under which attribute spelling, is a runtime question
/// answered by [`crate::battery::BatteryInventory`] — a profile that named
/// `BAT0` or `BAT1` was guessing, and guessed wrong on any machine but the one
/// it was written for.
#[derive(Debug, Clone, Deserialize)]
pub struct ChargeConfig {
    pub start: u8,
    pub stop: u8,
}

// ── M6: fan control ──────────────────────────────────────────────────────────

/// Which fan backend a profile wants used.
///
/// `Auto` (the default) probes in descending order of usefulness — real hwmon
/// PWM, then `msi-wmi-platform` (RPM only), then `msi-ec` — and takes whatever
/// is actually present. Naming a backend explicitly never *asserts* that it
/// exists: every leg still probes, and a named-but-absent backend degrades to
/// "unsupported" rather than to an error.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FanBackend {
    #[default]
    Auto,
    /// Generic hwmon `pwmN`/`fanN_input` only.
    Hwmon,
    /// The `msi-wmi-platform` hwmon device only (read-only fan RPM: the driver
    /// registers four `fanN_input` channels and no PWM at all).
    MsiWmi,
    /// The MSI embedded controller only.
    MsiEc,
    /// Fan control disabled for this machine.
    None,
}

/// One point on a fan curve.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct CurvePoint {
    /// Temperature in degrees Celsius.
    pub temp_c: f64,
    /// PWM duty cycle (0-255) at that temperature.
    pub pwm: u8,
}

fn default_min_pwm() -> u8 {
    // ~30% — low enough to be quiet, high enough that no curve, clamp bug or
    // rounding error can ever command a stopped fan.
    77
}
fn default_max_pwm() -> u8 {
    255
}
fn default_boost_threshold() -> u8 {
    // Above this duty cycle, an msi-ec machine (which has no PWM) turns cooler
    // boost on instead.
    224
}
fn default_curve_interval() -> u64 {
    3
}

/// Fan policy for a profile. Every field has a default, so a profile with no
/// `[fan]` table behaves exactly as it did before M6.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FanConfig {
    pub backend: FanBackend,
    /// Never command a duty cycle below this (safety floor).
    pub min_pwm: u8,
    pub max_pwm: u8,
    /// msi-ec cooler-boost threshold for `manual` duty cycles.
    pub boost_pwm_threshold: u8,
    /// Mode applied when the daemon starts. `None` leaves the firmware alone.
    pub default_mode: Option<String>,
    /// msi-ec `fan_mode` value meaning "let the EC decide".
    pub msi_ec_auto_mode: Option<String>,
    /// msi-ec `fan_mode` value used alongside cooler boost for `max`.
    pub msi_ec_max_mode: Option<String>,
    /// Curve points for `curve` mode (empty = no curve mode offered).
    pub curve: Vec<CurvePoint>,
    /// How often the daemon re-evaluates the curve.
    pub curve_interval_secs: u64,
    /// Only consider these hwmon chips (empty = all).
    pub include_hwmon: Vec<String>,
    /// Never touch these hwmon chips.
    pub exclude_hwmon: Vec<String>,
}

impl Default for FanConfig {
    fn default() -> FanConfig {
        FanConfig {
            backend: FanBackend::Auto,
            min_pwm: default_min_pwm(),
            max_pwm: default_max_pwm(),
            boost_pwm_threshold: default_boost_threshold(),
            default_mode: None,
            msi_ec_auto_mode: None,
            msi_ec_max_mode: None,
            curve: Vec::new(),
            curve_interval_secs: default_curve_interval(),
            include_hwmon: Vec::new(),
            exclude_hwmon: Vec::new(),
        }
    }
}

// ── M6: game orchestration ───────────────────────────────────────────────────

/// How a game's cpuset is chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CpusetPolicy {
    /// Pin to the performance cores (falls back to all CPUs when uniform).
    PCores,
    /// Every CPU (creates the cgroup but confines nothing).
    All,
    /// No cpuset work at all.
    Off,
    /// An explicit kernel CPU list, e.g. `0-11`.
    Explicit(String),
}

/// How interrupts are handled during a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqPolicy {
    /// Park interrupts on the CPUs the game is not using.
    AwayFromGame,
    /// Leave interrupt affinity alone.
    Off,
}

/// A GPU clock lock: `"max"`, a single MHz value, or a `[min, max]` pair.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum ClockSpec {
    Fixed(u32),
    Range([u32; 2]),
    Keyword(String),
}

/// NVIDIA handling inside game mode.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct NvidiaConfig {
    pub enabled: bool,
    /// Set persistence mode while the session is active.
    pub persistence: bool,
    /// Graphics clock lock; omitted = do not lock.
    pub graphics_clock: Option<ClockSpec>,
    /// Memory clock lock; omitted = do not lock.
    pub memory_clock: Option<ClockSpec>,
    /// Restrict to one GPU index (default: every GPU reported).
    pub gpu_index: Option<u32>,
}

impl Default for NvidiaConfig {
    fn default() -> NvidiaConfig {
        NvidiaConfig {
            enabled: true,
            persistence: true,
            graphics_clock: None,
            memory_clock: None,
            gpu_index: None,
        }
    }
}

/// What a game session asks of a non-NVIDIA GPU.
///
/// Deliberately small. These are the two controls whose whole range is safe:
/// amdgpu's DPM level is a documented enum the driver validates, and i915's
/// frequency floor is clamped by the driver to the limits it publishes. Neither
/// changes a voltage, neither can be set to a value that damages a card, and
/// both are restored on the way out.
///
/// What is NOT here, and will not be without hardware to test it on:
/// `pp_od_clk_voltage` (amdgpu overclocking, requires `amdgpu.ppfeaturemask`
/// and can hang a card) and any power-limit write.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct SysfsGpuConfig {
    pub enabled: bool,
    /// amdgpu `power_dpm_force_performance_level` while the game runs.
    /// `auto` is the shipped default and means "leave it alone".
    pub amd_perf_level: String,
    /// i915 frequency floor while the game runs, as a percentage of the range
    /// the hardware reports between `gt_RPn_freq_mhz` and `gt_RP0_freq_mhz`.
    /// 0 leaves the floor alone.
    pub intel_floor_percent: u8,
}

impl Default for SysfsGpuConfig {
    fn default() -> SysfsGpuConfig {
        SysfsGpuConfig {
            enabled: true,
            amd_perf_level: "high".to_string(),
            intel_floor_percent: 0,
        }
    }
}


fn default_cgroup() -> String {
    "/sys/fs/cgroup/rime-game".to_string()
}
fn default_cpuset() -> String {
    "p-cores".to_string()
}
fn default_irq() -> String {
    "away-from-game".to_string()
}

/// Game-mode policy for a profile. As with `[fan]`, every field defaults, so
/// pre-M6 profiles keep working untouched.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GameModeConfig {
    pub enabled: bool,
    /// Tier held for the duration of a session (default `performance`, the top
    /// tier every machine can honour).
    pub tier: Tier,
    /// Fan mode held for the duration of a session (`None` = leave as-is).
    pub fan_mode: Option<String>,
    /// `p-cores` | `all` | `off` | an explicit CPU list.
    pub cpuset: String,
    /// cgroup-v2 directory rimed creates for the session.
    pub cgroup: String,
    /// Value for `cpuset.mems`; `None` = read the root cgroup's effective mems.
    pub cpuset_mems: Option<String>,
    /// `away-from-game` | `off`.
    pub irq: String,
    /// Interrupt handler names that belong *on* the game's cores.
    pub irq_pin_to_game: Vec<String>,
    pub nvidia: NvidiaConfig,
    /// The AMD and Intel half of the same job. Named for the mechanism rather
    /// than the vendor because one config drives both: the fields that do not
    /// apply to the card in front of it are simply not planned.
    ///
    /// It exists because `[gamemode]` had an `[nvidia]` section and nothing
    /// else, so a Rime machine with an AMD or Intel GPU — which is most of
    /// them — got cpuset pinning and IRQ steering and no GPU handling at all.
    pub gpu: SysfsGpuConfig,
    /// sched-ext scheduler to load for the duration of a game session:
    /// `auto` (the default), a scheduler name, or empty string = leave the
    /// kernel's own scheduler alone.
    ///
    /// The image ships a CachyOS kernel with CONFIG_SCHED_CLASS_EXT=y and
    /// sixteen scx schedulers, and until now NOTHING selected one — the whole
    /// sched-ext capability that kernel was chosen for sat unused.
    ///
    /// `scx_lavd` is the scheduler meant (latency-aware virtual deadline: the
    /// scx scheduler built for interactive/gaming latency, as opposed to
    /// scx_rusty/scx_layered which target throughput). Defaulting rather than
    /// naming it per-profile is what makes Gaming Mode tuned on ANY machine
    /// instead of only on the author's Katana.
    ///
    /// Safe on a machine that never games, by construction rather than luck:
    /// `scxctl` is a D-Bus client for scx_loader, whose unit is `Type=dbus` and
    /// is NOT enabled at boot — it is activated by the first call, so no
    /// scheduler daemon runs on a laptop that never enters game mode, and the
    /// shipped /usr/share/scx_loader/config.toml leaves `default_sched`
    /// commented out. A profile can still opt out explicitly with `scx = ""`.
    ///
    /// The default is `auto`, not `scx_lavd`, since 2026-10-07: `auto` loads
    /// scx_lavd on a CPU whose cores are all one kind and leaves the kernel's
    /// scheduler alone on a P/E hybrid, the same scheduler the desktop runs.
    /// See [`GameModeConfig::scx_for`] for why. Naming a scheduler still loads
    /// it on any CPU.
    pub scx: String,
}

impl Default for GameModeConfig {
    fn default() -> GameModeConfig {
        GameModeConfig {
            enabled: true,
            tier: Tier::Performance,
            fan_mode: None,
            cpuset: default_cpuset(),
            cgroup: default_cgroup(),
            cpuset_mems: None,
            irq: default_irq(),
            irq_pin_to_game: Vec::new(),
            nvidia: NvidiaConfig::default(),
            gpu: SysfsGpuConfig::default(),
            scx: "auto".to_string(),
        }
    }
}

impl GameModeConfig {
    /// The sched-ext scheduler a session loads on this CPU, or `None` for the
    /// kernel's own scheduler.
    ///
    /// `auto` is `None` on a P/E hybrid, and that is a measurement, not
    /// caution. scx_lavd (1.1.3) splits threads between P- and E-cores by
    /// `perf_cri`, which is built from how often a thread sleeps and wakes
    /// others (`lat_cri.bpf.c`), and it re-picks the CPU at every wake-up. A
    /// game's bottleneck thread does the opposite: it runs flat out and rarely
    /// sleeps, so nothing in that score keeps it on a P-core. On katana
    /// (i7-12700H, 2026-10-07) Cyberpunk 2077's `GameThread` was 98 % busy,
    /// the GPU idle half the time, and under scx_lavd the thread sat on an
    /// E-core in 65 of 200 samples and visited all 20 CPUs in 5 s. The same
    /// game on the desktop, under the kernel's scheduler, played smoothly.
    /// `--performance` does not help: it turns off core compaction, not that
    /// split, and 1.1.3 has no option that does.
    ///
    /// On a CPU with one kind of core the split does not exist (lavd treats
    /// every thread as performance-critical there), so `auto` keeps lavd.
    /// "One kind" is [`CoreTopology::has_efficiency_cores`], not `is_hybrid`:
    /// AMD's preferred-core ranking is not a second kind of core. A CPU whose
    /// topology could not be read at all (`CoreSource::Unknown`) is not known
    /// to be one kind, so it keeps the kernel's scheduler too.
    pub fn scx_for(&self, topo: &CoreTopology) -> Option<String> {
        match self.scx.trim() {
            "" => None,
            s if s.eq_ignore_ascii_case("off") || s.eq_ignore_ascii_case("none") => None,
            s if s.eq_ignore_ascii_case("auto") => {
                let unread = topo.source == CoreSource::Unknown;
                (!unread && !topo.has_efficiency_cores()).then(|| "scx_lavd".to_string())
            }
            s => Some(s.to_string()),
        }
    }

    /// Parse the `cpuset` string into a policy.
    pub fn cpuset_policy(&self) -> CpusetPolicy {
        match self.cpuset.trim().to_ascii_lowercase().as_str() {
            "p-cores" | "pcores" | "performance" => CpusetPolicy::PCores,
            "all" | "" => CpusetPolicy::All,
            "off" | "none" | "disabled" => CpusetPolicy::Off,
            _ => CpusetPolicy::Explicit(self.cpuset.trim().to_string()),
        }
    }

    /// Parse the `irq` string into a policy. Anything unrecognised is treated
    /// as `off` — an unreadable policy must not start moving interrupts.
    pub fn irq_policy(&self) -> IrqPolicy {
        match self.irq.trim().to_ascii_lowercase().as_str() {
            "away-from-game" | "away" | "on" => IrqPolicy::AwayFromGame,
            _ => IrqPolicy::Off,
        }
    }
}

/// A fully-parsed system profile.
#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    pub id: String,
    pub kind: ProfileKind,
    #[serde(default)]
    pub description: String,
    pub defaults: Defaults,
    tiers: HashMap<Tier, TierSettings>,
    #[serde(default)]
    pub charge: Option<ChargeConfig>,
    /// M6 fan policy. Absent in pre-M6 profiles; defaults apply.
    #[serde(default)]
    pub fan: Option<FanConfig>,
    /// M6 game-mode policy. Absent in pre-M6 profiles; defaults apply.
    #[serde(default)]
    pub gamemode: Option<GameModeConfig>,
}

impl Profile {
    /// Parse a profile from a TOML string, validating that every tier is
    /// present.
    pub fn from_toml(s: &str) -> Result<Profile> {
        let p: Profile = toml::from_str(s).context("parsing profile TOML")?;
        for tier in Tier::ALL {
            if !p.tiers.contains_key(&tier) {
                bail!("profile '{}' is missing tier '{}'", p.id, tier);
            }
        }
        Ok(p)
    }

    /// Settings for a tier (always present after validation).
    pub fn tier(&self, tier: Tier) -> &TierSettings {
        self.tiers
            .get(&tier)
            .expect("all tiers validated present at load")
    }

    /// The ordered per-tier hardware plan for `tier`.
    ///
    /// This is the pure heart of the tier engine: it turns a (profile, tier)
    /// pair into the exact list of [`Action`]s a `SysWriter` should apply. It
    /// performs no I/O and is fully unit-testable.
    ///
    /// Charge thresholds are intentionally NOT part of a tier plan — they are
    /// applied independently against the discovered batteries (see
    /// [`Profile::charge_window`]).
    ///
    /// Every action a tier can emit is one of the three portable CPU/platform
    /// knobs. The writer validates each against what the running kernel
    /// advertises and skips or substitutes rather than failing, so the same
    /// plan is safe on AMD `amd-pstate`, Intel `intel_pstate` (hybrid or not),
    /// plain `acpi-cpufreq` and ARM `cpufreq-dt` alike.
    pub fn plan_tier(&self, tier: Tier) -> Vec<Action> {
        let mut actions = Vec::new();
        let s = self.tier(tier);
        if let Some(g) = &s.governor {
            actions.push(Action::Governor(g.clone()));
        }
        if let Some(e) = &s.epp {
            actions.push(Action::Epp(e.clone()));
        }
        if let Some(p) = &s.platform_profile {
            actions.push(Action::PlatformProfile(p.clone()));
        }
        actions
    }

    /// The transition plan from `from` to `to`. Tiers are pure state
    /// assertions, so this is simply the target tier's plan — there is nothing
    /// left over from the previous tier that needs tearing down.
    pub fn plan_transition(&self, _from: Option<Tier>, to: Tier) -> Vec<Action> {
        self.plan_tier(to)
    }

    /// The profile's fan policy, or the shipped defaults when it declares none.
    /// Pre-M6 profiles therefore get sane, safe behaviour without edits.
    pub fn fan_config(&self) -> FanConfig {
        self.fan.clone().unwrap_or_default()
    }

    /// The profile's game-mode policy, or the shipped defaults.
    pub fn game_config(&self) -> GameModeConfig {
        self.gamemode.clone().unwrap_or_default()
    }

    /// The `(start, stop)` charge window this profile wants, if it declares one.
    /// Turning that into writes is [`BatteryInventory::plan_thresholds`], which
    /// discovers the batteries and their attribute spellings at runtime.
    ///
    /// [`BatteryInventory::plan_thresholds`]: crate::battery::BatteryInventory::plan_thresholds
    pub fn charge_window(&self) -> Option<(u8, u8)> {
        self.charge.as_ref().map(|c| (c.start, c.stop))
    }

    /// The charge-threshold actions for this profile against `batteries`.
    /// Empty when the profile declares no window *or* when no battery on this
    /// machine accepts one — both are ordinary states, not errors.
    pub fn charge_actions(&self, batteries: &crate::battery::BatteryInventory) -> Vec<Action> {
        match self.charge_window() {
            Some((start, stop)) => batteries.plan_thresholds(start, stop),
            None => Vec::new(),
        }
    }
}

/// The set of profiles available for selection, keyed by ID.
#[derive(Debug, Clone, Default)]
pub struct ProfileSet {
    profiles: HashMap<String, Profile>,
}

/// The six profiles baked into the binary. Single source of truth is the
/// checked-in `config/sysprofiles/*.toml`; these are embedded so the daemon
/// works even if `/usr/share/rimeos/sysprofiles` is missing, and so tests run
/// against the exact shipped files.
pub const BUILTIN_PROFILE_TOML: [(&str, &str); 6] = [
    (
        "generic-desktop",
        include_str!("../../../config/sysprofiles/generic-desktop.toml"),
    ),
    (
        "generic-laptop",
        include_str!("../../../config/sysprofiles/generic-laptop.toml"),
    ),
    (
        "intel-hybrid",
        include_str!("../../../config/sysprofiles/intel-hybrid.toml"),
    ),
    (
        "amd-zen",
        include_str!("../../../config/sysprofiles/amd-zen.toml"),
    ),
    (
        "thinkpad-l16-g2",
        include_str!("../../../config/sysprofiles/thinkpad-l16-g2.toml"),
    ),
    (
        "msi-katana-gf76",
        include_str!("../../../config/sysprofiles/msi-katana-gf76.toml"),
    ),
];

impl ProfileSet {
    /// The six embedded profiles.
    pub fn builtin() -> ProfileSet {
        let mut set = ProfileSet::default();
        for (name, toml) in BUILTIN_PROFILE_TOML {
            let p = Profile::from_toml(toml)
                .unwrap_or_else(|e| panic!("builtin profile '{name}' failed to parse: {e:#}"));
            assert_eq!(p.id, name, "builtin profile id mismatch for {name}");
            set.profiles.insert(p.id.clone(), p);
        }
        set
    }

    /// Load profiles from a directory of `*.toml` files, falling back to the
    /// embedded set if the directory is absent, unreadable, or yields nothing
    /// usable.
    ///
    /// A malformed or stale file is **logged and skipped**, not fatal. This is
    /// deliberate: the on-disk set is an override that an older image, a hand
    /// edit or a partially-applied update can leave inconsistent with the
    /// binary's schema (a profile still naming a tier that no longer exists,
    /// say). Refusing to start the power daemon over one bad override would
    /// leave the machine with no power management at all, which is strictly
    /// worse than running the embedded profiles.
    pub fn load(dir: Option<&Path>) -> Result<ProfileSet> {
        let Some(dir) = dir else {
            return Ok(ProfileSet::builtin());
        };
        if !dir.is_dir() {
            return Ok(ProfileSet::builtin());
        }
        let mut set = ProfileSet::default();
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                eprintln!(
                    "rimed: cannot read {} ({e}) — using the embedded profiles",
                    dir.display()
                );
                return Ok(ProfileSet::builtin());
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("rimed: skipping {} (unreadable: {e})", path.display());
                    continue;
                }
            };
            match Profile::from_toml(&text) {
                Ok(p) => {
                    set.profiles.insert(p.id.clone(), p);
                }
                Err(e) => eprintln!("rimed: skipping {} ({e:#})", path.display()),
            }
        }
        if set.profiles.is_empty() {
            return Ok(ProfileSet::builtin());
        }
        // Backfill anything the override directory did not supply, so a
        // directory holding only a device profile still has a generic layer to
        // fall back to.
        for (name, toml) in BUILTIN_PROFILE_TOML {
            if !set.profiles.contains_key(name) {
                if let Ok(p) = Profile::from_toml(toml) {
                    set.profiles.insert(p.id.clone(), p);
                }
            }
        }
        Ok(set)
    }

    /// Look up a profile by ID.
    pub fn get(&self, id: &str) -> Option<&Profile> {
        self.profiles.get(id)
    }

    /// Number of profiles in the set.
    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    /// True if the set has no profiles.
    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }
}
