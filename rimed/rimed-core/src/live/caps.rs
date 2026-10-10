//! What this machine's kernel, driver, compositor and firmware allow beyond
//! file-level live activation, decided from facts the CLI reads.
//!
//! Each detector answers one question with a verdict and the evidence for it,
//! and "could not read" is its own verdict. None of them acts: livepatching,
//! a driver reload, a soft reboot or a kexec handover is never started by
//! `rime update`. They exist so `rime live doctor` and the plan can say
//! exactly which faster path exists here and which does not.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "kebab-case")]
pub enum Verdict {
    Available { evidence: String },
    Unavailable { why: String },
    Unknown { why: String },
}

impl Verdict {
    pub fn is_available(&self) -> bool {
        matches!(self, Verdict::Available { .. })
    }
    pub fn sentence(&self) -> String {
        match self {
            Verdict::Available { evidence } => format!("yes: {evidence}"),
            Verdict::Unavailable { why } => format!("no: {why}"),
            Verdict::Unknown { why } => format!("unknown: {why}"),
        }
    }
}

/// `CONFIG_X=y` from a kernel config, `None` when the option is absent.
pub fn kconfig(config: &str, option: &str) -> Option<String> {
    let set = format!("{option}=");
    let unset = format!("# {option} is not set");
    for l in config.lines() {
        if let Some(v) = l.strip_prefix(&set) {
            return Some(v.trim().to_string());
        }
        if l.trim() == unset {
            return Some("n".into());
        }
    }
    None
}

/// Kernel livepatching: the kernel must be built for it, and a patch must be
/// signed by a key the kernel trusts, because a livepatch is a module.
pub fn livepatch(config: Option<&str>, sysfs_present: bool, sig_enforce: Option<bool>) -> Verdict {
    let Some(cfg) = config else {
        return if sysfs_present {
            Verdict::Available { evidence: "/sys/kernel/livepatch exists".into() }
        } else {
            Verdict::Unknown { why: "the running kernel's config could not be read".into() }
        };
    };
    match kconfig(cfg, "CONFIG_LIVEPATCH").as_deref() {
        Some("y") if sysfs_present => Verdict::Available {
            evidence: format!(
                "CONFIG_LIVEPATCH=y, /sys/kernel/livepatch present{}",
                match sig_enforce {
                    Some(true) => "; patches must be signed with the machine's module key",
                    Some(false) => "; WARNING: module signatures are not enforced",
                    None => "",
                }
            ),
        },
        Some("y") => Verdict::Unknown { why: "CONFIG_LIVEPATCH=y but /sys/kernel/livepatch is missing".into() },
        _ => Verdict::Unavailable {
            why: if kconfig(cfg, "CONFIG_HAVE_LIVEPATCH").as_deref() == Some("y") {
                "the kernel supports livepatching but is built without CONFIG_LIVEPATCH".into()
            } else {
                "the kernel is built without livepatch support".into()
            },
        },
    }
}

/// Live Update Orchestrator / Kexec HandOver: lets a kexec carry state (memfds,
/// devices) into the next kernel. Needs the kernel options AND `kho=on` plus
/// the orchestrator device at runtime.
pub fn luo_kho(config: Option<&str>, cmdline: &str, dev_liveupdate: bool) -> Verdict {
    let Some(cfg) = config else {
        return Verdict::Unknown { why: "the running kernel's config could not be read".into() };
    };
    let kho = kconfig(cfg, "CONFIG_KEXEC_HANDOVER").as_deref() == Some("y");
    let luo = kconfig(cfg, "CONFIG_LIVEUPDATE").as_deref() == Some("y");
    if !kho || !luo {
        return Verdict::Unavailable { why: "the kernel is built without KEXEC_HANDOVER/LIVEUPDATE".into() };
    }
    let on = cmdline.split_whitespace().any(|a| a == "kho=on" || a == "kho=1" || a == "kho");
    match (on, dev_liveupdate) {
        (true, true) => Verdict::Available {
            evidence: "kho=on and /dev/liveupdate present (experimental; never used by rime update)".into(),
        },
        (true, false) => Verdict::Unavailable { why: "kho=on but /dev/liveupdate is missing".into() },
        (false, _) => Verdict::Unavailable {
            why: "built in, but not enabled at boot (kho=on is not on the command line)".into(),
        },
    }
}

