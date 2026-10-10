//! The Rime Live Update Engine's pure half.
//!
//! `rime update` stages a whole new deployment with bootc. Until this module,
//! every byte of it waited for a reboot, including a Rime Shell change that a
//! running desktop could take in two seconds. This is the part that decides,
//! for each component of a staged release, what activating it *now* would
//! take, whether that is safe on this machine at this moment, and how a
//! transaction that does it moves through its states.
//!
//! No files, no processes. The CLI measures the machine (the booted and staged
//! images, the package sets, the session and lock state, Gaming Mode) and hands
//! the measurements in, the same division `channel`, `blueprint` and `task`
//! use. Everything here is therefore testable on fixtures, and the fixtures in
//! the tests are real: the 2026.10.09 → 2026.10.10 diff is the first case.
//!
//! * [`diff`] parses the two ways the CLI can learn what changed between the
//!   booted and staged trees (`ostree diff`, `composefs-info dump`) and the
//!   package lists.
//! * [`classify`] maps each changed path to a [`Component`] and what
//!   activating it requires.
//! * [`plan`] turns those plus the machine's state into a [`Plan`]: what
//!   activates live, what is deferred and why, what needs a restart of which
//!   kind.
//! * [`txn`] is the durable transaction record and its state machine.
//! * [`elf`] reads the `DT_NEEDED` list out of an ELF file, which is how a
//!   first-party binary is proven not to depend on a library the release also
//!   changed.
//!
//! ## The rule this module exists to keep
//!
//! An unknown answer is never permission. A path no rule recognises, a
//! dependency that cannot be read, a lock state nobody could determine: each
//! of these defers the component and says which fact was missing. Live
//! activation is an optimisation over a reboot that already works; it has to
//! earn every component it touches.

pub mod caps;
pub mod classify;
pub mod diff;
pub mod elf;
pub mod plan;
pub mod txn;

use serde::{Deserialize, Serialize};

/// One independently reported part of a release.
///
/// The granularity is what a user can act on and what the engine activates
/// as a unit, not the package list: "Rime Shell", not 2,000 QML files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Component {
    /// `/usr/share/rime-shell` and its native module in `/usr/lib64/rime-shell`.
    Shell,
    /// The Hyprland modules Rime seeds into each user's `~/.config/hypr/rime`.
    HyprlandConfig,
    /// `rime`, `/usr/libexec/rime-*` and other first-party programs that run
    /// to completion: new code runs on their next invocation.
    RimeTools,
    /// `rimed`, the system policy daemon (and its unit).
    RimeDaemon,
    /// Other first-party long-running services and their units.
    RimeServices,
    /// systemd unit files outside the above.
    SystemUnits,
    /// systemd itself: PID 1, the user managers, libsystemd-shared.
    Systemd,
    /// Shared libraries and the dynamic linker.
    Libraries,
    /// Hyprland, niri, labwc, Xwayland.
    Compositor,
    /// Quickshell and Qt: the shell's runtime.
    ShellRuntime,
    /// The NVIDIA kernel modules and their userspace.
    Nvidia,
    /// The kernel image, its in-tree modules and the initramfs.
    Kernel,
    /// Device firmware files loaded by drivers (`/usr/lib/firmware`).
    FirmwareFiles,
    /// shim, GRUB, systemd-boot, bootupd payloads.
    Bootloader,
    /// Image defaults under `/usr/etc`, merged into `/etc` at deployment.
    EtcDefaults,
    /// `/usr/share/rime/release.json` and similar descriptions of the image.
    Metadata,
    /// Packages and files no narrower rule covers.
    OtherSystem,
}

impl Component {
    pub const ALL: [Component; 17] = [
        Component::Shell,
        Component::HyprlandConfig,
        Component::RimeTools,
        Component::RimeDaemon,
        Component::RimeServices,
        Component::SystemUnits,
        Component::Systemd,
        Component::Libraries,
        Component::Compositor,
        Component::ShellRuntime,
        Component::Nvidia,
        Component::Kernel,
        Component::FirmwareFiles,
        Component::Bootloader,
        Component::EtcDefaults,
        Component::Metadata,
        Component::OtherSystem,
    ];

