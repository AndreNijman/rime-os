//! `rime luks` — whether each LUKS volume's TPM slot still unlocks it, and the re-bind when it does not (NixOS).
//!
//! The judgement is `rime-luks-tpm-status` (nix/modules/luks-tpm.nix): it recomputes the PCR policy each
//! systemd-tpm2 token was sealed to from this boot's PCR values and compares digests, without asking the TPM to
//! unseal anything. It needs root (the LUKS header), so it runs once per boot as rime-luks-tpm-status.service and
//! writes /run/rime/luks-tpm.json; `rime doctor` reads that file unprivileged, `rime luks status` runs it live
//! when it can.
//!
//! A slot goes stale when PCR 7 changes: a new Secure Boot key, a dbx update, firmware that measures differently.
//! The volume then asks for its passphrase at every boot. `rime luks rebind` starts
//! rime-luks-tpm-reenrol@<uuid>.service for every stale volume (or the ones named): it asks once for the
//! passphrase or recovery key and re-seals the TPM slot to PCR 7, keeping every other keyslot.

use std::path::PathBuf;
use std::process::Command;

use clap::Subcommand;
use serde_json::Value;

const STATUS_FILE: &str = "/run/rime/luks-tpm.json";
const STATUS_BIN: &str = "/run/current-system/sw/bin/rime-luks-tpm-status";
const SYSTEMCTL: &str = "/run/current-system/sw/bin/systemctl";

#[derive(Subcommand, Debug)]
pub enum LuksCmd {
    /// Whether each LUKS volume's TPM slot still matches this boot's PCR values (so it unlocks without asking).
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Re-bind the TPM slot of every volume whose slot no longer matches PCR 7, or of the LUKS UUIDs given.
    /// Asks once per volume for its passphrase or recovery key; keeps every other keyslot. Needs root.
    Rebind {
        /// LUKS UUIDs (`cryptsetup luksUUID /dev/<part>`). None: every stale volume.
        uuids: Vec<String>,
    },
}

fn status_file() -> PathBuf {
    std::env::var_os("RIME_LUKS_TPM_STATUS").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(STATUS_FILE))
}

fn status_bin() -> PathBuf {
    std::env::var_os("RIME_LUKS_TPM_STATUS_BIN").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(STATUS_BIN))
}

fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// A LUKS UUID, and nothing that could change the unit name it is spliced into.
pub fn valid_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() }
        })
}

/// The report the boot unit wrote. `Ok(None)` = no report (the unit is not installed or has not run).
fn read_report() -> Result<Option<Value>, String> {
    let p = status_file();
    match std::fs::read_to_string(&p) {
        Ok(t) => serde_json::from_str(&t).map(Some).map_err(|e| format!("{}: {e}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", p.display())),
    }
}

/// A fresh measurement (root only); it also refreshes the file `rime doctor` reads.
fn measure() -> Result<Value, String> {
    let bin = status_bin();
    let out = Command::new(&bin)
        .arg("--write")
        .arg(status_file())
        .output()
        .map_err(|e| format!("cannot run {}: {e}", bin.display()))?;
    if !out.status.success() {
        return Err(format!("{} failed: {}", bin.display(), String::from_utf8_lossy(&out.stderr).trim()));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("{}: unreadable report: {e}", bin.display()))
}

fn volumes(report: &Value) -> Vec<&Value> {
    report["volumes"].as_array().map(|a| a.iter().collect()).unwrap_or_default()
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("?")
}

/// The doctor's lines for one report: a stale slot is a WARN naming the volume and the command; a volume with no
/// TPM slot gets no line (a passphrase-only volume is a choice, not a fault); unchecked passes with its reason.
pub fn lines_from(report: &Value) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    for v in volumes(report) {
        let (uuid, dev, why) = (s(v, "uuid"), s(v, "device"), s(v, "why"));
        match s(v, "state") {
            "ok" => out.push((true, format!("LUKS {uuid} ({dev}): {why}"))),
            "stale" => out.push((
                false,
                format!(
                    "LUKS {uuid} ({dev}): {why} — run `sudo rime luks rebind` \
                     (rime-luks-tpm-reenrol@{uuid}.service: asks once for the passphrase or recovery key)"
                ),
            )),
            "unchecked" => out.push((true, format!("LUKS {uuid} ({dev}): TPM slot not checked: {why}"))),
            _ => {}
        }
    }
    out
}

/// What `rime doctor` adds: nothing on a machine without the NixOS unit, the report's lines otherwise.
pub fn doctor_lines() -> Vec<(bool, String)> {
    let fixture = std::env::var_os("RIME_LUKS_TPM_STATUS").is_some();
    if !fixture && !crate::nixos::is_nixos() {
        return Vec::new();
    }
    match read_report() {
        Ok(Some(r)) => lines_from(&r),
        Ok(None) => Vec::new(),
        Err(e) => vec![(true, format!("LUKS TPM slots: not read ({e})"))],
    }
}

fn print_report(r: &Value, json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(r).unwrap_or_default());
        return;
    }
    let vols = volumes(r);
    if vols.is_empty() {
        println!("no LUKS volumes");
    }
    for v in vols {
        println!("{:<11} {} ({}): {}", s(v, "state"), s(v, "uuid"), s(v, "device"), s(v, "why"));
    }
}