/// How module loading is policed at runtime. Reported because livepatching
/// and a driver reload both load modules.
pub fn module_signing(sig_enforce: Option<bool>, lockdown: Option<&str>, secure_boot: Option<bool>) -> Verdict {
    let mode = lockdown.and_then(|l| {
        l.split_whitespace().find(|w| w.starts_with('[')).map(|w| w.trim_matches(|c| c == '[' || c == ']').to_string())
    });
    match sig_enforce {
        Some(true) => Verdict::Available {
            evidence: format!(
                "unsigned modules are refused (sig_enforce=Y; lockdown {}; Secure Boot {})",
                mode.as_deref().unwrap_or("unknown"),
                match secure_boot {
                    Some(true) => "on",
                    Some(false) => "off",
                    None => "unknown",
                }
            ),
        },
        Some(false) => Verdict::Unavailable {
            why: format!(
                "unsigned modules load with a taint (sig_enforce=N, lockdown {})",
                mode.as_deref().unwrap_or("unknown")
            ),
        },
        None => Verdict::Unknown { why: "/sys/module/module/parameters/sig_enforce is unreadable".into() },
    }
}

/// Facts about the NVIDIA kernel driver on this machine.
#[derive(Debug, Clone, Default)]
pub struct NvidiaFacts {
    pub gpu_present: bool,
    pub module_loaded: bool,
    /// `/sys/module/nvidia/refcnt`.
    pub refcnt: Option<u32>,
    /// Modules holding it (nvidia_drm, nvidia_modeset, nvidia_uvm).
    pub holders: Vec<String>,
    /// Processes with a `/dev/nvidia*` or the card's DRM node open.
    pub openers: Option<usize>,
    /// nvidia-drm has modeset=1 (it then drives displays/console).
    pub modeset: bool,
    /// The user opted in (`[live] nvidia_reload = true`).
    pub opted_in: bool,
}

/// Can the NVIDIA driver be swapped without a restart? Only when nothing uses
/// it: no process holds the device, no display runs on it. Rime never kills a
/// process to get there, so on a machine whose desktop runs on the card the
/// answer is no.
pub fn nvidia_reload(f: &NvidiaFacts) -> Verdict {
    if !f.gpu_present {
        return Verdict::Unavailable { why: "no NVIDIA GPU".into() };
    }
    if !f.module_loaded {
        return Verdict::Available { evidence: "the driver is not loaded; the new one loads on first use".into() };
    }
    let Some(openers) = f.openers else {
        return Verdict::Unknown { why: "could not tell which processes use the GPU".into() };
    };
    if openers > 0 {
        return Verdict::Unavailable { why: format!("{openers} process(es) are using the GPU; Rime does not close them") };
    }
    if f.modeset && f.holders.iter().any(|h| h == "nvidia_drm") && f.refcnt.unwrap_or(1) > 1 {
        return Verdict::Unavailable { why: "a display or console runs on the GPU (nvidia-drm modeset)".into() };
    }
    if !f.opted_in {
        return Verdict::Unavailable {
            why: "idle and eligible, but driver reloads are opt-in (`[live] nvidia_reload = true`)".into(),
        };
    }
    Verdict::Available { evidence: "no process uses the GPU and reloads are enabled".into() }
}

/// Can the compositor be replaced under a running session? Hyprland, niri and
/// labwc have no handover protocol: replacing one ends the session.
pub fn compositor_handover(compositor: &str) -> Verdict {
    match compositor {
        "" => Verdict::Unknown { why: "no graphical session".into() },
        c => Verdict::Unavailable {
            why: format!("{c} has no session handover; a new compositor takes effect at the next login"),
        },
    }
}

/// From `bootupctl status --json`: does the staged image carry a newer
/// bootloader than the one installed? A bootloader change never activates
/// live; it is installed by bootupd and takes effect at the next boot.
pub fn bootloader_update(status_json: &str) -> Result<Option<String>, String> {
    let v: serde_json::Value = serde_json::from_str(status_json).map_err(|e| format!("bootupctl status: {e}"))?;
    let comps = v.get("components").and_then(|c| c.as_object()).ok_or("bootupctl status has no components")?;
    let mut out = Vec::new();
    for (name, c) in comps {
        let inst = c.pointer("/installed/version").and_then(|x| x.as_str()).unwrap_or("");
        let upd = c.pointer("/update/version").and_then(|x| x.as_str()).unwrap_or("");
        if !upd.is_empty() && upd != inst {
            out.push(format!("{name}: {inst} → {upd}"));
        }
    }
    Ok(if out.is_empty() { None } else { Some(out.join(", ")) })
}

