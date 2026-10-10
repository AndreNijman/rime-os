//! `rimed-core` — the pure, testable heart of Rime OS power management.
//!
//! It has no D-Bus, no async runtime, and no unconditional I/O:
//!
//! * [`fingerprint`] reads (never writes) `/proc` + `/sys` to build a
//!   [`Fingerprint`].
//! * [`battery`] enumerates the machine's batteries and probes which of them
//!   (if any) accept charge thresholds. No battery is ever named in code or in
//!   a profile.
//! * [`select`] maps a [`Fingerprint`] to a layered [`Selection`]
//!   (generic -> class -> device).
//! * [`profile`] models the tuning profiles and turns a (profile, tier) pair
//!   into an ordered list of [`Action`]s.
//! * [`syswriter`] is the *only* thing that turns [`Action`]s into real
//!   effects; [`MockWriter`] records them for tests, [`RealWriter`] applies
//!   them (honouring dry-run).
//!
//! M6 adds four more pure modules on the same pattern — read sysfs, plan
//! [`Action`]s, let the writer do the touching:
//!
//! * [`topology`] resolves the P-core/E-core split (Alder Lake and friends).
//! * [`fan`] enumerates hwmon and msi-ec fans and plans mode changes, with an
//!   explicit "hand the fan back to firmware" primitive.
//! * [`gpu`] plans NVIDIA clock locks around `nvidia-smi`.
//! * [`irq`] plans interrupt-affinity steering, and [`game`] combines the three
//!   into a symmetric enter/exit pair.
//!
//! [`blueprint`] is the same pattern once more, for a different subject: it
//! parses the declarative Rime Blueprint, compares it to an observed machine,
//! and emits [`blueprint::Step`]s. It reads nothing and runs nothing — `rime`
//! does the probing and the converging, exactly as the daemon does for
//! [`Action`]s.
//!
//! [`task`] is that pattern for §21: it owns the task record's format and
//! validation and turns a task plus one observation of the machine into an
//! ordered resume plan. It performs no I/O, references no other subsystem's
//! state, and grants nothing.
//!
//! The daemon and CLI are thin shells over this crate.

pub mod ai;
pub mod aiprobe;
pub mod battery;
pub mod blueprint;
// §26's update channels and the health signal that stops a rollout. Pure, like
// its neighbours: the CLI measures the machine and hands the measurements in.
pub mod channel;
pub mod dispatch;
pub mod fan;
pub mod fingerprint;
// §P2-015's firmware readout. Pure: the CLI runs fwupdmgr and hands the JSON
// in. fwupd's exit status is never consulted anywhere, because it was measured
// to mean "nothing to do" when non-zero and to accompany an explicit `Error`
// document when zero — so the document is the only truth.
pub mod firmware;
pub mod game;
pub mod gameprofile;
pub mod gaming;
pub mod gpu;
pub mod host;
pub mod irq;
// P1-043's kernel-BTF probe. It reads `/sys/kernel/btf/vmlinux` — rooted, like
// `syswriter::read_scx_state`, so every answer is reachable from a temp
// directory — and says whether a sched-ext scheduler can bind to this kernel
// AT ALL. On every Rime image to date it cannot, for a kernel-build reason
// Rime does not own, and Gaming Mode's `not loaded` needed a way to say which
// kind of "not loaded" it is.
pub mod kernelbtf;
// P1-063's lid policy. Pure, like its neighbours: the decision is a total
// function over injected readings, because the act it authorises is a laptop
// going to sleep in someone's bag and the only machine that could exercise it
// for real is one a person is using.
pub mod lid;
// The live-update engine's pure half: classify a staged release's file diff
// into components, plan what can activate without a reboot on THIS machine,
// and the transaction rules. The `rime` CLI measures and acts.
pub mod live;
// §25's persistent-state migration framework. It resolves no paths and spawns
// nothing: callers hand it a document or a `&Path`, which is what lets the
// suite run entirely inside a temp directory rather than near a real
// `$XDG_STATE_HOME`.
pub mod migrate;
pub mod mode;
// Whether the NVIDIA driver branch in the image can drive this machine's
// NVIDIA GPU (Maxwell/Pascal/Volta need the 580 legacy branch Rime does not ship).
pub mod nvidia_support;
pub mod perf;
pub mod profile;
// §33's hardware qualification database. Pure, like its neighbours: it holds
// the document shape and the consent rule, and the CLI hands it a probe of the
// machine and resolves the path.
pub mod qualify;
pub mod recover;
pub mod select;
// §48's Storage Manager. Pure: the CLI runs smartctl and reads sysfs, and
// hands the text in — so every threshold, every bitmask and every "this could
// not be read" is driven from fixtures rather than from whatever disk the
// runner happens to have.
pub mod storage;
pub mod syswriter;
// §21's Task. Deliberately NOT re-exported at the crate root: its `Observed`
// would collide with `blueprint::Observed`, which is re-exported below, and
// both names are unambiguous at their own module path — `rimed_core::task::
// Observed` says which kind of observation it is, which a bare `Observed`
// hoisted under an alias would not. Same rule as `Step`, immediately below.
pub mod task;
pub mod tier;
pub mod topology;
pub mod workload;