    /// The name a user reads.
    pub fn label(self) -> &'static str {
        match self {
            Component::Shell => "Rime Shell",
            Component::HyprlandConfig => "Hyprland configuration",
            Component::RimeTools => "Rime tools",
            Component::RimeDaemon => "rimed",
            Component::RimeServices => "Rime services",
            Component::SystemUnits => "System services",
            Component::Systemd => "systemd",
            Component::Libraries => "System libraries",
            Component::Compositor => "Compositor",
            Component::ShellRuntime => "Quickshell and Qt",
            Component::Nvidia => "NVIDIA driver",
            Component::Kernel => "Kernel",
            Component::FirmwareFiles => "Device firmware files",
            Component::Bootloader => "Bootloader",
            Component::EtcDefaults => "System configuration defaults",
            Component::Metadata => "Release information",
            Component::OtherSystem => "Other system files",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Component::Shell => "shell",
            Component::HyprlandConfig => "hyprland-config",
            Component::RimeTools => "rime-tools",
            Component::RimeDaemon => "rime-daemon",
            Component::RimeServices => "rime-services",
            Component::SystemUnits => "system-units",
            Component::Systemd => "systemd",
            Component::Libraries => "libraries",
            Component::Compositor => "compositor",
            Component::ShellRuntime => "shell-runtime",
            Component::Nvidia => "nvidia",
            Component::Kernel => "kernel",
            Component::FirmwareFiles => "firmware-files",
            Component::Bootloader => "bootloader",
            Component::EtcDefaults => "etc-defaults",
            Component::Metadata => "metadata",
            Component::OtherSystem => "other-system",
        }
    }

    pub fn from_slug(s: &str) -> Option<Component> {
        Component::ALL.into_iter().find(|c| c.slug() == s)
    }
}

/// What it takes for the new version of a component to be the code that runs.
///
/// Ordered from least to most disruptive, so `max()` over a set is the
/// requirement of the whole set. This is the vocabulary the brief calls the
/// "actual update categories", minus the outcomes (deferred, failed), which
/// are [`Outcome`]s: a requirement is a property of the change, an outcome is
/// what happened to it on this machine today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Requirement {
    /// Nothing runs it: a description of the image, not code.
    Nothing,
    /// New code runs the next time the program is started; nothing running
    /// is touched. True of a CLI, a oneshot, a timer's next run.
    NextUse,
    /// Active in place, no process interrupted (a reload signal).
    LiveReload,
    /// Active once a specific service or per-user process is restarted, which
    /// the engine does itself when the restart is known to be safe.
    ServiceRestart,
    /// Active when the user next restarts an application or a service that
    /// holds their work (an agent daemon with live sessions). Never done for
    /// them.
    AppRestart,
    /// Active once a kernel driver is unloaded and loaded again, which is
    /// only possible while nothing uses the device (NVIDIA on a hybrid
    /// laptop whose desktop runs on the integrated GPU). Never automatic.
    DriverReload,
    /// Active after a compositor restart that keeps the clients. Hyprland
    /// 0.56 has no socket handover, so nothing reaches this today; it exists
    /// so the vocabulary does not have to change the day one does.
    CompositorHandover,
    /// Log out and back in.
    SessionRestart,
    /// `systemctl soft-reboot`: userspace restarts, the kernel stays.
    SoftReboot,
    /// A new kernel must be running: a reboot (or kexec, which this engine
    /// does not do on its own).
    KernelTransition,
    /// Firmware has to run it: a full reboot through the firmware.
    Reboot,
}

