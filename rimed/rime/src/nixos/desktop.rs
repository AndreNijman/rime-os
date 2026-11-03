//! `rime install <desktop>` and `rime remove <desktop>` (docs/nixos/desktops.md §5): one other desktop beside Rime's
//! own sessions, installed from the release this machine already runs.
//!
//! Install: root; a second desktop is refused by name; the disk must hold the floor of exactly the system the switches
//! then select (nix/release/floors.nix); the switch file is written; then the running release installs the system
//! with it (update::install_running_release: the channel is not read, so nothing newer comes with it). A release
//! without that system keeps the switch and says `sudo rime update` brings it later.
//!
//! Remove: the switch file goes, the running release installs the system without it. While a session of that desktop
//! is open the change waits for the next restart (stopping its services would end the session). User files stay.

use std::path::Path;

use rimed_core::live::SessionState;

use super::pkg::{edit_list, read_list, write_list, Catalogue, Feature};
use super::update::{install_running_release, SameRelease};
use super::variant;

fn say(msg: impl AsRef<str>) {
    eprintln!("rime: {}", msg.as_ref());
}

/// The refusal for a second desktop: another desktop's switch is on. `on`: the features switched on now.
pub fn second_desktop(cat: &Catalogue, feature: &str, on: &[String]) -> Option<String> {
    let other = on.iter().find(|f| f.as_str() != feature && cat.is_desktop(f))?;
    let title = cat.features.get(other).map_or(other.as_str(), |f| f.title.as_str());
    Some(format!(
        "{title} is installed; Rime adds one other desktop at a time: `sudo rime remove {other}` first"
    ))
}

/// The floor of exactly the system `feature` plus the switches already on select, against the filesystem holding
/// /nix (`fs_bytes`). `None`: it fits, or no floor is recorded for that system (the release has none to install).
pub fn floor_refusal(cat: &Catalogue, base: &str, on: &[String], feature: &str, fs_bytes: u64) -> Option<String> {
    let title = cat.features.get(feature).map_or(feature, |f| f.title.as_str());
    let mut want: Vec<String> = on.to_vec();
    want.push(feature.to_string());
    want.sort_unstable();
    want.dedup();
    let floor = cat.floors.get(&variant::name(base, &want))?;
    variant::floor_refusal(title, floor, fs_bytes)
}

/// An open session of this desktop (logind `Desktop` is one of its session ids).
pub fn open_session<'a>(f: &Feature, sessions: &'a [SessionState]) -> Option<&'a SessionState> {
    sessions.iter().find(|s| f.session_ids.iter().any(|id| id.eq_ignore_ascii_case(&s.desktop)))
}

fn base() -> String {
    super::Config::load().map(|c| c.variant).unwrap_or_else(|_| "plain".to_string())
}

pub fn install(cat: &Catalogue, name: &str, feature: &str) -> Result<(), String> {
    if crate::ops::effective_uid() != Some(0) {
        return Err(format!("installing a desktop changes the system: `sudo rime install {name}`"));
    }
    let f = cat.features.get(feature).ok_or_else(|| format!("the catalogue describes no feature '{feature}'"))?;
    let on = variant::switched_on(Some(cat));
    if let Some(why) = second_desktop(cat, feature, &on) {
        return Err(why);
    }
    if let Some(size) = variant::fs_size("/nix/store") {
        if let Some(why) = floor_refusal(cat, &base(), &on, feature, size) {
            return Err(why);
        }
    }
    let path = Path::new(&f.switch);
    write_list(path, &edit_list(&read_list(path), &[name.to_string()], &[]), false)?;
    say(format!("{} is switched on for this machine; installing it from the release this machine runs", f.title));
    match install_running_release(false)? {
        SameRelease::Live => say(format!(
            "{} is installed. Log out and choose {} at the login screen.",
            f.title, f.title
        )),
        SameRelease::NextBoot => say(format!(
            "{} is installed for the next restart. Restart, then choose {} at the login screen.",
            f.title, f.title
        )),
        SameRelease::NotInRelease => say(format!(
            "selected for this machine ({}): `sudo rime update` installs the system that carries it as soon as \
             Rime's release offers one for this machine",
            path.display()
        )),
        SameRelease::NewerStaged(r) => say(format!(
            "release {r} is installed for the next restart; restart into it, then `sudo rime update` installs {} \
             (it stays selected for this machine)",
            f.title
        )),
    }
    Ok(())
}

