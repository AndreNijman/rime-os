//! The UI boost's pure half (which tiers are boosted, to what, and the exact
//! restore) and the two tier knobs that moved from power-profiles-daemon into
//! rimed when rimed became the only writer (`cpu_boost`, `cpu_min_freq`).
//!
//! Every write here goes to a scratch sysfs tree; nothing touches `/sys`.

use std::path::PathBuf;

use rimed_core::profile::{epp_rank, Profile, ProfileSet};
use rimed_core::syswriter::{Outcome, RealWriter, SysWriter};
use rimed_core::tier::{Action, MinFreq, Tier};

fn builtin(id: &str) -> Profile {
    ProfileSet::builtin()
        .get(id)
        .unwrap_or_else(|| panic!("no builtin profile {id}"))
        .clone()
}

fn epp(e: &str) -> Action {
    Action::Epp(e.into())
}

#[test]
fn the_l16_boosts_balanced_only_and_restores_its_own_epp() {
    let p = builtin("thinkpad-l16-g2");
    let plan = p
        .ui_boost_plan(Tier::Balanced)
        .expect("balanced is boosted");
    assert_eq!(plan.boost, vec![epp("balance_performance")]);
    assert_eq!(plan.restore, vec![epp("balance_power")]);
    // The restore is the very action applying the tier emits: exact.
    let tier_epp: Vec<_> = p
        .plan_tier(Tier::Balanced)
        .into_iter()
        .filter(|a| matches!(a, Action::Epp(_)))
        .collect();
    assert_eq!(plan.restore, tier_epp);
    // Power-saver: the user chose the battery. Performance: nothing to raise.
    assert_eq!(p.ui_boost_plan(Tier::PowerSaver), None);
    assert_eq!(p.ui_boost_plan(Tier::Performance), None);
}

#[test]
fn the_battery_default_is_unchanged() {
    let p = builtin("thinkpad-l16-g2");
    assert_eq!(p.defaults.battery, Tier::Balanced);
    let plan = p.plan_tier(Tier::Balanced);
    assert!(plan.contains(&Action::Governor("powersave".into())));
    assert!(plan.contains(&epp("balance_power")));
}

const BASE: &str = r#"
    id = "t"
    kind = "device"
    [defaults]
    ac = "balanced"
    battery = "balanced"
    [tiers.performance]
    governor = "performance"
    epp = "performance"
    [tiers.balanced]
    governor = "powersave"
    epp = "balance_power"
    [tiers.power-saver]
    governor = "powersave"
    epp = "power"
"#;

#[test]
fn a_tier_already_at_or_above_the_boost_is_left_alone() {
    let p = Profile::from_toml(
        &BASE.replace("epp = \"balance_power\"", "epp = \"balance_performance\""),
    )
    .unwrap();
    assert_eq!(p.ui_boost_plan(Tier::Balanced), None);
}

#[test]
fn a_powersave_governor_tier_with_performance_governor_is_never_boosted() {
    let p = Profile::from_toml(&format!(
        "{BASE}\n[ui_boost]\ntiers = [\"performance\", \"balanced\", \"power-saver\"]\n"
    ))
    .unwrap();
    assert_eq!(
        p.ui_boost_plan(Tier::Performance),
        None,
        "governor performance pins EPP"
    );
    assert!(p.ui_boost_plan(Tier::Balanced).is_some());
    assert_eq!(
        p.ui_boost_plan(Tier::PowerSaver).unwrap().restore,
        vec![epp("power")],
        "a listed power-saver restores its own EPP"
    );
}

#[test]
fn a_disabled_boost_or_a_tier_without_epp_plans_nothing() {
    let off = Profile::from_toml(&format!("{BASE}\n[ui_boost]\nenabled = false\n")).unwrap();
    assert_eq!(off.ui_boost_plan(Tier::Balanced), None);
    let no_epp = Profile::from_toml(&BASE.replace("epp = \"balance_power\"\n", "")).unwrap();
    assert_eq!(
        no_epp.ui_boost_plan(Tier::Balanced),
        None,
        "nothing exact to put back"
    );
}

#[test]
fn a_bad_boost_epp_is_refused_at_load() {
    let err = Profile::from_toml(&format!("{BASE}\n[ui_boost]\nepp = \"turbo\"\n")).unwrap_err();
    assert!(format!("{err:#}").contains("ui_boost.epp"), "{err:#}");
    assert!(Profile::from_toml(&format!("{BASE}\n[ui_boost]\nbogus = 1\n")).is_err());
}

#[test]
fn epp_rank_orders_the_four_canonical_values() {
    let ranks: Vec<_> = [
        "performance",
        "balance_performance",
        "balance_power",
        "power",
        "default",
    ]
    .iter()
    .map(|e| epp_rank(e))
    .collect();
    assert_eq!(ranks, vec![Some(0), Some(1), Some(2), Some(3), None]);
}

// ── the knobs power-profiles-daemon used to write ────────────────────────────

#[test]
fn every_tier_of_the_amd_profiles_asserts_boost_and_floor() {
    for id in ["amd-zen", "thinkpad-l16-g2"] {
        let p = builtin(id);
        for (tier, on, floor) in [
            (Tier::Performance, true, MinFreq::LowestNonlinear),
            (Tier::Balanced, true, MinFreq::LowestNonlinear),
            (Tier::PowerSaver, false, MinFreq::CpuinfoMin),
        ] {
            let plan = p.plan_tier(tier);
            assert!(
                plan.contains(&Action::CpuBoost(on)),
                "{id} {tier}: {plan:?}"
            );
            assert!(
                plan.contains(&Action::CpuMinFreq(floor)),
                "{id} {tier}: {plan:?}"
            );
        }
    }
}

