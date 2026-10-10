//! From a classified diff and the machine's state to a plan: what activates
//! live now, what waits and why, and what the user will eventually have to do.
//!
//! ## Dependencies between live components
//!
//! The Rime pieces call each other, so activating one without another can put
//! a new caller in front of an old callee:
//!
//! * the `rime` CLI calls `rimed` over `org.rimeos.Rimed1`, and a new CLI may
//!   use a member an old daemon does not have. **RimeTools needs RimeDaemon.**
//!   (The other direction is safe: the daemon's API is a compatibility surface,
//!   so an old CLI against a new daemon is the supported case.)
//! * the shell runs `rime` verbs. **Shell needs RimeTools.**
//! * the Hyprland modules bind keys to shell IPC targets and `rime` verbs.
//!   **HyprlandConfig needs Shell and RimeTools.**
//! * the shell is QML for one Quickshell and Qt. **Shell needs ShellRuntime
//!   unchanged**: a staged Quickshell is not running, so QML written against
//!   it may not load in the old one.
//!
//! "Needs" means the dependency is unchanged or activates in the same plan. A
//! dependency that is deferred, pending or failed defers its dependents, and
//! the reason names it.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::classify::{Activator, Classified, RESTART_SAFE_UNITS};
use super::diff::{Change, ChangeKind, PackageChange};
use super::{Component, DeferReason, MachineState, Requirement};

/// Facts the CLI measured that the plan depends on.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    pub changes: Vec<Classified>,
    pub packages: Vec<PackageChange>,
    /// For every `elf_checked` path: Ok when its library closure is unchanged
    /// between the trees, Err with the reason otherwise. A missing entry is
    /// treated as an Err.
    pub elf: BTreeMap<String, Result<(), String>>,
    /// System units currently running as long-lived services (not oneshots).
    pub running_units: BTreeSet<String>,
    pub machine: MachineState,
    /// `bootc status` says a soft reboot can activate the staged deployment.
    pub soft_reboot_capable: bool,
    /// The user asked for no live activation (`--no-live`).
    pub no_live: bool,
    /// Restrict live activation to these components (`rime live apply --only`).
    pub only: Option<BTreeSet<Component>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "kebab-case")]
