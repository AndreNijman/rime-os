//! Assertions for the Performance Lab readers (roadmap §12).
//!
//! Fixture-rooted throughout, and read-only by construction: nothing here
//! constructs a writer, and the NVIDIA legs go through the injected querier so
//! no process is ever spawned.
//!
//! The most important case in this file is the one asserting that frame time
//! stays UNAVAILABLE. §12 lists frame time and Rime cannot measure it; a lab
//! that quietly substitutes GPU busy percentage would be showing a confident
//! number it did not measure, which is worse than an honest gap.

use std::fs;
use std::path::PathBuf;

use rimed_core::gpu::MockNvidiaSmi;
use rimed_core::perf::{
    parse_pp_dpm, read_battery_watts, read_cpu_clocks, read_frame_time, read_game_cpuset,
    read_gpu_busy, read_gpu_clock, read_platform_profile, read_policy_attr, read_power_sources,
    read_scheduler, read_temps, snapshot,
};
use rimed_core::workload::Roots;

struct Fixture(PathBuf);

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "rimed-perf-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).unwrap();
        Fixture(root)
    }

    fn write(&self, rel: &str, contents: &str) -> &Fixture {
        let p = self.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, contents).unwrap();
        self
    }

    fn sys(&self) -> PathBuf {
        self.0.join("sys")
    }

    fn roots(&self) -> Roots {
        Roots {
            sys: self.sys(),
            proc: self.0.join("proc"),
            game_cgroup: self.0.join("cgroup/rime-game"),
        }
    }
}

fn no_gpu() -> MockNvidiaSmi {
    MockNvidiaSmi::default()
}

// ── frame time: the deliberate gap ───────────────────────────────────────────

#[test]
fn frame_time_is_unavailable_and_explains_itself() {
    let s = read_frame_time();
    assert!(
        !s.is_measured(),
        "Rime cannot measure frame pacing; claiming otherwise would be a fabricated number"
    );
    let reason = s.reason().unwrap();
    // The reason must be actionable, not just a shrug.
    assert!(reason.contains("MangoHud"), "{reason}");
    assert!(reason.contains("swapchain"), "{reason}");
}

#[test]
fn a_fully_populated_machine_still_reports_no_frame_time() {
    // The failure mode this guards: someone wires GPU busy percentage or a
    // clock reading into the frame-time row because the field looked empty.
    let f = Fixture::new("nosubstitute");
    f.write("sys/class/drm/card1/device/gpu_busy_percent", "97\n")
        .write("sys/class/drm/card1/device/pp_dpm_sclk", "0: 2700Mhz *\n");
    let snap = snapshot(&f.roots(), &no_gpu());
    assert_eq!(snap.gpu.busy_percent.value(), Some(&97.0));
    assert!(
        !snap.frame_time.is_measured(),
        "a busy GPU is not a frame-time measurement"
    );
}

// ── CPU ──────────────────────────────────────────────────────────────────────

#[test]
fn cpu_clocks_report_every_policy_not_just_an_average() {
    // On a hybrid machine one average describes neither core type, which is
    // exactly the asymmetry someone opens the lab to see.
    let f = Fixture::new("clocks");
    f.write("sys/devices/system/cpu/cpufreq/policy0/scaling_cur_freq", "4200000\n")
        .write("sys/devices/system/cpu/cpufreq/policy4/scaling_cur_freq", "2800000\n")
        .write("sys/devices/system/cpu/cpufreq/policy8/scaling_cur_freq", "1000000\n");
    let c = read_cpu_clocks(&f.sys());
    let c = c.value().expect("policies publish a frequency");
    assert_eq!(c.min_mhz, 1000);
    assert_eq!(c.max_mhz, 4200);
    assert_eq!(c.mean_mhz, 2666);
    assert_eq!(
        c.per_policy,
        vec![
            ("policy0".to_string(), 4200),
            ("policy4".to_string(), 2800),
            ("policy8".to_string(), 1000),
        ]
    );
}

#[test]
fn a_machine_with_no_scaling_driver_says_so() {
    let f = Fixture::new("nocpufreq");
    f.write("sys/devices/system/cpu/online", "0-3\n");
    let c = read_cpu_clocks(&f.sys());
    assert!(!c.is_measured());
    assert!(c.reason().unwrap().contains("scaling driver"));
}

