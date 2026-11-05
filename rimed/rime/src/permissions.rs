//! `rime permissions` — what each application may touch, who enforces it, and
//! what taking it away would actually do.
//!
//! The model is in `rime-perm-core`; this is the part that reads the machine
//! and the part that writes back. Both halves are deliberately thin, and both
//! obey the same rule: **every answer comes from something that is actually
//! consulted at the moment of access**, and every write goes back to the same
//! place. Nothing here keeps a record of its own.
//!
//! ## What is read, and from where
//!
//! * The session's portal interfaces, by introspecting
//!   `org.freedesktop.portal.Desktop`. Never inferred from installed packages:
//!   `xdg-desktop-portal-gnome` is installed on every Rime machine and exports
//!   nothing at all in a Hyprland session, because `default=hyprland;gtk`
//!   never reaches it.
//! * The permission store, through `flatpak permission-list`, which is the
//!   `org.freedesktop.impl.portal.PermissionStore` interface with a CLI on it.
//! * Each application's `[Context]`, from `flatpak info --show-permissions`,
//!   merged with the user and system override files.
//! * The camera and audio device nodes, and whether this user can open them —
//!   `access(2)`, which answers through the POSIX ACL rather than around it.
//!
//! ## The one test-only lever, named as one
//!
//! `RIME_PERM_DEV_ROOT` replaces `/dev` as the directory the camera and audio
//! nodes are looked for in. It exists because the native row's whole subject is
//! a device node, so a machine with no webcam — every CI runner — produces
//! `NoPrimitive` and a test asserting the ACL sentence passes or fails on
//! whether the developer's laptop has a camera. A fixture makes that assertion
//! mean what it says. It is read once, in [`probe_session`], and changes only
//! where the nodes are looked for; every other answer on the row is derived the
//! same way it always is.
//!
//! ## The one heuristic, named as one
//!
//! Whether the compositor hands out a screen-copy protocol to any client that
//! asks is established by looking for the global's name inside the running
//! compositor's own binary. That is an advertisement check, not a bind: it
//! proves the compositor was built with the protocol, not that an arbitrary
//! client would be given it today. It is reported as a caveat on the row and
//! never as a grant, which is the direction an unverified check is allowed to
//! move a report in.

use std::collections::BTreeSet;
use std::ffi::CString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use rime_perm_core::context::{Context, Merged};
use rime_perm_core::model::{Capability, GrantOrigin, Grant, Revocation, Subject};
use rime_perm_core::report::{flatpak_grants, native_grants, Session, Store};

#[derive(clap::Subcommand, Debug)]
pub enum PermCmd {
    /// Every application, every capability, with the enforcer beside it.
    List {
        /// Machine-readable, for Settings.
        #[arg(long)]
        json: bool,
    },
    /// One application.
    Show {
        /// A Flatpak application id, or `native:<name>` for anything else.
        app: String,
        #[arg(long)]
        json: bool,
    },
    /// Take a capability away, or say precisely why it cannot be taken.
    ///
    /// Exits non-zero when there is nothing to revoke, and prints the reason
    /// rather than a generic failure — "it cannot be revoked" is the answer,
    /// not an error in producing one.
    Revoke {
        app: String,
        /// One of the names `rime permissions list --json` prints.
        capability: String,
        /// Forget the answer instead of refusing: the application is asked
        /// again next time, rather than silently denied.
        #[arg(long)]
        forget: bool,
        /// Print the command that would run, and run nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

pub fn main(cmd: Option<PermCmd>) -> i32 {
    match cmd.unwrap_or(PermCmd::List { json: false }) {
        PermCmd::List { json } => list(json),
        PermCmd::Show { app, json } => show(&app, json),
        PermCmd::Revoke {
            app,
            capability,
            forget,
            dry_run,
        } => revoke(&app, &capability, forget, dry_run),
    }
}

// ═════════════════════════════════════════════════════════════════════════════
//  reading the machine
// ═════════════════════════════════════════════════════════════════════════════

fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether this process could open the node for reading.
///
/// `access(2)` consults the POSIX ACL, which is the thing that actually grants
/// a native binary the camera: the node is `root:video 0660` and the user is
/// not in `video`. Checking group membership instead would answer "no" on a
/// machine where the camera plainly works.
fn can_open(path: &Path) -> bool {
    let Ok(c) = CString::new(path.as_os_str().to_string_lossy().as_bytes()) else {
        return false;
    };
    // SAFETY: `c` is a valid NUL-terminated C string for the duration of the
    // call, and access(2) has no other preconditions.
    unsafe { libc::access(c.as_ptr(), libc::R_OK) == 0 }
}

fn first_matching(dir: &str, pred: impl Fn(&str) -> bool) -> Option<PathBuf> {
    let mut hits: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(&pred)
                .unwrap_or(false)
        })
        .collect();
    hits.sort();
    hits.into_iter().next()
}

