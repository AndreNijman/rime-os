//! The Performance Lab (roadmap §12): "frame time, CPU/GPU clocks, power,
//! temperatures, VRAM and scheduler state".
//!
//! Read-only, root-free, and built on the same [`Signal`] vocabulary as
//! [`crate::workload`] — every reading carries the path it came from, and a
//! reading this hardware cannot produce says so and why.
//!
//! ## Frame time is reported as unavailable, and nothing stands in for it
//!
//! §12 asks for frame time and Rime cannot measure it. There is no generic
//! kernel or compositor interface that reports a running application's frame
//! pacing: it is a property of a client's swapchain, visible to the client, to
//! an interposed layer such as MangoHud, or to a compositor that chooses to
//! export it. Wayland exposes presentation feedback to the *client*, not to a
//! bystander asking sysfs.
//!
//! So this module reports `frame_time: unavailable`, names the reason, and says
//! how to obtain a real measurement. It deliberately does NOT substitute GPU
//! busy percentage, a clock reading or a frame *rate* derived from anything
//! else. Those correlate with frame pacing and are not frame pacing, and §12
//! and §13 are both explicit that measured signals are the point. A Performance
//! Lab that displays a confident number it did not measure is worse than one
//! with an honest gap.

use std::path::{Path, PathBuf};

use crate::gpu::{self, GpuVendor, NvidiaSmi};
use crate::topology::parse_cpu_list;
use crate::workload::{read_pressure, read_vram, Roots, Signal, Vram};

/// CPU frequencies across every cpufreq policy, in MHz.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuClocks {
    pub min_mhz: u64,
    pub max_mhz: u64,
    pub mean_mhz: u64,
    /// `(policy name, MHz)` for every policy, so a hybrid machine's P and E
    /// cores are visible individually rather than averaged into a number that
    /// describes neither.
    pub per_policy: Vec<(String, u64)>,
}

/// What the CPU side of the lab reports.
#[derive(Debug, Clone)]
pub struct CpuPerf {
    pub clocks: Signal<CpuClocks>,
    /// `scaling_governor`, or every distinct value when policies disagree.
    pub governor: Signal<String>,
    /// `energy_performance_preference`, same treatment.
    pub epp: Signal<String>,
    pub platform_profile: Signal<String>,
    /// PSI `some avg10`, the measured stall time.
    pub pressure: Signal<f64>,
}

/// One GPU, named, with what THAT card can answer.
///
/// The lab used to have exactly one set of GPU readings, filled from whichever
/// DRM card answered first. On a hybrid laptop that is the iGPU: on the MSI
/// Katana, `card1` is Alder Lake-P Iris Xe and `card2` is the RTX 3070, so the
/// lab reported the Intel chip's clock, called it "GPU", and never mentioned
/// the card the games run on. Naming them is the fix; there is no way to be
/// honest about a two-GPU machine with one row.
#[derive(Debug, Clone)]
pub struct GpuDevicePerf {
    /// `card1`, as under `/sys/class/drm`.
    pub card: String,
    /// "AMD", "Intel", "NVIDIA", or the raw PCI id.
    pub vendor: String,
    /// The bound kernel driver, when one is readable.
    pub driver: Option<String>,
    /// The firmware's display adapter — the iGPU, on a hybrid laptop.
    pub boot_vga: bool,
    pub clock_mhz: Signal<u64>,
    pub busy_percent: Signal<f64>,
}

/// What the GPU side reports.
#[derive(Debug, Clone)]
pub struct GpuPerf {
    /// The headline figures, taken from [`gpu::primary`] rather than from the
    /// lowest-numbered card.
    pub clock_mhz: Signal<u64>,
    pub busy_percent: Signal<f64>,
    pub vram: Signal<Vram>,
    /// Every GPU, so a hybrid machine is legible.
    pub devices: Vec<GpuDevicePerf>,
}

/// One named temperature reading.
#[derive(Debug, Clone, PartialEq)]
pub struct Temp {
    pub name: String,
    pub celsius: f64,
}

