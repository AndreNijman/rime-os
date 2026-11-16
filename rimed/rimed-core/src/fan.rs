//! Fan enumeration, fan modes, and the plans that drive them.
//!
//! Two backends, discovered independently and used together when both exist:
//!
//! * **hwmon** (`/sys/class/hwmon/hwmon*/`): `fanN_input` for RPM, `pwmN` +
//!   `pwmN_enable` for control. `pwmN_enable` follows the kernel hwmon ABI:
//!   `0` = no control (**full speed**), `1` = manual (`pwmN` is the duty cycle
//!   0-255), `2`+ = firmware/automatic control.
//! * **msi-ec** (`/sys/devices/platform/msi-ec/`): the MSI embedded-controller
//!   driver. It exposes **no PWM at all** — control is `fan_mode`
//!   (`auto`/`silent`/`basic`/`advanced`) plus `cooler_boost` (`on`/`off`), and
//!   the readings `cpu/realtime_fan_speed` and `gpu/realtime_fan_speed` are a
//!   **percentage, not RPM**. The Katana is therefore an "auto vs boost"
//!   machine, not a duty-cycle machine, and this module never fabricates an RPM
//!   for it.
//!
//! Nothing here writes: every mutation is an [`Action`] carrying an absolute
//! path, applied by a [`SysWriter`](crate::syswriter::SysWriter).

use std::fmt;
use std::path::Path;

use crate::profile::{CurvePoint, FanBackend, FanConfig};
use crate::tier::Action;

/// The msi-ec platform directory, relative to the sysfs root.
pub const MSI_EC_REL: &str = "devices/platform/msi-ec";

/// hwmon `name` values the `msi-wmi-platform` driver registers. It exposes four
/// read-only `fanN_input` channels and **no PWM**, so it can report speeds but
/// never command them.
pub const MSI_WMI_CHIPS: &[&str] = &["msi_wmi_platform", "msi-wmi-platform"];

/// A requested fan behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanMode {
    /// Firmware/EC control (the safe default, and the state we always restore).
    Auto,
    /// Everything flat out: `pwm_enable=1` + `pwm=255`, and `cooler_boost=on`
    /// on msi-ec.
    Max,
    /// A fixed duty cycle (0-255), floored by `FanConfig::min_pwm`.
    Manual(u8),
    /// The profile's temperature curve, re-evaluated by the daemon on a cadence.
    Curve,
}

impl FanMode {
    /// The wire/CLI keyword (a `Manual` duty cycle is reported separately).
    pub const fn as_str(self) -> &'static str {
        match self {
            FanMode::Auto => "auto",
            FanMode::Max => "max",
            FanMode::Manual(_) => "manual",
            FanMode::Curve => "curve",
        }
    }

    /// Parse a mode keyword: `auto`, `max`/`full`, `manual`, `manual:<0-255>`,
    /// `curve`.
    pub fn parse(s: &str, default_manual_pwm: u8) -> Result<FanMode, UnknownFanMode> {
        let s = s.trim().to_ascii_lowercase();
        match s.as_str() {
            "auto" | "firmware" => Ok(FanMode::Auto),
            "max" | "full" | "boost" => Ok(FanMode::Max),
            "manual" => Ok(FanMode::Manual(default_manual_pwm)),
            "curve" => Ok(FanMode::Curve),
            other => {
                if let Some(v) = other.strip_prefix("manual:") {
                    if let Ok(pwm) = v.trim().parse::<u8>() {
                        return Ok(FanMode::Manual(pwm));
                    }
                }
                Err(UnknownFanMode(other.to_string()))
            }
        }
    }
}

impl fmt::Display for FanMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FanMode::Manual(p) => write!(f, "manual:{p}"),
            other => f.write_str(other.as_str()),
        }
    }
}

/// Error for an unrecognised fan mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownFanMode(pub String);

impl fmt::Display for UnknownFanMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown fan mode '{}' (expected auto, max, manual[:0-255] or curve)",
            self.0
        )
    }
}

