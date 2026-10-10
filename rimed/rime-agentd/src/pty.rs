//! PTY creation and process spawning.
//!
//! The roadmap is explicit that the PTY is the primitive: Rime creates the
//! terminal, the sandbox and the environment, then launches the *normal* agent
//! binary inside it. The agent still sees an ordinary terminal, which is why
//! persistence, reattachment and status work without any cooperation from
//! upstream — and why nothing here scrapes terminal pixels.

use std::ffi::{CString, OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, OnceLock};

use anyhow::{bail, Context, Result};
use rime_agent_core::term::WinSize;

/// A spawned session: the master side of its PTY and its process ids.
#[derive(Debug)]
pub struct Spawned {
    /// Master side. The daemon owns this; the child never sees it.
    pub master: RawFd,
    /// The child process id.
    pub pid: libc::pid_t,
    /// The child's process group, which is its own because it calls `setsid`.
    ///
    /// Signals go to the group, not the process, so `rime agent pause` stops
    /// the agent *and* everything it started — including through the sandbox's
    /// PID namespace, which does not hide group membership from the host.
    pub pgid: libc::pid_t,
}

/// Launch `argv` on a new PTY.
///
/// `clear_env` unsets the names in `env` before setting them, so those names
/// cannot arrive with an inherited value. It does NOT clear the environment —
/// the implementation below says so, and it is MEASURED: an unconfined session
/// started from this daemon inherits every other variable the daemon holds, 80
/// of them on a developer's machine, `HOME` and `PATH` among them.
///
/// So this is not the `--clearenv` treatment a confined session gets, and it
/// is not default-deny. A confined session's environment is bwrap's, built
/// from `--setenv` alone; an unconfined session is the documented escape hatch
/// and its environment is the daemon's. Whoever narrows this must check
/// `disposable::engine_env`, which exists because of it.
///
/// **The fork happens on [`spawner`]'s thread, never the caller's.** A confined
/// session is `bwrap --die-with-parent`, which is `PR_SET_PDEATHSIG(SIGKILL)`,
/// and Linux fires that when the THREAD that forked the process exits — not
/// the process (prctl(2)). The daemon serves every connection on a thread of
/// its own, so forking here from the caller had a confined session SIGKILLed
/// the moment the client that started it hung up: `rime agent run -d`
/// returning was enough. It was a race — a connection thread that exited
/// before bwrap reached its `prctl` left bwrap reparented and safe — so it
/// passed on a fast machine and failed `tests/test-secret-broker.sh` on CI
/// with one transcript line and "killed by signal 9". rime-aid hit the same
/// kernel rule and solved it the same way (`rime-aid/src/main.rs`,
/// `Daemon::spawn`). `tests/spawner_thread.rs` pins it.
// Sessions start through `spawn_held`; this is the run-it-now form the unit
// tests use, so outside them nothing calls it.
#[cfg_attr(not(test), allow(dead_code))]
pub fn spawn(
    argv: &[String],
    cwd: &Path,
    env: &[(String, String)],
    clear_env: bool,
    no_new_privs: bool,
    size: WinSize,
) -> Result<Spawned> {
    let (spawned, held) = spawn_held(argv, cwd, env, clear_env, no_new_privs, size)?;
    if let Err(e) = held.release() {
        // Safe: the master is ours and nothing else has seen it.
        unsafe { libc::close(spawned.master) };
        return Err(e);
    }
    Ok(spawned)
}

/// [`spawn`], stopped just before the program runs.
///
/// The child is forked, on its terminal, in its own process group and with
/// every pre-exec step done, then waits for [`Held::release`] before it
/// `execve`s. A session is started this way so the daemon can record it
/// first: a connection is attributed to a session by walking its `/proc`
/// ancestry to a pid the registry holds, and a program that ran before its
/// pid was recorded had its first requests attributed to nobody, which
/// `may_be_granted` then took for the human at the terminal. Dropping the
/// [`Held`] without releasing it kills the child: a session that was never
/// recorded never runs.
pub fn spawn_held(
    argv: &[String],
    cwd: &Path,
    env: &[(String, String)],
    clear_env: bool,
    no_new_privs: bool,
    size: WinSize,
) -> Result<(Spawned, Held)> {
    let (argv, cwd, env) = (argv.to_vec(), cwd.to_path_buf(), env.to_vec());
    on_spawner_thread(move || spawn_here(&argv, &cwd, &env, clear_env, no_new_privs, size))
}

/// A child parked before `execve`. See [`spawn_held`].
#[derive(Debug)]
pub struct Held {
    pid: libc::pid_t,
    /// Write end of the release pipe; one byte lets the child exec.
    go: RawFd,
    /// Read end of the exec sync pipe, for the exec-or-fail answer.
    sync: RawFd,
}

impl Held {
    /// Let the program run, and wait until it has exec'd or failed to.
    ///
    /// Releasing only moves the wait [`spawn`] has always done: on `Ok` the
    /// program is running; on `Err` the child is reaped and nothing runs.
    pub fn release(mut self) -> Result<()> {
        let (pid, go, sync) = (self.pid, self.go, self.sync);
        self.pid = -1;
        let byte = 1u8;
        // Safe: a one-byte write to, then a close of, a descriptor we own.
        let wrote = unsafe {
            let n = libc::write(go, &byte as *const u8 as *const libc::c_void, 1);
            libc::close(go);
            n
        };
        if wrote != 1 {
            let err = std::io::Error::last_os_error();
            reap(pid, true);
            unsafe { libc::close(sync) };
            return Err(err).context("releasing the agent process");
        }
        let outcome = read_sync(sync, EXEC_DEADLINE_MS);
        unsafe { libc::close(sync) };
        exec_outcome(pid, outcome)
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if self.pid > 0 {
            reap(self.pid, true);
            // Safe: closing descriptors we own; release() marks them spent.
            unsafe {
                libc::close(self.go);
                libc::close(self.sync);
            }
        }
    }
}

/// Wait for `pid` to exit, killing it first when `kill` is set.
fn reap(pid: libc::pid_t, kill: bool) {
    // Safe: signals and reaps our own child.
    unsafe {
        if kill {
            libc::kill(pid, libc::SIGKILL);
        }
        let mut status: libc::c_int = 0;
        libc::waitpid(pid, &mut status, 0);
    }
}

/// What a sync-pipe answer means for a child that was released.
fn exec_outcome(pid: libc::pid_t, outcome: Exec) -> Result<()> {
    match outcome {
        Exec::Started => Ok(()),
        Exec::Failed(stage, errno) => {
            // Reap the child that is already on its way out, so it does not
            // linger as a zombie for a session that never started.
            reap(pid, false);
            Err(std::io::Error::from_raw_os_error(errno)).context(stage_name(stage).to_string())
        }
        Exec::Stuck => {
            // The child is wedged somewhere before exec and will never say so.
            // Kill it: a child that never execs never runs FD_CLOEXEC, so for
            // as long as it lives it holds open every descriptor it inherited
            // at fork — this process's pipes, sockets and terminals included.
            // Leaving it alive is what turns one stuck spawn into unrelated
            // reads elsewhere in the program that never see end-of-file.
            reap(pid, true);
            bail!(
                "the agent process was still not running {EXEC_DEADLINE_MS} ms \
                 after it was forked, so it was killed; it never reached the \
                 program, and nothing was started"
            )
        }
    }
}

