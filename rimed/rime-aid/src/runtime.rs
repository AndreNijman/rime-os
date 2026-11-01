//! Finding, confining, starting and stopping the inference runtime.
//!
//! The runtime is **not in the image** — see `rimed_core::ai`'s module docs —
//! so the first job here is to look for one and, when there is none, produce
//! the exact command that installs it. Nothing in this file downloads, builds
//! or unpacks anything: `AGENTS.md` forbids a second package mechanism and P1
//! already shipped both of the ones the hint names.
//!
//! ── Why the backend is confined, and what that buys ────────────────────────
//!
//! The backend is a large C++ program that maps untrusted weights, parses HTTP
//! from anything that can open the API socket, and holds the user's prompts. It
//! needs: its own binary and libraries, one read-only model file, one socket
//! directory, and a GPU device. It does **not** need the network, the user's
//! home, or a writable filesystem.
//!
//! So it is started under `bubblewrap` with `--unshare-net`, and that flag is
//! the load-bearing one. A path-named `AF_UNIX` socket is unaffected by network
//! namespaces — only the abstract namespace is per-netns — so the daemon still
//! reaches the backend through the filesystem while the backend's network is
//! its own.
//!
//! ── What `--unshare-net` does and does NOT do, measured ────────────────────
//!
//! Stated precisely, because the imprecise version invites the wrong test. The
//! backend can still *create* and *bind* an `AF_INET` socket: `--unshare-net`
//! gives it a private network namespace with its own loopback, not a missing
//! network stack. Verified on the katana — a program inside the sandbox bound
//! `127.0.0.1:18099` successfully.
//!
//! What it cannot do is be reached. From the host, in the same run:
//!
//! ```text
//! inside : bind+listen on 127.0.0.1:18099 succeeded
//! host   : connect to 127.0.0.1:18099 -> ConnectionRefusedError
//! host   : ss -ltn 'sport = 18099'     -> 0 lines
//! ```
//!
//! That is the property §14 needs: no port the backend opens is visible or
//! connectable from any other process on the machine, whatever account it runs
//! as. So "no local account can find a port to connect to" holds by
//! construction rather than by the planner having got `--host` right, and this
//! is the second line behind `rimed_core::ai::plan_launch`'s refusal to emit a
//! TCP address. The unit's `RestrictAddressFamilies=AF_UNIX AF_NETLINK` is the
//! third, and that one does remove the ability to create the socket at all.
//!
//! `bubblewrap` is the same security dependency the agent runtime uses, present
//! because flatpak pulls it in and asserted in `Containerfile.base`.
//!
//! ── There is no unconfined mode ─────────────────────────────────────────────
//!
//! This daemon used to run the backend UNCONFINED when `/usr/bin/bwrap` was
//! missing, and say so in `rime ai status`. That was a fail-open path: a
//! program that parses untrusted weights and HTTP, holding the user's prompts,
//! would quietly have had the network and the whole home directory, and the
//! only trace was one line of status nobody asked for. It is gone, and the
//! rule now applied is the one `AGENTS.md` sets for the agent sandbox: a
//! process that cannot be confined as required must not start.
//!
//! So confinement is a value, not a flag. [`require_confinement`] is the only
//! way to obtain a [`Confinement`], it succeeds only when `bwrap` is present,
//! executable, and actually builds the sandbox's namespaces (a probe run with
//! the same flags a backend gets), and [`start`] cannot be called without one.
//! The daemon asks for it before it creates a socket and refuses to start
//! without it; there is no code path left that launches a backend without
//! `bwrap` in front of it.

use std::io;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use rimed_core::ai::{Backend, LaunchPlan};

/// How long to wait for the backend to bind its socket.
///
/// Three minutes. A 4 GiB model already in the page cache is ready in a second
/// or two; the same file cold from a spinning disk, or one being paged into
/// VRAM over PCIe, is minutes. The bound exists so a runtime that will never
/// come up is reported rather than waited on forever — and the wait aborts the
/// moment the child exits, which is the case that actually happens.
const READY_TIMEOUT: Duration = Duration::from_secs(180);

/// How often to try connecting while waiting.
const READY_POLL: Duration = Duration::from_millis(100);

/// How long a stopped backend gets to exit before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// `bubblewrap`. The same path the agent runtime asserts in `Containerfile.base`.
///
/// A fixed path, never a `PATH` lookup: this is the security boundary, and a
/// `PATH`-resolved name would let a shadowing binary decide what "confined"
/// means. Nothing (no environment variable, no setting) can change it.
const BWRAP: &str = "/usr/bin/bwrap";