/// Scheduler state — the sched-ext half of §12.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerState {
    /// `/sys/kernel/sched_ext/state`, verbatim (`disabled`, `enabled`, …).
    pub sched_ext: String,
    /// The loaded scx scheduler's name, when the kernel publishes one.
    pub scheduler: Option<String>,
    /// `nr_rejected`: tasks the BPF scheduler declined. Non-zero is worth
    /// seeing — it is the signal that a scheduler is misbehaving under load.
    pub rejected: Option<u64>,
}

/// A whole reading of the machine.
#[derive(Debug, Clone)]
pub struct PerfSnapshot {
    pub cpu: CpuPerf,
    pub gpu: GpuPerf,
    /// Every hwmon power sensor, each naming the chip it came from. Not a
    /// single "package power" figure — see [`read_power_sources`] for why.
    pub power_sources: Signal<Vec<PowerReading>>,
    /// Battery power draw, watts. Negative would be charging, so only the
    /// discharge magnitude is reported.
    pub battery_watts: Signal<f64>,
    pub temps: Signal<Vec<Temp>>,
    pub scheduler: Signal<SchedulerState>,
    /// Always unavailable. See the module docs — this is a deliberate,
    /// documented gap rather than a missing implementation.
    pub frame_time: Signal<f64>,
}