#[test]
fn disagreeing_policies_are_reported_as_mixed_not_as_the_first_one() {
    // A hybrid machine can carry a different EPP on its P and E policies.
    // Printing whichever sorted first would hide a genuine asymmetry.
    let f = Fixture::new("mixed");
    f.write(
        "sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference",
        "performance\n",
    )
    .write(
        "sys/devices/system/cpu/cpufreq/policy8/energy_performance_preference",
        "balance_power\n",
    );
    let v = read_policy_attr(&f.sys(), "energy_performance_preference");
    let v = v.value().unwrap();
    assert!(v.starts_with("mixed:"), "{v}");
    assert!(v.contains("performance") && v.contains("balance_power"), "{v}");

    // Agreeing policies collapse to the single value, with no "mixed:" noise.
    let g = Fixture::new("agree");
    g.write("sys/devices/system/cpu/cpufreq/policy0/scaling_governor", "powersave\n")
        .write("sys/devices/system/cpu/cpufreq/policy4/scaling_governor", "powersave\n");
    assert_eq!(
        read_policy_attr(&g.sys(), "scaling_governor").value(),
        Some(&"powersave".to_string())
    );
}

// ── GPU ──────────────────────────────────────────────────────────────────────

#[test]
fn the_amdgpu_dpm_table_yields_the_active_level_not_the_ceiling() {
    // Taking the highest row would report the card's maximum as though it were
    // the live clock — a number that is always wrong and always plausible.
    let table = "0: 800Mhz *\n1: 1100Mhz \n2: 2700Mhz\n";
    assert_eq!(parse_pp_dpm(table), Some(800));
    // The star can sit on any row.
    assert_eq!(parse_pp_dpm("0: 800Mhz\n1: 1100Mhz\n2: 2700Mhz *\n"), Some(2700));
    // No active marker at all: no reading, rather than a guess.
    assert_eq!(parse_pp_dpm("0: 800Mhz\n1: 1100Mhz\n"), None);
    assert_eq!(parse_pp_dpm(""), None);
    // The level index must never be mistaken for a frequency.
    assert_eq!(parse_pp_dpm("3: 1500Mhz *\n"), Some(1500));
}

#[test]
fn gpu_readings_skip_the_drm_connector_symlinks() {
    let f = Fixture::new("gpu");
    f.write("sys/class/drm/card1/device/pp_dpm_sclk", "0: 800Mhz\n1: 2700Mhz *\n")
        .write("sys/class/drm/card1/device/gpu_busy_percent", "42\n")
        // Connectors carry no engine state and must not be walked as cards.
        .write("sys/class/drm/card1-eDP-1/status", "connected\n")
        .write("sys/class/drm/card1-HDMI-A-1/status", "disconnected\n");
    assert_eq!(read_gpu_clock(&f.sys(), &no_gpu()).value(), Some(&2700));
    assert_eq!(read_gpu_busy(&f.sys(), &no_gpu()).value(), Some(&42.0));
}

#[test]
fn nvidia_clocks_come_from_the_querier_and_never_from_a_spawn() {
    let f = Fixture::new("nvclock");
    let smi = MockNvidiaSmi {
        available: true,
        clocks: vec![(0, 1980, 7000)],
        ..Default::default()
    };
    let c = read_gpu_clock(&f.sys(), &smi);
    assert_eq!(c.value(), Some(&1980));
    assert!(c.source().contains("nvidia-smi"));
}

#[test]
fn a_machine_with_no_readable_gpu_clock_names_all_three_interfaces() {
    let f = Fixture::new("nogpu");
    let c = read_gpu_clock(&f.sys(), &no_gpu());
    assert!(!c.is_measured());
    let r = c.reason().unwrap();
    for want in ["pp_dpm_sclk", "gt_cur_freq_mhz", "nvidia-smi"] {
        assert!(r.contains(want), "{r} does not mention {want}");
    }
}

// ── power and temperature ────────────────────────────────────────────────────

