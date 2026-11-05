//! `org.rimeos.Rimed1.Updates` (docs/nixos/desktops.md §6.2): what the update engine is doing, for a desktop's own
//! panel (GNOME, KDE, COSMIC) that has no Rime Shell to read the files.
//!
//! rimed only reads. `rime update --check` (rime-update-check.service) writes [`CHECK_STATE`]; `rime update` writes
//! [`LIVE_STATUS`] (rimed/rime/src/nixos/mod.rs). rimed watches both directories with inotify, derives one [`View`],
//! and emits `PropertiesChanged` when it changes. `Check()` and `Apply()` start the two units; the work runs there,
//! never inside rimed.

use std::collections::HashMap;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde_json::Value as Json;
use zbus::message::Header;
use zbus::{interface, Connection, SignalContext};

use crate::dbus::OBJECT_PATH;
use crate::polkit::{authorize, authorize_interactive};

pub const CHECK_STATE: &str = "/run/rime-update/state";
pub const LIVE_STATUS: &str = "/run/rime-live/status.json";
/// `Check()`: allow_active yes, like the timer it runs early.
pub const ACTION_CHECK: &str = "org.rimeos.rimed.check-update";
/// `Apply()`: auth_admin_keep, the same as typing `sudo rime update`.
pub const ACTION_APPLY: &str = "org.rimeos.rimed.apply-update";
pub const CHECK_UNIT: &str = "rime-update-check.service";
pub const APPLY_UNIT: &str = "rime-update.service";

/// What the four properties say.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct View {
    /// `up-to-date|available|downloading|installing|deferred|restart|failed|unknown`.
    pub state: String,
    /// The release on offer, or "".
    pub available: String,
    /// The release this machine runs, or "" when nothing says.
    pub running: String,
    /// One sentence for a panel, or "".
    pub message: String,
}

/// `key=value` lines (the checker's file).
fn kv(text: &str) -> HashMap<&str, &str> {
    text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.trim(), v.trim())).collect()
}

fn s<'a>(v: &'a Json, path: &[&str]) -> &'a str {
    let mut cur = v;
    for p in path {
        cur = &cur[*p];
    }
    cur.as_str().unwrap_or("")
}

/// The restarts that leave work owed to a restart (docs/live-update.md `remaining`).
const RESTART_OWED: [&str; 4] = ["session-restart", "soft-reboot", "kernel-transition", "reboot"];

/// The view from the two files' contents. `status_newer`: the status file was written after the checker's (a failure
/// older than the latest check is history, not news). A transaction in flight, and a restart owed, always win: the
/// checker says "none" once the new release is installed, while the machine still runs the old one.
pub fn derive(check: Option<&str>, status: Option<&str>, status_newer: bool) -> View {
    let check = check.map(kv).unwrap_or_default();
    let status: Option<Json> = status.and_then(|t| serde_json::from_str(t).ok());
    let mut v = View::default();
    let check_release = check.get("release").copied().unwrap_or("");
    v.running = status
        .as_ref()
        .map(|j| s(j, &["booted", "release"]))
        .filter(|r| !r.is_empty())
        .or_else(|| check.get("running").copied())
        .unwrap_or("")
        .to_string();
    if let Some(j) = &status {
        let st = s(j, &["state"]);
        let target = s(j, &["target", "release"]);
        let summary = s(j, &["summary"]).to_string();
        let remaining = s(j, &["remaining"]);
        let staged = j["staged_for_boot"].as_bool().unwrap_or(false);
        let state = match st {
            "discovered" | "verified" => Some("downloading"),
            "planned" | "activating" | "verifying" | "rolling-back" => Some("installing"),
            "deferred" => Some("deferred"),
            "failed" if status_newer => Some("failed"),
            "staged" | "active" | "rolled-back" | "superseded"
                if (staged || st == "active") && RESTART_OWED.contains(&remaining) =>
            {
                Some("restart")
            }
            _ => None,
        };
        if let Some(state) = state {
            v.state = state.into();
            v.available = target.to_string();
            v.message = summary;
            return v;
        }
    }
    match check.get("state").copied() {
        Some("available") => {
            v.state = "available".into();
            v.available = check_release.to_string();
            v.message = if check_release.is_empty() {
                "An update to Rime is available.".into()
            } else {
                format!("Rime {check_release} is available.")
            };
        }
        Some("none") => v.state = "up-to-date".into(),
        _ => v.state = "unknown".into(),
    }
    v
}

