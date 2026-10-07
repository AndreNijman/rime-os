//! Game-mode planning, and the property that matters most: **exit restores
//! exactly what enter changed**.
//!
//! The restore test is deliberately a *filesystem* diff rather than a
//! comparison of action lists. Comparing plans only proves a restore was
//! planned; snapshotting every file in a fixture tree, running the enter plan
//! through a live `RealWriter`, then running the exit plan and asserting the
//! tree is byte-identical proves the restore actually happened.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rimed_core::game::{self, GameInputs, PidPlacement};
use rimed_core::gpu::{self, MockNvidiaSmi, NvidiaGpu, NvidiaSmi};
use rimed_core::irq::{self, IrqEntry};
use rimed_core::profile::{ClockSpec, CpusetPolicy, GameModeConfig, NvidiaConfig};
use rimed_core::syswriter::{RealWriter, SysWriter};
use rimed_core::tier::{Action, Tier};
use rimed_core::topology::CoreTopology;

struct Fixture(PathBuf);

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "rimed-game-{tag}-{}-{:?}",
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
    /// Every file in the tree, path -> contents.
    ///
    /// Contents are trimmed: sysfs and procfs hand back newline-terminated
    /// values on read and accept unterminated ones on write, so a trailing
    /// `\n` is an artifact of the fixture being a plain file, not a difference
    /// in what the kernel would hold.
    fn snapshot(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        walk(&self.0, &mut out);
        out
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

fn walk(dir: &Path, out: &mut BTreeMap<String, String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if let Ok(s) = fs::read_to_string(&p) {
            out.insert(p.to_string_lossy().to_string(), s.trim().to_string());
        }
    }
}

/// An Alder Lake machine with interrupts and a cgroup-v2 hierarchy.
fn machine(tag: &str) -> Fixture {
    let f = Fixture::new(tag);
    // 20 CPUs: 0-11 P, 12-19 E.
    for c in 0..20u32 {
        fs::create_dir_all(f.path().join(format!("sys/devices/system/cpu/cpu{c}"))).unwrap();
    }
    f.write("sys/devices/system/cpu/online", "0-19\n");
    f.write("sys/devices/cpu_core/cpus", "0-11\n");
    f.write("sys/devices/cpu_atom/cpus", "12-19\n");

    // Interrupts: the GPU, a USB controller, and the (unsteerable) timer.
    f.write("proc/irq/0/smp_affinity_list", "0-19\n");
    fs::create_dir_all(f.path().join("proc/irq/0/timer")).unwrap();
    f.write("proc/irq/16/smp_affinity_list", "0-19\n");
    fs::create_dir_all(f.path().join("proc/irq/16/nvidia")).unwrap();
    f.write("proc/irq/24/smp_affinity_list", "0-19\n");
    fs::create_dir_all(f.path().join("proc/irq/24/xhci_hcd")).unwrap();

    // cgroup v2 root plus the user scope the game process starts in. Real
    // `cgroup.procs` appends on write and lists on read; the fixture holds the
    // single PID so a restoring write reproduces the original contents.
    f.write("sys/fs/cgroup/cgroup.subtree_control", "memory pids");
    f.write("sys/fs/cgroup/cpuset.mems.effective", "0\n");
    f.write("sys/fs/cgroup/user.slice/cgroup.procs", "4242");
    f
}

fn katana_cfg(f: &Fixture) -> GameModeConfig {
    GameModeConfig {
        tier: Tier::Performance,
        fan_mode: Some("max".into()),
        cpuset: "p-cores".into(),
        cgroup: f.abs("sys/fs/cgroup/rime-game"),
        irq: "away-from-game".into(),
        irq_pin_to_game: vec!["nvidia".into()],
        nvidia: NvidiaConfig {
            enabled: true,
            persistence: true,
            graphics_clock: Some(ClockSpec::Range([1200, 1620])),
            memory_clock: Some(ClockSpec::Keyword("max".into())),
            gpu_index: None,
        },
        ..GameModeConfig::default()
    }
}

#[test]
fn enter_then_exit_restores_the_filesystem_byte_for_byte() {
    let f = machine("roundtrip");
    let cfg = katana_cfg(&f);
    let topo = CoreTopology::detect_from(&f.path().join("sys"));
    let irqs = irq::enumerate(&f.path().join("proc/irq"));
    let placements = vec![PidPlacement {
        pid: 4242,
        prior_cgroup: Some(f.abs("sys/fs/cgroup/user.slice")),
    }];
    let plan = game::plan(&GameInputs {
        cfg: &cfg,
        topo: &topo,
        nvidia: &[], // covered separately; nvidia-smi is not on the test host
        gpus: &[],
        irqs: &irqs,
        pids: &placements,
        mems: "0".into(),
        irqbalance: false,
    });
    assert_eq!(plan.cpu_list(), "0-11");

    let before = f.snapshot();
    let writer = RealWriter::new(false);

    // ── enter ────────────────────────────────────────────────────────────────
    for a in &plan.enter {
        writer.apply(a).unwrap();
    }
    assert_eq!(f.read("sys/fs/cgroup/rime-game/cpuset.cpus"), "0-11");
    assert_eq!(f.read("sys/fs/cgroup/rime-game/cpuset.mems"), "0");
    assert_eq!(f.read("sys/fs/cgroup/rime-game/cgroup.procs"), "4242");
    assert_eq!(f.read("proc/irq/24/smp_affinity_list"), "12-19", "housekeeping IRQ parked on the E-cores");
    assert_eq!(f.read("proc/irq/16/smp_affinity_list"), "0-11", "the GPU IRQ follows the game");
    assert_eq!(f.read("proc/irq/0/smp_affinity_list"), "0-19", "the timer IRQ is never touched");

    // ── exit ─────────────────────────────────────────────────────────────────
    for a in &plan.exit {
        writer.apply(a).unwrap();
    }
    assert!(
        !f.path().join("sys/fs/cgroup/rime-game").exists(),
        "the session cgroup is torn down"
    );

    let mut after = f.snapshot();
    // cgroup.subtree_control is write-a-delta / read-a-list in the kernel
    // ("+cpuset" enables the controller; reading returns the enabled set). A
    // plain fixture file cannot model that, so it is compared separately.
    let sc = f.abs("sys/fs/cgroup/cgroup.subtree_control");
    assert_eq!(
        after.remove(&sc).as_deref(),
        Some("+cpuset"),
        "the cpuset controller is enabled on the parent"
    );
    let mut expected = before.clone();
    expected.remove(&sc);
    assert_eq!(after, expected, "exit must leave every other file exactly as it found it");
}

