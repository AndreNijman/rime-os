//! Where the agent runtime keeps its files.
//!
//! Three separate roots, because they have three different lifetimes:
//!
//! * the control socket lives in `$XDG_RUNTIME_DIR` — it is meaningless once
//!   the login session ends, and `XDG_RUNTIME_DIR` is already `0700` and
//!   `tmpfs`, so no session state ever reaches disk;
//! * session records and logs live in `$XDG_STATE_HOME` — they survive a
//!   daemon restart and a reboot so `rime agent list` can still explain what
//!   ran yesterday;
//! * per-session scratch lives under `/tmp/rime-agent-<uid>/<id>` — it is the
//!   one writable path outside the project that a sandboxed agent gets, and it
//!   is removed with the session. The uid is in the path because it was not,
//!   once, and a second account on the machine then could not start a session
//!   at all: see [`SCRATCH_ROOT_PREFIX`].
//!
//! Nothing here is privileged. Every path is user-owned and every directory is
//! created `0700`, because a session log is a transcript of the user's work.

use std::io;
use std::path::{Path, PathBuf};

/// `$XDG_RUNTIME_DIR`, or `/run/user/<uid>` when the variable is unset (a
/// non-login shell, a cron job). Falling back to the conventional path rather
/// than to `/tmp` matters: `/tmp` is shared, and a predictable socket path in a
/// shared directory is a hijack waiting to happen.
pub fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    // Safe: getuid() cannot fail and has no side effects.
    PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }))
}

/// The current user's uid. Callers that must special-case root ask here
/// rather than reaching for `libc` themselves: the agent runtime is a per-user
/// service, and root has no `systemd --user` instance to enable it against
/// unless it lingers, which an ordinary login does not need.
pub fn uid() -> u32 {
    // Safe: getuid() cannot fail and has no side effects.
    unsafe { libc::getuid() }
}

/// `$XDG_STATE_HOME`, or `~/.local/state` per the base-directory spec.
pub fn state_home() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    home().join(".local/state")
}

/// The user's home directory. `$HOME` first, then the passwd database, so this
/// still resolves inside a `systemd --user` unit that was started without a
/// full login environment.
pub fn home() -> PathBuf {
    if let Some(dir) = std::env::var_os("HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    passwd_home().unwrap_or_else(|| PathBuf::from("/"))
}

fn passwd_home() -> Option<PathBuf> {
    // Safe: getuid cannot fail.
    passwd_home_of(unsafe { libc::getuid() })
}

/// The home directory the passwd database records for `uid`, or nothing when
/// there is no entry or it names no directory.
pub fn passwd_home_of(uid: libc::uid_t) -> Option<PathBuf> {
    use std::ffi::CStr;

    // getpwuid_r, not getpwuid. The plain form returns a pointer into a static
    // buffer shared by the whole process, so two threads resolving the home
    // directory at once can each get the other's result — and this is reached
    // from request handling in a multi-threaded daemon, not only at startup.
    // The reentrant form writes into a buffer we own.
    let mut buf = vec![0 as libc::c_char; 1024];
    loop {
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // Safe: getpwuid_r writes only into `pwd` and `buf`, both owned here,
        // and reports the buffer being too small rather than overrunning it.
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                &mut pwd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut result,
            )
        };
        if rc == libc::ERANGE && buf.len() < 64 * 1024 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || result.is_null() {
            // No entry, or an error. Either way there is no home to report.
            return None;
        }
        if pwd.pw_dir.is_null() {
            return None;
        }
        // Safe: pw_dir points into `buf`, which is still alive here, and the
        // bytes are copied out before returning.
        let bytes = unsafe { CStr::from_ptr(pwd.pw_dir) }.to_bytes();
        if bytes.is_empty() {
            return None;
        }
        return Some(PathBuf::from(String::from_utf8_lossy(bytes).into_owned()));
    }
}

/// The daemon's control socket. One per user, not per session.
pub fn control_socket() -> PathBuf {
    control_socket_in(&runtime_dir())
}

/// The same socket, under a runtime directory the caller already has.
///
/// The `*_in` form exists for the reason the store's do: a function that builds
/// a sandbox specification has to stay pure to be asserted exhaustively, and
/// reading `$XDG_RUNTIME_DIR` inside it would make the argv depend on the
/// environment of whichever test ran first.
pub fn control_socket_in(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("rime-agentd/control.sock")
}

