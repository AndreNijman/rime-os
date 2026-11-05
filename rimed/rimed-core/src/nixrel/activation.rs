//! Boot or switch (docs/nixos/updates-and-release.md §9), and the parser for
//! `switch-to-configuration dry-activate`.
//!
//! The rule, in one line: an update is applied live (`switch`) only when it is proven not to touch anything a
//! running process depends on; everything else installs for the next restart (`boot`). "Could not tell" is never
//! permission: an unreadable booted generation, an unmeasured running generation, a dry run that did not run or
//! printed something the parser does not know, an unknown busy state and an unknown lock state all mean `boot`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::live::{Component, DeferReason, MachineState};

/// Units a live switch may stop or restart (§9 rule 3). Grows one unit at a time, each with a VM test.
/// `polkit.service`: nixpkgs restarts it whenever `system.path` changes (`restartTriggers = [ config.system.path ]`),
/// and every Rime package (the Shell, rimed, the polkit actions) is in `system.path`, so without it no Rime-only release
/// could go live. It holds no session state; nix/tests/update-client.nix proves the restart happens and the switch lands.
/// `systemd-udevd.service`: nixpkgs restarts it when the udev rules or hwdb change (a desktop brings its own rules); it
/// holds no session state and replays nothing missed (nix/tests/desktops.nix, desktops-install-gnome).
/// `geoclue.service`: restarted when its agent whitelist changes (GNOME adds gnome-shell's); clients reconnect
/// (nix/tests/desktops.nix, desktops-install-gnome).
/// `systemd-tmpfiles-resetup.service`: a oneshot (`systemd-tmpfiles --create --remove`, no `--boot`) that every switch
/// starts through sysinit-reactivation.target and restarts when `/etc/tmpfiles.d` changes; it runs no process past its
/// own run. The first switch of a boot only starts it, so the second one restarts it: `rime remove gnome` after `rime
/// install gnome` in the same boot (desktops-install-gnome, measured).
pub const RESTART_SAFE_UNITS: &[&str] = &[
    "rimed.service",
    "rime-remoted.service",
    "polkit.service",
    "systemd-udevd.service",
    "geoclue.service",
    "systemd-tmpfiles-resetup.service",
];

/// Units a live switch may RELOAD (but not stop or restart). `dbus-broker.service`: nixpkgs reloads it
/// (`reloadIfChanged = true`) when its config dir, built from `system.path`, changes; a reload re-reads the policy and
/// keeps every connection. A restart of it would end every session's bus, so a restart still means `boot`.
pub const RELOAD_SAFE_UNITS: &[&str] = &["dbus-broker.service"];

/// What a generation boots: from its `boot.json` (bootspec) and `kernel-modules` link, or from a manifest's `boot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootFacts {
    pub kernel: String,
    pub initrd: String,
    pub kernel_modules: Option<String>,
    pub kernel_params: Vec<String>,
}

/// What `switch-to-configuration dry-activate` said it would do.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DryRun {
    pub restart_systemd: bool,
    /// Units (system and user) it would stop or restart.
    pub disruptive: BTreeSet<String>,
    /// Units (system and user) it would reload.
    pub reload: BTreeSet<String>,
    /// Units it would only start (new units): always allowed.
    pub start: BTreeSet<String>,
    /// Changed units it would NOT touch (`restartIfChanged = false`, e.g. greetd).
    pub skipped: BTreeSet<String>,
    /// System accounts the activation would remove (update-users-groups' "would remove user ‘…’"): the accounts of
    /// services the target no longer declares. Human accounts are never declared (`users.mutableUsers`), so they are
    /// never on this list; one with an open session still means `boot`.
    #[serde(default)]
    pub removed_users: BTreeSet<String>,
    /// The other account lines it printed (groups removed, users and groups revived with their old ids).
    #[serde(default)]
    pub accounts: Vec<String>,
    /// Lines the parser did not recognise. Any of them means `boot`.
    pub unrecognised: Vec<String>,
}

/// The name inside update-users-groups.pl's quotes: `‘colord’` (remove) or `'colord' with GID 990` (revive).
fn quoted_name(rest: &str) -> Option<&str> {
    let rest = rest.strip_prefix('‘').or_else(|| rest.strip_prefix('\''))?;
    let end = rest.find(['’', '\''])?;
    Some(&rest[..end]).filter(|n| !n.is_empty())
}

fn units(list: &str) -> impl Iterator<Item = String> + '_ {
    list.split(',').map(str::trim).filter(|u| !u.is_empty()).map(str::to_string)
}