#[test]
fn exit_is_idempotent() {
    let f = machine("idempotent");
    let cfg = katana_cfg(&f);
    let topo = CoreTopology::detect_from(&f.path().join("sys"));
    let irqs = irq::enumerate(&f.path().join("proc/irq"));
    let plan = game::plan(&GameInputs {
        cfg: &cfg,
        topo: &topo,
        nvidia: &[],
        gpus: &[],
        irqs: &irqs,
        pids: &[],
        mems: "0".into(),
        irqbalance: false,
    });
    let writer = RealWriter::new(false);
    for a in &plan.enter {
        writer.apply(a).unwrap();
    }
    for _ in 0..3 {
        for a in &plan.exit {
            writer.apply(a).unwrap();
        }
    }
    assert_eq!(f.read("proc/irq/24/smp_affinity_list"), "0-19");
    assert!(!f.path().join("sys/fs/cgroup/rime-game").exists());
}

#[test]
fn a_uniform_machine_plans_no_pinning_and_no_steering() {
    // The L16: 16 uniform threads. Pinning to "p-cores" degrades to all CPUs,
    // which in turn disables IRQ steering (there is nowhere to steer to).
    let f = Fixture::new("uniform");
    for c in 0..16u32 {
        fs::create_dir_all(f.path().join(format!("sys/devices/system/cpu/cpu{c}"))).unwrap();
    }
    f.write("sys/devices/system/cpu/online", "0-15\n");
    f.write("proc/irq/24/smp_affinity_list", "0-15\n");

    let cfg = GameModeConfig {
        cgroup: f.abs("sys/fs/cgroup/rime-game"),
        ..GameModeConfig::default()
    };
    let topo = CoreTopology::detect_from(&f.path().join("sys"));
    let irqs = irq::enumerate(&f.path().join("proc/irq"));
    let plan = game::plan(&GameInputs {
        cfg: &cfg,
        topo: &topo,
        nvidia: &[],
        gpus: &[],
        irqs: &irqs,
        pids: &[],
        mems: "0".into(),
        irqbalance: false,
    });
    assert_eq!(plan.irqs_attempted, 0);
    assert!(!plan.enter.iter().any(|a| matches!(a, Action::IrqAffinity { .. })));
    assert!(plan.notes.iter().any(|n| n.contains("no P/E split")));
}

#[test]
fn irq_policy_off_leaves_interrupts_alone() {
    let f = machine("irq-off");
    let cfg = GameModeConfig {
        irq: "off".into(),
        cgroup: f.abs("sys/fs/cgroup/rime-game"),
        ..GameModeConfig::default()
    };
    assert_eq!(cfg.irq_policy(), rimed_core::IrqPolicy::Off);
    let topo = CoreTopology::detect_from(&f.path().join("sys"));
    let irqs = irq::enumerate(&f.path().join("proc/irq"));
    let plan = game::plan(&GameInputs {
        cfg: &cfg,
        topo: &topo,
        nvidia: &[],
        gpus: &[],
        irqs: &irqs,
        pids: &[],
        mems: "0".into(),
        irqbalance: false,
    });
    assert_eq!(plan.irqs_attempted, 0);
    assert!(plan.enter.iter().any(|a| matches!(a, Action::CgroupEnsure { .. })));
}

#[test]
fn cpuset_off_plans_nothing_at_all() {
    let f = machine("cpuset-off");
    let cfg = GameModeConfig {
        cpuset: "off".into(),
        cgroup: f.abs("sys/fs/cgroup/rime-game"),
        // scx is explicitly cleared so this keeps asserting exactly what it was
        // written to assert: with no cpuset work configured, the plan is EMPTY.
        // sched-ext now defaults to scx_lavd, which is legitimately planned
        // independently of cpuset (turning CPU pinning off is not the same as
        // turning game mode off — `enabled = false` is), and that default has
        // its own coverage in `the_default_scx_is_the_only_thing_planned_...`
        // below. Clearing it here keeps the original guard intact instead of
        // loosening it to accommodate the new action.
        scx: String::new(),
        ..GameModeConfig::default()
    };
    assert_eq!(cfg.cpuset_policy(), CpusetPolicy::Off);
    let topo = CoreTopology::detect_from(&f.path().join("sys"));
    let plan = game::plan(&GameInputs {
        cfg: &cfg,
        topo: &topo,
        nvidia: &[],
        gpus: &[],
        irqs: &irq::enumerate(&f.path().join("proc/irq")),
        pids: &[],
        mems: "0".into(),
        irqbalance: false,
    });
    assert!(plan.enter.is_empty());
    assert!(plan.exit.is_empty());
}