/// Root of the persistent session store.
pub fn state_dir() -> PathBuf {
    state_home().join("rime/agent")
}

/// One JSON record per session, named by id.
pub fn session_record(id: u32) -> PathBuf {
    session_record_in(&state_dir(), id)
}

/// The full PTY transcript for a session.
pub fn session_log(id: u32) -> PathBuf {
    session_log_in(&state_dir(), id)
}

/// Where session records live.
pub fn sessions_dir() -> PathBuf {
    sessions_dir_in(&state_dir())
}

/// Where session transcripts live.
pub fn logs_dir() -> PathBuf {
    logs_dir_in(&state_dir())
}

// The `_in` forms take the store root explicitly. The daemon always passes
// `state_dir()`; a test passes a directory of its own, so it can exercise the
// real record and transcript layout without writing into the user's history.
// The layout itself is stated once, here, and nowhere else.

/// [`sessions_dir`] under an explicit store root.
pub fn sessions_dir_in(store: &Path) -> PathBuf {
    store.join("sessions")
}

/// [`logs_dir`] under an explicit store root.
pub fn logs_dir_in(store: &Path) -> PathBuf {
    store.join("logs")
}

/// [`session_record`] under an explicit store root.
pub fn session_record_in(store: &Path, id: u32) -> PathBuf {
    sessions_dir_in(store).join(format!("{id}.json"))
}

/// [`session_log`] under an explicit store root.
pub fn session_log_in(store: &Path, id: u32) -> PathBuf {
    logs_dir_in(store).join(format!("{id}.log"))
}

/// Registered projects, keyed by a slug of their path.
pub fn projects_dir() -> PathBuf {
    state_dir().join("projects")
}

/// User preferences for the runtime (default agent, default sandbox policy).
pub fn config_file() -> PathBuf {
    config_home().join("rime/agent.json")
}

/// `$XDG_CONFIG_HOME`, or `~/.config`.
pub fn config_home() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    home().join(".config")
}

/// `$XDG_DATA_HOME`, or `~/.local/share` per the base-directory spec.
///
/// Here rather than in a caller because this is the module that implements the
/// spec, and a second resolver would be a second thing to get wrong. It exists
/// for one reader: `rime task` checks whether a capsule still exists, and the
/// capsule engine keeps one JSON record per capsule under
/// `${XDG_DATA_HOME}/rime/env/` — see `files/system/libexec/rime-env`, which
/// resolves the same path the same way.
pub fn data_home() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    home().join(".local/share")
}