fn read_trim(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// Every cpufreq policy directory, preferring the per-policy form and falling
/// back to the per-CPU links older kernels present. Mirrors the writer's own
/// discovery so the lab reports the same policies the tier engine writes.
fn cpufreq_policies(sys: &Path) -> Vec<PathBuf> {
    let base = sys.join("devices/system/cpu/cpufreq");
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
        for cpu in crate::topology::online_cpus(sys) {
            let p = sys.join(format!("devices/system/cpu/cpu{cpu}/cpufreq"));
            if p.is_dir() {
                out.push(p);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// A short label for a policy directory (`policy0`, `cpu4`).
fn policy_label(p: &Path) -> String {
    p.file_name()
        .and_then(|s| s.to_str())
        .filter(|s| *s != "cpufreq")
        .map(|s| s.to_string())
        .or_else(|| {
            // The per-CPU fallback is `.../cpu4/cpufreq`, so the useful name is
            // the parent's.
            p.parent()
                .and_then(|q| q.file_name())
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "cpu".to_string())
}

/// Current CPU clocks, in MHz.
///
/// `scaling_cur_freq` is preferred over `cpuinfo_cur_freq`: the latter needs the
/// driver to actually query the hardware and is absent on `intel_pstate` in
/// active mode, where it would silently produce no reading on very common
/// hardware.
pub fn read_cpu_clocks(sys: &Path) -> Signal<CpuClocks> {
    let policies = cpufreq_policies(sys);
    let src = sys
        .join("devices/system/cpu/cpufreq/policy*/scaling_cur_freq")
        .display()
        .to_string();
    if policies.is_empty() {
        return Signal::unavailable("no cpufreq policies (no scaling driver)", src);
    }
    let mut per_policy = Vec::new();
    for p in &policies {
        let khz = read_trim(&p.join("scaling_cur_freq"))
            .or_else(|| read_trim(&p.join("cpuinfo_cur_freq")))
            .and_then(|s| s.parse::<u64>().ok());
        if let Some(khz) = khz {
            per_policy.push((policy_label(p), khz / 1000));
        }
    }
    if per_policy.is_empty() {
        return Signal::unavailable(
            "cpufreq policies exist but publish no current frequency",
            src,
        );
    }
    let values: Vec<u64> = per_policy.iter().map(|(_, v)| *v).collect();
    let sum: u64 = values.iter().sum();
    Signal::measured(
        CpuClocks {
            min_mhz: *values.iter().min().unwrap(),
            max_mhz: *values.iter().max().unwrap(),
            mean_mhz: sum / values.len() as u64,
            per_policy,
        },
        src,
    )
}

/// Read one per-policy attribute, collapsing agreeing policies to one value and
/// listing the distinct values when they disagree.
///
/// Disagreement is a real state, not a bug: a hybrid machine can carry different
/// EPP values on its P and E policies, and reporting only the first would hide
/// exactly the asymmetry someone opens a Performance Lab to see.
pub fn read_policy_attr(sys: &Path, attr: &str) -> Signal<String> {
    let policies = cpufreq_policies(sys);
    let src = sys
        .join(format!("devices/system/cpu/cpufreq/policy*/{attr}"))
        .display()
        .to_string();
    let mut seen: Vec<String> = Vec::new();
    for p in &policies {
        if let Some(v) = read_trim(&p.join(attr)) {
            if !v.is_empty() && !seen.contains(&v) {
                seen.push(v);
            }
        }
    }
    match seen.len() {
        0 => Signal::unavailable(format!("no policy publishes {attr}"), src),
        1 => Signal::measured(seen.remove(0), src),
        _ => Signal::measured(format!("mixed: {}", seen.join(", ")), src),
    }
}

/// The ACPI platform profile, or the MSI EC's `shift_mode` where the firmware
/// has no ACPI interface and rimed drives that instead.
pub fn read_platform_profile(sys: &Path) -> Signal<String> {
    let p = sys.join("firmware/acpi/platform_profile");
    let src = p.display().to_string();
    match read_trim(&p) {
        Some(v) if !v.is_empty() => Signal::measured(v, src),
        _ => {
            let shift = sys.join(crate::fan::MSI_EC_REL).join("shift_mode");
            match read_trim(&shift) {
                Some(v) if !v.is_empty() => Signal::measured(v, shift.display().to_string()),
                _ => Signal::unavailable("this firmware exposes no ACPI platform profile", src),
            }
        }
    }
}

/// The DRM card directories, skipping the `cardN-CONNECTOR` symlinks which
/// carry no engine or memory state.
fn drm_cards(sys: &Path) -> Vec<PathBuf> {
    let base = sys.join("class/drm");
    let mut out: Vec<PathBuf> = match std::fs::read_dir(&base) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.starts_with("card") && !s.contains('-'))
                    .unwrap_or(false)
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort();
    out
}

/// Pull the active frequency out of an amdgpu `pp_dpm_sclk` table.
///
/// The format is one DPM level per line, with the active one marked by a
/// trailing `*`:
///
/// ```text
/// 0: 800Mhz *
/// 1: 1100Mhz
/// 2: 2700Mhz
/// ```
///
/// Only the starred line is current; taking the highest would report the card's
/// ceiling as though it were the live clock.
pub fn parse_pp_dpm(text: &str) -> Option<u64> {
    for line in text.lines() {
        if !line.trim_end().ends_with('*') {
            continue;
        }
        for tok in line.split_whitespace() {
            let t = tok.trim_end_matches('*').trim();
            let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
            // `0:` is the level index, not a frequency; require the MHz suffix.
            if !digits.is_empty() && t.len() > digits.len() {
                let suffix = t[digits.len()..].to_ascii_lowercase();
                if suffix.starts_with("mhz") {
                    return digits.parse().ok();
                }
            }
        }
    }
    None
}

/// Current GPU core clock, in MHz.
pub fn read_gpu_clock(sys: &Path, smi: &dyn NvidiaSmi) -> Signal<u64> {
    let src = sys.join("class/drm/card*/device").display().to_string();
    for card in drm_cards(sys) {
        let dev = card.join("device");
        // amdgpu.
        if let Ok(text) = std::fs::read_to_string(dev.join("pp_dpm_sclk")) {
            if let Some(mhz) = parse_pp_dpm(&text) {
                return Signal::measured(mhz, dev.join("pp_dpm_sclk").display().to_string());
            }
        }
        // i915 (older) and xe/i915 (per-GT).
        for rel in [
            "gt_cur_freq_mhz",
            "gt/gt0/rps_cur_freq_mhz",
            "device/gt_cur_freq_mhz",
        ] {
            let p = card.join(rel);
            if let Some(v) = read_trim(&p).and_then(|s| s.parse::<u64>().ok()) {
                return Signal::measured(v, p.display().to_string());
            }
        }
    }
    if let Some((_, graphics_mhz, _)) = smi.clocks_mhz().first() {
        return Signal::measured(
            *graphics_mhz,
            "nvidia-smi --query-gpu=clocks.current.graphics",
        );
    }
    Signal::unavailable(
        "no GPU on this machine publishes a current core clock (amdgpu uses \
         pp_dpm_sclk, i915/xe gt_cur_freq_mhz, NVIDIA needs nvidia-smi)",
        src,
    )
}

/// GPU utilisation percentage, where the driver reports one.
///
/// This is engine busy time, **not** frame rate and not a frame-time proxy — a
/// GPU can sit at 99% while a game stutters, and at 40% while it runs perfectly.
///
/// Three vendors, three answers, and only one of them is a sysfs file:
///
/// * amdgpu publishes `gpu_busy_percent`.
/// * NVIDIA publishes nothing in sysfs; `nvidia-smi --query-gpu=utilization.gpu`
///   is the only source, and until it was wired in here the lab said "no driver
///   here publishes gpu_busy_percent" on a machine with an RTX 3070 in it.
/// * i915 publishes no busy percentage at all. Engine busy time is available
///   through the i915 PMU (`perf_event_open` on the `i915` PMU, or
///   `intel_gpu_top`), which needs a perf event rather than a file read and is
///   NOT substituted with a clock reading here.
pub fn read_gpu_busy(sys: &Path, smi: &dyn NvidiaSmi) -> Signal<f64> {
    let src = sys
        .join("class/drm/card*/device/gpu_busy_percent")
        .display()
        .to_string();
    for card in drm_cards(sys) {
        let p = card.join("device/gpu_busy_percent");
        if let Some(v) = read_trim(&p).and_then(|s| s.parse::<f64>().ok()) {
            return Signal::measured(v, p.display().to_string());
        }
    }
    if let Some((_, pct)) = smi.utilization().first() {
        return Signal::measured(*pct, "nvidia-smi --query-gpu=utilization.gpu");
    }
    Signal::unavailable(
        "no GPU here publishes a utilisation figure (amdgpu uses gpu_busy_percent, \
         NVIDIA needs nvidia-smi, and i915 exposes engine busy only through its PMU)",
        src,
    )
}

/// Every GPU, each with what that card can answer for itself.
///
/// NVIDIA's readings come from `nvidia-smi`, matched to the DRM card by ORDER
/// among the NVIDIA cards rather than by index: nvidia-smi's index and the DRM
/// card number are two different numbering schemes and there is no published
/// mapping between them. With one NVIDIA GPU — which is every machine this has
/// been run on — order is exact; with two it is a guess, and the field says so
/// rather than the code pretending otherwise.
pub fn read_gpu_devices(sys: &Path, smi: &dyn NvidiaSmi) -> Vec<GpuDevicePerf> {
    let devices = gpu::discover(sys);
    let nv_clocks = smi.clocks_mhz();
    let nv_util = smi.utilization();
    let mut nv_seen = 0usize;

    devices
        .iter()
        .map(|d| {
            let is_nv = d.vendor == GpuVendor::Nvidia;
            let nv_slot = if is_nv {
                let i = nv_seen;
                nv_seen += 1;
                Some(i)
            } else {
                None
            };

            let clock = match &d.clock_path {
                Some(p) if p.ends_with("pp_dpm_sclk") => read_trim(p)
                    .and_then(|t| parse_pp_dpm(&t))
                    .map(|v| Signal::measured(v, p.display().to_string()))
                    .unwrap_or_else(|| {
                        Signal::unavailable(
                            "pp_dpm_sclk lists no active level",
                            p.display().to_string(),
                        )
                    }),
                Some(p) => read_trim(p)
                    .and_then(|t| t.parse::<u64>().ok())
                    .map(|v| Signal::measured(v, p.display().to_string()))
                    .unwrap_or_else(|| {
                        Signal::unavailable("unreadable", p.display().to_string())
                    }),
                None => match nv_slot.and_then(|i| nv_clocks.get(i)) {
                    Some((_, ghz, _)) => Signal::measured(
                        *ghz,
                        "nvidia-smi --query-gpu=clocks.current.graphics".to_string(),
                    ),
                    None => Signal::unavailable(
                        "this driver publishes no current core clock",
                        format!("{}/class/drm/{}", sys.display(), d.card),
                    ),
                },
            };

            let busy = match &d.busy_path {
                Some(p) => read_trim(p)
                    .and_then(|t| t.parse::<f64>().ok())
                    .map(|v| Signal::measured(v, p.display().to_string()))
                    .unwrap_or_else(|| {
                        Signal::unavailable("unreadable", p.display().to_string())
                    }),
                None => match nv_slot.and_then(|i| nv_util.get(i)) {
                    Some((_, pct)) => Signal::measured(
                        *pct,
                        "nvidia-smi --query-gpu=utilization.gpu".to_string(),
                    ),
                    None if d.vendor == GpuVendor::Intel => Signal::unavailable(
                        "i915 exposes engine busy through its PMU, not through sysfs",
                        format!("{}/class/drm/{}", sys.display(), d.card),
                    ),
                    None => Signal::unavailable(
                        "this driver publishes no utilisation figure",
                        format!("{}/class/drm/{}", sys.display(), d.card),
                    ),
                },
            };

            GpuDevicePerf {
                card: d.card.clone(),
                vendor: d.vendor.label().to_string(),
                driver: d.driver.clone(),
                boot_vga: d.boot_vga,
                clock_mhz: clock,
                busy_percent: busy,
            }
        })
        .collect()
}

/// One power sensor, named by the chip that owns it.
#[derive(Debug, Clone, PartialEq)]
pub struct PowerReading {
    /// The hwmon chip `name` (`amdgpu`, `k10temp`, `power_meter`).
    pub chip: String,
    /// The sensor's own label (`PPT`, `SVI2_P_Core`), when it publishes one.
    pub label: Option<String>,
    pub watts: f64,
}

impl PowerReading {
    /// `amdgpu/PPT` — what the reading actually is.
    pub fn name(&self) -> String {
        match &self.label {
            Some(l) => format!("{}/{l}", self.chip),
            None => self.chip.clone(),
        }
    }
}

/// Every hwmon power sensor, each named by its chip and label.
///
/// **This deliberately does not return one number called "package power".** It
/// used to, by taking the first hwmon that published `power1_*` — and on the
/// development ThinkPad that is `hwmon4`, which belongs to `BAT0`. The lab
/// therefore printed the battery's 20.5 W discharge as "package: 20.47 W", with
/// the battery row underneath showing the identical figure from the identical
/// sensor. Both numbers were real and the label was a fabrication.
///
/// So: every source is reported, each says which chip and which sensor it came
/// from, and hwmon devices that hang off a `power_supply` (a battery or a mains
/// adapter) are skipped here because [`read_battery_watts`] already covers them.
/// Whether a given chip's figure is "the package" is a question the reader can
/// now answer from the label, which is more than this function could honestly
/// decide on their behalf.
pub fn read_power_sources(sys: &Path) -> Signal<Vec<PowerReading>> {
    let base = sys.join("class/hwmon");
    let src = base.join("hwmon*/power*_input").display().to_string();
    let mut out: Vec<PowerReading> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&base) {
        let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        dirs.sort();
        for d in dirs {
            // A hwmon hanging off a power_supply is the battery or the charger.
            // `device/type` is how the power_supply class identifies itself, and
            // is the same attribute the AC detection keys on.
            if read_trim(&d.join("device/type")).is_some() {
                continue;
            }
            let chip = read_trim(&d.join("name")).unwrap_or_else(|| "hwmon".to_string());
            for n in 1..=4u32 {
                // `_average` is the smoothed figure where the driver offers it;
                // `_input` is the instantaneous one.
                let uw = read_trim(&d.join(format!("power{n}_average")))
                    .or_else(|| read_trim(&d.join(format!("power{n}_input"))))
                    .and_then(|s| s.parse::<f64>().ok());
                let Some(uw) = uw else { continue };
                out.push(PowerReading {
                    chip: chip.clone(),
                    label: read_trim(&d.join(format!("power{n}_label"))),
                    watts: uw / 1_000_000.0,
                });
            }
        }
    }
    if out.is_empty() {
        return Signal::unavailable(
            "no hwmon device outside the power supplies reports power (RAPL exposes \
             energy counters, which need two samples over an interval rather than \
             one read, so they are not surfaced here)",
            src,
        );
    }
    Signal::measured(out, src)
}

/// Battery discharge in watts.
///
/// `power_now` where the driver has it; otherwise `current_now * voltage_now`,
/// which is how the charge-reporting drivers express the same thing.
pub fn read_battery_watts(sys: &Path) -> Signal<f64> {
    let base = sys.join("class/power_supply");
    let src = base.join("*/power_now").display().to_string();
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Signal::unavailable("no power_supply class", src);
    };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    dirs.sort();
    for d in dirs {
        if read_trim(&d.join("type")).as_deref() != Some("Battery") {
            continue;
        }
        if let Some(uw) = read_trim(&d.join("power_now")).and_then(|s| s.parse::<f64>().ok()) {
            return Signal::measured(uw.abs() / 1_000_000.0, d.join("power_now").display().to_string());
        }
        let ua = read_trim(&d.join("current_now")).and_then(|s| s.parse::<f64>().ok());
        let uv = read_trim(&d.join("voltage_now")).and_then(|s| s.parse::<f64>().ok());
        if let (Some(ua), Some(uv)) = (ua, uv) {
            return Signal::measured(
                (ua.abs() * uv.abs()) / 1e12,
                d.join("current_now").display().to_string(),
            );
        }
    }
    Signal::unavailable("no battery reports power or current", src)
}

/// Thermal zones plus labelled hwmon inputs.
pub fn read_temps(sys: &Path) -> Signal<Vec<Temp>> {
    let src = sys.join("class/thermal/thermal_zone*/temp").display().to_string();
    let mut out: Vec<Temp> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(sys.join("class/thermal")) {
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.starts_with("thermal_zone"))
                    .unwrap_or(false)
            })
            .collect();
        dirs.sort();
        for d in dirs {
            let name = read_trim(&d.join("type")).unwrap_or_else(|| {
                d.file_name().unwrap_or_default().to_string_lossy().to_string()
            });
            if let Some(milli) = read_trim(&d.join("temp")).and_then(|s| s.parse::<f64>().ok()) {
                out.push(Temp {
                    name,
                    celsius: milli / 1000.0,
                });
            }
        }
    }

    // hwmon adds the chip-level sensors (`k10temp`, `amdgpu`, `nvme`) that the
    // ACPI thermal zones frequently do not cover.
    if let Ok(entries) = std::fs::read_dir(sys.join("class/hwmon")) {
        let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        dirs.sort();
        for d in dirs {
            let chip = read_trim(&d.join("name")).unwrap_or_else(|| "hwmon".to_string());
            for n in 1..=8u32 {
                let Some(milli) =
                    read_trim(&d.join(format!("temp{n}_input"))).and_then(|s| s.parse::<f64>().ok())
                else {
                    continue;
                };
                let label = read_trim(&d.join(format!("temp{n}_label")))
                    .unwrap_or_else(|| format!("temp{n}"));
                out.push(Temp {
                    name: format!("{chip}/{label}"),
                    celsius: milli / 1000.0,
                });
            }
        }
    }

    if out.is_empty() {
        return Signal::unavailable("no thermal zone or hwmon temperature found", src);
    }
    Signal::measured(out, src)
}

