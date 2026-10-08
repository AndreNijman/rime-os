//! Profile-schema tests for the M6 additions.
//!
//! The contract is: `[fan]` and `[gamemode]` are optional everywhere. A profile
//! written before M6 must still parse and must behave exactly as it did, which
//! is why the assertions below check both the shipped profiles that *do* carry
//! the new tables and the ones that deliberately do not.

use rimed_core::profile::{ClockSpec, CpusetPolicy, FanBackend, Profile, ProfileSet};
use rimed_core::tier::Tier;
use rimed_core::IrqPolicy;

fn set() -> ProfileSet {
    ProfileSet::builtin()
}

/// A minimal pre-M6 profile: no `[fan]`, no `[gamemode]`.
const PRE_M6: &str = r#"
    id = "legacy-box"
    kind = "generic"
    [defaults]
    ac = "balanced"
    battery = "power-saver"
    [tiers.performance]
    governor = "performance"
    [tiers.balanced]
    governor = "powersave"
    [tiers.power-saver]
    governor = "powersave"
"#;

#[test]
fn a_profile_without_the_new_keys_still_parses() {
    let p = Profile::from_toml(PRE_M6).expect("pre-M6 profile parses");
    assert!(p.fan.is_none());
    assert!(p.gamemode.is_none());
    // ...and gets safe defaults rather than nothing.
    let fan = p.fan_config();
    assert_eq!(fan.backend, FanBackend::Auto);
    assert_eq!(fan.min_pwm, 77, "a default floor, never 0");
    assert!(fan.curve.is_empty());
    assert!(fan.default_mode.is_none(), "no unsolicited fan writes at start-up");
    let game = p.game_config();
    assert!(game.enabled);
    assert_eq!(game.tier, Tier::Performance);
    assert_eq!(game.cpuset_policy(), CpusetPolicy::PCores);
    assert_eq!(game.irq_policy(), IrqPolicy::AwayFromGame);
    assert_eq!(game.cgroup, "/sys/fs/cgroup/rime-game");
    assert!(game.nvidia.enabled);
    assert!(game.nvidia.graphics_clock.is_none(), "no clock lock unless asked");
}

#[test]
fn the_shipped_pre_m6_profiles_are_untouched() {
    let s = set();
    for id in ["generic-desktop", "generic-laptop", "amd-zen"] {
        let p = s.get(id).unwrap_or_else(|| panic!("{id} present"));
        assert!(p.fan.is_none(), "{id} declares no [fan]");
        assert!(p.gamemode.is_none(), "{id} declares no [gamemode]");
    }
}

#[test]
fn a_partial_fan_table_keeps_the_other_defaults() {
    let toml = PRE_M6.to_string() + "\n[fan]\nmin_pwm = 120\n";
    let p = Profile::from_toml(&toml).unwrap();
    let fan = p.fan_config();
    assert_eq!(fan.min_pwm, 120);
    assert_eq!(fan.max_pwm, 255);
    assert_eq!(fan.backend, FanBackend::Auto);
    assert_eq!(fan.curve_interval_secs, 3);
}

#[test]
fn a_partial_gamemode_table_keeps_the_other_defaults() {
    let toml = PRE_M6.to_string() + "\n[gamemode]\ncpuset = \"off\"\n";
    let p = Profile::from_toml(&toml).unwrap();
    let game = p.game_config();
    assert_eq!(game.cpuset_policy(), CpusetPolicy::Off);
    assert_eq!(game.tier, Tier::Performance);
    assert_eq!(game.irq_policy(), IrqPolicy::AwayFromGame);
    assert!(game.nvidia.enabled);
}

#[test]
fn katana_carries_the_real_m6_values() {
    let s = set();
    let p = s.get("msi-katana-gf76").unwrap();

    let fan = p.fan_config();
    assert_eq!(fan.backend, FanBackend::Auto);
    assert_eq!(fan.min_pwm, 90);
    assert_eq!(fan.boost_pwm_threshold, 200);
    assert_eq!(fan.msi_ec_max_mode.as_deref(), Some("advanced"));
    assert_eq!(fan.msi_ec_auto_mode.as_deref(), Some("auto"));
    assert_eq!(fan.curve.len(), 4);
    assert_eq!(fan.curve[0].temp_c, 45.0);
    assert_eq!(fan.curve[3].pwm, 255);
    // The curve must be monotonic and must never dip below the floor.
    for w in fan.curve.windows(2) {
        assert!(w[1].temp_c > w[0].temp_c, "curve temperatures ascend");
        assert!(w[1].pwm >= w[0].pwm, "curve duty cycles ascend");
    }
    assert!(fan.curve.iter().all(|c| c.pwm >= fan.min_pwm));

    let game = p.game_config();
    assert!(game.enabled);
    assert_eq!(game.tier, Tier::Performance);
    assert_eq!(game.fan_mode.as_deref(), Some("auto"));
    assert_eq!(game.cpuset_policy(), CpusetPolicy::PCores);
    assert_eq!(game.irq_policy(), IrqPolicy::AwayFromGame);
    assert_eq!(game.irq_pin_to_game, vec!["nvidia".to_string()]);
    assert_eq!(game.cgroup, "/sys/fs/cgroup/rime-game");

    // The kernel's own scheduler (measured best here, 2026-10-08).
    assert_eq!(game.scx, "");

    let nv = &game.nvidia;
    assert!(nv.enabled && nv.persistence);
    // No clock locks (2026-10-07). The old [1200, 1620] held the card at its
    // rated boost, 480 MHz under the 2100 MHz nvidia-smi reports on katana and
    // the desktop lets it reach.
    assert_eq!(nv.graphics_clock, None);
    assert_eq!(nv.memory_clock, None);
    // The clamp a profile that does lock still gets, against what the GPU
    // reports.
    let range = ClockSpec::Range([1200, 1620]);
    assert_eq!(range.resolve(1620), Some((1200, 1620)));
    assert_eq!(
        range.resolve(1400),
        Some((1200, 1400)),
        "a ceiling above what the GPU supports is clamped down"
    );
    let max = ClockSpec::Keyword("max".into());
    assert_eq!(max.resolve(6001), Some((6001, 6001)));
    assert_eq!(max.resolve(0), None);
}