#[test]
fn an_explicit_cpuset_is_honoured_and_validated() {
    let f = machine("explicit");
    let topo = CoreTopology::detect_from(&f.path().join("sys"));

    let cfg = GameModeConfig {
        cpuset: "0-7".into(),
        cgroup: f.abs("sys/fs/cgroup/rime-game"),
        ..GameModeConfig::default()
    };
    let mut notes = Vec::new();
    assert_eq!(
        rimed_core::game::resolve_cpus(&cfg, &topo, &mut notes),
        (0..=7).collect::<Vec<u32>>()
    );

    // A list that matches nothing online falls back to every CPU with a note.
    let cfg = GameModeConfig {
        cpuset: "64-71".into(),
        ..cfg
    };
    let mut notes = Vec::new();
    assert_eq!(
        rimed_core::game::resolve_cpus(&cfg, &topo, &mut notes).len(),
        20
    );
    assert!(notes.iter().any(|n| n.contains("matches no online CPU")));
}

#[test]
fn irq_enumeration_skips_interrupts_with_no_affinity_control() {
    let f = Fixture::new("irq-enum");
    f.write("proc/irq/24/smp_affinity_list", "0-7\n");
    fs::create_dir_all(f.path().join("proc/irq/24/xhci_hcd")).unwrap();
    // A per-CPU interrupt with no smp_affinity_list at all.
    fs::create_dir_all(f.path().join("proc/irq/31")).unwrap();
    // A non-numeric entry (procfs has `default_smp_affinity` at the top level).
    f.write("proc/irq/default_smp_affinity", "ffff\n");

    let entries = irq::enumerate(&f.path().join("proc/irq"));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].irq, 24);
    assert_eq!(entries[0].actions, vec!["xhci_hcd".to_string()]);
    assert!(entries[0].matches(&["xhci".to_string()]));
    assert!(!entries[0].matches(&["nvidia".to_string()]));

    assert!(irq::enumerate(Path::new("/nonexistent/rimed-irq")).is_empty());
}

#[test]
fn already_correct_affinities_are_not_rewritten() {
    let entries = vec![IrqEntry {
        irq: 24,
        path: "/proc/irq/24/smp_affinity_list".into(),
        affinity: "12-19".into(),
        actions: vec!["xhci_hcd".into()],
    }];
    let (steer, restore) = irq::plan_steer(&entries, &(0..=11).collect::<Vec<u32>>(), &(12..=19).collect::<Vec<u32>>(), &[]);
    assert!(steer.is_empty(), "no write, so nothing to restore either");
    assert!(restore.is_empty());
}

#[test]
fn a_running_irqbalance_is_called_out() {
    let f = machine("irqbalance");
    let cfg = katana_cfg(&f);
    let topo = CoreTopology::detect_from(&f.path().join("sys"));
    let irqs = irq::enumerate(&f.path().join("proc/irq"));
    let plan = game::plan(&GameInputs {
        cfg: &cfg,
        topo: &topo,
        nvidia: &[],
        gpus: &[],
        irqs: &irqs,
        pids: &[PidPlacement {
            pid: 4242,
            prior_cgroup: Some(f.abs("sys/fs/cgroup/user.slice")),
        }],
        mems: "0".into(),
        irqbalance: true,
    });
    assert!(plan.irqs_attempted > 0);
    assert!(
        plan.notes.iter().any(|n| n.contains("irqbalance")),
        "steering while irqbalance runs must be reported, not silently lost"
    );

    // Detection itself, against a synthetic /proc.
    let g = Fixture::new("proc-scan");
    g.write("proc/1/comm", "systemd\n");
    g.write("proc/812/comm", "irqbalance\n");
    g.write("proc/self/comm", "cargo\n");
    assert!(irq::irqbalance_running(&g.path().join("proc")));
    let h = Fixture::new("proc-scan-clean");
    h.write("proc/1/comm", "systemd\n");
    assert!(!irq::irqbalance_running(&h.path().join("proc")));
    assert!(!irq::irqbalance_running(Path::new("/nonexistent/rimed-proc")));
}

#[test]
fn nvidia_locks_are_clamped_to_what_the_gpu_reports() {
    let gpu_info = NvidiaGpu {
        index: 0,
        name: "NVIDIA GeForce RTX 3070 Laptop GPU".into(),
        max_graphics_mhz: Some(1620),
        max_memory_mhz: Some(6001),
        persistence: Some(false),
    };
    let cfg = NvidiaConfig {
        enabled: true,
        persistence: true,
        // Deliberately over-ambitious: must be clamped, not passed through.
        graphics_clock: Some(ClockSpec::Range([1200, 2400])),
        memory_clock: Some(ClockSpec::Keyword("max".into())),
        gpu_index: None,
    };
    assert_eq!(
        gpu::plan_lock(&cfg, &gpu_info),
        vec![
            Action::NvidiaPersistence { gpu: 0, enabled: true },
            Action::NvidiaLockGraphics { gpu: 0, min_mhz: 1200, max_mhz: 1620 },
            Action::NvidiaLockMemory { gpu: 0, min_mhz: 6001, max_mhz: 6001 },
        ]
    );
    // Exit releases both locks and puts persistence back where it was.
    assert_eq!(
        gpu::plan_unlock(&cfg, &gpu_info),
        vec![
            Action::NvidiaResetGraphics { gpu: 0 },
            Action::NvidiaResetMemory { gpu: 0 },
            Action::NvidiaPersistence { gpu: 0, enabled: false },
        ]
    );
}