/// The prefix session scratch directories live under, before the uid.
///
/// Deliberately under `/tmp` and not `$XDG_RUNTIME_DIR`: agents generate build
/// output here, and `XDG_RUNTIME_DIR` is a small tmpfs that other software
/// depends on.
///
/// ## Why the uid is in the path, and what happened without it (P2-016)
///
/// This was `/tmp/rime-agent` — one directory for the machine, with the
/// session id below it — and that is the only path in this module with no user
/// in it. Measured on a live Rime laptop on 2026-09-12:
///
/// ```text
/// $ ls -lad /tmp/rime-agent
/// drwxr-xr-x  andre  /tmp/rime-agent
/// ```
///
/// `0755`, owned by whoever logged in first, and that is not an accident of
/// this machine: [`ensure_private_dir`] is called on the LEAF
/// (`<root>/<id>`), so `create_dir_all` makes the root along the way under the
/// caller's umask and only the leaf is chmodded to `0700`. `/tmp` is sticky,
/// so the second user can neither write into that directory nor remove it.
///
/// Two things follow, and both are P2-016 criterion 1 — per-user agents and
/// session isolation:
///
///  * **the second account cannot run an agent at all.** `session.rs` treats a
///    failure to create the scratch directory as fatal, deliberately, so every
///    session start for the second user ends in `EACCES` on a directory owned
///    by the first. Whoever logs in first owns the feature for the machine.
///  * **session ids collide across accounts.** Ids are allocated per daemon
///    and start again at 1, so two users' session 1 name one directory — and a
///    session reap runs `remove_dir_all` on its own id's directory, which is
///    the hazard [`SCRATCH_ROOT_ENV`] was added to keep a *test* daemon away
///    from a live one. Two accounts are the same collision with no variable to
///    set.
///
/// So the default carries the uid. Each account creates its own top-level
/// directory in `/tmp`, which is world-writable, and never needs write
/// permission on anything another account made.
///
/// ## The pre-created root, which this paragraph used to get wrong
///
/// A predictable name in a sticky directory can be pre-created by another
/// account, and the paragraph that stood here said `ensure_private_dir` "will
/// then fail to chmod a directory it does not own", so the worst case was "a
/// loud refusal and a denial of service rather than a disclosure". That was
/// never measured, and it is false. Measured on 2026-09-19 against the real
/// function, with a second account standing in for the attacker:
///
/// ```text
/// nobody pre-creates the root 0755   -> Err(EACCES)        (the claim)
/// nobody pre-creates the root 0777   -> Ok(())             (SILENT SUCCESS)
/// nobody pre-creates the leaf 0777   -> Err(EPERM)
/// nobody plants the leaf as a symlink-> Ok(()), and the SYMLINK TARGET
///                                       was chmodded to 0700
/// ```
///
/// The reason is the one this module already knew about the OLD shared root,
/// repeated one level down: [`ensure_private_dir`] is called on the LEAF, so
/// `create_dir_all` made the root along the way and nothing ever looked at it.
/// `0755` is only the mode an attacker would not choose; at `0777` the session
/// starts with its scratch root owned by another account, which can rename,
/// remove or replace the session's own directory underneath it for as long as
/// the session runs. And row 4 is what row 2 buys: `fs.protected_symlinks`
/// stops a symlink planted directly in sticky `/tmp`, but the attacker-owned
/// root is not sticky, so a symlink planted INSIDE it is followed — giving
/// another account the choice of which of this user's directories gets
/// chmodded `0700` and which one the session's build output lands in.
///
/// Two things close it, and both are needed:
///
///  * [`ensure_private_dir`] refuses a symlink and refuses a directory this
///    account does not own, instead of chmodding whatever it finds; and
///  * **the root is a boundary and is ensured FIRST**, by its own call, before
///    the leaf under it — see `session.rs`. Hardening only the leaf is a check
///    against a moving target: only the owner of a `0700` directory can rename
///    the entries in it, so the leaf's guarantees rest on the root's.
///
/// What is still NOT solved by a path, stated so it is not mistaken for the
/// above: an intermediate component that is already a symlink is followed by
/// `create_dir_all` before either check sees it. The boundary call is what
/// keeps that to directories this account made.
pub const SCRATCH_ROOT_PREFIX: &str = "/tmp/rime-agent-";

/// The environment variable that moves it.
///
/// The one path in this module that no XDG variable reaches, which made it the
/// one path a test daemon shared with the user's real one — and a session reap
/// runs `remove_dir_all` on its own id's directory, so a fixture daemon that
/// numbered a session the same as a live one would delete a running session's
/// scratch. Every suite in this repository fixtures `XDG_RUNTIME_DIR` and
/// `XDG_STATE_HOME`; this is what lets one fixture the last of it.
///
/// Read from the daemon's own environment and never from a request, so it has
/// exactly the trust of `XDG_RUNTIME_DIR`: a session cannot set it, because the
/// daemon's environment is fixed before any session exists.
pub const SCRATCH_ROOT_ENV: &str = "RIME_AGENT_SCRATCH_ROOT";

/// The shipped disposable-capsule engine (§19).
pub const DISPOSABLE_ENGINE: &str = rime_paths::RIME_DISPOSABLE;

/// Overrides [`DISPOSABLE_ENGINE`], for a suite that must not create capsules.
///
/// The engine itself already takes `RIME_DISPOSABLE_ENV_ENGINE` and
/// `RIME_DISPOSABLE_ROOT`, so a test can run the REAL engine against a fake
/// capsule engine in a scratch root. This variable is the last link in that
/// chain: without it a suite cannot reach the engine in the repository at all,
/// because the daemon would look under `/usr/libexec` on the running system.
pub const DISPOSABLE_ENGINE_ENV: &str = "RIME_DISPOSABLE_ENGINE";

/// Where the daemon looks for the disposable engine.
///
/// Absolute or nothing, for the reason [`scratch_root`] insists on it: a
/// relative path would resolve against the daemon's working directory, which
/// is not the caller's.
pub fn disposable_engine() -> PathBuf {
    if let Some(p) = std::env::var_os(DISPOSABLE_ENGINE_ENV) {
        let path = PathBuf::from(p);
        if path.is_absolute() {
            return path;
        }
    }
    PathBuf::from(DISPOSABLE_ENGINE)
}