/// sched-ext state.
///
/// The image ships a CachyOS kernel with `CONFIG_SCHED_CLASS_EXT=y` and sixteen
/// scx schedulers, and game mode is what selects one — so "which scheduler is
/// running" is a first-class thing to be able to see. Read-only: this reports
/// the state, it never switches anything.
pub fn read_scheduler(sys: &Path) -> Signal<SchedulerState> {
    let base = sys.join("kernel/sched_ext");
    let state_path = base.join("state");
    let src = state_path.display().to_string();
    // `read_trim`'s `.ok()` cannot tell "this kernel has no sched_ext" from
    // "sched_ext exists and this read was refused" — both landed on the same
    // Unavailable state, but with the same misdiagnosis: a confined
    // environment or an unusual permission would be told to go check
    // CONFIG_SCHED_CLASS_EXT, which is not what was wrong. `state` is
    // ordinarily world-readable (0444) on a kernel that ships it, so ENOENT
    // is the expected shape of "no support" and everything else gets its own
    // reason instead of borrowing that one.
    let state = match std::fs::read_to_string(&state_path) {
        Ok(s) => s.trim().to_string(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Signal::unavailable(
                "this kernel has no sched_ext support (CONFIG_SCHED_CLASS_EXT)",
                src,
            );
        }
        Err(e) => return Signal::unavailable(format!("{}: {e}", state_path.display()), src),
    };
    // The scheduler's own name lives under `root/` on kernels that publish it;
    // several do not, so its absence is reported as None rather than guessed at
    // from the enable state.
    let scheduler = read_trim(&base.join("root/ops"));
    let rejected = read_trim(&base.join("nr_rejected")).and_then(|s| s.parse::<u64>().ok());
    Signal::measured(
        SchedulerState {
            sched_ext: state,
            scheduler,
            rejected,
        },
        src,
    )
}