// §14's local inference service. `Store`, `Settings` and `Manifest` are
// unambiguous at the crate root; `Backend`, `Runtime`, `Listen` and the two
// `plan_*` functions deliberately stay behind `ai::` — a bare `Backend` in a
// call site would not say whether it meant a compute backend or something
// else, and `rimed-core` already learned that lesson with `Step`.
pub use ai::{AiError, Manifest as AiManifest, Settings as AiSettings, Store as AiStore};
pub use battery::{Battery, BatteryInventory, ThresholdSupport};
// `Step` is deliberately NOT re-exported here from either module. Phase 7's
// blueprint and phase 8's modes each have a type called `Step` — a convergence
// step and a mode-application step — and re-exporting both at the crate root is
// an E0252 collision. Neither is ambiguous at its own module path, and both
// consumers already import it that way (`rimed_core::blueprint::Step`,
// `rimed_core::mode::Step`), so nothing is lost by leaving the name where it is
// unambiguous. Hoisting one under an alias would just make the call sites lie
// about which kind of step they mean.
pub use blueprint::{
    AppliedState, Blueprint, Bundle, Change, Domain, Observed, Plan, ProjectRef,
};
pub use fan::{FanInventory, FanMode, FanSnapshot, UnknownFanMode};
pub use fingerprint::{CpuInfo, CpuVendor, Fingerprint, GpuInfo, GpuVendor};
pub use game::{GameInputs, GamePlan, PidPlacement};
// §12's two halves. `Step` stays behind its module path for the reason given
// above; `Resolution` and `Readiness` are unambiguous.
pub use gameprofile::{GameProfile, GameProfiles, Resolution};
pub use gaming::{Probe, Readiness};
pub use gpu::{NvidiaGpu, NvidiaSmi, RealNvidiaSmi};
pub use mode::{Mode, ModeId, ModeMatch, ModeState, PolicyIntent, TierPolicy, UnknownMode};
pub use perf::{CpuPerf, GpuPerf, PerfSnapshot, PowerReading, SchedulerState, Temp};
pub use profile::{
    ChargeConfig, CpusetPolicy, FanConfig, GameModeConfig, IrqPolicy, NvidiaConfig, Profile,
    ProfileKind, ProfileSet, TierSettings,
};
pub use select::{select, Selection};
pub use syswriter::{MockWriter, Outcome, RealWriter, SysWriter};
pub use tier::{Action, Tier, UnknownTier};
pub use topology::{CoreSource, CoreTopology};
pub use workload::{Assessment, Signal, Signals, Vram, Workload};

/// The default on-disk override directory for profiles. If present it wins
/// over the embedded set.
pub const PROFILE_DIR: &str = "/usr/share/rimeos/sysprofiles";

/// True when dry-run is forced via the environment (`RIMED_DRY_RUN=1`). The
/// daemon also exposes a `--dry-run` flag; either turns every real write off.
pub fn dry_run_from_env() -> bool {
    matches!(
        std::env::var("RIMED_DRY_RUN").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}
