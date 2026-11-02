//! The bootc side of the move to Rime OS on NixOS (PLAN.md D1, D2):
//! `rime migrate`, and the refusals that hold the old system still once
//! `rime migrate start` has run.
//!
//! ## `rime migrate`
//!
//! The bridge itself is one Python program, `/usr/libexec/rime-migrate`
//! (standard library only, Fedora's own python). This CLI execs it with the
//! arguments it was given. The bridge checks the payload itself, where it
//! reads it: `manifest.sig` (ed25519, the `rime-cache-1` key the NixOS update
//! path already trusts, compiled into the bridge as `TRUSTED_PAYLOAD_KEYS`)
//! over the manifest, and the sha256 of every file the manifest names.
//! tests/test-rime-migrate.sh pins that trust root.
//!
//! ## The refusals
//!
//! `rime migrate start` writes `/var/lib/rime/nixos-bridge/state.json` at its
//! first step and rewrites it at every step after that. From then on, every
//! verb that drives bootc or ostree on the old side is refused by name:
//! `update` (every form, `--check` included), `rollback`, `pin`, `channel set`,
//! `boot migrate`, and the live-update verbs that apply, swap or soft-reboot
//! into a staged deployment. The ostree entries are the way back to the old
//! system if the new one does not come up; an update, a rollback or a boot-path
//! migration on top of a finished move changes that way back, and an update to
//! the old side is never what the user wanted at that point.
//!
//! The record decides, read as follows (decided 2026-11-02, written here and in
//! docs/nixos-move.md):
//!
//! | `state.json` | answer |
//! |---|---|
//! | absent | allowed: the move never started |
//! | `"failed"` set AND the last step is `"rolled back"` | allowed: start removed what it made and unmasked the old side |
//! | `"done"` set | refused: the machine has moved |
//! | anything else that parses | refused: a start began and did not finish (a crash, a power cut, or one running now) |
//! | unreadable or not JSON, as root | refused: nobody knows which of the above it is |
//!
//! Unreadable as an ordinary user is not a refusal: the directory is 0700
//! root, so a non-root read always fails, and every verb gated here is also
//! root-only, so the root gate that runs next refuses it anyway.
//!
//! The gate runs in `main` before the root gate and before any D-Bus connect
//! or subprocess, which is also what lets the fixture suite
//! (tests/test-rime-migrate.sh) drive the real binary without root.
//! `RIME_BRIDGE_ROOT` relocates the one file this module READS (the record);
//! no program path comes from the environment.

use std::io::ErrorKind;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// The bridge program. A constant, like every engine path in `ops`.
pub const MIGRATE_ENGINE: &str = "/usr/libexec/rime-migrate";

/// What `rime migrate start` writes at every step (`STATE` in rime-migrate).
pub const BRIDGE_STATE: &str = "/var/lib/rime/nixos-bridge/state.json";

/// The refusal name every gated verb prints.
pub const REFUSAL: &str = "nixos-migration-started";

/// Exit code of a refusal: the bridge's own `EXIT_REFUSED`.
pub const EXIT_REFUSED: i32 = 3;

/// What `state.json` says about the move.
#[derive(Debug, PartialEq, Eq)]
pub enum Marker {
    /// No record: `start` never ran here.
    Absent,
    /// `start` failed and removed what it made.
    RolledBack { started: String },
    /// `start` ran to the end.
    Done { started: String, done: String },
    /// `start` began and did not finish.
    Unfinished { started: String, last_step: String },
    /// The record exists and could not be read or understood.
    Unreadable { why: String },
    /// The record could not be read because this process is not root.
    NotRoot,
}

fn relocated(absolute: &str) -> PathBuf {
    match std::env::var_os("RIME_BRIDGE_ROOT") {
        // `absolute` starts with '/'; without the strip `join` would read the
        // real machine (see trust::Roots::path).
        Some(root) => PathBuf::from(root).join(absolute.trim_start_matches('/')),
        None => PathBuf::from(absolute),
    }
}

fn text_of(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => "unknown".to_string(),
        Some(other) => other.to_string(),
    }
}