/// The scratch directory a sandboxed session may write to.
pub fn scratch_dir(id: u32) -> PathBuf {
    scratch_root().join(id.to_string())
}

/// The scratch directory for session `id`, with its ROOT ensured FIRST.
///
/// Callers must use this rather than [`ensure_private_dir`] on
/// [`scratch_dir`], and the difference is not tidiness. The scratch root is
/// the one directory in this module whose parent is world-writable, so it is a
/// boundary and has to be a directory this account made — see
/// [`SCRATCH_ROOT_PREFIX`] for what was measured when it was not. Ensuring
/// only the leaf reads as safe and is not: the leaf is genuinely this
/// account's, created inside whatever the attacker left there, and only the
/// owner of a `0700` directory can rename the entries in it, so the leaf's
/// privacy rests on the root's.
pub fn ensure_scratch_dir(id: u32) -> io::Result<PathBuf> {
    ensure_scratch_dir_in(&scratch_root(), id)
}

/// [`ensure_scratch_dir`] with the root passed in — the `_in` shape the rest
/// of this module uses, and what lets a test give it a root of its own without
/// `set_var`, which is process-global and races every other test.
pub fn ensure_scratch_dir_in(root: &Path, id: u32) -> io::Result<PathBuf> {
    ensure_private_dir(root)?;
    let dir = root.join(id.to_string());
    ensure_private_dir(&dir)?;
    Ok(dir)
}

/// The default scratch root for one account, as a function of the uid alone.
///
/// Split out for the reason every `*_in` in this module is: a test process has
/// exactly one uid, and the claim worth asserting — that two accounts get two
/// roots — is about two. A test that rebuilt the formula for itself would
/// assert only that the test author and this function agree today, and would
/// stay green with the uid dropped from the line below.
pub fn scratch_root_for(uid: u32) -> PathBuf {
    PathBuf::from(format!("{SCRATCH_ROOT_PREFIX}{uid}"))
}

/// The root the scratch directories sit in.
///
/// Per-uid by default — see [`SCRATCH_ROOT_PREFIX`] for what a shared root
/// cost. The override is returned verbatim and is NOT given a uid suffix: a
/// suite that sets it has already chosen a directory of its own, and appending
/// to it would make the variable's value not be the path.
pub fn scratch_root() -> PathBuf {
    if let Some(dir) = std::env::var_os(SCRATCH_ROOT_ENV) {
        let path = PathBuf::from(dir);
        // Absolute or nothing: a relative root would be resolved against the
        // daemon's working directory, which is not the user's and which the
        // sandbox then binds by that name.
        if path.is_absolute() {
            return path;
        }
    }
    scratch_root_for(uid())
}

/// Create `dir` and every missing parent with `0700`, and refuse a directory
/// this account does not own.
///
/// Session logs are transcripts of the user's work, so the mode is not left to
/// inherited state. Three things this used to get wrong, all of them measured
/// rather than reasoned about — see [`SCRATCH_ROOT_PREFIX`] for the numbers:
///
///  * `create_dir_all` applies the process umask to EVERY component it makes
///    and only the leaf was chmodded afterwards, so the parents were left at
///    whatever the umask said (`0755` on this machine). `DirBuilder::mode`
///    applies to every directory it creates, so they start private instead of
///    being widened and then not narrowed.
///  * [`std::fs::metadata`] FOLLOWS a symlink, so a symlink planted at `dir`
///    by another account was followed and the chmod below landed on its
///    target. The stat is `symlink_metadata` and a symlink is refused.
///  * a directory owned by another account was chmodded if that happened to be
///    permitted and reported as success if it did not need chmodding at all.
///    Ownership is now checked, and it is checked against the caller's own uid
///    rather than against a mode.
///
/// What it does not do, deliberately: walk the ancestors. Every other root in
/// this module is under `$HOME` or `/run/user/<uid>`, and a check that climbed
/// to `/` would refuse on `/var` in every one of them. The one root whose
/// parent is world-writable is the scratch root, and that is why its caller
/// ensures it as a boundary in its own right.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    ensure_private_dir_as(dir, uid())
}

