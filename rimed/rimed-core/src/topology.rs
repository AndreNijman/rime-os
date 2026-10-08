//! CPU topology: which logical CPUs are performance cores and which are
//! efficiency cores.
//!
//! There is no single blessed sysfs attribute for this on x86, so detection is
//! a **ladder** of interfaces, most authoritative first, and the rung that
//! answered is recorded in [`CoreTopology::source`] so `rime game status` and
//! the notes can say *how* the split was determined:
//!
//! 1. `/sys/devices/cpu_core/cpus` + `/sys/devices/cpu_atom/cpus` — the hybrid
//!    perf PMUs Alder Lake+ kernels export. This is the same signal
//!    [`crate::fingerprint`] already uses to set `CpuInfo::hybrid`, and on an
//!    i7-12700H it reads `0-11` (6 P-cores × 2 threads) and `12-19` (8 E-cores).
//! 2. `/sys/devices/system/cpu/types/*/cpulist|cpumap` — the newer per-core-type
//!    directories (`intel_core_*` / `intel_atom_*`).
//! 3. `/sys/devices/system/cpu/cpu*/cpu_capacity` — capacity-aware scheduling.
//! 4. `/sys/devices/system/cpu/cpu*/acpi_cppc/highest_perf` — CPPC.
//! 5. `/sys/devices/system/cpu/cpu*/cpufreq/cpuinfo_max_freq` — the crudest
//!    signal, and only trusted when the spread is large (>=15%).
//!
//! Everything here is read-only and takes an explicit sysfs root so it can be
//! pointed at a fixture.

use std::collections::BTreeMap;
use std::path::Path;

/// Which rung of the detection ladder produced the split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreSource {
    /// `/sys/devices/cpu_core/cpus` + `/sys/devices/cpu_atom/cpus`.
    HybridPmu,
    /// `/sys/devices/system/cpu/types/*`.
    CpuTypes,
    /// `cpu_capacity`.
    Capacity,
    /// `acpi_cppc/highest_perf`.
    Cppc,
    /// `cpufreq/cpuinfo_max_freq`.
    MaxFreq,
    /// No hybrid split found — every CPU is treated as a performance core.
    Uniform,
    /// No CPUs could be enumerated at all (no sysfs / fixture empty).
    Unknown,
}

impl CoreSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            CoreSource::HybridPmu => "hybrid-pmu",
            CoreSource::CpuTypes => "cpu-types",
            CoreSource::Capacity => "cpu-capacity",
            CoreSource::Cppc => "acpi-cppc",
            CoreSource::MaxFreq => "max-freq",
            CoreSource::Uniform => "uniform",
            CoreSource::Unknown => "unknown",
        }
    }
}

/// The P/E split for a machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreTopology {
    /// Every online logical CPU, ascending.
    pub all: Vec<u32>,
    /// Performance cores (all of `all` on a uniform machine).
    pub pcores: Vec<u32>,
    /// Efficiency cores (empty on a uniform machine).
    pub ecores: Vec<u32>,
    /// Which rung of the ladder answered.
    pub source: CoreSource,
}

impl CoreTopology {
    /// True when a genuine P/E split was found.
    pub fn is_hybrid(&self) -> bool {
        !self.ecores.is_empty() && !self.pcores.is_empty()
    }

    /// True when the CPU has genuinely different kinds of core, as the kernel
    /// reports them: Intel's P/E split (the `cpu_core`/`cpu_atom` PMUs, or the
    /// `types` directory) or differing `cpu_capacity` (Arm big.LITTLE).
    ///
    /// Stricter than [`is_hybrid`](Self::is_hybrid) on purpose. The CPPC and
    /// max-frequency rungs also split CPUs that are all the same core and are
    /// only ranked: AMD's preferred cores. Measured on a Ryzen 7 PRO 250 (eight
    /// identical Zen 5 cores): `cpu_capacity` 1024 on every CPU, and
    /// `acpi_cppc/highest_perf` 196-232, which `is_hybrid` reads as four
    /// P-cores and twelve E-cores.
    pub fn has_efficiency_cores(&self) -> bool {
        self.is_hybrid()
            && matches!(
                self.source,
                CoreSource::HybridPmu | CoreSource::CpuTypes | CoreSource::Capacity
            )
    }

