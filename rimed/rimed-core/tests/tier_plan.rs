//! Tier-engine tests: (profile, tier) -> [Action], transitions, proof that
//! MockWriter records exactly the plan while RealWriter's dry-run gate performs
//! zero writes — and, since the universal-hardware pass, proof that the writer
//! adapts a plan to whatever knobs the running kernel actually advertises
//! instead of failing on hardware it was not written for.

use std::path::{Path, PathBuf};

use rimed_core::profile::ProfileSet;
use rimed_core::syswriter::{MockWriter, RealWriter, SysWriter};
use rimed_core::tier::{Action, Tier};

fn set() -> ProfileSet {
    ProfileSet::builtin()
}

/// A scratch sysfs tree. Nothing here ever touches the real `/sys`.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "rimed-tier-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    fn write(&self, rel: &str, contents: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, contents).unwrap();
    }

    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.root.join(rel))
            .ok()
            .map(|s| s.trim().to_string())
    }

    fn writer(&self) -> RealWriter {
        RealWriter::with_root(false, &self.root)
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

// ── the tier table itself ────────────────────────────────────────────────────

#[test]
fn there_are_exactly_three_tiers_and_they_are_universal() {
    // The removed `ultra` / `ultra-max` tiers existed to drive a RyzenAdj
    // EC-defeat path that only worked on one specific laptop. What remains must
    // be expressible on any machine.
    assert_eq!(Tier::ALL.len(), 3);
    assert_eq!(
        Tier::all_ids(),
        vec!["performance", "balanced", "power-saver"]
    );
    assert!("ultra".parse::<Tier>().is_err(), "ultra must not resolve");
    assert!(
        "ultra-max".parse::<Tier>().is_err(),
        "ultra-max must not resolve"
    );
    // Round-trip every surviving ID.
    for t in Tier::ALL {
        assert_eq!(t.as_str().parse::<Tier>().unwrap(), t);
    }
}

#[test]
fn no_shipped_profile_can_emit_a_vendor_power_limit_action() {
    // Every action a tier plans is one of the three portable knobs, or the
    // kernel's own RAPL powercap limits (CpuPowerLimit / CpuPowerFirmware).
    // Nothing may reintroduce a vendor power-limit tool (the removed RyzenAdj
    // EC-defeat path) through a profile, and only a DEVICE profile may hold
    // CPU power limits at all: only it knows the cooling they assume.
    for (id, _) in rimed_core::profile::BUILTIN_PROFILE_TOML {
        let s = set();
        let p = s.get(id).unwrap();
        for tier in Tier::ALL {
            for a in p.plan_tier(tier) {
                assert!(
                    matches!(
                        a,
                        Action::Governor(_)
                            | Action::Epp(_)
                            | Action::PlatformProfile(_)
                            | Action::CpuPowerLimit { .. }
                            | Action::CpuPowerFirmware
                    ),
                    "{id}/{tier} planned a non-portable action: {a:?}"
                );
                if matches!(a, Action::CpuPowerLimit { .. }) {
                    assert_eq!(
                        p.kind,
                        rimed_core::profile::ProfileKind::Device,
                        "{id}/{tier}: only a device profile may hold CPU power limits"
                    );
                }
            }
        }
    }
}

#[test]
fn amd_zen_performance_is_the_top_of_the_table() {
    let s = set();
    let p = s.get("amd-zen").unwrap();
    assert_eq!(
        p.plan_tier(Tier::Performance),
        vec![
            Action::Governor("performance".into()),
            Action::Epp("performance".into()),
            Action::PlatformProfile("performance".into()),
        ]
    );
    assert_eq!(
        p.plan_tier(Tier::PowerSaver),
        vec![
            Action::Governor("powersave".into()),
            Action::Epp("power".into()),
            Action::PlatformProfile("low-power".into()),
        ]
    );
}

#[test]
fn a_transition_is_just_the_target_tier() {
    // Tiers are pure state assertions: there is nothing left over from the
    // previous tier that has to be torn down.
    let s = set();
    let p = s.get("thinkpad-l16-g2").unwrap();
    for from in [None, Some(Tier::Performance), Some(Tier::PowerSaver)] {
        assert_eq!(
            p.plan_transition(from, Tier::Balanced),
            p.plan_tier(Tier::Balanced),
            "transition from {from:?} should equal the target plan"
        );
    }
}

#[test]
fn msi_katana_omits_platform_profile() {
    let s = set();
    let p = s.get("msi-katana-gf76").unwrap();
    assert_eq!(
        p.plan_tier(Tier::Performance),
        vec![
            Action::Governor("performance".into()),
            Action::Epp("performance".into()),
            Action::CpuPowerLimit { pl1_w: 45, pl2_w: 65 },
        ]
    );
    // Its AC default is the top tier, which every machine can honour.
    assert_eq!(p.defaults.ac, Tier::Performance);
}

#[test]
fn charge_windows_carry_no_sysfs_paths() {
    // A profile declares a window; which battery honours it is discovered at
    // runtime. The device profiles are the only ones that declare a window at
    // all — a generic profile must not impose one on unknown hardware.
    let s = set();
    assert_eq!(
        s.get("thinkpad-l16-g2").unwrap().charge_window(),
        Some((75, 80))
    );
    assert_eq!(
        s.get("msi-katana-gf76").unwrap().charge_window(),
        Some((60, 80))
    );
    for id in ["amd-zen", "intel-hybrid", "generic-laptop", "generic-desktop"] {
        assert_eq!(
            s.get(id).unwrap().charge_window(),
            None,
            "{id} must not impose a charge window"
        );
    }
    // And no profile source names a battery.
    for (id, toml) in rimed_core::profile::BUILTIN_PROFILE_TOML {
        assert!(
            !toml.contains("start_path") && !toml.contains("end_path"),
            "{id} still hard-codes a charge threshold path"
        );
    }
}

// ── the writer: adapt to the machine, never abort on it ──────────────────────

#[test]
fn a_governor_the_driver_does_not_offer_falls_back_to_one_it_does() {
    // An ARM/`cpufreq-dt` style kernel with no `powersave` governor built in.
    // The old writer pushed the string at the driver and turned the resulting
    // EINVAL into a hard error that aborted the whole plan.
    let f = Fixture::new("gov-ladder");
    f.write(
        "devices/system/cpu/cpufreq/policy0/scaling_available_governors",
        "performance schedutil\n",
    );
    f.write("devices/system/cpu/cpufreq/policy0/scaling_governor", "performance");

    let w = f.writer();
    w.apply(&Action::Governor("powersave".into())).unwrap();
    assert_eq!(
        f.read("devices/system/cpu/cpufreq/policy0/scaling_governor").as_deref(),
        Some("schedutil"),
        "powersave is unavailable, so the nearest offered governor is used"
    );

    // The requested value still wins when it is on offer.
    w.apply(&Action::Governor("performance".into())).unwrap();
    assert_eq!(
        f.read("devices/system/cpu/cpufreq/policy0/scaling_governor").as_deref(),
        Some("performance")
    );
}

#[test]
fn a_driver_that_publishes_no_governor_list_gets_the_value_verbatim() {
    let f = Fixture::new("gov-nolist");
    f.write("devices/system/cpu/cpufreq/policy0/scaling_governor", "ondemand");
    f.writer()
        .apply(&Action::Governor("powersave".into()))
        .unwrap();
    assert_eq!(
        f.read("devices/system/cpu/cpufreq/policy0/scaling_governor").as_deref(),
        Some("powersave")
    );
}

#[test]
fn epp_is_skipped_where_the_attribute_does_not_exist() {
    // A plain acpi-cpufreq machine: a governor and nothing else. Planning EPP
    // must not fail, and must not create the attribute.
    let f = Fixture::new("no-epp");
    f.write("devices/system/cpu/cpufreq/policy0/scaling_governor", "performance");
    let w = f.writer();
    assert!(w.apply(&Action::Epp("balance_power".into())).is_ok());
    assert!(
        f.read("devices/system/cpu/cpufreq/policy0/energy_performance_preference").is_none(),
        "a skip must not conjure the attribute"
    );
}

#[test]
fn epp_maps_onto_the_preferences_the_driver_advertises() {
    let f = Fixture::new("epp-ladder");
    let base = "devices/system/cpu/cpufreq/policy0";
    f.write(
        &format!("{base}/energy_performance_available_preferences"),
        "default performance\n",
    );
    f.write(&format!("{base}/energy_performance_preference"), "default");
    f.writer()
        .apply(&Action::Epp("balance_power".into()))
        .unwrap();
    assert_eq!(
        f.read(&format!("{base}/energy_performance_preference")).as_deref(),
        Some("default"),
        "balance_power is unavailable; `default` is the ladder's landing spot"
    );
}

#[test]
fn every_cpufreq_policy_is_written_not_just_the_first() {
    // A hybrid machine presents one policy per core cluster.
    let f = Fixture::new("all-policies");
    for p in ["policy0", "policy12"] {
        f.write(
            &format!("devices/system/cpu/cpufreq/{p}/scaling_governor"),
            "powersave",
        );
    }
    f.writer()
        .apply(&Action::Governor("performance".into()))
        .unwrap();
    for p in ["policy0", "policy12"] {
        assert_eq!(
            f.read(&format!("devices/system/cpu/cpufreq/{p}/scaling_governor")).as_deref(),
            Some("performance"),
            "{p} was not written"
        );
    }
}

#[test]
fn per_cpu_cpufreq_directories_are_found_when_there_are_no_policy_dirs() {
    // Older kernels and some ARM setups expose `cpuN/cpufreq` and no
    // `cpufreq/policyN` at all.
    let f = Fixture::new("percpu-cpufreq");
    f.write("devices/system/cpu/online", "0-1");
    for cpu in [0, 1] {
        f.write(
            &format!("devices/system/cpu/cpu{cpu}/cpufreq/scaling_governor"),
            "powersave",
        );
    }
    f.writer()
        .apply(&Action::Governor("performance".into()))
        .unwrap();
    for cpu in [0, 1] {
        assert_eq!(
            f.read(&format!("devices/system/cpu/cpu{cpu}/cpufreq/scaling_governor")).as_deref(),
            Some("performance")
        );
    }
}

#[test]
fn a_machine_with_no_cpufreq_at_all_is_not_an_error() {
    // A VM with no scaling driver. Every tier action must still succeed.
    let f = Fixture::new("no-cpufreq");
    let w = f.writer();
    for tier in Tier::ALL {
        for a in set().get("generic-desktop").unwrap().plan_tier(tier) {
            assert!(w.apply(&a).is_ok(), "{a:?} failed on a machine with no cpufreq");
        }
    }
}

#[test]
fn platform_profile_maps_onto_the_firmwares_own_vocabulary() {
    // Firmware that spells the frugal end `quiet` rather than `low-power`.
    let f = Fixture::new("pp-ladder");
    f.write(
        "firmware/acpi/platform_profile_choices",
        "quiet balanced balanced-performance performance\n",
    );
    f.write("firmware/acpi/platform_profile", "balanced");
    let w = f.writer();

    w.apply(&Action::PlatformProfile("low-power".into())).unwrap();
    assert_eq!(
        f.read("firmware/acpi/platform_profile").as_deref(),
        Some("quiet"),
        "low-power has no literal match; quiet is its synonym here"
    );

    w.apply(&Action::PlatformProfile("performance".into())).unwrap();
    assert_eq!(
        f.read("firmware/acpi/platform_profile").as_deref(),
        Some("performance")
    );
}

#[test]
fn an_absent_platform_profile_is_a_skip_not_a_failure() {
    let f = Fixture::new("pp-absent");
    assert!(f
        .writer()
        .apply(&Action::PlatformProfile("performance".into()))
        .is_ok());
    assert!(f.read("firmware/acpi/platform_profile").is_none());
}

#[test]
fn a_whole_tier_plan_survives_a_driver_that_refuses_one_knob() {
    // intel_pstate in active mode refuses an EPP write while the `performance`
    // governor is selected. The refusal must not abort the platform_profile
    // write that follows it in the same plan.
    let f = Fixture::new("partial-refusal");
    let base = "devices/system/cpu/cpufreq/policy0";
    f.write(&format!("{base}/scaling_available_governors"), "performance powersave");
    f.write(&format!("{base}/scaling_governor"), "powersave");
    // EPP exists but only offers a value our ladder cannot reach.
    f.write(&format!("{base}/energy_performance_available_preferences"), "nonsense");
    f.write(&format!("{base}/energy_performance_preference"), "nonsense");
    f.write("firmware/acpi/platform_profile_choices", "low-power balanced performance");
    f.write("firmware/acpi/platform_profile", "balanced");

    let w = f.writer();
    let plan = set().get("generic-laptop").unwrap().plan_tier(Tier::Performance);
    assert!(w.apply_all(&plan).is_ok(), "one refused knob must not abort the plan");
    assert_eq!(f.read(&format!("{base}/scaling_governor")).as_deref(), Some("performance"));
    assert_eq!(
        f.read(&format!("{base}/energy_performance_preference")).as_deref(),
        Some("nonsense"),
        "the unreachable EPP value was left alone"
    );
    assert_eq!(
        f.read("firmware/acpi/platform_profile").as_deref(),
        Some("performance"),
        "the action after the skipped one still ran"
    );
}

// ── dry-run and the mock ─────────────────────────────────────────────────────

#[test]
fn mock_writer_records_plan_and_writes_nothing_real() {
    let s = set();
    let p = s.get("thinkpad-l16-g2").unwrap();
    let plan = p.plan_tier(Tier::Performance);
    let mock = MockWriter::new();
    mock.apply_all(&plan).unwrap();
    assert_eq!(mock.recorded(), plan);
    assert!(!mock.is_live());
}

#[test]
fn real_writer_dry_run_does_not_write_fixture_sysfs() {
    // Build a fake sysfs tree in a temp dir (NOT real /sys) and prove the
    // dry-run gate leaves it untouched, then that a live writer does write it.
    let f = Fixture::new("dry-run");
    f.write("devices/system/cpu/cpufreq/policy0/scaling_governor", "powersave");
    let gov = "devices/system/cpu/cpufreq/policy0/scaling_governor";

    let dry = RealWriter::with_root(true, f.path());
    assert!(!dry.is_live());
    dry.apply(&Action::Governor("performance".into())).unwrap();
    assert_eq!(f.read(gov).as_deref(), Some("powersave"));

    let live = f.writer();
    assert!(live.is_live());
    live.apply(&Action::Governor("performance".into())).unwrap();
    assert_eq!(f.read(gov).as_deref(), Some("performance"));
}