/// From `fwupdmgr get-devices --json`: devices with firmware installed and
/// waiting on a restart to apply. `needs-reboot` in `Flags` is a capability
/// ("an update to this device would need a reboot"), not a pending update;
/// `UpdateState` is the state: 1 pending, 4 needs-reboot.
pub fn firmware_needs_restart(devices_json: &str) -> Result<Vec<String>, String> {
    let v: serde_json::Value = serde_json::from_str(devices_json).map_err(|e| format!("fwupd: {e}"))?;
    let devs = v.get("Devices").and_then(|d| d.as_array()).ok_or("fwupd: no Devices")?;
    let mut out = Vec::new();
    for d in devs {
        if matches!(d.get("UpdateState").and_then(|u| u.as_u64()), Some(1) | Some(4)) {
            out.push(d.get("Name").and_then(|n| n.as_str()).unwrap_or("device").to_string());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RIME_729: &str = "CONFIG_HAVE_LIVEPATCH=y\n# CONFIG_LIVEPATCH is not set\nCONFIG_KEXEC_HANDOVER=y\nCONFIG_LIVEUPDATE=y\n";

    #[test]
    fn rime_kernel_has_no_livepatch() {
        let v = livepatch(Some(RIME_729), false, Some(true));
        assert!(matches!(v, Verdict::Unavailable { ref why } if why.contains("without CONFIG_LIVEPATCH")), "{v:?}");
        let on = "CONFIG_LIVEPATCH=y\n";
        assert!(livepatch(Some(on), true, Some(true)).is_available());
        assert!(matches!(livepatch(Some(on), false, None), Verdict::Unknown { .. }));
        assert!(matches!(livepatch(None, false, None), Verdict::Unknown { .. }));
    }

    #[test]
    fn luo_needs_the_cmdline() {
        assert!(matches!(luo_kho(Some(RIME_729), "quiet splash", false), Verdict::Unavailable { ref why } if why.contains("not enabled")));
        assert!(luo_kho(Some(RIME_729), "quiet kho=on", true).is_available());
        assert!(!luo_kho(Some("CONFIG_KEXEC_HANDOVER=y\n"), "kho=on", true).is_available());
    }

    #[test]
    fn module_signing_reads_runtime_policy() {
        let v = module_signing(Some(true), Some("[none] integrity confidentiality"), Some(true));
        assert!(matches!(v, Verdict::Available { ref evidence } if evidence.contains("lockdown none")));
        assert!(!module_signing(Some(false), Some("none [integrity] confidentiality"), None).is_available());
        assert!(matches!(module_signing(None, None, None), Verdict::Unknown { .. }));
    }

    #[test]
    fn nvidia_reload_is_conservative() {
        let mut f = NvidiaFacts { gpu_present: true, module_loaded: true, refcnt: Some(3), holders: vec!["nvidia_drm".into(), "nvidia_modeset".into()], openers: Some(2), modeset: true, opted_in: true };
        assert!(matches!(nvidia_reload(&f), Verdict::Unavailable { ref why } if why.contains("process")));
        f.openers = Some(0);
        assert!(matches!(nvidia_reload(&f), Verdict::Unavailable { ref why } if why.contains("display")));
        f.refcnt = Some(0);
        f.holders.clear();
        assert!(nvidia_reload(&f).is_available());
        f.opted_in = false;
        assert!(!nvidia_reload(&f).is_available(), "opt-in is required");
        f.openers = None;
        assert!(matches!(nvidia_reload(&f), Verdict::Unknown { .. }));
        assert!(!nvidia_reload(&NvidiaFacts::default()).is_available());
    }

    #[test]
    fn compositors_have_no_handover() {
        assert!(!compositor_handover("hyprland").is_available());
    }

    #[test]
    fn bootloader_and_firmware() {
        let same = r#"{"components":{"EFI":{"installed":{"version":"grub2-1:2.12-81.fc45,shim-16.1-7"},"update":{"version":"grub2-1:2.12-81.fc45,shim-16.1-7"}}}}"#;
        assert_eq!(bootloader_update(same).unwrap(), None);
        let newer = same.replacen("81.fc45,shim-16.1-7\"}}}}", "82.fc45,shim-16.1-7\"}}}}", 1);
        assert!(bootloader_update(&newer).unwrap().unwrap().contains("EFI"));
        assert!(bootloader_update("{").is_err());
        let fw = r#"{"Devices":[{"Name":"System Firmware","Flags":["internal","needs-reboot"],"UpdateState":4},{"Name":"TPM","Flags":["needs-reboot"],"UpdateState":2},{"Name":"Dock","Flags":["needs-reboot"]}]}"#;
        assert_eq!(firmware_needs_restart(fw).unwrap(), vec!["System Firmware"]);
    }
}