impl std::error::Error for UnknownFanMode {}

/// One readable fan (RPM via hwmon, or a percentage via msi-ec).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanSensor {
    /// Stable-ish identifier, e.g. `nct6797/fan1` or `msi-ec/cpu`.
    pub id: String,
    /// Backend chip name (`nct6797`, `msi-ec`, ...).
    pub chip: String,
    /// Absolute path to an RPM attribute, if the backend reports RPM.
    pub rpm_path: Option<String>,
    /// Absolute path to a percentage attribute (msi-ec), if that is all there is.
    pub percent_path: Option<String>,
}

/// One controllable PWM channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanControl {
    pub id: String,
    pub chip: String,
    /// Absolute path to `pwmN`.
    pub pwm_path: String,
    /// Absolute path to `pwmN_enable`, when the driver has one.
    pub enable_path: Option<String>,
}

/// The msi-ec vendor backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsiEc {
    pub root: String,
    pub fan_mode_path: Option<String>,
    pub available_modes: Vec<String>,
    pub cooler_boost_path: Option<String>,
    pub cpu_speed_path: Option<String>,
    pub gpu_speed_path: Option<String>,
}

/// Everything discovered about this machine's fans.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FanInventory {
    pub sensors: Vec<FanSensor>,
    pub controls: Vec<FanControl>,
    pub msi_ec: Option<MsiEc>,
}

impl FanInventory {
    /// True when *something* can be commanded. A machine with only readable
    /// fans reports `false` here and the daemon answers `Supported = false`
    /// rather than pretending.
    pub fn controllable(&self) -> bool {
        !self.controls.is_empty()
            || self
                .msi_ec
                .as_ref()
                .map(|m| m.fan_mode_path.is_some() || m.cooler_boost_path.is_some())
                .unwrap_or(false)
    }

    /// The mode keywords this hardware can actually honour.
    pub fn modes(&self, cfg: &FanConfig) -> Vec<String> {
        let mut out = Vec::new();
        if !self.controllable() {
            return out;
        }
        out.push("auto".to_string());
        out.push("max".to_string());
        if !self.controls.is_empty() {
            // A duty cycle only means something on the hwmon backend; msi-ec has
            // no PWM.
            out.push("manual".to_string());
            if !cfg.curve.is_empty() {
                out.push("curve".to_string());
            }
        }
        out
    }

    /// A human-readable list of what discovery actually found, distinguishing
    /// controllable channels from read-only sources. This is what `rime fan
    /// status` and the daemon's start-up line report, so an operator can tell
    /// "no fan hardware" from "fans visible but not controllable".
    pub fn summary(&self) -> Vec<String> {
        let mut out = Vec::new();
        for c in &self.controls {
            out.push(format!("{} (pwm control)", c.id));
        }
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for s in &self.sensors {
            if self.controls.iter().any(|c| c.chip == s.chip) || !seen.insert(&s.chip) {
                continue;
            }
            let n = self.sensors.iter().filter(|x| x.chip == s.chip).count();
            let label = if MSI_WMI_CHIPS.iter().any(|c| s.chip.eq_ignore_ascii_case(c)) {
                "msi-wmi-platform: read-only RPM, no PWM in this driver"
            } else if s.chip == "msi-ec" {
                "msi-ec: read-only fan percentage"
            } else {
                "read-only"
            };
            out.push(format!("{} x{n} ({label})", s.chip));
        }
        if let Some(ec) = &self.msi_ec {
            out.push(format!(
                "msi-ec {} (fan_mode={}, cooler_boost={})",
                ec.root,
                if ec.fan_mode_path.is_some() { "yes" } else { "no" },
                if ec.cooler_boost_path.is_some() { "yes" } else { "no" }
            ));
        }
        if out.is_empty() {
            out.push("nothing found".to_string());
        }
        out
    }

