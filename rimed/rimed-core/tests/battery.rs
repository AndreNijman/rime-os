//! Battery discovery against synthetic sysfs fixtures.
//!
//! The rule under test is that **no battery is ever named in code**. These
//! fixtures cover the shapes real machines present: no battery at all, the
//! usual `BAT0`, a `BAT1`-only chassis, two packs, the older attribute
//! spelling, end-only threshold support, and a driver that publishes charge
//! rather than energy. Nothing here touches the real `/sys`.

use std::path::PathBuf;

use rimed_core::battery::{BatteryInventory, ThresholdSupport};
use rimed_core::tier::Action;

struct Sysfs {
    root: PathBuf,
}

impl Sysfs {
    fn new(tag: &str) -> Sysfs {
        let root = std::env::temp_dir().join(format!(
            "rimed-bat-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        Sysfs { root }
    }

    /// Add a power supply of an arbitrary `type` with arbitrary attributes.
    fn supply(&self, name: &str, ty: &str, attrs: &[(&str, &str)]) -> &Sysfs {
        let dir = self.root.join("class/power_supply").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("type"), ty).unwrap();
        for (k, v) in attrs {
            std::fs::write(dir.join(k), v).unwrap();
        }
        self
    }

    fn discover(&self) -> BatteryInventory {
        BatteryInventory::discover(&self.root)
    }
}

impl Drop for Sysfs {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

/// The standard modern threshold pair, ready to splice into an attribute list.
const MODERN_THRESHOLDS: [(&str, &str); 2] = [
    ("charge_control_start_threshold", "70"),
    ("charge_control_end_threshold", "80"),
];

/// Concatenate two attribute lists.
fn attrs<'a>(a: &[(&'a str, &'a str)], b: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    a.iter().chain(b.iter()).copied().collect()
}

#[test]
fn a_desktop_with_no_battery_is_empty_and_not_an_error() {
    let s = Sysfs::new("desktop");
    s.supply("AC", "Mains", &[("online", "1")]);
    let inv = s.discover();
    assert!(inv.is_empty());
    assert_eq!(inv.len(), 0);
    assert!(inv.primary().is_none());
    assert_eq!(inv.threshold_support(), ThresholdSupport::None);
    assert!(!inv.supports_thresholds());
    assert!(inv.plan_thresholds(60, 80).is_empty());
    assert!(inv.energy_uwh().is_none());
    assert_eq!(inv.summary(), "no battery (desktop or VM)");
}

#[test]
fn a_machine_with_no_power_supply_class_at_all_is_empty() {
    // Containers and some VMs have no `class/power_supply` directory.
    let s = Sysfs::new("nopsclass");
    let inv = s.discover();
    assert!(inv.is_empty());
    assert!(!inv.supports_thresholds());
}

#[test]
fn the_ordinary_bat0_laptop_is_discovered_with_both_thresholds() {
    let s = Sysfs::new("bat0");
    s.supply("AC", "Mains", &[("online", "0")]);
    s.supply(
        "BAT0",
        "Battery",
        &attrs(
            &[
                ("capacity", "64"),
                ("status", "Discharging"),
                ("energy_now", "31000000"),
            ],
            &MODERN_THRESHOLDS,
        ),
    );
    let inv = s.discover();
    assert_eq!(inv.names(), vec!["BAT0"]);
    assert_eq!(inv.threshold_support(), ThresholdSupport::StartAndEnd);
    assert_eq!(inv.primary().unwrap().read("capacity").as_deref(), Some("64"));
    assert_eq!(inv.energy_uwh(), Some(31_000_000));

    let plan = inv.plan_thresholds(60, 80);
    assert_eq!(plan.len(), 1);
    match &plan[0] {
        Action::ChargeThresholds { start, stop, start_path, end_path } => {
            assert_eq!((*start, *stop), (60, 80));
            assert!(start_path.as_ref().unwrap().ends_with("BAT0/charge_control_start_threshold"));
            assert!(end_path.as_ref().unwrap().ends_with("BAT0/charge_control_end_threshold"));
        }
        other => panic!("expected ChargeThresholds, got {other:?}"),
    }
}

#[test]
fn a_bat1_only_chassis_needs_no_special_casing() {
    // The MSI Katana presents its pack as BAT1. Discovery finds it because it
    // enumerates by `type`, not by name.
    let s = Sysfs::new("bat1");
    s.supply(
        "BAT1",
        "Battery",
        &attrs(&[("capacity", "91")], &MODERN_THRESHOLDS),
    );
    let inv = s.discover();
    assert_eq!(inv.names(), vec!["BAT1"]);
    assert!(inv.supports_thresholds());
    let plan = inv.plan_thresholds(60, 80);
    assert_eq!(plan.len(), 1);
    assert!(matches!(
        &plan[0],
        Action::ChargeThresholds { end_path: Some(p), .. } if p.contains("BAT1")
    ));
}

#[test]
fn an_unconventionally_named_pack_is_still_a_battery() {
    // Chromebooks and several ARM laptops use names that are not `BAT*`.
    let s = Sysfs::new("cmb0");
    s.supply("CMB0", "Battery", &[("capacity", "50")]);
    s.supply("macsmc-battery", "Battery", &[("capacity", "70")]);
    let inv = s.discover();
    assert_eq!(inv.len(), 2);
    assert_eq!(inv.names(), vec!["CMB0", "macsmc-battery"]);
}

#[test]
fn two_batteries_both_get_the_window() {
    // ThinkPad dual-battery: the plan must cover every pack that supports it.
    let s = Sysfs::new("dual");
    s.supply("BAT0", "Battery", &attrs(&[("capacity", "88")], &MODERN_THRESHOLDS));
    s.supply("BAT1", "Battery", &attrs(&[("capacity", "40")], &MODERN_THRESHOLDS));
    let inv = s.discover();
    assert_eq!(inv.len(), 2);
    let plan = inv.plan_thresholds(75, 80);
    assert_eq!(plan.len(), 2, "both packs are written");
    let paths: Vec<String> = plan
        .iter()
        .map(|a| match a {
            Action::ChargeThresholds { end_path, .. } => end_path.clone().unwrap(),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert!(paths.iter().any(|p| p.contains("BAT0")));
    assert!(paths.iter().any(|p| p.contains("BAT1")));
}

#[test]
fn a_pack_without_thresholds_is_skipped_while_its_sibling_is_written() {
    let s = Sysfs::new("mixed");
    s.supply("BAT0", "Battery", &MODERN_THRESHOLDS);
    s.supply("BAT1", "Battery", &[("capacity", "10")]); // no threshold attrs
    let inv = s.discover();
    assert_eq!(inv.len(), 2);
    assert_eq!(inv.threshold_support(), ThresholdSupport::StartAndEnd);
    assert_eq!(inv.plan_thresholds(75, 80).len(), 1);
}

#[test]
fn end_only_hardware_is_supported_not_rejected() {
    // ASUS and several Dell/LG models expose a stop threshold and no start
    // threshold. That is a real support state, not a failure.
    let s = Sysfs::new("endonly");
    s.supply(
        "BAT0",
        "Battery",
        &[("capacity", "77"), ("charge_control_end_threshold", "80")],
    );
    let inv = s.discover();
    assert_eq!(inv.threshold_support(), ThresholdSupport::EndOnly);
    assert!(inv.supports_thresholds());
    let plan = inv.plan_thresholds(60, 80);
    assert_eq!(plan.len(), 1);
    match &plan[0] {
        Action::ChargeThresholds { start_path, end_path, stop, .. } => {
            assert!(start_path.is_none(), "there is no start threshold to write");
            assert!(end_path.is_some());
            assert_eq!(*stop, 80);
        }
        other => panic!("expected ChargeThresholds, got {other:?}"),
    }
}

#[test]
fn the_older_attribute_spelling_is_also_probed() {
    let s = Sysfs::new("oldspelling");
    s.supply(
        "BAT0",
        "Battery",
        &[
            ("charge_start_threshold", "70"),
            ("charge_stop_threshold", "80"),
        ],
    );
    let inv = s.discover();
    assert_eq!(inv.threshold_support(), ThresholdSupport::StartAndEnd);
    assert!(inv
        .batteries
        .first()
        .unwrap()
        .end_path
        .as_ref()
        .unwrap()
        .ends_with("charge_stop_threshold"));
}

#[test]
fn a_laptop_whose_driver_offers_no_thresholds_reports_unsupported() {
    // The common case, and the one the Katana hits when msi-ec does not bind.
    let s = Sysfs::new("nothresholds");
    s.supply("BAT0", "Battery", &[("capacity", "42"), ("status", "Full")]);
    let inv = s.discover();
    assert!(!inv.is_empty(), "the battery itself is still discovered");
    assert_eq!(inv.threshold_support(), ThresholdSupport::None);
    assert!(!inv.supports_thresholds());
    assert!(
        inv.plan_thresholds(60, 80).is_empty(),
        "nothing is written, and nothing errors"
    );
    assert!(inv.summary().contains("thresholds: none"));
}

#[test]
fn energy_is_derived_from_charge_and_voltage_where_the_driver_reports_no_energy() {
    let s = Sysfs::new("charge-now");
    // 3 000 000 µAh at 11 000 000 µV = 33 000 000 µWh.
    s.supply(
        "BAT0",
        "Battery",
        &[("charge_now", "3000000"), ("voltage_now", "11000000")],
    );
    assert_eq!(s.discover().energy_uwh(), Some(33_000_000));
}

#[test]
fn energy_sums_over_multiple_packs() {
    let s = Sysfs::new("energy-sum");
    s.supply("BAT0", "Battery", &[("energy_now", "20000000")]);
    s.supply("BAT1", "Battery", &[("energy_now", "5000000")]);
    assert_eq!(s.discover().energy_uwh(), Some(25_000_000));
}

#[test]
fn primary_prefers_a_pack_that_actually_reports_a_capacity() {
    // An empty secondary bay sorts first by name but has nothing to report.
    let s = Sysfs::new("primary");
    s.supply("BAT0", "Battery", &[]);
    s.supply("BAT1", "Battery", &[("capacity", "55")]);
    let inv = s.discover();
    assert_eq!(inv.primary().unwrap().name, "BAT1");
}

#[test]
fn non_battery_supplies_are_ignored() {
    let s = Sysfs::new("usbpd");
    s.supply("AC", "Mains", &[("online", "1")]);
    s.supply("ucsi-source-psy-USBC000:001", "USB", &[("online", "1")]);
    s.supply("hidpp_battery_0", "Battery", &[("capacity", "80")]);
    let inv = s.discover();
    // A wireless mouse *is* a battery as far as sysfs is concerned; the point
    // of the test is that the Mains and USB supplies are not.
    assert_eq!(inv.names(), vec!["hidpp_battery_0"]);
}

// ── The writer ───────────────────────────────────────────────────────────────

/// Plan the window for a fixture battery and apply it with the real writer.
fn apply_window(s: &Sysfs, start: u8, stop: u8) {
    use rimed_core::syswriter::{RealWriter, SysWriter};
    let writer = RealWriter::new(false);
    for a in s.discover().plan_thresholds(start, stop) {
        writer.apply(&a).unwrap();
    }
}

#[test]
fn independent_thresholds_land_as_asked() {
    let s = Sysfs::new("write-pair");
    s.supply("BAT0", "Battery", &MODERN_THRESHOLDS);
    apply_window(&s, 60, 80);
    let dir = s.root.join("class/power_supply/BAT0");
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
    assert_eq!(read("charge_control_start_threshold"), "60");
    assert_eq!(read("charge_control_end_threshold"), "80");
}

/// msi-ec keeps both thresholds in ONE EC byte (start always reads stop - 10),
/// so whichever attribute is written last decides. Stand that in with one
/// file behind both names: the stop must be the last word.
#[cfg(unix)]
#[test]
fn on_a_shared_register_the_stop_is_written_last() {
    let s = Sysfs::new("write-shared");
    s.supply("BAT1", "Battery", &[("charge_control_end_threshold", "100")]);
    let dir = s.root.join("class/power_supply/BAT1");
    std::os::unix::fs::symlink(
        dir.join("charge_control_end_threshold"),
        dir.join("charge_control_start_threshold"),
    )
    .unwrap();
    apply_window(&s, 60, 80);
    assert_eq!(
        std::fs::read_to_string(dir.join("charge_control_end_threshold")).unwrap(),
        "80",
        "a 60/80 window on msi-ec must keep the 80 % stop (it used to land as 60/70)"
    );
}