fn stage_name(stage: u8) -> &'static str {
    match stage {
        STAGE_LOGIN_TTY => "attaching the agent to its terminal",
        STAGE_CHDIR => "entering the working directory",
        STAGE_NO_NEW_PRIVS => "locking the session out of privilege escalation",
        STAGE_HELD => "waiting to be released",
        _ => "starting the agent program",
    }
}

/// A unit of work for the spawner thread.
type Job = Box<dyn FnOnce() + Send + 'static>;

/// The one thread every session is forked from. It is started on first use
/// and NEVER exits: its job loop reads from a channel whose sender lives in a
/// static, so the loop has no end, and a panicking job is caught rather than
/// allowed to unwind the thread — because that thread exiting is exactly the
/// event that kills every session forked from it.
fn spawner() -> Result<&'static mpsc::Sender<Job>> {
    static SPAWNER: OnceLock<std::result::Result<mpsc::Sender<Job>, String>> = OnceLock::new();
    SPAWNER
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Job>();
            std::thread::Builder::new()
                .name("rime-agentd-spawn".into())
                .spawn(move || {
                    for job in rx {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                    }
                })
                .map(|_| tx)
                .map_err(|e| format!("starting the session spawner thread: {e}"))
        })
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Run `f` on the spawner thread and wait for its result.
///
/// There is deliberately no fallback to running `f` here: forking from the
/// caller's thread is the defect this exists to prevent, and a session that
/// dies when its client hangs up is worse than one that refuses to start.
fn on_spawner_thread<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    spawner()?
        .send(Box::new(move || {
            let _ = reply_tx.send(f());
        }))
        .map_err(|_| anyhow::anyhow!("the session spawner thread is gone"))?;
    reply_rx
        .recv()
        .map_err(|_| anyhow::anyhow!("the session spawner thread dropped the request (it panicked)"))?
}