    /// Discover fans under `sys_root`, honouring the profile's backend
    /// preference and hwmon allow/deny lists. Read-only; never fails.
    ///
    /// Every leg *probes*: naming `msi-ec` in a profile does not conjure the
    /// platform device, and on a machine where the module never bound (the
    /// Katana's `MS-17L3` board is not in the in-tree driver's firmware
    /// allowlist) the result is simply an inventory with nothing controllable.
    pub fn discover(sys_root: &Path, cfg: &FanConfig) -> FanInventory {
        let mut inv = FanInventory::default();
        match cfg.backend {
            FanBackend::None => return inv,
            FanBackend::Hwmon => {
                discover_hwmon(sys_root, cfg, &mut inv, None);
            }
            FanBackend::MsiWmi => {
                discover_hwmon(sys_root, cfg, &mut inv, Some(MSI_WMI_CHIPS));
            }
            FanBackend::MsiEc => {
                inv.msi_ec = discover_msi_ec(sys_root);
            }
            FanBackend::Auto => {
                // hwmon covers both a real PWM controller and the read-only
                // msi-wmi-platform RPM channels; msi-ec is the last resort.
                discover_hwmon(sys_root, cfg, &mut inv, None);
                inv.msi_ec = discover_msi_ec(sys_root);
            }
        }
        if let Some(ec) = &inv.msi_ec {
            if let Some(p) = &ec.cpu_speed_path {
                inv.sensors.push(FanSensor {
                    id: "msi-ec/cpu".into(),
                    chip: "msi-ec".into(),
                    rpm_path: None,
                    percent_path: Some(p.clone()),
                });
            }
            if let Some(p) = &ec.gpu_speed_path {
                inv.sensors.push(FanSensor {
                    id: "msi-ec/gpu".into(),
                    chip: "msi-ec".into(),
                    rpm_path: None,
                    percent_path: Some(p.clone()),
                });
            }
        }
        inv
    }

    /// Read every sensor and control. Absolute paths, read-only.
    pub fn read(&self) -> Vec<FanReading> {
        let mut out: Vec<FanReading> = self
            .sensors
            .iter()
            .map(|s| FanReading {
                id: s.id.clone(),
                chip: s.chip.clone(),
                rpm: s.rpm_path.as_deref().and_then(read_u32),
                percent: s
                    .percent_path
                    .as_deref()
                    .and_then(read_u32)
                    .map(|v| v.min(255) as u8),
                pwm: None,
                controllable: false,
            })
            .collect();
        for c in &self.controls {
            let pwm = read_u32(&c.pwm_path).map(|v| v.min(255) as u8);
            // Fold the duty cycle into the matching sensor when the chip and
            // channel line up (nct6797/fan1 <-> nct6797/pwm1).
            let channel = c.id.rsplit('/').next().unwrap_or_default().replace("pwm", "");
            if let Some(r) = out
                .iter_mut()
                .find(|r| r.chip == c.chip && r.id.ends_with(&format!("fan{channel}")))
            {
                r.pwm = pwm;
                r.controllable = true;
            } else {
                out.push(FanReading {
                    id: c.id.clone(),
                    chip: c.chip.clone(),
                    rpm: None,
                    percent: None,
                    pwm,
                    controllable: true,
                });
            }
        }
        out
    }
}

/// A point-in-time reading of one fan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanReading {
    pub id: String,
    pub chip: String,
    /// Revolutions per minute, where the backend reports RPM at all.
    pub rpm: Option<u32>,
    /// Percentage of full speed, where the backend reports a percentage
    /// (msi-ec) instead of RPM.
    pub percent: Option<u8>,
    /// Current PWM duty cycle (0-255) if this fan has a control channel.
    pub pwm: Option<u8>,
    pub controllable: bool,
}

fn read_u32(path: &str) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse::<u32>().ok()
}

