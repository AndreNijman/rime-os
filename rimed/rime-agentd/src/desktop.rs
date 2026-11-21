//! From a paired phone onto the computer itself: `clipboard_write` and
//! `save_file` (`docs/remote-live-contract.md` §1.8).
//!
//! Neither verb touches a session. One sets the Wayland clipboard; the other
//! saves a shared file into the user's Downloads folder. Both are refused to
//! the callers [`privilege::refuse_desktop`] names, before anything else is
//! looked at.
//!
//! ## Tests never reach the real desktop
//!
//! A test daemon runs as the developer, in their home, with their compositor
//! one socket away. With [`STUB_ENV`] set, both verbs write into that
//! directory instead (`<dir>/clipboard` and `<dir>/Downloads`), and never run
//! `wl-copy` or read `user-dirs.dirs`. A unit-test build with no stub refuses
//! rather than falling through to the real thing: [`desk`] is the only way to
//! the real backend, and under `cfg(test)` it has no such arm.
//!
//! ## Why the file is streamed to disk, unlike `receive`
//!
//! `receive` buffers up to 32 MiB because it ends by typing a path into a
//! terminal and wants no partial file in an inbox. A photo or a video shared
//! from a phone is routinely larger than that, so this verb streams into a
//! hidden temporary file in the destination folder and renames it into place
//! only once every byte has arrived, never over an existing file. Every way
//! the upload can end early removes the temporary file.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rime_agent_core::protocol::{ErrorKind, Response};

use crate::privilege;
use crate::Daemon;

/// Points both verbs at a directory instead of the real desktop.
pub const STUB_ENV: &str = "RIME_AGENTD_DESKTOP_STUB";

/// The largest file `save_file` accepts.
pub const MAX_SAVE_BYTES: u64 = 256 * 1024 * 1024;

/// Space the folder's file system must still have after the file.
pub const FREE_MARGIN: u64 = 64 * 1024 * 1024;

// The name rules are shared with rime-remoted, which reduces the name of a
// file it sends to a phone the same way (contract §2.6).
pub use rime_agent_core::filename::{file_name, numbered};

/// The whole budget for setting the clipboard. The control worker answers in
/// order, so a `wl-copy` that never returns must not hold the link.
const COPY_SECS: u64 = 5;

/// Nothing arriving for this long ends an upload.
const STALL_SECS: u64 = 30;

/// An upload's budget is this, plus one second per [`BYTES_PER_SEC`]: never
/// less than what rime-remoted gives a file from another computer (60 s and a
/// second per 64 KiB), or a file over a slow link would be cut here.
const BASE_SECS: u64 = 300;
const BYTES_PER_SEC: u64 = 64 * 1024;

/// Copies a numbered name may try before giving up.
const MAX_COPIES: u32 = 99;

/// Where the two verbs write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Desk {
    /// The user's real clipboard and Downloads folder.
    Real,
    /// A directory standing in for both.
    Stub(PathBuf),
}

/// The desk this process uses: the stub when [`STUB_ENV`] is set, the real
/// desktop otherwise, and never the real desktop in a unit-test build.
pub fn desk() -> Result<Desk, String> {
    match std::env::var_os(STUB_ENV) {
        Some(d) if !d.is_empty() => Ok(Desk::Stub(PathBuf::from(d))),
        _ if cfg!(test) => Err(format!(
            "this is a test build and {STUB_ENV} is not set, so nothing was written"
        )),
        _ => Ok(Desk::Real),
    }
}

// ── clipboard_write ─────────────────────────────────────────────────────────

/// Handle one `ClipboardWrite`. Who is asking is settled before the text is
/// looked at.
pub fn handle_clipboard_write(
    daemon: &Arc<Daemon>,
    caller: &privilege::Caller,
    text: &str,
) -> Response {
    let who = privilege::origin(daemon, caller);
    if let Some(refusal) = privilege::refuse_desktop(&who, "set this machine's clipboard") {
        return refusal;
    }
    match desk() {
        Ok(d) => clipboard_write(&d, text),
        Err(why) => Response::error(ErrorKind::Internal, why),
    }
}