#[test]
fn a_knob_set_in_only_some_tiers_is_refused() {
    let partial = BASE.replace(
        "[tiers.balanced]\n",
        "[tiers.balanced]\n    cpu_boost = true\n",
    );
    let err = Profile::from_toml(&partial).unwrap_err();
    assert!(format!("{err:#}").contains("cpu_boost"), "{err:#}");
}

struct Sys {
    root: PathBuf,
}

impl Sys {
    fn new(tag: &str) -> Sys {
        let root = std::env::temp_dir().join(format!(
            "rimed-uiboost-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        Sys { root }
    }
    fn put(&self, rel: &str, v: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, v).unwrap();
    }
    fn get(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel))
            .unwrap()
            .trim()
            .to_string()
    }
    fn policy(&self, n: u32, extra: &[(&str, &str)]) {
        let d = format!("devices/system/cpu/cpufreq/policy{n}");
        self.put(&format!("{d}/scaling_min_freq"), "400000\n");
        self.put(&format!("{d}/cpuinfo_min_freq"), "400000\n");
        for (k, v) in extra {
            self.put(&format!("{d}/{k}"), v);
        }
    }
    fn writer(&self) -> RealWriter {
        RealWriter::with_root(false, &self.root)
    }
}

impl Drop for Sys {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

#[test]
fn boost_is_written_per_policy_where_the_driver_publishes_it() {
    let s = Sys::new("perpolicy");
    s.policy(0, &[("boost", "1\n")]);
    s.policy(1, &[("boost", "1\n")]);
    s.put("devices/system/cpu/cpufreq/boost", "1\n");
    assert_eq!(
        s.writer().apply(&Action::CpuBoost(false)).unwrap(),
        Outcome::Landed
    );
    assert_eq!(s.get("devices/system/cpu/cpufreq/policy0/boost"), "0");
    assert_eq!(s.get("devices/system/cpu/cpufreq/policy1/boost"), "0");
    assert_eq!(
        s.get("devices/system/cpu/cpufreq/boost"),
        "1",
        "global left alone"
    );
}

#[test]
fn boost_falls_back_to_the_global_switch_then_to_intel_no_turbo() {
    let s = Sys::new("global");
    s.policy(0, &[]);
    s.put("devices/system/cpu/cpufreq/boost", "1\n");
    assert_eq!(
        s.writer().apply(&Action::CpuBoost(false)).unwrap(),
        Outcome::Landed
    );
    assert_eq!(s.get("devices/system/cpu/cpufreq/boost"), "0");

    let i = Sys::new("intel");
    i.policy(0, &[]);
    i.put("devices/system/cpu/intel_pstate/no_turbo", "0\n");
    assert_eq!(
        i.writer().apply(&Action::CpuBoost(false)).unwrap(),
        Outcome::Landed
    );
    assert_eq!(
        i.get("devices/system/cpu/intel_pstate/no_turbo"),
        "1",
        "inverted sense"
    );
    i.writer().apply(&Action::CpuBoost(true)).unwrap();
    assert_eq!(i.get("devices/system/cpu/intel_pstate/no_turbo"), "0");

    let none = Sys::new("none");
    none.policy(0, &[]);
    assert!(matches!(
        none.writer().apply(&Action::CpuBoost(true)).unwrap(),
        Outcome::Refused(_)
    ));
}

#[test]
fn the_floor_is_read_from_each_policy_never_guessed() {
    let s = Sys::new("floor");
    s.policy(0, &[("amd_pstate_lowest_nonlinear_freq", "1600000\n")]);
    s.policy(1, &[("amd_pstate_lowest_nonlinear_freq", "1500000\n")]);
    s.policy(2, &[]);
    let w = s.writer();
    assert_eq!(
        w.apply(&Action::CpuMinFreq(MinFreq::LowestNonlinear))
            .unwrap(),
        Outcome::Landed
    );
    assert_eq!(
        s.get("devices/system/cpu/cpufreq/policy0/scaling_min_freq"),
        "1600000"
    );
    assert_eq!(
        s.get("devices/system/cpu/cpufreq/policy1/scaling_min_freq"),
        "1500000"
    );
    assert_eq!(
        s.get("devices/system/cpu/cpufreq/policy2/scaling_min_freq"),
        "400000",
        "a policy without the attribute is skipped"
    );
    w.apply(&Action::CpuMinFreq(MinFreq::CpuinfoMin)).unwrap();
    assert_eq!(
        s.get("devices/system/cpu/cpufreq/policy0/scaling_min_freq"),
        "400000"
    );
}

#[test]
fn dry_run_writes_neither_knob() {
    let s = Sys::new("dry");
    s.policy(
        0,
        &[
            ("boost", "1\n"),
            ("amd_pstate_lowest_nonlinear_freq", "1600000\n"),
        ],
    );
    let w = RealWriter::with_root(true, &s.root);
    w.apply(&Action::CpuBoost(false)).unwrap();
    w.apply(&Action::CpuMinFreq(MinFreq::LowestNonlinear))
        .unwrap();
    assert_eq!(s.get("devices/system/cpu/cpufreq/policy0/boost"), "1");
    assert_eq!(
        s.get("devices/system/cpu/cpufreq/policy0/scaling_min_freq"),
        "400000"
    );
}