#[test]
fn thinkpad_degrades_gracefully() {
    let s = set();
    let p = s.get("thinkpad-l16-g2").unwrap();
    let game = p.game_config();
    // Uniform Zen cores: nothing to pin to, nothing to steer, no NVIDIA.
    assert_eq!(game.cpuset_policy(), CpusetPolicy::All);
    assert_eq!(game.irq_policy(), IrqPolicy::Off);
    assert!(!game.nvidia.enabled);
    assert_eq!(game.tier, Tier::Performance, "game mode holds the top tier");
    assert_eq!(p.fan_config().min_pwm, 90);
    assert_eq!(p.defaults.ac, Tier::Performance);
}

#[test]
fn intel_hybrid_class_pins_but_does_not_lock_clocks() {
    let s = set();
    let p = s.get("intel-hybrid").unwrap();
    let game = p.game_config();
    assert_eq!(game.cpuset_policy(), CpusetPolicy::PCores);
    assert_eq!(game.irq_policy(), IrqPolicy::AwayFromGame);
    assert!(game.nvidia.enabled);
    assert!(
        game.nvidia.graphics_clock.is_none() && game.nvidia.memory_clock.is_none(),
        "a class profile cannot know which dGPU is fitted"
    );
}

#[test]
fn clock_spec_accepts_all_three_toml_shapes() {
    #[derive(serde::Deserialize)]
    struct Holder {
        a: ClockSpec,
        b: ClockSpec,
        c: ClockSpec,
    }
    let h: Holder = toml::from_str("a = \"max\"\nb = 1500\nc = [1200, 1620]\n").unwrap();
    assert_eq!(h.a, ClockSpec::Keyword("max".into()));
    assert_eq!(h.b, ClockSpec::Fixed(1500));
    assert_eq!(h.c, ClockSpec::Range([1200, 1620]));
    // Resolution rules.
    assert_eq!(h.a.resolve(1620), Some((1620, 1620)));
    assert_eq!(h.b.resolve(1400), Some((1400, 1400)), "clamped to the maximum");
    assert_eq!(h.c.resolve(1620), Some((1200, 1620)));
    // An unrecognised keyword never locks anything.
    assert_eq!(ClockSpec::Keyword("turbo".into()).resolve(1620), None);
    // A reversed range is normalised rather than rejected.
    assert_eq!(ClockSpec::Range([1620, 1200]).resolve(1620), Some((1200, 1620)));
}

#[test]
fn an_unreadable_irq_policy_defaults_to_off() {
    let toml = PRE_M6.to_string() + "\n[gamemode]\nirq = \"go-wild\"\n";
    let p = Profile::from_toml(&toml).unwrap();
    assert_eq!(
        p.game_config().irq_policy(),
        IrqPolicy::Off,
        "an unparseable policy must not start moving interrupts"
    );
}

#[test]
fn every_shipped_profile_still_loads_and_plans_tiers() {
    let s = set();
    assert_eq!(s.len(), 6);
    for (id, _) in rimed_core::profile::BUILTIN_PROFILE_TOML {
        let p = s.get(id).unwrap();
        for tier in Tier::ALL {
            // Fan and game actions must never leak into a tier plan.
            for a in p.plan_tier(tier) {
                assert!(
                    !matches!(
                        a,
                        rimed_core::Action::FanPwm { .. }
                            | rimed_core::Action::FanPwmEnable { .. }
                            | rimed_core::Action::FanVendorAttr { .. }
                            | rimed_core::Action::CgroupEnsure { .. }
                            | rimed_core::Action::IrqAffinity { .. }
                    ),
                    "{id}/{tier} leaked an M6 action into the tier plan"
                );
            }
        }
    }
}