/// The text rules, then the write. Pure apart from the write itself.
pub fn clipboard_write(desk: &Desk, text: &str) -> Response {
    if text.is_empty() {
        return Response::error(
            ErrorKind::BadRequest,
            "there is no text to put on this machine's clipboard",
        );
    }
    if text.len() > crate::clipboard::MAX_BYTES {
        return Response::error(
            ErrorKind::BadRequest,
            format!(
                "{} bytes is more than the {}-byte limit for this machine's clipboard. \
                 Send it as a file instead",
                text.len(),
                crate::clipboard::MAX_BYTES
            ),
        );
    }
    let written = match desk {
        Desk::Stub(dir) => stub_clipboard(dir, text),
        Desk::Real => real_clipboard(text, Duration::from_secs(COPY_SECS)),
    };
    match written {
        Ok(()) => Response::ClipboardWritten { bytes: text.len() },
        Err(why) => Response::error(ErrorKind::Internal, why),
    }
}

fn stub_clipboard(dir: &Path, text: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("the desktop stub {}: {e}", dir.display()))?;
    let tmp = dir.join(".clipboard.part");
    std::fs::write(&tmp, text.as_bytes())
        .and_then(|()| std::fs::rename(&tmp, dir.join("clipboard")))
        .map_err(|e| format!("the desktop stub's clipboard: {e}"))
}

/// `wl-copy` on the seat [`crate::clipboard::display`] finds, the text on
/// stdin. stdout and stderr go nowhere: `wl-copy` leaves a child behind to
/// serve the selection, and a pipe that child inherited would never reach EOF.
fn real_clipboard(text: &str, budget: Duration) -> Result<(), String> {
    let seat = crate::clipboard::display_for("set")?;
    let mut child = Command::new("wl-copy")
        .args(["--type", "text/plain;charset=utf-8"])
        .env("WAYLAND_DISPLAY", &seat.display)
        .env("XDG_RUNTIME_DIR", &seat.runtime_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            format!(
                "cannot set this machine's clipboard: wl-copy is not available to the agent \
                 runtime ({e})"
            )
        })?;
    // At most `clipboard::MAX_BYTES`, far under a pipe buffer, so this write
    // cannot block on a reader that is not reading.
    {
        let mut stdin = child.stdin.take().expect("stdin was piped");
        if let Err(e) = stdin.write_all(text.as_bytes()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("cannot set this machine's clipboard: wl-copy stopped reading ({e})"));
        }
    }
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(format!(
                    "wl-copy could not set this machine's clipboard ({status})"
                ))
            }
            Ok(None) => {}
            Err(e) => return Err(format!("wl-copy could not be waited for ({e})")),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "setting this machine's clipboard took longer than {}s and was stopped",
                budget.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ── save_file ───────────────────────────────────────────────────────────────

/// Where a file will go, decided before the takeover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub dir: PathBuf,
    pub name: String,
}

/// Handle one `SaveFile`: refuse what can be refused, take the connection
/// over, stream the bytes, put the file in place, answer.
pub fn handle_save(
    daemon: &Arc<Daemon>,
    caller: &privilege::Caller,
    mut writer: UnixStream,
    mut reader: std::io::BufReader<UnixStream>,
    name: &str,
    len: u64,
) -> anyhow::Result<()> {
    let who = privilege::origin(daemon, caller);
    if let Some(refusal) = privilege::refuse_desktop(&who, "save files on this machine") {
        return crate::session::write_response(&mut writer, &refusal);
    }
    let plan = match desk().and_then(|d| plan(&d, name, len)) {
        Ok(p) => p,
        Err(why) => {
            return crate::session::write_response(
                &mut writer,
                &Response::error(ErrorKind::BadRequest, why),
            )
        }
    };
    crate::session::write_response(&mut writer, &Response::Saving { len })?;
    let socket = writer.try_clone()?;
    let response = save(
        &plan,
        &socket,
        &mut reader,
        len,
        Duration::from_secs(STALL_SECS),
        budget(len),
    );
    crate::session::write_response(&mut writer, &response)
}

/// The whole-upload budget for `len` bytes.
pub fn budget(len: u64) -> Duration {
    Duration::from_secs(BASE_SECS + len / BYTES_PER_SEC)
}