#[test]
fn nvidia_is_a_no_op_without_a_gpu_or_when_disabled() {
    let cfg = NvidiaConfig {
        enabled: false,
        ..NvidiaConfig::default()
    };
    let gpu_info = NvidiaGpu {
        index: 0,
        max_graphics_mhz: Some(1620),
        ..NvidiaGpu::default()
    };
    assert!(gpu::plan_lock(&cfg, &gpu_info).is_empty());
    assert!(gpu::plan_unlock(&cfg, &gpu_info).is_empty());

    // Enabled, but the GPU reports no maximum clock: nothing is locked, because
    // an unvalidated MHz value must never reach the driver.
    let cfg = NvidiaConfig {
        graphics_clock: Some(ClockSpec::Fixed(1500)),
        ..NvidiaConfig::default()
    };
    let unknown = NvidiaGpu {
        index: 0,
        max_graphics_mhz: None,
        ..NvidiaGpu::default()
    };
    assert_eq!(
        gpu::plan_lock(&cfg, &unknown),
        vec![Action::NvidiaPersistence { gpu: 0, enabled: true }]
    );

    // No nvidia-smi at all.
    let mock = MockNvidiaSmi::default();
    assert!(!mock.available());
    assert!(mock.query().is_empty());
}

#[test]
fn nvidia_query_parsing_tolerates_na_fields() {
    let gpus = gpu::parse_query(
        "0, NVIDIA GeForce RTX 3070 Laptop GPU, 1620, 6001, Disabled\n\
         1, NVIDIA T400, [N/A], [N/A], Enabled\n",
    );
    assert_eq!(gpus.len(), 2);
    assert_eq!(gpus[0].max_graphics_mhz, Some(1620));
    assert_eq!(gpus[0].persistence, Some(false));
    assert_eq!(gpus[1].max_graphics_mhz, None);
    assert_eq!(gpus[1].persistence, Some(true));
}

#[test]
fn a_session_with_a_gpu_locks_and_unlocks_it() {
    let f = machine("with-gpu");
    let cfg = katana_cfg(&f);
    let topo = CoreTopology::detect_from(&f.path().join("sys"));
    let nvidia = MockNvidiaSmi {
        available: true,
        gpus: vec![NvidiaGpu {
            index: 0,
            name: "NVIDIA GeForce RTX 3070 Laptop GPU".into(),
            max_graphics_mhz: Some(1620),
            max_memory_mhz: Some(6001),
            persistence: Some(false),
        }],
        // Game planning does not read VRAM; spelled with the struct-update
        // syntax so a future querier field does not break this case again.
        ..Default::default()
    };
    let plan = game::plan(&GameInputs {
        cfg: &cfg,
        topo: &topo,
        nvidia: &nvidia.query(),
        gpus: &[],
        irqs: &irq::enumerate(&f.path().join("proc/irq")),
        pids: &[],
        mems: "0".into(),
        irqbalance: false,
    });
    assert_eq!(plan.gpus_locked, vec![0]);
    assert!(plan.enter.contains(&Action::NvidiaLockGraphics {
        gpu: 0,
        min_mhz: 1200,
        max_mhz: 1620
    }));
    // The GPU is released before anything else on the way out.
    assert_eq!(plan.exit.first(), Some(&Action::NvidiaResetGraphics { gpu: 0 }));
    assert!(plan.exit.contains(&Action::NvidiaResetMemory { gpu: 0 }));
}

#[test]
fn pid_cgroup_lookup_reads_the_v2_line() {
    let f = Fixture::new("pidcg");
    f.write(
        "proc/4242/cgroup",
        "0::/user.slice/user-1000.slice/session-3.scope\n",
    );
    assert_eq!(
        game::read_pid_cgroup(&f.path().join("proc"), "/sys/fs/cgroup", 4242),
        Some("/sys/fs/cgroup/user.slice/user-1000.slice/session-3.scope".to_string())
    );
    // cgroup v1 lines only -> no v2 path.
    f.write("proc/99/cgroup", "1:cpuset:/\n2:memory:/\n");
    assert_eq!(game::read_pid_cgroup(&f.path().join("proc"), "/sys/fs/cgroup", 99), None);
    // A process that has gone away.
    assert_eq!(game::read_pid_cgroup(&f.path().join("proc"), "/sys/fs/cgroup", 7), None);
}

#[test]
fn cgroup_mems_falls_back_to_node_zero() {
    let f = Fixture::new("mems");
    assert_eq!(game::read_cgroup_mems(&f.path().join("sys/fs/cgroup")), "0");
    f.write("sys/fs/cgroup/cpuset.mems.effective", "0-1\n");
    assert_eq!(game::read_cgroup_mems(&f.path().join("sys/fs/cgroup")), "0-1");
}

