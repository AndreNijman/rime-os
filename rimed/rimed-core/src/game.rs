//! Game-mode orchestration planning: cpuset pinning, IRQ steering and NVIDIA
//! clock locks, resolved into a symmetric pair of action lists.
//!
//! The planner is pure. It takes the machine's topology, the interrupts as they
//! are *right now*, and what `nvidia-smi` reported, and returns both the enter
//! plan and the exit plan that undoes it. Building the exit plan up-front, from
//! values read before anything was written, is what makes "exit restores
//! exactly what enter changed" a property of the data rather than of a code
//! path that has to be remembered.
//!
//! Tier and fan changes are *not* in these lists — those go through the
//! daemon's existing tier engine and fan controller, which have their own
//! restore paths.

use crate::gpu::{self, GpuDevice, NvidiaGpu, SysfsGpuPrior};
use crate::irq::{self, IrqEntry};
use crate::profile::{CpusetPolicy, GameModeConfig, IrqPolicy};
use crate::tier::Action;
use crate::topology::{format_cpu_list, parse_cpu_list, CoreTopology};

/// The default cgroup-v2 mount point.
pub const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Read the cgroup-v2 root's effective NUMA nodes (for `cpuset.mems`).
/// Falls back to `0`, which is correct for every single-socket machine.
pub fn read_cgroup_mems(cgroup_root: &std::path::Path) -> String {
    for attr in ["cpuset.mems.effective", "cpuset.mems"] {
        if let Ok(s) = std::fs::read_to_string(cgroup_root.join(attr)) {
            let s = s.trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    "0".to_string()
}

/// The absolute cgroup-v2 directory a PID currently lives in, by reading
/// `/proc/<pid>/cgroup`. `None` when the process is gone or on cgroup v1.
pub fn read_pid_cgroup(proc_root: &std::path::Path, cgroup_root: &str, pid: u32) -> Option<String> {
    let text = std::fs::read_to_string(proc_root.join(pid.to_string()).join("cgroup")).ok()?;
    for line in text.lines() {
        // cgroup v2 lines look like `0::/user.slice/...`.
        if let Some(rest) = line.strip_prefix("0::") {
            let rel = rest.trim();
            let rel = rel.strip_prefix('/').unwrap_or(rel);
            return Some(if rel.is_empty() {
                cgroup_root.to_string()
            } else {
                format!("{cgroup_root}/{rel}")
            });
        }
    }
    None
}

// ── who owns a session, and how the daemon knows it is gone ─────────────────
//
// THE DEFECT THIS EXISTS FOR, measured on katana 2026-09-19
// (`ROADMAP/evidence/katana-image-qual-20260919.md` §3.4). Gaming Mode's
// session script releases game mode from an EXIT trap that runs
// `rime game stop`. That call goes through polkit action
// `org.rimeos.rimed.manage-power`, whose defaults are `allow_active=yes` and
// `auth_admin` for everything else. The moment logind stops calling the session
// *active* — a greetd restart, a VT switch away, any logind-driven teardown —
// the trap's own call is REFUSED, and the machine keeps a p-core cpuset, IRQ
// steering, the `performance` tier and `scx_lavd` with nothing able to undo
// them and no prompt anyone will ever see.
//
// The remedy is not to loosen that polkit rule: that would let any unprivileged
// local caller switch Gaming Mode off. It is for the daemon — which is already
// root, already holds the session's exit plan, and does not ask polkit anything
// about itself — to notice that the process which asked for game mode has died,
// and undo it. That is what these two types are read by.
//
// WHY A PID AND A START TIME AND NOT A PID. PIDs are reused. A bare-PID watch
// would keep game mode engaged forever the moment the kernel handed the
// session script's number to something else, which is the exact failure it is
// supposed to fix, only quieter. `starttime` (field 22 of `/proc/<pid>/stat`,
// in clock ticks since boot) is the kernel's own tiebreaker and is stable for
// the life of a process.

/// The process whose death ends a game-mode session.
///
/// Recorded at enter time. Not serialised anywhere: like the rest of
/// [`crate::game`]'s session state this lives in the daemon and dies with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionOwner {
    pub pid: u32,
    /// `starttime` from `/proc/<pid>/stat`, which disambiguates a reused PID.
    pub starttime: u64,
}

/// What one liveness reading concluded.
///
/// `Unknown` is a THIRD answer and not a quiet `Gone`: a `/proc` that cannot be
/// read has measured nothing, and releasing game mode on it would turn an
/// unreadable file into a hardware change. Callers must treat it as "leave the
/// session alone" and say so out loud.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerState {
    Alive,
    /// The owner is gone; the string is why, for the log line.
    Gone(String),
    /// The question could not be answered; the string is why.
    Unknown(String),
}

