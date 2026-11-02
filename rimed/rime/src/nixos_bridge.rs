//! The bootc side of the move to Rime OS on NixOS (PLAN.md D1, D2):
//! `rime migrate`, and the refusals that hold the old system still once
//! `rime migrate start` has run.
//!
//! ## `rime migrate`
//!
//! The bridge itself is one Python program, `/usr/libexec/rime-migrate`
//! (standard library only, Fedora's own python). This CLI execs it with the
//! arguments it was given and adds one gate of its own in front of `start`:
//! the payload manifest must carry an ed25519 signature by the key this image
//! ships at [`PAYLOAD_KEY`]. The program's own `verify_payload` checks the
//! sha256 of every payload file against that manifest, so a signed manifest
//! covers the whole payload.
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
//! `RIME_BRIDGE_ROOT` relocates the files this module READS (the record and
//! the payload key); no program path comes from the environment.

use std::io::ErrorKind;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// The bridge program. A constant, like every engine path in `ops`.
pub const MIGRATE_ENGINE: &str = "/usr/libexec/rime-migrate";

/// What `rime migrate start` writes at every step (`STATE` in rime-migrate).
pub const BRIDGE_STATE: &str = "/var/lib/rime/nixos-bridge/state.json";

/// The public half of the key that signs bridge payload manifests.
///
/// Nix's key format, one line: `<name>:<base64 of the 32-byte ed25519 key>`,
/// the same shape as the binary cache's `rime-cache-1:…`. The image ships no
/// such file until the signing key is decided; until then `start` is refused
/// by name (`payload-key-missing`) rather than installing an unsigned payload.
pub const PAYLOAD_KEY: &str = "/usr/share/rime-os/trust/nixos-bridge-payload.pub";

/// The signature beside the manifest: `<key name>:<base64 of the 64-byte
/// ed25519 signature>` over the exact bytes of `manifest.json`.
pub const PAYLOAD_SIG: &str = "manifest.json.sig";

/// openssl does the ed25519 arithmetic, as it does for every signature
/// `verify.rs` checks. A fixed absolute path, never one from the environment.
const OPENSSL: &str = "/usr/bin/openssl";

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

// ── the payload signature ────────────────────────────────────────────────────

/// A refusal of `start`, by name, as the bridge prints them.
#[derive(Debug, PartialEq, Eq)]
pub struct PayloadRefusal {
    pub name: &'static str,
    pub text: String,
}

fn refuse(name: &'static str, text: String) -> PayloadRefusal {
    PayloadRefusal { name, text }
}

/// `<name>:<base64>` decoded to its name and exactly `len` bytes.
fn nix_style(text: &str, len: usize) -> Option<(String, Vec<u8>)> {
    let line = text.trim();
    if line.lines().count() != 1 {
        return None;
    }
    let (name, b64) = line.split_once(':')?;
    if name.is_empty() || name.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    let bytes = crate::verify::b64_decode(b64)?;
    if bytes.len() == len {
        Some((name.to_string(), bytes))
    } else {
        None
    }
}

