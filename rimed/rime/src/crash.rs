//! `rime crash` — why the kernel died last time, in plain words.
//!
//! At a kernel panic the kernel keeps the end of its log in EFI variables
//! (efi_pstore, `files/system/kargs.d/30-rime-crash.toml`); systemd-pstore
//! moves them to /var/lib/systemd/pstore at the next boot, and
//! `rime-crash-record` (a root oneshot after it) writes the short summary this
//! prints: `/var/lib/rime/crash/last`, one `key=value` per line.
//!
//! Read-only and needs no root: the summary is world-readable, and the full
//! record is named, not opened. Nothing is sent anywhere.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::json;

/// Written by files/system/libexec/rime-crash-record.
const LAST: &str = "/var/lib/rime/crash/last";

/// The summary's fields, as rime-crash-record writes them.
#[derive(Debug, Default, PartialEq)]
pub struct Crash {
    pub id: String,
    pub kind: String,
    pub time: String,
    pub kernel: String,
    pub reason: String,
    pub module: String,
    pub record: String,
}

/// Parse the summary. `None` when it names no crash (no id).
pub fn parse(text: &str) -> Option<Crash> {
    let mut f: BTreeMap<&str, &str> = BTreeMap::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            // The first value wins: the writer never repeats a key.
            f.entry(k.trim()).or_insert(v.trim());
        }
    }
    let get = |k: &str| f.get(k).map(|v| v.to_string()).unwrap_or_default();
    let c = Crash {
        id: get("id"),
        kind: get("kind"),
        time: get("time"),
        kernel: get("kernel"),
        reason: get("reason"),
        module: get("module"),
        record: get("record"),
    };
    if c.id.is_empty() {
        None
    } else {
        Some(c)
    }
}

/// The plain-text report. `record_exists` says whether the full record is
/// still on disk (systemd ages the archive after 14 days).
pub fn render(c: &Crash, record_exists: bool) -> String {
    let mut out = String::new();
    let what = if c.kind == "panic" {
        "Last kernel crash (the machine restarted)"
    } else {
        "Last kernel error recorded (it did not stop the machine by itself)"
    };
    out.push_str(&format!("{what}\n"));
    let row = |out: &mut String, k: &str, v: &str| {
        if !v.is_empty() {
            out.push_str(&format!("  {k:<9} {v}\n"));
        }
    };
    row(&mut out, "When", &c.time);
    row(&mut out, "Kernel", &c.kernel);
    row(
        &mut out,
        "Reason",
        if c.reason.is_empty() { "not in the record" } else { &c.reason },
    );
    if !c.module.is_empty() {
        row(&mut out, "Driver", &format!("{} (named by the backtrace)", c.module));
    }
    if record_exists {
        row(&mut out, "Record", &c.record);
    } else if !c.record.is_empty() {
        row(&mut out, "Record", &format!("{} (no longer kept)", c.record));
    }
    out.push_str("Nothing was sent anywhere. The record is the end of the kernel log at the crash.\n");
    out
}

pub fn main(json_out: bool) -> i32 {
    let path = std::env::var("RIME_CRASH_LAST").unwrap_or_else(|_| LAST.to_string());
    let crash = match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            eprintln!("rime crash: cannot read {path}: {e}");
            return 1;
        }
    };
    match crash {
        None => {
            if json_out {
                println!("{}", json!({ "crash": null }));
            } else {
                println!("No kernel crash recorded on this machine.");
            }
        }
        Some(c) => {
            let exists = !c.record.is_empty() && Path::new(&c.record).exists();
            if json_out {
                let v = json!({
                    "crash": {
                        "id": c.id, "kind": c.kind, "time": c.time, "kernel": c.kernel,
                        "reason": c.reason, "module": c.module, "record": c.record,
                        "record_exists": exists,
                    }
                });
                println!("{v}");
            } else {
                print!("{}", render(&c, exists));
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "id=17635\nkind=panic\ntime=2026-11-19T01:02:03Z\nkernel=7.2.11-cachyos1-rime1\n\
reason=Kernel panic - not syncing: softlockup: hung tasks\nmodule=rtl8xxxu\n\
record=/var/lib/systemd/pstore/17635/dmesg.txt\nboot_id=abc\n";

    #[test]
    fn a_summary_parses_into_its_fields() {
        let c = parse(SAMPLE).expect("a crash");
        assert_eq!(c.kind, "panic");
        assert_eq!(c.module, "rtl8xxxu");
        assert_eq!(c.reason, "Kernel panic - not syncing: softlockup: hung tasks");
        assert_eq!(c.record, "/var/lib/systemd/pstore/17635/dmesg.txt");
    }

    #[test]
    fn a_reason_with_an_equals_sign_is_kept_whole() {
        let c = parse("id=1\nreason=BUG: a=b c=d\n").expect("a crash");
        assert_eq!(c.reason, "BUG: a=b c=d");
    }

    #[test]
    fn no_id_is_no_crash() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("kind=panic\n"), None);
    }

    #[test]
    fn the_report_names_the_reason_the_driver_and_the_record() {
        let c = parse(SAMPLE).unwrap();
        let r = render(&c, true);
        assert!(r.starts_with("Last kernel crash (the machine restarted)"), "{r}");
        assert!(r.contains("Kernel panic - not syncing: softlockup: hung tasks"), "{r}");
        assert!(r.contains("rtl8xxxu (named by the backtrace)"), "{r}");
        assert!(r.contains("/var/lib/systemd/pstore/17635/dmesg.txt\n"), "{r}");
        assert!(r.contains("Nothing was sent anywhere"), "{r}");
    }

    #[test]
    fn an_aged_out_record_is_said_to_be_gone() {
        let c = parse(SAMPLE).unwrap();
        assert!(render(&c, false).contains("dmesg.txt (no longer kept)"));
    }

    #[test]
    fn an_oops_is_not_called_a_restart() {
        let c = parse("id=2\nkind=oops\nreason=BUG: kernel NULL pointer dereference\n").unwrap();
        let r = render(&c, false);
        assert!(r.starts_with("Last kernel error recorded"), "{r}");
        assert!(!r.contains("Driver"), "{r}");
    }
}
