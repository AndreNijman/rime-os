//! Which component a changed path belongs to, and what activating it takes.
//!
//! The rules are deliberately narrow on the live side and broad on the other.
//! Only files Rime itself builds and owns can enter the live layer: the shell,
//! the Hyprland seed modules, the `rime*` programs and helpers, and their
//! units. Everything that arrives from a Fedora or third-party package is
//! classified precisely (kernel, NVIDIA, systemd, compositor, libraries…) and
//! left to the staged deployment, because a package's files are only known to
//! work as the set that package was built and tested in.
//!
//! What "not live" costs is stated per component instead of collapsed into
//! "reboot": a library or systemd change needs userspace restarted (a soft
//! reboot does it), a kernel change needs a new kernel, firmware needs the
//! firmware.

use super::diff::{Change, ChangeKind};
use super::{Component, Requirement};

/// How a live component's new code is brought into use once its files are in
/// the live layer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "action", content = "target", rename_all = "kebab-case")]
pub enum Activator {
    /// `systemctl daemon-reload`, after a system unit file changed.
    DaemonReload,
    /// `systemctl --user daemon-reload` in every session's manager.
    UserDaemonReload,
    /// Restart one system unit the engine knows is safe to restart.
    RestartUnit(String),
    /// Replace (or reload) Rime Shell in every unlocked graphical session.
    Shell,
    /// Re-seed the Rime-managed Hyprland modules and `hyprctl reload`.
    HyprlandConfig,
}

/// What one path is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathClass {
    pub component: Component,
    pub requirement: Requirement,
    /// Whether this file may be placed in the live layer at all.
    pub live: bool,
    /// Needs its `DT_NEEDED` closure proven unchanged before it can go live.
    pub elf_checked: bool,
    pub activator: Option<Activator>,
}

/// Paths that are part of the image but not of the running system.
///
/// `/var` in an image is copied once at install and never by an update; the
/// rpmdb and dnf's history are rewritten by every build whether or not a
/// package moved (the 2026.10.09 → .10 diff touches all of them and changed no
/// package). Reporting them would turn every release into "system files
/// changed".
pub fn ignored(path: &str) -> bool {
    path == "/var"
        || path.starts_with("/var/")
        || path.starts_with("/usr/lib/sysimage/")
        || path.starts_with("/usr/share/rpm/")
        || path == "/usr/lib/sysimage"
        || path == "/usr/share/rpm"
}

/// Long-running first-party services the engine may restart by itself, and
/// why each is safe.
///
/// * `rimed`: its one piece of user-chosen state, the held mode, is written to
///   /var/lib/rimed/mode and re-applied at every start (rime-os #107), and the
///   CLI already retries a missing bus name. Restarted only when the machine is
///   not busy, so no game's tuning is dropped mid-frame.
/// * `rime-remoted`: the phone relay; clients reconnect on their own.
///
/// Not here on purpose: `rime-agentd` (holds the PTYs of running agent
/// sessions), `rime-secretd` (an in-flight credential operation would fail),
/// `rime-lid` (holds the lid inhibitor; a restart is a window in which closing
/// the lid suspends work the user asked to keep running), `rime-aid`.
pub const RESTART_SAFE_UNITS: [&str; 2] = ["rimed.service", "rime-remoted.service"];

fn unit_name(path: &str) -> Option<&str> {
    let name = path.rsplit('/').next()?;
    let unit_suffixes = [".service", ".timer", ".path", ".socket", ".target", ".mount"];
    if unit_suffixes.iter().any(|s| name.ends_with(s)) {
        Some(name)
    } else {
        None
    }
}

/// A unit file or drop-in: `/usr/lib/systemd/system/x.service` or
/// `/usr/lib/systemd/system/x.service.d/10-y.conf`. Returns (unit, user?).
fn unit_of(path: &str) -> Option<(String, bool)> {
    for (dir, user) in [("/usr/lib/systemd/system/", false), ("/usr/lib/systemd/user/", true)] {
        if let Some(rest) = path.strip_prefix(dir) {
            let first = rest.split('/').next()?;
            let unit = first.strip_suffix(".d").unwrap_or(first);
            return unit_name(unit).map(|u| (u.to_string(), user));
        }
    }
    None
}

fn is_rime_unit(unit: &str) -> bool {
    unit.starts_with("rime-") || unit.starts_with("rimed.") || unit.starts_with("rimed@")
}