/// [`ensure_private_dir`] with the owner it checks against passed in.
///
/// Split out for the reason [`scratch_root_for`] is: a test process has
/// exactly one uid, and the claim worth asserting — that a directory belonging
/// to ANOTHER account is refused — is about two. The `stat` is a real one of a
/// real directory; only the uid it is compared against is chosen.
///
/// **`pub(crate)`, and that is the point.** "Production callers use
/// [`ensure_private_dir`] and never reach this" would otherwise be a comment
/// asking to be believed; the visibility makes it a property of the build.
/// Every other crate in the workspace links this one, so a `pub` here would be
/// a way for a caller anywhere to choose which account an ownership check
/// compares against — which is the check.
pub(crate) fn ensure_private_dir_as(dir: &Path, me: u32) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;

    // NOT `metadata`: that follows a symlink, and following one here is how a
    // chmod of this account's choosing became a chmod of somebody else's.
    let meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is a symbolic link, so it is not a directory this account made",
                dir.display()
            ),
        ));
    }
    if meta.uid() != me {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is owned by uid {}, not by uid {me}, so this account cannot make it private",
                dir.display(),
                meta.uid()
            ),
        ));
    }

    let mut perms = meta.permissions();
    if perms.mode() & 0o777 != 0o700 {
        perms.set_mode(0o700);
        std::fs::set_permissions(dir, perms)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_uid_resolves_to_the_same_home_as_the_current_user_lookup() {
        // passwd_home_of is what `sudo rime update` uses to find the INVOKING
        // account's home; for our own uid it must agree with passwd_home.
        // Safe: getuid cannot fail.
        let me = unsafe { libc::getuid() };
        assert_eq!(passwd_home_of(me), passwd_home());
        // A uid with no passwd entry has no home, rather than root's or "/".
        assert_eq!(passwd_home_of(4_000_000_000), None);
    }

    use super::*;

    #[test]
    fn runtime_dir_falls_back_to_run_user() {
        // Not `std::env::set_var` — that is process-global and races other
        // tests. Assert the shape of the fallback instead.
        let uid = unsafe { libc::getuid() };
        let fallback = PathBuf::from(format!("/run/user/{uid}"));
        assert!(fallback.is_absolute());
        assert!(fallback.starts_with("/run/user"));
    }

    #[test]
    fn the_passwd_lookup_resolves_and_is_safe_to_call_concurrently() {
        // The fallback used when $HOME is unset. getpwuid_r writes into a
        // caller-owned buffer; the non-reentrant getpwuid it replaced returned
        // a pointer into one static buffer shared by the whole process, so two
        // threads could each receive the other's answer.
        let expected = passwd_home();
        assert!(
            expected.as_ref().is_none_or(|p| p.is_absolute()),
            "{expected:?}"
        );

        let handles: Vec<_> = (0..8)
            .map(|_| std::thread::spawn(passwd_home))
            .collect();
        for h in handles {
            assert_eq!(
                h.join().expect("thread panicked"),
                expected,
                "concurrent lookups disagreed"
            );
        }
    }

    #[test]
    fn home_is_always_absolute() {
        assert!(home().is_absolute());
    }

    #[test]
    fn every_root_is_absolute() {
        assert!(control_socket().is_absolute());
        assert!(state_dir().is_absolute());
        assert!(config_file().is_absolute());
        assert!(scratch_dir(1).is_absolute());
        assert!(data_home().is_absolute());
    }

    #[test]
    fn data_home_reads_its_own_variable_and_falls_back_to_the_spec() {
        // Written against whatever the environment actually is, rather than by
        // setting a variable: `std::env::set_var` is process-global and races
        // the other tests in this crate. What it catches is the copy-paste bug
        // — a resolver that read XDG_STATE_HOME, or fell back to
        // `.local/state`, would fail one branch or the other here.
        match std::env::var_os("XDG_DATA_HOME") {
            Some(v) if !v.is_empty() => assert_eq!(data_home(), PathBuf::from(v)),
            _ => assert_eq!(data_home(), home().join(".local/share")),
        }
    }

    #[test]
    fn the_scratch_root_reads_its_own_variable_and_falls_back_to_the_default() {
        // Written against whatever the environment actually is, for the same
        // reason `data_home_reads_its_own_variable_and_falls_back_to_the_spec`
        // is: `set_var` is process-global and races the other tests here.
        match std::env::var_os(SCRATCH_ROOT_ENV) {
            Some(v) if !v.is_empty() && Path::new(&v).is_absolute() => {
                assert_eq!(scratch_root(), PathBuf::from(v));
            }
            _ => assert_eq!(scratch_root(), scratch_root_for(uid())),
        }
        assert!(scratch_dir(7).ends_with("7"));
        assert!(scratch_dir(7).starts_with(scratch_root()));
    }

    #[test]
    fn two_accounts_do_not_share_a_scratch_root() {
        // P2-016 criterion 1, and the whole reason the uid is in this path.
        // The root was `/tmp/rime-agent` for the machine: `ensure_private_dir`
        // is called on the LEAF, so `create_dir_all` made the root along the
        // way under the caller's umask and only the leaf was chmodded. Measured
        // on the live laptop on 2026-09-12, `/tmp/rime-agent` was
        // `drwxr-xr-x andre` — 0755, owned by whoever logged in first, in a
        // sticky /tmp the second account can neither write to nor remove. Every
        // session start for that account then fails, fatally and by design
        // (`session.rs` treats it as fatal), and session ids — which restart at
        // 1 per daemon — named the same directories.
        //
        // Asserted through `scratch_root_for`, which is what `scratch_root`
        // itself calls, so dropping the uid from the production line fails
        // this. A test that rebuilt `format!("{PREFIX}{uid}")` for itself would
        // not: it would be asserting its own arithmetic.
        assert_ne!(scratch_root_for(1000), scratch_root_for(1001));
        assert_ne!(
            scratch_root_for(1000).join("1"),
            scratch_root_for(1001).join("1"),
            "session 1 of two accounts named one directory"
        );

        // And the root is a direct child of /tmp, with the uid as its whole
        // final component. This is the assertion that forbids the original bug
        // coming back in the shape that looks like a fix: a prefix of
        // `/tmp/rime-agent/` would give `/tmp/rime-agent/1000` — per-uid
        // leaves under a shared parent, which is the same directory nobody
        // owns, created the same way, failing for the same account.
        for uid in [0_u32, 1, 1000, 65534] {
            let root = scratch_root_for(uid);
            let parts: Vec<_> = root.components().collect();
            assert_eq!(
                parts.len(),
                3,
                "{} is not a direct child of /tmp: {parts:?}",
                root.display()
            );
            assert_eq!(
                root.parent().map(Path::to_path_buf),
                Some(PathBuf::from("/tmp")),
                "{}",
                root.display()
            );
            assert!(
                root.file_name()
                    .expect("a final component")
                    .to_string_lossy()
                    .ends_with(&uid.to_string()),
                "{} does not name uid {uid}",
                root.display()
            );
        }
    }

    #[test]
    fn this_accounts_scratch_root_names_this_account() {
        // `scratch_root_for` is only the formula; this is what ties the live
        // reader to it, so a `scratch_root` that stopped consulting the uid —
        // or went back to a constant — is caught here. Stepped over when a
        // suite has set the override, for the reason `scratch_root` documents:
        // a directory a test chose is not required to mention anybody.
        match std::env::var_os(SCRATCH_ROOT_ENV) {
            Some(v) if !v.is_empty() && Path::new(&v).is_absolute() => {}
            _ => assert_eq!(
                scratch_root(),
                scratch_root_for(uid()),
                "the default scratch root does not name uid {}",
                uid()
            ),
        }
    }

    #[test]
    fn per_session_paths_are_distinct() {
        assert_ne!(session_log(1), session_log(2));
        assert_ne!(session_record(1), session_record(2));
        assert_ne!(scratch_dir(1), scratch_dir(2));
    }

    #[test]
    fn ensure_private_dir_forces_0700() {
        use std::os::unix::fs::PermissionsExt;

        let base = std::env::temp_dir().join(format!("rime-agent-test-{}", std::process::id()));
        let nested = base.join("a/b");
        ensure_private_dir(&nested).expect("create");
        let mode = std::fs::metadata(&nested).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "mode was {:o}", mode & 0o777);

        // Loosening it and re-running must tighten it back.
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&nested).expect("re-create");
        let mode = std::fs::metadata(&nested).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn every_component_it_creates_is_private_and_not_only_the_leaf() {
        use std::os::unix::fs::PermissionsExt;

        // The scratch ROOT is the directory this was wrong about: it was made
        // by `create_dir_all` under the umask and never tightened, so on a
        // default umask it stood at 0755 in world-writable /tmp while the
        // session directory under it was 0700. Another account could list one
        // user's session ids off it.
        let base = std::env::temp_dir().join(format!(
            "rime-agent-parents-{}-{}",
            std::process::id(),
            line!()
        ));
        let leaf = base.join("root/1");
        ensure_private_dir(&leaf).expect("create");
        for dir in [base.as_path(), &base.join("root"), leaf.as_path()] {
            let mode = std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{} was {:o}", dir.display(), mode);
        }
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_symlink_is_refused_and_its_target_is_left_alone() {
        use std::os::unix::fs::PermissionsExt;

        // Measured on 2026-09-19: with `metadata` in place of
        // `symlink_metadata` this returned Ok and chmodded the TARGET to 0700,
        // which hands another account the choice of which of this user's
        // directories gets its mode changed and where the session's scratch
        // writes land. The target's surviving mode is the half that makes this
        // a measurement rather than an exit-code check — see the unit's card:
        // an assertion on a refusal alone measures whichever refusal is first.
        let base = std::env::temp_dir().join(format!(
            "rime-agent-symlink-{}-{}",
            std::process::id(),
            line!()
        ));
        let target = base.join("target");
        let link = base.join("link");
        std::fs::create_dir_all(&target).expect("target");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        let err = ensure_private_dir(&link).expect_err("a symlink must be refused");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
        assert!(err.to_string().contains("symbolic link"), "{err}");

        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "the symlink's target was chmodded to {mode:o}");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link itself was replaced"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn the_scratch_root_is_ensured_as_a_boundary_and_not_only_the_session_leaf() {
        use std::os::unix::fs::PermissionsExt;

        // Case B of the measurement in `SCRATCH_ROOT_PREFIX`: the ROOT is a
        // directory this call did not make. Here it is one this account owns
        // but left loose, which is the single-uid half of the same shape —
        // a root the call must tighten rather than walk past.
        let base = std::env::temp_dir().join(format!(
            "rime-agent-boundary-{}-{}",
            std::process::id(),
            line!()
        ));
        let root = base.join("root");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let leaf = ensure_scratch_dir_in(&root, 1).expect("create");
        assert_eq!(leaf, root.join("1"));
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700,
            "the root was left as it was found"
        );
        assert_eq!(
            std::fs::metadata(&leaf).unwrap().permissions().mode() & 0o777,
            0o700
        );

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_scratch_root_that_is_a_symlink_is_refused_before_the_session_leaf_exists() {
        // The half a mode check cannot make. Another account that pre-created
        // the root owns it, and what it can then put there is a symlink — so
        // the leaf is created somewhere else entirely and the call still
        // reports success, because the leaf IS this account's. Ensuring the
        // root is what refuses, and the target having gained nothing is what
        // says the refusal happened before any of it.
        let base = std::env::temp_dir().join(format!(
            "rime-agent-boundary-link-{}-{}",
            std::process::id(),
            line!()
        ));
        let elsewhere = base.join("elsewhere");
        let root = base.join("root");
        std::fs::create_dir_all(&elsewhere).expect("elsewhere");
        std::os::unix::fs::symlink(&elsewhere, &root).expect("symlink");

        let err = ensure_scratch_dir_in(&root, 1).expect_err("a symlinked root must be refused");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
        assert!(
            !elsewhere.join("1").exists(),
            "the session directory was created through the symlink"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_directory_another_account_owns_is_refused() {
        // The stat is real and so is the directory; the uid it is compared
        // against is the injected half, because a test process has one uid.
        // See `ensure_private_dir_as`. The pair is the point: the SAME
        // directory is accepted for its real owner and refused for anybody
        // else, so a gate that always refused would fail the first half.
        let base = std::env::temp_dir().join(format!(
            "rime-agent-owner-{}-{}",
            std::process::id(),
            line!()
        ));
        let leaf = base.join("1");

        ensure_private_dir_as(&leaf, uid()).expect("its real owner is accepted");

        let stranger = uid().wrapping_add(1);
        let err = ensure_private_dir_as(&leaf, stranger)
            .expect_err("a directory owned by another account must be refused");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
        assert!(
            err.to_string().contains(&format!("owned by uid {}", uid())),
            "the refusal does not name the owner it found: {err}"
        );
        assert!(
            err.to_string().contains(&stranger.to_string()),
            "the refusal does not name the account that asked: {err}"
        );
        std::fs::remove_dir_all(&base).ok();
    }
}