/// [`spawn`], on the calling thread. Only [`spawn`] calls this, and only from
/// the spawner thread.
fn spawn_here(
    argv: &[String],
    cwd: &Path,
    env: &[(String, String)],
    clear_env: bool,
    no_new_privs: bool,
    size: WinSize,
) -> Result<(Spawned, Held)> {
    if argv.is_empty() {
        bail!("no program to run");
    }

    // Everything that can allocate or fail must happen *before* fork: between
    // fork and exec only async-signal-safe calls are legal, and a failed
    // allocation there would be undebuggable.
    let c_argv: Vec<CString> = argv
        .iter()
        .map(|a| CString::new(a.as_bytes()))
        .collect::<std::result::Result<_, _>>()
        .context("an argument contained a NUL byte")?;
    let mut argv_ptrs: Vec<*const libc::c_char> =
        c_argv.iter().map(|s| s.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());

    let c_cwd = CString::new(cwd.as_os_str().as_bytes()).context("cwd contained a NUL byte")?;

    // The child's entire environment, assembled HERE, because the child cannot
    // assemble it. See `child_env`: the calls that used to build it after the
    // fork are the defect this file was rewritten to remove.
    let c_envp = child_env(env, clear_env).context("building the agent environment")?;
    let mut envp_ptrs: Vec<*const libc::c_char> = c_envp.iter().map(|s| s.as_ptr()).collect();
    envp_ptrs.push(std::ptr::null());

    // Every path the child will try, in `execvp`'s order, resolved against the
    // PATH the child is about to be given rather than this process's own.
    let candidates = program_candidates(&argv[0], cwd, path_of(&c_envp));
    let c_candidates: Vec<CString> = candidates
        .iter()
        .map(|c| CString::new(c.as_os_str().as_bytes()))
        .collect::<std::result::Result<_, _>>()
        .context("a program path contained a NUL byte")?;
    let candidate_ptrs: Vec<*const libc::c_char> =
        c_candidates.iter().map(|s| s.as_ptr()).collect();

    let ws = libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    // An empty signal mask for the child, built before the fork so the child
    // only has to apply it.
    //
    // This matters more than it looks. A process's blocked-signal mask is
    // inherited across fork *and preserved across execve* — unlike signal
    // dispositions, which exec resets. The daemon blocks SIGTERM, SIGINT and
    // SIGHUP so its signal thread can wait on them, and without clearing that
    // here every agent session inherited the block: `rime agent kill` did
    // nothing, and ctrl-C inside an attached agent was swallowed.
    let empty_mask: libc::sigset_t = unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        set
    };

    // Synchronisation pipe. The child holds the write end with FD_CLOEXEC, so
    // a successful execvp closes it and the parent's read sees end-of-file;
    // any failure before exec writes its errno down it instead.
    //
    // This is not only for error reporting. The child becomes a session leader
    // inside login_tty, which happens *after* fork returns in the parent —
    // without waiting for it, an `rime agent kill` issued immediately after
    // `run` would call killpg on a process group that does not exist yet, get
    // ESRCH, and silently do nothing while the agent kept running.
    let mut sync_fds = [0 as RawFd; 2];
    // Safe: pipe2 writes two descriptors into an array we own.
    if unsafe { libc::pipe2(sync_fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error()).context("creating the exec sync pipe");
    }
    let (sync_read, sync_write) = (sync_fds[0], sync_fds[1]);

    // Release pipe. The child parks on its read end just before execve and
    // runs the program only when a byte arrives: see `spawn_held`. End-of-file
    // instead (the parent dropped the `Held`, or died) means the session was
    // never recorded, and the child exits without running anything.
    let mut go_fds = [0 as RawFd; 2];
    // Safe: pipe2 writes two descriptors into an array we own.
    if unsafe { libc::pipe2(go_fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        let err = std::io::Error::last_os_error();
        unsafe {
            libc::close(sync_read);
            libc::close(sync_write);
        }
        return Err(err).context("creating the release pipe");
    }
    let (go_read, go_write) = (go_fds[0], go_fds[1]);

    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    // Safe: openpty writes two descriptors we own and reads the winsize we
    // just built. Passing null for name and termios takes the defaults.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &ws,
        )
    };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        unsafe {
            libc::close(sync_read);
            libc::close(sync_write);
            libc::close(go_read);
            libc::close(go_write);
        }
        return Err(err).context("allocating a pseudo-terminal");
    }

    // Safe: fork with no allocation in the child path below.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        let err = std::io::Error::last_os_error();
        unsafe {
            libc::close(master);
            libc::close(slave);
            libc::close(sync_read);
            libc::close(sync_write);
            libc::close(go_read);
            libc::close(go_write);
        }
        return Err(err).context("forking the agent process");
    }

    if pid == 0 {
        // ── child ────────────────────────────────────────────────────────
        // No allocation, no Rust I/O, no panicking: only raw syscalls.
        unsafe {
            libc::close(master);
            libc::close(sync_read);
            libc::close(go_write);

            // Report a pre-exec failure to the parent and stop. The parent
            // distinguishes this from a successful exec by the pipe carrying
            // bytes instead of closing empty.
            let fail = |stage: u8, code: i32| -> ! {
                // Read here rather than passed in: every caller has just made
                // the failing call, and the exec loop below sets it explicitly.
                let errno = *libc::__errno_location();
                let msg = [stage, errno as u8, (errno >> 8) as u8];
                libc::write(sync_write, msg.as_ptr() as *const libc::c_void, msg.len());
                libc::_exit(code)
            };

            // Becomes a session leader, makes the slave its controlling
            // terminal, and wires it to stdin/stdout/stderr. This is what makes
            // the process group its own, and therefore signalable as a unit.
            if libc::login_tty(slave) != 0 {
                fail(STAGE_LOGIN_TTY, 126);
            }

            if libc::chdir(c_cwd.as_ptr()) != 0 {
                fail(STAGE_CHDIR, 125);
            }

            // The kernel half of "unrestricted-user does not imply root"
            // (§4.3, §3.3). With PR_SET_NO_NEW_PRIVS set, execve stops
            // honouring a setuid bit or a file capability, so sudo, su and
            // pkexec still run inside the session but come up unprivileged and
            // fail — and the flag is inherited by every descendant and cannot
            // be cleared, which is what makes it a boundary rather than a
            // setting.
            //
            // A confined session already has it: bwrap sets it unconditionally
            // (measured — NoNewPrivs is 1 inside and 0 outside). Setting it
            // here is what extends the same property to an unconfined one,
            // which is the case §4.3 is about. Doing it for both is deliberate:
            // the policy decides, not the sandbox mode, so a future sandbox
            // that stops setting it cannot silently take this with it.
            //
            // Failure is fatal rather than ignored. A session that reported
            // no_new_privs and did not have it would be exactly the silent
            // downgrade the rest of this file refuses to perform.
            if no_new_privs && libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                fail(STAGE_NO_NEW_PRIVS, 124);
            }

            // Default the dispositions the daemon changed. exec resets handlers
            // but not SIG_IGN, and an inherited ignored SIGPIPE changes how
            // every pipeline the agent runs behaves.
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            libc::signal(libc::SIGCHLD, libc::SIG_DFL);
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::signal(libc::SIGTERM, libc::SIG_DFL);
            libc::signal(libc::SIGHUP, libc::SIG_DFL);

            // Unblock everything. exec does NOT clear the signal mask, so
            // without this the agent inherits the daemon's blocked SIGTERM and
            // cannot be killed or interrupted.
            libc::sigprocmask(libc::SIG_SETMASK, &empty_mask, std::ptr::null_mut());

            // Parked: every step above is done (so the process group exists),
            // and the program does not run until the daemon has recorded the
            // session and sends one byte. write and read are async-signal-safe.
            let held = [STAGE_HELD, 0u8, 0u8];
            libc::write(sync_write, held.as_ptr() as *const libc::c_void, held.len());
            let mut go = 0u8;
            loop {
                let n = libc::read(go_read, &mut go as *mut u8 as *mut libc::c_void, 1);
                if n == 1 {
                    break;
                }
                if n < 0 && *libc::__errno_location() == libc::EINTR {
                    continue;
                }
                // Never released: the session was not recorded, so its
                // program must not run.
                libc::_exit(122);
            }

            // The environment arrives as execve's third argument, and the PATH
            // search already happened in the parent. THIS IS THE WHOLE POINT OF
            // THE FILE'S SHAPE and it is not a style preference:
            //
            // Between fork and exec, a child of a MULTI-THREADED process may
            // call only async-signal-safe functions. Every other thread is gone
            // in the child, but the locks they held are not: they are copied in
            // the locked state and nothing will ever unlock them. `unsetenv`,
            // `putenv` and `setenv` all take glibc's environment lock, and
            // putenv can reallocate `environ` and so take the malloc lock too.
            // This child used to call unsetenv and putenv here, and when the
            // daemon forked while any other thread was inside setenv, the child
            // blocked in __lll_lock_wait_private and NEVER REACHED EXEC — so
            // FD_CLOEXEC never fired and it held every descriptor it inherited
            // open for as long as it lived, hanging reads in unrelated parts of
            // the program that were waiting for an end-of-file it now pinned.
            //
            // execve is a bare syscall wrapper: no PATH lookup, no getenv, no
            // allocation, no lock. Whoever adds a call here must check it
            // against signal-safety(7) first.
            //
            // One deliberate difference from execvp: a file that is neither ELF
            // nor has a shebang gives ENOEXEC here, where execvp would silently
            // re-run it under /bin/sh. Reviving that would mean building a
            // second argv before the fork; no agent program is in that shape,
            // and a clear ENOEXEC beats an implicit shell.
            //
            // On success this never returns and FD_CLOEXEC closes sync_write,
            // which is what the parent reads as "the agent is running".
            let mut eacces = false;
            // What an empty candidate list reports, matching execvp on an
            // unsearchable PATH.
            *libc::__errno_location() = libc::ENOENT;
            for prog in &candidate_ptrs {
                libc::execve(*prog, argv_ptrs.as_ptr(), envp_ptrs.as_ptr());
                if *libc::__errno_location() == libc::EACCES {
                    eacces = true;
                }
            }
            if eacces {
                // execvp's rule: a candidate we were refused permission to run
                // is more informative than a later one that did not exist.
                *libc::__errno_location() = libc::EACCES;
            }
            // 127 is the shell's convention for "command not found", which is
            // what this almost always is.
            fail(STAGE_EXEC, 127);
        }
    }

    // ── parent ───────────────────────────────────────────────────────────
    // Safe: closing descriptors we own; the child has its own copies.
    unsafe {
        libc::close(slave);
        libc::close(sync_write);
        libc::close(go_read);
    }

    // Block until the child has parked or failed. This is what makes the
    // returned `pgid` real: login_tty's setsid has definitely run by the time
    // the child parks, so a kill issued immediately after this returns cannot
    // race the process group into existence. The exec itself is waited for
    // by `Held::release`.
    let held = Held {
        pid,
        go: go_write,
        sync: sync_read,
    };
    match read_sync(sync_read, EXEC_DEADLINE_MS) {
        Exec::Failed(STAGE_HELD, _) => {}
        // End-of-file before parking cannot be an exec (the child execs only
        // after it parks), so it is a child that died: say so, not "started".
        Exec::Started => {
            drop(held);
            unsafe { libc::close(master) };
            bail!("the agent process exited before it was ready to run its program");
        }
        outcome => {
            // `exec_outcome` reaps the child; the descriptors are closed here.
            let mut held = held;
            held.pid = -1;
            unsafe {
                libc::close(go_write);
                libc::close(sync_read);
                libc::close(master);
            }
            exec_outcome(pid, outcome)?;
            unreachable!("exec_outcome is Ok only for Exec::Started");
        }
    }

    if let Err(e) = set_nonblocking(master).and_then(|()| set_cloexec(master)) {
        drop(held);
        unsafe { libc::close(master) };
        return Err(e);
    }

    Ok((
        Spawned {
            master,
            pid,
            // login_tty called setsid, so the child's group id is its own pid,
            // and the child parking proves it has already happened.
            pgid: pid,
        },
        held,
    ))
}

/// Stage markers written down the sync pipe when the child fails before exec.
const STAGE_LOGIN_TTY: u8 = 1;
const STAGE_CHDIR: u8 = 2;
const STAGE_EXEC: u8 = 3;
const STAGE_NO_NEW_PRIVS: u8 = 4;
/// Not a failure: the child is parked before exec, waiting for its release.
const STAGE_HELD: u8 = 5;

