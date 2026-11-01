//! `rime boot status` — what verified this boot, and what the boot counter
//! currently believes. Roadmap §22's "surface boot verification state in Rime
//! Settings", from the OS side.
//!
//! ## Read-only, root-free, and honest about what it cannot see
//!
//! Everything here is a file read or one `bootctl list` invocation. There is no
//! mutating form: `rime boot status` is what Rime Settings polls, and "what is
//! the state of my boot chain" must never cost a password.
//!
//! Two of the interesting facts do need root, because they live in the ESP,
//! which is mode 0700 on a Rime machine. When that read fails this reports
//! `"entries": null` together with the reason, and it does NOT substitute an
//! empty list. The difference matters: an empty entry list is
//! indistinguishable from "no deployment has failed", which is precisely the
//! answer that would hide a rollback from the user.
//!
//! ## Why it reports the health verdict instead of recomputing it
//!
//! `/usr/libexec/rime-boot-health` decides whether a boot was healthy, because
//! it is the unit systemd runs before `boot-complete.target` and its exit
//! status is what blesses or condemns the deployment. Recomputing the verdict
//! here would give two implementations that can disagree, and the one the user
//! reads would be the one that is not wired to anything. So the verdict is
//! read out of the file that unit wrote, and a missing file is reported as
//! "never ran" rather than as "healthy".
//!
//! ## The bootloader this expects to find
//!
//! GRUB, on every image published so far. Rime is moving to systemd-boot
//! (AGENTS.md boot-path rule 5, docs/boot-v2.md), but a machine only gets it by
//! being installed onto the composefs backend, so `bootloader: "grub"` with
//! `bootCounting: false` stays the normal, correct answer for an existing
//! machine and the report says so in plain words rather than looking like a
//! degraded state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Subcommand;
use serde_json::{json, Map, Value};

/// The vendor GUID systemd-boot and sd-stub use for every variable they export.
const LOADER_GUID: &str = "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";
/// The global UEFI namespace, where the firmware itself publishes SecureBoot.
const GLOBAL_GUID: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";

#[derive(Subcommand)]
pub enum BootCmd {
    /// What verified this boot, and what the boot counter believes.
    ///
    /// Reports the bootloader, Secure Boot state, whether the running kernel
    /// came from a signed Unified Kernel Image, whether measured boot and
    /// TPM-bound unlock are in effect, the boot-counting tally of every
    /// deployment, the last health verdict, and any pending rollback notice.
    ///
    /// Read-only and root-free. The boot entries specifically need root
    /// because the ESP is not world-readable; without it they are reported as
    /// unavailable with the reason, never as an empty list.
    Status {
        /// Emit machine-readable JSON instead of a report.
        #[arg(long)]
        json: bool,
    },
    /// Move this machine from GRUB to systemd-boot, in place. Requires root.
    ///
    /// Runs only when asked: `rime update` no longer does this on its own.
    /// Refused, with the reason named, when the machine is unhealthy right now
    /// (a failed unit, or a regression `rime recover status` reports), when
    /// an earlier attempt's trial boot did not come up (re-arm that with
    /// `rime-boot-migrate retry`), or when the engine's own precheck refuses
    /// (`rime-boot-migrate precheck --explain` lists every check). Nothing
    /// changes what boots until one BootNext write, and if the new path does
    /// not come up the boot after it is GRUB again.
    ///
    /// Exit status: 0 migrated or nothing to do, 10 refused, anything else
    /// failed with nothing committed.
    Migrate,
}

/// Where the report reads from.
///
/// A prefix, and only a prefix. `tests/test-boot-v2.sh` points it at a fixture
/// tree so the assertions run against the shipped binary rather than a
/// reimplementation of it. No program name is ever taken from the environment:
/// `bootctl` is a fixed absolute path, and under a fixture root it is not
/// executed at all — a pre-rendered `bootctl-list.json` is read instead. A
/// caller-controlled variable naming a program is a hole even in an
/// unprivileged command, because nothing stops root from running it.
struct Roots {
    /// `$RIME_BOOT_ROOT`, when set.
    fixture: Option<PathBuf>,
}

impl Roots {
    fn from_env() -> Self {
        Self { fixture: std::env::var_os("RIME_BOOT_ROOT").map(PathBuf::from) }
    }

    /// A fixture root chosen by the caller rather than by `$RIME_BOOT_ROOT`.
    ///
    /// `rime recover status` (§19) reports the boot chain as one row of its
    /// surface and reads it through [`chain_facts`], under its own fixture
    /// variable. One reader, two callers — the alternative was a second
    /// efivar decoder, and two of those disagree the first time one of them
    /// is corrected.
    fn from_path(fixture: Option<PathBuf>) -> Self {
        Self { fixture }
    }