/// Everything decidable before a byte is sent: the length, the name, the
/// folder and the free space.
pub fn plan(desk: &Desk, name: &str, len: u64) -> Result<Plan, String> {
    if len == 0 {
        return Err("a file of no bytes is not worth saving; nothing was taken over".into());
    }
    if len > MAX_SAVE_BYTES {
        return Err(format!(
            "{len} bytes is past the {MAX_SAVE_BYTES}-byte limit for saving a file on this \
             machine, so nothing was sent"
        ));
    }
    let name = file_name(name)?;
    let dir = downloads(desk)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("cannot make the folder {}: {e}", dir.display()))?;
    let free = free_bytes(&dir)?;
    if free < len.saturating_add(FREE_MARGIN) {
        return Err(format!(
            "{} has {} MB free, and this file needs {} MB plus {} MB to spare, so nothing was \
             sent",
            dir.display(),
            free / 1_000_000,
            len.div_ceil(1_000_000),
            FREE_MARGIN / 1_000_000
        ));
    }
    Ok(Plan { dir, name })
}

/// The Downloads folder: the stub's, or `XDG_DOWNLOAD_DIR` from
/// `user-dirs.dirs`, or `~/Downloads`.
pub fn downloads(desk: &Desk) -> Result<PathBuf, String> {
    match desk {
        Desk::Stub(dir) => Ok(dir.join("Downloads")),
        Desk::Real => {
            let home = std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(PathBuf::from)
                .ok_or("the agent runtime has no HOME, so there is no Downloads folder")?;
            let config = std::env::var_os("XDG_CONFIG_HOME")
                .filter(|c| !c.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"));
            let dirs = std::fs::read_to_string(config.join("user-dirs.dirs")).unwrap_or_default();
            Ok(user_dir(&dirs, &home).unwrap_or_else(|| home.join("Downloads")))
        }
    }
}

/// `XDG_DOWNLOAD_DIR` out of a `user-dirs.dirs`. Its format is shell
/// assignments where the value is `"$HOME/…"` or an absolute path; anything
/// else is ignored, as `xdg-user-dir` ignores it.
pub fn user_dir(dirs: &str, home: &Path) -> Option<PathBuf> {
    for line in dirs.lines() {
        let Some(value) = line.trim().strip_prefix("XDG_DOWNLOAD_DIR=") else {
            continue;
        };
        let value = value.trim().strip_prefix('"')?.strip_suffix('"')?;
        if value.contains("..") {
            return None;
        }
        if let Some(rest) = value.strip_prefix("$HOME") {
            let rest = rest.trim_start_matches('/');
            return Some(if rest.is_empty() { home.to_path_buf() } else { home.join(rest) });
        }
        if value.starts_with('/') {
            return Some(PathBuf::from(value));
        }
        return None;
    }
    None
}

fn free_bytes(dir: &Path) -> Result<u64, String> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes())
        .map_err(|_| format!("{} is not a usable path", dir.display()))?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a NUL-terminated path and `st` is a valid out-pointer.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return Err(format!(
            "cannot tell how much space {} has: {}",
            dir.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

static SEQ: AtomicU64 = AtomicU64::new(0);

/// A temporary file in `dir` that did not exist and is not a link.
fn temp_file(dir: &Path) -> Result<(PathBuf, File), String> {
    for _ in 0..16 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let path = dir.join(format!(
            ".rime-save-{}-{}-{nanos}.part",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
        {
            Ok(f) => return Ok((path, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("cannot write into {}: {e}", dir.display())),
        }
    }
    Err(format!("cannot find a free temporary name in {}", dir.display()))
}

/// The process id in a temporary name [`temp_file`] made, and nothing for
/// any other name: `.rime-save-<pid>-<seq>-<nanos>.part`, digits only.
fn partial_pid(name: &str) -> Option<u32> {
    let middle = name.strip_prefix(".rime-save-")?.strip_suffix(".part")?;
    let parts: Vec<&str> = middle.split('-').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    parts[0].parse().ok()
}

fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    // SAFETY: signal 0 only asks whether the process exists.
    pid > 0 && (unsafe { libc::kill(pid, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}

/// Remove the temporary files an earlier `rime-agentd` left in `dir` when it
/// stopped in the middle of a save. Only names [`temp_file`] makes, only
/// regular files (never through a link) this user owns, and only when the
/// process that wrote them is gone: never this process's own. What was
/// removed.
pub fn sweep_partials(dir: &Path, own: u32, alive: impl Fn(u32) -> bool) -> Vec<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let mut removed = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return removed };
    // SAFETY: geteuid cannot fail.
    let uid = unsafe { libc::geteuid() };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(partial_pid) else { continue };
        if pid == own || alive(pid) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if !meta.file_type().is_file() || meta.uid() != uid {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            removed.push(path);
        }
    }
    removed
}

/// [`sweep_partials`] on the folder a save is about to write into. Run by
/// every save rather than once at start: a daemon started by a test (or by
/// anything else without the desktop stub) never looks at the real Downloads
/// unless it is asked to save there.
fn sweep_before_save(dir: &Path) {
    for p in sweep_partials(dir, std::process::id(), process_alive) {
        eprintln!("rime-agentd: removed a file left half saved: {}", p.display());
    }
}

/// Removes the temporary file unless it was put in place.
struct Partial(Option<PathBuf>);

impl Drop for Partial {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Stream exactly `len` bytes into a temporary file, then rename it into
/// place without replacing anything. `stall` and `budget` are parameters so a
/// test of them does not wait thirty seconds.
pub fn save(
    plan: &Plan,
    socket: &UnixStream,
    reader: &mut impl Read,
    len: u64,
    stall: Duration,
    budget: Duration,
) -> Response {
    match save_inner(plan, socket, reader, len, stall, budget) {
        Ok((name, path)) => Response::Saved {
            name,
            path: path.to_string_lossy().into_owned(),
        },
        Err(why) => Response::error(ErrorKind::BadRequest, why),
    }
}

fn save_inner(
    plan: &Plan,
    socket: &UnixStream,
    reader: &mut impl Read,
    len: u64,
    stall: Duration,
    budget: Duration,
) -> Result<(String, PathBuf), String> {
    socket
        .set_read_timeout(Some(stall))
        .map_err(|e| format!("the upload could not be given a read timeout: {e}"))?;
    sweep_before_save(&plan.dir);
    let (tmp, mut file) = temp_file(&plan.dir)?;
    let mut partial = Partial(Some(tmp.clone()));
    let deadline = Instant::now() + budget;
    let mut got: u64 = 0;
    let mut chunk = vec![0u8; 64 * 1024];
    while got < len {
        if Instant::now() >= deadline {
            return Err(format!(
                "the file was still arriving after {}s ({got} of {len} bytes); nothing was saved",
                budget.as_secs()
            ));
        }
        let want = std::cmp::min(chunk.len() as u64, len - got) as usize;
        match reader.read(&mut chunk[..want]) {
            Ok(0) => {
                return Err(format!(
                    "the connection ended after {got} of {len} bytes; nothing was saved"
                ))
            }
            Ok(n) => {
                file.write_all(&chunk[..n])
                    .map_err(|e| format!("writing the file failed after {got} bytes: {e}"))?;
                got += n as u64;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Err(format!(
                    "no bytes arrived for {}s ({got} of {len} received); nothing was saved",
                    stall.as_secs()
                ))
            }
            Err(e) => return Err(format!("reading the upload: {e}")),
        }
    }
    file.sync_all()
        .map_err(|e| format!("the file could not be written out: {e}"))?;
    drop(file);
    for n in 1..=MAX_COPIES {
        let name = numbered(&plan.name, n);
        let dest = plan.dir.join(&name);
        match rename_noreplace(&tmp, &dest) {
            Ok(()) => {
                partial.0 = None;
                return Ok((name, dest));
            }
            Err(e) if e.raw_os_error() == Some(libc::EEXIST) => continue,
            Err(e) => return Err(format!("the file could not be put in place: {e}")),
        }
    }
    Err(format!(
        "{} already has {MAX_COPIES} files called {}; nothing was saved",
        plan.dir.display(),
        plan.name
    ))
}

fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let f = std::ffi::CString::new(from.as_os_str().as_bytes())?;
    let t = std::ffi::CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: both are NUL-terminated paths; AT_FDCWD with absolute paths.
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            f.as_ptr(),
            libc::AT_FDCWD,
            t.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rime_agent_core::filename::{FALLBACK_NAME, MAX_NAME};

    /// A fresh directory, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(prefix: &str) -> Scratch {
            let p = std::env::temp_dir().join(format!(
                "{prefix}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Scratch(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn only_a_dead_saves_own_temporary_files_are_swept() {
        let s = Scratch::new("rime-sweep");
        let d = s.path();
        let write = |name: &str| std::fs::write(d.join(name), b"partial").unwrap();
        let dead = ".rime-save-4000001-3-123.part";
        let live = ".rime-save-4000002-0-9.part";
        let own = format!(".rime-save-{}-1-5.part", std::process::id());
        for n in [dead, live, own.as_str()] {
            write(n);
        }
        // Names it does not make, however close.
        let others = [
            "report.part",
            ".rime-save-4000001-3.part",
            ".rime-save-4000001-3-1-2.part",
            ".rime-save-x-3-1.part",
            ".rime-save-4000001--1.part",
            ".rime-save-4000001-3-1.part.keep",
            "rime-save-4000001-3-1.part",
            ".rime-save-+4000001-3-1.part",
        ];
        for n in others {
            write(n);
        }
        // A link with the right name is never followed or removed, and the
        // file it points at stays.
        let target = d.join("precious.txt");
        std::fs::write(&target, b"keep me").unwrap();
        std::os::unix::fs::symlink(&target, d.join(".rime-save-4000003-0-0.part")).unwrap();
        // A directory with the right name stays too.
        std::fs::create_dir(d.join(".rime-save-4000004-0-0.part")).unwrap();
        let removed = sweep_partials(d, std::process::id(), |pid| pid == 4000002);
        assert_eq!(removed, vec![d.join(dead)]);
        assert!(!d.join(dead).exists());
        for n in [live, own.as_str()].into_iter().chain(others) {
            assert!(d.join(n).exists(), "{n} was removed");
        }
        assert!(std::fs::symlink_metadata(d.join(".rime-save-4000003-0-0.part")).is_ok());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep me");
        assert!(d.join(".rime-save-4000004-0-0.part").is_dir());
        // A folder that is not there is no error.
        assert!(sweep_partials(&d.join("missing"), 1, |_| false).is_empty());
        assert_eq!(partial_pid(dead), Some(4000001));
        assert_eq!(partial_pid(".rime-save-99999999999-1-1.part"), None, "a pid past u32");
    }

    #[test]
    fn a_name_temp_file_makes_is_one_the_sweep_knows() {
        let s = Scratch::new("rime-sweep-own");
        let (path, _f) = temp_file(s.path()).unwrap();
        let name = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(partial_pid(name), Some(std::process::id()), "{name}");
    }

    fn stub() -> (Scratch, Desk) {
        let t = Scratch::new("rime-desk");
        let d = Desk::Stub(t.path().to_path_buf());
        (t, d)
    }

    #[test]
    fn a_test_build_never_reaches_the_real_desktop() {
        // The stub env is never set in this process's unit tests.
        if std::env::var_os(STUB_ENV).is_none() {
            assert!(desk().is_err());
        }
    }

    #[test]
    fn names_are_names_never_paths_and_never_hidden() {
        assert_eq!(file_name("IMG_2041.jpg").unwrap(), "IMG_2041.jpg");
        assert_eq!(file_name("../../.bashrc").unwrap(), "bashrc");
        assert_eq!(file_name("C:\\Users\\x\\report final.pdf").unwrap(), "report final.pdf");
        assert_eq!(file_name("/etc/passwd").unwrap(), "passwd");
        assert_eq!(file_name(" . .hidden ").unwrap(), "hidden");
        assert_eq!(file_name("..").unwrap(), FALLBACK_NAME);
        assert_eq!(file_name("dir/").unwrap(), FALLBACK_NAME);
        assert_eq!(file_name("").unwrap(), FALLBACK_NAME);
        assert_eq!(file_name("Grüße 日本.txt").unwrap(), "Grüße 日本.txt");
        assert!(file_name("a\nb.txt").is_err());
        assert!(file_name("a\u{7f}b").is_err());
    }

    #[test]
    fn a_long_name_keeps_its_extension_and_a_whole_character() {
        let long = format!("{}.jpg", "é".repeat(300));
        let got = file_name(&long).unwrap();
        assert!(got.len() <= MAX_NAME, "{}", got.len());
        assert!(got.ends_with(".jpg"));
        let plain = "x".repeat(400);
        assert_eq!(file_name(&plain).unwrap().len(), MAX_NAME);
    }

    #[test]
    fn copies_are_numbered_before_the_extension() {
        assert_eq!(numbered("IMG.jpg", 1), "IMG.jpg");
        assert_eq!(numbered("IMG.jpg", 2), "IMG (2).jpg");
        assert_eq!(numbered("archive.tar.gz", 3), "archive.tar (3).gz");
        assert_eq!(numbered("notes", 2), "notes (2)");
        assert_eq!(numbered(".profile", 2), ".profile (2)");
    }

    #[test]
    fn the_download_folder_comes_from_user_dirs_with_a_fallback() {
        let home = Path::new("/home/t");
        let dirs = "# comment\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_DOWNLOAD_DIR=\"$HOME/Telechargements\"\n";
        assert_eq!(user_dir(dirs, home), Some(home.join("Telechargements")));
        assert_eq!(user_dir("XDG_DOWNLOAD_DIR=\"/data/dl\"\n", home), Some(PathBuf::from("/data/dl")));
        assert_eq!(user_dir("XDG_DOWNLOAD_DIR=\"$HOME/\"\n", home), Some(home.to_path_buf()));
        assert_eq!(user_dir("XDG_DOWNLOAD_DIR=\"$HOME/../x\"\n", home), None);
        assert_eq!(user_dir("XDG_DOWNLOAD_DIR=relative\n", home), None);
        assert_eq!(user_dir("", home), None);
    }

    #[test]
    fn the_clipboard_text_is_checked_then_written_verbatim() {
        let (t, d) = stub();
        assert!(matches!(clipboard_write(&d, ""), Response::Error { .. }));
        let big = "x".repeat(crate::clipboard::MAX_BYTES + 1);
        assert!(matches!(clipboard_write(&d, &big), Response::Error { .. }));
        assert!(!t.path().join("clipboard").exists(), "a refusal must write nothing");
        let text = "two\nlines \"quoted\"\n";
        assert!(matches!(clipboard_write(&d, text), Response::ClipboardWritten { bytes } if bytes == text.len()));
        assert_eq!(std::fs::read_to_string(t.path().join("clipboard")).unwrap(), text);
    }

    #[test]
    fn a_plan_refuses_before_any_byte_is_sent() {
        let (_t, d) = stub();
        assert!(plan(&d, "a.txt", 0).is_err());
        assert!(plan(&d, "a.txt", MAX_SAVE_BYTES + 1).is_err());
        assert!(plan(&d, "a\tb", 5).is_err());
        let p = plan(&d, "../a.txt", 5).unwrap();
        assert_eq!(p.name, "a.txt");
        assert!(p.dir.ends_with("Downloads") && p.dir.is_dir());
    }

    fn pair() -> (UnixStream, UnixStream) {
        UnixStream::pair().unwrap()
    }

    fn saved(r: Response) -> (String, String) {
        match r {
            Response::Saved { name, path } => (name, path),
            other => panic!("not saved: {other:?}"),
        }
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect()
    }

    #[test]
    fn a_file_arrives_whole_and_never_replaces_one_that_is_there() {
        let (_t, d) = stub();
        let p = plan(&d, "IMG.jpg", 6).unwrap();
        std::fs::write(p.dir.join("IMG.jpg"), b"theirs").unwrap();
        let (mut tx, rx) = pair();
        tx.write_all(b"abcdef").unwrap();
        let mut r = rx.try_clone().unwrap();
        let (name, path) = saved(save(&p, &rx, &mut r, 6, Duration::from_secs(2), Duration::from_secs(5)));
        assert_eq!(name, "IMG (2).jpg");
        assert_eq!(std::fs::read(&path).unwrap(), b"abcdef");
        assert_eq!(std::fs::read(p.dir.join("IMG.jpg")).unwrap(), b"theirs");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o111, 0, "a saved file is never executable");
        assert!(leftovers(&p.dir).is_empty());
    }

    #[test]
    fn a_save_sweeps_what_a_stopped_daemon_left_and_keeps_what_a_live_one_writes() {
        let (_t, d) = stub();
        let p = plan(&d, "next.txt", 3).unwrap();
        // A pid past the kernel's ceiling, so never running.
        let stale = p.dir.join(".rime-save-4194999-7-1.part");
        std::fs::write(&stale, b"half").unwrap();
        let own = p.dir.join(format!(".rime-save-{}-999999-1.part", std::process::id()));
        std::fs::write(&own, b"arriving").unwrap();
        let (mut tx, rx) = pair();
        tx.write_all(b"abc").unwrap();
        let mut r = rx.try_clone().unwrap();
        let (name, _) = saved(save(&p, &rx, &mut r, 3, Duration::from_secs(2), Duration::from_secs(5)));
        assert_eq!(name, "next.txt");
        assert!(!stale.exists(), "the stopped save's file was left");
        assert!(own.exists(), "this daemon's own arriving file was removed");
    }

    #[test]
    fn an_upload_that_stops_leaves_nothing_behind() {
        let (_t, d) = stub();
        let p = plan(&d, "half.bin", 10).unwrap();
        let (mut tx, rx) = pair();
        tx.write_all(b"abc").unwrap();
        drop(tx);
        let mut r = rx.try_clone().unwrap();
        let resp = save(&p, &rx, &mut r, 10, Duration::from_secs(2), Duration::from_secs(5));
        assert!(matches!(resp, Response::Error { .. }), "{resp:?}");
        assert!(std::fs::read_dir(&p.dir).unwrap().next().is_none(), "nothing may be left");
    }

    #[test]
    fn a_stalled_upload_is_ended_by_its_clock_and_leaves_nothing() {
        let (_t, d) = stub();
        let p = plan(&d, "slow.bin", 10).unwrap();
        let (mut tx, rx) = pair();
        tx.write_all(b"ab").unwrap();
        let mut r = rx.try_clone().unwrap();
        let resp = save(&p, &rx, &mut r, 10, Duration::from_millis(100), Duration::from_secs(5));
        match resp {
            Response::Error { message, .. } => assert!(message.contains("no bytes arrived"), "{message}"),
            other => panic!("{other:?}"),
        }
        assert!(std::fs::read_dir(&p.dir).unwrap().next().is_none());
        drop(tx);
    }

    #[test]
    fn bytes_past_len_are_not_read() {
        let (_t, d) = stub();
        let p = plan(&d, "exact.txt", 3).unwrap();
        let (mut tx, rx) = pair();
        tx.write_all(b"abcdef").unwrap();
        let mut r = rx.try_clone().unwrap();
        let (_, path) = saved(save(&p, &rx, &mut r, 3, Duration::from_secs(2), Duration::from_secs(5)));
        assert_eq!(std::fs::read(path).unwrap(), b"abc");
    }

    #[test]
    fn the_budget_grows_with_the_file() {
        assert_eq!(budget(0), Duration::from_secs(BASE_SECS));
        assert_eq!(budget(MAX_SAVE_BYTES), Duration::from_secs(BASE_SECS + 4096));
        // Longer than rime-remoted's schedule for a file between computers, at every size.
        for len in [1u64, 16 << 20, MAX_SAVE_BYTES] {
            assert!(budget(len) > Duration::from_secs(60 + len / (64 * 1024)), "{len}");
        }
    }

    #[test]
    #[ignore = "starts a private headless labwc; run with --ignored"]
    fn the_real_clipboard_write_reaches_a_compositor() {
        // A compositor of its own, in a short private runtime dir (Wayland
        // socket paths are capped at 108 bytes), so the developer's session
        // is never touched.
        // Under /tmp, not temp_dir(): an agent's TMPDIR is too long a path.
        let rt = Scratch(PathBuf::from(format!("/tmp/rwl{}", std::process::id())));
        let _ = std::fs::remove_dir_all(rt.path());
        std::fs::create_dir_all(rt.path()).unwrap();
        std::fs::set_permissions(rt.path(), std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        let mut labwc = Command::new("labwc")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", rt.path())
            .env("XDG_RUNTIME_DIR", rt.path())
            .env("WLR_BACKENDS", "headless")
            .env("WLR_RENDERER", "pixman")
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("labwc");
        let sock = rt.path().join("wayland-0");
        let until = Instant::now() + Duration::from_secs(10);
        while !sock.exists() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50));
        }
        // SAFETY: this ignored test runs alone (`--ignored --test-threads=1`).
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", rt.path());
            std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
        }
        let text = "from the phone\nsecond line";
        let r = real_clipboard(text, Duration::from_secs(COPY_SECS));
        let seat = crate::clipboard::display().expect("the private compositor");
        let back = crate::clipboard::read(&seat);
        let _ = labwc.kill();
        let _ = labwc.wait();
        r.expect("wl-copy");
        assert!(matches!(back, Response::Clipboard { text: ref t } if t == text), "{back:?}");
    }
}