// ── sched-ext (scx) ──────────────────────────────────────────────────────────
// The kernel has shipped CONFIG_SCHED_CLASS_EXT=y and sixteen scx schedulers
// since M1 with nothing selecting one. These pin the switch that fixes that:
// that it is planned at all, that it is ORDERED correctly around the cpuset
// work, and that it stays absent for every profile that does not ask.

/// A plan built on the Katana fixture with `scx` set to whatever is under test.
/// Uses the same fixture as the rest of this file so the surrounding cpuset/IRQ
/// actions are real — the ordering assertions below only mean something against
/// a plan that actually contains other work.
fn scx_plan(scx: &str) -> (Fixture, game::GamePlan) {
    let f = machine(&format!("scx{}", scx.trim().len()));
    let cfg = GameModeConfig {
        scx: scx.to_string(),
        ..katana_cfg(&f)
    };
    let topo = CoreTopology::detect_from(&f.path().join("sys"));
    let irqs = irq::enumerate(&f.path().join("proc/irq"));
    let placements = vec![PidPlacement {
        pid: 4242,
        prior_cgroup: Some(f.abs("sys/fs/cgroup/user.slice")),
    }];
    let plan = game::plan(&GameInputs {
        cfg: &cfg,
        topo: &topo,
        nvidia: &[],
        gpus: &[],
        irqs: &irqs,
        pids: &placements,
        mems: "0".into(),
        irqbalance: false,
    });
    (f, plan)
}

#[test]
fn scx_defaults_to_auto() {
    // Every machine's game mode still asks for a scheduler by default, so
    // Gaming Mode is tuned on hardware other than the author's; `auto` decides
    // which one from the CPU (tests below).
    assert_eq!(GameModeConfig::default().scx, "auto");
}

/// The same machine as `machine()`, minus the P/E split: 16 CPUs of one kind.
/// `acpi_cppc/highest_perf` still differs per CPU, the way AMD ranks its
/// preferred cores (values from a Ryzen 7 PRO 250), and `cpu_capacity` is 1024
/// everywhere.
fn uniform_machine(tag: &str) -> Fixture {
    let f = Fixture::new(tag);
    let ranks = [202, 202, 196, 196, 208, 208, 232, 232, 214, 214, 232, 232, 226, 226, 220, 220];
    for (c, rank) in ranks.iter().enumerate() {
        f.write(&format!("sys/devices/system/cpu/cpu{c}/cpu_capacity"), "1024\n");
        f.write(&format!("sys/devices/system/cpu/cpu{c}/acpi_cppc/highest_perf"), &format!("{rank}\n"));
    }
    f.write("sys/devices/system/cpu/online", "0-15\n");
    f.write("proc/irq/24/smp_affinity_list", "0-15\n");
    fs::create_dir_all(f.path().join("proc/irq/24/xhci_hcd")).unwrap();
    f.write("sys/fs/cgroup/cgroup.subtree_control", "memory pids");
    f.write("sys/fs/cgroup/cpuset.mems.effective", "0\n");
    f
}

fn default_plan(f: &Fixture) -> game::GamePlan {
    let cfg = GameModeConfig {
        cgroup: f.abs("sys/fs/cgroup/rime-game"),
        ..GameModeConfig::default()
    };
    let topo = CoreTopology::detect_from(&f.path().join("sys"));
    game::plan(&GameInputs {
        cfg: &cfg,
        topo: &topo,
        nvidia: &[],
        gpus: &[],
        irqs: &irq::enumerate(&f.path().join("proc/irq")),
        pids: &[],
        mems: "0".into(),
        irqbalance: false,
    })
}

#[test]
fn auto_gives_a_p_e_hybrid_the_kernels_own_scheduler() {
    // katana, 2026-10-07: under scx_lavd Cyberpunk's 98 %-busy main thread sat
    // on an E-core a third of the time, and the game ran slower than on the
    // desktop, which runs the kernel's scheduler. On a P/E CPU `auto` is that
    // scheduler: nothing to start, nothing to stop.
    let f = machine("scx-auto-hybrid");
    let plan = default_plan(&f);
    assert!(
        !plan.enter.iter().any(|a| matches!(a, Action::ScxSwitch { .. })),
        "auto must not load a scheduler on a P/E CPU: {:?}",
        plan.enter
    );
    assert!(!plan.exit.iter().any(|a| matches!(a, Action::ScxStop)));
    let note = plan
        .notes
        .iter()
        .find(|n| n.starts_with("sched-ext:"))
        .unwrap_or_else(|| panic!("the choice must be explained: {:?}", plan.notes));
    assert!(note.contains("0-11") && note.contains("12-19"), "name the cores: {note}");
    assert!(note.contains("scx_lavd"), "say how to get lavd back: {note}");
}

#[test]
fn auto_still_loads_lavd_on_one_kind_of_core_even_with_ranked_cores() {
    // The negative control, and the trap it guards: AMD's preferred-core
    // ranking makes `is_hybrid()` read 16 identical cores as 4 P + 12 E. That
    // is not a second kind of core, and `auto` must not drop lavd for it.
    let f = uniform_machine("scx-auto-uniform");
    let topo = CoreTopology::detect_from(&f.path().join("sys"));
    assert!(topo.is_hybrid(), "the fixture must reproduce the ranked-core split");
    assert!(!topo.has_efficiency_cores());
    let plan = default_plan(&f);
    assert_eq!(
        plan.enter.first(),
        Some(&Action::ScxSwitch { sched: "scx_lavd".into() }),
        "auto must load lavd on a CPU with one kind of core"
    );
    assert_eq!(plan.exit.last(), Some(&Action::ScxStop));
}