/// The files rimed reads (fixtures in tests).
#[derive(Debug, Clone)]
pub struct Paths {
    pub check: PathBuf,
    pub status: PathBuf,
    /// `/etc/NIXOS` and `/run/current-system`: the same test as `rime`'s `nixos::is_nixos`.
    pub nixos_marker: PathBuf,
    pub current_system: PathBuf,
}

impl Paths {
    pub fn system() -> Paths {
        Paths {
            check: CHECK_STATE.into(),
            status: LIVE_STATUS.into(),
            nixos_marker: "/etc/NIXOS".into(),
            current_system: "/run/current-system".into(),
        }
    }

    /// Rime on NixOS: the marker exists and the current system points into the store.
    pub fn on_nixos(&self) -> bool {
        self.nixos_marker.exists()
            && std::fs::read_link(&self.current_system).map(|p| p.starts_with("/nix/store")).unwrap_or(false)
    }

    pub fn read(&self) -> View {
        let read = |p: &Path| std::fs::read_to_string(p).ok();
        let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
        let newer = mtime(&self.status) >= mtime(&self.check);
        derive(read(&self.check).as_deref(), read(&self.status).as_deref(), newer)
    }
}

pub struct UpdatesIface {
    pub paths: Paths,
    pub view: Mutex<View>,
}

impl UpdatesIface {
    pub fn new(paths: Paths) -> UpdatesIface {
        let view = paths.read();
        UpdatesIface { paths, view: Mutex::new(view) }
    }

    fn get(&self) -> View {
        self.view.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// Read the files again; true when the view changed.
    pub fn refresh(&self) -> bool {
        let now = self.paths.read();
        let Ok(mut v) = self.view.lock() else { return false };
        if *v == now {
            return false;
        }
        *v = now;
        true
    }
}

async fn start_unit(conn: &Connection, unit: &str) -> zbus::fdo::Result<()> {
    conn.call_method(
        Some("org.freedesktop.systemd1"),
        "/org/freedesktop/systemd1",
        Some("org.freedesktop.systemd1.Manager"),
        "StartUnit",
        &(unit, "replace"),
    )
    .await
    .map(|_| ())
    .map_err(|e| zbus::fdo::Error::Failed(format!("starting {unit} failed: {e}")))
}

#[interface(name = "org.rimeos.Rimed1.Updates")]
impl UpdatesIface {
    #[zbus(property)]
    async fn state(&self) -> String {
        self.get().state
    }
    #[zbus(property)]
    async fn available(&self) -> String {
        self.get().available
    }
    #[zbus(property)]
    async fn running(&self) -> String {
        self.get().running
    }
    #[zbus(property)]
    async fn message(&self) -> String {
        self.get().message
    }

    /// Check now (rime-update-check.service). polkit `check-update`.
    async fn check(&self, #[zbus(connection)] conn: &Connection, #[zbus(header)] hdr: Header<'_>) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, ACTION_CHECK).await?;
        start_unit(conn, CHECK_UNIT).await
    }

    /// Install the update (rime-update.service, `rime update`). polkit `apply-update`, which asks for an
    /// administrator's password through the caller's agent.
    /// Refused before polkit off NixOS: [`APPLY_UNIT`] exists only there, and asking for a password to then fail
    /// to start a unit that is not installed tells the user nothing.
    async fn apply(&self, #[zbus(connection)] conn: &Connection, #[zbus(header)] hdr: Header<'_>) -> zbus::fdo::Result<()> {
        if !self.paths.on_nixos() {
            return Err(zbus::fdo::Error::NotSupported(format!(
                "Apply installs updates on Rime on NixOS only ({APPLY_UNIT}); on this system run `sudo rime update`"
            )));
        }
        authorize_interactive(conn, &hdr, ACTION_APPLY).await?;
        start_unit(conn, APPLY_UNIT).await
    }
}

impl UpdatesIface {
    async fn emit(&self, ctxt: &SignalContext<'_>) {
        let _ = self.state_changed(ctxt).await;
        let _ = self.available_changed(ctxt).await;
        let _ = self.running_changed(ctxt).await;
        let _ = self.message_changed(ctxt).await;
    }
}

/// inotify on both files' directories and on their parents (a directory created later, or replaced, is noticed),
/// on a thread of its own; each wake-up is sent down `tx`. Watches are added again after every event, so a directory
/// that appears is watched from then on.
fn inotify_thread(paths: Paths, tx: tokio::sync::mpsc::UnboundedSender<()>) {
    // SAFETY: plain syscalls on a descriptor this thread owns.
    let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
    if fd < 0 {
        eprintln!("rimed: updates: inotify unavailable ({}); the Updates properties follow nothing", std::io::Error::last_os_error());
        return;
    }
    let mask = libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_MOVED_FROM | libc::IN_CREATE | libc::IN_DELETE;
    let mut dirs: Vec<PathBuf> = Vec::new();
    for f in [&paths.check, &paths.status] {
        if let Some(d) = f.parent() {
            dirs.push(d.to_path_buf());
            if let Some(p) = d.parent() {
                dirs.push(p.to_path_buf());
            }
        }
    }
    let mut buf = [0u8; 4096];
    loop {
        for d in &dirs {
            if let Ok(c) = CString::new(d.as_os_str().as_bytes()) {
                // SAFETY: c is a valid C string; a missing directory just fails.
                unsafe { libc::inotify_add_watch(fd, c.as_ptr(), mask) };
            }
        }
        // SAFETY: buf is ours and the length is its size.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            eprintln!("rimed: updates: inotify read failed ({})", std::io::Error::last_os_error());
            break;
        }
        if tx.send(()).is_err() {
            break;
        }
    }
    // SAFETY: closing our own descriptor.
    unsafe { libc::close(fd) };
}