/// Frame time — always unavailable, deliberately.
///
/// See the module docs. Kept as a real field returning a real gap so the
/// Performance Lab answers §12's question honestly rather than omitting the row
/// and leaving the reader to wonder whether it was measured.
pub fn read_frame_time() -> Signal<f64> {
    Signal::unavailable(
        "no generic source exists: frame pacing belongs to a client's swapchain, \
         so it is visible to the application, to an interposed layer such as \
         MangoHud, or to a compositor that exports it — not to sysfs. Run a game \
         with `mangohud` (or MANGOHUD=1) for a real per-frame measurement; Rime \
         will not substitute GPU busy percentage for it.",
        "none",
    )
}

/// One complete reading.
pub fn snapshot(roots: &Roots, smi: &dyn NvidiaSmi) -> PerfSnapshot {
    let sys = roots.sys.as_path();
    PerfSnapshot {
        cpu: CpuPerf {
            clocks: read_cpu_clocks(sys),
            governor: read_policy_attr(sys, "scaling_governor"),
            epp: read_policy_attr(sys, "energy_performance_preference"),
            platform_profile: read_platform_profile(sys),
            pressure: read_pressure(roots, "cpu"),
        },
        gpu: {
            let devices = read_gpu_devices(sys, smi);
            // The headline row follows the card that matters, not card order.
            // On a hybrid laptop those are different cards.
            let lead = gpu::primary(&gpu::discover(sys)).map(|d| d.card.clone());
            let leading = lead
                .as_ref()
                .and_then(|c| devices.iter().find(|d| &d.card == c));
            GpuPerf {
                clock_mhz: leading
                    .map(|d| d.clock_mhz.clone())
                    .unwrap_or_else(|| read_gpu_clock(sys, smi)),
                busy_percent: leading
                    .map(|d| d.busy_percent.clone())
                    .unwrap_or_else(|| read_gpu_busy(sys, smi)),
                vram: read_vram(roots, smi),
                devices,
            }
        },
        power_sources: read_power_sources(sys),
        battery_watts: read_battery_watts(sys),
        temps: read_temps(sys),
        scheduler: read_scheduler(sys),
        frame_time: read_frame_time(),
    }
}

/// The CPUs the game cgroup currently confines work to, if a session is live.
/// Part of the lab because "which cores is the game actually on" is the first
/// question when frame pacing looks wrong.
pub fn read_game_cpuset(roots: &Roots) -> Signal<Vec<u32>> {
    let p = roots.game_cgroup.join("cpuset.cpus.effective");
    let src = p.display().to_string();
    for cand in [p, roots.game_cgroup.join("cpuset.cpus")] {
        if let Some(v) = read_trim(&cand) {
            if !v.is_empty() {
                return Signal::measured(parse_cpu_list(&v), cand.display().to_string());
            }
        }
    }
    Signal::unavailable("no game session is confining anything right now", src)
}