#[test]
fn scx_keywords_resolve_against_the_cpu() {
    let hybrid = machine("scx-kw-hybrid");
    let uniform = uniform_machine("scx-kw-uniform");
    let h = CoreTopology::detect_from(&hybrid.path().join("sys"));
    let u = CoreTopology::detect_from(&uniform.path().join("sys"));
    let with = |scx: &str| GameModeConfig { scx: scx.into(), ..GameModeConfig::default() };
    for off in ["", "  ", "off", "none", "OFF"] {
        assert_eq!(with(off).scx_for(&h), None, "{off:?} on hybrid");
        assert_eq!(with(off).scx_for(&u), None, "{off:?} on uniform");
    }
    assert_eq!(with("auto").scx_for(&h), None);
    assert_eq!(with("Auto").scx_for(&u), Some("scx_lavd".into()));
    // A named scheduler is a decision the profile made: it loads anywhere.
    assert_eq!(with("scx_lavd").scx_for(&h), Some("scx_lavd".into()));
    assert_eq!(with("scx_bpfland").scx_for(&u), Some("scx_bpfland".into()));
}

#[test]
fn an_empty_scx_opts_out_entirely() {
    // A profile must be able to say "leave the scheduler alone" and have
    // NOTHING planned — not a switch, and not a stop on the way out.
    let (_f, plan) = scx_plan("");
    assert!(
        !plan.enter.iter().any(|a| matches!(a, Action::ScxSwitch { .. })),
        "scx = \"\" must plan no scheduler switch"
    );
    assert!(
        !plan.exit.iter().any(|a| matches!(a, Action::ScxStop)),
        "scx = \"\" must not plan a stop either"
    );
}

#[test]
fn scx_switches_on_enter_and_stops_on_exit() {
    let (_f, plan) = scx_plan("scx_lavd");
    assert!(plan.enter.contains(&Action::ScxSwitch {
        sched: "scx_lavd".into()
    }));
    assert!(plan.exit.contains(&Action::ScxStop));
}

#[test]
fn whitespace_only_scx_is_treated_as_unset() {
    // A profile with `scx = "  "` means "no", not "load a scheduler called
    // nothing" — scxctl would fail confusingly.
    let (_f, plan) = scx_plan("   ");
    assert!(!plan.enter.iter().any(|a| matches!(a, Action::ScxSwitch { .. })));
}

#[test]
fn scx_is_first_on_enter_and_last_on_exit() {
    // Ordering is the substance, not cosmetics: swapping the scheduler migrates
    // every runnable task, so it must happen BEFORE the game is confined to its
    // cpuset and be undone AFTER that confinement is unwound. Otherwise the swap
    // shuffles tasks that are mid-move.
    let (_f, plan) = scx_plan("scx_lavd");
    assert!(
        matches!(plan.enter.first(), Some(Action::ScxSwitch { .. })),
        "scx must be the first enter action, got {:?}",
        plan.enter.first()
    );
    assert!(
        matches!(plan.exit.last(), Some(Action::ScxStop)),
        "scx stop must be the last exit action, got {:?}",
        plan.exit.last()
    );
}

#[test]
fn the_plan_note_asks_for_a_scheduler_and_does_not_claim_one() {
    // A PLAN cannot know what the machine did with it, and this note is what
    // `rime game status` carried as its only sched-ext surface. It read
    //
    //     sched-ext: scx_lavd for the session, kernel scheduler restored on exit
    //
    // and on katana it was printed directly beneath the journal line recording
    // `scxctl` refusing the call — for three consecutive boots, on two
    // different images. Nobody could have caught it, because the sentence was
    // copied out of the plan and never checked against anything.
    //
    // Both directions matter. A note that stops naming the scheduler is no
    // use either, so the name is required as well.
    let (_f, plan) = scx_plan("scx_lavd");
    let note = plan
        .notes
        .iter()
        .find(|n| n.contains("sched-ext"))
        .unwrap_or_else(|| panic!("the plan must still explain its scx step: {:?}", plan.notes));

    assert!(
        note.contains("scx_lavd"),
        "the note must name the scheduler it plans: {note}"
    );
    assert!(
        note.to_lowercase().contains("asks"),
        "a plan ASKS; it is in no position to report. Got: {note}"
    );
    assert!(
        note.contains("scx_state"),
        "and it must point at the surface that DOES report, or the reader has \
         nowhere to go for the answer: {note}"
    );
    assert!(
        !note.contains("for the session, kernel scheduler restored on exit"),
        "this is the exact sentence that asserted a scheduler that had never \
         loaded — it must not come back: {note}"
    );
}

#[test]
fn a_profile_that_asks_for_no_scheduler_plans_no_note_about_one() {
    // The other direction: `scx = ""` must produce silence, not a note
    // explaining a step that is not in the plan.
    let (_f, plan) = scx_plan("");
    assert!(
        !plan.notes.iter().any(|n| n.contains("sched-ext")),
        "nothing was planned, so nothing is to be said: {:?}",
        plan.notes
    );
}