/// Follow the files: on any change in their directories read them again and, if the view changed, emit the four
/// properties.
pub async fn watch(conn: Connection, paths: Paths) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let p = paths.clone();
    if std::thread::Builder::new().name("rimed-updates".into()).spawn(move || inotify_thread(p, tx)).is_err() {
        eprintln!("rimed: updates: cannot start the watch thread");
        return;
    }
    while rx.recv().await.is_some() {
        // A writer renames a finished file into place; collect the burst, then read once.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        while rx.try_recv().is_ok() {}
        let Ok(iref) = conn.object_server().interface::<_, UpdatesIface>(OBJECT_PATH).await else { continue };
        let ctxt = iref.signal_context().clone();
        let i = iref.get().await;
        if i.refresh() {
            i.emit(&ctxt).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAGED: &str = r#"{"schema":1,"state":"staged","booted":{"release":"2026.11.01"},
        "target":{"release":"2026.11.02"},"staged_for_boot":true,"remaining":"reboot",
        "summary":"2026.11.02 is installed for the next restart."}"#;

    #[test]
    fn the_checker_alone() {
        assert_eq!(derive(None, None, true).state, "unknown");
        let v = derive(Some("state=none\ndigest=\n"), None, false);
        assert_eq!((v.state.as_str(), v.available.as_str()), ("up-to-date", ""));
        let v = derive(Some("state=available\ndigest=sha256:x\nrelease=2026.11.02\nrunning=2026.11.01\n"), None, false);
        assert_eq!(v.state, "available");
        assert_eq!(v.available, "2026.11.02");
        assert_eq!(v.running, "2026.11.01");
        assert_eq!(v.message, "Rime 2026.11.02 is available.");
        // The format before `release=` existed.
        assert_eq!(derive(Some("state=available\ndigest=sha256:x\n"), None, false).available, "");
    }

    #[test]
    fn a_restart_owed_beats_a_checker_that_says_none() {
        let v = derive(Some("state=none\n"), Some(STAGED), false);
        assert_eq!(v.state, "restart");
        assert_eq!(v.available, "2026.11.02");
        assert_eq!(v.running, "2026.11.01");
        assert_eq!(v.message, "2026.11.02 is installed for the next restart.");
        // Everything active, nothing owed: the checker decides.
        let done = STAGED.replace("\"staged\"", "\"active\"").replace("\"reboot\"", "\"nothing\"");
        assert_eq!(derive(Some("state=none\n"), Some(&done), true).state, "up-to-date");
    }

    #[test]
    fn a_transaction_in_flight_and_a_fresh_failure() {
        let act = STAGED.replace("\"staged\"", "\"activating\"");
        assert_eq!(derive(Some("state=available\n"), Some(&act), false).state, "installing");
        let def = STAGED.replace("\"staged\"", "\"deferred\"");
        assert_eq!(derive(None, Some(&def), false).state, "deferred");
        let failed = STAGED.replace("\"staged\"", "\"failed\"");
        assert_eq!(derive(Some("state=available\n"), Some(&failed), true).state, "failed");
        // A failure older than the latest check is history.
        assert_eq!(derive(Some("state=available\n"), Some(&failed), false).state, "available");
        // Unreadable JSON is no status at all.
        assert_eq!(derive(Some("state=none\n"), Some("{not json"), true).state, "up-to-date");
    }
}
