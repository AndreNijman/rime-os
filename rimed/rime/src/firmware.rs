//! `rime firmware` — what fwupd knows about this machine's firmware.
//!
//! The split is the one `rime storage` uses and for the same reason: every
//! judgement lives in `rimed_core::firmware` and is driven from captured
//! documents, and this file only decides where the document comes from. Under
//! `$RIME_FIRMWARE_ROOT` no subprocess is spawned at all, so the suite
//! exercises the shipped binary against this machine's own 28-device answer
//! without a daemon, a bus or a privilege.
//!
//! **fwupd's exit status is never read here.** Measured on the development
//! machine: `get-updates --json` with nothing to do exits 0, the same command
//! without `--json` exits 2 with a full device list, and a request for a
//! device that does not exist exits 0 with an `Error` object in the body. The
//! status carries no information in either direction, and
//! `rimed_core::firmware::parse_devices` therefore takes no exit code to be
//! given one.

use std::path::PathBuf;
use std::process::Command;

use rimed_core::firmware::{self, Device, Report, Writable};
use rimed_core::storage::Reading;
use clap::Subcommand;
use serde_json::{json, Value};

/// fwupdmgr, by absolute path. `rime update` already spawns it from `PATH`;
/// this path is read-only and unprivileged, but a firmware tool is the last
/// place to start trusting `PATH`.
const FWUPDMGR: &str = rime_paths::FWUPDMGR;

/// Where the readout gets its documents.
pub struct Roots {
    fixture: Option<PathBuf>,
}

impl Roots {
    pub fn from_env() -> Self {
        Self { fixture: std::env::var_os("RIME_FIRMWARE_ROOT").map(PathBuf::from) }
    }

    fn is_fixture(&self) -> bool {
        self.fixture.is_some()
    }

    /// A captured fwupd answer, or the reason there is not one.
    ///
    /// `NotFound` is the only definite absence — the same rule the storage
    /// guard had to learn twice, kept here so a fixture with an unreadable
    /// file reports that rather than silently behaving like a fixture that
    /// deliberately omitted it.
    fn fixture_file(&self, rel: &str) -> Result<String, String> {
        let root = self.fixture.as_ref().ok_or("no fixture root")?;
        let p = root.join(".fixture").join(rel);
        match std::fs::read_to_string(&p) {
            Ok(t) => Ok(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(format!("the fixture did not capture {rel}"))
            }
            Err(e) => Err(format!("{}: {e}", p.display())),
        }
    }
}

#[derive(Subcommand)]
pub enum FirmwareCmd {
    /// What firmware this machine carries, and what has an update waiting.
    Status {
        #[arg(long)]
        json: bool,
    },
}

/// One `fwupdmgr … --json` answer, as text.
///
/// The exit status is deliberately not looked at; only whether the process
/// could be started at all, which is a different question. `stderr` is folded
/// into the failure message because fwupd puts `No updates available` there
/// while putting the answer on stdout, and a reader that sees neither should
/// be told what it did say.
fn ask(roots: &Roots, verb: &str) -> Result<String, String> {
    if roots.is_fixture() {
        return roots.fixture_file(&format!("{verb}.json"));
    }
    match std::fs::metadata(FWUPDMGR) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "{FWUPDMGR} is not installed, so nothing on this machine can \
                 report its firmware — install fwupd"
            ))
        }
        Err(e) => return Err(format!("{FWUPDMGR}: {e}")),
        Ok(_) => {}
    }
    let out = Command::new(FWUPDMGR)
        .args([verb, "--json"])
        .output()
        .map_err(|e| format!("{FWUPDMGR} {verb}: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if text.trim().is_empty() {
        let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if said.is_empty() {
            format!("{FWUPDMGR} {verb} printed nothing at all")
        } else {
            format!("{FWUPDMGR} {verb} printed nothing on stdout; it said: {said}")
        });
    }
    Ok(text)
}

/// Both documents, judged.
fn report(roots: &Roots) -> Result<Report, String> {
    let devices = firmware::parse_devices(&ask(roots, "get-devices")?)?;
    // The pending list is a `Reading`, not a `?`: fwupd being unable to say
    // whether updates exist is a thing to report, and it is the row that must
    // not be silent — a machine nobody could ask looks exactly like a machine
    // with nothing to do. The device list, by contrast, is the whole subject
    // of the command, so failing to read it is failing.
    let pending = match ask(roots, "get-updates") {
        Err(why) => Reading::Unavailable(why),
        Ok(text) => match firmware::parse_devices(&text) {
            Err(why) => Reading::Unavailable(why),
            Ok(list) => Reading::Known(list),
        },
    };
    Ok(Report::new(devices, pending))
}