fn status(json: bool) -> i32 {
    let r = if is_root() { measure().map(Some) } else { read_report() };
    match r {
        Ok(Some(r)) => {
            print_report(&r, json);
            if volumes(&r).iter().any(|v| s(v, "state") == "stale") { 1 } else { 0 }
        }
        Ok(None) => {
            eprintln!(
                "rime luks status: no report at {} (rime-luks-tpm-status.service has not run); \
                 `sudo rime luks status` measures now",
                status_file().display()
            );
            2
        }
        Err(e) => {
            eprintln!("rime luks status: {e}");
            2
        }
    }
}

fn rebind(uuids: Vec<String>) -> i32 {
    if let Some(bad) = uuids.iter().find(|u| !valid_uuid(u)) {
        eprintln!("rime luks rebind: {bad:?} is not a LUKS UUID (cryptsetup luksUUID /dev/<part>)");
        return 2;
    }
    if !is_root() {
        eprintln!("rime luks rebind: needs root (it re-seals a LUKS keyslot): sudo rime luks rebind");
        return 2;
    }
    let before = match measure() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("rime luks rebind: {e}");
            return 2;
        }
    };
    let known: Vec<String> = volumes(&before).iter().map(|v| s(v, "uuid").to_ascii_lowercase()).collect();
    let targets: Vec<String> = if uuids.is_empty() {
        volumes(&before).iter().filter(|v| s(v, "state") == "stale").map(|v| s(v, "uuid").to_string()).collect()
    } else {
        for u in &uuids {
            if !known.contains(&u.to_ascii_lowercase()) {
                eprintln!("rime luks rebind: no LUKS volume with UUID {u} on this machine");
                return 2;
            }
        }
        uuids
    };
    if targets.is_empty() {
        println!("nothing to re-bind: every LUKS volume's TPM slot matches this boot's PCR values");
        return 0;
    }
    let mut failed = 0;
    for u in &targets {
        let unit = format!("rime-luks-tpm-reenrol@{u}.service");
        println!("re-binding {u}: systemctl start {unit}");
        // Inherited stdio: systemctl's password agent answers the unit's systemd-ask-password on this terminal.
        match Command::new(SYSTEMCTL).args(["start", &unit]).status() {
            Ok(st) if st.success() => println!("{u}: re-bound"),
            Ok(st) => {
                failed += 1;
                eprintln!("{u}: {unit} failed ({st}); see journalctl -u {unit}");
            }
            Err(e) => {
                failed += 1;
                eprintln!("{u}: cannot run {SYSTEMCTL}: {e}");
            }
        }
    }
    match measure() {
        Ok(after) => {
            print_report(&after, false);
            let still: Vec<&str> = volumes(&after)
                .into_iter()
                .filter(|v| targets.iter().any(|t| t.eq_ignore_ascii_case(s(v, "uuid"))) && s(v, "state") != "ok")
                .map(|v| s(v, "uuid"))
                .collect();
            if failed == 0 && still.is_empty() { 0 } else { 1 }
        }
        Err(e) => {
            eprintln!("rime luks rebind: {e}");
            1
        }
    }
}

pub fn main(cmd: LuksCmd) -> i32 {
    match cmd {
        LuksCmd::Status { json } => status(json),
        LuksCmd::Rebind { uuids } => rebind(uuids),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn uuids_are_checked_before_they_reach_a_unit_name() {
        assert!(valid_uuid("5b1f0c2e-7d4a-4e8b-9c3f-a1b2c3d4e5f6"));
        assert!(!valid_uuid("5b1f0c2e-7d4a-4e8b-9c3f-a1b2c3d4e5f"));
        assert!(!valid_uuid("5b1f0c2e-7d4a-4e8b-9c3f-a1b2c3d4e5f6x"));
        assert!(!valid_uuid("5b1f0c2e-7d4a-4e8b-9c3f.a1b2c3d4e5f6"));
        assert!(!valid_uuid("../../../etc/passwd-aaaaaaaaaaaaaaaaaaaa"));
    }

    #[test]
    fn a_stale_slot_is_a_warning_that_names_the_volume_and_the_command() {
        let r = json!({"volumes": [
            {"uuid": "11111111-2222-3333-4444-555555555555", "device": "/dev/vdb", "state": "stale", "why": "w"},
            {"uuid": "66666666-2222-3333-4444-555555555555", "device": "/dev/vdc", "state": "ok", "why": "fine"},
            {"uuid": "77777777-2222-3333-4444-555555555555", "device": "/dev/vdd", "state": "no-tpm-slot", "why": "x"},
            {"uuid": "88888888-2222-3333-4444-555555555555", "device": "/dev/vde", "state": "unchecked", "why": "y"}
        ]});
        let l = lines_from(&r);
        assert_eq!(l.len(), 3, "{l:?}");
        assert!(!l[0].0);
        assert!(l[0].1.contains("11111111-2222-3333-4444-555555555555"));
        assert!(l[0].1.contains("sudo rime luks rebind"));
        assert!(l[0].1.contains("rime-luks-tpm-reenrol@11111111-2222-3333-4444-555555555555.service"));
        assert!(l[1].0 && l[2].0);
        assert!(l.iter().all(|(_, w)| !w.contains("77777777")), "{l:?}");
    }

    #[test]
    fn no_volumes_no_lines() {
        assert!(lines_from(&json!({"volumes": []})).is_empty());
        assert!(lines_from(&json!({})).is_empty());
    }
}