/// How long the parent waits for the child to reach `execve`.
///
/// Between fork and exec the child runs a handful of syscalls, so this is not a
/// performance budget — it is the line between a spawn that reports a failure
/// and one that hangs forever. It exists because the wait below used to be a
/// bare `read` with no deadline: when a child blocked before exec, the parent
/// blocked with it, and a test that hangs reports neither pass nor fail. That
/// cost two agent sessions and four hours of a wedged worktree, where the same
/// defect with a deadline would have been one red test in ten seconds.
const EXEC_DEADLINE_MS: u64 = 10_000;

/// What the sync pipe said about the child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exec {
    /// The pipe closed empty: `FD_CLOEXEC` fired, so exec succeeded.
    Started,
    /// A pre-exec failure the child described: stage marker and errno.
    Failed(u8, i32),
    /// The deadline passed with the child neither exec'd nor failed.
    Stuck,
}

/// Wait for the child to exec or report a failure, for at most `deadline_ms`.
fn read_sync(fd: RawFd, deadline_ms: u64) -> Exec {
    let mut buf = [0u8; 3];
    let mut filled = 0usize;
    let start = std::time::Instant::now();
    while filled < buf.len() {
        let elapsed = start.elapsed().as_millis() as u64;
        if elapsed >= deadline_ms {
            return Exec::Stuck;
        }
        // poll, not a bare blocking read, is the whole point: the deadline has
        // to be enforced by the wait itself. EINTR is retried against the
        // original deadline rather than restarting it, so a signal storm
        // cannot extend the wait indefinitely.
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // Safe: poll reads and writes one pollfd we own.
        let rc = unsafe { libc::poll(&mut pfd, 1, (deadline_ms - elapsed) as i32) };
        if rc == 0 {
            return Exec::Stuck;
        }
        if rc < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            // An unpollable descriptor is not the child's fault; fall through
            // to the same "nothing was reported" reading as an empty pipe.
            break;
        }
        // Readable, hung up, or in error — in every case the answer is to read
        // and let the result speak. A hangup with no bytes is a successful
        // exec, which is exactly what this pipe exists to signal.
        //
        // Safe: read into a buffer we own, bounded by its remaining length.
        let n = unsafe {
            libc::read(
                fd,
                buf.as_mut_ptr().add(filled) as *mut libc::c_void,
                buf.len() - filled,
            )
        };
        if n > 0 {
            filled += n as usize;
            continue;
        }
        if n == 0 {
            break;
        }
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        break;
    }
    if filled < buf.len() {
        return Exec::Started;
    }
    let errno = i32::from(buf[1]) | (i32::from(buf[2]) << 8);
    Exec::Failed(buf[0], errno)
}

/// Put a descriptor into non-blocking mode.
pub fn set_nonblocking(fd: RawFd) -> Result<()> {
    // Safe: F_GETFL/F_SETFL only read and write this descriptor's flags.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            return Err(std::io::Error::last_os_error()).context("reading descriptor flags");
        }
        if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(std::io::Error::last_os_error()).context("setting O_NONBLOCK");
        }
    }
    Ok(())
}

/// Close a descriptor on exec, so a later session never inherits an earlier
/// session's PTY.
pub fn set_cloexec(fd: RawFd) -> Result<()> {
    // Safe: F_GETFD/F_SETFD only read and write this descriptor's flags.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags < 0 {
            return Err(std::io::Error::last_os_error()).context("reading descriptor flags");
        }
        if libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
            return Err(std::io::Error::last_os_error()).context("setting FD_CLOEXEC");
        }
    }
    Ok(())
}

/// Read from a non-blocking descriptor.
///
/// A PTY master reports `EIO` rather than end-of-file once the last slave
/// descriptor closes, which is the normal way a session ends. Both are reported
/// as `Ok(None)`.
pub fn read_nonblocking(fd: RawFd, buf: &mut [u8]) -> Result<Option<usize>> {
    // Safe: read into a buffer we own, bounded by its length.
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n > 0 {
        return Ok(Some(n as usize));
    }
    if n == 0 {
        return Ok(None);
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::EAGAIN) | Some(libc::EINTR) => Ok(Some(0)),
        // The child closed the terminal: end of session, not a failure.
        Some(libc::EIO) => Ok(None),
        _ => Err(err).context("reading the session terminal"),
    }
}

/// Write to the PTY master, retrying short writes.
pub fn write_all(fd: RawFd, mut buf: &[u8]) -> Result<()> {
    while !buf.is_empty() {
        // Safe: write from a buffer we own, bounded by its length.
        let n = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
        if n > 0 {
            buf = &buf[n as usize..];
            continue;
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EAGAIN) => {
                // The agent is not reading. Wait for writability rather than
                // spinning; a busy loop here would burn a core whenever a TUI
                // paused its input.
                if !wait_writable(fd, 100) {
                    continue;
                }
            }
            _ => return Err(err).context("writing to the session terminal"),
        }
    }
    Ok(())
}

/// Wait until `fd` is readable or `timeout_ms` elapses. True when readable.
pub fn wait_readable(fd: RawFd, timeout_ms: i32) -> bool {
    poll_one(fd, libc::POLLIN, timeout_ms)
}

/// Wait until `fd` is writable or `timeout_ms` elapses. True when writable.
pub fn wait_writable(fd: RawFd, timeout_ms: i32) -> bool {
    poll_one(fd, libc::POLLOUT, timeout_ms)
}

fn poll_one(fd: RawFd, events: libc::c_short, timeout_ms: i32) -> bool {
    let mut pfd = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    // Safe: poll reads and writes one pollfd we own.
    let rc = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    // POLLHUP and POLLERR also mean "stop waiting": the caller's next read
    // will see the end of the session.
    rc > 0 && (pfd.revents & (events | libc::POLLHUP | libc::POLLERR)) != 0
}

/// How a child process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// Still running.
    Running,
    /// Exited with this status.
    Exited(i32),
    /// Killed by this signal.
    Signalled(i32),
    /// Already reaped, or never ours.
    Gone,
}

/// Reap `pid` without blocking.
pub fn try_wait(pid: libc::pid_t) -> Wait {
    let mut status: libc::c_int = 0;
    // Safe: waitpid writes one int we own.
    let rc = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    if rc == 0 {
        return Wait::Running;
    }
    if rc < 0 {
        return Wait::Gone;
    }
    decode_status(status)
}

/// Decode a `waitpid` status word.
pub fn decode_status(status: libc::c_int) -> Wait {
    // libc::WIFEXITED and friends are macros in C; the crate exposes them as
    // functions with the same semantics.
    if libc::WIFEXITED(status) {
        Wait::Exited(libc::WEXITSTATUS(status))
    } else if libc::WIFSIGNALED(status) {
        Wait::Signalled(libc::WTERMSIG(status))
    } else {
        Wait::Running
    }
}