fn shared_object(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.ends_with(".so") || name.contains(".so.") || name.starts_with("ld-linux")
}

fn under(path: &str, dir: &str) -> bool {
    path == dir || path.starts_with(&format!("{dir}/"))
}

fn nvidia_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    (path.starts_with("/usr/lib/modules/") && (name.starts_with("nvidia") || path.contains("/extra/nvidia")))
        || under(path, "/usr/lib/firmware/nvidia")
        || name.starts_with("libnvidia")
        || name.starts_with("libcuda")
        || name.starts_with("libnvcuvid")
        || name.starts_with("libnvoptix")
        || name.contains("vdpau_nvidia")
        || name.starts_with("nvidia_drv")
        || (path.starts_with("/usr/bin/") && name.starts_with("nvidia-"))
        || (path.starts_with("/usr/share/vulkan/") && name.starts_with("nvidia"))
        || (path.starts_with("/usr/share/glvnd/") && name.contains("nvidia"))
        || (path.starts_with("/usr/share/egl/") && name.contains("nvidia"))
        || path.starts_with("/usr/lib/systemd/system/nvidia-")
}

/// Classify one path. Total: every path gets an answer, and the default for a
/// path no rule names is "not live, userspace restart".
pub fn classify_path(path: &str) -> PathClass {
    let not_live = |component, requirement| PathClass {
        component,
        requirement,
        live: false,
        elf_checked: false,
        activator: None,
    };
    let live = |component, requirement, elf_checked, activator| PathClass {
        component,
        requirement,
        live: true,
        elf_checked,
        activator,
    };

    // ── Rime-owned: eligible for the live layer ──────────────────────────────
    if path == "/usr/share/rime/release.json" {
        // Describes the staged image. Overlaying it would make every reader of
        // "which release is this" (the shell's what's-new flow, `rime
        // changelog`) claim a release whose kernel is not running.
        return not_live(Component::Metadata, Requirement::Nothing);
    }
    if under(path, "/usr/share/rime-shell") {
        return live(Component::Shell, Requirement::ServiceRestart, false, Some(Activator::Shell));
    }
    if under(path, "/usr/lib64/rime-shell") {
        // The compiled Rime.I18n module: a process restart, never an in-place
        // reload (a loaded .so is not unloaded), and its Qt closure must hold.
        return live(Component::Shell, Requirement::ServiceRestart, true, Some(Activator::Shell));
    }
    if under(path, "/usr/share/rime/hypr") {
        return live(
            Component::HyprlandConfig,
            Requirement::LiveReload,
            false,
            Some(Activator::HyprlandConfig),
        );
    }
    if path == "/usr/bin/rimed" {
        return live(
            Component::RimeDaemon,
            Requirement::ServiceRestart,
            true,
            Some(Activator::RestartUnit("rimed.service".into())),
        );
    }
    if let Some((unit, user)) = unit_of(path) {
        if unit == "rimed.service" {
            return live(
                Component::RimeDaemon,
                Requirement::ServiceRestart,
                false,
                Some(Activator::RestartUnit("rimed.service".into())),
            );
        }
        if is_rime_unit(&unit) {
            // A unit file is a definition: daemon-reload makes it current, and
            // the running instance (if any) changes only when restarted. The
            // planner decides that from whether the unit is running and on
            // RESTART_SAFE_UNITS.
            return live(
                Component::RimeServices,
                Requirement::NextUse,
                false,
                Some(if user { Activator::UserDaemonReload } else { Activator::DaemonReload }),
            );
        }
        if path.starts_with("/usr/lib/systemd/system/nvidia-") {
            return not_live(Component::Nvidia, Requirement::DriverReload);
        }
        return not_live(Component::SystemUnits, Requirement::SoftReboot);
    }
    if let Some(name) = path.strip_prefix("/usr/bin/") {
        match name {
            "rime" => return live(Component::RimeTools, Requirement::NextUse, true, None),
            "rime-remoted" => {
                return live(
                    Component::RimeServices,
                    Requirement::ServiceRestart,
                    true,
                    Some(Activator::RestartUnit("rime-remoted.service".into())),
                )
            }
            "rime-agentd" | "rime-secretd" | "rime-aid" => {
                return live(Component::RimeServices, Requirement::AppRestart, true, None)
            }
            _ => {}
        }
    }
    if let Some(name) = path.strip_prefix("/usr/libexec/") {
        if name.starts_with("rime-") || name.starts_with("__pycache__/rime-") {
            return live(Component::RimeTools, Requirement::NextUse, true, None);
        }
    }

    // ── Everything else: classified, never live ──────────────────────────────
    if nvidia_path(path) {
        return not_live(Component::Nvidia, Requirement::DriverReload);
    }
    if under(path, "/usr/lib/modules") || under(path, "/boot") || under(path, "/usr/lib/ostree-boot") {
        return not_live(Component::Kernel, Requirement::KernelTransition);
    }
    if under(path, "/usr/lib/firmware") {
        // Read by a driver when it initialises a device, which on a running
        // machine has already happened.
        return not_live(Component::FirmwareFiles, Requirement::Reboot);
    }
    if under(path, "/usr/lib/bootupd")
        || under(path, "/usr/lib/efi")
        || under(path, "/usr/share/efi")
        || under(path, "/usr/lib/systemd/boot")
    {
        return not_live(Component::Bootloader, Requirement::Reboot);
    }
    if under(path, "/usr/etc") || under(path, "/etc") {
        // ostree's three-way /etc merge runs when the new deployment is
        // finalised; doing it by hand here would re-implement it.
        return not_live(Component::EtcDefaults, Requirement::SoftReboot);
    }
    if path == "/usr/lib/systemd/systemd"
        || path.starts_with("/usr/lib/systemd/systemd-")
        || path.starts_with("/usr/lib64/systemd/")
        || path == "/usr/bin/systemctl"
    {
        return not_live(Component::Systemd, Requirement::SoftReboot);
    }
    {
        let name = path.rsplit('/').next().unwrap_or(path);
        let compositor = matches!(
            name,
            "Hyprland" | "hyprland" | "niri" | "labwc" | "Xwayland" | "hyprctl" | "gamescope"
        ) || name.starts_with("libhyprland")
            || name.starts_with("libaquamarine")
            || name.starts_with("libhyprutils")
            || name.starts_with("libhyprlang")
            || name.starts_with("libhyprgraphics")
            || name.starts_with("libhyprcursor");
        if compositor {
            return not_live(Component::Compositor, Requirement::SoftReboot);
        }
        if matches!(name, "quickshell" | "qs")
            || name.starts_with("libQt6")
            || under(path, "/usr/lib64/qt6")
            || under(path, "/usr/share/qt6")
        {
            return not_live(Component::ShellRuntime, Requirement::SoftReboot);
        }
    }
    if shared_object(path) {
        return not_live(Component::Libraries, Requirement::SoftReboot);
    }
    not_live(Component::OtherSystem, Requirement::SoftReboot)
}