/// The DER SubjectPublicKeyInfo of a raw ed25519 public key (RFC 8410).
fn ed25519_spki_der(key: &[u8]) -> Vec<u8> {
    let mut der = vec![
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    der.extend_from_slice(key);
    der
}

/// A fresh private directory for openssl's inputs (public data only; created,
/// never reused, so a planted path cannot stand in for it).
fn scratch_dir() -> Result<PathBuf, String> {
    use std::os::unix::fs::DirBuilderExt;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "rime-migrate-verify.{}.{nanos}",
        std::process::id()
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

/// Check `payload/manifest.json` against its signature and the key file.
/// `Ok(key name)` when it verifies; a named refusal otherwise.
pub fn verify_payload_signature(
    openssl: &Path,
    key_file: &Path,
    payload: &Path,
) -> Result<String, PayloadRefusal> {
    let key_text = match std::fs::read_to_string(key_file) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Err(refuse(
                "payload-key-missing",
                format!(
                    "this system has no payload signing key ({}), so no payload can be checked; \
                     start waits for the Rime OS update that ships it",
                    key_file.display()
                ),
            ))
        }
        Err(e) => {
            return Err(refuse(
                "payload-key-missing",
                format!("{}: {e}", key_file.display()),
            ))
        }
    };
    let (key_name, key) = nix_style(&key_text, 32).ok_or_else(|| {
        refuse(
            "payload-key-bad",
            format!(
                "{} is not one `<name>:<base64 ed25519 public key>` line",
                key_file.display()
            ),
        )
    })?;
    let manifest = payload.join("manifest.json");
    let sig_path = payload.join(PAYLOAD_SIG);
    let sig_text = std::fs::read_to_string(&sig_path).map_err(|e| {
        refuse(
            "payload-unsigned",
            format!("{}: {e}; an unsigned payload is never installed", sig_path.display()),
        )
    })?;
    let (sig_name, sig) = nix_style(&sig_text, 64).ok_or_else(|| {
        refuse(
            "payload-signature-bad",
            format!(
                "{} is not one `<key name>:<base64 ed25519 signature>` line",
                sig_path.display()
            ),
        )
    })?;
    if sig_name != key_name {
        return Err(refuse(
            "payload-signature-bad",
            format!("the payload is signed by key {sig_name}; this system trusts {key_name}"),
        ));
    }
    if !manifest.is_file() {
        return Err(refuse(
            "payload-signature-bad",
            format!("{}: no such file", manifest.display()),
        ));
    }
    let dir = scratch_dir().map_err(|e| refuse("payload-unverifiable", e))?;
    let result = openssl_verify(openssl, &dir, &key_name, &key, &sig, &manifest);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// The ed25519 check itself, with its inputs written into `dir`.
fn openssl_verify(
    openssl: &Path,
    dir: &Path,
    key_name: &str,
    key: &[u8],
    sig: &[u8],
    manifest: &Path,
) -> Result<String, PayloadRefusal> {
    let der = dir.join("key.der");
    let raw = dir.join("sig.bin");
    std::fs::write(&der, ed25519_spki_der(key))
        .and_then(|_| std::fs::write(&raw, sig))
        .map_err(|e| refuse("payload-unverifiable", e.to_string()))?;
    let out = Command::new(openssl)
        .args(["pkeyutl", "-verify", "-pubin", "-keyform", "DER", "-rawin", "-inkey"])
        .arg(&der)
        .arg("-in")
        .arg(manifest)
        .arg("-sigfile")
        .arg(&raw)
        .output()
        .map_err(|e| {
            refuse(
                "payload-unverifiable",
                format!("could not run {}: {e}", openssl.display()),
            )
        })?;
    if out.status.success() {
        return Ok(key_name.to_string());
    }
    let why = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Err(refuse(
        "payload-signature-bad",
        format!(
            "the manifest's signature does not verify against {key_name} ({})",
            if why.is_empty() {
                "openssl refused it".to_string()
            } else {
                why
            }
        ),
    ))
}

/// The value of `--payload` in `start`'s arguments, either spelling.
pub fn payload_arg(args: &[String]) -> Option<PathBuf> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--payload" {
            return it.next().map(PathBuf::from);
        }
        if let Some(v) = a.strip_prefix("--payload=") {
            return Some(PathBuf::from(v));
        }
    }
    None
}