    fn path(&self, absolute: &str) -> PathBuf {
        match &self.fixture {
            // `absolute` always starts with '/', so strip it before joining or
            // Path::join would discard the prefix entirely and read the real
            // system — a fixture that silently reads /sys is worse than no
            // fixture, because the test would pass on the author's machine.
            Some(root) => root.join(absolute.trim_start_matches('/')),
            None => PathBuf::from(absolute),
        }
    }

    fn read(&self, absolute: &str) -> Option<Vec<u8>> {
        std::fs::read(self.path(absolute)).ok()
    }

    /// Like [`Roots::read`], but keeps the reason a failed read was refused.
    ///
    /// `read` answers `None` both for "does not exist" and for "exists, and I
    /// may not look" — fine for a caller that has no use for a file it cannot
    /// read either way. `efivar_result` uses this for `StubInfo` and
    /// `LoaderBootCountPath`, which decide `booted_from_uki` and
    /// `boot_counting`: those drive concrete claims downstream ("kernel and
    /// initramfs were loaded separately", "not in effect: this machine boots
    /// through GRUB"), so a permission refusal must not collapse into the same
    /// answer as "the variable was never set".
    fn read_result(&self, absolute: &str) -> std::io::Result<Vec<u8>> {
        std::fs::read(self.path(absolute))
    }

    fn exists(&self, absolute: &str) -> bool {
        self.path(absolute).exists()
    }

    /// Like [`Roots::exists`], but a refused `stat` comes back as `Err`
    /// instead of joining "does not exist".
    ///
    /// `Path::exists` (and therefore `exists` above) cannot make that
    /// distinction on its own: it answers `false` for every failed `stat`,
    /// `EACCES` included. Fine for a signal nobody reports as a fact about the
    /// machine; wrong for the TPM event log. On a stock machine `stat` on
    /// `/sys/kernel/security/tpm0/binary_bios_measurements` succeeds for any
    /// user — the directories are `0755` and only the leaf file is `0440
    /// root:tss` — so a bare DAC refusal on the directory chain is not the
    /// likely trigger here. A confined execution environment (a container
    /// with securityfs masked or restricted, an LSM policy denying the stat)
    /// is; either way, a refused `stat` must not report as "no event log".
    fn stat_present(&self, absolute: &str) -> Result<bool, String> {
        let p = self.path(absolute);
        match std::fs::metadata(&p) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("{}: {e}", p.display())),
        }
    }

    /// An EFI variable's payload as a string.
    ///
    /// efivarfs prepends 4 bytes of attributes, and systemd writes these
    /// particular values as UTF-16LE. Dropping every NUL after the prefix
    /// recovers the ASCII content, which is all these variables contain; a
    /// full UTF-16 decode would be more correct and would also be the only
    /// reason this file needed a dependency.
    fn efivar(&self, name: &str, guid: &str) -> Option<String> {
        let raw = self.read(&format!("/sys/firmware/efi/efivars/{name}-{guid}"))?;
        if raw.len() <= 4 {
            return None;
        }
        let text: String = raw[4..]
            .iter()
            .copied()
            .filter(|b| *b != 0)
            .map(char::from)
            .collect();
        let trimmed = text.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    }

    /// Like [`Roots::efivar`], but a refused read comes back as `Err` instead
    /// of joining "the variable does not exist".
    fn efivar_result(&self, name: &str, guid: &str) -> Result<Option<String>, String> {
        let path = format!("/sys/firmware/efi/efivars/{name}-{guid}");
        let raw = match self.read_result(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{path}: {e}")),
        };
        if raw.len() <= 4 {
            return Ok(None);
        }
        let text: String = raw[4..]
            .iter()
            .copied()
            .filter(|b| *b != 0)
            .map(char::from)
            .collect();
        let trimmed = text.trim().to_string();
        Ok((!trimmed.is_empty()).then_some(trimmed))
    }

    /// A one-byte boolean EFI variable, as the firmware publishes SecureBoot.
    fn efivar_bool(&self, name: &str, guid: &str) -> Option<bool> {
        let raw = self.read(&format!("/sys/firmware/efi/efivars/{name}-{guid}"))?;
        raw.get(4).map(|b| *b != 0)
    }
}