#[test]
fn battery_power_falls_back_to_current_times_voltage() {
    // Many drivers report charge rather than energy, so `power_now` is absent
    // and the same figure has to be derived.
    let f = Fixture::new("battwatts");
    f.write("sys/class/power_supply/BAT0/type", "Battery\n")
        .write("sys/class/power_supply/BAT0/current_now", "1500000\n")
        .write("sys/class/power_supply/BAT0/voltage_now", "12000000\n");
    let w = read_battery_watts(&f.sys());
    assert_eq!(w.value(), Some(&18.0));

    // power_now wins where it exists, and a negative (charging) reading is
    // reported as a magnitude rather than a negative wattage.
    let g = Fixture::new("battnow");
    g.write("sys/class/power_supply/BAT0/type", "Battery\n")
        .write("sys/class/power_supply/BAT0/power_now", "-9500000\n");
    assert_eq!(read_battery_watts(&g.sys()).value(), Some(&9.5));
}

#[test]
fn the_batterys_own_hwmon_is_never_reported_as_chip_power() {
    // A REAL BUG, found by running `rime perf` on the development ThinkPad.
    // The reader took the first hwmon publishing `power1_*`, which there is
    // hwmon4 — owned by BAT0. The lab printed "package: 20.47 W" above a
    // battery row showing the identical figure from the identical sensor.
    // Both numbers were real; the word "package" was invented.
    //
    // The discriminator is `device/type`, the same attribute the AC detection
    // keys on: a hwmon hanging off a power_supply has one, a chip does not.
    let f = Fixture::new("battshadow");
    // The battery's hwmon sorts FIRST, exactly as on the real machine.
    f.write("sys/class/hwmon/hwmon0/name", "BAT0\n")
        .write("sys/class/hwmon/hwmon0/device/type", "Battery\n")
        .write("sys/class/hwmon/hwmon0/power1_input", "20472000\n")
        // The mains adapter, likewise skipped.
        .write("sys/class/hwmon/hwmon1/name", "AC\n")
        .write("sys/class/hwmon/hwmon1/device/type", "Mains\n")
        .write("sys/class/hwmon/hwmon1/power1_input", "45000000\n")
        // The actual chip sensor.
        .write("sys/class/hwmon/hwmon9/name", "amdgpu\n")
        .write("sys/class/hwmon/hwmon9/power1_average", "10000000\n")
        .write("sys/class/hwmon/hwmon9/power1_label", "PPT\n");

    let p = read_power_sources(&f.sys());
    let sources = p.value().expect("the amdgpu sensor is readable");
    let names: Vec<String> = sources.iter().map(|r| r.name()).collect();
    assert_eq!(names, vec!["amdgpu/PPT".to_string()], "{names:?}");
    assert_eq!(sources[0].watts, 10.0);
    assert!(
        !names.iter().any(|n| n.contains("BAT") || n.contains("AC")),
        "a power-supply sensor leaked into the chip power list: {names:?}"
    );
}

#[test]
fn a_power_sensor_without_a_label_still_names_its_chip() {
    // The figure is only meaningful alongside which sensor produced it, so a
    // reading may never be anonymous.
    let f = Fixture::new("nolabel");
    f.write("sys/class/hwmon/hwmon0/name", "power_meter\n")
        .write("sys/class/hwmon/hwmon0/power1_average", "35500000\n");
    let p = read_power_sources(&f.sys());
    let s = p.value().unwrap();
    assert_eq!(s[0].name(), "power_meter");
    assert_eq!(s[0].watts, 35.5);
}

#[test]
fn a_machine_whose_only_power_sensor_is_the_battery_reports_a_gap() {
    // Not zero watts, and not the battery's figure under another name.
    let f = Fixture::new("onlybatt");
    f.write("sys/class/hwmon/hwmon0/name", "BAT0\n")
        .write("sys/class/hwmon/hwmon0/device/type", "Battery\n")
        .write("sys/class/hwmon/hwmon0/power1_input", "20472000\n");
    let p = read_power_sources(&f.sys());
    assert!(!p.is_measured());
    assert!(p.reason().unwrap().contains("power supplies"));
}

#[test]
fn a_desktop_with_no_battery_reports_a_gap() {
    let f = Fixture::new("nobatt");
    f.write("sys/class/power_supply/AC/type", "Mains\n");
    assert!(!read_battery_watts(&f.sys()).is_measured());
}