    /// `0-11` style rendering of the performance cores.
    pub fn pcore_list(&self) -> String {
        format_cpu_list(&self.pcores)
    }

    /// `12-19` style rendering of the efficiency cores.
    pub fn ecore_list(&self) -> String {
        format_cpu_list(&self.ecores)
    }

    /// Every CPU that is *not* in `cpus`, in ascending order. Used to park IRQs
    /// away from the cores a game is pinned to.
    pub fn complement(&self, cpus: &[u32]) -> Vec<u32> {
        self.all.iter().copied().filter(|c| !cpus.contains(c)).collect()
    }

    /// Detect from the live machine. Read-only.
    pub fn detect() -> CoreTopology {
        Self::detect_from(Path::new("/sys"))
    }

    /// Detect using an explicit sysfs root (fixtures/tests).
    pub fn detect_from(sys_root: &Path) -> CoreTopology {
        let all = online_cpus(sys_root);
        if all.is_empty() {
            return CoreTopology {
                all,
                pcores: Vec::new(),
                ecores: Vec::new(),
                source: CoreSource::Unknown,
            };
        }

        for (source, split) in [
            (CoreSource::HybridPmu, hybrid_pmu_split(sys_root)),
            (CoreSource::CpuTypes, cpu_types_split(sys_root)),
            (CoreSource::Capacity, attr_split(sys_root, &all, "cpu_capacity", 1.0)),
            (
                CoreSource::Cppc,
                attr_split(sys_root, &all, "acpi_cppc/highest_perf", 1.0),
            ),
            (
                CoreSource::MaxFreq,
                attr_split(sys_root, &all, "cpufreq/cpuinfo_max_freq", 1.15),
            ),
        ] {
            if let Some((pcores, ecores)) = split {
                // Only accept a split that covers CPUs we actually see online.
                let pcores: Vec<u32> = pcores.into_iter().filter(|c| all.contains(c)).collect();
                let ecores: Vec<u32> = ecores.into_iter().filter(|c| all.contains(c)).collect();
                if !pcores.is_empty() && !ecores.is_empty() {
                    return CoreTopology {
                        all,
                        pcores,
                        ecores,
                        source,
                    };
                }
            }
        }

        CoreTopology {
            pcores: all.clone(),
            ecores: Vec::new(),
            all,
            source: CoreSource::Uniform,
        }
    }
}

