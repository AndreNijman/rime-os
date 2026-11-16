//! Fan enumeration and mode planning against synthetic hwmon / msi-ec trees,
//! plus the safety properties: no plan ever commands a duty cycle below the
//! profile floor, and every restore path ends in firmware control.

use std::fs;
use std::path::{Path, PathBuf};

use rimed_core::fan::{
    self, plan_firmware_restore, plan_mode, FanInventory, FanMode, FanSnapshot,
};
use rimed_core::profile::{CurvePoint, FanBackend, FanConfig};
use rimed_core::syswriter::{MockWriter, RealWriter, SysWriter};
use rimed_core::tier::Action;

struct Fixture(PathBuf);

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "rimed-fan-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).unwrap();
        Fixture(root)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn write(&self, rel: &str, contents: &str) {
        let p = self.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, contents).unwrap();
    }
    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.0.join(rel)).unwrap().trim().to_string()
    }
    fn abs(&self, rel: &str) -> String {
        self.0.join(rel).to_string_lossy().to_string()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

/// A two-fan hwmon chip with one controllable channel, plus an uncontrollable
/// nvme sensor that must be ignored by the deny list.
fn hwmon_fixture(tag: &str) -> Fixture {
    let f = Fixture::new(tag);
    f.write("class/hwmon/hwmon0/name", "nct6797\n");
    f.write("class/hwmon/hwmon0/fan1_input", "2400\n");
    f.write("class/hwmon/hwmon0/fan2_input", "3100\n");
    f.write("class/hwmon/hwmon0/pwm1", "128\n");
    f.write("class/hwmon/hwmon0/pwm1_enable", "2\n");
    f.write("class/hwmon/hwmon1/name", "nvme\n");
    f.write("class/hwmon/hwmon1/temp1_input", "42000\n");
    f
}

/// The Katana's shape: msi-ec with fan_mode + cooler_boost and percentage
/// readings, and no pwm anywhere.
fn msi_ec_fixture(tag: &str) -> Fixture {
    let f = Fixture::new(tag);
    f.write("devices/platform/msi-ec/fan_mode", "auto\n");
    f.write(
        "devices/platform/msi-ec/available_fan_modes",
        "auto\nsilent\nbasic\nadvanced\n",
    );
    f.write("devices/platform/msi-ec/cooler_boost", "off\n");
    f.write("devices/platform/msi-ec/cpu/realtime_fan_speed", "42\n");
    f.write("devices/platform/msi-ec/gpu/realtime_fan_speed", "38\n");
    f
}

#[test]
fn hwmon_discovery_finds_fans_and_one_control() {
    let f = hwmon_fixture("discover");
    let inv = FanInventory::discover(f.path(), &FanConfig::default());
    assert_eq!(inv.sensors.len(), 2, "two fanN_input sensors");
    assert_eq!(inv.controls.len(), 1, "one pwm channel");
    assert_eq!(inv.controls[0].id, "nct6797/pwm1");
    assert_eq!(
        inv.controls[0].enable_path.as_deref(),
        Some(f.abs("class/hwmon/hwmon0/pwm1_enable").as_str())
    );
    assert!(inv.controllable());
    assert_eq!(inv.modes(&FanConfig::default()), vec!["auto", "max", "manual"]);

    // Readings fold the duty cycle into the matching fan.
    let readings = inv.read();
    let fan1 = readings.iter().find(|r| r.id == "nct6797/fan1").unwrap();
    assert_eq!(fan1.rpm, Some(2400));
    assert_eq!(fan1.pwm, Some(128));
    assert!(fan1.controllable);
    assert_eq!(fan1.percent, None, "hwmon reports RPM, never a percentage");
}

#[test]
fn hwmon_exclude_list_is_honoured() {
    let f = hwmon_fixture("exclude");
    let cfg = FanConfig {
        exclude_hwmon: vec!["nct6797".to_string()],
        ..FanConfig::default()
    };
    let inv = FanInventory::discover(f.path(), &cfg);
    assert!(inv.sensors.is_empty());
    assert!(inv.controls.is_empty());
    assert!(!inv.controllable());
    assert!(inv.modes(&cfg).is_empty());
}

#[test]
fn no_hwmon_at_all_reports_unsupported_and_plans_nothing() {
    let f = Fixture::new("bare");
    let cfg = FanConfig::default();
    let inv = FanInventory::discover(f.path(), &cfg);
    assert!(inv.sensors.is_empty());
    assert!(inv.controls.is_empty());
    assert!(inv.msi_ec.is_none());
    assert!(!inv.controllable());
    assert!(plan_mode(&inv, &cfg, FanMode::Max).is_empty());
    assert!(plan_firmware_restore(&inv).is_empty());
    // And a completely absent sysfs root is fine too.
    let inv = FanInventory::discover(Path::new("/nonexistent/rimed-fan"), &cfg);
    assert!(!inv.controllable());
}

#[test]
fn msi_ec_discovery_reports_percent_not_rpm() {
    let f = msi_ec_fixture("discover");
    let inv = FanInventory::discover(f.path(), &FanConfig::default());
    let ec = inv.msi_ec.as_ref().expect("msi-ec found");
    assert!(ec.fan_mode_path.is_some());
    assert!(ec.cooler_boost_path.is_some());
    assert_eq!(ec.available_modes, vec!["auto", "silent", "basic", "advanced"]);
    assert!(inv.controllable());
    // No pwm channel -> no manual/curve on offer.
    assert_eq!(inv.modes(&FanConfig::default()), vec!["auto", "max"]);

    let readings = inv.read();
    let cpu = readings.iter().find(|r| r.id == "msi-ec/cpu").unwrap();
    assert_eq!(cpu.percent, Some(42));
    assert_eq!(cpu.rpm, None, "msi-ec has no RPM to report — never fabricate one");
    assert_eq!(readings.iter().filter(|r| r.rpm.is_some()).count(), 0);
}

#[test]
fn msi_ec_max_is_cooler_boost() {
    let f = msi_ec_fixture("max");
    let cfg = FanConfig {
        msi_ec_max_mode: Some("advanced".into()),
        ..FanConfig::default()
    };
    let inv = FanInventory::discover(f.path(), &cfg);
    let plan = plan_mode(&inv, &cfg, FanMode::Max);
    assert_eq!(
        plan,
        vec![
            Action::FanVendorAttr {
                path: f.abs("devices/platform/msi-ec/fan_mode"),
                value: "advanced".into(),
                what: "msi-ec fan_mode".into(),
            },
            Action::FanVendorAttr {
                path: f.abs("devices/platform/msi-ec/cooler_boost"),
                value: "on".into(),
                what: "msi-ec cooler_boost".into(),
            },
        ]
    );

    // A vendor mode the EC does not advertise falls back to something it does.
    let cfg = FanConfig {
        msi_ec_max_mode: Some("turbo-nonsense".into()),
        ..FanConfig::default()
    };
    let plan = plan_mode(&inv, &cfg, FanMode::Max);
    match &plan[0] {
        Action::FanVendorAttr { value, .. } => assert_eq!(value, "advanced"),
        other => panic!("expected a fan_mode write, got {other:?}"),
    }
}

#[test]
fn hwmon_max_is_manual_at_full_duty() {
    let f = hwmon_fixture("max");
    let cfg = FanConfig::default();
    let inv = FanInventory::discover(f.path(), &cfg);
    assert_eq!(
        plan_mode(&inv, &cfg, FanMode::Max),
        vec![
            Action::FanPwmEnable {
                path: f.abs("class/hwmon/hwmon0/pwm1_enable"),
                value: 1,
            },
            Action::FanPwm {
                path: f.abs("class/hwmon/hwmon0/pwm1"),
                value: 255,
            },
        ]
    );
}

#[test]
fn manual_never_commands_below_the_floor() {
    let f = hwmon_fixture("floor");
    let cfg = FanConfig {
        min_pwm: 90,
        ..FanConfig::default()
    };
    let inv = FanInventory::discover(f.path(), &cfg);
    // Ask for a stopped fan; get the floor.
    for asked in [0u8, 1, 45, 89] {
        let plan = plan_mode(&inv, &cfg, FanMode::Manual(asked));
        let pwm = plan
            .iter()
            .find_map(|a| match a {
                Action::FanPwm { value, .. } => Some(*value),
                _ => None,
            })
            .expect("a duty cycle was planned");
        assert_eq!(pwm, 90, "asking for {asked} must clamp up to the floor");
    }
    // Above the floor is passed through.
    let plan = plan_mode(&inv, &cfg, FanMode::Manual(200));
    assert!(plan.contains(&Action::FanPwm {
        path: f.abs("class/hwmon/hwmon0/pwm1"),
        value: 200,
    }));
}

#[test]
fn manual_maps_to_cooler_boost_on_msi_ec() {
    let f = msi_ec_fixture("manual");
    let cfg = FanConfig {
        boost_pwm_threshold: 200,
        ..FanConfig::default()
    };
    let inv = FanInventory::discover(f.path(), &cfg);
    let boost = |pwm: u8| -> String {
        plan_mode(&inv, &cfg, FanMode::Manual(pwm))
            .iter()
            .find_map(|a| match a {
                Action::FanVendorAttr { value, what, .. } if what.contains("cooler_boost") => {
                    Some(value.clone())
                }
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(boost(255), "on");
    assert_eq!(boost(210), "on");
    assert_eq!(boost(120), "off");
}

#[test]
fn snapshot_restore_is_exact_and_survives_a_round_trip_on_disk() {
    let f = hwmon_fixture("restore");
    let cfg = FanConfig::default();
    let inv = FanInventory::discover(f.path(), &cfg);

    // Prior state: firmware control (2) at duty cycle 128.
    let snap = FanSnapshot::capture(&inv);
    let writer = RealWriter::new(false);

    // Go to max...
    for a in plan_mode(&inv, &cfg, FanMode::Max) {
        writer.apply(&a).unwrap();
    }
    assert_eq!(f.read("class/hwmon/hwmon0/pwm1_enable"), "1");
    assert_eq!(f.read("class/hwmon/hwmon0/pwm1"), "255");

    // ...and back.
    for a in snap.plan_restore() {
        writer.apply(&a).unwrap();
    }
    assert_eq!(f.read("class/hwmon/hwmon0/pwm1_enable"), "2");
    // pwm is left where the firmware wants it: we only rewrite the duty cycle
    // when the prior state was itself manual.
    assert_eq!(f.read("class/hwmon/hwmon0/pwm1"), "255");
}

#[test]
fn restore_from_a_manual_prior_state_puts_the_duty_cycle_back_too() {
    let f = Fixture::new("restore-manual");
    f.write("class/hwmon/hwmon0/name", "nct6797\n");
    f.write("class/hwmon/hwmon0/fan1_input", "2400\n");
    f.write("class/hwmon/hwmon0/pwm1", "96\n");
    f.write("class/hwmon/hwmon0/pwm1_enable", "1\n"); // already manual

    let cfg = FanConfig::default();
    let inv = FanInventory::discover(f.path(), &cfg);
    let snap = FanSnapshot::capture(&inv);
    let writer = RealWriter::new(false);
    for a in plan_mode(&inv, &cfg, FanMode::Max) {
        writer.apply(&a).unwrap();
    }
    for a in snap.plan_restore() {
        writer.apply(&a).unwrap();
    }
    assert_eq!(f.read("class/hwmon/hwmon0/pwm1_enable"), "1");
    assert_eq!(f.read("class/hwmon/hwmon0/pwm1"), "96");
}

#[test]
fn firmware_restore_needs_no_prior_state() {
    // The crash path: no snapshot, no daemon — just hand everything back.
    let f = hwmon_fixture("firmware");
    let cfg = FanConfig::default();
    let writer = RealWriter::new(false);
    // Leave the fan stuck in manual at a dangerously low duty cycle first.
    fs::write(f.path().join("class/hwmon/hwmon0/pwm1_enable"), "1").unwrap();
    fs::write(f.path().join("class/hwmon/hwmon0/pwm1"), "0").unwrap();

    let n = fan::restore_to_firmware(f.path(), &cfg, &writer);
    assert_eq!(n, 1);
    assert_eq!(
        f.read("class/hwmon/hwmon0/pwm1_enable"),
        "2",
        "a fan left in manual at 0 must come back under firmware control"
    );
}

#[test]
fn firmware_restore_leaves_a_fan_the_firmware_already_owns() {
    // A ThinkPad without fan_control=1 refuses every pwm1_enable write; its fan
    // reads 2 because nothing ever took it away from the firmware. Nothing to do.
    let f = hwmon_fixture("firmware-owned");
    let cfg = FanConfig::default();
    let writer = RealWriter::new(false);
    fs::write(f.path().join("class/hwmon/hwmon0/pwm1_enable"), "2\n").unwrap();
    fs::write(f.path().join("class/hwmon/hwmon0/pwm1"), "77").unwrap();

    assert_eq!(fan::restore_to_firmware(f.path(), &cfg, &writer), 0);
    assert_eq!(f.read("class/hwmon/hwmon0/pwm1_enable"), "2");
    assert_eq!(f.read("class/hwmon/hwmon0/pwm1"), "77", "a firmware-owned fan is not written");
}

#[test]
fn safe_restore_falls_back_to_full_speed_when_auto_is_refused() {
    // A driver that rejects everything except 0/1: writing "2" fails, so the
    // ladder must end with the fan at full speed, never stopped.
    let f = Fixture::new("stubborn");
    f.write("class/hwmon/hwmon0/name", "stubborn\n");
    f.write("class/hwmon/hwmon0/pwm1", "0\n");
    // A directory where the enable attribute should be: every write to it
    // fails, which is exactly the "driver refuses" case.
    fs::create_dir_all(f.path().join("class/hwmon/hwmon0/pwm1_enable")).unwrap();

    let writer = RealWriter::new(false);
    writer
        .apply(&Action::FanSafeRestore {
            enable_path: Some(f.abs("class/hwmon/hwmon0/pwm1_enable")),
            pwm_path: Some(f.abs("class/hwmon/hwmon0/pwm1")),
            prior_enable: None,
            prior_pwm: None,
        })
        .unwrap();
    assert_eq!(
        f.read("class/hwmon/hwmon0/pwm1"),
        "255",
        "if firmware control cannot be restored the fan goes to full speed"
    );
}

#[test]
fn auto_mode_hands_control_back_rather_than_writing_a_duty_cycle() {
    let f = hwmon_fixture("auto");
    let cfg = FanConfig::default();
    let inv = FanInventory::discover(f.path(), &cfg);
    let plan = plan_mode(&inv, &cfg, FanMode::Auto);
    assert!(matches!(plan[0], Action::FanSafeRestore { prior_enable: Some(2), .. }));
    assert!(!plan.iter().any(|a| matches!(a, Action::FanPwm { .. })));
}

#[test]
fn dry_run_writes_nothing() {
    let f = hwmon_fixture("dry");
    let cfg = FanConfig::default();
    let inv = FanInventory::discover(f.path(), &cfg);
    let writer = RealWriter::new(true);
    for a in plan_mode(&inv, &cfg, FanMode::Max) {
        writer.apply(&a).unwrap();
    }
    assert_eq!(f.read("class/hwmon/hwmon0/pwm1_enable"), "2");
    assert_eq!(f.read("class/hwmon/hwmon0/pwm1"), "128");

    // The mock records the plan and touches nothing either.
    let mock = MockWriter::new();
    let plan = plan_mode(&inv, &cfg, FanMode::Max);
    mock.apply_all(&plan).unwrap();
    assert_eq!(mock.recorded(), plan);
}

#[test]
fn backend_selection_is_honoured() {
    let f = msi_ec_fixture("backend");
    f.write("class/hwmon/hwmon0/name", "nct6797\n");
    f.write("class/hwmon/hwmon0/pwm1", "128\n");

    let hwmon_only = FanInventory::discover(
        f.path(),
        &FanConfig {
            backend: FanBackend::Hwmon,
            ..FanConfig::default()
        },
    );
    assert!(hwmon_only.msi_ec.is_none());
    assert_eq!(hwmon_only.controls.len(), 1);

    let ec_only = FanInventory::discover(
        f.path(),
        &FanConfig {
            backend: FanBackend::MsiEc,
            ..FanConfig::default()
        },
    );
    assert!(ec_only.msi_ec.is_some());
    assert!(ec_only.controls.is_empty());

    let off = FanInventory::discover(
        f.path(),
        &FanConfig {
            backend: FanBackend::None,
            ..FanConfig::default()
        },
    );
    assert!(!off.controllable());
}

#[test]
fn msi_wmi_platform_gives_rpm_but_no_control() {
    // What the Katana actually presents once msi-wmi-platform is force-loaded:
    // four read-only fanN_input channels, no pwm anywhere.
    let f = Fixture::new("msi-wmi");
    f.write("class/hwmon/hwmon3/name", "msi_wmi_platform\n");
    for n in 1..=4 {
        f.write(&format!("class/hwmon/hwmon3/fan{n}_input"), &format!("{}\n", 2000 + n * 100));
    }
    let cfg = FanConfig::default();
    let inv = FanInventory::discover(f.path(), &cfg);
    assert_eq!(inv.sensors.len(), 4);
    assert!(inv.controls.is_empty(), "the driver exposes no pwm");
    assert!(!inv.controllable(), "RPM visibility is not controllability");
    assert!(inv.modes(&cfg).is_empty());
    assert_eq!(inv.read()[0].rpm, Some(2100));
    assert!(plan_mode(&inv, &cfg, FanMode::Max).is_empty());
    assert!(inv.summary().iter().any(|s| s.contains("msi-wmi-platform")));

    // Selecting the backend explicitly finds the same thing and ignores others.
    f.write("class/hwmon/hwmon0/name", "nvme\n");
    f.write("class/hwmon/hwmon0/fan1_input", "900\n");
    let only = FanInventory::discover(
        f.path(),
        &FanConfig {
            backend: FanBackend::MsiWmi,
            ..FanConfig::default()
        },
    );
    assert_eq!(only.sensors.len(), 4);
    assert!(only.sensors.iter().all(|s| s.chip == "msi_wmi_platform"));
}

#[test]
fn a_named_backend_that_is_absent_degrades_to_unsupported() {
    // The profile asks for msi-ec; the module never bound, so there is no
    // platform device. This must be a clean "unsupported", not an error.
    let f = Fixture::new("msi-ec-absent");
    f.write("class/hwmon/hwmon3/name", "msi_wmi_platform\n");
    f.write("class/hwmon/hwmon3/fan1_input", "2400\n");
    let cfg = FanConfig {
        backend: FanBackend::MsiEc,
        ..FanConfig::default()
    };
    let inv = FanInventory::discover(f.path(), &cfg);
    assert!(inv.msi_ec.is_none());
    assert!(!inv.controllable());
    assert!(inv.modes(&cfg).is_empty());
    assert!(plan_mode(&inv, &cfg, FanMode::Max).is_empty());
    assert!(plan_firmware_restore(&inv).is_empty());
    assert_eq!(fan::restore_to_firmware(f.path(), &cfg, &RealWriter::new(false)), 0);

    // Same story in "auto": the Katana as shipped — RPM from msi-wmi-platform,
    // nothing controllable.
    let cfg = FanConfig::default();
    let inv = FanInventory::discover(f.path(), &cfg);
    assert_eq!(inv.sensors.len(), 1);
    assert!(!inv.controllable());
}

#[test]
fn an_empty_msi_ec_directory_is_not_a_backend() {
    // A platform directory with none of the fan attributes (a partially bound
    // or stubbed driver) must not be advertised as controllable.
    let f = Fixture::new("msi-ec-empty");
    fs::create_dir_all(f.path().join("devices/platform/msi-ec")).unwrap();
    f.write("devices/platform/msi-ec/fw_version", "17L3EMS1.100\n");
    let inv = FanInventory::discover(f.path(), &FanConfig::default());
    assert!(inv.msi_ec.is_none());
    assert!(!inv.controllable());
}

#[test]
fn curve_interpolates_and_respects_the_floor() {
    let points = vec![
        CurvePoint { temp_c: 45.0, pwm: 90 },
        CurvePoint { temp_c: 60.0, pwm: 130 },
        CurvePoint { temp_c: 75.0, pwm: 190 },
        CurvePoint { temp_c: 85.0, pwm: 255 },
    ];
    // Below/above the ends clamps to the end points.
    assert_eq!(fan::curve_pwm(&points, 20.0, 77, 255), 90);
    assert_eq!(fan::curve_pwm(&points, 99.0, 77, 255), 255);
    // Exactly on a point.
    assert_eq!(fan::curve_pwm(&points, 60.0, 77, 255), 130);
    // Halfway between 60 (130) and 75 (190) -> 160.
    assert_eq!(fan::curve_pwm(&points, 67.5, 77, 255), 160);
    // The floor wins over a curve that asks for less.
    assert_eq!(fan::curve_pwm(&points, 20.0, 120, 255), 120);
    // The ceiling wins over a curve that asks for more.
    assert_eq!(fan::curve_pwm(&points, 99.0, 77, 200), 200);
    // An empty curve yields the floor, never zero.
    assert_eq!(fan::curve_pwm(&[], 80.0, 77, 255), 77);
}

#[test]
fn curve_temperature_prefers_a_package_sensor() {
    let f = Fixture::new("temp");
    f.write("class/hwmon/hwmon0/name", "nvme\n");
    f.write("class/hwmon/hwmon0/temp1_input", "90000\n"); // hot SSD, not the CPU
    f.write("class/hwmon/hwmon1/name", "coretemp\n");
    f.write("class/hwmon/hwmon1/temp1_input", "61000\n");
    f.write("class/hwmon/hwmon1/temp2_input", "64000\n");
    assert_eq!(fan::read_curve_temp(f.path()), Some(64.0));

    // With no hwmon at all, fall back to the thermal zones.
    let g = Fixture::new("temp-zones");
    g.write("class/thermal/thermal_zone0/temp", "55000\n");
    g.write("class/thermal/thermal_zone1/temp", "58500\n");
    assert_eq!(fan::read_curve_temp(g.path()), Some(58.5));

    assert_eq!(fan::read_curve_temp(Path::new("/nonexistent/rimed-temp")), None);
}

#[test]
fn mode_parsing_covers_the_cli_surface() {
    assert_eq!(FanMode::parse("auto", 128).unwrap(), FanMode::Auto);
    assert_eq!(FanMode::parse("MAX", 128).unwrap(), FanMode::Max);
    assert_eq!(FanMode::parse("full", 128).unwrap(), FanMode::Max);
    assert_eq!(FanMode::parse("manual", 128).unwrap(), FanMode::Manual(128));
    assert_eq!(FanMode::parse("manual:200", 128).unwrap(), FanMode::Manual(200));
    assert_eq!(FanMode::parse("curve", 128).unwrap(), FanMode::Curve);
    assert!(FanMode::parse("turbo", 128).is_err());
    assert!(FanMode::parse("manual:999", 128).is_err());
    assert_eq!(FanMode::Manual(200).to_string(), "manual:200");
    assert_eq!(FanMode::Auto.as_str(), "auto");
}