/// How long the startup probe may take. Building the namespaces and running
/// `true` takes milliseconds; the bound only stops a wedged `bwrap` from
/// wedging the daemon's start with it.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Proof that the backend sandbox can be applied on this machine.
///
/// The field is private, so outside this module the only way to get one is
/// [`require_confinement`], and [`start`] takes one by reference: no proof, no
/// launch. That is the compiler holding the line a `confine: bool` parameter
/// used to leave to every caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confinement {
    bwrap: PathBuf,
}

/// Check that the backend sandbox can be applied, or say exactly why not.
///
/// Called by the daemon before it creates a socket. An `Err` here means the
/// daemon refuses to start; there is no fallback.
pub fn require_confinement() -> Result<Confinement> {
    confinement_at(Path::new(BWRAP))
}

/// [`require_confinement`] against a given `bwrap`, so the refusals can be
/// tested without touching `/usr/bin`. Private on purpose: production code
/// cannot choose a different binary.
fn confinement_at(bwrap: &Path) -> Result<Confinement> {
    use std::os::unix::fs::PermissionsExt;

    let refuse = |why: String| -> anyhow::Error {
        anyhow::anyhow!(
            "refusing to start: the sandbox local models run in cannot be applied ({why}). \
             rime-aid never runs a model unsandboxed. It needs bubblewrap ({}), which ships \
             with Rime OS, and a kernel that lets it create user namespaces",
            bwrap.display()
        )
    };

    let meta = match std::fs::metadata(bwrap) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(refuse(format!("{} is not installed", bwrap.display())))
        }
        Err(e) => return Err(refuse(format!("cannot inspect {}: {e}", bwrap.display()))),
    };
    if !meta.is_file() {
        return Err(refuse(format!("{} is not a regular file", bwrap.display())));
    }
    if meta.permissions().mode() & 0o111 == 0 {
        return Err(refuse(format!("{} is not executable", bwrap.display())));
    }

    // Present is not the same as working. A kernel or policy that refuses
    // unprivileged user namespaces leaves a perfectly good `bwrap` that cannot
    // build a sandbox, so it is RUN, with the namespace flags a backend gets.
    let confinement = Confinement { bwrap: bwrap.to_path_buf() };
    probe(&confinement).map_err(|e| refuse(format!("{e:#}")))?;
    Ok(confinement)
}

/// Run `true` inside the sandbox a backend would get, minus the backend's own
/// paths and devices.
fn probe(c: &Confinement) -> Result<()> {
    let mut argv = sandbox_argv(c);
    argv.push("--".into());
    argv.push("/usr/bin/true".into());
    let (program, args) = argv.split_first().context("an empty probe argv")?;

    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/usr/sbin")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("cannot run {}", c.bwrap.display()))?;

    let deadline = Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().context("waiting for the sandbox probe")? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "{} did not finish building a test sandbox within {}s",
                c.bwrap.display(),
                PROBE_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if status.success() {
        return Ok(());
    }
    let mut err = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        use std::io::Read;
        let _ = pipe.read_to_string(&mut err);
    }
    let err = err.trim();
    bail!(
        "{} could not build a test sandbox ({status}){}",
        c.bwrap.display(),
        if err.is_empty() { String::new() } else { format!(": {err}") }
    )
}

/// Paths the confined backend must be able to see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exposed<'a> {
    /// The model blob. Bound read-only — the store is root-owned and `0444`
    /// anyway, so this is the second of two independent reasons the inference
    /// process cannot alter a model.
    pub model: &'a Path,
    /// The socket directory, bound writable so the backend can create its
    /// listener there.
    pub socket_dir: &'a Path,
    /// Device nodes to pass through, already filtered to ones that exist.
    pub devices: Vec<PathBuf>,
}

/// Which device nodes a backend needs, given its compute backend.
///
/// Only paths that exist are returned: `--dev-bind` of a missing node makes
/// `bwrap` fail, so a CUDA machine's node list must not be asked for on an AMD
/// one. Verified labels on the katana, an SELinux-enforcing Rime host:
/// `/dev/nvidia*` are `xserver_misc_device_t` and `/dev/dri/renderD*` are
/// `dri_device_t`, both of which an unconfined user process may open — which is
/// why this needs no policy module of its own.
pub fn device_nodes(backend: Backend) -> Vec<PathBuf> {
    let candidates: &[&str] = match backend {
        Backend::Cuda => &[
            "/dev/nvidiactl",
            "/dev/nvidia0",
            "/dev/nvidia1",
            "/dev/nvidia-uvm",
            "/dev/nvidia-uvm-tools",
            "/dev/nvidia-modeset",
        ],
        // ROCm needs the compute device and the render nodes.
        Backend::Rocm => &["/dev/kfd", "/dev/dri"],
        // Vulkan is render nodes only.
        Backend::Vulkan => &["/dev/dri"],
        Backend::Cpu => &[],
    };
    candidates
        .iter()
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .collect()
}