fn version_or_reason(d: &Device) -> String {
    match d.version_text() {
        Reading::Known(v) => match d.version_format.as_deref() {
            // `0x0006` is a real version and looks like a mistake, so the
            // format fwupd declared travels with it.
            Some("hex") => format!("{v} (hex)"),
            _ => v,
        },
        Reading::Unavailable(_) => "no version reported".into(),
    }
}

fn device_json(d: &Device) -> Value {
    json!({
        "device_id": d.device_id,
        "label": d.label(),
        "name": d.name,
        "version": d.version,
        "version_format": d.version_format,
        "vendor": d.vendor,
        "plugin": d.plugin,
        // `updatable` is kept because it is what fwupd literally said, and
        // `writable` beside it because the flag alone is a momentary
        // condition that a consumer would read as a capability.
        "updatable": d.is_updatable(),
        "writable": match d.writable() {
            Writable::Now => json!("now"),
            Writable::Never => json!("not-offered"),
            Writable::Blocked(why) => json!({"blocked": why}),
        },
        "needs_reboot": d.needs_reboot(),
        // The reason travels in the JSON too. The storage half learned this
        // the hard way: a remedy that reaches only the human-readable footer
        // is a remedy the settings page and the notifier never see.
        "version_unavailable": d.version_text().reason(),
        "blockers": d.blockers(),
    })
}

fn status(as_json: bool) -> i32 {
    let roots = Roots::from_env();
    let r = match report(&roots) {
        Ok(r) => r,
        Err(why) => {
            if as_json {
                println!("{}", json!({"error": why}));
            } else {
                eprintln!("rime: cannot read this machine's firmware: {why}");
            }
            return 2;
        }
    };
    let attention = r.attention();

    if as_json {
        println!(
            "{}",
            json!({
                "hardware": r.hardware.iter().map(device_json).collect::<Vec<_>>(),
                "secure_boot": r.secure_boot.iter().map(device_json).collect::<Vec<_>>(),
                "pending": r.pending.known()
                    .map(|l| l.iter().map(device_json).collect::<Vec<_>>()),
                "pending_unavailable": r.pending.reason(),
                "attention": attention,
            })
        );
        return i32::from(!attention.is_empty());
    }

    println!("firmware, as fwupd reports it");
    for d in &r.hardware {
        // Three states, not a boolean. `updatable` is a momentary condition:
        // seven of these rows lose the flag the instant the laptop comes off
        // AC, and printing that as "read-only" says the System Firmware
        // cannot be updated when what is true is "not while on battery".
        let tag = match d.writable() {
            Writable::Now => "updatable",
            Writable::Never => "not offered",
            Writable::Blocked(_) => "blocked",
        };
        println!(
            "  [{tag}] {} — {} ({}, {})",
            d.label(),
            version_or_reason(d),
            d.plugin.as_deref().unwrap_or("no plugin"),
            d.short_id()
        );
    }
    // The reasons once, with a count, rather than on seven consecutive rows:
    // on battery this machine blocks seven devices for the same reason, and
    // repeating a sentence seven times is how a reader stops reading it.
    let mut blocked: Vec<(String, usize)> = Vec::new();
    for d in r.hardware.iter().chain(r.secure_boot.iter()) {
        if let Writable::Blocked(why) = d.writable() {
            for w in why {
                match blocked.iter_mut().find(|(s, _)| *s == w) {
                    Some((_, n)) => *n += 1,
                    None => blocked.push((w, 1)),
                }
            }
        }
    }
    for (why, n) in &blocked {
        println!("\n{n} device(s) cannot be written at the moment: {why}");
    }
    // Kept, not hidden: `fwupdmgr update` offers these, so a readout that
    // dropped them could not explain what it was about to do. Measured: nine
    // of this machine's eleven `updatable` rows are in this section, which is
    // why the split is on the plugin and not on the flag.
    if !r.secure_boot.is_empty() {
        println!(
            "\nSecure Boot keys and revocation lists ({} rows — not components of \
             this machine)",
            r.secure_boot.len()
        );
        for d in &r.secure_boot {
            println!("  {} — {}", d.label(), version_or_reason(d));
        }
    }

    println!();
    match &r.pending {
        Reading::Unavailable(why) => println!("[unavailable] firmware updates: {why}"),
        Reading::Known(l) if l.is_empty() => println!("no firmware updates are waiting"),
        Reading::Known(l) => println!("{} firmware update(s) waiting", l.len()),
    }
    for a in &attention {
        println!("  - {a}");
    }
    i32::from(!attention.is_empty())
}