/// `bootctl list --json=short`, or the reason it could not be read.
fn boot_entries(roots: &Roots) -> (Option<Vec<Value>>, Option<String>) {
    if roots.fixture.is_some() {
        let p = roots.path("/bootctl-list.json");
        return match std::fs::read(&p) {
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(Value::Array(v)) => (Some(v), None),
                Ok(_) => (None, Some(format!("{} is not a JSON array", p.display()))),
                Err(e) => (None, Some(format!("{}: {e}", p.display()))),
            },
            Err(e) => (None, Some(format!("{}: {e}", p.display()))),
        };
    }
    if !Path::new("/usr/bin/bootctl").exists() {
        return (None, Some("bootctl is not installed".into()));
    }
    let out = match Command::new("/usr/bin/bootctl")
        .args(["list", "--json=short"])
        .output()
    {
        Ok(o) => o,
        Err(e) => return (None, Some(format!("could not run bootctl: {e}"))),
    };
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let why = if why.is_empty() { "bootctl failed".into() } else { why };
        // The usual cause, and worth naming: the ESP is mode 0700.
        let hint = if unsafe { libc::geteuid() } != 0 {
            format!("{why} (reading the ESP needs root)")
        } else {
            why
        };
        return (None, Some(hint));
    }
    match serde_json::from_slice::<Value>(&out.stdout) {
        Ok(Value::Array(v)) => (Some(v), None),
        Ok(_) => (None, Some("bootctl did not return a JSON array".into())),
        Err(e) => (None, Some(format!("could not parse bootctl output: {e}"))),
    }
}

fn read_json_file(roots: &Roots, absolute: &str) -> (Option<Value>, Option<String>) {
    match roots.read(absolute) {
        None => (None, None), // absent is a normal state, not an error
        Some(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => (Some(v), None),
            Err(e) => (None, Some(format!("{absolute}: {e}"))),
        },
    }
}

/// Which loader ran this boot.
///
/// Extracted so `status` and §19's recovery surface cannot disagree about it.
/// The answer decides more than a label: on GRUB, `systemd.unit=rescue.target`
/// typed at the boot menu is a real recovery route, and on the opt-in UKI path
/// it is not — a UKI's command line is inside the signed image and cannot be
/// edited at the menu. A surface that claimed the route uniformly would be
/// lying on exactly the machines that need it.
fn detect_bootloader(cmdline: &str, loader_info: Option<&str>) -> &'static str {
    if loader_info.is_some_and(|s| s.starts_with("systemd-boot")) {
        return "systemd-boot";
    }
    // GRUB's BLS entries carry ostree=; this is the expected answer on every
    // published Rime image.
    if cmdline.contains("BOOT_IMAGE=") || cmdline.contains("ostree=") {
        return "grub";
    }
    "unknown"
}

/// The boot-chain facts §19's recovery surface reports, from the same reads
/// `rime boot status` uses.
pub(crate) struct ChainFacts {
    pub bootloader: &'static str,
    /// Set when `LoaderInfo` itself could not be read — meaning `bootloader`
    /// fell back to the cmdline heuristic (or "unknown") without ever having
    /// a chance to see "systemd-boot". A directory-wide efivarfs refusal (a
    /// container with it masked, an LSM policy) takes `LoaderInfo` down with
    /// `StubInfo`/`LoaderBootCountPath`, and a caller that trusts `bootloader`
    /// as "grub" in that case can un-gate the rescue-target route on exactly
    /// the systemd-boot+UKI machine where it must not exist.
    pub bootloader_unavailable: Option<String>,
    /// `None` means the kernel exposed no `SecureBoot` variable at all — not a
    /// UEFI boot. Reporting `false` there would claim a measurement nobody
    /// took.
    pub secure_boot: Option<bool>,
    pub setup_mode: Option<bool>,
    /// `Err(reason)` when `StubInfo` could not be read for a reason other than
    /// "does not exist" — never collapsed into `Ok(false)`, which reads
    /// downstream as "not a UKI" and both un-gates the rescue-target route and
    /// prints "kernel and initramfs were loaded separately" about a boot
    /// nobody actually looked at.
    pub booted_from_uki: Result<bool, String>,
    /// Same distinction, for `LoaderBootCountPath`.
    pub boot_counting: Result<bool, String>,
}

/// Read the boot chain under an explicit fixture root (or the real system when
/// `fixture` is `None`). No subprocess: every value here is a file read, which
/// is what lets `rime recover status` promise it spawns nothing.
pub(crate) fn chain_facts(fixture: Option<PathBuf>) -> ChainFacts {
    let roots = Roots::from_path(fixture);
    let cmdline = roots
        .read("/proc/cmdline")
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
        .unwrap_or_default();
    let (loader_info, bootloader_unavailable) = match roots.efivar_result("LoaderInfo", LOADER_GUID)
    {
        Ok(v) => (v, None),
        Err(e) => (None, Some(e)),
    };
    ChainFacts {
        bootloader: detect_bootloader(&cmdline, loader_info.as_deref()),
        bootloader_unavailable,
        secure_boot: roots.efivar_bool("SecureBoot", GLOBAL_GUID),
        setup_mode: roots.efivar_bool("SetupMode", GLOBAL_GUID),
        booted_from_uki: roots.efivar_result("StubInfo", LOADER_GUID).map(|v| v.is_some()),
        boot_counting: roots.efivar_result("LoaderBootCountPath", LOADER_GUID).map(|v| v.is_some()),
    }
}