/// Build the `bwrap` argv that runs `plan` confined.
///
/// Pure, so every flag can be asserted without a sandbox. The order matters and
/// is not arbitrary: `--dev /dev` replaces the device tree, so the `--dev-bind`
/// entries have to come after it or they are thrown away — a mistake whose
/// symptom is "CUDA cannot find a device" three layers from its cause.
pub fn confine_argv(c: &Confinement, plan: &LaunchPlan, exposed: &Exposed<'_>) -> Vec<String> {
    let mut a = sandbox_argv(c);

    // A runtime that is not under /usr needs its own directory bound, or the
    // sandbox cannot see the program it is being asked to exec. That is the
    // case for a build tree, and for the shell suite's fake backend; a
    // system-extension install lands at /usr/bin and is already covered.
    //
    // READ-ONLY, and that is the point rather than an oversight: this must not
    // become a way to hand the backend a writable path. The directory is bound
    // rather than the file because a program's loader may need siblings next to
    // it — a build tree's shared objects, a script's interpreter shim.
    if let Some(dir) = program_dir_to_bind(&plan.program) {
        a.push("--ro-bind".into());
        a.push(dir.clone());
        a.push(dir);
    }

    // The two paths that are this backend's job.
    a.push("--ro-bind".into());
    a.push(exposed.model.display().to_string());
    a.push(exposed.model.display().to_string());
    a.push("--bind".into());
    a.push(exposed.socket_dir.display().to_string());
    a.push(exposed.socket_dir.display().to_string());

    // Devices, AFTER --dev.
    for d in &exposed.devices {
        a.push("--dev-bind".into());
        a.push(d.display().to_string());
        a.push(d.display().to_string());
    }

    a.push("--".into());
    a.push(plan.program.clone());
    a.extend(plan.argv.iter().cloned());
    a
}

/// The sandbox itself: `bwrap`, the namespaces, the read-only system and the
/// fresh `/proc`, `/dev` and `/tmp`. Shared by every backend launch and by the
/// startup probe, so the probe tests the namespaces a backend really gets.
fn sandbox_argv(c: &Confinement) -> Vec<String> {
    let mut a: Vec<String> = vec![c.bwrap.display().to_string()];

    a.extend(
        [
            // No network. THE flag: see the module docs.
            "--unshare-net",
            "--unshare-ipc",
            "--unshare-uts",
            "--unshare-pid",
            // The backend must not outlive the daemon that is supposed to
            // unload it.
            "--die-with-parent",
            // A new session, so the backend cannot reach a controlling
            // terminal.
            "--new-session",
            // A read-only system. /usr carries the runtime and its libraries —
            // the system-extension merge is already applied, so a
            // sysext-installed llama-server and its dependencies are both
            // inside it. /etc carries ld.so.cache, without which nothing
            // dynamically linked starts.
            "--ro-bind",
            "/usr",
            "/usr",
            "--ro-bind",
            "/etc",
            "/etc",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    for link in ["lib", "lib64", "bin", "sbin"] {
        a.push("--symlink".into());
        a.push(format!("usr/{link}"));
        a.push(format!("/{link}"));
    }
    a.extend(
        ["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"]
            .iter()
            .map(|s| s.to_string()),
    );
    a
}

/// The directory to bind so the sandbox can see `program`, or `None` when it
/// is already inside a path the sandbox has.
///
/// Refuses to bind anything that would widen the sandbox meaningfully:
///
/// * a program under `/usr` needs nothing — `--ro-bind /usr /usr` covers it;
/// * a bare name (no `/`) is resolved by the sandbox's own `PATH`, which is
///   `/usr/bin:/usr/sbin`, so it is also covered;
/// * `/`, `/home`, `/var` and the other broad roots are refused, because
///   binding one of those would undo the confinement rather than complete it.
///   A runtime sitting directly in such a directory is not a case worth
///   supporting; put it in a subdirectory.
fn program_dir_to_bind(program: &str) -> Option<String> {
    let path = Path::new(program);
    if !path.is_absolute() {
        return None;
    }
    if program.starts_with("/usr/") {
        return None;
    }
    let dir = path.parent()?;
    let dir_str = dir.to_string_lossy().to_string();
    // The roots that must never be bound wholesale.
    const TOO_BROAD: &[&str] = &[
        "/", "/home", "/var", "/var/home", "/etc", "/opt", "/run", "/tmp", "/srv", "/mnt",
    ];
    if TOO_BROAD.contains(&dir_str.as_str()) {
        eprintln!(
            "rime-aid: refusing to bind {dir_str} into the sandbox — it is too broad. \
             Put the runtime in a subdirectory, or install it with `rime install`."
        );
        return None;
    }
    Some(dir_str)
}

/// A running backend.
pub struct Running {
    /// The model it holds.
    pub model: String,
    /// The socket it listens on.
    pub socket: PathBuf,
    /// The plan that started it, for `rime ai status`.
    pub plan: LaunchPlan,
    child: Child,
    /// Process group, so stopping reaches `bwrap` and whatever it started.
    pgid: libc::pid_t,
    started: Instant,
}

impl Running {
    /// How long it has been up.
    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    /// Whether the child is still alive. Reaps it if not, so a backend that
    /// crashed does not linger as a zombie claiming to be loaded.
    pub fn alive(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                eprintln!("rime-aid: backend exited: {status}");
                false
            }
            Err(e) => {
                eprintln!("rime-aid: cannot check the backend: {e}");
                false
            }
        }
    }
}

