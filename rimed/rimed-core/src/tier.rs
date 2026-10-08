//! Power tiers and the hardware actions they map to.
//!
//! The three tier IDs are the wire contract: they must match the strings the
//! D-Bus API and the rime-shell `PowerProfileService` use verbatim:
//! `performance`, `balanced`, `power-saver`.
//!
//! Every tier here is expressible on *any* machine, because a tier is only ever
//! a request for the three portable knobs (`scaling_governor`,
//! `energy_performance_preference`, ACPI `platform_profile`) and the writer
//! applies only the ones the running kernel actually exposes. The former
//! `ultra` / `ultra-max` tiers were removed in the universal-hardware pass:
//! they existed to drive a RyzenAdj/EC-defeat path that only ever worked on one
//! specific laptop and could not be honoured anywhere else.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A power tier, ordered from most aggressive (`Performance`) to most frugal
/// (`PowerSaver`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Tier {
    #[serde(rename = "performance")]
    Performance,
    #[serde(rename = "balanced")]
    Balanced,
    #[serde(rename = "power-saver")]
    PowerSaver,
}

impl Tier {
    /// All tiers, highest to lowest. This is the canonical order the CLI and
    /// D-Bus `Tiers` property advertise.
    pub const ALL: [Tier; 3] = [Tier::Performance, Tier::Balanced, Tier::PowerSaver];

    /// The wire-facing string ID.
    pub const fn as_str(self) -> &'static str {
        match self {
            Tier::Performance => "performance",
            Tier::Balanced => "balanced",
            Tier::PowerSaver => "power-saver",
        }
    }

    /// A human-friendly label (matches the shell's picker labels).
    pub const fn label(self) -> &'static str {
        match self {
            Tier::Performance => "Performance",
            Tier::Balanced => "Balanced",
            Tier::PowerSaver => "Power Saver",
        }
    }

    /// The list of frozen IDs, for the D-Bus `Tiers` property.
    pub fn all_ids() -> Vec<String> {
        Tier::ALL.iter().map(|t| t.as_str().to_string()).collect()
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Tier {
    type Err = UnknownTier;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "performance" => Ok(Tier::Performance),
            "balanced" => Ok(Tier::Balanced),
            "power-saver" => Ok(Tier::PowerSaver),
            other => Err(UnknownTier(other.to_string())),
        }
    }
}

/// Error for an unrecognised tier ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownTier(pub String);

impl fmt::Display for UnknownTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown tier '{}' (expected one of: {})",
            self.0,
            Tier::ALL
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl std::error::Error for UnknownTier {}