#[test]
fn temperatures_come_from_both_thermal_zones_and_hwmon() {
    // The ACPI zones frequently miss the chip sensors (k10temp, amdgpu, nvme)
    // that matter most when diagnosing a thermal problem.
    let f = Fixture::new("temps");
    f.write("sys/class/thermal/thermal_zone0/type", "acpitz\n")
        .write("sys/class/thermal/thermal_zone0/temp", "45000\n")
        .write("sys/class/hwmon/hwmon2/name", "k10temp\n")
        .write("sys/class/hwmon/hwmon2/temp1_label", "Tctl\n")
        .write("sys/class/hwmon/hwmon2/temp1_input", "61500\n");
    let t = read_temps(&f.sys());
    let t = t.value().unwrap();
    let names: Vec<&str> = t.iter().map(|x| x.name.as_str()).collect();
    assert!(names.contains(&"acpitz"), "{names:?}");
    assert!(names.contains(&"k10temp/Tctl"), "{names:?}");
    assert_eq!(t.iter().find(|x| x.name == "acpitz").unwrap().celsius, 45.0);
    assert_eq!(
        t.iter().find(|x| x.name == "k10temp/Tctl").unwrap().celsius,
        61.5
    );
}

// ── scheduler ────────────────────────────────────────────────────────────────

#[test]
fn sched_ext_state_is_read_verbatim_and_its_absence_is_named() {
    let f = Fixture::new("scx");
    f.write("sys/kernel/sched_ext/state", "disabled\n")
        .write("sys/kernel/sched_ext/nr_rejected", "0\n");
    let s = read_scheduler(&f.sys());
    let s = s.value().unwrap();
    assert_eq!(s.sched_ext, "disabled");
    // This kernel publishes no scheduler name; that must be None, never a
    // value inferred from the enable state.
    assert_eq!(s.scheduler, None);
    assert_eq!(s.rejected, Some(0));

    let bare = Fixture::new("noscx");
    let s = read_scheduler(&bare.sys());
    assert!(!s.is_measured());
    assert!(s.reason().unwrap().contains("CONFIG_SCHED_CLASS_EXT"));
}

#[test]
fn a_loaded_scx_scheduler_is_named_when_the_kernel_publishes_it() {
    let f = Fixture::new("scxon");
    f.write("sys/kernel/sched_ext/state", "enabled\n")
        .write("sys/kernel/sched_ext/root/ops", "scx_lavd\n")
        .write("sys/kernel/sched_ext/nr_rejected", "3\n");
    let s = read_scheduler(&f.sys());
    let s = s.value().unwrap();
    assert_eq!(s.scheduler.as_deref(), Some("scx_lavd"));
    assert_eq!(s.rejected, Some(3));
}

#[test]
fn a_refused_sched_ext_state_read_is_not_diagnosed_as_missing_kernel_support() {
    // `state` exists at 0444 on a kernel that ships sched_ext, so a genuinely
    // refused read is a confined environment rather than an absent feature —
    // and `read_trim`'s `.ok()` could not tell that apart from ENOENT, so the
    // Unavailable reason told the reader to go check CONFIG_SCHED_CLASS_EXT
    // for a problem that was never that.
    let f = Fixture::new("scx-eacces");
    f.write("sys/kernel/sched_ext/state", "disabled\n");
    let state = f.sys().join("kernel/sched_ext/state");
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&state).unwrap().permissions();
    perms.set_mode(0o000);
    fs::set_permissions(&state, perms).unwrap();
    let sealed = matches!(
        fs::read_to_string(&state),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied
    );

    let s = read_scheduler(&f.sys());

    let mut perms = fs::metadata(&state).unwrap().permissions();
    perms.set_mode(0o644);
    fs::set_permissions(&state, perms).ok();

    if !sealed {
        return; // the caller overrides the mode bit; it proves nothing here
    }
    assert!(!s.is_measured());
    assert!(
        !s.reason().unwrap_or("").contains("CONFIG_SCHED_CLASS_EXT"),
        "a refused read must not be diagnosed as missing kernel support, got {:?}",
        s.reason()
    );
}