/// Parse the dry run's stderr+stdout. The recognised lines are exactly what nixpkgs' `switch-to-configuration-ng`
/// prints in dry-activate mode at the pinned revision (src/main.rs: "would stop/reload/restart/start the following
/// [user ]units: …", "would NOT stop the following changed units: …", "would NOT restart the following user units:
/// …", "would restart systemd", "would activate the configuration...", and the deprecation WARN line).
pub fn parse_dry_activate(text: &str) -> DryRun {
    let mut d = DryRun::default();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty()
            || line == "would activate the configuration..."
            || line.starts_with("WARN: restarting or reloading systemd units from the activation script is deprecated")
            // switchable-system.nix's pre-switch check skips the switch inhibitors on dry-activate (a real `test`
            // checks them, and refuses, before activating anything).
            || line == "Not checking switch inhibitors (action = dry-activate)"
        {
            continue;
        }
        if line == "would restart systemd" {
            d.restart_systemd = true;
            continue;
        }
        // The account bookkeeping of nixos/modules/config/update-users-groups.pl in dry mode (its `dry_print`). A
        // desktop removed takes its service accounts with it (GNOME: colord); one added again revives them. Its
        // "warning: would not apply …" lines stay unrecognised.
        if let Some(rest) = line.strip_prefix("would remove user ") {
            if let Some(name) = quoted_name(rest) {
                d.removed_users.insert(name.to_string());
                continue;
            }
        }
        if ["would remove group ", "would revive group ", "would revive user "]
            .iter()
            .any(|p| line.strip_prefix(p).and_then(quoted_name).is_some())
        {
            d.accounts.push(line.to_string());
            continue;
        }
        let mut matched = false;
        for (prefix, kind) in [
            ("would stop the following units: ", 'd'),
            ("would reload the following units: ", 'r'),
            ("would restart the following units: ", 'd'),
            ("would start the following units: ", 's'),
            ("would NOT stop the following changed units: ", 'k'),
            ("would stop the following user units: ", 'd'),
            ("would reload the following user units: ", 'r'),
            ("would restart the following user units: ", 'd'),
            ("would start the following user units: ", 's'),
            ("would NOT restart the following user units: ", 'k'),
        ] {
            if let Some(rest) = line.strip_prefix(prefix) {
                let set = match kind {
                    'd' => &mut d.disruptive,
                    'r' => &mut d.reload,
                    's' => &mut d.start,
                    _ => &mut d.skipped,
                };
                set.extend(units(rest));
                matched = true;
                break;
            }
        }
        if !matched {
            d.unrecognised.push(line.to_string());
        }
    }
    d
}

/// The dry run's state when [`activation`] is asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DryActivate {
    /// Not run yet (the plan before download): rule 3 is open and the answer is provisional.
    NotYet,
    Ran(DryRun),
    /// It could not be run, or exited non-zero.
    Failed(String),
}

/// The closures, grouped by component ([`super::classify::by_component`]).
pub type ByComponent = BTreeMap<Component, BTreeSet<String>>;

pub struct Inputs<'a> {
    /// `/run/booted-system`'s boot facts; `None` = could not be read.
    pub booted_boot: Option<&'a BootFacts>,
    /// `/run/booted-system`'s closure; `None` = could not be measured.
    pub booted: Option<&'a ByComponent>,
    /// `/run/current-system`'s closure; `None` = could not be measured.
    pub current: Option<&'a ByComponent>,
    pub target_boot: &'a BootFacts,
    pub target: &'a ByComponent,
    pub dry_run: &'a DryActivate,
    pub machine: &'a MachineState,
    /// Units (system and user) only one of the running and target generations has (their `etc/systemd/{system,user}`):
    /// a desktop's own units when it is added or removed (docs/nixos/desktops.md §5). Rule 3 lets activation stop or
    /// start them; a unit both generations have is judged as before.
    pub one_sided: &'a BTreeSet<String>,
    /// The logind `Desktop` of every desktop in the catalogue that is not Rime's (`gnome`, `plasma`, `cosmic`). Their
    /// sessions run no Rime Shell, so rule 6 does not wait for them to unlock.
    pub other_desktops: &'a BTreeSet<String>,
}