/// Parse a kernel CPU list (`0-11,16,18-19`) into individual CPU numbers.
pub fn parse_cpu_list(s: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for part in s.trim().split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.split_once('-') {
            Some((a, b)) => {
                if let (Ok(a), Ok(b)) = (a.trim().parse::<u32>(), b.trim().parse::<u32>()) {
                    for c in a..=b.max(a) {
                        out.push(c);
                    }
                }
            }
            None => {
                if let Ok(c) = part.parse::<u32>() {
                    out.push(c);
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Render CPU numbers as a compact kernel CPU list (`0-11,16`).
pub fn format_cpu_list(cpus: &[u32]) -> String {
    let mut cpus: Vec<u32> = cpus.to_vec();
    cpus.sort_unstable();
    cpus.dedup();
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < cpus.len() {
        let start = cpus[i];
        let mut end = start;
        while i + 1 < cpus.len() && cpus[i + 1] == end + 1 {
            i += 1;
            end = cpus[i];
        }
        parts.push(if start == end {
            start.to_string()
        } else {
            format!("{start}-{end}")
        });
        i += 1;
    }
    parts.join(",")
}

/// Parse a kernel cpumap (`00000000,000fffff`) into CPU numbers.
pub fn parse_cpu_mask(s: &str) -> Vec<u32> {
    let mut out = Vec::new();
    // Groups are 32-bit, most significant first.
    let groups: Vec<&str> = s.trim().split(',').collect();
    for (gi, group) in groups.iter().rev().enumerate() {
        let Ok(bits) = u32::from_str_radix(group.trim(), 16) else {
            continue;
        };
        for b in 0..32u32 {
            if bits & (1 << b) != 0 {
                out.push(gi as u32 * 32 + b);
            }
        }
    }
    out.sort_unstable();
    out
}

fn read_trim(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Every online logical CPU. Prefers `cpu/online`, falls back to enumerating
/// `cpu[0-9]+` directories.
pub fn online_cpus(sys_root: &Path) -> Vec<u32> {
    let base = sys_root.join("devices/system/cpu");
    if let Some(list) = read_trim(&base.join("online")) {
        let cpus = parse_cpu_list(&list);
        if !cpus.is_empty() {
            return cpus;
        }
    }
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&base) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(rest) = name.strip_prefix("cpu") {
                if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                    if let Ok(n) = rest.parse::<u32>() {
                        out.push(n);
                    }
                }
            }
        }
    }
    out.sort_unstable();
    out
}

/// Rung 1: the hybrid perf PMUs.
fn hybrid_pmu_split(sys_root: &Path) -> Option<(Vec<u32>, Vec<u32>)> {
    let p = read_trim(&sys_root.join("devices/cpu_core/cpus"))?;
    let e = read_trim(&sys_root.join("devices/cpu_atom/cpus"))?;
    Some((parse_cpu_list(&p), parse_cpu_list(&e)))
}

/// Rung 2: `/sys/devices/system/cpu/types/{intel_core_*,intel_atom_*}`.
fn cpu_types_split(sys_root: &Path) -> Option<(Vec<u32>, Vec<u32>)> {
    let dir = sys_root.join("devices/system/cpu/types");
    let entries = std::fs::read_dir(&dir).ok()?;
    let mut pcores = Vec::new();
    let mut ecores = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let cpus = read_trim(&e.path().join("cpulist"))
            .map(|s| parse_cpu_list(&s))
            .or_else(|| read_trim(&e.path().join("cpumap")).map(|s| parse_cpu_mask(&s)))
            .unwrap_or_default();
        if name.contains("atom") {
            ecores.extend(cpus);
        } else if name.contains("core") {
            pcores.extend(cpus);
        }
    }
    if pcores.is_empty() && ecores.is_empty() {
        return None;
    }
    pcores.sort_unstable();
    ecores.sort_unstable();
    Some((pcores, ecores))
}

/// Rungs 3-5: group CPUs by a numeric per-CPU attribute; the higher group is
/// the performance set. `min_ratio` guards against calling a machine hybrid
/// because of a trivial spread (bin-specific turbo tables, say).
fn attr_split(
    sys_root: &Path,
    all: &[u32],
    rel: &str,
    min_ratio: f64,
) -> Option<(Vec<u32>, Vec<u32>)> {
    let base = sys_root.join("devices/system/cpu");
    let mut values: BTreeMap<u64, Vec<u32>> = BTreeMap::new();
    for cpu in all {
        let v = read_trim(&base.join(format!("cpu{cpu}")).join(rel))?
            .parse::<u64>()
            .ok()?;
        values.entry(v).or_default().push(*cpu);
    }
    if values.len() < 2 {
        return None;
    }
    let lowest = *values.keys().next()?;
    let highest = *values.keys().next_back()?;
    if lowest == 0 || (highest as f64) / (lowest as f64) < min_ratio {
        return None;
    }
    // Everything at the top value is a P-core; everything below is an E-core.
    // (Alder Lake reports exactly two values; a three-value machine — e.g. a
    // favoured-core bin — still lands the fastest set in `pcores`.)
    let mut pcores = Vec::new();
    let mut ecores = Vec::new();
    for (v, cpus) in &values {
        if *v == highest {
            pcores.extend(cpus.iter().copied());
        } else {
            ecores.extend(cpus.iter().copied());
        }
    }
    pcores.sort_unstable();
    ecores.sort_unstable();
    Some((pcores, ecores))
}