/// The record's text, classified. Pure: the table in the module doc.
pub fn classify(text: &str) -> Marker {
    let v: Value = match serde_json::from_str(text) {
        Ok(v @ Value::Object(_)) => v,
        Ok(_) => {
            return Marker::Unreadable {
                why: "the record is not a JSON object".to_string(),
            }
        }
        Err(e) => {
            return Marker::Unreadable {
                why: format!("the record is not JSON ({e})"),
            }
        }
    };
    let started = text_of(&v, "started");
    let last_step = v
        .get("steps")
        .and_then(Value::as_array)
        .and_then(|s| s.last())
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if v.get("done").is_some_and(|d| !d.is_null()) {
        return Marker::Done {
            started,
            done: text_of(&v, "done"),
        };
    }
    // Both, not either: "failed" is written just before the "rolled back"
    // step, and a rollback that died in between has not restored the old side.
    if v.get("failed").is_some() && last_step == "rolled back" {
        return Marker::RolledBack { started };
    }
    Marker::Unfinished {
        started,
        last_step: if last_step.is_empty() {
            "none recorded".to_string()
        } else {
            last_step
        },
    }
}

/// The record at `path`, or what stopped it being read.
pub fn read_marker(path: &Path, is_root: bool) -> Marker {
    match std::fs::read_to_string(path) {
        Ok(text) => classify(&text),
        Err(e) if e.kind() == ErrorKind::NotFound => Marker::Absent,
        Err(e) if e.kind() == ErrorKind::PermissionDenied && !is_root => Marker::NotRoot,
        Err(e) => Marker::Unreadable {
            why: format!("{}: {e}", path.display()),
        },
    }
}

/// The refusal for `verb`, or `None` when the record allows it.
pub fn refusal(verb: &str, marker: &Marker, record: &Path) -> Option<String> {
    let what = match marker {
        Marker::Absent | Marker::RolledBack { .. } | Marker::NotRoot => return None,
        Marker::Done { started, done } => format!(
            "this machine has moved to Rime OS on NixOS: `rime migrate start` began {started}\n\
             and finished {done}. The old system takes no more updates, rollbacks or boot\n\
             changes: its ostree entries are your way back if the new system does not start,\n\
             and they stay exactly as the move left them.\n\
             Reboot and pick \"Rime (generation …)\" in the boot menu (the default). Updates\n\
             there are `sudo rime update`."
        ),
        Marker::Unfinished { started, last_step } => format!(
            "a move to Rime OS on NixOS began {started} and has not finished (last step:\n\
             {last_step}). Nothing on the old system changes until that is settled: a run that\n\
             fails removes what it made and then allows this again.\n\
             See what state the machine is in with `sudo rime migrate check --explain`."
        ),
        Marker::Unreadable { why } => format!(
            "this machine has a record of a move to Rime OS on NixOS that cannot be read\n\
             ({why}). Until it can, the old system is treated as moved.\n\
             See what state the machine is in with `sudo rime migrate check --explain`."
        ),
    };
    let body: String = what.lines().map(|l| format!("rime: {l}\n")).collect();
    Some(format!(
        "rime: '{verb}' REFUSED [{REFUSAL}]\n{body}rime: record: {}",
        record.display()
    ))
}

/// The gate `main` runs for every verb `bridged_verb` names.
pub fn guard(verb: &str) -> Result<(), i32> {
    let record = relocated(BRIDGE_STATE);
    let is_root = crate::ops::effective_uid() == Some(0);
    match refusal(verb, &read_marker(&record, is_root), &record) {
        None => Ok(()),
        Some(text) => {
            eprintln!("{text}");
            Err(EXIT_REFUSED)
        }
    }
}

// ── rime migrate ─────────────────────────────────────────────────────────────