/// Lines for `rime doctor`: `(ok, sentence)`.
///
/// The same rule the §48 storage lines follow, and it is not a soft one: only
/// a row with something to DO becomes a WARN, and a row nobody could measure
/// is printed with its reason and passes. `rime doctor` runs unprivileged on
/// machines where fwupd may not be installed at all — measured, nothing in any
/// Containerfile installs it today — and a subsystem that could not be
/// consulted must not turn every run of the doctor red. What it must not do
/// is go quiet: the reason is on the line either way.
pub fn doctor_lines(roots: &Roots) -> Vec<(bool, String)> {
    let r = match report(roots) {
        Err(why) => return vec![(true, format!("firmware: not measured — {why}"))],
        Ok(r) => r,
    };
    // The early return is a fix, not a shortcut. Falling through to the
    // attention loop below reported the SAME condition twice — once as a PASS
    // and once as a WARN — because `attention()`'s unavailable sentence reads
    // "whether this machine has firmware updates is unknown: …", which does
    // not contain the words "update waiting" and so sailed through the
    // filter. `rime doctor` printed a green line and a red line for one
    // unreadable fwupd, and the doc comment above promised the opposite.
    //
    // Fixed by structure rather than by widening the filter: a filter that
    // has to know the wording of every sentence `attention()` can produce is
    // one edit away from saying it twice again.
    let pending = match &r.pending {
        Reading::Unavailable(why) => {
            return vec![(true, format!("firmware updates: not measured — {why}"))]
        }
        Reading::Known(l) => l,
    };
    let mut out = Vec::new();
    if pending.is_empty() {
        out.push((true, "firmware: nothing waiting".into()));
    } else {
        out.push((false, format!("firmware: {} update(s) waiting", pending.len())));
    }
    // The blockers, which `attention` already restricts to devices that have
    // an update waiting — so this cannot become a standing complaint on a
    // machine with seven `require-ac-power` rows and nothing to install.
    for a in r.attention().iter().filter(|a| !a.contains("update waiting")) {
        out.push((false, format!("firmware: {a}")));
    }
    out
}