/// Send a signal to a session's whole process group.
///
/// The group, not the process: an agent that started a build must have the
/// build stopped with it, or `rime agent pause` would leave a compiler running.
pub fn signal_group(pgid: libc::pid_t, signal: i32) -> Result<()> {
    if pgid <= 1 {
        bail!("refusing to signal process group {pgid}");
    }
    // Safe: killpg only delivers a signal.
    if unsafe { libc::killpg(pgid, signal) } != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            // Already gone. Not a failure worth reporting to the user.
            return Ok(());
        }
        return Err(err).context("signalling the session");
    }
    Ok(())
}

/// Resize a session's terminal.
pub fn resize(master: RawFd, size: WinSize) -> Result<()> {
    rime_agent_core::term::set_window_size(master, size).context("resizing the session terminal")
}

/// Close a descriptor, ignoring failure.
pub fn close(fd: RawFd) {
    if fd >= 0 {
        // Safe: closing a descriptor we own exactly once.
        unsafe { libc::close(fd) };
    }
}

/// A PTY pair with no process on it: `(master, slave)`, the slave in raw
/// mode and non-blocking.
///
/// For tests of what the DAEMON does to a terminal — its size, and the bytes
/// and their timing that reach the program's side — which need a real line
/// discipline and nothing else. No fork, so it works where [`spawn`] cannot
/// (a process under `no_new_privs`, a runner that refuses `setsid`), and there
/// is no child to reap or to outlive the test.
///
/// Raw mode for the reason a real agent is in it: Claude Code reads its
/// terminal raw, so a CR arrives as CR and a read returns whatever has been
/// written, with no line to wait for. `None` when the machine has no
/// `/dev/ptmx`, and the caller says it skipped.
#[cfg(test)]
pub(crate) fn bare_pair() -> Option<(RawFd, RawFd)> {
    // Safe throughout: libc calls on descriptors this function owns, with
    // buffers it owns; every failure path closes what it opened.
    unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
        if master < 0 {
            return None;
        }
        if libc::grantpt(master) != 0 || libc::unlockpt(master) != 0 {
            libc::close(master);
            return None;
        }
        let mut name = [0 as libc::c_char; 128];
        if libc::ptsname_r(master, name.as_mut_ptr(), name.len()) != 0 {
            libc::close(master);
            return None;
        }
        let slave = libc::open(
            name.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC | libc::O_NONBLOCK,
        );
        if slave < 0 {
            libc::close(master);
            return None;
        }
        let mut tio: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(slave, &mut tio) != 0 {
            libc::close(slave);
            libc::close(master);
            return None;
        }
        libc::cfmakeraw(&mut tio);
        if libc::tcsetattr(slave, libc::TCSANOW, &tio) != 0 {
            libc::close(slave);
            libc::close(master);
            return None;
        }
        Some((master, slave))
    }
}

/// Build the environment block the child will exec with.
///
/// This is the daemon's own environment with `overrides` applied — which is
/// what the child ended up with when it called `putenv` for itself, and is
/// documented and measured on `spawn`: an unconfined session inherits every
/// other variable the daemon holds.
///
/// It is built in the parent for one reason: after a fork, a child of a
/// multi-threaded process may not touch the environment at all. `std::env` here
/// is safe because this process still has all its threads, and Rust's own lock
/// serialises this read against any `set_var` running beside it.
///
/// `clear_env` is honoured as `unsetenv`-then-`putenv` honoured it: every
/// inherited entry for that name is dropped before ours is added, so a
/// duplicated name cannot shadow it. Without it the first entry is replaced in
/// place. The two differ only for an environment block that carries the same
/// name twice, which is why callers could never tell them apart.
fn child_env(overrides: &[(String, String)], clear_env: bool) -> Result<Vec<CString>> {
    let mut block: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    for (k, v) in overrides {
        let key = OsString::from(k);
        let val = OsString::from(v);
        if clear_env {
            block.retain(|(name, _)| name != &key);
            block.push((key, val));
        } else if let Some(slot) = block.iter_mut().find(|(name, _)| name == &key) {
            slot.1 = val;
        } else {
            block.push((key, val));
        }
    }
    block
        .iter()
        .map(|(k, v)| {
            let mut entry = k.clone();
            entry.push("=");
            entry.push(v);
            CString::new(entry.as_os_str().as_bytes())
        })
        .collect::<std::result::Result<_, _>>()
        .context("an environment entry contained a NUL byte")
}

/// The value of `PATH` inside an already-built environment block.
///
/// The child's PATH, not the daemon's: a caller that overrides PATH for a
/// session expects the session's program to be found on it, and after the fork
/// nothing can call `getenv` to find that out.
fn path_of(block: &[CString]) -> Option<&OsStr> {
    block.iter().find_map(|entry| {
        let bytes = entry.as_bytes();
        bytes.strip_prefix(b"PATH=").map(OsStr::from_bytes)
    })
}

/// Every path `execvp` would have tried for `program`, in order.
///
/// Resolved here because the search reads the environment, and reading the
/// environment after a fork is precisely the lock the child cannot take. The
/// child tries these with plain `execve` instead, which is the same sequence of
/// exec attempts execvp would have made.
///
/// Relative candidates are joined to `cwd`, which is where the child chdirs
/// before exec — so this is the same set it would have resolved for itself, and
/// an empty PATH entry keeps meaning "the working directory" as POSIX says.
fn program_candidates(program: &str, cwd: &Path, path: Option<&OsStr>) -> Vec<PathBuf> {
    let absolute = |p: PathBuf| if p.is_absolute() { p } else { cwd.join(p) };
    if program.contains('/') {
        return vec![absolute(PathBuf::from(program))];
    }
    // What execvp falls back to when PATH is unset: confstr(_CS_PATH).
    let path = path.unwrap_or_else(|| OsStr::new("/bin:/usr/bin"));
    std::env::split_paths(path)
        .map(|dir| absolute(dir.join(program)))
        .collect()
}