/// `rime migrate …`: the bridge itself, with every argument unchanged.
///
/// No gate of its own: the bridge verifies the payload's signature where it
/// reads the payload (`verify_payload`: `manifest.sig` over
/// `rime-bridge-payload-v1\n` + `manifest.json`, by a key in its
/// `TRUSTED_PAYLOAD_KEYS`), so a second check here could only disagree with it
/// (decided 2026-11-02, docs/nixos-move.md).
pub fn migrate(args: &[String]) -> i32 {
    let err = Command::new(MIGRATE_ENGINE).args(args).exec();
    eprintln!("rime: cannot run the bridge ({MIGRATE_ENGINE}): {err}");
    eprintln!(
        "rime: no bridge on this system: it predates `rime migrate`.\n\
         \x20      run `sudo rime update` first."
    );
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec() -> PathBuf {
        PathBuf::from(BRIDGE_STATE)
    }

    #[test]
    fn absent_and_rolled_back_allow_everything() {
        assert_eq!(refusal("update", &Marker::Absent, &rec()), None);
        let rb = classify(
            r#"{"started":"2026-11-02T01:00:00+00:00","failed":"x","steps":["backup","old side","rolled back"]}"#,
        );
        assert_eq!(
            rb,
            Marker::RolledBack {
                started: "2026-11-02T01:00:00+00:00".into()
            }
        );
        assert_eq!(refusal("update", &rb, &rec()), None);
        assert_eq!(refusal("update", &Marker::NotRoot, &rec()), None);
    }

    #[test]
    fn done_refuses_by_name_and_points_at_the_new_entry() {
        let m = classify(
            r#"{"started":"S","done":"D","steps":["backup","done"],"topology":{"topology":"A"}}"#,
        );
        assert_eq!(
            m,
            Marker::Done {
                started: "S".into(),
                done: "D".into()
            }
        );
        let t = refusal("boot migrate", &m, &rec()).expect("refused");
        assert!(t.starts_with("rime: 'boot migrate' REFUSED [nixos-migration-started]\n"), "{t}");
        assert!(t.contains("Rime (generation"), "{t}");
        assert!(t.contains("began S"), "{t}");
        assert!(t.ends_with(BRIDGE_STATE), "{t}");
        assert!(t.lines().all(|l| l.starts_with("rime: ")), "{t}");
    }

    #[test]
    fn failed_without_the_rolled_back_step_still_refuses() {
        // "failed" lands before the "rolled back" step: a crash in between
        // has not unmasked the old side.
        let m = classify(r#"{"started":"S","failed":"boom","steps":["backup","old side"]}"#);
        assert!(matches!(m, Marker::Unfinished { .. }), "{m:?}");
        assert!(refusal("update", &m, &rec()).is_some());
    }

    #[test]
    fn rolled_back_step_without_failed_still_refuses() {
        let m = classify(r#"{"started":"S","steps":["backup","rolled back"]}"#);
        assert!(matches!(m, Marker::Unfinished { .. }), "{m:?}");
    }

    #[test]
    fn a_start_in_progress_refuses_and_names_the_step() {
        let m = classify(r#"{"started":"S","steps":["backup","old side","install closure"]}"#);
        assert_eq!(
            m,
            Marker::Unfinished {
                started: "S".into(),
                last_step: "install closure".into()
            }
        );
        let t = refusal("update", &m, &rec()).unwrap();
        assert!(t.contains("install closure"), "{t}");
        assert!(t.contains("rime migrate check --explain"), "{t}");
    }

    #[test]
    fn an_unreadable_record_fails_closed() {
        for text in ["", "{\"started\": \"S\", \"steps\": [\"bac", "[]", "null"] {
            let m = classify(text);
            assert!(matches!(m, Marker::Unreadable { .. }), "{text:?} -> {m:?}");
            assert!(refusal("rollback", &m, &rec()).is_some(), "{text:?}");
        }
    }

    #[test]
    fn read_marker_absent_vs_denied() {
        let dir = std::env::temp_dir().join(format!("rime-bridge-test.{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_marker(&dir.join("nope.json"), true), Marker::Absent);
        let p = dir.join("state.json");
        std::fs::write(&p, r#"{"started":"S","done":"D","steps":["done"]}"#).unwrap();
        assert!(matches!(read_marker(&p, false), Marker::Done { .. }));
        // A directory where the file should be: unreadable, and so refused.
        let d = dir.join("dir.json");
        std::fs::create_dir_all(&d).unwrap();
        assert!(matches!(read_marker(&d, true), Marker::Unreadable { .. }));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