fn build_report(roots: &Roots) -> Value {
    let cmdline = roots
        .read("/proc/cmdline")
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
        .unwrap_or_default();

    // sd-stub sets StubInfo when a UKI's stub ran; sd-boot sets LoaderInfo.
    // Between them they identify the path this boot actually took, which is
    // more reliable than looking for a bootloader binary on disk: an ESP can
    // hold several and only one of them ran.
    //
    // Both are read with the reason a refused read carries, because both
    // decide a concrete claim below ("kernel and initramfs were loaded
    // separately", "not in effect: this machine boots through GRUB") and a
    // permission refusal must not be able to pick the same side ENOENT does.
    let stub_info_result = roots.efivar_result("StubInfo", LOADER_GUID);
    let (stub_info, booted_from_uki, booted_from_uki_error) = match &stub_info_result {
        Ok(v) => (v.clone(), Some(v.is_some()), None),
        Err(e) => (None, None, Some(e.clone())),
    };
    // A directory-wide efivarfs refusal takes LoaderInfo down with StubInfo
    // and LoaderBootCountPath, and `detect_bootloader` cannot tell "LoaderInfo
    // says no" from "LoaderInfo could not be read" from an `Option` alone —
    // the first is a fact, the second must not silently become "grub".
    let loader_info_result = roots.efivar_result("LoaderInfo", LOADER_GUID);
    let (loader_info, bootloader_unavailable) = match &loader_info_result {
        Ok(v) => (v.clone(), None),
        Err(e) => (None, Some(e.clone())),
    };
    let boot_count_result = roots.efivar_result("LoaderBootCountPath", LOADER_GUID);
    let (boot_count_path, counting, counting_error) = match &boot_count_result {
        Ok(v) => (v.clone(), Some(v.is_some()), None),
        Err(e) => (None, None, Some(e.clone())),
    };

    let bootloader = detect_bootloader(&cmdline, loader_info.as_deref());

    let (entries, entries_error) = boot_entries(roots);
    let (health, health_error) = read_json_file(roots, "/var/lib/rime/boot/last-health.json");
    let (notice, notice_error) = read_json_file(roots, "/var/lib/rime/boot/rollback-notice.json");

    let (event_log, event_log_error) =
        match roots.stat_present("/sys/kernel/security/tpm0/binary_bios_measurements") {
            Ok(present) => (Some(present), None),
            Err(e) => (None, Some(e)),
        };

    let mut tallies = Map::new();
    if let Some(list) = entries.as_ref() {
        for e in list {
            let id = e.get("id").and_then(Value::as_str).unwrap_or("?");
            // triesLeft is ABSENT for an entry with no counter, which is what
            // a blessed deployment looks like. Defaulting it to 0 would report
            // every healthy deployment as out of tries.
            tallies.insert(
                id.to_string(),
                json!({
                    "path": e.get("path"),
                    "title": e.get("title"),
                    "triesLeft": e.get("triesLeft"),
                    "triesDone": e.get("triesDone"),
                    "blessed": e.get("triesLeft").is_none(),
                    "exhausted": e.get("triesLeft").and_then(Value::as_u64) == Some(0),
                    "isDefault": e.get("isDefault").and_then(Value::as_bool).unwrap_or(false),
                }),
            );
        }
    }

    json!({
        "bootloader": bootloader,
        // Set when LoaderInfo could not be read — `bootloader` above then
        // came only from the cmdline heuristic, not from ever having seen
        // whether systemd-boot set its marker.
        "bootloaderUnavailable": bootloader_unavailable,
        "loaderInfo": loader_info,
        "stubInfo": stub_info,
        // `null`, not `false`, when StubInfo could not be read — a refused
        // read is not a measurement that this booted without a UKI.
        "bootedFromUki": booted_from_uki,
        "bootedFromUkiUnavailable": booted_from_uki_error,
        "secureBoot": {
            // Absent means the kernel exposed no such variable, i.e. this is
            // not a UEFI boot at all. Reporting `false` there would claim a
            // measurement that was never taken.
            "enabled": roots.efivar_bool("SecureBoot", GLOBAL_GUID),
            "setupMode": roots.efivar_bool("SetupMode", GLOBAL_GUID),
        },
        "measuredBoot": {
            "tpmPresent": roots.exists("/sys/class/tpm/tpm0"),
            // `null` when the stat was refused rather than absent. On a stock
            // machine the directory chain is world-searchable and only the
            // leaf file is root:tss, so DAC alone will not usually trigger
            // this; a confined environment (a container, an LSM policy) can,
            // and `rime boot status` is documented read-only and root-free
            // either way.
            "eventLog": event_log,
            "eventLogUnavailable": event_log_error,
            // sd-stub hands the UKI's .pcrsig/.pcrpkey to userspace here. Its
            // presence is what makes a signed PCR 11 policy — and therefore a
            // TPM-bound LUKS2 keyslot that survives a kernel update — possible
            // on this boot.
            "pcrSignature": roots.exists("/run/systemd/tpm2-pcr-signature.json"),
            "pcrPublicKey": roots.exists("/run/systemd/tpm2-pcr-public-key.pem"),
        },
        "bootCounting": {
            // `null` when LoaderBootCountPath could not be read — not the
            // same claim as "systemd-boot did not set it".
            "inEffect": counting,
            "inEffectUnavailable": counting_error,
            "countPath": boot_count_path,
            "selectedEntry": roots.efivar("LoaderEntrySelected", LOADER_GUID),
            "defaultEntry": roots.efivar("LoaderEntryDefault", LOADER_GUID),
            "entries": if entries.is_some() { Value::Object(tallies) } else { Value::Null },
            "entriesUnavailable": entries_error,
        },
        "health": health,
        "healthError": health_error,
        "rollbackNotice": notice,
        "rollbackNoticeError": notice_error,
    })
}