/// Resolve a program name the way `execvp` will, so a missing binary is
/// reported as a clear error at request time instead of as exit code 127
/// several seconds later inside a PTY nobody is watching yet.
pub fn resolve_program(program: &str) -> Option<std::path::PathBuf> {
    let p = Path::new(program);
    if program.contains('/') {
        return is_executable(p).then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    let c = match CString::new(path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return false,
    };
    // Safe: access only reads the path we just built.
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_word_decodes_to_the_right_outcome() {
        // Encoded the way the kernel does: low byte holds the signal, second
        // byte the exit status.
        assert_eq!(decode_status(0), Wait::Exited(0));
        assert_eq!(decode_status(3 << 8), Wait::Exited(3));
        assert_eq!(decode_status(libc::SIGKILL), Wait::Signalled(libc::SIGKILL));
    }

    #[test]
    fn refusing_to_signal_init_or_the_whole_world() {
        assert!(signal_group(0, libc::SIGTERM).is_err());
        assert!(signal_group(1, libc::SIGTERM).is_err());
        assert!(signal_group(-1, libc::SIGTERM).is_err());
    }

    #[test]
    fn an_absolute_program_resolves_only_when_executable() {
        assert_eq!(
            resolve_program("/bin/sh"),
            Some(std::path::PathBuf::from("/bin/sh"))
        );
        assert_eq!(resolve_program("/nonexistent/binary"), None);
        assert_eq!(resolve_program("/etc/hostname"), None, "not executable");
    }

    #[test]
    fn a_bare_name_resolves_through_path() {
        let sh = resolve_program("sh").expect("sh must be on PATH");
        assert!(sh.is_absolute());
        assert!(sh.ends_with("sh"));
        assert_eq!(resolve_program("definitely-not-a-real-binary-xyz"), None);
    }

    #[test]
    fn spawning_runs_the_real_program_on_a_real_terminal() {
        // The load-bearing property: the child must believe it has a terminal,
        // because every TUI agent behaves differently when it does not.
        let spawned = spawn(
            &[
                "/bin/sh".to_string(),
                "-c".to_string(),
                "test -t 1 && echo IS_A_TTY; exit 7".to_string(),
            ],
            Path::new("/tmp"),
            &[("TERM".to_string(), "xterm-256color".to_string())],
            true,
            true,
            WinSize {
                cols: 100,
                rows: 40,
            },
        )
        .expect("spawn");

        let mut collected = Vec::new();
        let mut buf = [0u8; 1024];
        for _ in 0..200 {
            wait_readable(spawned.master, 25);
            match read_nonblocking(spawned.master, &mut buf) {
                Ok(Some(0)) => {}
                Ok(Some(n)) => collected.extend_from_slice(&buf[..n]),
                // End of the terminal. The child may still not be reaped: the
                // PTY closing and waitpid reporting the exit are two separate
                // events and they race.
                Ok(None) | Err(_) => break,
            }
        }
        let status = wait_for_exit(spawned.pid);
        close(spawned.master);

        let text = String::from_utf8_lossy(&collected);
        assert!(text.contains("IS_A_TTY"), "child saw no tty: {text:?}");
        assert_eq!(status, Wait::Exited(7), "exit status was not propagated");
    }

    /// Reap `pid`, tolerating the gap between the PTY closing and the kernel
    /// making the exit status available.
    fn wait_for_exit(pid: libc::pid_t) -> Wait {
        for _ in 0..500 {
            match try_wait(pid) {
                Wait::Running => std::thread::sleep(std::time::Duration::from_millis(10)),
                other => return other,
            }
        }
        Wait::Running
    }

    #[test]
    fn the_child_gets_its_own_process_group() {
        // Required for signal_group: without setsid the group would be the
        // daemon's, and pausing a session would pause the daemon.
        let spawned = spawn(
            &["/bin/sh".to_string(), "-c".to_string(), "sleep 5".to_string()],
            Path::new("/tmp"),
            &[],
            true,
            true,
            WinSize::FALLBACK,
        )
        .expect("spawn");

        assert_eq!(spawned.pgid, spawned.pid);
        assert_ne!(spawned.pgid, unsafe { libc::getpgrp() });

        signal_group(spawned.pgid, libc::SIGKILL).expect("kill");
        let outcome = wait_for_exit(spawned.pid);
        close(spawned.master);
        assert_eq!(outcome, Wait::Signalled(libc::SIGKILL));
    }

    #[test]
    fn the_requested_window_size_reaches_the_child() {
        let spawned = spawn(
            &[
                "/bin/sh".to_string(),
                "-c".to_string(),
                "stty size 2>/dev/null || echo no-stty".to_string(),
            ],
            Path::new("/tmp"),
            &[("TERM".to_string(), "xterm".to_string())],
            true,
            true,
            WinSize {
                cols: 132,
                rows: 43,
            },
        )
        .expect("spawn");

        let mut collected = Vec::new();
        let mut buf = [0u8; 512];
        for _ in 0..200 {
            wait_readable(spawned.master, 25);
            match read_nonblocking(spawned.master, &mut buf) {
                Ok(Some(0)) => {}
                Ok(Some(n)) => collected.extend_from_slice(&buf[..n]),
                _ => break,
            }
            if String::from_utf8_lossy(&collected).contains('\n') {
                break;
            }
        }
        close(spawned.master);
        let text = String::from_utf8_lossy(&collected);
        assert!(text.contains("43 132"), "stty reported {text:?}");
    }

    #[test]
    fn a_missing_program_is_reported_before_it_is_spawned() {
        assert_eq!(resolve_program("definitely-not-a-real-binary-xyz"), None);
    }

    #[test]
    fn a_failed_exec_is_an_error_not_a_silently_started_session() {
        // Without the sync pipe this returned Ok with a live-looking session
        // whose process had already exited 127 into a terminal nobody was
        // watching yet.
        let err = spawn(
            &["/nonexistent/agent-binary".to_string()],
            Path::new("/tmp"),
            &[],
            true,
            true,
            WinSize::FALLBACK,
        )
        .expect_err("a missing binary must fail the spawn");
        let text = format!("{err:#}");
        assert!(text.contains("starting the agent program"), "{text}");
    }

    #[test]
    fn a_bad_working_directory_is_reported_with_its_own_cause() {
        let err = spawn(
            &["/bin/sh".to_string()],
            Path::new("/nonexistent/directory"),
            &[],
            true,
            true,
            WinSize::FALLBACK,
        )
        .expect_err("a missing cwd must fail the spawn");
        let text = format!("{err:#}");
        assert!(text.contains("entering the working directory"), "{text}");
    }

    #[test]
    fn a_session_does_not_inherit_the_daemons_blocked_signals() {
        // The regression: the daemon blocks SIGTERM/SIGINT/SIGHUP so its signal
        // thread can sigwait on them. That mask survives execve, so every
        // session started with SIGTERM blocked and `rime agent kill` was a
        // no-op — the process sat there with the signal permanently pending.
        //
        // Block them here the way the daemon does, then assert the child comes
        // out clean.
        let mut blocked: libc::sigset_t = unsafe { std::mem::zeroed() };
        let mut previous: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut blocked);
            libc::sigaddset(&mut blocked, libc::SIGTERM);
            libc::sigaddset(&mut blocked, libc::SIGINT);
            libc::sigaddset(&mut blocked, libc::SIGHUP);
            libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut previous);
        }

        let spawned = spawn(
            &[
                "/bin/sh".to_string(),
                "-c".to_string(),
                // SigBlk from the child's own /proc entry, as a hex mask.
                "grep '^SigBlk' /proc/self/status".to_string(),
            ],
            Path::new("/tmp"),
            &[],
            true,
            true,
            WinSize::FALLBACK,
        )
        .expect("spawn");

        // Restore the test process's mask before asserting, so a failure here
        // does not leave the harness with signals blocked.
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut()) };

        let mut collected = Vec::new();
        let mut buf = [0u8; 1024];
        for _ in 0..200 {
            wait_readable(spawned.master, 25);
            match read_nonblocking(spawned.master, &mut buf) {
                Ok(Some(0)) => {}
                Ok(Some(n)) => collected.extend_from_slice(&buf[..n]),
                Ok(None) | Err(_) => break,
            }
            if String::from_utf8_lossy(&collected).contains('\n') {
                break;
            }
        }
        wait_for_exit(spawned.pid);
        close(spawned.master);

        let text = String::from_utf8_lossy(&collected);
        let hex = text
            .split_whitespace()
            .next_back()
            .expect("SigBlk line: {text:?}");
        let mask = u64::from_str_radix(hex.trim(), 16).unwrap_or_else(|e| {
            panic!("cannot parse SigBlk {hex:?} from {text:?}: {e}");
        });

        for (signal, name) in [
            (libc::SIGTERM, "SIGTERM"),
            (libc::SIGINT, "SIGINT"),
            (libc::SIGHUP, "SIGHUP"),
        ] {
            let bit = 1u64 << (signal - 1);
            assert_eq!(
                mask & bit,
                0,
                "{name} is blocked in the session (SigBlk {hex}); it would be unkillable"
            );
        }
    }

    #[test]
    fn a_kill_issued_immediately_after_spawn_reaches_the_session() {
        // The regression this pins: login_tty (and therefore setsid) runs in
        // the child *after* fork returns. Before the sync pipe, killpg on the
        // freshly returned pgid raced that setsid, failed with ESRCH, and was
        // swallowed as success — so `rime agent kill` right after `run`
        // silently did nothing while the agent kept running.
        for attempt in 0..20 {
            let spawned = spawn(
                &[
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "sleep 30".to_string(),
                ],
                Path::new("/tmp"),
                &[],
                true,
                true,
                WinSize::FALLBACK,
            )
            .expect("spawn");

            // No sleep, no poll: signal the group the instant spawn returns.
            signal_group(spawned.pgid, libc::SIGKILL).expect("kill");

            let outcome = wait_for_exit(spawned.pid);
            close(spawned.master);
            assert_eq!(
                outcome,
                Wait::Signalled(libc::SIGKILL),
                "attempt {attempt}: the immediate kill did not reach the session"
            );
        }
    }

    /// The environment is delivered by `execve`, not assembled after the fork.
    ///
    /// If this passes while the child builds its own environment it proves
    /// nothing; read it together with the test below, which is the one that
    /// fails when the child does its own env calls.
    #[test]
    fn the_environment_the_caller_asked_for_reaches_the_program() {
        let spawned = spawn(
            &[
                "/bin/sh".to_string(),
                "-c".to_string(),
                "printf 'MARKER=%s\\n' \"$RIME_PTY_TEST_MARKER\"".to_string(),
            ],
            Path::new("/tmp"),
            &[(
                "RIME_PTY_TEST_MARKER".to_string(),
                "delivered-by-execve".to_string(),
            )],
            true,
            true,
            WinSize::FALLBACK,
        )
        .expect("spawn");

        let mut collected = Vec::new();
        let mut buf = [0u8; 1024];
        for _ in 0..200 {
            wait_readable(spawned.master, 25);
            match read_nonblocking(spawned.master, &mut buf) {
                Ok(Some(0)) => {}
                Ok(Some(n)) => collected.extend_from_slice(&buf[..n]),
                Ok(None) | Err(_) => break,
            }
        }
        wait_for_exit(spawned.pid);
        close(spawned.master);

        let text = String::from_utf8_lossy(&collected);
        assert!(
            text.contains("MARKER=delivered-by-execve"),
            "the session did not get its environment: {text:?}"
        );
    }

    /// THE regression. Read the comment in the child if this ever goes red.
    ///
    /// A child of a multi-threaded process inherits every lock in the state it
    /// was in at fork, with no thread left alive to release it. The child used
    /// to call `unsetenv` and `putenv`, which take glibc's environment lock, so
    /// forking while any other thread sat inside `set_var` wedged the child
    /// before exec — permanently, since nothing there has a deadline. It then
    /// held every descriptor it had inherited, and reads in unrelated tests
    /// hung waiting for an end-of-file that could no longer come. Two agent
    /// sessions and four hours of a worktree went to exactly this.
    ///
    /// So this test does on purpose what `grants::tests` did by accident: it
    /// keeps the environment lock busy while spawning. Against the child that
    /// built its own environment it fails; against one that execs a block built
    /// before the fork it cannot, because the child no longer touches the lock.
    ///
    /// Measured trip rate is recorded in the commit that added this.
    #[test]
    fn spawning_beside_a_thread_that_is_rewriting_the_environment() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        const SPAWNS: usize = 300;

        let stop = Arc::new(AtomicBool::new(false));
        let hammers: Vec<_> = (0..4)
            .map(|i| {
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    // A name nothing else in this process reads or writes, so
                    // the only thing contended here is the lock itself.
                    let name = format!("RIME_PTY_ENV_CONTENTION_{i}");
                    let value = "x".repeat(4096);
                    while !stop.load(Ordering::Relaxed) {
                        // Safe: nothing reads this name, and the environment
                        // reads that race it go through std's own lock.
                        unsafe {
                            std::env::set_var(&name, &value);
                            std::env::remove_var(&name);
                        }
                    }
                })
            })
            .collect();

        let mut failure = None;
        let mut started = 0usize;
        for _ in 0..SPAWNS {
            match spawn(
                &["/bin/true".to_string()],
                Path::new("/tmp"),
                // A non-empty block is what drove the child into unsetenv and
                // putenv at all: with an empty one it took no lock and this
                // test would prove nothing.
                &[("TERM".to_string(), "xterm-256color".to_string())],
                true,
                true,
                WinSize::FALLBACK,
            ) {
                Ok(s) => {
                    wait_for_exit(s.pid);
                    close(s.master);
                    started += 1;
                }
                Err(e) => {
                    failure = Some(format!("{e:#}"));
                    break;
                }
            }
        }

        stop.store(true, Ordering::Relaxed);
        for h in hammers {
            h.join().expect("hammer thread");
        }

        assert!(
            failure.is_none(),
            "after {started} clean spawns, spawn {} never reached the program: {}",
            started + 1,
            failure.unwrap_or_default()
        );
    }

    #[test]
    fn the_child_environment_is_the_daemons_with_the_overrides_applied() {
        let block = child_env(
            &[
                ("RIME_PTY_UNIT_A".to_string(), "one".to_string()),
                ("RIME_PTY_UNIT_B".to_string(), "two".to_string()),
            ],
            true,
        )
        .expect("build");
        let entries: Vec<String> = block
            .iter()
            .map(|c| c.to_string_lossy().into_owned())
            .collect();

        assert!(entries.iter().any(|e| e == "RIME_PTY_UNIT_A=one"));
        assert!(entries.iter().any(|e| e == "RIME_PTY_UNIT_B=two"));
        // Inheritance is the documented, measured behaviour: an unconfined
        // session keeps the daemon's environment. A block that only held the
        // overrides would be a silent --clearenv.
        assert!(
            entries.iter().any(|e| e.starts_with("PATH=")),
            "the inherited environment was dropped: {entries:?}"
        );
        // Exactly once, whatever the caller passed.
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.starts_with("RIME_PTY_UNIT_A="))
                .count(),
            1
        );
        // Every entry is a NUL-free k=v pair, because execve will read it as
        // one and a malformed entry there is not reportable.
        assert!(entries.iter().all(|e| e.contains('=')));
    }

    #[test]
    fn the_program_is_searched_on_the_childs_path_and_from_its_working_dir() {
        let block = child_env(&[("PATH".to_string(), "/opt/one:/opt/two".to_string())], true)
            .expect("build");
        assert_eq!(path_of(&block), Some(std::ffi::OsStr::new("/opt/one:/opt/two")));

        // The caller's PATH, not this process's: a session that overrides PATH
        // expects its program to be found on it.
        assert_eq!(
            program_candidates("agent", Path::new("/work"), path_of(&block)),
            vec![
                std::path::PathBuf::from("/opt/one/agent"),
                std::path::PathBuf::from("/opt/two/agent"),
            ]
        );
        // A name with a slash is not searched, exactly as execvp does not.
        assert_eq!(
            program_candidates("/bin/sh", Path::new("/work"), path_of(&block)),
            vec![std::path::PathBuf::from("/bin/sh")]
        );
        // Relative candidates resolve against the directory the child chdirs
        // to, not the daemon's.
        assert_eq!(
            program_candidates("./agent", Path::new("/work"), path_of(&block)),
            vec![std::path::PathBuf::from("/work/./agent")]
        );
        // An empty PATH entry means the working directory, as POSIX says.
        assert_eq!(
            program_candidates("agent", Path::new("/work"), Some(std::ffi::OsStr::new(""))),
            vec![std::path::PathBuf::from("/work/agent")]
        );
        // No PATH at all falls back to confstr(_CS_PATH), like execvp.
        assert_eq!(
            program_candidates("agent", Path::new("/work"), None),
            vec![
                std::path::PathBuf::from("/bin/agent"),
                std::path::PathBuf::from("/usr/bin/agent"),
            ]
        );
    }

    #[test]
    fn spawning_with_no_argv_is_an_error_not_a_panic() {
        assert!(spawn(&[], Path::new("/tmp"), &[], true, true, WinSize::FALLBACK).is_err());
    }

    /// A program started held must not run until it is released: that gap is
    /// where the daemon records the session, so the program's first
    /// connection is already attributable to it.
    fn held_marker(tag: &str) -> (PathBuf, Vec<String>) {
        let marker = std::env::temp_dir().join(format!(
            "rime-agentd-held-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&marker);
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("touch '{}'", marker.display()),
        ];
        (marker, argv)
    }

    fn appears(path: &Path, within: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + within;
        while std::time::Instant::now() < deadline {
            if path.exists() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        path.exists()
    }

    #[test]
    fn a_held_program_runs_only_once_released() {
        let (marker, argv) = held_marker("release");
        let (spawned, held) =
            spawn_held(&argv, Path::new("/tmp"), &[], false, true, WinSize::FALLBACK)
                .expect("spawn held");
        assert!(
            !appears(&marker, std::time::Duration::from_millis(300)),
            "the program ran before it was released"
        );
        held.release().expect("release");
        assert!(
            appears(&marker, std::time::Duration::from_secs(5)),
            "the program did not run after it was released"
        );
        unsafe {
            let mut status: libc::c_int = 0;
            libc::waitpid(spawned.pid, &mut status, 0);
            libc::close(spawned.master);
        }
        let _ = std::fs::remove_file(&marker);
    }

    #[test]
    fn a_held_program_that_is_never_released_never_runs() {
        let (marker, argv) = held_marker("drop");
        let (spawned, held) =
            spawn_held(&argv, Path::new("/tmp"), &[], false, true, WinSize::FALLBACK)
                .expect("spawn held");
        drop(held);
        // Reaped by the drop: the pid is no longer our child.
        let gone = unsafe { libc::waitpid(spawned.pid, std::ptr::null_mut(), libc::WNOHANG) };
        assert_eq!(gone, -1, "the dropped child was left running or unreaped");
        assert!(
            !appears(&marker, std::time::Duration::from_millis(300)),
            "a program whose session was never recorded ran anyway"
        );
        unsafe { libc::close(spawned.master) };
    }

    #[test]
    fn a_held_program_that_cannot_exec_fails_at_release() {
        let argv = vec!["/nonexistent/rime-agentd-no-such-program".to_string()];
        let (spawned, held) =
            spawn_held(&argv, Path::new("/tmp"), &[], false, true, WinSize::FALLBACK)
                .expect("spawn held");
        let err = held.release().expect_err("exec of a missing program succeeded");
        assert!(format!("{err:#}").contains("No such file"), "{err:#}");
        unsafe { libc::close(spawned.master) };
    }

    /// Read `NoNewPrivs` from a spawned child's own `/proc` entry.
    ///
    /// The child's, not the parent's: the flag is set between fork and exec,
    /// so nothing outside the child can observe it any other way.
    fn spawned_no_new_privs(flag: bool) -> String {
        let spawned = spawn(
            &[
                "/bin/sh".to_string(),
                "-c".to_string(),
                "grep '^NoNewPrivs' /proc/self/status".to_string(),
            ],
            Path::new("/tmp"),
            &[],
            true,
            flag,
            WinSize::FALLBACK,
        )
        .expect("spawn");

        let mut collected = Vec::new();
        let mut buf = [0u8; 512];
        for _ in 0..200 {
            wait_readable(spawned.master, 25);
            match read_nonblocking(spawned.master, &mut buf) {
                Ok(Some(0)) => {}
                Ok(Some(n)) => collected.extend_from_slice(&buf[..n]),
                _ => break,
            }
            if String::from_utf8_lossy(&collected).contains('\n') {
                break;
            }
        }
        close(spawned.master);
        wait_for_exit(spawned.pid);
        String::from_utf8_lossy(&collected).trim().to_string()
    }

    #[test]
    fn no_new_privs_reaches_the_session_and_the_negative_control_proves_it() {
        // P0-004 criterion 3, with kernel teeth: an unconfined session runs
        // with PR_SET_NO_NEW_PRIVS, so execve will not grant it privilege from
        // a setuid binary — sudo, pkexec and su all stop working inside it.
        // "Unrestricted user" is a filesystem and process statement, not a
        // route to root.
        //
        // The negative control comes FIRST, so a probe that could never report
        // a 1 cannot masquerade as a working guard.
        let without = spawned_no_new_privs(false);
        assert!(
            without.contains('0'),
            "the probe never reports an unset flag, so it proves nothing: {without:?}"
        );

        let with = spawned_no_new_privs(true);
        assert!(
            with.contains('1'),
            "the session did not get no_new_privs: {with:?}"
        );
    }

    #[test]
    fn the_flag_survives_into_the_processes_the_session_starts() {
        // The property that makes it a boundary rather than a setting: a
        // process cannot clear PR_SET_NO_NEW_PRIVS, and every descendant
        // inherits it across fork and exec. A session that could shed it in a
        // subshell would have gained nothing from it at all.
        let spawned = spawn(
            &[
                "/bin/sh".to_string(),
                "-c".to_string(),
                "/bin/sh -c \"grep '^NoNewPrivs' /proc/self/status\"".to_string(),
            ],
            Path::new("/tmp"),
            &[],
            true,
            true,
            WinSize::FALLBACK,
        )
        .expect("spawn");

        let mut collected = Vec::new();
        let mut buf = [0u8; 512];
        for _ in 0..200 {
            wait_readable(spawned.master, 25);
            match read_nonblocking(spawned.master, &mut buf) {
                Ok(Some(0)) => {}
                Ok(Some(n)) => collected.extend_from_slice(&buf[..n]),
                _ => break,
            }
            if String::from_utf8_lossy(&collected).contains('\n') {
                break;
            }
        }
        close(spawned.master);
        wait_for_exit(spawned.pid);
        let text = String::from_utf8_lossy(&collected);
        assert!(
            text.contains('1'),
            "a grandchild of the session lost no_new_privs: {text:?}"
        );
    }
}