/// Look for a Wayland global's name inside the running compositor's binary.
///
/// An advertisement check, not a bind — see the module docs. Reads the file in
/// chunks so a 100 MB compositor does not become 100 MB of resident memory in
/// a CLI that prints a table.
fn binary_advertises(path: &Path, needles: &[&str]) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 1 << 20];
    let mut carry: Vec<u8> = Vec::new();
    let longest = needles.iter().map(|n| n.len()).max().unwrap_or(0);
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        let mut window = carry.clone();
        window.extend_from_slice(&buf[..n]);
        for needle in needles {
            if window
                .windows(needle.len())
                .any(|w| w == needle.as_bytes())
            {
                return Some((*needle).to_string());
            }
        }
        let keep = window.len().saturating_sub(longest);
        carry = window[keep..].to_vec();
    }
}

fn compositor_binary() -> Option<PathBuf> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let name = desktop.split(':').next().unwrap_or("");
    let candidates = match name.to_ascii_lowercase().as_str() {
        "hyprland" => ["Hyprland", "hyprland"].as_slice(),
        "niri" => ["niri"].as_slice(),
        "labwc" => ["labwc"].as_slice(),
        _ => return None,
    };
    candidates
        .iter()
        .map(|c| PathBuf::from(rime_paths::SYSTEM_BIN_DIR).join(c))
        .find(|p| p.exists())
}

/// Everything the rows are derived from.
pub fn probe_session() -> Session {
    let introspect = run(
        "busctl",
        &[
            "--user",
            "introspect",
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
        ],
    )
    .unwrap_or_default();
    let portal_interfaces: BTreeSet<String> = introspect
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|w| w.starts_with("org.freedesktop.portal."))
        .map(str::to_string)
        .collect();

    let dev = std::env::var("RIME_PERM_DEV_ROOT").unwrap_or_else(|_| "/dev".into());
    let camera_node = first_matching(&dev, |n| {
        n.starts_with("video") && n[5..].chars().all(|c| c.is_ascii_digit())
    });
    let audio_capture_node = first_matching(&format!("{dev}/snd"), |n| {
        n.starts_with("pcmC") && n.ends_with('c')
    });

    // The ACL question is asked of whichever node exists. `false` when there
    // is no node at all is right: nothing to be granted.
    let seat_acl = camera_node
        .as_deref()
        .or(audio_capture_node.as_deref())
        .map(can_open)
        .unwrap_or(false);

    let unrestricted_screencopy = compositor_binary().and_then(|p| {
        binary_advertises(
            &p,
            &[
                "zwlr_screencopy_manager_v1",
                "ext_image_copy_capture_manager_v1",
                "hyprland_toplevel_export_manager_v1",
            ],
        )
    });

    Session {
        desktop: std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
        portal_interfaces,
        unrestricted_screencopy,
        camera_node: camera_node.map(|p| p.display().to_string()),
        audio_capture_node: audio_capture_node.map(|p| p.display().to_string()),
        seat_acl,
    }
}

fn probe_store() -> Store {
    Store::parse(&run("flatpak", &["permission-list"]).unwrap_or_default())
}

fn installed_flatpaks() -> Vec<String> {
    run(
        "flatpak",
        &["list", "--app", "--columns=application"],
    )
    .unwrap_or_default()
    .lines()
    .map(str::trim)
    .filter(|l| !l.is_empty() && l.contains('.'))
    .map(str::to_string)
    .collect()
}

fn override_paths(app: &str) -> [(PathBuf, GrantOrigin); 2] {
    let home = std::env::var("HOME").unwrap_or_default();
    let user = PathBuf::from(&home)
        .join(".local/share/flatpak/overrides")
        .join(app);
    let system = PathBuf::from("/var/lib/flatpak/overrides").join(app);
    [
        (
            system.clone(),
            GrantOrigin::SystemOverride {
                path: system.display().to_string(),
            },
        ),
        (
            user.clone(),
            GrantOrigin::UserOverride {
                path: user.display().to_string(),
            },
        ),
    ]
}

fn merged_context(app: &str) -> Merged {
    let manifest = run("flatpak", &["info", "--show-permissions", app]).unwrap_or_default();
    let mut merged = Merged::from_manifest(&Context::parse(&manifest));
    // System first, then user: the user's own file is the last word, and the
    // owner is entitled to see their own name on a permission they changed.
    for (path, origin) in override_paths(app) {
        if let Ok(text) = fs::read_to_string(&path) {
            merged.apply_override(&Context::parse(&text), origin);
        }
    }
    merged
}