#[test]
fn the_default_scx_is_the_only_thing_planned_when_cpuset_is_off() {
    // The complement of cpuset_off_plans_nothing_at_all: with the shipped
    // default, `cpuset = "off"` plans the scheduler switch and NOTHING else. If
    // a future change starts planning cgroup work behind an off cpuset, this
    // fails rather than hiding behind "well, the plan is non-empty now".
    // On a CPU with one kind of core, where `auto` is lavd.
    let f = uniform_machine("cpuset-off-scx");
    let cfg = GameModeConfig {
        cpuset: "off".into(),
        cgroup: f.abs("sys/fs/cgroup/rime-game"),
        ..GameModeConfig::default()
    };
    let topo = CoreTopology::detect_from(&f.path().join("sys"));
    let plan = game::plan(&GameInputs {
        cfg: &cfg,
        topo: &topo,
        nvidia: &[],
        gpus: &[],
        irqs: &irq::enumerate(&f.path().join("proc/irq")),
        pids: &[],
        mems: "0".into(),
        irqbalance: false,
    });
    assert_eq!(
        plan.enter,
        vec![Action::ScxSwitch { sched: "scx_lavd".into() }],
        "cpuset off must plan the scheduler switch and nothing else"
    );
    assert_eq!(plan.exit, vec![Action::ScxStop]);
}

// ── interrupts move only when a game is actually in the cpuset ──────────────

#[test]
fn a_session_with_no_game_in_its_cpuset_moves_no_interrupts() {
    // Gaming Mode's own call: `rime game start --owner-pid $$`, no game. On
    // katana the plan still moved 47 interrupts onto the E-cores, which the
    // game (running on all 20 CPUs, outside the empty cpuset) was using too.
    let f = machine("irq-no-game");
    let cfg = katana_cfg(&f);
    let topo = CoreTopology::detect_from(&f.path().join("sys"));
    let irqs = irq::enumerate(&f.path().join("proc/irq"));
    let plan = game::plan(&GameInputs {
        cfg: &cfg,
        topo: &topo,
        nvidia: &[],
        gpus: &[],
        irqs: &irqs,
        pids: &[],
        mems: "0".into(),
        irqbalance: false,
    });
    assert_eq!(plan.irqs_attempted, 0);
    assert!(
        !plan.enter.iter().any(|a| matches!(a, Action::IrqAffinity { .. })),
        "no game in the cpuset, so no interrupt moves: {:?}",
        plan.enter
    );
    assert!(
        plan.notes.iter().any(|n| n.starts_with("IRQ steering waits for a game")),
        "and status says so: {:?}",
        plan.notes
    );
    // The cpuset itself is still made ready for a game to be attached to.
    assert!(plan.enter.iter().any(|a| matches!(a, Action::CgroupEnsure { .. })));

    // The control: the same plan WITH a game moves them, so the zero above is
    // the rule and not a fixture with nothing to steer.
    let with_game = game::plan(&GameInputs {
        cfg: &cfg,
        topo: &topo,
        nvidia: &[],
        gpus: &[],
        irqs: &irqs,
        pids: &[PidPlacement { pid: 4242, prior_cgroup: None }],
        mems: "0".into(),
        irqbalance: false,
    });
    assert!(with_game.irqs_attempted > 0);
}

#[test]
fn the_first_attach_moves_the_interrupts_the_plan_held_back() {
    let f = machine("irq-on-attach");
    let cfg = katana_cfg(&f);
    let topo = CoreTopology::detect_from(&f.path().join("sys"));
    let irqs = irq::enumerate(&f.path().join("proc/irq"));
    let pcores: Vec<u32> = (0..=11).collect();

    // Exactly what a plan with the game in it from the start would steer.
    let (steer, restore) = game::steer_on_attach(&cfg, &topo, &irqs, &pcores);
    let (want_steer, want_restore) =
        irq::plan_steer(&irqs, &pcores, &topo.complement(&pcores), &cfg.irq_pin_to_game);
    assert!(!steer.is_empty());
    assert_eq!(steer, want_steer);
    assert_eq!(restore, want_restore);

    // Nothing when the profile does not steer, or the cpuset confines nothing.
    let off = GameModeConfig { irq: "off".into(), ..katana_cfg(&f) };
    assert_eq!(game::steer_on_attach(&off, &topo, &irqs, &pcores), (vec![], vec![]));
    assert_eq!(game::steer_on_attach(&cfg, &topo, &irqs, &topo.all), (vec![], vec![]));
    assert_eq!(game::steer_on_attach(&cfg, &topo, &irqs, &[]), (vec![], vec![]));
}

// ── the session owner: who the daemon watches so a torn-down session releases ─
//
// These stand behind the fix for the katana 2026-09-19 defect (evidence §3.4):
// Gaming Mode could not release itself when logind deactivated its session,
// because the EXIT trap's `rime game stop` is a polkit `allow_active=yes`
// action and a deactivated session is no longer active. rimed now watches the
// process that asked for game mode and releases it when that process dies.
// Everything below is the reading that decision is made from.

use rimed_core::game::{owner_for_pid, owner_state, parse_proc_stat, OwnerState, SessionOwner};

/// A `/proc/<pid>/stat` line in the kernel's real shape. Fields 1 and 2 are
/// `pid` and `(comm)`; everything after the closing paren is field 3 onward.
fn stat_line(pid: u32, comm: &str, state: char, starttime: u64) -> String {
    let mut fields: Vec<String> = Vec::new();
    // fields 4..=21 — their values do not matter here, only their COUNT does.
    for n in 4..=21 {
        fields.push(n.to_string());
    }
    format!(
        "{pid} ({comm}) {state} {} {starttime} 0 0 0",
        fields.join(" ")
    )
}

