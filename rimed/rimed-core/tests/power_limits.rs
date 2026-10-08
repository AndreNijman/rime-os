//! CPU package power limits held by a tier (`cpu_power_limit_w`): what the
//! profile plans, and what the writer does to the RAPL zones.
//!
//! The writer here never runs host commands (`RealWriter::with_root`), so the
//! thermald half is logged and skipped; the sysfs half is exercised in full
//! against a fixture powercap tree.

use std::fs;
use std::path::{Path, PathBuf};

use rimed_core::profile::{Profile, ProfileSet};
use rimed_core::syswriter::{Outcome, RealWriter, SysWriter};
use rimed_core::tier::{Action, Tier};

const W: u64 = 1_000_000;

struct Fixture(PathBuf);
impl Fixture {
    fn new(tag: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!("rimed-power-{tag}-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).unwrap();
        Fixture(root)
    }
    fn zone(&self, name: &str, pl1_w: u64, pl2_w: u64) -> PathBuf {
        let d = self.0.join("class/powercap").join(name);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("name"), "package-0\n").unwrap();
        fs::write(d.join("constraint_0_power_limit_uw"), format!("{}\n", pl1_w * W)).unwrap();
        fs::write(d.join("constraint_1_power_limit_uw"), format!("{}\n", pl2_w * W)).unwrap();
        d
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

fn read_w(dir: &Path, attr: &str) -> u64 {
    fs::read_to_string(dir.join(attr)).unwrap().trim().parse::<u64>().unwrap() / W
}

fn katana() -> Profile {
    ProfileSet::load(None).unwrap().get("msi-katana-gf76").unwrap().clone()
}

#[test]
fn katana_holds_45_65_on_performance_and_hands_back_on_the_other_tiers() {
    let p = katana();
    assert!(p
        .plan_tier(Tier::Performance)
        .contains(&Action::CpuPowerLimit { pl1_w: 45, pl2_w: 65 }));
    for t in [Tier::Balanced, Tier::PowerSaver] {
        let plan = p.plan_tier(t);
        assert!(plan.contains(&Action::CpuPowerFirmware), "{t}: {plan:?}");
        assert!(!plan.iter().any(|a| matches!(a, Action::CpuPowerLimit { .. })));
    }
}

#[test]
fn a_profile_that_holds_no_limits_plans_exactly_what_it_did() {
    // Every shipped profile but katana's: no power action in any tier, so
    // their tier plans are byte-for-byte what they were.
    let set = ProfileSet::load(None).unwrap();
    for id in ["intel-hybrid", "amd-zen", "generic-laptop", "generic-desktop", "thinkpad-l16-g2"] {
        let p = set.get(id).unwrap();
        for t in [Tier::Performance, Tier::Balanced, Tier::PowerSaver] {
            assert!(
                !p.plan_tier(t)
                    .iter()
                    .any(|a| matches!(a, Action::CpuPowerLimit { .. } | Action::CpuPowerFirmware)),
                "{id}/{t}"
            );
        }
    }
}

#[test]
fn nonsense_limits_are_refused_at_load() {
    let base = |v: &str| {
        format!(
            r#"
            id = "t"
            kind = "device"
            [defaults]
            ac = "performance"
            battery = "balanced"
            [tiers.performance]
            cpu_power_limit_w = {v}
            [tiers.balanced]
            [tiers.power-saver]
            "#
        )
    };
    assert!(Profile::from_toml(&base("[45, 90]")).is_ok());
    for bad in ["[90, 45]", "[0, 90]", "[45, 900]", "[3, 4]"] {
        assert!(Profile::from_toml(&base(bad)).is_err(), "{bad} must be refused");
    }
}

#[test]
fn the_writer_sets_the_mmio_zone_and_only_raises_the_msr_zone() {
    let f = Fixture::new("set");
    let mmio = f.zone("intel-rapl-mmio:0", 38, 45);
    let msr = f.zone("intel-rapl:0", 50, 200); // katana's MSR zone
    let w = RealWriter::with_root(false, &f.0);

    assert_eq!(w.apply(&Action::CpuPowerLimit { pl1_w: 45, pl2_w: 90 }).unwrap(), Outcome::Landed);
    assert_eq!((read_w(&mmio, "constraint_0_power_limit_uw"), read_w(&mmio, "constraint_1_power_limit_uw")), (45, 90));
    // MSR already allows more than asked: untouched (it never binds lower).
    assert_eq!((read_w(&msr, "constraint_0_power_limit_uw"), read_w(&msr, "constraint_1_power_limit_uw")), (50, 200));

    // A higher request raises the MSR zone's PL1 too, and handing back
    // restores it exactly.
    assert_eq!(w.apply(&Action::CpuPowerLimit { pl1_w: 60, pl2_w: 115 }).unwrap(), Outcome::Landed);
    assert_eq!(read_w(&msr, "constraint_0_power_limit_uw"), 60);
    assert_eq!(read_w(&msr, "constraint_1_power_limit_uw"), 200);
    assert_eq!(w.apply(&Action::CpuPowerFirmware).unwrap(), Outcome::Landed);
    assert_eq!(read_w(&msr, "constraint_0_power_limit_uw"), 50);
}

#[test]
fn a_machine_without_rapl_refuses_and_writes_nothing() {
    let f = Fixture::new("none");
    let w = RealWriter::with_root(false, &f.0);
    assert!(matches!(
        w.apply(&Action::CpuPowerLimit { pl1_w: 45, pl2_w: 90 }).unwrap(),
        Outcome::Refused(_)
    ));
    // Handing back with nothing held is a no-op that landed.
    assert_eq!(w.apply(&Action::CpuPowerFirmware).unwrap(), Outcome::Landed);
}

#[test]
fn a_zone_that_is_not_the_package_is_left_alone() {
    let f = Fixture::new("notpkg");
    let d = f.zone("intel-rapl-mmio:0", 38, 45);
    fs::write(d.join("name"), "psys\n").unwrap();
    let w = RealWriter::with_root(false, &f.0);
    assert!(matches!(w.apply(&Action::CpuPowerLimit { pl1_w: 45, pl2_w: 90 }).unwrap(), Outcome::Refused(_)));
    assert_eq!(read_w(&d, "constraint_0_power_limit_uw"), 38);
}