/// One process's state character and start time, from `/proc/<pid>/stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStat {
    /// Field 3: `R`, `S`, `D`, `Z`, `T`, …
    pub state: char,
    /// Field 22.
    pub starttime: u64,
}

/// Parse the fields of `/proc/<pid>/stat` this module needs.
///
/// **Split after the LAST `)`, not on whitespace from the start.** Field 2 is
/// `comm`, which is the executable's name in parentheses and may itself contain
/// spaces *and* parentheses — `(my prog (old))` is a legal comm. Counting
/// whitespace-separated fields from the beginning is the classic way to read
/// the wrong number out of this file, and on a 2 Hz watch it would read the
/// wrong number forever.
pub fn parse_proc_stat(text: &str) -> Option<ProcStat> {
    let rest = &text[text.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // `fields[0]` is field 3 (state), so field N is `fields[N - 3]`.
    let state = fields.first()?.chars().next()?;
    let starttime = fields.get(22 - 3)?.parse().ok()?;
    Some(ProcStat { state, starttime })
}

/// Read [`ProcStat`] for `pid` under `proc_root` (`/proc` on a real machine, a
/// fixture tree in a test).
pub fn read_proc_stat(proc_root: &std::path::Path, pid: u32) -> Option<ProcStat> {
    let text = std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat")).ok()?;
    parse_proc_stat(&text)
}

/// Is the process that asked for game mode still there?
///
/// A zombie counts as gone. It has already exited and released everything it
/// held; only its parent's failure to reap it keeps the directory alive, and
/// waiting for that parent would mean a greetd that died mid-teardown pins the
/// machine in a gaming power profile indefinitely — which is the defect.
pub fn owner_state(proc_root: &std::path::Path, owner: &SessionOwner) -> OwnerState {
    if !proc_root.is_dir() {
        return OwnerState::Unknown(format!(
            "{} is not readable, so nothing was measured",
            proc_root.display()
        ));
    }
    let dir = proc_root.join(owner.pid.to_string());
    if !dir.exists() {
        return OwnerState::Gone(format!("/proc/{} is gone", owner.pid));
    }
    match read_proc_stat(proc_root, owner.pid) {
        Some(st) if st.starttime != owner.starttime => OwnerState::Gone(format!(
            "pid {} was reused (start time {} != {})",
            owner.pid, st.starttime, owner.starttime
        )),
        Some(st) if st.state == 'Z' => {
            OwnerState::Gone(format!("pid {} is a zombie", owner.pid))
        }
        Some(_) => OwnerState::Alive,
        None => OwnerState::Unknown(format!(
            "/proc/{}/stat exists but could not be parsed",
            owner.pid
        )),
    }
}

/// Read the owner identity for a live PID, for recording at enter time.
/// `None` when the PID does not exist or `/proc` cannot answer.
pub fn owner_for_pid(proc_root: &std::path::Path, pid: u32) -> Option<SessionOwner> {
    read_proc_stat(proc_root, pid).map(|st| SessionOwner {
        pid,
        starttime: st.starttime,
    })
}

/// A process to pin, plus the cgroup it came from (so exit can put it back).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PidPlacement {
    pub pid: u32,
    /// Absolute path of the cgroup the PID was in before we moved it.
    pub prior_cgroup: Option<String>,
}

/// Everything the planner needs.
pub struct GameInputs<'a> {
    pub cfg: &'a GameModeConfig,
    pub topo: &'a CoreTopology,
    pub nvidia: &'a [NvidiaGpu],
    /// Every GPU under `/sys/class/drm`, with its prior control values already
    /// read. The prior values are an INPUT rather than something the planner
    /// goes and fetches, for the same reason the fan's are: a plan has to be
    /// buildable against a fixture, and a planner that reads the live machine
    /// cannot be.
    pub gpus: &'a [(GpuDevice, SysfsGpuPrior)],
    pub irqs: &'a [IrqEntry],
    pub pids: &'a [PidPlacement],
    /// Value for `cpuset.mems` (normally the root cgroup's effective mems).
    pub mems: String,
    /// Whether an `irqbalance` daemon is running; it will undo IRQ steering, so
    /// the plan says so out loud rather than pretending the pinning holds.
    pub irqbalance: bool,
}