/// A change with its classification, for the planner.
#[derive(Debug, Clone)]
pub struct Classified {
    pub change: Change,
    pub class: PathClass,
}

/// Classify a whole diff, dropping what is not part of the running system.
pub fn classify(changes: &[Change]) -> Vec<Classified> {
    changes
        .iter()
        .filter(|c| !ignored(&c.path))
        // A changed directory entry carries no content of its own; its files
        // are listed separately. ostree diff does not list directories; the
        // composefs dump does when their mode or label moved, and those land
        // on the same component as their files below.
        .map(|c| Classified { change: c.clone(), class: classify_path(&c.path) })
        .collect()
}

/// A removed file can only be removed from the live layer's view with an
/// overlay whiteout, which the live layer supports; removing one from the
/// shell tree is an ordinary part of a shell release.
pub fn removal_ok(c: &Classified) -> bool {
    c.change.kind != ChangeKind::Removed || c.class.live
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::diff::parse_ostree_diff;

    fn class(p: &str) -> (Component, Requirement, bool) {
        let c = classify_path(p);
        (c.component, c.requirement, c.live)
    }

    #[test]
    fn rime_owned_paths_are_live() {
        assert_eq!(class("/usr/share/rime-shell/shell.qml"), (Component::Shell, Requirement::ServiceRestart, true));
        assert_eq!(
            class("/usr/lib64/rime-shell/qml/Rime/I18n/librimei18n.so"),
            (Component::Shell, Requirement::ServiceRestart, true)
        );
        assert!(classify_path("/usr/lib64/rime-shell/qml/Rime/I18n/librimei18n.so").elf_checked);
        assert_eq!(class("/usr/bin/rimed"), (Component::RimeDaemon, Requirement::ServiceRestart, true));
        assert_eq!(class("/usr/lib/systemd/system/rimed.service"), (Component::RimeDaemon, Requirement::ServiceRestart, true));
        assert_eq!(class("/usr/bin/rime"), (Component::RimeTools, Requirement::NextUse, true));
        assert_eq!(class("/usr/libexec/rime-pkg"), (Component::RimeTools, Requirement::NextUse, true));
        assert_eq!(
            class("/usr/share/rime/hypr/rime/keybindings.lua"),
            (Component::HyprlandConfig, Requirement::LiveReload, true)
        );
        assert_eq!(
            class("/usr/lib/systemd/user/rime-update-notice.path"),
            (Component::RimeServices, Requirement::NextUse, true)
        );
        assert_eq!(
            classify_path("/usr/lib/systemd/system/rime-lid.service.d/10-x.conf").activator,
            Some(Activator::DaemonReload)
        );
    }

    #[test]
    fn agent_daemon_is_never_restarted_for_the_user() {
        assert_eq!(class("/usr/bin/rime-agentd"), (Component::RimeServices, Requirement::AppRestart, true));
        assert!(!RESTART_SAFE_UNITS.contains(&"rime-agentd.service"));
        assert!(!RESTART_SAFE_UNITS.contains(&"rime-lid.service"));
    }

    #[test]
    fn third_party_paths_are_classified_and_not_live() {
        assert_eq!(class("/usr/lib/modules/7.2.9/vmlinuz"), (Component::Kernel, Requirement::KernelTransition, false));
        assert_eq!(
            class("/usr/lib/modules/7.2.9/extra/nvidia/nvidia.ko.xz"),
            (Component::Nvidia, Requirement::DriverReload, false)
        );
        assert_eq!(class("/usr/lib64/libnvidia-glcore.so.615.71.09"), (Component::Nvidia, Requirement::DriverReload, false));
        assert_eq!(class("/usr/lib/firmware/nvidia/615/gsp_ga10x.bin"), (Component::Nvidia, Requirement::DriverReload, false));
        assert_eq!(class("/usr/lib/firmware/amdgpu/x.bin"), (Component::FirmwareFiles, Requirement::Reboot, false));
        assert_eq!(class("/usr/lib/systemd/systemd"), (Component::Systemd, Requirement::SoftReboot, false));
        assert_eq!(class("/usr/lib64/systemd/libsystemd-shared-262.so"), (Component::Systemd, Requirement::SoftReboot, false));
        assert_eq!(class("/usr/lib64/libc.so.6"), (Component::Libraries, Requirement::SoftReboot, false));
        assert_eq!(class("/usr/lib64/ld-linux-x86-64.so.2"), (Component::Libraries, Requirement::SoftReboot, false));
        assert_eq!(class("/usr/bin/Hyprland"), (Component::Compositor, Requirement::SoftReboot, false));
        assert_eq!(class("/usr/lib64/libQt6Core.so.6"), (Component::ShellRuntime, Requirement::SoftReboot, false));
        assert_eq!(class("/usr/bin/quickshell"), (Component::ShellRuntime, Requirement::SoftReboot, false));
        assert_eq!(class("/usr/lib/bootupd/updates/EFI/fedora/shimx64.efi"), (Component::Bootloader, Requirement::Reboot, false));
        assert_eq!(class("/usr/etc/hosts"), (Component::EtcDefaults, Requirement::SoftReboot, false));
        assert_eq!(class("/usr/lib/systemd/system/sshd.service"), (Component::SystemUnits, Requirement::SoftReboot, false));
        assert_eq!(class("/usr/bin/firefox"), (Component::OtherSystem, Requirement::SoftReboot, false));
        assert_eq!(class("/usr/share/rime/release.json"), (Component::Metadata, Requirement::Nothing, false));
    }

    #[test]
    fn the_real_1009_to_1010_release() {
        let changes = parse_ostree_diff(crate::live::diff::tests::DIFF_1009_1010).unwrap();
        let c = classify(&changes);
        // rpmdb, dnf history, ldconfig cache and logs are not the running system.
        assert_eq!(c.len(), 7, "{c:#?}");
        let comps: std::collections::BTreeSet<_> = c.iter().map(|x| x.class.component).collect();
        assert_eq!(
            comps.into_iter().collect::<Vec<_>>(),
            vec![Component::RimeTools, Component::RimeDaemon, Component::Metadata]
        );
        assert!(c.iter().all(|x| x.class.live || x.class.component == Component::Metadata));
    }
}