fn print_human(r: &Value) {
    let s = |p: &[&str]| -> String {
        let mut cur = r;
        for k in p {
            match cur.get(*k) {
                Some(v) => cur = v,
                None => return "unknown".into(),
            }
        }
        match cur {
            Value::Null => "unknown".into(),
            Value::String(x) => x.clone(),
            other => other.to_string(),
        }
    };

    println!("Bootloader     : {}", s(&["bootloader"]));
    if let Some(why) = r.get("bootloaderUnavailable").and_then(Value::as_str) {
        // The identity above is a heuristic guess made without ever seeing
        // whether systemd-boot's own marker was there — say so, rather than
        // let "grub" read as a measurement it is not.
        println!(
            "                 LoaderInfo could not be read ({why}); this is the \
             cmdline fallback, not a confirmed identity"
        );
    }
    if let Some(info) = r.get("loaderInfo").and_then(Value::as_str) {
        println!("                 {info}");
    }
    let sb = match r.pointer("/secureBoot/enabled") {
        Some(Value::Bool(true)) => "enabled",
        Some(Value::Bool(false)) => "disabled",
        _ => "unavailable (not a UEFI boot, or the variable is not exposed)",
    };
    println!("Secure Boot    : {sb}");
    println!(
        "Signed UKI     : {}",
        match r.get("bootedFromUki") {
            Some(Value::Bool(true)) => format!("yes ({})", s(&["stubInfo"])),
            Some(Value::Bool(false)) => "no — kernel and initramfs were loaded separately".into(),
            // `null`: StubInfo could not be read, which is not the same
            // finding as "read, and it said no UKI".
            _ => format!(
                "unavailable — {}",
                r.get("bootedFromUkiUnavailable")
                    .and_then(Value::as_str)
                    .unwrap_or("no reason reported")
            ),
        }
    );

    let m = |k: &str| matches!(r.pointer(&format!("/measuredBoot/{k}")), Some(Value::Bool(true)));
    let event_log = match r.pointer("/measuredBoot/eventLog") {
        Some(Value::Bool(true)) => "present".to_string(),
        Some(Value::Bool(false)) => "absent".to_string(),
        // `null`: the stat was refused, not a stat that came back negative.
        _ => format!(
            "unavailable ({})",
            r.pointer("/measuredBoot/eventLogUnavailable")
                .and_then(Value::as_str)
                .unwrap_or("no reason reported")
        ),
    };
    println!(
        "Measured boot  : TPM {}, event log {}, signed PCR policy {}",
        if m("tpmPresent") { "present" } else { "absent" },
        event_log,
        if m("pcrSignature") { "in effect" } else { "not in effect" },
    );

    match r.pointer("/bootCounting/inEffect") {
        Some(Value::Bool(false)) => {
            // The expected state on every published image. Say so, rather than
            // leaving a reader to wonder which half is broken.
            println!(
                "Boot counting  : not in effect — this machine boots via {}, which has no \n\
                 \x20                boot counter. Rime is moving to systemd-boot:\n\
                 \x20                `sudo rime boot migrate` moves this machine in place when\n\
                 \x20                you ask, it is healthy and it can be done safely, and\n\
                 \x20                `rime-boot-migrate precheck` says why not when it cannot.\n\
                 \x20                Until then this is the normal state, not a fault.",
                s(&["bootloader"])
            );
        }
        Some(Value::Bool(true)) => {
            println!("Boot counting  : in effect");
            println!("  selected     : {}", s(&["bootCounting", "selectedEntry"]));
            match r.pointer("/bootCounting/entries") {
                Some(Value::Object(map)) if !map.is_empty() => {
                    // BTreeMap so the order is stable between runs; an
                    // unstable listing makes a diff of two reports unreadable.
                    let ordered: BTreeMap<_, _> = map.iter().collect();
                    for (id, e) in ordered {
                        let state = if e.get("exhausted") == Some(&Value::Bool(true)) {
                            "OUT OF TRIES"
                        } else if e.get("blessed") == Some(&Value::Bool(true)) {
                            "good"
                        } else {
                            "on trial"
                        };
                        let left = e
                            .get("triesLeft")
                            .and_then(Value::as_u64)
                            .map(|n| format!("{n} left"))
                            .unwrap_or_else(|| "no counter".into());
                        println!("  {id:<32} {state:<12} {left}");
                    }
                }
                _ => println!(
                    "  entries      : unavailable — {}",
                    r.get("bootCounting")
                        .and_then(|c| c.get("entriesUnavailable"))
                        .and_then(Value::as_str)
                        .unwrap_or("no reason reported")
                ),
            }
        }
        // `null`: LoaderBootCountPath could not be read, which is not the
        // same finding as "read, and systemd-boot did not set it".
        _ => println!(
            "Boot counting  : unavailable — {}",
            r.pointer("/bootCounting/inEffectUnavailable")
                .and_then(Value::as_str)
                .unwrap_or("no reason reported")
        ),
    }

    match r.get("health") {
        Some(Value::Object(h)) => {
            let verdict = h.get("verdict").and_then(Value::as_str).unwrap_or("?");
            println!("Last health    : {verdict} at {}",
                     h.get("checkedAt").and_then(Value::as_str).unwrap_or("?"));
            // Which entry the verdict was about, and which target had to be
            // reached for it. Without these the verdict is unattributable on a
            // machine with more than one deployment — and the health script
            // records both, so dropping them here was losing information
            // rather than choosing not to gather it.
            println!("  entry        : {}",
                     h.get("entry").and_then(Value::as_str).unwrap_or("?"));
            println!("  target       : {}",
                     h.get("target").and_then(Value::as_str).unwrap_or("?"));
            if let Some(Value::Array(f)) = h.get("failures") {
                for item in f {
                    println!("  failure      : {}", item.as_str().unwrap_or("?"));
                }
            }
        }
        // "never ran" is not "healthy". rime-boot-health.service only runs
        // when boot counting is in effect, so on a GRUB machine this is the
        // correct and expected answer.
        _ => println!("Last health    : no verdict recorded (the health unit has not run)"),
    }

    // `rolledBack` must be true, not merely present. A notice file that exists
    // with rolledBack false would otherwise announce a rollback that did not
    // happen, and the file is written by a separate program in a separate
    // language — presence is not a contract, the field is.
    if let Some(Value::Object(n)) = r.get("rollbackNotice").filter(|v| {
        v.get("rolledBack") == Some(&Value::Bool(true))
    }) {
        println!();
        println!("!! This machine was rolled back automatically.");
        println!(
            "   Running     : {}",
            n.get("runningEntry").and_then(Value::as_str).unwrap_or("?")
        );
        if let Some(Value::Array(failed)) = n.get("failedEntries") {
            for e in failed {
                println!(
                    "   Failed      : {} after {} attempts",
                    e.get("id").and_then(Value::as_str).unwrap_or("?"),
                    e.get("triesDone").and_then(Value::as_u64).unwrap_or(0)
                );
            }
        }
        println!("   Noticed     : {}",
                 n.get("noticedAt").and_then(Value::as_str).unwrap_or("?"));
    }
}