/// Start a backend and wait for it to be reachable.
///
/// The socket file is removed first: a path left by a previous backend cannot
/// be bound over, and the failure reads as "the runtime will not start".
///
/// Always under `bwrap`: `confinement` is the proof that it works, and there is
/// no argument that asks for anything else.
pub fn start(
    confinement: &Confinement,
    plan: &LaunchPlan,
    model: &str,
    socket: &Path,
    exposed: &Exposed<'_>,
) -> Result<Running> {
    let _ = std::fs::remove_file(socket);

    let argv = confine_argv(confinement, plan, exposed);
    let (program, args) = argv
        .split_first()
        .context("a launch plan with no program")?;

    let mut cmd = Command::new(program);
    cmd.args(args);

    // The environment is CLEARED and rebuilt from a declared list, the rule the
    // agent runtime's sandbox follows. Inheriting the user's environment would
    // let `LLAMA_ARG_*` — which llama-server reads for most of its flags —
    // silently override the plan, and would hand the backend every token and
    // API key in the shell.
    cmd.env_clear();
    cmd.env("PATH", "/usr/bin:/usr/sbin");
    // A locale-independent child, so log parsing and number formatting cannot
    // depend on the user's settings.
    cmd.env("LC_ALL", "C");
    for (k, v) in &plan.env {
        cmd.env(k, v);
    }

    // No terminal, and no way to read one. stdout and stderr go to this
    // process's, which under `systemd --user` is the journal.
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());

    // Two things in the forked child, both async-signal-safe and both
    // load-bearing.
    //
    // 1. Its own process group, so stopping it signals `bwrap` and everything
    //    beneath rather than only the direct child.
    //
    // 2. THE SIGNAL MASK IS CLEARED, and this one was a real bug found by
    //    timing a shutdown on the katana. The daemon blocks SIGTERM, SIGINT and
    //    SIGHUP process-wide so that only its signal thread receives them
    //    (`block_termination_signals`). A signal mask is inherited across
    //    `fork(2)` and — unlike handler dispositions — is NOT reset by
    //    `execve(2)`. So every backend inherited a mask with SIGTERM blocked
    //    and could not receive it at all: `stop()` signalled correctly, nothing
    //    happened, the five-second grace elapsed, and the backend was SIGKILLed
    //    on every single stop. Measured before the fix:
    //
    //        rime-aid: signal 15, stopping the backend
    //        rime-aid: backend did not stop within 5s; killing it
    //
    //    What that cost is not the five seconds. It is that a real runtime
    //    holding several gigabytes of VRAM never got the chance to release it
    //    itself — every `systemctl --user restart`, every logout and every idle
    //    unload tore the allocation down with SIGKILL instead.
    // Safe: setpgid and sigprocmask in the forked child touch nothing this
    // process owns, and both are on the async-signal-safe list.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut empty: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut empty);
            if libc::pthread_sigmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd
        .spawn()
        .with_context(|| format!("starting {}", plan.program))?;
    let pgid = child.id() as libc::pid_t;

    let mut running = Running {
        model: model.to_string(),
        socket: socket.to_path_buf(),
        plan: plan.clone(),
        child,
        pgid,
        started: Instant::now(),
    };

    match wait_ready(&mut running) {
        Ok(()) => Ok(running),
        Err(e) => {
            stop(&mut running);
            Err(e)
        }
    }
}