impl Requirement {
    pub fn describe(self) -> &'static str {
        match self {
            Requirement::Nothing => "no activation needed",
            Requirement::NextUse => "active the next time it starts",
            Requirement::LiveReload => "can be reloaded in place",
            Requirement::ServiceRestart => "needs a service restart",
            Requirement::AppRestart => "needs an application restart",
            Requirement::DriverReload => "needs the driver reloaded while the device is idle",
            Requirement::CompositorHandover => "needs a compositor handover",
            Requirement::SessionRestart => "needs you to log out and back in",
            Requirement::SoftReboot => "needs a userspace restart (soft reboot)",
            Requirement::KernelTransition => "needs the new kernel (restart)",
            Requirement::Reboot => "needs a restart",
        }
    }

    /// Whether the engine may carry this out by itself during `rime update`.
    /// Everything above a service restart interrupts the user's work and is
    /// only ever recommended.
    pub fn engine_may_apply(self) -> bool {
        matches!(
            self,
            Requirement::Nothing
                | Requirement::NextUse
                | Requirement::LiveReload
                | Requirement::ServiceRestart
        )
    }
}

/// Why a component that could have been activated live was not, this time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "kebab-case")]
pub enum DeferReason {
    /// A game is running, Gaming Mode is held, or a gamescope session is up.
    Busy(String),
    /// The screen is locked, an authentication is in progress, or the lock
    /// state could not be read.
    Locked(String),
    /// A fact the activation depends on could not be established.
    Unproven(String),
    /// Another component it must activate with was deferred.
    Grouped(String),
    /// The user asked for it (`--no-live`, `rime live apply --only`).
    NotRequested,
}

impl DeferReason {
    pub fn describe(&self) -> String {
        match self {
            DeferReason::Busy(s) => format!("deferred while the machine is busy: {s}"),
            DeferReason::Locked(s) => format!("deferred until the session is known to be unlocked: {s}"),
            DeferReason::Unproven(s) => format!("deferred, compatibility not proven: {s}"),
            DeferReason::Grouped(s) => format!("deferred together with {s}"),
            DeferReason::NotRequested => "not activated live (not requested)".to_string(),
        }
    }
}

/// What happened to one component on this machine, as measured afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Outcome {
    /// The release does not change it.
    Unchanged,
    /// The new version is the one running, and the engine checked.
    Active { how: Requirement, evidence: String },
    /// On disk now; new code runs at its next start.
    ActiveAtNextUse { evidence: String },
    /// Could activate live, and did not, for a stated reason.
    Deferred { reason: DeferReason },
    /// Waiting for something only the user can do (restart an app, log out,
    /// reboot). The staged deployment carries it.
    Pending { requirement: Requirement },
    /// Activation was attempted and undone.
    FailedRolledBack { error: String },
    /// Activation was attempted, failed, and could not be undone cleanly.
    Failed { error: String },
    /// Not something this machine has (an NVIDIA change on an AMD laptop).
    NotApplicable { why: String },
}

/// Facts about the machine the planner needs and cannot read itself.
///
/// Every field that could fail to be measured is an `Option` or carries its
/// error, so "I could not tell" reaches the planner as itself rather than as a
/// default that happens to permit something.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MachineState {
    /// Gaming Mode held, a game registered with rimed, or a gamescope session.
    /// `None` = could not be determined.
    pub busy: Option<Option<String>>,
    /// Lock state of every graphical session the shell would be restarted in.
    pub sessions: Vec<SessionState>,
    /// Whether the machine has an NVIDIA GPU at all.
    pub has_nvidia: Option<bool>,
}

/// One graphical session on the machine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionState {
    pub user: String,
    pub uid: u32,
    pub session_id: String,
    /// "hyprland", "niri", "labwc", "gamescope", or what logind said.
    pub desktop: String,
    /// `Some(true)` locked or authenticating, `Some(false)` unlocked,
    /// `None` could not be read. Only `Some(false)` permits a shell restart.
    pub locked: Option<bool>,
}