/// `foo@bar.service` is in `set` as itself or as its template `foo@.service`.
fn unit_in(set: &BTreeSet<String>, unit: &str) -> bool {
    if set.contains(unit) {
        return true;
    }
    match (unit.split_once('@'), unit.rsplit_once('.')) {
        (Some((name, _)), Some((_, kind))) => set.contains(&format!("{name}@.{kind}")),
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Installs for the next restart; nothing is activated now.
    Boot,
    /// Installs for the next restart AND activates now.
    Switch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activation {
    pub kind: Kind,
    /// Components whose store paths differ between the running generation and the target.
    pub changed: BTreeSet<Component>,
    /// Why it is `boot`: one line per rule that fired (`"rule 1: …"`). Empty for `switch`.
    pub reasons: Vec<String>,
    /// Components that would have gone live but wait for a reason the user can clear (busy, locked): `rime live
    /// apply` activates them later. Only set when rules 5/6 are the only reasons.
    pub deferred: Vec<(Component, DeferReason)>,
    /// `kind` is provisional: rule 3 still needs the dry run (which runs only on verified, downloaded code).
    pub needs_dry_run: bool,
}

/// Components that never change under a running system without a restart.
const BOOT_ONLY: &[Component] = &[Component::Kernel, Component::Systemd, Component::Bootloader, Component::FirmwareFiles];

fn differs(a: Option<&ByComponent>, b: &ByComponent, c: Component) -> Option<bool> {
    let empty = BTreeSet::new();
    a.map(|a| a.get(&c).unwrap_or(&empty) != b.get(&c).unwrap_or(&empty))
}

/// The dbus and dbus-broker builds in a closure (`dbus-1.16.2`, `dbus-1.16.2-lib`, `dbus-broker-37`), not the
/// `dbus-1` config dir and not unrelated `dbus-*` libraries (`dbus-glib-…`).
fn bus_daemon(m: &ByComponent) -> BTreeSet<&str> {
    let versioned = |rest: &str| rest.chars().next().is_some_and(|c| c.is_ascii_digit());
    m.values()
        .flatten()
        .map(String::as_str)
        .filter(|p| {
            let n = super::classify::store_name(p);
            n.strip_prefix("dbus-broker-").is_some_and(versioned)
                || n.strip_prefix("dbus-").is_some_and(|r| versioned(r) && r.contains('.'))
        })
        .collect()
}

/// §9, rules 1-6, plus the kept `(HyprlandConfig, Compositor)` pairing of §10.1.
pub fn activation(i: &Inputs) -> Activation {
    let mut reasons = Vec::new();
    let mut user_clearable: Vec<DeferReason> = Vec::new();
    let mut needs_dry_run = false;

    let changed = match i.current {
        Some(cur) => super::classify::changed(cur, i.target),
        None => {
            reasons.push("the running generation could not be measured".to_string());
            i.target.keys().copied().collect()
        }
    };

    // Rule 1: against the BOOTED generation, so a pending boot-class update keeps every later one boot-class.
    match i.booted_boot {
        None => reasons.push("rule 1: the booted generation's kernel could not be read".into()),
        Some(b) => {
            let t = i.target_boot;
            for (what, same) in [
                ("kernel", b.kernel == t.kernel),
                ("initrd", b.initrd == t.initrd),
                ("kernel modules", b.kernel_modules == t.kernel_modules),
                ("kernel parameters", b.kernel_params == t.kernel_params),
            ] {
                if !same {
                    reasons.push(format!("rule 1: the {what} changed from the booted generation's"));
                }
            }
        }
    }

    // Rule 2: NVIDIA userspace must match the loaded kernel module (#113). A machine known to have no NVIDIA GPU
    // loads no module, so the rule does not apply there.
    if i.machine.has_nvidia != Some(false) {
        match differs(i.booted, i.target, Component::Nvidia) {
            Some(false) => {}
            Some(true) => reasons.push("rule 2: the NVIDIA driver changes".into()),
            None => reasons.push("rule 2: the booted generation's NVIDIA driver could not be measured".into()),
        }
    }

    // Rule 3: what activation would do to running units.
    match i.dry_run {
        DryActivate::NotYet => needs_dry_run = true,
        DryActivate::Failed(e) => reasons.push(format!("rule 3: the dry activation failed ({e})")),
        DryActivate::Ran(d) => {
            if d.restart_systemd {
                reasons.push("rule 3: it would restart systemd".into());
            }
            let outside: Vec<&str> = d
                .disruptive
                .iter()
                .map(String::as_str)
                .filter(|u| !RESTART_SAFE_UNITS.contains(u) && !unit_in(i.one_sided, u))
                .collect();
            if !outside.is_empty() {
                reasons.push(format!("rule 3: it would stop or restart {}", outside.join(", ")));
            }
            let outside: Vec<&str> = d
                .reload
                .iter()
                .map(String::as_str)
                .filter(|u| !RELOAD_SAFE_UNITS.contains(u) && !RESTART_SAFE_UNITS.contains(u))
                .collect();
            if !outside.is_empty() {
                reasons.push(format!("rule 3: it would reload {}", outside.join(", ")));
            }
            let logged_in: Vec<&str> = d
                .removed_users
                .iter()
                .map(String::as_str)
                .filter(|u| i.machine.sessions.iter().any(|s| s.user == *u))
                .collect();
            if !logged_in.is_empty() {
                reasons.push(format!("rule 3: it would remove the account of {}, logged in now", logged_in.join(", ")));
            }
            if let Some(l) = d.unrecognised.first() {
                reasons.push(format!("rule 3: the dry activation printed something not understood ({l})"));
            }
        }
    }

    // Rule 4: glibc / linux-pam, against both the running and the booted generation.
    for (what, against) in [("running", i.current), ("booted", i.booted)] {
        match differs(against, i.target, Component::Libraries) {
            Some(false) => {}
            Some(true) => reasons.push(format!("rule 4: glibc or linux-pam differ from the {what} generation's")),
            None => reasons.push(format!("rule 4: the {what} generation's glibc could not be measured")),
        }
    }

    // Parts of the system that only a restart replaces.
    for c in BOOT_ONLY {
        if changed.contains(c) {
            reasons.push(format!("{} changes", c.label()));
        }
    }

    // The message bus daemon. nixpkgs only RELOADS dbus-broker when its unit changes (`reloadIfChanged = true`), so a
    // new dbus or dbus-broker build would pass rule 3 as a reload while the daemon started at boot keeps running: it is
    // compared with the BOOTED generation, the one whose daemon runs. Its config dir (`dbus-1`, rebuilt from
    // system.path by every Rime-only release) is not the daemon and is not matched.
    if let Some(b) = i.booted {
        if bus_daemon(b) != bus_daemon(i.target) {
            reasons.push("the D-Bus daemon (dbus or dbus-broker) changes from the booted generation's".into());
        }
    }

    // §10.1: the running compositor keeps running; a new Hyprland configuration for an old Hyprland is not proven.
    if changed.contains(&Component::HyprlandConfig) && changed.contains(&Component::Compositor) {
        reasons.push("the Hyprland configuration changes together with the compositor".into());
    }

    // Rule 5: busy, or could not be told.
    match &i.machine.busy {
        Some(None) => {}
        Some(Some(why)) => user_clearable.push(DeferReason::Busy(why.clone())),
        None => user_clearable.push(DeferReason::Busy("whether a game is running could not be determined".into())),
    }

    // Rule 6: a locked or unknown session, and the switch would restart the Shell or reload Hyprland config. Only a
    // session that may run the Shell counts: another desktop's (GNOME, Plasma, COSMIC) runs none.
    let per_session = changed.contains(&Component::Shell) || changed.contains(&Component::HyprlandConfig);
    if per_session {
        let shell_sessions = i.machine.sessions.iter().filter(|s| !i.other_desktops.contains(&s.desktop));
        if let Some(s) = shell_sessions.into_iter().find(|s| s.locked != Some(false)) {
            let state = if s.locked == Some(true) { "locked" } else { "lock state unknown" };
            user_clearable.push(DeferReason::Locked(format!("{}'s session {} is {state}", s.user, s.session_id)));
        }
    }

    let deferred = if reasons.is_empty() && !user_clearable.is_empty() {
        let live: Vec<Component> = changed.iter().copied().filter(|c| *c != Component::Metadata).collect();
        live.into_iter().flat_map(|c| user_clearable.iter().map(move |r| (c, r.clone()))).collect()
    } else {
        Vec::new()
    };
    for r in &user_clearable {
        reasons.push(match r {
            DeferReason::Busy(_) => format!("rule 5: {}", r.describe()),
            _ => format!("rule 6: {}", r.describe()),
        });
    }

    Activation {
        kind: if reasons.is_empty() { Kind::Switch } else { Kind::Boot },
        changed,
        reasons,
        deferred,
        needs_dry_run,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::SessionState;
    use crate::nixrel::classify::by_component;

    fn p(name: &str) -> String {
        format!("/nix/store/{}-{name}", "a".repeat(32))
    }

    fn closure(names: &[&str]) -> ByComponent {
        let v: Vec<String> = names.iter().map(|n| p(n)).collect();
        by_component(v.iter().map(String::as_str))
    }

    const A: &[&str] = &["nixos-system-a", "rime-shell-1", "glibc-2.42", "linux-6.12.41", "bash-5.3", "hyprland-0.57.0", "rime-hypr-1"];
    const B_SHELL: &[&str] = &["nixos-system-b", "rime-shell-2", "glibc-2.42", "linux-6.12.41", "bash-5.3", "hyprland-0.57.0", "rime-hypr-1"];

    fn boot(k: &str) -> BootFacts {
        BootFacts { kernel: p(k), initrd: p("initrd-linux-6.12.41"), kernel_modules: Some(p("kernel-modules")), kernel_params: vec!["quiet".into()] }
    }

    fn idle_unlocked() -> MachineState {
        MachineState {
            busy: Some(None),
            sessions: vec![SessionState { user: "alex".into(), uid: 1000, session_id: "2".into(), desktop: "hyprland".into(), locked: Some(false) }],
            has_nvidia: Some(false),
        }
    }

    fn quiet() -> DryActivate {
        DryActivate::Ran(parse_dry_activate("would activate the configuration...\n"))
    }

    struct Case {
        booted_boot: Option<BootFacts>,
        booted: Option<ByComponent>,
        current: Option<ByComponent>,
        target_boot: BootFacts,
        target: ByComponent,
        dry: DryActivate,
        machine: MachineState,
        one_sided: BTreeSet<String>,
        other_desktops: BTreeSet<String>,
    }

    impl Case {
        fn shell_only() -> Case {
            Case {
                booted_boot: Some(boot("linux-6.12.41")),
                booted: Some(closure(A)),
                current: Some(closure(A)),
                target_boot: boot("linux-6.12.41"),
                target: closure(B_SHELL),
                dry: quiet(),
                machine: idle_unlocked(),
                one_sided: BTreeSet::new(),
                other_desktops: ["gnome", "plasma", "cosmic"].map(String::from).into(),
            }
        }
        fn run(&self) -> Activation {
            activation(&Inputs {
                booted_boot: self.booted_boot.as_ref(),
                booted: self.booted.as_ref(),
                current: self.current.as_ref(),
                target_boot: &self.target_boot,
                target: &self.target,
                dry_run: &self.dry,
                machine: &self.machine,
                one_sided: &self.one_sided,
                other_desktops: &self.other_desktops,
            })
        }
    }

    fn has(a: &Activation, needle: &str) -> bool {
        a.reasons.iter().any(|r| r.contains(needle))
    }

    #[test]
    fn a_shell_only_release_switches() {
        let a = Case::shell_only().run();
        assert_eq!(a.kind, Kind::Switch, "{:?}", a.reasons);
        assert!(a.changed.contains(&Component::Shell));
        assert!(!a.needs_dry_run);
    }

    #[test]
    fn rule_1_kernel_against_the_booted_generation() {
        let mut c = Case::shell_only();
        c.target_boot = boot("linux-6.12.42");
        assert!(has(&c.run(), "rule 1: the kernel"));
        let mut c = Case::shell_only();
        c.target_boot.kernel_params.push("mitigations=off".into());
        assert!(has(&c.run(), "rule 1: the kernel parameters"));
        let mut c = Case::shell_only();
        c.booted_boot = None;
        assert_eq!(c.run().kind, Kind::Boot);
    }

    #[test]
    fn a_pending_boot_class_update_then_a_shell_only_update_stays_boot() {
        // Booted A on kernel .41; current is still A (the pending C is only in the profile); C' = C + new shell,
        // kernel .42. Compared with current alone the kernel paths in the CLOSURE do differ, but the point is the
        // boot facts: compared with booted they differ, even when the running closure already had .42 (switch-only
        // history), so the result must be boot.
        let mut c = Case::shell_only();
        let with_new_kernel: Vec<&str> = B_SHELL.iter().map(|n| if *n == "linux-6.12.41" { "linux-6.12.42" } else { n }).collect();
        c.current = Some(closure(&with_new_kernel));
        c.target = closure(&with_new_kernel.iter().map(|n| if *n == "rime-shell-2" { "rime-shell-3" } else { n }).collect::<Vec<_>>());
        c.target_boot = boot("linux-6.12.42");
        let a = c.run();
        assert_eq!(a.kind, Kind::Boot);
        assert!(has(&a, "rule 1: the kernel changed from the booted"), "{:?}", a.reasons);
    }

    #[test]
    fn rule_2_nvidia_unless_the_machine_has_none() {
        let mut c = Case::shell_only();
        c.machine.has_nvidia = Some(true);
        c.booted = Some(closure(&[A, &["nvidia-x11-615.78-6.12.41"]].concat()));
        c.current = c.booted.clone();
        c.target = closure(&[B_SHELL, &["nvidia-x11-615.80-6.12.41"]].concat());
        assert!(has(&c.run(), "rule 2"));
        c.machine.has_nvidia = None;
        assert!(has(&c.run(), "rule 2"));
        // NVIDIA changed on a machine without NVIDIA: no module is loaded, nothing to mismatch.
        c.machine.has_nvidia = Some(false);
        assert!(!has(&c.run(), "rule 2"));
    }

    #[test]
    fn rule_3_dry_activation() {
        let mut c = Case::shell_only();
        c.dry = DryActivate::Ran(parse_dry_activate("would restart systemd\n"));
        assert!(has(&c.run(), "rule 3: it would restart systemd"));
        c.dry = DryActivate::Ran(parse_dry_activate("would restart the following units: dbus.service, rimed.service\n"));
        let a = c.run();
        assert!(has(&a, "dbus.service") && !has(&a, "rimed.service"), "{:?}", a.reasons);
        c.dry = DryActivate::Ran(parse_dry_activate("would restart the following units: rimed.service\nwould start the following units: new.service\nwould NOT stop the following changed units: greetd.service\n"));
        assert_eq!(c.run().kind, Kind::Switch);
        // The display manager, if it were ever restarted by a switch (restartIfChanged = true): every session ends.
        c.dry = DryActivate::Ran(parse_dry_activate("would restart the following units: greetd.service\n"));
        let a = c.run();
        assert!(a.kind == Kind::Boot && has(&a, "greetd.service"), "{:?}", a.reasons);
        // What a Shell-only release prints at the pinned nixpkgs (system.path moved): a dbus-broker reload and a polkit
        // restart. Both are allowed; a dbus-broker RESTART is not (every bus connection would end).
        c.dry = DryActivate::Ran(parse_dry_activate(
            "would stop the following units: polkit.service\nwould reload the following units: dbus-broker.service\n\
             would start the following units: polkit.service\n",
        ));
        assert_eq!(c.run().kind, Kind::Switch, "{:?}", c.run().reasons);
        c.dry = DryActivate::Ran(parse_dry_activate("would restart the following units: dbus-broker.service\n"));
        let a = c.run();
        assert!(a.kind == Kind::Boot && has(&a, "rule 3: it would stop or restart dbus-broker.service"), "{:?}", a.reasons);
        c.dry = DryActivate::Ran(parse_dry_activate("would reload the following units: NetworkManager.service\n"));
        let a = c.run();
        assert!(a.kind == Kind::Boot && has(&a, "rule 3: it would reload NetworkManager.service"), "{:?}", a.reasons);
        c.dry = DryActivate::Ran(parse_dry_activate("would stop swap device: /dev/vda2\n"));
        assert!(has(&c.run(), "not understood"));
        c.dry = DryActivate::Failed("exit 1".into());
        assert_eq!(c.run().kind, Kind::Boot);
        c.dry = DryActivate::NotYet;
        let a = c.run();
        assert!(a.needs_dry_run && a.kind == Kind::Switch);
    }

    #[test]
    fn a_new_bus_daemon_waits_for_a_restart_but_its_config_dir_does_not() {
        let with = |extra: &[&'static str]| closure(&[A, extra].concat());
        let mut c = Case::shell_only();
        c.booted = Some(with(&["dbus-1.16.2", "dbus-1.16.2-lib", "dbus-broker-37", "dbus-1", "dbus-glib-0.112"]));
        c.current = c.booted.clone();
        // A Rime-only release: only the config dir (built from system.path) and an unrelated dbus library move.
        let mut t: Vec<&str> = B_SHELL.to_vec();
        t.extend(["dbus-1.16.2", "dbus-1.16.2-lib", "dbus-broker-37"]);
        let mut cfg_only = closure(&t);
        cfg_only.entry(Component::OtherSystem).or_default().insert(format!("/nix/store/{}-dbus-1", "b".repeat(32)));
        cfg_only.entry(Component::OtherSystem).or_default().insert(format!("/nix/store/{}-dbus-glib-0.114", "b".repeat(32)));
        c.target = cfg_only;
        let a = c.run();
        assert_eq!(a.kind, Kind::Switch, "{:?}", a.reasons);
        // A new dbus-broker: a reload would keep the old daemon running, so it waits for a restart.
        c.target = closure(&[B_SHELL, &["dbus-1.16.2", "dbus-1.16.2-lib", "dbus-broker-38", "dbus-1"]].concat());
        let a = c.run();
        assert!(a.kind == Kind::Boot && has(&a, "D-Bus daemon"), "{:?}", a.reasons);
        // A new dbus (the library and dbus-daemon tools dbus-broker links).
        c.target = closure(&[B_SHELL, &["dbus-1.16.4", "dbus-1.16.4-lib", "dbus-broker-37", "dbus-1"]].concat());
        assert!(has(&c.run(), "D-Bus daemon"));
    }

    #[test]
    fn rule_4_glibc() {
        let mut c = Case::shell_only();
        c.target = closure(&B_SHELL.iter().map(|n| if *n == "glibc-2.42" { "glibc-2.42-48" } else { n }).collect::<Vec<_>>());
        assert!(has(&c.run(), "rule 4"));
    }

    #[test]
    fn rule_5_busy_or_unknown_defers() {
        let mut c = Case::shell_only();
        c.machine.busy = Some(Some("Gaming Mode is on".into()));
        let a = c.run();
        assert_eq!(a.kind, Kind::Boot);
        assert!(a.deferred.iter().any(|(comp, r)| *comp == Component::Shell && matches!(r, DeferReason::Busy(_))));
        c.machine.busy = None;
        assert_eq!(c.run().kind, Kind::Boot);
    }

    #[test]
    fn rule_6_locked_with_shell_defers_but_tools_only_proceeds() {
        let mut c = Case::shell_only();
        c.machine.sessions[0].locked = Some(true);
        let a = c.run();
        assert_eq!(a.kind, Kind::Boot);
        assert!(a.deferred.iter().any(|(comp, r)| *comp == Component::Shell && matches!(r, DeferReason::Locked(_))));
        c.machine.sessions[0].locked = None;
        assert_eq!(c.run().kind, Kind::Boot);

        let tools_a = ["nixos-system-a", "rime-libexec-1", "glibc-2.42", "linux-6.12.41"];
        let tools_b = ["nixos-system-b", "rime-libexec-2", "glibc-2.42", "linux-6.12.41"];
        let mut c = Case::shell_only();
        c.booted = Some(closure(&tools_a));
        c.current = Some(closure(&tools_a));
        c.target = closure(&tools_b);
        c.machine.sessions[0].locked = Some(true);
        assert_eq!(c.run().kind, Kind::Switch, "{:?}", c.run().reasons);
    }

    #[test]
    fn rule_3_lets_a_removed_desktops_service_accounts_go() {
        // What `rime remove gnome` printed in desktops-install-gnome (after `rime install gnome` in the same boot).
        let text = "would remove group ‘colord’\nwould remove user ‘colord’\n\
                    would restart the following units: systemd-tmpfiles-resetup.service\n";
        let d = parse_dry_activate(text);
        assert!(d.unrecognised.is_empty(), "{:?}", d.unrecognised);
        assert_eq!(d.removed_users, ["colord".to_string()].into());
        let mut c = Case::shell_only();
        c.dry = DryActivate::Ran(d.clone());
        assert_eq!(c.run().kind, Kind::Switch, "{:?}", c.run().reasons);
        // Installing it again revives them with their old ids.
        let d = parse_dry_activate("would revive group 'colord' with GID 990\nwould revive user 'colord' with UID 990\n");
        assert!(d.unrecognised.is_empty() && d.accounts.len() == 2, "{d:?}");
        // An account with an open session is not removed under it.
        let mut c = Case::shell_only();
        let user = c.machine.sessions[0].user.clone();
        c.dry = DryActivate::Ran(parse_dry_activate(&format!("would remove user ‘{user}’\n")));
        let a = c.run();
        assert!(a.kind == Kind::Boot && has(&a, "would remove the account of"), "{:?}", a.reasons);
        // update-users-groups' warnings, and anything else, still mean a restart.
        for l in ["warning: would not apply GID change of group ‘x’ (1 -> 2) in /etc/group", "would remove group"] {
            assert_eq!(parse_dry_activate(l).unrecognised, [l.to_string()], "{l}");
        }
    }

    #[test]
    fn rule_3_allows_udevd_and_geoclue_restarts() {
        // What adding GNOME prints (docs/nixos/desktops.md §5): new udev rules restart udevd, geoclue's whitelist
        // restarts geoclue. Each one alone switches; udevd is proved in nix/tests/desktops-install.nix (its mutant).
        for u in ["systemd-udevd.service", "geoclue.service"] {
            let mut c = Case::shell_only();
            c.dry = DryActivate::Ran(parse_dry_activate(&format!("would restart the following units: {u}\n")));
            assert_eq!(c.run().kind, Kind::Switch, "{u}: {:?}", c.run().reasons);
        }
    }

    #[test]
    fn rule_3_lets_a_removed_or_added_desktops_own_units_stop() {
        // Removing GNOME: units only the running generation has are stopped; that is the removal itself.
        let mut c = Case::shell_only();
        c.dry = DryActivate::Ran(parse_dry_activate(
            "would stop the following units: accounts-daemon.service, gnome-remote-desktop.service, user@1000.service\n\
             would stop the following user units: gnome-session-manager@gnome.service\n",
        ));
        let a = c.run();
        assert!(a.kind == Kind::Boot && has(&a, "accounts-daemon.service"), "{:?}", a.reasons);
        c.one_sided = ["accounts-daemon.service", "gnome-remote-desktop.service", "gnome-session-manager@.service"]
            .map(String::from)
            .into();
        let a = c.run();
        // user@1000.service is in both generations: still judged, still boot.
        assert!(a.kind == Kind::Boot && has(&a, "user@1000.service") && !has(&a, "accounts-daemon"), "{:?}", a.reasons);
        assert!(!has(&a, "gnome-session-manager"), "a template in one generation covers its instances: {:?}", a.reasons);
        c.dry = DryActivate::Ran(parse_dry_activate(
            "would stop the following units: accounts-daemon.service, gnome-remote-desktop.service\n",
        ));
        assert_eq!(c.run().kind, Kind::Switch, "{:?}", c.run().reasons);
        // A unit both generations have is still refused: one_sided is not an allowlist for everything.
        c.dry = DryActivate::Ran(parse_dry_activate("would restart the following units: NetworkManager.service\n"));
        assert_eq!(c.run().kind, Kind::Boot);
    }

    #[test]
    fn rule_6_asks_only_sessions_that_run_the_shell() {
        // A locked GNOME session beside an unlocked Rime one: the Shell swap goes ahead.
        let mut c = Case::shell_only();
        c.machine.sessions.push(SessionState {
            user: "sam".into(),
            uid: 1001,
            session_id: "3".into(),
            desktop: "gnome".into(),
            locked: Some(true),
        });
        assert_eq!(c.run().kind, Kind::Switch, "{:?}", c.run().reasons);
        // The same locked session with an unknown desktop still waits: an unknown session may run the Shell.
        c.machine.sessions[1].desktop = String::new();
        assert!(has(&c.run(), "rule 6"), "{:?}", c.run().reasons);
        // And a locked Rime session still waits whatever else is logged in.
        c.machine.sessions[1].desktop = "gnome".into();
        c.machine.sessions[0].locked = Some(true);
        assert!(has(&c.run(), "rule 6: "), "{:?}", c.run().reasons);
    }

    #[test]
    fn hyprland_config_with_compositor_stays_boot_but_shell_with_runtime_switches() {
        let mut c = Case::shell_only();
        c.target = closure(&["nixos-system-b", "rime-shell-1", "glibc-2.42", "linux-6.12.41", "bash-5.3", "hyprland-0.58.0", "rime-hypr-2"]);
        assert!(has(&c.run(), "together with the compositor"));
        // (Shell, ShellRuntime) is dropped on NixOS: a restarted Shell runs the new Quickshell.
        let mut c = Case::shell_only();
        c.current = Some(closure(&[A, &["quickshell-0.3.1"]].concat()));
        c.booted = c.current.clone();
        c.target = closure(&[B_SHELL, &["quickshell-0.3.2"]].concat());
        assert_eq!(c.run().kind, Kind::Switch, "{:?}", c.run().reasons);
    }

    #[test]
    fn an_unmeasured_running_generation_is_boot() {
        let mut c = Case::shell_only();
        c.current = None;
        assert_eq!(c.run().kind, Kind::Boot);
    }

    #[test]
    fn the_parser_reads_the_pinned_switch_to_configuration_output() {
        let d = parse_dry_activate(
            "Not checking switch inhibitors (action = dry-activate)\n\
             would stop the following units: a.service\nwould NOT stop the following changed units: greetd.service\n\
             would activate the configuration...\nwould restart systemd\nwould reload the following units: b.service\n\
             would restart the following units: c.service, d.service\nwould start the following units: e.service\n\
             would restart the following user units: f.service\n",
        );
        assert!(d.restart_systemd);
        assert_eq!(d.disruptive, ["a.service", "c.service", "d.service", "f.service"].iter().map(|s| s.to_string()).collect());
        assert_eq!(d.reload, ["b.service".to_string()].into());
        assert_eq!(d.skipped, ["greetd.service".to_string()].into());
        assert_eq!(d.start, ["e.service".to_string()].into());
        assert!(d.unrecognised.is_empty());
    }
}