/// `rime migrate …`: the signature gate for `start`, then the bridge itself.
pub fn migrate(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("start") {
        // No --payload: the program's own argument parser says so.
        if let Some(payload) = payload_arg(&args[1..]) {
            match verify_payload_signature(Path::new(OPENSSL), &relocated(PAYLOAD_KEY), &payload) {
                Ok(name) => eprintln!("rime: payload manifest signed by {name}: verified"),
                Err(r) => {
                    println!("REFUSED [{}] {}", r.name, r.text);
                    return EXIT_REFUSED;
                }
            }
        }
    }
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

    #[test]
    fn payload_arg_both_spellings() {
        let v = |a: &[&str]| payload_arg(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(v(&["--backup", "/b", "--payload", "/p", "--yes"]), Some("/p".into()));
        assert_eq!(v(&["--payload=/q"]), Some("/q".into()));
        assert_eq!(v(&["--backup", "/b"]), None);
        assert_eq!(v(&["--payload"]), None);
    }

    #[test]
    fn nix_style_lines() {
        assert!(nix_style("rime-cache-1:LXocssi9/mdVEgEUlO+thxyiumct4b+HfqW2nIFmX1k=\n", 32).is_some());
        assert!(nix_style("rime-cache-1:LXocssi9/mdVEgEUlO+thxyiumct4b+HfqW2nIFmX1k=", 64).is_none());
        assert!(nix_style(":LXocssi9/mdVEgEUlO+thxyiumct4b+HfqW2nIFmX1k=", 32).is_none());
        assert!(nix_style("LXocssi9/mdVEgEUlO+thxyiumct4b+HfqW2nIFmX1k=", 32).is_none());
        assert!(nix_style("a:LXocssi9/mdVEgEUlO+thxyiumct4b+HfqW2nIFmX1k=\nb:x", 32).is_none());
    }

    // RFC 8032 §7.1 TEST 2: one-byte message 0x72.
    const RFC_PUB: &str = "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c";
    const RFC_SIG: &str = "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da\
                           085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00";

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn b64(bytes: &[u8]) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    /// openssl from PATH: the CI runner has it at /usr/bin, a Nix shell does not.
    fn openssl() -> PathBuf {
        let path = std::env::var_os("PATH").expect("PATH");
        std::env::split_paths(&path)
            .map(|d| d.join("openssl"))
            .find(|p| p.is_file())
            .expect("these tests need openssl on PATH (the CI runner has it)")
    }

    struct Lab {
        dir: PathBuf,
    }

    impl Lab {
        fn new(tag: &str, manifest: &[u8], sig: Option<String>, key: Option<String>) -> Lab {
            let dir = std::env::temp_dir().join(format!(
                "rime-bridge-sig.{tag}.{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("payload")).unwrap();
            std::fs::write(dir.join("payload/manifest.json"), manifest).unwrap();
            if let Some(s) = sig {
                std::fs::write(dir.join("payload").join(PAYLOAD_SIG), s).unwrap();
            }
            if let Some(k) = key {
                std::fs::write(dir.join("key.pub"), k).unwrap();
            }
            Lab { dir }
        }
        fn check(&self) -> Result<String, PayloadRefusal> {
            verify_payload_signature(&openssl(), &self.dir.join("key.pub"), &self.dir.join("payload"))
        }
    }

    impl Drop for Lab {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn key_line() -> String {
        format!("rime-test-bridge-1:{}\n", b64(&hex(RFC_PUB)))
    }

    fn sig_line(name: &str) -> String {
        format!("{name}:{}\n", b64(&hex(RFC_SIG)))
    }

    #[test]
    fn a_good_signature_verifies() {
        let lab = Lab::new("good", &[0x72], Some(sig_line("rime-test-bridge-1")), Some(key_line()));
        assert_eq!(lab.check(), Ok("rime-test-bridge-1".to_string()));
    }

    #[test]
    fn a_changed_manifest_is_refused() {
        let lab = Lab::new("tampered", &[0x73], Some(sig_line("rime-test-bridge-1")), Some(key_line()));
        assert_eq!(lab.check().unwrap_err().name, "payload-signature-bad");
    }

    #[test]
    fn unsigned_unkeyed_and_wrong_key_are_refused_by_name() {
        let unsigned = Lab::new("unsigned", &[0x72], None, Some(key_line()));
        assert_eq!(unsigned.check().unwrap_err().name, "payload-unsigned");
        let unkeyed = Lab::new("unkeyed", &[0x72], Some(sig_line("rime-test-bridge-1")), None);
        assert_eq!(unkeyed.check().unwrap_err().name, "payload-key-missing");
        let other = Lab::new("other", &[0x72], Some(sig_line("someone-else-1")), Some(key_line()));
        assert_eq!(other.check().unwrap_err().name, "payload-signature-bad");
        let badkey = Lab::new("badkey", &[0x72], Some(sig_line("rime-test-bridge-1")), Some("x\n".into()));
        assert_eq!(badkey.check().unwrap_err().name, "payload-key-bad");
    }
}