pub fn main(cmd: FirmwareCmd) -> i32 {
    match cmd {
        FirmwareCmd::Status { json } => status(json),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fwupdmgr_path_is_absolute() {
        assert!(FWUPDMGR.starts_with('/'), "{FWUPDMGR}");
    }

    #[test]
    fn a_hex_version_is_labelled_so_it_does_not_read_as_a_mistake() {
        let d = firmware::parse_devices(
            r#"{"Devices": [{"DeviceId": "aa", "Name": "Touchpad",
                             "Version": "0x0006", "VersionFormat": "hex"}]}"#,
        )
        .unwrap();
        assert_eq!(version_or_reason(&d[0]), "0x0006 (hex)");
    }

    #[test]
    fn a_device_with_no_version_says_so_rather_than_printing_a_blank() {
        let d = firmware::parse_devices(
            r#"{"Devices": [{"DeviceId": "aa", "Name": "GPIO controller"}]}"#,
        )
        .unwrap();
        assert_eq!(version_or_reason(&d[0]), "no version reported");
    }

    #[test]
    fn the_json_row_carries_the_reason_and_not_only_the_value() {
        // The storage half's lesson: a reason that reaches only the
        // human-readable footer is one the settings page never sees.
        let d = firmware::parse_devices(
            r#"{"Devices": [{"DeviceId": "aa", "Name": "GPIO controller"}]}"#,
        )
        .unwrap();
        let v = device_json(&d[0]);
        assert!(v["version"].is_null());
        assert!(
            v["version_unavailable"].as_str().unwrap().contains("no version"),
            "{v}"
        );
    }

    #[test]
    fn a_fixture_root_never_spawns_anything() {
        // The whole safety argument for the binary half of the suite.
        let dir = std::env::temp_dir().join(format!("rime-fw-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".fixture")).unwrap();
        let roots = Roots { fixture: Some(dir.clone()) };
        // Nothing captured: a reason, not a silent empty list, and certainly
        // not a subprocess.
        let why = ask(&roots, "get-devices").expect_err("nothing was captured");
        assert!(why.contains("did not capture"), "{why}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unreadable_capture_is_not_the_same_as_an_absent_one() {
        let dir = std::env::temp_dir().join(format!("rime-fw-x-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".fixture")).unwrap();
        let f = dir.join(".fixture").join("get-devices.json");
        std::fs::write(&f, "{}").unwrap();
        let mut perms = std::fs::metadata(&f).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o000);
        }
        std::fs::set_permissions(&f, perms).unwrap();
        let roots = Roots { fixture: Some(dir.clone()) };
        let why = ask(&roots, "get-devices").expect_err("an unreadable file is not absent");
        assert!(!why.contains("did not capture"), "{why}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A fixture root holding exactly the captures named, and nothing else.
    fn fixture(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("rime-fw-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join(".fixture")).unwrap();
        for (name, body) in files {
            std::fs::write(dir.join(".fixture").join(name), body).unwrap();
        }
        dir
    }

    /// One row that fwupd will not write right now, and says why.
    const ON_BATTERY: &str = r#"{"Devices": [
        {"DeviceId": "e2f710f8ff0d3b0a5a3f9d0b7c8e1f2a3b4c5d6e",
         "Name": "System Firmware", "Version": "0.1.17",
         "Plugin": "uefi_capsule", "Flags": ["internal", "needs-reboot"],
         "Problems": ["require-ac-power"]}]}"#;

    #[test]
    fn an_unconsultable_fwupd_is_exactly_one_doctor_line_and_it_passes() {
        // The defect this test exists for: the unavailable branch used to fall
        // through to the attention loop, whose sentence ("…firmware updates is
        // unknown: …") does not contain "update waiting" and so passed the
        // filter. `rime doctor` reported ONE unreadable fwupd as a PASS and a
        // WARN at the same time — and the comment on `doctor_lines` promised a
        // subsystem nobody could consult would not turn the doctor red.
        let dir = fixture("doctor-unavail", &[("get-devices.json", ON_BATTERY)]);
        let lines = doctor_lines(&Roots { fixture: Some(dir.clone()) });
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].0, "an unconsultable subsystem turned the doctor red: {lines:?}");
        assert!(lines[0].1.contains("not measured"), "{lines:?}");
        assert!(
            lines.iter().all(|(ok, _)| *ok),
            "the same condition was reported as a pass and a warning: {lines:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_healthy_machine_on_battery_is_one_passing_doctor_line() {
        // Seven rows on the real machine carry `require-ac-power` with nothing
        // to install. If those reached the doctor, every run on an unplugged
        // laptop would be red for ever.
        let dir = fixture(
            "doctor-healthy",
            &[
                ("get-devices.json", ON_BATTERY),
                ("get-updates.json", r#"{"Devices": []}"#),
            ],
        );
        let lines = doctor_lines(&Roots { fixture: Some(dir.clone()) });
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].0, "{lines:?}");
        assert!(lines[0].1.contains("nothing waiting"), "{lines:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_update_waiting_is_a_doctor_warning_and_its_blocker_comes_with_it() {
        // The other direction: a blocker is worth saying exactly when it
        // stands between this machine and an update it is being offered.
        let dir = fixture(
            "doctor-waiting",
            &[("get-devices.json", ON_BATTERY), ("get-updates.json", ON_BATTERY)],
        );
        let lines = doctor_lines(&Roots { fixture: Some(dir.clone()) });
        assert!(
            lines.iter().any(|(ok, w)| !ok && w.contains("update(s) waiting")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|(ok, w)| !ok && w.contains("plug the machine in")),
            "the blocker standing in the update's way never reached the doctor: {lines:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_fwupd_that_cannot_be_read_at_all_still_only_passes() {
        // No captures whatsoever: `report()` itself fails. Same rule.
        let dir = fixture("doctor-empty", &[]);
        let lines = doctor_lines(&Roots { fixture: Some(dir.clone()) });
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].0, "{lines:?}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