/// A single intended hardware effect. Every effect a tier can request is one of
/// these; the `SysWriter` trait is the only thing that turns them into real
/// sysfs writes or process execs, which is what makes the whole engine
/// testable with a `MockWriter`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Write `scaling_governor` on every cpufreq policy.
    Governor(String),
    /// Write `energy_performance_preference` on every cpufreq policy (skipped
    /// by the real writer where the attribute is absent).
    Epp(String),
    /// Write `/sys/firmware/acpi/platform_profile`; where that is absent, the
    /// MSI EC's `shift_mode` when msi-ec is bound (skipped where neither is).
    PlatformProfile(String),
    /// Hold the CPU package to these power limits (watts): PL1 (sustained)
    /// and PL2 (burst). Where thermald runs, it is paused for as long as the
    /// limits are held, because its adaptive mode re-applies the firmware's
    /// own limits within seconds. See `RealWriter::apply_cpu_power_limit`.
    CpuPowerLimit { pl1_w: u32, pl2_w: u32 },
    /// Hand the CPU package's power limits back to the firmware: undo a
    /// [`Action::CpuPowerLimit`] (restart thermald where it was paused). A
    /// no-op when nothing was held.
    CpuPowerFirmware,
    /// Write a power-saving control (absolute sysfs path) for game mode:
    /// USB runtime PM, the HDA codec's power_save. `what` is a log label.
    PowerSaveAttr { path: String, value: String, what: String },
    /// Write battery charge start/stop thresholds on one battery.
    ///
    /// Both paths are optional because charge-threshold support is ragged: many
    /// drivers expose only `charge_control_end_threshold`, some use the older
    /// `charge_{start,stop}_threshold` spelling, and most hardware has neither.
    /// The paths are discovered at runtime (see [`crate::battery`]) rather than
    /// named in a profile, so a machine with one battery, two batteries or none
    /// at all is handled by the same code.
    ChargeThresholds {
        start: u8,
        stop: u8,
        start_path: Option<String>,
        end_path: Option<String>,
    },

    // ── M6: fan control ──────────────────────────────────────────────────────
    /// Write a hwmon `pwmN_enable` (0 = full speed / no control, 1 = manual,
    /// 2 = firmware automatic). Absolute path.
    FanPwmEnable { path: String, value: u8 },
    /// Write a hwmon `pwmN` duty cycle (0-255). Absolute path.
    FanPwm { path: String, value: u8 },
    /// Write a vendor fan attribute (e.g. msi-ec `fan_mode` / `cooler_boost`).
    /// Absolute path, string value; `what` is a log label.
    FanVendorAttr {
        path: String,
        value: String,
        what: String,
    },
    /// The one safety primitive: hand a fan back to firmware control, with a
    /// documented fallback ladder (prior enable -> 2 (auto) -> full speed).
    /// A fan must never be left in manual mode at a low duty cycle.
    FanSafeRestore {
        enable_path: Option<String>,
        pwm_path: Option<String>,
        prior_enable: Option<u8>,
        prior_pwm: Option<u8>,
    },

    // ── M6: game orchestration ───────────────────────────────────────────────
    /// `nvidia-smi -i <gpu> -pm <0|1>`.
    NvidiaPersistence { gpu: u32, enabled: bool },
    /// `nvidia-smi -i <gpu> -lgc <min>,<max>`.
    NvidiaLockGraphics { gpu: u32, min_mhz: u32, max_mhz: u32 },
    /// `nvidia-smi -i <gpu> -lmc <min>,<max>`.
    NvidiaLockMemory { gpu: u32, min_mhz: u32, max_mhz: u32 },
    /// `nvidia-smi -i <gpu> -rgc`.
    NvidiaResetGraphics { gpu: u32 },
    /// `nvidia-smi -i <gpu> -rmc`.
    NvidiaResetMemory { gpu: u32 },
    /// Write an absolute GPU sysfs attribute — amdgpu's
    /// `power_dpm_force_performance_level`, i915's `gt_min_freq_mhz`. `what` is
    /// a log label.
    ///
    /// Generic on purpose, and separate from [`Action::FanVendorAttr`] on
    /// purpose too: the two are the same write with different consequences, and
    /// a plan that mixes them cannot be read to say which hardware it touches.
    /// The values are validated where they are PLANNED (see [`crate::gpu`]),
    /// because the driver answers an invalid one with `-EINVAL` and a refused
    /// write is indistinguishable from an applied one at this level.
    GpuSysfsAttr {
        path: String,
        value: String,
        what: String,
    },
    /// Write a CPU list to an absolute `/proc/irq/<n>/smp_affinity_list` path.
    /// Never fatal: many IRQs are kernel-managed and reject affinity writes.
    IrqAffinity { path: String, cpus: String },
    /// Ensure a cgroup-v2 directory exists and carries the given cpuset.
    CgroupEnsure {
        path: String,
        cpus: String,
        mems: String,
    },
    /// Move a PID into the cgroup at `path` (writes `<path>/cgroup.procs`).
    /// Used both to pin a game and — with the recorded prior path — to restore.
    CgroupAttach { path: String, pid: u32 },
    /// Remove an emptied cgroup directory (best-effort).
    CgroupRemove { path: String },

    // ── sched-ext (sched_ext / scx) ───────────────────────────────────────────
    // The shipped CachyOS kernel has CONFIG_SCHED_CLASS_EXT=y and the image
    // carries sixteen scx schedulers plus scxctl, and nothing ever selected one.
    // These two actions are that missing switch, driven from the game profile so
    // it applies on EVERY path into game mode: `rime game start`, gamemode.ini's
    // hook, and the gamescope session.
    /// `scxctl switch -s <sched>` — load or replace the running scx scheduler.
    ScxSwitch { sched: String },
    /// `scxctl stop` — hand scheduling back to the kernel's own class (EEVDF).
    ScxStop,
}