// ── the game cpuset ──────────────────────────────────────────────────────────

#[test]
fn the_game_cpuset_is_reported_when_a_session_confines_something() {
    let f = Fixture::new("cpuset");
    f.write("cgroup/rime-game/cpuset.cpus.effective", "0-5,12\n");
    assert_eq!(
        read_game_cpuset(&f.roots()).value(),
        Some(&vec![0, 1, 2, 3, 4, 5, 12])
    );

    let idle = Fixture::new("nocpuset");
    let s = read_game_cpuset(&idle.roots());
    assert!(!s.is_measured());
    assert!(s.reason().unwrap().contains("no game session"));
}

// ── platform profile ─────────────────────────────────────────────────────────

#[test]
fn the_platform_profile_falls_back_to_the_msi_ec_shift_mode() {
    // No ACPI platform_profile, msi-ec bound: rimed drives shift_mode there,
    // so that is what status reports, with its own path as the source.
    let f = Fixture::new("pp-msi");
    f.write("sys/devices/platform/msi-ec/shift_mode", "turbo\n");
    let s = read_platform_profile(&f.sys());
    assert_eq!(s.value().map(String::as_str), Some("turbo"));
    assert!(s.source().ends_with("devices/platform/msi-ec/shift_mode"));

    // Where both exist, ACPI is the answer.
    f.write("sys/firmware/acpi/platform_profile", "balanced\n");
    let s = read_platform_profile(&f.sys());
    assert_eq!(s.value().map(String::as_str), Some("balanced"));
    assert!(s.source().ends_with("firmware/acpi/platform_profile"));
}

// ── the whole snapshot ───────────────────────────────────────────────────────

#[test]
fn an_empty_machine_produces_a_snapshot_of_gaps_rather_than_zeroes() {
    // A Performance Lab full of confident zeroes is worse than one that says
    // what it could not read. Every field must degrade to `unavailable`.
    let f = Fixture::new("bare");
    let s = snapshot(&f.roots(), &no_gpu());
    assert!(!s.cpu.clocks.is_measured());
    assert!(!s.cpu.governor.is_measured());
    assert!(!s.cpu.platform_profile.is_measured());
    assert!(!s.gpu.clock_mhz.is_measured());
    assert!(!s.gpu.busy_percent.is_measured());
    assert!(!s.gpu.vram.is_measured());
    assert!(!s.power_sources.is_measured());
    assert!(!s.battery_watts.is_measured());
    assert!(!s.temps.is_measured());
    assert!(!s.scheduler.is_measured());
    assert!(!s.frame_time.is_measured());
    // …and every one of them explains itself.
    for (what, reason) in [
        ("cpu clocks", s.cpu.clocks.reason()),
        ("governor", s.cpu.governor.reason()),
        ("gpu clock", s.gpu.clock_mhz.reason()),
        ("vram", s.gpu.vram.reason()),
        ("power", s.power_sources.reason()),
        ("temps", s.temps.reason()),
        ("scheduler", s.scheduler.reason()),
        ("frame time", s.frame_time.reason()),
    ] {
        let r = reason.unwrap_or("");
        assert!(r.len() > 15, "{what} gives an unhelpful reason: {r:?}");
    }
}

#[test]
fn every_reading_names_the_path_it_came_from() {
    let f = Fixture::new("sources");
    f.write("sys/devices/system/cpu/cpufreq/policy0/scaling_cur_freq", "3000000\n")
        .write("sys/devices/system/cpu/cpufreq/policy0/scaling_governor", "schedutil\n")
        .write("sys/class/thermal/thermal_zone0/type", "acpitz\n")
        .write("sys/class/thermal/thermal_zone0/temp", "40000\n")
        .write("sys/kernel/sched_ext/state", "disabled\n");
    let s = snapshot(&f.roots(), &no_gpu());
    let root = f.0.to_string_lossy().to_string();
    for (what, src) in [
        ("cpu clocks", s.cpu.clocks.source()),
        ("governor", s.cpu.governor.source()),
        ("temps", s.temps.source()),
        ("scheduler", s.scheduler.source()),
    ] {
        assert!(
            src.starts_with(&root),
            "{what} source {src:?} escapes the fixture root"
        );
    }
}