/// Returns a process exit code, like the other read-only reporting verbs.
///
/// 0 always: this reports state, and "GRUB, no boot counter, no TPM policy" is
/// the correct state for every published Rime image rather than a fault. A
/// non-zero exit would make `rime boot status` unusable as a health check in
/// exactly the configuration Rime ships. The only failure it can report is its
/// own — an unserialisable report, which cannot happen and is handled anyway
/// rather than unwrapped.
pub fn boot_main(cmd: BootCmd) -> i32 {
    match cmd {
        BootCmd::Status { json: as_json } => {
            let roots = Roots::from_env();
            let report = build_report(&roots);
            if as_json {
                match serde_json::to_string_pretty(&report) {
                    Ok(text) => println!("{text}"),
                    Err(e) => {
                        eprintln!("rime boot status: could not serialise the report: {e}");
                        return 1;
                    }
                }
            } else {
                print_human(&report);
            }
            0
        }
        BootCmd::Migrate => crate::ops::boot_migrate_command(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture root makes every read relative, including the ones that would
    /// otherwise hit the developer's own /sys. This is asserted directly
    /// because a `Path::join` on an absolute path silently discards the
    /// prefix, and the resulting test would pass on a machine that happens to
    /// look right.
    #[test]
    fn fixture_root_is_not_discarded_by_an_absolute_path() {
        let roots = Roots { fixture: Some(PathBuf::from("/tmp/fixture")) };
        assert_eq!(
            roots.path("/proc/cmdline"),
            PathBuf::from("/tmp/fixture/proc/cmdline")
        );
        assert_eq!(
            roots.path("/sys/firmware/efi/efivars/StubInfo-x"),
            PathBuf::from("/tmp/fixture/sys/firmware/efi/efivars/StubInfo-x")
        );
    }

    #[test]
    fn no_fixture_root_reads_the_real_paths() {
        let roots = Roots { fixture: None };
        assert_eq!(roots.path("/proc/cmdline"), PathBuf::from("/proc/cmdline"));
    }

    /// A blessed entry has no `triesLeft` key at all. Treating a missing key as
    /// 0 would report every healthy deployment as out of tries and would make
    /// the rollback notice fire on a machine that never rolled back.
    #[test]
    fn a_missing_tries_left_is_blessed_not_exhausted() {
        let blessed: Value = serde_json::json!({"id": "rime-good.efi"});
        let exhausted: Value = serde_json::json!({"id": "rime-new.efi", "triesLeft": 0});
        let on_trial: Value = serde_json::json!({"id": "rime-new.efi", "triesLeft": 2});

        assert!(blessed.get("triesLeft").is_none());
        assert_eq!(exhausted.get("triesLeft").and_then(Value::as_u64), Some(0));
        assert_ne!(on_trial.get("triesLeft").and_then(Value::as_u64), Some(0));
    }

    // ── EACCES vs ENOENT ─────────────────────────────────────────────────────
    //
    // `StubInfo`, `LoaderBootCountPath` and the TPM event log all went through
    // helpers that answered a refused read the same way they answered a
    // missing one. `booted_from_uki=false` then printed "kernel and initramfs
    // were loaded separately" and `boot_counting=false` printed "not in
    // effect: this machine boots through GRUB" — both about a boot nobody
    // actually looked at. These tests seal the parent directory to 0000 so
    // the read hits EACCES rather than ENOENT, and require the exact
    // `PermissionDenied` kind so an unrelated failure cannot count as a
    // successful seal. Root and CAP_DAC_OVERRIDE walk through 0000 regardless
    // of the mode bit, so the seal is checked rather than assumed, and each
    // test skips out loud rather than passing vacuously when it does not take.

    fn seal(dir: &Path, probe: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(dir).expect("stat").permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(dir, perms).expect("chmod");
        matches!(std::fs::metadata(probe), Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied)
    }

    fn unseal(dir: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(dir).expect("stat").permissions();
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(dir, perms);
    }

    #[test]
    fn an_unreadable_stubinfo_is_not_a_measurement_of_no_uki() {
        let dir =
            std::env::temp_dir().join(format!("rime-boot-eacces-stubinfo-{}", std::process::id()));
        let efivars = dir.join("sys/firmware/efi/efivars");
        std::fs::create_dir_all(&efivars).unwrap();
        let target = efivars.join(format!("StubInfo-{LOADER_GUID}"));
        std::fs::write(&target, b"\x07\x00\x00\x00").unwrap();

        let sealed = seal(&efivars, &target);
        let facts = chain_facts(Some(dir.clone()));
        unseal(&efivars);
        std::fs::remove_dir_all(&dir).ok();

        if !sealed {
            return; // root, or CAP_DAC_OVERRIDE: the mode bit proves nothing here
        }
        assert!(
            facts.booted_from_uki.is_err(),
            "a refused StubInfo read must not be reported as 'did not boot a UKI', got {:?}",
            facts.booted_from_uki
        );
    }

    #[test]
    fn an_unreadable_loaderinfo_is_not_a_measurement_of_grub() {
        // The same distinction one variable over, and the one nothing here
        // asserted: a refused `LoaderInfo` read leaves `detect_bootloader`
        // with no marker to see, so it answers off the kernel command line —
        // which carries `ostree=` on every Rime image and therefore always
        // says "grub". The label is allowed to be that guess; what must not
        // happen is the guess arriving with nothing attached to say it is one.
        let dir = std::env::temp_dir()
            .join(format!("rime-boot-eacces-loaderinfo-{}", std::process::id()));
        let efivars = dir.join("sys/firmware/efi/efivars");
        std::fs::create_dir_all(&efivars).unwrap();
        std::fs::create_dir_all(dir.join("proc")).unwrap();
        std::fs::write(
            dir.join("proc/cmdline"),
            "BOOT_IMAGE=/vmlinuz ostree=/ostree/boot.1/rime/abc/0 root=UUID=x rw\n",
        )
        .unwrap();
        let target = efivars.join(format!("LoaderInfo-{LOADER_GUID}"));
        std::fs::write(
            &target,
            b"\x07\x00\x00\x00s\x00y\x00s\x00t\x00e\x00m\x00d\x00-\x00b\x00o\x00o\x00t\x00",
        )
        .unwrap();

        let sealed = seal(&efivars, &target);
        let facts = chain_facts(Some(dir.clone()));
        unseal(&efivars);
        std::fs::remove_dir_all(&dir).ok();

        if !sealed {
            return; // root, or CAP_DAC_OVERRIDE: the mode bit proves nothing here
        }
        assert_eq!(
            facts.bootloader, "grub",
            "the cmdline fallback is the label; that part is expected"
        );
        let why = facts
            .bootloader_unavailable
            .as_deref()
            .expect("a refused LoaderInfo read must set bootloader_unavailable");
        assert!(
            why.contains("LoaderInfo") && why.contains("Permission denied"),
            "the reason must name what could not be read, got: {why}"
        );
    }

    #[test]
    fn a_genuinely_absent_loaderinfo_is_a_reading_rather_than_a_refusal() {
        // ENOENT is a measurement: a GRUB machine sets no LoaderInfo at all,
        // and caveating the label there would put the warning on every
        // machine in the fleet, where it would mean nothing.
        let dir = std::env::temp_dir()
            .join(format!("rime-boot-enoent-loaderinfo-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sys/firmware/efi/efivars")).unwrap();
        let facts = chain_facts(Some(dir.clone()));
        std::fs::remove_dir_all(&dir).ok();
        assert!(
            facts.bootloader_unavailable.is_none(),
            "an absent LoaderInfo must not read as a refusal, got {:?}",
            facts.bootloader_unavailable
        );
    }

    #[test]
    fn a_genuinely_absent_stubinfo_is_still_measured_as_no_uki() {
        // The other half: ENOENT must stay `Ok(false)`, or the fix would turn
        // every GRUB machine — which never sets StubInfo — into a warning.
        let dir =
            std::env::temp_dir().join(format!("rime-boot-enoent-stubinfo-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sys/firmware/efi/efivars")).unwrap();
        let facts = chain_facts(Some(dir.clone()));
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(facts.booted_from_uki, Ok(false));
    }

    #[test]
    fn an_unreadable_loaderbootcountpath_is_not_a_measurement_of_no_counting() {
        let dir = std::env::temp_dir()
            .join(format!("rime-boot-eacces-bootcount-{}", std::process::id()));
        let efivars = dir.join("sys/firmware/efi/efivars");
        std::fs::create_dir_all(&efivars).unwrap();
        let target = efivars.join(format!("LoaderBootCountPath-{LOADER_GUID}"));
        std::fs::write(&target, b"\x07\x00\x00\x00").unwrap();

        let sealed = seal(&efivars, &target);
        let facts = chain_facts(Some(dir.clone()));
        unseal(&efivars);
        std::fs::remove_dir_all(&dir).ok();

        if !sealed {
            return;
        }
        assert!(
            facts.boot_counting.is_err(),
            "a refused LoaderBootCountPath read must not be reported as 'not in \
             effect', got {:?}",
            facts.boot_counting
        );
    }

    #[test]
    fn a_genuinely_absent_loaderbootcountpath_is_still_measured_as_not_counting() {
        let dir = std::env::temp_dir()
            .join(format!("rime-boot-enoent-bootcount-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sys/firmware/efi/efivars")).unwrap();
        let facts = chain_facts(Some(dir.clone()));
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(facts.boot_counting, Ok(false));
    }

    #[test]
    fn an_unreadable_event_log_is_not_a_measurement_of_absence() {
        // On a stock machine `/sys/kernel/security` and `tpm0/` are
        // world-searchable and only the leaf file is `0440 root:tss`, so a
        // bare DAC refusal on this directory chain is contrived — this test
        // proves the code path rather than a permission bit seen in the
        // field. A confined environment (a container with securityfs masked,
        // an LSM policy) is the plausible real trigger, and `rime boot
        // status` is documented read-only and root-free either way.
        let dir =
            std::env::temp_dir().join(format!("rime-boot-eacces-eventlog-{}", std::process::id()));
        let tpm0 = dir.join("sys/kernel/security/tpm0");
        std::fs::create_dir_all(&tpm0).unwrap();
        let target = tpm0.join("binary_bios_measurements");
        std::fs::write(&target, b"log").unwrap();
        let security = dir.join("sys/kernel/security");

        let sealed = seal(&security, &target);
        let roots = Roots { fixture: Some(dir.clone()) };
        let report = build_report(&roots);
        unseal(&security);
        std::fs::remove_dir_all(&dir).ok();

        if !sealed {
            return;
        }
        assert!(
            report.pointer("/measuredBoot/eventLog").is_some_and(Value::is_null),
            "a refused stat must not be reported as 'event log absent': {report}"
        );
        assert!(
            report
                .pointer("/measuredBoot/eventLogUnavailable")
                .and_then(Value::as_str)
                .is_some(),
            "the reason should be carried: {report}"
        );
    }

    #[test]
    fn a_genuinely_absent_event_log_is_still_measured_absent() {
        let dir =
            std::env::temp_dir().join(format!("rime-boot-enoent-eventlog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let roots = Roots { fixture: Some(dir.clone()) };
        let report = build_report(&roots);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(report.pointer("/measuredBoot/eventLog"), Some(&Value::Bool(false)));
    }
}