fn rows_for(app: &str, store: &Store, session: &Session) -> Vec<Grant> {
    match app.strip_prefix("native:") {
        Some(name) => native_grants(name, session),
        None => flatpak_grants(app, &merged_context(app), store, session),
    }
}

// ═════════════════════════════════════════════════════════════════════════════
//  output
// ═════════════════════════════════════════════════════════════════════════════

fn session_json(session: &Session) -> serde_json::Value {
    serde_json::json!({
        "desktop": session.desktop,
        "portal_interfaces": session.portal_interfaces.iter().collect::<Vec<_>>(),
        "unrestricted_screencopy": session.unrestricted_screencopy,
        "camera_node": session.camera_node,
        "audio_capture_node": session.audio_capture_node,
        "seat_acl": session.seat_acl,
    })
}

fn grant_json(g: &Grant) -> serde_json::Value {
    let mut v = serde_json::json!({
        "app": g.subject.id(),
        // Whether the subject is a Flatpak is NOT how enforcement is decided —
        // see rime-perm-core's model docs — but the page still has to know,
        // because the token `revoke` takes differs and because a reader is
        // entitled to see which kind of application a row is about. Sniffing
        // it out of the display id in the consumer is how that becomes wrong
        // the first time somebody installs something called `native:thing`.
        "native": matches!(g.subject, Subject::Native { .. }),
        // Exactly what to pass back as `rime permissions revoke <this> …`.
        "revoke_id": match &g.subject {
            Subject::Flatpak { app_id } => app_id.clone(),
            Subject::Native { id } => format!("native:{id}"),
        },
        "subject": g.subject,
        "capability": g.capability.tag(),
        "capability_label": g.capability.label(),
        "state": g.state.tag(),
        "enforcer": g.enforcer.tag(),
        "enforcer_detail": g.enforcer,
        "enforcer_why": g.enforcer.why(),
        "origin": g.origin.tag(),
        "origin_label": g.origin.label(),
        "revocation": g.revocation.tag(),
        "revocation_detail": g.revocation,
        "timing": g.revocation.timing().tag(),
        "timing_label": g.revocation.timing().label(),
        "offers_control": g.revocation.offers_a_control(),
        "effective": g.effective(),
        "headline": g.headline(),
    });
    if let Some(c) = &g.caveat {
        v["caveat"] = serde_json::Value::String(c.clone());
    }
    v
}

fn print_rows(rows: &[Grant]) {
    for g in rows {
        println!(
            "  {:<24} {:<34} {}",
            g.capability.label(),
            g.headline(),
            g.origin.label()
        );
        // The caveat is the more specific sentence where there is one, and
        // printing both says the same thing twice in a page whose whole job is
        // to be read.
        match (&g.revocation, &g.caveat) {
            (_, Some(c)) => println!("  {:<24} └ {c}", ""),
            (Revocation::Unsupported { why }, None) => println!("  {:<24} └ {why}", ""),
            _ => {}
        }
    }
}

