//! `org.rimeos.Rimed1.Switch` — runtime switches (nixos-pivot PLAN.md D2), additive like Mode.
//!
//! bootc enabled a few units at run time with `systemctl enable` (a disposable
//! guest's `rime-guest-session@<uid>.service`, `rime-firewall.service`). A
//! NixOS `/etc/systemd/system` is the system's, so each such unit carries a
//! condition on a file under `/var/lib/rime/switch/` instead
//! (nix/modules/hostlayer.nix):
//!
//! * a unit that is OFF unless switched on: `ConditionPathExists=<dir>/%n.on`
//! * a unit that is ON unless switched off: `ConditionPathExists=!<dir>/%n.off`
//!
//! The registry `/etc/rime/hostlayer/switches` (one `<unit> <on|off>` per
//! line; a template ends in `@.service`) says which units are switches and
//! their state with no marker. Anything not in it is refused, so this method
//! cannot create a file for any other unit. `rime-hostlayer switch` (root) does
//! the same with the same files. On a bootc image there is no registry and
//! every name is refused.

use std::io;
use std::path::{Path, PathBuf};

use zbus::message::Header;
use zbus::{interface, Connection};

use crate::polkit::authorize;

/// Polkit action for changing a runtime switch (rime-hostlayer ships it).
pub const ACTION_SWITCHES: &str = "org.rimeos.rimed.manage-switches";
const REGISTRY: &str = "/etc/rime/hostlayer/switches";
const DIR: &str = "/var/lib/rime/switch";

const SUFFIXES: [&str; 5] = [".service", ".socket", ".timer", ".path", ".mount"];

fn plain(s: &str) -> bool {
    !s.is_empty()
        && s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

/// A unit name a marker file may be named after: `[A-Za-z0-9_.:-]+`, at most
/// one `@` instance of the same characters, a unit suffix. Never a `/`.
pub fn valid_unit(name: &str) -> bool {
    let Some(stem) = SUFFIXES.iter().find_map(|s| name.strip_suffix(s)) else {
        return false;
    };
    match stem.split_once('@') {
        None => plain(stem),
        Some((prefix, inst)) => plain(prefix) && (inst.is_empty() || plain(inst)),
    }
}

/// `(unit, default_on)` for every well-formed registry line.
pub fn parse_registry(text: &str) -> Vec<(String, bool)> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let (unit, state) = (it.next()?, it.next()?);
            if it.next().is_some() || !valid_unit(unit) || unit.starts_with('#') {
                return None;
            }
            match state {
                "on" => Some((unit.to_string(), true)),
                "off" => Some((unit.to_string(), false)),
                _ => None,
            }
        })
        .collect()
}

/// The default state of `name` (an instance resolves through its template),
/// or why it is not a switch.
pub fn resolve(reg: &[(String, bool)], name: &str) -> Result<bool, String> {
    if !valid_unit(name) {
        return Err(format!("{name:?} is not a unit name"));
    }
    if let Some((_, d)) = reg.iter().find(|(u, _)| u == name) {
        return Ok(*d);
    }
    if let Some((prefix, rest)) = name.split_once('@') {
        if let Some((inst, suffix)) = rest.rsplit_once('.') {
            if !inst.is_empty() {
                let tmpl = format!("{prefix}@.{suffix}");
                if let Some((_, d)) = reg.iter().find(|(u, _)| *u == tmpl) {
                    return Ok(*d);
                }
            }
        }
    }
    Err(format!("{name} is not a runtime switch on this system"))
}

fn marker(dir: &Path, name: &str, default_on: bool) -> PathBuf {
    dir.join(format!("{name}.{}", if default_on { "off" } else { "on" }))
}

/// Whether the switch is on now.
pub fn state(dir: &Path, name: &str, default_on: bool) -> bool {
    marker(dir, name, default_on).exists() != default_on
}

/// Turn the switch on or off: create or remove its marker.
pub fn set(dir: &Path, name: &str, default_on: bool, on: bool) -> io::Result<()> {
    let m = marker(dir, name, default_on);
    if on != default_on {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".{name}.rimed.tmp"));
        std::fs::write(&tmp, b"rimed\n")?;
        std::fs::rename(&tmp, &m)
    } else {
        match std::fs::remove_file(&m) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            r => r,
        }
    }
}

pub struct SwitchIface {
    registry: PathBuf,
    dir: PathBuf,
}

impl SwitchIface {
    pub fn system() -> Self {
        Self { registry: REGISTRY.into(), dir: DIR.into() }
    }

    fn registry(&self) -> Vec<(String, bool)> {
        std::fs::read_to_string(&self.registry)
            .map(|t| parse_registry(&t))
            .unwrap_or_default()
    }

    fn default_of(&self, name: &str) -> zbus::fdo::Result<bool> {
        resolve(&self.registry(), name).map_err(zbus::fdo::Error::InvalidArgs)
    }
}

#[interface(name = "org.rimeos.Rimed1.Switch")]
impl SwitchIface {
    /// Every switch: (unit or template, state with no marker).
    async fn list(&self) -> Vec<(String, bool)> {
        self.registry()
    }

    /// Whether `name` is on. No authorization: the markers are world-readable.
    async fn get(&self, name: String) -> zbus::fdo::Result<bool> {
        let d = self.default_of(&name)?;
        Ok(state(&self.dir, &name, d))
    }

    /// Turn `name` on or off from the next start of the unit (no start or stop
    /// now, like `systemctl enable`). polkit `manage-switches`.
    async fn set(
        &self,
        name: String,
        on: bool,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, ACTION_SWITCHES).await?;
        let d = self.default_of(&name)?;
        set(&self.dir, &name, d, on)
            .map_err(|e| zbus::fdo::Error::Failed(format!("{}: {e}", self.dir.display())))?;
        eprintln!("rimed: switch {name} {}", if on { "on" } else { "off" });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REG: &str = "# switches\nrime-guest-session@.service off\nrime-firewall.service on\nbad line here\n\
                       x/y.service on\n";

    #[test]
    fn only_registered_units_are_switches() {
        let reg = parse_registry(REG);
        assert_eq!(reg.len(), 2);
        assert_eq!(resolve(&reg, "rime-guest-session@1001.service"), Ok(false));
        assert_eq!(resolve(&reg, "rime-firewall.service"), Ok(true));
        for bad in [
            "sshd.service",
            "rime-guest-session@.service.on",
            "../etc/shadow",
            "rime-guest-session@../x.service",
            "rime-guest-session@a/b.service",
            "rime-guest-session@1@2.service",
            "",
        ] {
            assert!(resolve(&reg, bad).is_err(), "{bad} resolved");
        }
    }

    #[test]
    fn markers_follow_the_default() {
        let d = std::env::temp_dir().join(format!("rimed-switch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let g = "rime-guest-session@1001.service";
        assert!(!state(&d, g, false));
        set(&d, g, false, true).unwrap();
        assert!(d.join(format!("{g}.on")).exists());
        assert!(state(&d, g, false));
        set(&d, g, false, false).unwrap();
        assert!(!d.join(format!("{g}.on")).exists());
        let f = "rime-firewall.service";
        assert!(state(&d, f, true));
        set(&d, f, true, false).unwrap();
        assert!(d.join(format!("{f}.off")).exists());
        assert!(!state(&d, f, true));
        set(&d, f, true, true).unwrap();
        assert!(state(&d, f, true));
        let _ = std::fs::remove_dir_all(&d);
    }
}