/// The symmetric plan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GamePlan {
    pub enter: Vec<Action>,
    pub exit: Vec<Action>,
    /// CPUs the game is confined to.
    pub cpus: Vec<u32>,
    /// CPUs everything else is pushed onto.
    pub housekeeping: Vec<u32>,
    /// GPU indices whose clocks the plan asks to lock (NVIDIA, by nvidia-smi
    /// index).
    pub gpus_locked: Vec<u32>,
    /// DRM cards whose sysfs controls the plan asks to change (AMD, Intel), by
    /// `cardN`. Separate from `gpus_locked` because the two are different
    /// identifiers for different things and merging them would produce a list
    /// nothing could look anything up in.
    pub gpus_controlled: Vec<String>,
    /// How many interrupts the plan ATTEMPTS to move.
    ///
    /// Named for what it is. It used to be called `irqs_steered` and was handed
    /// straight to `rime game status`, which then reported a plan as a
    /// measurement: on a machine that refuses every affinity write — and
    /// kernel-managed MSI-X queues do — status said "N IRQs steered" having
    /// steered none. What landed is only knowable after the writer has run;
    /// see `GameSession` in the daemon.
    pub irqs_attempted: usize,
    /// Human-readable explanations (shown by `rime game status`).
    pub notes: Vec<String>,
}

impl GamePlan {
    /// `0-11` rendering of the game's cpuset.
    pub fn cpu_list(&self) -> String {
        format_cpu_list(&self.cpus)
    }
}

/// Resolve the cpuset policy into an explicit CPU list.
pub fn resolve_cpus(cfg: &GameModeConfig, topo: &CoreTopology, notes: &mut Vec<String>) -> Vec<u32> {
    match cfg.cpuset_policy() {
        CpusetPolicy::Off => {
            notes.push("cpuset pinning disabled by profile".into());
            Vec::new()
        }
        CpusetPolicy::All => topo.all.clone(),
        CpusetPolicy::PCores => {
            if topo.is_hybrid() {
                notes.push(format!(
                    "P-cores {} (detected via {}), E-cores {}",
                    topo.pcore_list(),
                    topo.source.as_str(),
                    topo.ecore_list()
                ));
                topo.pcores.clone()
            } else {
                notes.push(format!(
                    "no P/E split detected ({}) — pinning to all CPUs",
                    topo.source.as_str()
                ));
                topo.all.clone()
            }
        }
        CpusetPolicy::Explicit(list) => {
            let want = parse_cpu_list(&list);
            let have: Vec<u32> = want.iter().copied().filter(|c| topo.all.contains(c)).collect();
            if have.is_empty() {
                notes.push(format!(
                    "profile cpuset '{list}' matches no online CPU — pinning to all CPUs"
                ));
                topo.all.clone()
            } else {
                have
            }
        }
    }
}

/// The IRQ steering `plan` leaves out of a session that starts with an empty
/// cpuset, planned when the first game process is attached to it: the same
/// rule as `plan`, `(steer, restore)`, both empty when the profile does not
/// steer or `cpus` confines nothing.
pub fn steer_on_attach(
    cfg: &GameModeConfig,
    topo: &CoreTopology,
    irqs: &[IrqEntry],
    cpus: &[u32],
) -> (Vec<Action>, Vec<Action>) {
    let pinning = !cpus.is_empty() && cpus.len() < topo.all.len().max(1);
    if cfg.irq_policy() != IrqPolicy::AwayFromGame || !pinning {
        return (Vec::new(), Vec::new());
    }
    irq::plan_steer(irqs, cpus, &topo.complement(cpus), &cfg.irq_pin_to_game)
}