fn read_trim(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn abs(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn discover_hwmon(
    sys_root: &Path,
    cfg: &FanConfig,
    inv: &mut FanInventory,
    only_chips: Option<&[&str]>,
) {
    let base = sys_root.join("class/hwmon");
    let Ok(entries) = std::fs::read_dir(&base) else {
        return;
    };
    let mut dirs: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    dirs.sort();
    for dir in dirs {
        let chip = read_trim(&dir.join("name")).unwrap_or_else(|| {
            dir.file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        });
        if let Some(only) = only_chips {
            if !only.iter().any(|c| chip.eq_ignore_ascii_case(c)) {
                continue;
            }
        }
        if !cfg.include_hwmon.is_empty() && !cfg.include_hwmon.iter().any(|n| n == &chip) {
            continue;
        }
        if cfg.exclude_hwmon.iter().any(|n| n == &chip) {
            continue;
        }
        // hwmon channels are 1-based and rarely go beyond a handful.
        for n in 1..=8u32 {
            let rpm = dir.join(format!("fan{n}_input"));
            if rpm.exists() {
                inv.sensors.push(FanSensor {
                    id: format!("{chip}/fan{n}"),
                    chip: chip.clone(),
                    rpm_path: Some(abs(&rpm)),
                    percent_path: None,
                });
            }
            let pwm = dir.join(format!("pwm{n}"));
            if pwm.exists() {
                let enable = dir.join(format!("pwm{n}_enable"));
                inv.controls.push(FanControl {
                    id: format!("{chip}/pwm{n}"),
                    chip: chip.clone(),
                    pwm_path: abs(&pwm),
                    enable_path: enable.exists().then(|| abs(&enable)),
                });
            }
        }
    }
}

fn discover_msi_ec(sys_root: &Path) -> Option<MsiEc> {
    let root = sys_root.join(MSI_EC_REL);
    if !root.is_dir() {
        // The module never bound (or is not installed): there is no platform
        // device, so there is nothing to probe. This is the expected outcome on
        // a Katana whose EC firmware is absent from the in-tree driver's
        // allowlist.
        return None;
    }
    let fan_mode = root.join("fan_mode");
    let cooler_boost = root.join("cooler_boost");
    let cpu_speed = root.join("cpu/realtime_fan_speed");
    let gpu_speed = root.join("gpu/realtime_fan_speed");
    let available_modes = read_trim(&root.join("available_fan_modes"))
        .map(|s| {
            s.split_whitespace()
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let ec = MsiEc {
        root: abs(&root),
        fan_mode_path: fan_mode.exists().then(|| abs(&fan_mode)),
        available_modes,
        cooler_boost_path: cooler_boost.exists().then(|| abs(&cooler_boost)),
        cpu_speed_path: cpu_speed.exists().then(|| abs(&cpu_speed)),
        gpu_speed_path: gpu_speed.exists().then(|| abs(&gpu_speed)),
    };
    // A directory with none of the fan attributes is not a usable backend —
    // do not advertise one.
    let usable = ec.fan_mode_path.is_some()
        || ec.cooler_boost_path.is_some()
        || ec.cpu_speed_path.is_some()
        || ec.gpu_speed_path.is_some();
    usable.then_some(ec)
}

/// What the fans looked like before rimed first touched them. Captured once,
/// on the first mutation, and replayed verbatim on restore.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FanSnapshot {
    pub controls: Vec<ControlSnapshot>,
    pub vendor: Option<VendorSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlSnapshot {
    pub id: String,
    pub pwm_path: String,
    pub enable_path: Option<String>,
    pub enable: Option<u8>,
    pub pwm: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorSnapshot {
    pub fan_mode_path: Option<String>,
    pub fan_mode: Option<String>,
    pub cooler_boost_path: Option<String>,
    pub cooler_boost: Option<String>,
}

impl FanSnapshot {
    /// Read the current state of everything the inventory can command.
    pub fn capture(inv: &FanInventory) -> FanSnapshot {
        FanSnapshot {
            controls: inv
                .controls
                .iter()
                .map(|c| ControlSnapshot {
                    id: c.id.clone(),
                    pwm_path: c.pwm_path.clone(),
                    enable_path: c.enable_path.clone(),
                    enable: c
                        .enable_path
                        .as_deref()
                        .and_then(read_u32)
                        .map(|v| v.min(255) as u8),
                    pwm: read_u32(&c.pwm_path).map(|v| v.min(255) as u8),
                })
                .collect(),
            vendor: inv.msi_ec.as_ref().map(|ec| VendorSnapshot {
                fan_mode_path: ec.fan_mode_path.clone(),
                fan_mode: ec.fan_mode_path.as_deref().and_then(|p| read_trim(Path::new(p))),
                cooler_boost_path: ec.cooler_boost_path.clone(),
                cooler_boost: ec
                    .cooler_boost_path
                    .as_deref()
                    .and_then(|p| read_trim(Path::new(p))),
            }),
        }
    }

    /// The plan that puts everything back exactly as captured — and, where a
    /// prior value is unknown, back to firmware control (never to a stopped
    /// fan). See [`Action::FanSafeRestore`].
    pub fn plan_restore(&self) -> Vec<Action> {
        let mut actions = Vec::new();
        for c in &self.controls {
            actions.push(Action::FanSafeRestore {
                enable_path: c.enable_path.clone(),
                pwm_path: Some(c.pwm_path.clone()),
                prior_enable: c.enable,
                prior_pwm: c.pwm,
            });
        }
        if let Some(v) = &self.vendor {
            if let Some(path) = &v.cooler_boost_path {
                actions.push(Action::FanVendorAttr {
                    path: path.clone(),
                    value: v.cooler_boost.clone().unwrap_or_else(|| "off".to_string()),
                    what: "msi-ec cooler_boost".to_string(),
                });
            }
            if let Some(path) = &v.fan_mode_path {
                actions.push(Action::FanVendorAttr {
                    path: path.clone(),
                    value: v.fan_mode.clone().unwrap_or_else(|| "auto".to_string()),
                    what: "msi-ec fan_mode".to_string(),
                });
            }
        }
        actions
    }
}

/// The unconditional "hand everything back to the firmware" plan, used when no
/// snapshot exists — `rime fan restore --local` after a daemon crash, and the
/// `ExecStopPost=` hook. It needs no prior state and cannot leave a fan off.
pub fn plan_firmware_restore(inv: &FanInventory) -> Vec<Action> {
    let mut actions = Vec::new();
    for c in &inv.controls {
        actions.push(Action::FanSafeRestore {
            enable_path: c.enable_path.clone(),
            pwm_path: Some(c.pwm_path.clone()),
            prior_enable: None,
            prior_pwm: None,
        });
    }
    if let Some(ec) = &inv.msi_ec {
        if let Some(path) = &ec.cooler_boost_path {
            actions.push(Action::FanVendorAttr {
                path: path.clone(),
                value: "off".to_string(),
                what: "msi-ec cooler_boost".to_string(),
            });
        }
        if let Some(path) = &ec.fan_mode_path {
            actions.push(Action::FanVendorAttr {
                path: path.clone(),
                value: "auto".to_string(),
                what: "msi-ec fan_mode".to_string(),
            });
        }
    }
    actions
}

/// Discover the machine's fans and hand every one of them back to firmware
/// control, right now, through `writer`. This is the daemon-less safety path —
/// `rime fan restore --local`, and therefore the `ExecStopPost=` hook that runs
/// after a crash, when no in-process restore could possibly have happened.
/// Returns how many actions were attempted.
///
/// A fan whose `pwm*_enable` already reads `2` (firmware automatic) is skipped:
/// that is the state the ladder's second rung writes, so there is nothing to hand
/// back. Some drivers refuse every write while the firmware owns the fan
/// (thinkpad_acpi without `fan_control=1`), and asking them anyway printed two
/// "Operation not permitted" lines at every shutdown of a machine whose fan Rime
/// had never touched.
pub fn restore_to_firmware(
    sys_root: &Path,
    cfg: &FanConfig,
    writer: &dyn crate::syswriter::SysWriter,
) -> usize {
    let mut inv = FanInventory::discover(sys_root, cfg);
    inv.controls.retain(|c| {
        c.enable_path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map_or(true, |v| v.trim() != "2")
    });
    let plan = plan_firmware_restore(&inv);
    for a in &plan {
        if let Err(e) = writer.apply(a) {
            eprintln!("rime: fan restore action failed ({}): {e:#}", a.describe());
        }
    }
    plan.len()
}

/// The plan for a requested mode. `Curve` plans nothing by itself: the daemon
/// evaluates the curve and issues `Manual` plans on a cadence.
pub fn plan_mode(inv: &FanInventory, cfg: &FanConfig, mode: FanMode) -> Vec<Action> {
    let mut actions = Vec::new();
    match mode {
        FanMode::Auto => {
            for c in &inv.controls {
                // Ask for firmware control; the safe-restore ladder covers
                // drivers that refuse `2`.
                actions.push(Action::FanSafeRestore {
                    enable_path: c.enable_path.clone(),
                    pwm_path: Some(c.pwm_path.clone()),
                    prior_enable: Some(2),
                    prior_pwm: None,
                });
            }
            if let Some(ec) = &inv.msi_ec {
                if let Some(path) = &ec.cooler_boost_path {
                    actions.push(Action::FanVendorAttr {
                        path: path.clone(),
                        value: "off".into(),
                        what: "msi-ec cooler_boost".into(),
                    });
                }
                if let Some(path) = &ec.fan_mode_path {
                    actions.push(Action::FanVendorAttr {
                        path: path.clone(),
                        value: vendor_mode(ec, &cfg.msi_ec_auto_mode, "auto"),
                        what: "msi-ec fan_mode".into(),
                    });
                }
            }
        }
        FanMode::Max => {
            for c in &inv.controls {
                if let Some(enable) = &c.enable_path {
                    actions.push(Action::FanPwmEnable {
                        path: enable.clone(),
                        value: 1,
                    });
                }
                actions.push(Action::FanPwm {
                    path: c.pwm_path.clone(),
                    value: 255,
                });
            }
            if let Some(ec) = &inv.msi_ec {
                // msi-ec has no duty cycle: "max" is cooler boost.
                if let Some(path) = &ec.fan_mode_path {
                    actions.push(Action::FanVendorAttr {
                        path: path.clone(),
                        value: vendor_mode(ec, &cfg.msi_ec_max_mode, "advanced"),
                        what: "msi-ec fan_mode".into(),
                    });
                }
                if let Some(path) = &ec.cooler_boost_path {
                    actions.push(Action::FanVendorAttr {
                        path: path.clone(),
                        value: "on".into(),
                        what: "msi-ec cooler_boost".into(),
                    });
                }
            }
        }
        FanMode::Manual(pwm) => {
            let pwm = pwm.clamp(cfg.min_pwm, cfg.max_pwm.max(cfg.min_pwm));
            for c in &inv.controls {
                if let Some(enable) = &c.enable_path {
                    actions.push(Action::FanPwmEnable {
                        path: enable.clone(),
                        value: 1,
                    });
                }
                actions.push(Action::FanPwm {
                    path: c.pwm_path.clone(),
                    value: pwm,
                });
            }
            if let Some(ec) = &inv.msi_ec {
                // Best available approximation of a duty cycle on msi-ec:
                // boost above the threshold, firmware control below it.
                if let Some(path) = &ec.cooler_boost_path {
                    actions.push(Action::FanVendorAttr {
                        path: path.clone(),
                        value: if pwm >= cfg.boost_pwm_threshold { "on" } else { "off" }.into(),
                        what: "msi-ec cooler_boost".into(),
                    });
                }
            }
        }
        FanMode::Curve => {}
    }
    actions
}

/// Pick a vendor mode string, preferring the profile's choice but only if the
/// EC actually advertises it.
fn vendor_mode(ec: &MsiEc, preferred: &Option<String>, fallback: &str) -> String {
    match preferred {
        Some(m)
            if ec.available_modes.is_empty()
                || ec.available_modes.iter().any(|a| a.eq_ignore_ascii_case(m)) =>
        {
            m.clone()
        }
        _ => {
            if ec.available_modes.is_empty()
                || ec.available_modes.iter().any(|a| a.eq_ignore_ascii_case(fallback))
            {
                fallback.to_string()
            } else {
                "auto".to_string()
            }
        }
    }
}

/// Evaluate a fan curve: linear interpolation between points, clamped to the
/// profile's `min_pwm`/`max_pwm` floor and ceiling. An empty curve yields the
/// floor, never zero.
pub fn curve_pwm(points: &[CurvePoint], temp_c: f64, min_pwm: u8, max_pwm: u8) -> u8 {
    let ceiling = max_pwm.max(min_pwm);
    if points.is_empty() {
        return min_pwm;
    }
    let mut pts: Vec<&CurvePoint> = points.iter().collect();
    pts.sort_by(|a, b| a.temp_c.partial_cmp(&b.temp_c).unwrap_or(std::cmp::Ordering::Equal));
    if temp_c <= pts[0].temp_c {
        return pts[0].pwm.clamp(min_pwm, ceiling);
    }
    if temp_c >= pts[pts.len() - 1].temp_c {
        return pts[pts.len() - 1].pwm.clamp(min_pwm, ceiling);
    }
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if temp_c >= a.temp_c && temp_c <= b.temp_c {
            let span = b.temp_c - a.temp_c;
            let t = if span <= 0.0 {
                0.0
            } else {
                (temp_c - a.temp_c) / span
            };
            let pwm = a.pwm as f64 + t * (b.pwm as f64 - a.pwm as f64);
            return (pwm.round().clamp(0.0, 255.0) as u8).clamp(min_pwm, ceiling);
        }
    }
    min_pwm
}

/// The hottest CPU-ish temperature in degrees Celsius, for curve evaluation.
/// Prefers well-known package sensors, then any hwmon temperature, then the
/// thermal zones. Read-only; `None` when nothing is readable.
pub fn read_curve_temp(sys_root: &Path) -> Option<f64> {
    const PREFERRED: [&str; 5] = ["coretemp", "k10temp", "zenpower", "msi-ec", "acpitz"];
    let mut best: Option<f64> = None;
    let mut preferred_best: Option<f64> = None;

    if let Ok(entries) = std::fs::read_dir(sys_root.join("class/hwmon")) {
        for e in entries.flatten() {
            let dir = e.path();
            let chip = read_trim(&dir.join("name")).unwrap_or_default();
            for n in 1..=16u32 {
                let p = dir.join(format!("temp{n}_input"));
                if !p.exists() {
                    continue;
                }
                let Some(milli) = read_trim(&p).and_then(|s| s.parse::<f64>().ok()) else {
                    continue;
                };
                let c = milli / 1000.0;
                if !(0.0..=150.0).contains(&c) {
                    continue;
                }
                if PREFERRED.contains(&chip.as_str()) {
                    preferred_best = Some(preferred_best.map_or(c, |b: f64| b.max(c)));
                }
                best = Some(best.map_or(c, |b: f64| b.max(c)));
            }
        }
    }
    if preferred_best.is_some() {
        return preferred_best;
    }
    if best.is_some() {
        return best;
    }

    // Fall back to the thermal zones.
    if let Ok(entries) = std::fs::read_dir(sys_root.join("class/thermal")) {
        for e in entries.flatten() {
            let p = e.path().join("temp");
            if let Some(milli) = read_trim(&p).and_then(|s| s.parse::<f64>().ok()) {
                let c = milli / 1000.0;
                if (0.0..=150.0).contains(&c) {
                    best = Some(best.map_or(c, |b: f64| b.max(c)));
                }
            }
        }
    }
    best
}
