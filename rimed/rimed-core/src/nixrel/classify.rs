//! Store path → live-engine component (§3.1 `components`, §10.1 "classification by store path").
//!
//! One function used by the client (to measure what changed between two closures it holds) and by any
//! publisher (to fill a manifest's `components`), so the two cannot disagree about what "shell-only" means.
//! Classification is by the store path's NAME (the part after `<hash>-`), which is the package's pname and
//! version: the hash says nothing about what a path is.

use std::collections::{BTreeMap, BTreeSet};

use crate::live::Component;

/// The name part of `/nix/store/<32 chars>-<name>`, or the whole input when it is not a store path.
pub fn store_name(path: &str) -> &str {
    let base = path.rsplit('/').next().unwrap_or(path);
    match base.split_once('-') {
        Some((hash, name)) if hash.len() == 32 => name,
        _ => base,
    }
}

fn starts_with_version(rest: &str) -> bool {
    rest.chars().next().is_some_and(|c| c.is_ascii_digit())
}

/// The component a store path belongs to.
pub fn component_of(path: &str) -> Component {
    let n = store_name(path);
    // Order matters: linux-pam and linux-firmware are not the kernel.
    if n.starts_with("linux-pam-") || n.starts_with("glibc-") || n == "glibc" {
        return Component::Libraries;
    }
    if n.starts_with("linux-firmware") || n.ends_with("-firmware") || n.contains("-firmware-") {
        return Component::FirmwareFiles;
    }
    if n.starts_with("nvidia-x11-") || n.starts_with("nvidia-open-") || n.starts_with("nvidia-settings-") || n.starts_with("nvidia-persistenced-") {
        return Component::Nvidia;
    }
    if let Some(rest) = n.strip_prefix("linux-") {
        if starts_with_version(rest) || rest.starts_with("rime-") {
            return Component::Kernel;
        }
    }
    if n == "kernel-modules" || n.starts_with("initrd-linux-") || n.starts_with("initrd-") || n.ends_with("-shrunk") {
        return Component::Kernel;
    }
    if n.starts_with("systemd-boot") || n.starts_with("grub-") || n.starts_with("shim-") {
        return Component::Bootloader;
    }
    if let Some(rest) = n.strip_prefix("systemd-") {
        if starts_with_version(rest) || rest.starts_with("minimal-") {
            return Component::Systemd;
        }
    }
    if n.starts_with("rime-shell-") || n == "rime-shell" {
        return Component::Shell;
    }
    if n.starts_with("rimed-") || n == "rimed" {
        return Component::RimeDaemon;
    }
    if n.starts_with("rime-libexec") {
        return Component::RimeTools;
    }
    if n.starts_with("rime-desktop-data") || n.starts_with("rime-hypr") {
        return Component::HyprlandConfig;
    }
    for p in ["hyprland-", "niri-", "labwc-", "xwayland-", "aquamarine-", "gamescope-"] {
        if n.starts_with(p) {
            return Component::Compositor;
        }
    }
    if n.starts_with("quickshell-") || n.starts_with("qtbase-") || n.starts_with("qtdeclarative-") || n.starts_with("qtwayland-") {
        return Component::ShellRuntime;
    }
    if n.starts_with("nixos-system-") {
        return Component::Metadata;
    }
    if n == "etc" || n.starts_with("etc-") {
        return Component::EtcDefaults;
    }
    if n == "system-units" || n == "user-units" || n.starts_with("unit-") {
        return Component::SystemUnits;
    }
    Component::OtherSystem
}

/// The paths of a closure, grouped by component.
pub fn by_component<'a>(closure: impl IntoIterator<Item = &'a str>) -> BTreeMap<Component, BTreeSet<String>> {
    let mut m: BTreeMap<Component, BTreeSet<String>> = BTreeMap::new();
    for p in closure {
        m.entry(component_of(p)).or_default().insert(p.to_string());
    }
    m
}

/// Components whose set of store paths differs between two closures.
pub fn changed(
    from: &BTreeMap<Component, BTreeSet<String>>,
    to: &BTreeMap<Component, BTreeSet<String>>,
) -> BTreeSet<Component> {
    let empty = BTreeSet::new();
    from.keys()
        .chain(to.keys())
        .filter(|c| from.get(c).unwrap_or(&empty) != to.get(c).unwrap_or(&empty))
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str) -> String {
        format!("/nix/store/{}-{name}", "a".repeat(32))
    }

    #[test]
    fn names_map_to_components() {
        let rows = [
            ("linux-6.12.41", Component::Kernel),
            ("linux-7.2.9-cachyos1-rime1-modules", Component::Kernel),
            ("linux-rime-7.2.9", Component::Kernel),
            ("kernel-modules", Component::Kernel),
            ("initrd-linux-6.12.41", Component::Kernel),
            ("linux-pam-1.7.1", Component::Libraries),
            ("glibc-2.42-47", Component::Libraries),
            ("linux-firmware-20251021-zstd", Component::FirmwareFiles),
            ("nvidia-x11-615.78.08-6.12.41", Component::Nvidia),
            ("systemd-258.1", Component::Systemd),
            ("systemd-minimal-258.1", Component::Systemd),
            ("systemd-boot-258.1", Component::Bootloader),
            ("rime-shell-0-unstable-2026-11-01", Component::Shell),
            ("rimed-0.1.0", Component::RimeDaemon),
            ("rime-libexec-0.1.0", Component::RimeTools),
            ("hyprland-0.57.0", Component::Compositor),
            ("quickshell-0.3.1", Component::ShellRuntime),
            ("nixos-system-rime-26.11", Component::Metadata),
            ("etc", Component::EtcDefaults),
            ("unit-greetd.service", Component::SystemUnits),
            ("system-units", Component::SystemUnits),
            ("bash-5.3", Component::OtherSystem),
        ];
        for (n, want) in rows {
            assert_eq!(component_of(&p(n)), want, "{n}");
        }
    }

    #[test]
    fn a_shell_only_change_changes_only_the_shell_and_the_wrappers() {
        let a = [p("rime-shell-1"), p("glibc-2.42"), p("nixos-system-x"), p("bash-5.3")];
        let b = [p("rime-shell-2"), p("glibc-2.42"), p("nixos-system-y"), p("bash-5.3")];
        let ch = changed(&by_component(a.iter().map(String::as_str)), &by_component(b.iter().map(String::as_str)));
        assert_eq!(ch, [Component::Shell, Component::Metadata].into_iter().collect());
    }
}