pub enum Decision {
    /// Put its files in the live layer and run its activators. `residual` is
    /// what is still needed afterwards for every running copy to be new (an
    /// agent daemon nobody may restart for the user).
    Activate { residual: Option<Requirement> },
    Defer { reason: DeferReason },
    /// Only the staged deployment can carry it; this is what that takes.
    Pending { requirement: Requirement },
    NotApplicable { why: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentPlan {
    pub component: Component,
    pub label: String,
    pub requirement: Requirement,
    pub files: usize,
    pub decision: Decision,
    /// e.g. `systemd 262-3.fc45 → 262-4.fc45`.
    pub versions: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub components: Vec<ComponentPlan>,
    /// Exactly the paths that go into the live layer.
    pub live_set: Vec<Change>,
    /// What to run after the layer is in place, in order.
    pub actions: Vec<Activator>,
    /// The most disruptive thing still needed once the plan has run.
    pub remaining: Requirement,
    /// The least disruptive way to finish activating everything.
    pub recommendation: String,
}

impl Plan {
    pub fn component(&self, c: Component) -> Option<&ComponentPlan> {
        self.components.iter().find(|p| p.component == c)
    }
    pub fn activates_anything(&self) -> bool {
        !self.live_set.is_empty() || !self.actions.is_empty()
    }
}

const DEPENDS: &[(Component, Component)] = &[
    (Component::RimeTools, Component::RimeDaemon),
    (Component::Shell, Component::RimeTools),
    (Component::HyprlandConfig, Component::Shell),
    (Component::HyprlandConfig, Component::RimeTools),
];

/// Changed at all and not activating in this plan: dependents must wait.
const MUST_BE_UNCHANGED: &[(Component, Component)] = &[
    (Component::Shell, Component::ShellRuntime),
    (Component::HyprlandConfig, Component::Compositor),
];

fn package_component(name: &str) -> Option<Component> {
    let n = name;
    if n.starts_with("kernel") {
        return Some(Component::Kernel);
    }
    if n.contains("nvidia") || n.starts_with("kmod-nvidia") {
        return Some(Component::Nvidia);
    }
    if n == "systemd" || n.starts_with("systemd-") {
        return Some(Component::Systemd);
    }
    if matches!(n, "glibc" | "glibc-common" | "glibc-minimal-langpack" | "glibc-all-langpacks") {
        return Some(Component::Libraries);
    }
    if n.starts_with("hyprland") || n.starts_with("aquamarine") || n == "niri" || n == "labwc" || n.starts_with("xorg-x11-server-Xwayland") {
        return Some(Component::Compositor);
    }
    if n.starts_with("quickshell") || n.starts_with("qt6-") {
        return Some(Component::ShellRuntime);
    }
    if n.starts_with("linux-firmware") || n.ends_with("-firmware") {
        return Some(Component::FirmwareFiles);
    }
    if n.starts_with("shim-") || n.starts_with("grub2-") || n == "bootupd" || n == "systemd-boot-unsigned" {
        return Some(Component::Bootloader);
    }
    None
}

fn busy_reason(m: &MachineState) -> Option<DeferReason> {
    match &m.busy {
        Some(None) => None,
        Some(Some(why)) => Some(DeferReason::Busy(why.clone())),
        None => Some(DeferReason::Unproven(
            "could not tell whether a game or Gaming Mode is running".into(),
        )),
    }
}

/// Why the shell cannot be replaced right now, if it cannot.
fn session_block(m: &MachineState) -> Option<DeferReason> {
    for s in &m.sessions {
        if s.desktop == "gamescope" {
            return Some(DeferReason::Busy(format!("Gaming Mode session for {}", s.user)));
        }
        match s.locked {
            Some(false) => {}
            Some(true) => {
                return Some(DeferReason::Locked(format!(
                    "{}'s session {} is locked",
                    s.user, s.session_id
                )))
            }
            None => {
                return Some(DeferReason::Locked(format!(
                    "could not read whether {}'s session {} is locked",
                    s.user, s.session_id
                )))
            }
        }
    }
    None
}

pub fn plan(inp: &Inputs) -> Plan {
    let mut by_comp: BTreeMap<Component, Vec<&Classified>> = BTreeMap::new();
    for c in &inp.changes {
        by_comp.entry(c.class.component).or_default().push(c);
    }
    let mut versions: BTreeMap<Component, Vec<String>> = BTreeMap::new();
    for p in &inp.packages {
        if let Some(c) = package_component(&p.name) {
            let from = p.from.as_deref().unwrap_or("(new)");
            let to = p.to.as_deref().unwrap_or("(removed)");
            versions.entry(c).or_default().push(format!("{} {from} → {to}", p.name));
            // A package change with no file under a recognised path still
            // belongs to its component (e.g. only a doc file moved): record
            // the component so it is reported.
            by_comp.entry(c).or_default();
        }
    }

    let busy = busy_reason(&inp.machine);
    let sessions = session_block(&inp.machine);
    let mut plans: BTreeMap<Component, ComponentPlan> = BTreeMap::new();

    for (&comp, entries) in &by_comp {
        let requirement = entries
            .iter()
            .map(|e| e.class.requirement)
            .max()
            .unwrap_or(match comp {
                Component::Kernel => Requirement::KernelTransition,
                Component::FirmwareFiles | Component::Bootloader => Requirement::Reboot,
                Component::Nvidia => Requirement::DriverReload,
                _ => Requirement::SoftReboot,
            });
        let all_live = !entries.is_empty() && entries.iter().all(|e| e.class.live);
        let mut notes = Vec::new();
        let mut residual: Option<Requirement> = None;

        let decision = if comp == Component::Metadata {
            Decision::Pending { requirement: Requirement::Nothing }
        } else if comp == Component::Nvidia && inp.machine.has_nvidia == Some(false) {
            Decision::NotApplicable { why: "this machine has no NVIDIA GPU".into() }
        } else if !all_live {
            Decision::Pending { requirement }
        } else if inp.no_live || inp.only.as_ref().is_some_and(|o| !o.contains(&comp)) {
            Decision::Defer { reason: DeferReason::NotRequested }
        } else if let Some(bad) = entries
            .iter()
            .filter(|e| e.class.elf_checked && e.change.kind != ChangeKind::Removed)
            .find_map(|e| match inp.elf.get(&e.change.path) {
                Some(Ok(())) => None,
                Some(Err(why)) => Some(format!("{}: {why}", e.change.path)),
                None => Some(format!("{}: its libraries were not checked", e.change.path)),
            })
        {
            Decision::Defer { reason: DeferReason::Unproven(bad) }
        } else if let Some(b) = busy.clone() {
            Decision::Defer { reason: b }
        } else if let (true, Some(r)) =
            (matches!(comp, Component::Shell | Component::HyprlandConfig), sessions.clone())
        {
            Decision::Defer { reason: r }
        } else {
            // Units: a definition change reaches a running service only when it
            // restarts.
            for e in entries {
                if let Some(Activator::DaemonReload) = e.class.activator {
                    let unit = e.change.path.rsplit('/').next().unwrap_or("");
                    let unit = e
                        .change
                        .path
                        .split('/')
                        .find(|s| s.ends_with(".d"))
                        .map(|d| d.trim_end_matches(".d"))
                        .unwrap_or(unit);
                    if inp.running_units.contains(unit) && !RESTART_SAFE_UNITS.contains(&unit) {
                        notes.push(format!("{unit} is running; restart it to use the new definition"));
                        residual = residual.max(Some(Requirement::AppRestart));
                    }
                }
            }
            if requirement == Requirement::AppRestart {
                residual = residual.max(Some(Requirement::AppRestart));
                notes.push("installed live; running copies change when restarted".into());
            }
            Decision::Activate { residual }
        };

        plans.insert(
            comp,
            ComponentPlan {
                component: comp,
                label: comp.label().to_string(),
                requirement,
                files: entries.len(),
                decision,
                versions: versions.remove(&comp).unwrap_or_default(),
                notes,
            },
        );
    }

    // Dependencies, to a fixed point: a deferral can cascade two levels
    // (daemon → tools → shell → Hyprland config).
    loop {
        let mut changed = false;
        let snapshot: BTreeMap<Component, bool> = plans
            .iter()
            .map(|(c, p)| (*c, matches!(p.decision, Decision::Activate { .. })))
            .collect();
        for (dep, on) in DEPENDS {
            if let (Some(false), Some(p)) = (snapshot.get(on).copied(), plans.get_mut(dep)) {
                if matches!(p.decision, Decision::Activate { .. }) {
                    p.decision = Decision::Defer {
                        reason: DeferReason::Grouped(on.label().to_string()),
                    };
                    changed = true;
                }
            }
        }
        for (dep, on) in MUST_BE_UNCHANGED {
            if plans.contains_key(on) {
                if let Some(p) = plans.get_mut(dep) {
                    if matches!(p.decision, Decision::Activate { .. }) {
                        p.decision = Decision::Defer {
                            reason: DeferReason::Unproven(format!(
                                "this release also changes {}, which is not running yet",
                                on.label()
                            )),
                        };
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    let activating: BTreeSet<Component> = plans
        .iter()
        .filter(|(_, p)| matches!(p.decision, Decision::Activate { .. }))
        .map(|(c, _)| *c)
        .collect();
    let live_set: Vec<Change> = inp
        .changes
        .iter()
        .filter(|c| c.class.live && activating.contains(&c.class.component))
        .map(|c| c.change.clone())
        .collect();

    let mut actions: BTreeSet<(u8, Activator)> = BTreeSet::new();
    for c in inp.changes.iter().filter(|c| activating.contains(&c.class.component)) {
        match &c.class.activator {
            Some(Activator::DaemonReload) => {
                actions.insert((0, Activator::DaemonReload));
            }
            Some(Activator::RestartUnit(u)) => {
                // A changed unit file needs the manager to read it first.
                if c.change.path.starts_with("/usr/lib/systemd/system/") {
                    actions.insert((0, Activator::DaemonReload));
                }
                actions.insert((1, Activator::RestartUnit(u.clone())));
            }
            Some(Activator::UserDaemonReload) => {
                actions.insert((2, Activator::UserDaemonReload));
            }
            Some(Activator::Shell) => {
                actions.insert((3, Activator::Shell));
            }
            Some(Activator::HyprlandConfig) => {
                actions.insert((4, Activator::HyprlandConfig));
            }
            None => {}
        }
    }
    // A unit-file change for a running, restart-safe unit restarts it too.
    for c in inp.changes.iter().filter(|c| activating.contains(&c.class.component)) {
        if c.class.activator == Some(Activator::DaemonReload) {
            if let Some(u) = c.change.path.rsplit('/').next() {
                if inp.running_units.contains(u) && RESTART_SAFE_UNITS.contains(&u) {
                    actions.insert((1, Activator::RestartUnit(u.to_string())));
                }
            }
        }
    }

    let components: Vec<ComponentPlan> = plans.into_values().collect();
    let remaining = components
        .iter()
        .map(|p| match &p.decision {
            Decision::Pending { requirement } => *requirement,
            Decision::Activate { residual: Some(r) } => *r,
            _ => Requirement::Nothing,
        })
        .max()
        .unwrap_or(Requirement::Nothing);
    let recommendation = recommend(remaining, inp.soft_reboot_capable);

    Plan {
        components,
        live_set,
        actions: actions.into_iter().map(|(_, a)| a).collect(),
        remaining,
        recommendation,
    }
}

/// The least disruptive safe way to finish, as advice. Never executed by
/// `rime update`.
pub fn recommend(remaining: Requirement, soft_reboot_capable: bool) -> String {
    match remaining {
        Requirement::Nothing | Requirement::NextUse | Requirement::LiveReload | Requirement::ServiceRestart => {
            "nothing: everything in this release is active".into()
        }
        Requirement::AppRestart => "restart the applications listed above when convenient".into(),
        Requirement::DriverReload => {
            "restart when convenient (`rime live doctor` says whether this GPU driver could be reloaded live)".into()
        }
        Requirement::CompositorHandover | Requirement::SessionRestart => "log out and back in".into(),
        Requirement::SoftReboot if soft_reboot_capable => {
            "restart when convenient; a userspace restart is enough (`sudo rime live soft-reboot --yes`, closes all applications)".into()
        }
        Requirement::SoftReboot | Requirement::KernelTransition | Requirement::Reboot => {
            "restart when convenient".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::classify::classify;
    use crate::live::diff::{parse_ostree_diff, Change, ChangeKind};
    use crate::live::SessionState;

    fn ch(kind: ChangeKind, p: &str) -> Change {
        Change { kind, path: p.into() }
    }

    fn idle_unlocked() -> MachineState {
        MachineState {
            busy: Some(None),
            sessions: vec![SessionState {
                user: "andre".into(),
                uid: 1000,
                session_id: "2".into(),
                desktop: "hyprland".into(),
                locked: Some(false),
            }],
            has_nvidia: Some(false),
        }
    }

    fn inputs(changes: Vec<Change>, machine: MachineState) -> Inputs {
        let classified = classify(&changes);
        let elf = classified
            .iter()
            .filter(|c| c.class.elf_checked)
            .map(|c| (c.change.path.clone(), Ok(())))
            .collect();
        Inputs {
            changes: classified,
            elf,
            running_units: ["rimed.service".to_string(), "rime-lid.service".to_string()].into(),
            machine,
            soft_reboot_capable: true,
            ..Default::default()
        }
    }

    #[test]
    fn shell_only_release_activates_live() {
        let p = plan(&inputs(
            vec![
                ch(ChangeKind::Modified, "/usr/share/rime-shell/src/services/UpdateService.qml"),
                ch(ChangeKind::Added, "/usr/share/rime-shell/src/nexus/UpdatesPage.qml"),
                ch(ChangeKind::Removed, "/usr/share/rime-shell/src/windows/UpdatePopup.qml"),
                ch(ChangeKind::Modified, "/usr/share/rime/release.json"),
            ],
            idle_unlocked(),
        ));
        let s = p.component(Component::Shell).unwrap();
        assert_eq!(s.decision, Decision::Activate { residual: None });
        assert_eq!(p.live_set.len(), 3, "release.json never enters the live layer");
        assert_eq!(p.actions, vec![Activator::Shell]);
        assert_eq!(p.remaining, Requirement::Nothing);
    }

    #[test]
    fn the_real_1009_to_1010_release_restarts_rimed() {
        let changes = parse_ostree_diff(crate::live::diff::tests::DIFF_1009_1010).unwrap();
        let p = plan(&inputs(changes, idle_unlocked()));
        assert!(matches!(p.component(Component::RimeDaemon).unwrap().decision, Decision::Activate { .. }));
        assert!(matches!(p.component(Component::RimeTools).unwrap().decision, Decision::Activate { .. }));
        assert_eq!(
            p.actions,
            vec![Activator::DaemonReload, Activator::RestartUnit("rimed.service".into())]
        );
        assert_eq!(p.remaining, Requirement::Nothing);
        assert_eq!(p.live_set.len(), 6);
    }

    #[test]
    fn busy_defers_everything_live_and_says_why() {
        let mut m = idle_unlocked();
        m.busy = Some(Some("Gaming Mode is held".into()));
        let changes = parse_ostree_diff(crate::live::diff::tests::DIFF_1009_1010).unwrap();
        let p = plan(&inputs(changes, m));
        assert!(p.live_set.is_empty());
        assert!(p.actions.is_empty());
        assert_eq!(
            p.component(Component::RimeDaemon).unwrap().decision,
            Decision::Defer { reason: DeferReason::Busy("Gaming Mode is held".into()) }
        );
    }

    #[test]
    fn unknown_busy_state_is_not_permission() {
        let mut m = idle_unlocked();
        m.busy = None;
        let p = plan(&inputs(vec![ch(ChangeKind::Modified, "/usr/bin/rime")], m));
        assert!(matches!(
            p.component(Component::RimeTools).unwrap().decision,
            Decision::Defer { reason: DeferReason::Unproven(_) }
        ));
        assert!(p.live_set.is_empty());
    }

    #[test]
    fn locked_or_unknown_lock_defers_the_shell_only() {
        for locked in [Some(true), None] {
            let mut m = idle_unlocked();
            m.sessions[0].locked = locked;
            let p = plan(&inputs(
                vec![
                    ch(ChangeKind::Modified, "/usr/share/rime-shell/shell.qml"),
                    ch(ChangeKind::Modified, "/usr/libexec/rime-pkg"),
                ],
                m,
            ));
            assert!(matches!(
                p.component(Component::Shell).unwrap().decision,
                Decision::Defer { reason: DeferReason::Locked(_) }
            ));
            // Tools do not depend on the shell.
            assert!(matches!(p.component(Component::RimeTools).unwrap().decision, Decision::Activate { .. }));
            assert_eq!(p.live_set, vec![ch(ChangeKind::Modified, "/usr/libexec/rime-pkg")]);
        }
    }

    #[test]
    fn gamescope_session_counts_as_busy_for_the_shell() {
        let mut m = idle_unlocked();
        m.sessions[0].desktop = "gamescope".into();
        let p = plan(&inputs(vec![ch(ChangeKind::Modified, "/usr/share/rime-shell/shell.qml")], m));
        assert!(matches!(
            p.component(Component::Shell).unwrap().decision,
            Decision::Defer { reason: DeferReason::Busy(_) }
        ));
    }

    #[test]
    fn deferral_cascades_through_dependencies() {
        let mut inp = inputs(
            vec![
                ch(ChangeKind::Modified, "/usr/bin/rimed"),
                ch(ChangeKind::Modified, "/usr/bin/rime"),
                ch(ChangeKind::Modified, "/usr/share/rime-shell/shell.qml"),
                ch(ChangeKind::Modified, "/usr/share/rime/hypr/rime/keybindings.lua"),
            ],
            idle_unlocked(),
        );
        inp.elf.insert("/usr/bin/rimed".into(), Err("links libdbus-1.so.3, which this release changes".into()));
        let p = plan(&inp);
        assert!(matches!(
            p.component(Component::RimeDaemon).unwrap().decision,
            Decision::Defer { reason: DeferReason::Unproven(_) }
        ));
        for c in [Component::RimeTools, Component::Shell, Component::HyprlandConfig] {
            assert!(
                matches!(p.component(c).unwrap().decision, Decision::Defer { reason: DeferReason::Grouped(_) }),
                "{c:?}"
            );
        }
        assert!(p.live_set.is_empty());
    }

    #[test]
    fn unchecked_elf_is_unproven() {
        let mut inp = inputs(vec![ch(ChangeKind::Modified, "/usr/bin/rimed")], idle_unlocked());
        inp.elf.clear();
        let p = plan(&inp);
        assert!(matches!(
            p.component(Component::RimeDaemon).unwrap().decision,
            Decision::Defer { reason: DeferReason::Unproven(_) }
        ));
    }

    #[test]
    fn shell_waits_for_a_new_quickshell() {
        let p = plan(&inputs(
            vec![
                ch(ChangeKind::Modified, "/usr/share/rime-shell/shell.qml"),
                ch(ChangeKind::Modified, "/usr/lib64/libQt6Quick.so.6.11.0"),
            ],
            idle_unlocked(),
        ));
        assert!(matches!(
            p.component(Component::Shell).unwrap().decision,
            Decision::Defer { reason: DeferReason::Unproven(_) }
        ));
        assert_eq!(p.component(Component::ShellRuntime).unwrap().decision, Decision::Pending { requirement: Requirement::SoftReboot });
        assert!(p.recommendation.contains("userspace restart"));
    }

    #[test]
    fn mixed_release_reports_each_component() {
        let mut inp = inputs(
            vec![
                ch(ChangeKind::Modified, "/usr/share/rime-shell/shell.qml"),
                ch(ChangeKind::Modified, "/usr/lib/modules/7.2.10/vmlinuz"),
                ch(ChangeKind::Modified, "/usr/lib/modules/7.2.10/extra/nvidia/nvidia.ko.xz"),
                ch(ChangeKind::Modified, "/usr/lib/systemd/systemd"),
                ch(ChangeKind::Modified, "/usr/lib/bootupd/updates/EFI/fedora/grubx64.efi"),
            ],
            idle_unlocked(),
        );
        inp.machine.has_nvidia = Some(true);
        let p = plan(&inp);
        assert!(matches!(p.component(Component::Shell).unwrap().decision, Decision::Activate { .. }));
        assert_eq!(p.component(Component::Kernel).unwrap().decision, Decision::Pending { requirement: Requirement::KernelTransition });
        assert_eq!(p.component(Component::Nvidia).unwrap().decision, Decision::Pending { requirement: Requirement::DriverReload });
        assert_eq!(p.component(Component::Systemd).unwrap().decision, Decision::Pending { requirement: Requirement::SoftReboot });
        assert_eq!(p.component(Component::Bootloader).unwrap().decision, Decision::Pending { requirement: Requirement::Reboot });
        assert_eq!(p.remaining, Requirement::Reboot);
        assert_eq!(p.recommendation, "restart when convenient");
    }

    #[test]
    fn nvidia_change_on_amd_machine_is_not_applicable() {
        let p = plan(&inputs(
            vec![ch(ChangeKind::Modified, "/usr/lib64/libnvidia-glcore.so.620.1")],
            idle_unlocked(),
        ));
        assert!(matches!(p.component(Component::Nvidia).unwrap().decision, Decision::NotApplicable { .. }));
        assert_eq!(p.remaining, Requirement::Nothing);
    }

    #[test]
    fn running_unsafe_unit_is_left_running_and_reported() {
        let p = plan(&inputs(
            vec![ch(ChangeKind::Modified, "/usr/lib/systemd/system/rime-lid.service")],
            idle_unlocked(),
        ));
        let c = p.component(Component::RimeServices).unwrap();
        assert_eq!(c.decision, Decision::Activate { residual: Some(Requirement::AppRestart) });
        assert_eq!(p.actions, vec![Activator::DaemonReload]);
        assert!(c.notes[0].contains("rime-lid.service"));
    }

    #[test]
    fn agent_daemon_installs_live_and_is_not_restarted() {
        let p = plan(&inputs(vec![ch(ChangeKind::Modified, "/usr/bin/rime-agentd")], idle_unlocked()));
        assert_eq!(
            p.component(Component::RimeServices).unwrap().decision,
            Decision::Activate { residual: Some(Requirement::AppRestart) }
        );
        assert!(p.actions.is_empty());
        assert_eq!(p.live_set.len(), 1);
    }

    #[test]
    fn no_live_defers_and_only_filters() {
        let mut inp = inputs(
            vec![ch(ChangeKind::Modified, "/usr/share/rime-shell/shell.qml"), ch(ChangeKind::Modified, "/usr/libexec/rime-pkg")],
            idle_unlocked(),
        );
        inp.no_live = true;
        assert!(plan(&inp).live_set.is_empty());
        inp.no_live = false;
        inp.only = Some([Component::RimeTools].into());
        let p = plan(&inp);
        assert_eq!(p.live_set.len(), 1);
        assert!(matches!(
            p.component(Component::Shell).unwrap().decision,
            Decision::Defer { reason: DeferReason::NotRequested }
        ));
    }

    #[test]
    fn package_versions_are_reported() {
        let mut inp = inputs(vec![ch(ChangeKind::Modified, "/usr/lib/systemd/systemd")], idle_unlocked());
        inp.packages = vec![PackageChange {
            name: "systemd".into(),
            arch: "x86_64".into(),
            from: Some("262-3.fc45".into()),
            to: Some("262-4.fc45".into()),
        }];
        let p = plan(&inp);
        assert_eq!(p.component(Component::Systemd).unwrap().versions, vec!["systemd 262-3.fc45 → 262-4.fc45"]);
    }
}