fn proc_fixture(tag: &str) -> Fixture {
    Fixture::new(tag)
}

#[test]
fn starttime_is_read_after_the_last_paren_so_a_comm_with_spaces_cannot_shift_it() {
    // The same process, named three ways. A whitespace-split from the start of
    // the line reads a different field for each of these; the parse must not.
    for comm in ["bash", "rime gaming session", "my prog (old)", "a) b (c"] {
        let line = stat_line(4242, comm, 'S', 99_887_766);
        let st = parse_proc_stat(&line)
            .unwrap_or_else(|| panic!("comm {comm:?} made the stat line unparseable"));
        assert_eq!(
            st.starttime, 99_887_766,
            "comm {comm:?} shifted the start time; field 2 was not skipped by the last ')'"
        );
        assert_eq!(st.state, 'S', "comm {comm:?} shifted the state field");
    }
}

#[test]
fn a_truncated_stat_line_is_unparseable_rather_than_wrong() {
    // 22 fields are needed. A line with 21 must yield None — NOT a zero, which
    // would compare unequal to every recorded start time and release game mode
    // on every tick.
    let short = format!("7 (x) S {}", (4..=21).map(|n| n.to_string()).collect::<Vec<_>>().join(" "));
    assert_eq!(parse_proc_stat(&short), None);
    assert_eq!(parse_proc_stat(""), None);
    assert_eq!(parse_proc_stat("7 x S 1 2 3"), None, "no ')' at all");
}

#[test]
fn an_owner_that_is_still_running_is_alive() {
    let f = proc_fixture("owner-alive");
    f.write("proc/900/stat", &stat_line(900, "rime-gaming-ses", 'S', 12345));
    let owner = owner_for_pid(&f.path().join("proc"), 900).expect("the fixture pid is readable");
    assert_eq!(owner, SessionOwner { pid: 900, starttime: 12345 });
    assert_eq!(owner_state(&f.path().join("proc"), &owner), OwnerState::Alive);
}

#[test]
fn an_owner_whose_proc_entry_vanished_is_gone() {
    let f = proc_fixture("owner-gone");
    f.write("proc/901/stat", &stat_line(901, "rime-gaming-ses", 'S', 12345));
    let proc = f.path().join("proc");
    let owner = owner_for_pid(&proc, 901).unwrap();
    assert_eq!(owner_state(&proc, &owner), OwnerState::Alive);

    fs::remove_dir_all(proc.join("901")).unwrap();
    match owner_state(&proc, &owner) {
        OwnerState::Gone(why) => assert!(why.contains("/proc/901"), "why was {why:?}"),
        other => panic!("a vanished owner read as {other:?}"),
    }
}

#[test]
fn a_reused_pid_is_gone_and_not_alive() {
    // The whole reason the start time is recorded. Same number, different
    // process: a bare-PID watch would call this Alive and pin the machine in a
    // gaming power profile for as long as the impostor lives.
    let f = proc_fixture("owner-reused");
    let proc = f.path().join("proc");
    f.write("proc/902/stat", &stat_line(902, "rime-gaming-ses", 'S', 12345));
    let owner = owner_for_pid(&proc, 902).unwrap();

    f.write("proc/902/stat", &stat_line(902, "something-else", 'S', 999_999));
    match owner_state(&proc, &owner) {
        OwnerState::Gone(why) => assert!(why.contains("reused"), "why was {why:?}"),
        other => panic!("a reused pid read as {other:?}"),
    }
}

#[test]
fn a_zombie_owner_is_gone() {
    // It has exited and holds nothing; only an unreaped parent keeps the
    // directory. Waiting for the reap would mean a greetd that died mid-
    // teardown leaves the machine in the gaming profile indefinitely.
    let f = proc_fixture("owner-zombie");
    let proc = f.path().join("proc");
    f.write("proc/903/stat", &stat_line(903, "rime-gaming-ses", 'Z', 12345));
    let owner = SessionOwner { pid: 903, starttime: 12345 };
    match owner_state(&proc, &owner) {
        OwnerState::Gone(why) => assert!(why.contains("zombie"), "why was {why:?}"),
        other => panic!("a zombie owner read as {other:?}"),
    }
}

#[test]
fn an_unreadable_proc_is_a_third_answer_and_never_a_release() {
    // Fail closed. An unreadable /proc has measured nothing, and turning that
    // into a release would make an I/O error change the machine's power state.
    let f = proc_fixture("owner-unreadable");
    let owner = SessionOwner { pid: 904, starttime: 12345 };

    match owner_state(&f.path().join("no-such-proc"), &owner) {
        OwnerState::Unknown(why) => assert!(why.contains("no-such-proc"), "why was {why:?}"),
        other => panic!("an absent /proc read as {other:?}"),
    }

    // Present but garbage: also Unknown, not Gone.
    f.write("proc/904/stat", "this is not a stat line");
    match owner_state(&f.path().join("proc"), &owner) {
        OwnerState::Unknown(why) => assert!(why.contains("904"), "why was {why:?}"),
        other => panic!("an unparseable stat read as {other:?}"),
    }
}

#[test]
fn owner_for_pid_refuses_a_pid_that_does_not_exist() {
    let f = proc_fixture("owner-absent");
    assert_eq!(owner_for_pid(&f.path().join("proc"), 905), None);
}