/// Poll until the backend's socket accepts a connection.
///
/// Checks whether the child is still alive on every pass. Without that, a
/// runtime that exits immediately — a missing library, an unreadable model, a
/// GPU that will not initialise — would be waited on for three minutes and then
/// reported as a timeout, hiding the real error that is already in the journal.
fn wait_ready(running: &mut Running) -> Result<()> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if !running.alive() {
            bail!(
                "{} exited before it was ready; its output is in `journalctl --user -u rime-aid`",
                running.plan.program
            );
        }
        if UnixStream::connect(&running.socket).is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "{} did not bind {} within {}s",
                running.plan.program,
                running.socket.display(),
                READY_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(READY_POLL);
    }
}

/// Stop a backend, releasing its VRAM.
///
/// `SIGTERM` to the process group, then `SIGKILL` after [`STOP_GRACE`]. The
/// group rather than the pid because with confinement the direct child is
/// `bwrap`; `--die-with-parent` and the pid namespace would eventually clean up
/// after it, but "eventually" is not good enough for a function whose whole
/// purpose is that the VRAM is free when it returns.
pub fn stop(running: &mut Running) {
    let started = Instant::now();
    signal_group(running.pgid, libc::SIGTERM);
    let deadline = started + STOP_GRACE;
    while Instant::now() < deadline {
        match running.child.try_wait() {
            Ok(Some(_)) => {
                let _ = std::fs::remove_file(&running.socket);
                eprintln!(
                    "rime-aid: backend stopped in {} ms",
                    started.elapsed().as_millis()
                );
                return;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break,
        }
    }
    eprintln!(
        "rime-aid: backend did not stop within {}s; killing it",
        STOP_GRACE.as_secs()
    );
    signal_group(running.pgid, libc::SIGKILL);
    let _ = running.child.wait();
    let _ = std::fs::remove_file(&running.socket);
}

/// Signal a whole process group, and the leader as a fallback.
///
/// The return value is CHECKED and reported, which it was not before. A
/// discarded `kill` is how a function whose entire contract is "the VRAM is
/// free when this returns" comes to return having signalled nothing — and the
/// only symptom is the five-second grace period elapsing and a SIGKILL, which
/// reads as a slow backend rather than as a signal that went nowhere.
///
/// The group AND the leader, because `bwrap --new-session` calls `setsid(2)` in
/// the sandboxed child, which puts it in a new session and a new process group.
/// The group we know about therefore contains only the `bwrap` process itself,
/// and signalling the group alone leaves the sandbox's own group unsignalled —
/// so the leader is signalled directly as well. `bwrap --die-with-parent`
/// propagates from there.
fn signal_group(pgid: libc::pid_t, sig: libc::c_int) {
    if pgid <= 1 {
        // Never `kill(-1, …)`, which means every process this user can signal.
        eprintln!("rime-aid: refusing to signal process group {pgid}");
        return;
    }
    // Safe: kill() with a negative pid signals the group, with a positive pid
    // one process; neither can corrupt memory. The guard above is what keeps
    // the negative form from meaning "everything".
    let group = unsafe { libc::kill(-pgid, sig) };
    let leader = unsafe { libc::kill(pgid, sig) };
    if group != 0 && leader != 0 {
        // ESRCH on both means it is already gone, which is the ordinary case
        // when a backend crashed. Anything else is worth seeing.
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ESRCH) {
            eprintln!("rime-aid: signal {sig} to backend {pgid} failed: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimed_core::ai::{
        plan_fit, plan_launch, select_backend, Accel, Device, LaunchRequest, Listen, Runtime,
    };

    fn a_plan() -> LaunchPlan {
        let fit = plan_fit(4096, 32, 32, 8192, 7628);
        let choice = select_backend(
            &Accel { cuda: true, rocm: false, vulkan: true },
            &[Device { index: 0, name: "gpu".into(), total_mib: 8192, used_mib: 52 }],
            None,
        )
        .unwrap();
        plan_launch(&LaunchRequest {
            runtime: Runtime::LlamaCpp,
            program: "/usr/bin/llama-server",
            model_path: Path::new("/var/lib/rime/ai/models/blobs/sha256-aa"),
            listen: Listen::Unix(PathBuf::from("/run/user/1000/rime-ai/backend.sock")),
            fit: &fit,
            choice: &choice,
            threads: 8,
        })
        .unwrap()
    }

    fn exposed<'a>(model: &'a Path, dir: &'a Path) -> Exposed<'a> {
        Exposed { model, socket_dir: dir, devices: vec![PathBuf::from("/dev/nvidiactl")] }
    }

    /// The shipped `bwrap`, for the pure argv tests. Built directly because
    /// this module may; nothing outside it can.
    fn shipped() -> Confinement {
        Confinement { bwrap: PathBuf::from(BWRAP) }
    }

    /// A scratch directory unique to one test.
    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "rime-aid-confine-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Write `body` as a shell script with the given mode.
    fn script(path: &Path, body: &str, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn the_confined_backend_has_no_network() {
        // THE assertion. Without --unshare-net the "no port another account can
        // open" claim rests entirely on the planner having got --host right.
        let plan = a_plan();
        let argv = confine_argv(
            &shipped(),
            &plan,
            &exposed(Path::new("/var/lib/rime/ai/models/blobs/sha256-aa"), Path::new("/run/x")),
        );
        assert!(argv.contains(&"--unshare-net".to_string()), "{argv:?}");
    }

    #[test]
    fn device_binds_come_after_dev_or_they_are_discarded() {
        // `--dev /dev` replaces the device tree. A --dev-bind before it is
        // silently undone, and the symptom is "CUDA found no device".
        let plan = a_plan();
        let argv = confine_argv(&shipped(), &plan, &exposed(Path::new("/m.gguf"), Path::new("/run/x")));
        let dev = argv.iter().position(|a| a == "--dev").expect("--dev is present");
        let bind = argv
            .iter()
            .position(|a| a == "--dev-bind")
            .expect("--dev-bind is present");
        assert!(bind > dev, "--dev-bind at {bind} precedes --dev at {dev}: {argv:?}");
    }

    #[test]
    fn the_model_is_bound_read_only_and_the_socket_directory_is_not() {
        let plan = a_plan();
        let argv = confine_argv(&shipped(), &plan, &exposed(Path::new("/m.gguf"), Path::new("/run/x")));
        // Exact triples, not "contains": a --bind where --ro-bind belongs would
        // make the store writable by the inference process, which is the thing
        // the whole store design refuses.
        assert!(
            argv.windows(3).any(|w| w == ["--ro-bind", "/m.gguf", "/m.gguf"]),
            "{argv:?}"
        );
        assert!(
            argv.windows(3).any(|w| w == ["--bind", "/run/x", "/run/x"]),
            "{argv:?}"
        );
        assert!(
            !argv.windows(3).any(|w| w == ["--bind", "/m.gguf", "/m.gguf"]),
            "the model must never be bound writable: {argv:?}"
        );
    }

    #[test]
    fn the_system_is_read_only_and_home_is_never_bound() {
        let plan = a_plan();
        let argv = confine_argv(&shipped(), &plan, &exposed(Path::new("/m.gguf"), Path::new("/run/x")));
        assert!(argv.windows(3).any(|w| w == ["--ro-bind", "/usr", "/usr"]), "{argv:?}");
        assert!(argv.windows(3).any(|w| w == ["--ro-bind", "/etc", "/etc"]), "{argv:?}");
        // The backend has no business in the user's home. It gets one model
        // file and one socket directory.
        for a in &argv {
            assert!(!a.contains("/home/"), "home is bound into the sandbox: {a}");
            assert!(!a.contains("/var/home/"), "home is bound into the sandbox: {a}");
        }
        // And there is no writable bind other than the socket directory.
        let writable: Vec<&String> = argv
            .iter()
            .enumerate()
            .filter(|(i, a)| *a == "--bind" && argv.get(i + 1).is_some())
            .map(|(i, _)| &argv[i + 1])
            .collect();
        assert_eq!(writable, vec!["/run/x"], "unexpected writable bind: {argv:?}");
    }

    #[test]
    fn the_program_and_its_arguments_come_after_a_double_dash() {
        let plan = a_plan();
        let argv = confine_argv(&shipped(), &plan, &exposed(Path::new("/m.gguf"), Path::new("/run/x")));
        let dd = argv.iter().position(|a| a == "--").expect("--");
        assert_eq!(argv[dd + 1], plan.program);
        assert_eq!(&argv[dd + 2..], &plan.argv[..]);
    }

    #[test]
    fn a_runtime_under_usr_needs_no_extra_bind() {
        // The shipped case: `sudo rime install llama-cpp` lands at /usr/bin,
        // which `--ro-bind /usr /usr` already covers. An extra bind here would
        // be noise in every real launch.
        assert_eq!(program_dir_to_bind("/usr/bin/llama-server"), None);
        assert_eq!(program_dir_to_bind("/usr/local/bin/llama-server"), None);
        // A bare name is resolved by the sandbox's own PATH, which is
        // /usr/bin:/usr/sbin.
        assert_eq!(program_dir_to_bind("llama-server"), None);
    }

    #[test]
    fn a_runtime_outside_usr_gets_its_directory_bound_read_only() {
        // A build tree, or the shell suite's fake backend. Without this the
        // sandbox cannot see the program it is asked to exec, and the symptom
        // is "the runtime will not start" three layers from its cause — which
        // is how this case was found, running against the katana.
        assert_eq!(
            program_dir_to_bind("/opt/llama/bin/llama-server"),
            Some("/opt/llama/bin".to_string())
        );

        let plan = LaunchPlan {
            program: "/opt/llama/bin/llama-server".to_string(),
            argv: vec!["--model".into(), "/m.gguf".into()],
            env: vec![],
            listen: Listen::Unix(PathBuf::from("/run/x/b.sock")),
            notes: vec![],
        };
        let argv = confine_argv(&shipped(), &plan, &exposed(Path::new("/m.gguf"), Path::new("/run/x")));
        assert!(
            argv.windows(3)
                .any(|w| w == ["--ro-bind", "/opt/llama/bin", "/opt/llama/bin"]),
            "{argv:?}"
        );
        // Read-only, never writable: this must not become a way to hand the
        // backend a writable path.
        assert!(
            !argv.windows(2).any(|w| w[0] == "--bind" && w[1].starts_with("/opt/")),
            "the runtime directory was bound writable: {argv:?}"
        );
    }

    #[test]
    fn a_runtime_sitting_in_a_broad_root_is_refused_rather_than_bound() {
        // Binding /home or /var wholesale would undo the confinement rather
        // than complete it, so the answer is None and the backend fails to
        // start with a message — not a sandbox that sees everything.
        for p in [
            "/llama-server",
            "/home/llama-server",
            "/var/llama-server",
            "/tmp/llama-server",
            "/etc/llama-server",
            "/opt/llama-server",
        ] {
            assert_eq!(program_dir_to_bind(p), None, "{p} was bound");
        }
    }

    #[test]
    fn the_confinement_argv_is_pure() {
        let plan = a_plan();
        let e = exposed(Path::new("/m.gguf"), Path::new("/run/x"));
        assert_eq!(confine_argv(&shipped(), &plan, &e), confine_argv(&shipped(), &plan, &e));
    }





    // ── fail closed ─────────────────────────────────────────────────────────
    //
    // The daemon used to run the backend UNCONFINED when bwrap was missing.
    // These pin the replacement: no working bwrap, no Confinement, no start.

    #[test]
    fn the_shipped_bwrap_path_is_fixed_and_first() {
        // argv[0] is what gets exec'd. Anything other than the fixed bwrap
        // there means the backend is not inside the sandbox at all.
        assert_eq!(BWRAP, "/usr/bin/bwrap");
        let plan = a_plan();
        let argv = confine_argv(&shipped(), &plan, &exposed(Path::new("/m.gguf"), Path::new("/run/x")));
        assert_eq!(argv[0], BWRAP, "{argv:?}");
        // And the backend is only ever what bwrap runs, after `--`.
        let dd = argv.iter().position(|a| a == "--").expect("--");
        assert!(!argv[..dd].contains(&plan.program), "{argv:?}");
    }

    #[test]
    fn a_missing_bwrap_refuses_to_start() {
        let d = scratch("missing");
        let err = confinement_at(&d.join("bwrap")).expect_err("a missing bwrap was accepted");
        let msg = format!("{err:#}");
        assert!(msg.contains("refusing to start"), "{msg}");
        assert!(msg.contains("not installed"), "{msg}");
        assert!(msg.contains("never runs a model unsandboxed"), "{msg}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_bwrap_that_is_not_executable_refuses_to_start() {
        // What a bind-mounted empty file, a half-written package or a stray
        // chmod leaves. `is_file()` alone said yes to this.
        let d = scratch("noexec");
        let b = d.join("bwrap");
        script(&b, "exit 0", 0o644);
        let msg = format!("{:#}", confinement_at(&b).expect_err("accepted a 0644 bwrap"));
        assert!(msg.contains("not executable"), "{msg}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_directory_where_bwrap_should_be_refuses_to_start() {
        let d = scratch("dir");
        let b = d.join("bwrap");
        std::fs::create_dir(&b).unwrap();
        let msg = format!("{:#}", confinement_at(&b).expect_err("accepted a directory"));
        assert!(msg.contains("not a regular file"), "{msg}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_bwrap_that_cannot_build_the_sandbox_refuses_to_start() {
        // Present and executable is not working: a kernel that refuses
        // unprivileged user namespaces leaves exactly this. bwrap's own reason
        // must reach the message, because it is the actionable part.
        let d = scratch("broken");
        let b = d.join("bwrap");
        script(&b, "echo 'bwrap: No permissions to create new namespace' >&2; exit 1", 0o755);
        let msg = format!("{:#}", confinement_at(&b).expect_err("accepted a broken bwrap"));
        assert!(msg.contains("could not build a test sandbox"), "{msg}");
        assert!(msg.contains("No permissions to create new namespace"), "{msg}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn the_probe_builds_the_same_namespaces_a_backend_gets() {
        // The probe is only evidence if it asks for what a launch asks for. A
        // fake bwrap records its arguments and succeeds.
        let d = scratch("probe");
        let b = d.join("bwrap");
        let log = d.join("argv");
        script(&b, &format!("printf '%s\\n' \"$@\" > '{}'; exit 0", log.display()), 0o755);
        let c = confinement_at(&b).expect("a working bwrap is accepted");
        assert_eq!(c, Confinement { bwrap: b.clone() });
        let probed: Vec<String> =
            std::fs::read_to_string(&log).unwrap().lines().map(String::from).collect();
        let launch = confine_argv(&c, &a_plan(), &exposed(Path::new("/m.gguf"), Path::new("/run/x")));
        let sandbox = sandbox_argv(&c);
        assert_eq!(&probed[..sandbox.len() - 1], &sandbox[1..], "probe argv: {probed:?}");
        assert_eq!(&launch[..sandbox.len()], &sandbox[..]);
        for flag in ["--unshare-net", "--unshare-pid", "--die-with-parent", "--new-session"] {
            assert!(probed.iter().any(|a| a == flag), "probe lacks {flag}: {probed:?}");
        }
        assert_eq!(probed.last().map(String::as_str), Some("/usr/bin/true"));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn start_runs_the_backend_only_through_bwrap() {
        // End to end through `start`: the fake bwrap records that IT was run
        // and then fails, the way a sandbox that cannot be built does. The
        // backend writes a marker if it is ever exec'd directly. It must not
        // be, and the failure must be reported rather than retried unconfined.
        let d = scratch("start");
        let b = d.join("bwrap");
        let log = d.join("bwrap-ran");
        let marker = d.join("backend-ran-unconfined");
        script(&b, &format!("printf '%s\\n' \"$@\" > '{}'; exit 1", log.display()), 0o755);
        let backend = d.join("bin/llama-server");
        std::fs::create_dir_all(backend.parent().unwrap()).unwrap();
        script(&backend, &format!(": > '{}'; exit 0", marker.display()), 0o755);

        let plan = LaunchPlan {
            program: backend.display().to_string(),
            argv: vec!["--model".into(), "/m.gguf".into()],
            env: vec![],
            listen: Listen::Unix(d.join("b.sock")),
            notes: vec![],
        };
        let c = Confinement { bwrap: b.clone() };
        let socket = d.join("b.sock");
        let ex = Exposed { model: Path::new("/m.gguf"), socket_dir: &d, devices: vec![] };
        let result = start(&c, &plan, "m", &socket, &ex);
        assert!(result.is_err(), "a backend whose sandbox failed was reported as running");
        let ran = std::fs::read_to_string(&log).expect("bwrap was not the program started");
        assert!(ran.lines().any(|l| l == "--unshare-net"), "{ran}");
        assert!(ran.lines().any(|l| l == plan.program), "{ran}");
        assert!(!marker.exists(), "the backend ran without its sandbox");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn signalling_group_zero_or_one_is_refused() {
        // kill(-1, …) signals every process this user owns. The guard is the
        // difference between "stop the backend" and "log out".
        // Nothing is signalled here: pgid 0 and 1 both take the refusal path.
        signal_group(0, libc::SIGTERM);
        signal_group(1, libc::SIGTERM);
        signal_group(-5, libc::SIGTERM);
        // Reaching this line without having killed the test runner is the
        // assertion.
    }
}