impl Action {
    /// A stable, log-friendly rendering of the action (used by the dry-run
    /// planner and the CLI).
    pub fn describe(&self) -> String {
        match self {
            Action::Governor(g) => format!("scaling_governor = {g} (all policies)"),
            Action::Epp(e) => format!("energy_performance_preference = {e} (all policies)"),
            Action::PlatformProfile(p) => format!("platform_profile = {p}"),
            Action::CpuPowerLimit { pl1_w, pl2_w } => {
                format!("CPU package power limits = {pl1_w} W sustained, {pl2_w} W burst")
            }
            Action::CpuPowerFirmware => "CPU package power limits = the firmware's".to_string(),
            Action::PowerSaveAttr { path, value, what } => format!("{what}: {path} <- {value}"),
            Action::ChargeThresholds {
                start,
                stop,
                start_path,
                end_path,
            } => format!(
                "charge thresholds start={start} ({}) stop={stop} ({})",
                start_path.as_deref().unwrap_or("unsupported"),
                end_path.as_deref().unwrap_or("unsupported"),
            ),
            Action::FanPwmEnable { path, value } => {
                let meaning = match value {
                    0 => " (no control = full speed)",
                    1 => " (manual)",
                    2 => " (firmware automatic)",
                    _ => "",
                };
                format!("{path} <- {value}{meaning}")
            }
            Action::FanPwm { path, value } => {
                format!("{path} <- {value} ({}%)", (*value as u32 * 100) / 255)
            }
            Action::FanVendorAttr { path, value, what } => {
                format!("{what}: {path} <- {value}")
            }
            Action::GpuSysfsAttr { path, value, what } => {
                format!("{what}: {path} <- {value}")
            }
            Action::FanSafeRestore {
                enable_path,
                prior_enable,
                prior_pwm,
                ..
            } => format!(
                "restore fan to firmware control ({}, prior enable={}, prior pwm={})",
                enable_path.as_deref().unwrap_or("no pwm_enable"),
                prior_enable
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "?".into()),
                prior_pwm.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
            ),
            Action::NvidiaPersistence { gpu, enabled } => {
                format!("nvidia-smi -i {gpu} -pm {}", u8::from(*enabled))
            }
            Action::ScxSwitch { sched } => format!("sched-ext scheduler = {sched}"),
            Action::ScxStop => "sched-ext stopped (back to the kernel scheduler)".to_string(),
            Action::NvidiaLockGraphics {
                gpu,
                min_mhz,
                max_mhz,
            } => format!("nvidia-smi -i {gpu} -lgc {min_mhz},{max_mhz}"),
            Action::NvidiaLockMemory {
                gpu,
                min_mhz,
                max_mhz,
            } => format!("nvidia-smi -i {gpu} -lmc {min_mhz},{max_mhz}"),
            Action::NvidiaResetGraphics { gpu } => format!("nvidia-smi -i {gpu} -rgc"),
            Action::NvidiaResetMemory { gpu } => format!("nvidia-smi -i {gpu} -rmc"),
            Action::IrqAffinity { path, cpus } => format!("{path} <- {cpus}"),
            Action::CgroupEnsure { path, cpus, mems } => {
                format!("cgroup {path}: cpuset.cpus={cpus} cpuset.mems={mems}")
            }
            Action::CgroupAttach { path, pid } => format!("cgroup {path}: attach pid {pid}"),
            Action::CgroupRemove { path } => format!("cgroup {path}: remove"),
        }
    }
}