/// Build the enter/exit plans.
pub fn plan(inputs: &GameInputs<'_>) -> GamePlan {
    let cfg = inputs.cfg;
    let mut notes = Vec::new();
    let cpus = resolve_cpus(cfg, inputs.topo, &mut notes);
    let housekeeping = inputs.topo.complement(&cpus);

    let mut enter = Vec::new();
    let mut exit = Vec::new();

    // ── 0. sched-ext scheduler ───────────────────────────────────────────────
    // FIRST on enter and LAST on exit, on purpose: swapping the scheduler
    // migrates every runnable task, so do it before the game is pinned into its
    // cpuset (and undo it after the pinning is unwound), rather than shuffling
    // tasks that are mid-move.
    //
    // `scx_for` resolves the profile's `scx`: empty = the kernel scheduler,
    // a name = that scheduler, `auto` (the default) = scx_lavd unless the CPU
    // is a P/E hybrid, where lavd was measured moving a game's bottleneck
    // thread onto E-cores. The reasoning is on `GameModeConfig::scx_for`.
    let scx = cfg.scx_for(inputs.topo);
    if scx.is_none() && cfg.scx.trim().eq_ignore_ascii_case("auto") {
        notes.push(if inputs.topo.has_efficiency_cores() {
            format!(
                "sched-ext: none on this CPU (P-cores {}, E-cores {}). The game gets the \
                 kernel's own scheduler, the same as the desktop: scx_lavd puts a game's \
                 busiest thread on E-cores. Set `scx = \"scx_lavd\"` in the profile to load it anyway.",
                inputs.topo.pcore_list(),
                inputs.topo.ecore_list()
            )
        } else {
            "sched-ext: none — the CPU's topology could not be read, so whether it has \
             E-cores (where scx_lavd misplaces a game's busiest thread) is unknown. The game \
             gets the kernel's own scheduler. Set `scx = \"scx_lavd\"` in the profile to load it anyway."
                .to_string()
        });
    }
    if let Some(sched) = &scx {
        enter.push(Action::ScxSwitch {
            sched: sched.clone(),
        });
        // NOTE the exit half is appended at the very END of this function, not
        // here: pushing it now would make ScxStop the FIRST exit action, i.e.
        // restore the scheduler while the game is still pinned. A test asserts
        // the ordering, and it caught exactly that mistake.
        // ── This note is an INTENT, and it is worded as one ──────────────────
        //
        // It used to read "sched-ext: scx_lavd for the session, kernel
        // scheduler restored on exit", and it was `rime game status`'s `notes`
        // field. On katana it was printed directly underneath
        //
        //     scxctl switch -s scx_lavd failed: no scx scheduler running
        //
        // for three consecutive boots — asserting as a fact the thing the line
        // above it had just reported as failed. A plan cannot know what the
        // machine did with it; the daemon applies the plan and replaces this
        // with what it measured. Anything that renders a plan WITHOUT applying
        // it (`rime game profile`, the dry-run planner) shows this one, so it
        // has to be true of a plan — which is that it asks.
        notes.push(format!(
            "sched-ext: this profile ASKS for {} and stops it again on exit. \
             Whether it loaded is a separate fact, reported as `scx_state` once \
             a session is running.",
            sched
        ));
    }

    // ── 1. cpuset ────────────────────────────────────────────────────────────
    let pinning = !cpus.is_empty() && cpus.len() < inputs.topo.all.len().max(1);
    if !cpus.is_empty() && cfg.cpuset_policy() != CpusetPolicy::Off {
        enter.push(Action::CgroupEnsure {
            path: cfg.cgroup.clone(),
            cpus: format_cpu_list(&cpus),
            mems: inputs.mems.clone(),
        });
        for p in inputs.pids {
            enter.push(Action::CgroupAttach {
                path: cfg.cgroup.clone(),
                pid: p.pid,
            });
        }
        if !pinning {
            notes.push("cpuset covers every CPU — the cgroup is created but confines nothing".into());
        }
    }

    // ── 2. IRQ steering ──────────────────────────────────────────────────────
    let (mut steer, mut irq_restore) = match cfg.irq_policy() {
        IrqPolicy::Off => (Vec::new(), Vec::new()),
        IrqPolicy::AwayFromGame => {
            if !pinning {
                notes.push("IRQ steering skipped — the game is not confined to a subset of CPUs".into());
                (Vec::new(), Vec::new())
            } else if inputs.pids.is_empty() {
                // Gaming Mode starts this way: the session names no game, so the
                // cpuset is empty and the game runs on every CPU. Moving every
                // interrupt onto the "other" cores would then pile them onto
                // cores the game is using too (katana: 47 IRQs on its E-cores,
                // where Cyberpunk's threads also ran). `steer_on_attach` moves
                // them when a game process actually enters the cpuset.
                notes.push(
                    "IRQ steering waits for a game: no process is in the cpuset yet, so \
                     interrupts stay where they are until one is attached"
                        .into(),
                );
                (Vec::new(), Vec::new())
            } else {
                irq::plan_steer(inputs.irqs, &cpus, &housekeeping, &cfg.irq_pin_to_game)
            }
        }
    };
    let irqs_attempted = steer.len();
    if irqs_attempted > 0 && inputs.irqbalance {
        notes.push(
            "irqbalance is running and will re-scatter these interrupts — mask it, or ban the game CPUs in its config".into(),
        );
    }
    enter.append(&mut steer);

    // ── 3. NVIDIA clock locks ────────────────────────────────────────────────
    let mut gpus_locked = Vec::new();
    for gpu_info in inputs.nvidia {
        if let Some(only) = cfg.nvidia.gpu_index {
            if only != gpu_info.index {
                continue;
            }
        }
        let lock = gpu::plan_lock(&cfg.nvidia, gpu_info);
        if lock.is_empty() {
            continue;
        }
        let has_clock_lock = lock.iter().any(|a| {
            matches!(
                a,
                Action::NvidiaLockGraphics { .. } | Action::NvidiaLockMemory { .. }
            )
        });
        if has_clock_lock {
            gpus_locked.push(gpu_info.index);
        }
        enter.extend(lock);
        // Unlock first on the way out.
        let mut unlock = gpu::plan_unlock(&cfg.nvidia, gpu_info);
        unlock.append(&mut exit);
        exit = unlock;
    }
    if inputs.nvidia.is_empty() {
        notes.push("no NVIDIA GPU reported by nvidia-smi — GPU clock locking skipped".into());
    }

    // ── 3b. The AMD and Intel GPUs, which had nothing at all ─────────────────
    //
    // Same shape as the NVIDIA locks above: enter, and an exit built from what
    // was read before the change rather than from a default. The exit goes on
    // the FRONT of the exit plan, like the NVIDIA unlock, so the GPU is handed
    // back before the cgroup that pins the game is torn down.
    let mut sysfs_controlled = Vec::new();
    for (dev, prior) in inputs.gpus {
        let (mut enter_gpu, mut gpu_notes) = gpu::plan_sysfs_enter(&cfg.gpu, dev, prior);
        notes.append(&mut gpu_notes);
        if enter_gpu.is_empty() {
            continue;
        }
        sysfs_controlled.push(dev.card.clone());
        enter.append(&mut enter_gpu);
        let mut restore = gpu::plan_sysfs_exit(prior);
        restore.append(&mut exit);
        exit = restore;
    }
    if cfg.gpu.enabled && sysfs_controlled.is_empty() && !inputs.gpus.is_empty() {
        let vendors: Vec<String> = inputs
            .gpus
            .iter()
            .map(|(d, _)| format!("{} ({})", d.card, d.vendor.label()))
            .collect();
        notes.push(format!(
            "no sysfs GPU control was applied to {} — either the profile leaves \
             the knob at its default or this driver does not publish one",
            vendors.join(", ")
        ));
    }

    // ── exit: IRQs, then release the cgroup ──────────────────────────────────
    exit.append(&mut irq_restore);
    if !cpus.is_empty() && cfg.cpuset_policy() != CpusetPolicy::Off {
        for p in inputs.pids {
            if let Some(prior) = &p.prior_cgroup {
                exit.push(Action::CgroupAttach {
                    path: prior.clone(),
                    pid: p.pid,
                });
            }
        }
        exit.push(Action::CgroupRemove {
            path: cfg.cgroup.clone(),
        });
    }

    // Hand scheduling back only after every cpuset/IRQ/clock action has been
    // unwound — the mirror of loading it first on enter.
    if scx.is_some() {
        exit.push(Action::ScxStop);
    }

    GamePlan {
        enter,
        exit,
        cpus,
        housekeeping,
        gpus_locked,
        gpus_controlled: sysfs_controlled,
        irqs_attempted,
        notes,
    }
}