pub fn remove(cat: &Catalogue, name: &str, feature: &str) -> Result<(), String> {
    if crate::ops::effective_uid() != Some(0) {
        return Err(format!("removing a desktop changes the system: `sudo rime remove {name}`"));
    }
    let f = cat.features.get(feature).ok_or_else(|| format!("the catalogue describes no feature '{feature}'"))?;
    let path = Path::new(&f.switch);
    // Every name for this desktop goes: the desktop is one switch, whichever alias put it there.
    write_list(path, &[], true)?;
    let sessions = crate::live::sessions().unwrap_or_default();
    let open = open_session(f, &sessions);
    if let Some(s) = open {
        say(format!(
            "{}'s {} session is open, so {} is removed at the next restart (its services keep the session running)",
            s.user, f.title, f.title
        ));
    }
    match install_running_release(open.is_some())? {
        SameRelease::Live => say(format!("{} is removed.", f.title)),
        SameRelease::NextBoot => say(format!("{} is removed from the next restart.", f.title)),
        SameRelease::NotInRelease => say(format!("{} is switched off for this machine.", f.title)),
        SameRelease::NewerStaged(r) => say(format!(
            "{} is switched off; release {r}, installed for the next restart, is installed without it by the next \
             `sudo rime update` after that restart",
            f.title
        )),
    }
    say("your files, and the settings this desktop kept in your home folder, are kept");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat() -> Catalogue {
        Catalogue::parse(
            r#"{
              "schema": 1, "flathub_remote": "flathub", "switch_dir": "/var/lib/rime/switch",
              "apps_profile": "/nix/var/nix/profiles/rime-apps", "outs_file": "/var/lib/rime/nix/catalogue-outs.json",
              "features": {
                "gaming": {"title": "Gaming", "offered": true, "in_system": false, "unit": "rime-feature@gaming.target",
                           "switch": "/x/rime-feature@gaming.target.on"},
                "gnome": {"title": "GNOME", "offered": true, "in_system": false, "unit": "rime-feature@gnome.target",
                          "switch": "/x/rime-feature@gnome.target.on", "group": "desktop", "session_ids": ["gnome"]},
                "kde": {"title": "KDE Plasma", "offered": true, "in_system": false, "unit": "rime-feature@kde.target",
                        "switch": "/x/rime-feature@kde.target.on", "group": "desktop", "session_ids": ["plasma"]}
              },
              "entries": {},
              "aliases": {},
              "refuse": [],
              "floors": {
                "plain-gnome": {"disk_gb": 17, "store_bytes": 14400000000},
                "plain-gaming-gnome": {"disk_gb": 19, "store_bytes": 16700000000},
                "plain-gaming": {"disk_gb": 18, "store_bytes": 15000000000}
              }
            }"#,
        )
        .expect("parses")
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn a_second_desktop_is_refused_by_name() {
        let c = cat();
        assert_eq!(
            second_desktop(&c, "kde", &s(&["gaming", "gnome"])).as_deref(),
            Some("GNOME is installed; Rime adds one other desktop at a time: `sudo rime remove gnome` first")
        );
        // The same desktop again, or beside a feature that is not a desktop: no refusal.
        assert_eq!(second_desktop(&c, "gnome", &s(&["gnome", "gaming"])), None);
        assert_eq!(second_desktop(&c, "kde", &s(&["gaming"])), None);
        assert!(c.is_desktop("gnome") && !c.is_desktop("gaming"));
        assert_eq!(c.other_desktop_sessions(), ["gnome", "plasma"].map(String::from).into());
    }

    #[test]
    fn the_floor_is_the_floor_of_exactly_the_system_selected() {
        let c = cat();
        // gaming on + GNOME: plain-gaming-gnome's floor (16.7 GB), not plain-gnome's (14.4 GB).
        let why = floor_refusal(&c, "plain", &s(&["gaming"]), "gnome", 15_000_000_000).expect("refused");
        assert!(why.starts_with("Rime's GNOME system needs a disk of at least 19 GB"), "{why}");
        assert_eq!(floor_refusal(&c, "plain", &s(&["gaming"]), "gnome", 16_700_000_000), None);
        assert_eq!(floor_refusal(&c, "plain", &[], "gnome", 15_000_000_000), None);
        assert!(floor_refusal(&c, "plain", &[], "gnome", 14_399_999_999).is_some());
        // No floor recorded for that system (the release builds none): nothing to refuse here.
        assert_eq!(floor_refusal(&c, "plain", &[], "kde", 1), None);
    }

    #[test]
    fn an_open_session_of_the_desktop_is_found_by_its_session_id() {
        let c = cat();
        let sess = |d: &str| SessionState { user: "alex".into(), uid: 1000, session_id: "2".into(), desktop: d.into(), locked: Some(false) };
        let list = vec![sess("hyprland"), sess("gnome")];
        assert_eq!(open_session(&c.features["gnome"], &list).map(|s| s.desktop.as_str()), Some("gnome"));
        assert!(open_session(&c.features["kde"], &list).is_none());
        assert!(open_session(&c.features["kde"], &[sess("plasma")]).is_some());
    }
}