fn list(json: bool) -> i32 {
    let session = probe_session();
    let store = probe_store();
    let mut apps = installed_flatpaks();
    // Anything the store has heard of but that is no longer installed still
    // holds a grant, and leaving it out would be the one omission this whole
    // feature exists to prevent.
    for a in store.apps() {
        if !apps.contains(&a) {
            apps.push(a);
        }
    }
    apps.sort();
    apps.dedup();

    let mut all: Vec<Grant> = Vec::new();
    for app in &apps {
        all.extend(rows_for(app, &store, &session));
    }
    all.extend(native_grants("any native application", &session));

    if json {
        let doc = serde_json::json!({
            "session": session_json(&session),
            "apps": apps,
            "grants": all.iter().map(grant_json).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
        return 0;
    }

    println!("session: {}", session.desktop);
    println!(
        "portals brokering anything here: {}",
        if session.portal_interfaces.is_empty() {
            "none — could not reach xdg-desktop-portal".to_string()
        } else {
            Capability::ALL
                .iter()
                .filter(|c| session.brokers(**c))
                .map(|c| c.label())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    println!();
    for app in &apps {
        println!("{app}");
        print_rows(&rows_for(app, &store, &session));
        println!();
    }
    println!("any native application");
    print_rows(&native_grants("any native application", &session));
    0
}

fn show(app: &str, json: bool) -> i32 {
    let session = probe_session();
    let store = probe_store();
    let rows = rows_for(app, &store, &session);
    if json {
        let doc = serde_json::json!({
            "session": session_json(&session),
            "grants": rows.iter().map(grant_json).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
    } else {
        println!("{app}");
        print_rows(&rows);
    }
    0
}

// ═════════════════════════════════════════════════════════════════════════════
//  writing back
// ═════════════════════════════════════════════════════════════════════════════

fn capability_by_tag(tag: &str) -> Option<Capability> {
    Capability::ALL.iter().copied().find(|c| c.tag() == tag)
}

fn revoke(app: &str, capability: &str, forget: bool, dry_run: bool) -> i32 {
    let Some(cap) = capability_by_tag(capability) else {
        eprintln!(
            "unknown capability {capability}. One of: {}",
            Capability::ALL
                .iter()
                .map(|c| c.tag())
                .collect::<Vec<_>>()
                .join(", ")
        );
        return 2;
    };
    let session = probe_session();
    let store = probe_store();
    let rows = rows_for(app, &store, &session);
    let Some(g) = rows.iter().find(|g| g.capability == cap) else {
        eprintln!("no {capability} row for {app}");
        return 2;
    };

    // The refusals come first, and they exit non-zero with the reason. The
    // caller asked for something the machine cannot do; saying so plainly is
    // the answer, and pretending otherwise is the defect this item is about.
    match &g.revocation {
        Revocation::NoPrimitive => {
            eprintln!(
                "{app}: {} is not brokered in this session, so there is no permission to revoke.",
                cap.label()
            );
            eprintln!("  {}", g.enforcer.why());
            return 1;
        }
        Revocation::Unsupported { why } => {
            eprintln!("{app}: {} cannot be revoked for one app.", cap.label());
            eprintln!("  {why}");
            return 1;
        }
        _ => {}
    }

    let app_id = match &g.subject {
        Subject::Flatpak { app_id } => app_id.clone(),
        Subject::Native { .. } => {
            // Unreachable through the match above — a native row can only
            // produce Unsupported — but written out rather than unwrapped, so
            // that a future enforcer variant cannot quietly reach a store
            // write with no application id to write it under.
            eprintln!("{app}: nothing per-application to change.");
            return 1;
        }
    };

    let argv: Vec<String> = match (&g.revocation, forget) {
        (Revocation::StoreDeny { table, id }, false) => vec![
            "permission-set".into(),
            (*table).to_string(),
            (*id).to_string(),
            app_id,
            "no".into(),
        ],
        (Revocation::StoreDeny { table, id }, true)
        | (Revocation::StoreForget { table, id }, _) => vec![
            "permission-remove".into(),
            (*table).to_string(),
            (*id).to_string(),
            app_id,
        ],
        (Revocation::ContextEdit { key }, _) => {
            let Some((list, item)) = key.split_once('=') else {
                eprintln!("{app}: cannot parse the sandbox key {key}");
                return 2;
            };
            let flag = match list {
                "shared" => format!("--unshare={item}"),
                "sockets" => format!("--nosocket={item}"),
                "devices" => format!("--nodevice={item}"),
                "filesystems" => format!("--nofilesystem={item}"),
                other => {
                    eprintln!("{app}: no override flag for {other}");
                    return 2;
                }
            };
            vec!["override".into(), "--user".into(), flag, app_id]
        }
        _ => {
            eprintln!("{app}: nothing to do");
            return 1;
        }
    };

    let printable: Vec<&str> = argv.iter().map(String::as_str).collect();
    if dry_run {
        println!("flatpak {}", printable.join(" "));
        println!("{}", g.revocation.timing().label());
        return 0;
    }
    match Command::new("flatpak").args(&printable).status() {
        Ok(s) if s.success() => {
            println!(
                "{app}: {} revoked — {}",
                cap.label(),
                g.revocation.timing().label()
            );
            0
        }
        Ok(s) => {
            eprintln!("flatpak {} exited {}", printable.join(" "), s);
            1
        }
        Err(e) => {
            eprintln!("could not run flatpak: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capability_tag_round_trips() {
        for c in Capability::ALL {
            assert_eq!(capability_by_tag(c.tag()), Some(c));
        }
        assert_eq!(capability_by_tag("webcam"), None);
    }

    /// The scanner has to find a needle that straddles two reads, or a
    /// compositor whose protocol name happens to land on a 1 MiB boundary
    /// would be reported as not offering it — and the row would silently
    /// become "brokered".
    #[test]
    fn the_binary_scan_sees_across_its_own_chunk_boundary() {
        let dir = std::env::temp_dir().join(format!("rime-perm-scan-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("blob");
        let needle = "zwlr_screencopy_manager_v1";
        let mut blob = vec![b'.'; (1 << 20) - 6];
        blob.extend_from_slice(needle.as_bytes());
        blob.extend(vec![b'.'; 4096]);
        fs::write(&path, &blob).expect("write");
        assert_eq!(
            binary_advertises(&path, &[needle]),
            Some(needle.to_string())
        );
        fs::write(&path, vec![b'.'; 1 << 21]).expect("write");
        assert_eq!(binary_advertises(&path, &[needle]), None);
        let _ = fs::remove_dir_all(&dir);
    }
}
