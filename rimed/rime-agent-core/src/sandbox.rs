//! Project-scoped confinement for agent sessions.
//!
//! Built on `bubblewrap`, which is already in the image — this adds no package
//! to `Containerfile.core` and therefore costs the fleet no update. The
//! confinement is mount-namespace based, which for the properties the roadmap
//! asks for (project writable, `~/.ssh` unreachable, browser profiles
//! unreachable, unrelated documents unreachable, no camera or microphone, base
//! OS not modifiable) is stronger than a path-allowlist LSM would be: the files
//! are not merely denied, they are not in the mount namespace at all.
//!
//! ## Default-deny, not blocklist
//!
//! `$HOME`, `/run` and `$XDG_RUNTIME_DIR` are replaced with empty tmpfs mounts
//! and only an explicit allowlist is bound back. A blocklist ("hide `~/.ssh`,
//! hide `~/.mozilla`") is unmaintainable — every new credential store a tool
//! invents is a hole until someone notices. Default-deny means `~/.ssh`,
//! `~/.gnupg`, `~/.aws`, browser profiles and the ssh-agent and gpg-agent
//! sockets in `$XDG_RUNTIME_DIR` are all unreachable because nothing bound
//! them, not because anything listed them.
//!
//! The environment is treated the same way: `--clearenv` and then an explicit
//! set, so an `ANTHROPIC_API_KEY` or `GITHUB_TOKEN` sitting in the user's shell
//! does not leak into a session that never asked for it.
//!
//! ## The agent's own profile is not one allowlist entry
//!
//! What comes back into the masked home is decided by [`crate::adapter`], and
//! for an agent Rime has a [`crate::profile`] description of it comes back path
//! by path rather than as a directory. The reusable half — instructions,
//! skills, slash commands, subagent definitions, settings — is in [`spec.ro`],
//! so a session cannot rewrite what the next one will be started with; the
//! session and plugin state it writes as it runs is in [`spec.rw`].
//!
//! The profile directory itself is in neither, which is what makes the rest of
//! it an overlay: the home is a tmpfs and `bwrap` creates its own mount points,
//! so `~/.claude` is an empty writable directory inside the session with the
//! listed entries mounted into it. A path a later release invents there is
//! writable, private to the session, and gone with it.
//!
//! [`spec.ro`]: SandboxSpec::ro
//! [`spec.rw`]: SandboxSpec::rw
//!
//! ## Why `/run` is masked, and why a socket denylist would not do
//!
//! `--ro-bind / /` made the whole of `/run` visible, including
//! `/run/dbus/system_bus_socket`, which is mode `0666`. The system bus is where
//! `org.rimeos.Rimed1` lives, and its mutating methods are gated by polkit
//! actions that ship `allow_active = yes` — passwordless for the logged-in
//! local user. A confined session runs as that user, in that session, so polkit
//! authorised it: `SetTier`, `SetChargeThresholds`, `Fan.SetPwm` and
//! `GameMode.StartForPid` were all reachable from inside the sandbox. Measured,
//! not theorised — `SetTier` returned success from confinement.
//!
//! A denylist of known sockets cannot fix this. `/run` is a tmpfs on the host
//! and `--ro-bind / /` is a bind of that same filesystem, so a socket created
//! *after* the sandbox starts appears inside it. Anything computed at spawn
//! time is stale by construction. Masking the directory and binding back the
//! one thing a build genuinely needs — the resolver configuration — is the only
//! form of this that stays correct.
//!
//! ## Verified properties
//!
//! Measured on Rime OS 43, kernel 7.1.5, bubblewrap 0.11.0:
//!
//! | property | outside | inside |
//! |---|---|---|
//! | processes visible in `/proc` | 408 | 4 |
//! | `/dev/video*` nodes | 4 | 0 |
//! | `/dev/snd` nodes | 14 | 0 |
//! | `~/.ssh` readable | yes | no |
//! | project readable/writable | yes | yes |
//! | system bus reachable | yes | **no** |
//! | `org.rimeos.Rimed1` callable | yes | **no** |
//! | DNS resolution | yes | yes |
//! | `~/.claude/skills` writable | yes | **no** |
//! | `~/.claude/projects` writable | yes | yes |
//!
//! The last three are what the `/run` tmpfs changed. DNS is in the table
//! because masking `/run` breaks it by default: `/etc/resolv.conf` is a symlink
//! into `/run`, so the target has to be bound back or every confined session
//! loses name resolution — which is the kind of regression that gets a security
//! fix reverted.
//!
//! Exit status propagates through `bwrap --unshare-pid` unchanged, and
//! `killpg` on the session's process group still stops, continues and kills the
//! whole tree — which is what makes `rime agent pause` work despite the PID
//! namespace.
//!
//! ## What this does not do
//!
//! It does not confine a session with `SandboxPolicy::Unrestricted`, by design
//! — that is the escape hatch. It does not sandbox ordinary terminal processes:
//! policy applies to sessions the runtime manages and to nothing else.

use std::path::{Path, PathBuf};

use crate::policy::{AgentPolicy, NetworkPolicy};
use crate::protocol::SandboxPolicy;

/// The port the egress bridge listens on inside a session's own namespace.
///
/// Fixed, and safe to fix: the namespace is empty apart from loopback and the
/// bridge, so there is nothing for it to collide with. 3128 because a proxy on
/// 3128 is what every HTTP client's documentation shows.
pub const BRIDGE_PORT: u16 = 3128;

/// The flag that turns the runtime binary into the in-sandbox bridge.
pub const BRIDGE_FLAG: &str = "--net-bridge";

/// The way out of an `allowlist` session, and the only one it has.
///
/// A session in this mode runs with `--unshare-net`, so nothing inside it can
/// reach an address. What it can reach is a Unix socket, because `AF_UNIX` is
/// a filesystem object and a network namespace does not touch it — and the
/// daemon holds the other end of that socket, outside the namespace, where the
/// destination policy is applied and the connection is actually made.
///
/// The gap between the two is that no HTTP client speaks to a proxy over a
/// Unix socket. So a small bridge runs *inside* the namespace as the session's
/// parent process: it listens on loopback, carries bytes to the socket, and
/// contains no policy at all. Replacing it buys an agent nothing — the far end
/// is still the daemon, still deciding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressBridge {
    /// The bridge program. `rime-agentd` re-executed with [`BRIDGE_FLAG`]:
    /// both ends of the socket protocol then live in one binary and there is
    /// no sibling to look up on a path the sandbox may not have.
    ///
    /// It has to be a path the *session* can see, which `/usr/bin/rime-agentd`
    /// is and a development build under `/tmp` or `$HOME` is not — both of
    /// those are masked. The daemon checks before it starts a session rather
    /// than leaving it to fail as an exec error inside somebody's terminal.
    pub program: PathBuf,
    /// The daemon's egress socket, inside the session's scratch directory —
    /// already bound read-write, so it needs no mount of its own and cannot
    /// disturb the ordering the `/run` mask depends on.
    pub socket: PathBuf,
    /// Where the bridge listens inside the session's namespace.
    pub port: u16,
}

/// Environment variables every session keeps, regardless of adapter.
///
/// Locale and terminal identity, because a TUI renders wrongly without them,
/// and the account identity a toolchain expects. Notably absent: anything that
/// could carry a credential.
const ENV_BASE: &[&str] = &[
    "TERM",
    "COLORTERM",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_NUMERIC",
    "LC_TIME",
    "LC_COLLATE",
    "LC_MONETARY",
    "TZ",
    "TERMINFO",
    "NO_COLOR",
];

/// Absolute paths that may never be bound writable, whatever asks for it.
///
/// Binding any of these read-write would hand back everything the tmpfs masks
/// (`$HOME`), or make the image-owned base mutable (`/usr`, `/etc`), which is
/// the one thing an atomic OS must not allow a confined process to do.
/// `/var/lib/apex` is the same state under its name before the rename, which a
/// machine can still carry, or carry as a link to the new one.
const NEVER_WRITABLE: &[&str] = &[
    "/",
    "/usr",
    "/etc",
    "/boot",
    "/sysroot",
    "/var/lib/rime",
    "/var/lib/apex",  // rime-rename: keep (the pre-rename state directory)
];

/// The runtime directory masked for every confined session.
const RUN_DIR: &str = "/run";

/// Paths under [`RUN_DIR`] that must be bound back for a confined session to
/// resolve names.
///
/// `/etc/resolv.conf` is a symlink on every systemd-resolved machine
/// (`../run/systemd/resolve/stub-resolv.conf` here), and masking `/run` breaks
/// the link. This follows the link and returns the target when it lands under
/// `/run`.
///
/// Resolved rather than hardcoded, deliberately. On a machine using
/// NetworkManager's own `resolv.conf`, or a plain file, the systemd path does
/// not exist — and because the bind is a `-try`, a hardcoded path would
/// silently no-op and ship every session with broken DNS. A target outside
/// `/run` needs no bind at all, since `--ro-bind / /` still covers it.
///
/// Errors are not propagated: an unreadable link means "nothing to bind", and
/// the caller has nothing useful to do with the distinction.
pub fn resolv_binds() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let run = Path::new(RUN_DIR);
    // canonicalize, not read_link: the link is relative ("../run/...") and can
    // point at another link. This resolves the whole chain to a real path.
    if let Ok(target) = std::fs::canonicalize("/etc/resolv.conf") {
        if target.starts_with(run) {
            out.push(target);
        }
    }
    out
}

/// Why a sandbox could not be built. Every variant is fatal: a session is never
/// silently downgraded to a weaker policy than the one that was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxError {
    /// `bwrap` is missing from the image.
    MissingBwrap,
    /// `dev.tty.legacy_tiocsti` is enabled, so terminal input injection into
    /// the controlling terminal is possible and confinement is not meaningful.
    TiocstiEnabled,
    /// A requested writable path is one that may never be writable.
    ForbiddenWritable(PathBuf),
    /// A path that must be absolute was not.
    NotAbsolute(PathBuf),
    /// The policy needs a network namespace and the sandbox has none to give.
    NetworkWithoutNamespace(NetworkPolicy),
    /// An `allowlist` session was built without the bridge that is its only
    /// way onto the network.
    EgressUnavailable,
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SandboxError::MissingBwrap => write!(
                f,
                "bubblewrap (/usr/bin/bwrap) is not installed, so an agent session cannot be \
                 confined; re-run with `--sandbox unrestricted` to run without confinement"
            ),
            SandboxError::TiocstiEnabled => write!(
                f,
                "dev.tty.legacy_tiocsti is 1, which lets a confined process inject keystrokes \
                 into the controlling terminal; set it to 0 (the kernel default) or re-run with \
                 `--sandbox unrestricted`"
            ),
            SandboxError::ForbiddenWritable(p) => {
                write!(f, "{} may never be made writable to an agent", p.display())
            }
            SandboxError::NotAbsolute(p) => {
                write!(f, "{} must be an absolute path", p.display())
            }
            SandboxError::NetworkWithoutNamespace(mode) => write!(
                f,
                "the {mode} network mode is enforced by unsharing the session's network \
                 namespace, and an unconfined session has none; use `--sandbox project`"
            ),
            SandboxError::EgressUnavailable => write!(
                f,
                "an allowlisted session reaches the network only through the runtime's \
                 egress bridge, and this one was built without it; re-run with \
                 `--network offline` or start the agent runtime again"
            ),
        }
    }
}

impl std::error::Error for SandboxError {}

/// Everything needed to build one session's confinement.
#[derive(Debug, Clone)]
pub struct SandboxSpec {
    /// All six dimensions, not just the sandbox one.
    ///
    /// This module enforces two of them — the sandbox itself and, through
    /// `--unshare-net`, the network — and carrying the whole policy is what
    /// stops the two from being set from different objects. A `network` field
    /// beside a separate `sandbox` field is a call site away from a `strict`
    /// spec built with the network left open, and the argv would look correct
    /// in every test that did not check for that exact combination.
    pub policy: AgentPolicy,
    /// The user's home. Masked with a tmpfs unless the policy is unrestricted.
    pub home: PathBuf,
    /// `$XDG_RUNTIME_DIR`. Masked, so the ssh-agent and gpg-agent sockets go
    /// with it.
    pub runtime_dir: PathBuf,
    /// Paths under `/run` bound back read-only after `/run` is masked.
    ///
    /// In practice this is the resolver configuration and nothing else. It is a
    /// field rather than a constant because the path is machine-dependent —
    /// [`resolv_binds`] resolves it — and keeping the I/O out of
    /// [`build_argv`] is what lets the argv builder stay a pure function with
    /// exhaustive tests.
    pub run_ro: Vec<PathBuf>,
    /// The agentd control socket, bound back writable so a session can publish
    /// its own state through the open event protocol.
    pub control_socket: PathBuf,
    /// Per-session scratch, writable.
    pub scratch: PathBuf,
    /// Working directory inside the sandbox.
    pub cwd: PathBuf,
    /// Writable paths: the project root, the worktree, anything `--allow`ed.
    pub rw: Vec<PathBuf>,
    /// Read-only paths bound back into the masked home: toolchain caches,
    /// the agent's own configuration, anything `--allow-ro`ed.
    pub ro: Vec<PathBuf>,
    /// Read-only binds whose source is not their destination: `(from, at)`.
    ///
    /// One thing needs this, and it is the reason it exists. Claude's
    /// `settings.json` has to be in the session — it carries the model, the
    /// hooks and the theme — and it also carries an `env` block that Claude
    /// applies to every tool it runs, which on this machine is where a GitHub
    /// PAT lived. Neither binding the file nor leaving it out is right, so the
    /// daemon writes a copy with the credential values gone and binds that
    /// copy *at* the real path. Applied after [`SandboxSpec::ro`], so the copy
    /// lands on top of the original rather than racing it.
    pub ro_at: Vec<(PathBuf, PathBuf)>,
    /// Files to blank out *after* the allowlists have been applied.
    ///
    /// Needed because some allowlist entries are directories that a toolchain
    /// must have but that also hold a credential in a fixed location —
    /// `~/.cargo` carries the build cache and `credentials.toml` in the same
    /// tree. Each of these is replaced with `/dev/null`, so the file reads
    /// empty rather than being merely absent, which is what tools that expect
    /// it to exist handle gracefully.
    pub mask: Vec<PathBuf>,
    /// Variables set explicitly (name, value).
    pub env_set: Vec<(String, String)>,
    /// Variable names inherited from the daemon's environment when present.
    /// Adapters use this to declare the credentials their agent needs.
    pub env_pass: Vec<String>,
    /// The session's only route onto the network, for the `allowlist` mode.
    ///
    /// Required by that mode and ignored by every other one: `open` needs no
    /// route because it has the host's, and `offline` and `brokered` are
    /// supposed not to have one. A spec in `allowlist` mode without it is
    /// refused rather than built, because the argv that came back would be a
    /// session with no network at all reporting an allowlist.
    pub egress: Option<EgressBridge>,
}

impl SandboxSpec {
    /// A spec with nothing allowed beyond the defaults.
    pub fn new(policy: AgentPolicy, home: PathBuf, runtime_dir: PathBuf) -> SandboxSpec {
        SandboxSpec {
            policy,
            home,
            runtime_dir,
            run_ro: Vec::new(),
            control_socket: PathBuf::new(),
            scratch: PathBuf::new(),
            cwd: PathBuf::from("/"),
            rw: Vec::new(),
            ro: Vec::new(),
            ro_at: Vec::new(),
            mask: Vec::new(),
            env_set: Vec::new(),
            env_pass: Vec::new(),
            egress: None,
        }
    }
}

/// Check that a confined session can actually be started.
///
/// Called before every confined run. Fails closed: if `bwrap` is missing or the
/// kernel allows terminal injection, the session does not start unconfined, it
/// does not start at all.
pub fn preflight(policy: SandboxPolicy) -> Result<(), SandboxError> {
    if !policy.is_confined() {
        return Ok(());
    }
    if !bwrap_path().exists() {
        return Err(SandboxError::MissingBwrap);
    }
    if tiocsti_enabled() {
        return Err(SandboxError::TiocstiEnabled);
    }
    Ok(())
}

/// Where `bwrap` lives. A fixed path, not a `PATH` lookup: this is a security
/// boundary and resolving it through a user-controlled `PATH` would let a
/// shadowing binary decide what "confined" means.
pub fn bwrap_path() -> PathBuf {
    PathBuf::from(rime_paths::BWRAP)
}

/// Resolve a mount target to a real, symlink-free path.
///
/// `bwrap` refuses to create a mount point whose path traverses a symlink — it
/// exits with "Can't mount on symlink destination" — as a defence against a
/// symlinked target redirecting a bind somewhere unintended. On an atomic OS
/// the home directories are exactly such symlinks (`/root -> var/roothome`,
/// `/home -> var/home`), so masking `$HOME` with a tmpfs fails for the root
/// account and for any user whose home is reached through `/home`. Resolving
/// the path first hands `bwrap` the real directory to mount on; the logical
/// path still exists inside the sandbox as a symlink to it, so an env
/// `HOME=/root` keeps resolving.
///
/// Only what exists is resolved: an allowlist entry that is not present yet is
/// bound with a `-try` and must pass through untouched, not become an error.
/// This stays out of [`build_argv`], which is a pure function of its spec so its
/// argv can be asserted exhaustively — the daemon resolves the spec's paths
/// before handing it over, the same division of labour [`resolv_binds`] uses.
pub fn real_target(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Whether the kernel still honours the legacy `TIOCSTI` ioctl.
///
/// Absent file means the knob does not exist on this kernel, which means the
/// ioctl was compiled out — safe.
fn tiocsti_enabled() -> bool {
    match std::fs::read_to_string("/proc/sys/dev/tty/legacy_tiocsti") {
        Ok(text) => text.trim() == "1",
        Err(_) => false,
    }
}

/// Build the full argv for a confined session: `bwrap … -- program args…`.
///
/// For [`SandboxPolicy::Unrestricted`] this returns `program args…` unchanged,
/// so callers have a single code path.
pub fn build_argv(
    spec: &SandboxSpec,
    program: &str,
    args: &[String],
) -> Result<Vec<String>, SandboxError> {
    let network = spec.policy.effective_network();

    if !spec.policy.sandbox.is_confined() {
        // The unconfined path returns the command untouched, which for any
        // network mode but `open` would be a session running with the host's
        // network under a policy that said otherwise. `AgentPolicy::validate`
        // already refuses that combination; this refuses it again at the point
        // where the argv is built, because that is the last place a mistake
        // can still be caught and the first place it would be invisible.
        if network.removes_direct_egress() {
            return Err(SandboxError::NetworkWithoutNamespace(network));
        }
        let mut argv = vec![program.to_string()];
        argv.extend(args.iter().cloned());
        return Ok(argv);
    }

    // The allowlist's route out has to exist before the session does. Checked
    // here rather than where the bridge is filled in, so a caller that forgets
    // gets a refusal instead of a session that quietly cannot reach anything.
    if network == NetworkPolicy::Allowlist && spec.egress.is_none() {
        return Err(SandboxError::EgressUnavailable);
    }

    for p in spec.rw.iter().chain(std::iter::once(&spec.scratch)) {
        if p.as_os_str().is_empty() {
            continue;
        }
        check_writable(p, &spec.home)?;
    }
    if !spec.cwd.is_absolute() {
        return Err(SandboxError::NotAbsolute(spec.cwd.clone()));
    }

    let mut a: Vec<String> = Vec::new();
    let mut push = |s: &str| a.push(s.to_string());

    push(bwrap_path().to_string_lossy().as_ref());

    // 1. The whole filesystem, read-only. Everything after this is an overlay
    //    on top, so ordering matters and this must come first.
    push("--ro-bind");
    push("/");
    push("/");

    // 2. Fresh /proc and a minimal /dev. The minimal /dev is what removes the
    //    camera and sound devices — it exposes only null, zero, full, random,
    //    urandom, tty and a private pts.
    push("--proc");
    push("/proc");
    push("--dev");
    push("/dev");

    // 3. Private /tmp, then the session's own scratch inside it.
    push("--tmpfs");
    push("/tmp");
    if !spec.scratch.as_os_str().is_empty() {
        push("--bind");
        push(&spec.scratch.to_string_lossy());
        push(&spec.scratch.to_string_lossy());
    }

    // 4. Mask the home, /run, and the runtime directory, then bind back only
    //    what was asked for. This is the default-deny core of the policy.
    //
    //    /run carries the system bus socket, which is world-writable and is how
    //    a confined session reached rimed's polkit-gated methods and changed OS
    //    state. See the module docs: this must be a tmpfs and not a denylist,
    //    because /run is a host tmpfs that keeps growing new sockets after the
    //    sandbox has started.
    //
    //    $XDG_RUNTIME_DIR is normally /run/user/<uid>, i.e. already inside the
    //    /run tmpfs. The separate mount is kept anyway: the variable is not
    //    required to point under /run, and a session whose runtime dir sits
    //    somewhere else must still have it masked.
    push("--tmpfs");
    push(&spec.home.to_string_lossy());
    push("--tmpfs");
    push(RUN_DIR);
    if !spec.runtime_dir.as_os_str().is_empty() {
        push("--tmpfs");
        push(&spec.runtime_dir.to_string_lossy());
    }

    // 5. Bound back into the masked /run: the resolver configuration, and
    //    nothing else. Without this every confined session loses DNS, because
    //    /etc/resolv.conf is a symlink into /run.
    for p in &spec.run_ro {
        if p.as_os_str().is_empty() {
            continue;
        }
        push("--ro-bind-try");
        push(&p.to_string_lossy());
        push(&p.to_string_lossy());
    }

    // 6. The control socket, so the session can publish its own events. Bound
    //    after BOTH tmpfs mounts above — it lives under $XDG_RUNTIME_DIR, so
    //    masking either one after this point would erase it — and writable
    //    because connecting to a Unix socket needs write access to it.
    if !spec.control_socket.as_os_str().is_empty() {
        push("--bind-try");
        push(&spec.control_socket.to_string_lossy());
        push(&spec.control_socket.to_string_lossy());
    }

    // 7. The allowlists. `-try` variants throughout: a toolchain cache that
    //    does not exist yet must not stop the session from starting.
    for p in &spec.ro {
        if p.as_os_str().is_empty() {
            continue;
        }
        push("--ro-bind-try");
        push(&p.to_string_lossy());
        push(&p.to_string_lossy());
    }
    for p in &spec.rw {
        if p.as_os_str().is_empty() {
            continue;
        }
        push("--bind-try");
        push(&p.to_string_lossy());
        push(&p.to_string_lossy());
    }

    // 7b. Redacted copies, bound over what step 7 just put there. After the
    //     allowlists and before the masks, because this is the same idea as a
    //     mask with a file instead of /dev/null: the session gets a document it
    //     needs, minus the part it may not have.
    for (from, at) in &spec.ro_at {
        if from.as_os_str().is_empty() || at.as_os_str().is_empty() {
            continue;
        }
        push("--ro-bind-try");
        push(&from.to_string_lossy());
        push(&at.to_string_lossy());
    }

    // 8. Blank out credential files that sit inside an allowlisted directory.
    //    Last, so nothing bound above can bring one back.
    for p in &spec.mask {
        if p.as_os_str().is_empty() {
            continue;
        }
        push("--ro-bind-try");
        push("/dev/null");
        push(&p.to_string_lossy());
    }

    // 9. Namespaces. PID isolation is what stops an agent signalling the
    //    user's other processes; the host still reaches the session's process
    //    group, so pause/resume/kill keep working.
    push("--unshare-pid");
    push("--unshare-ipc");
    push("--unshare-uts");
    // The NETWORK dimension decides this, not the sandbox one. `strict` still
    // removes the network, because `effective_network` forces it to `offline`
    // — but it does so as a policy floor rather than as a property of the
    // sandbox mode, which is what lets `--sandbox project --network offline`
    // exist.
    //
    // Every mode but `open` takes this flag, and the flag is the whole of the
    // kernel enforcement: `offline`, `brokered` and `allowlist` are one
    // namespace with nothing in it, and they differ in what rime-agentd offers
    // over a Unix socket afterwards. Asked of the policy rather than matched
    // on `Offline` here, so a mode added later cannot arrive with the
    // namespace quietly left shared.
    if network.removes_direct_egress() {
        push("--unshare-net");
    }

    // Deliberately no `--new-session`: the session must keep the PTY as its
    // controlling terminal or every TUI agent breaks. `preflight` refuses to
    // run confined when `dev.tty.legacy_tiocsti` is enabled, which is what
    // `--new-session` would otherwise be protecting against.

    // 10. Environment: clear, then set exactly what was allowed.
    push("--clearenv");
    for (k, v) in resolved_env(spec) {
        push("--setenv");
        push(&k);
        push(&v);
    }

    push("--chdir");
    push(&spec.cwd.to_string_lossy());
    // Die with the daemon, so no session outlives the runtime that tracks it.
    push("--die-with-parent");

    push("--");

    // 11. The egress bridge, when there is one, as the session's parent
    //     process. It has to be inside the sandbox because the loopback it
    //     listens on is the session's own — a listener in the daemon would be
    //     on the host's loopback, which the session cannot see. It carries
    //     bytes and holds no policy; the far end of its socket is where the
    //     destination is decided.
    if let Some(bridge) = spec.egress.as_ref().filter(|_| network == NetworkPolicy::Allowlist) {
        push(&bridge.program.to_string_lossy());
        push(BRIDGE_FLAG);
        push(&bridge.socket.to_string_lossy());
        push(&bridge.port.to_string());
        // A second separator, so the agent's own arguments cannot be read as
        // the bridge's for the same reason the first one exists.
        push("--");
    }

    push(program);
    for arg in args {
        a.push(arg.clone());
    }

    Ok(a)
}

/// The environment a confined session actually gets, sorted by name so the
/// argv is deterministic and therefore testable.
pub fn resolved_env(spec: &SandboxSpec) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = Vec::new();
    let seen = |env: &[(String, String)], k: &str| env.iter().any(|(n, _)| n == k);

    // The proxy variables first, so nothing an adapter or a caller sets can
    // point a session's HTTP client somewhere the bridge is not. They are not
    // the enforcement — the namespace is, and a client that ignores them
    // simply fails to connect — but without them nothing in the session knows
    // the bridge is there.
    //
    // Both cases of each name: curl reads the lowercase forms and deliberately
    // ignores an uppercase `HTTP_PROXY`, because that one is settable by a CGI
    // request header. Tools that read only the uppercase forms are at least as
    // common. Setting both is the only way to be understood by both.
    if let Some(bridge) = spec
        .egress
        .as_ref()
        .filter(|_| spec.policy.effective_network() == NetworkPolicy::Allowlist)
    {
        let url = format!("http://127.0.0.1:{}", bridge.port);
        for name in [
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            env.push((name.to_string(), url.clone()));
        }
    }

    // Explicit values win over anything inherited.
    for (k, v) in &spec.env_set {
        if !seen(&env, k) {
            env.push((k.clone(), v.clone()));
        }
    }

    for name in ENV_BASE.iter().map(|s| s.to_string()).chain(
        // Adapter-declared passthrough, e.g. the credential a given agent
        // needs. Everything else in the caller's environment is dropped.
        spec.env_pass.iter().cloned(),
    ) {
        if seen(&env, &name) {
            continue;
        }
        if let Some(val) = std::env::var_os(&name) {
            env.push((name, val.to_string_lossy().into_owned()));
        }
    }

    env.sort_by(|a, b| a.0.cmp(&b.0));
    env
}

/// Reject writable binds that would defeat the policy.
fn check_writable(path: &Path, home: &Path) -> Result<(), SandboxError> {
    if !path.is_absolute() {
        return Err(SandboxError::NotAbsolute(path.to_path_buf()));
    }
    // Normalise away a trailing slash so "/usr/" is caught alongside "/usr".
    let text = path.to_string_lossy();
    let trimmed = text.trim_end_matches('/');
    let normalised = if trimmed.is_empty() { "/" } else { trimmed };

    for forbidden in NEVER_WRITABLE {
        if normalised == *forbidden {
            return Err(SandboxError::ForbiddenWritable(path.to_path_buf()));
        }
    }
    // The home itself, which the tmpfs is masking. A subdirectory is fine.
    if !home.as_os_str().is_empty() && normalised == home.to_string_lossy().trim_end_matches('/') {
        return Err(SandboxError::ForbiddenWritable(path.to_path_buf()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn real_target_resolves_a_symlinked_directory() {
        use super::real_target;
        use std::path::PathBuf;
        // Mirrors an atomic OS's /root -> var/roothome: bwrap will not mount
        // on the symlink, so the daemon must hand build_argv the real path.
        let base: PathBuf =
            std::env::temp_dir().join(format!("rime-realtarget-{}", std::process::id()));
        let real = base.join("var/roothome");
        std::fs::create_dir_all(&real).expect("mkdir real");
        let link = base.join("root");
        std::os::unix::fs::symlink("var/roothome", &link).expect("symlink");

        let resolved = real_target(&link);
        assert_eq!(resolved, std::fs::canonicalize(&real).unwrap());
        assert_ne!(resolved, link, "the symlink must be resolved, not passed through");

        // A path that does not exist passes through unchanged, so a -try
        // allowlist entry for a cache that is not there yet still no-ops.
        let missing = base.join("does/not/exist");
        assert_eq!(real_target(&missing), missing);

        std::fs::remove_dir_all(&base).ok();
    }

    use super::*;
    use crate::policy::NetworkPolicy;

    fn spec() -> SandboxSpec {
        let mut s = SandboxSpec::new(
            AgentPolicy::default(),
            PathBuf::from("/home/tester"),
            PathBuf::from("/run/user/1000"),
        );
        s.control_socket = PathBuf::from("/run/user/1000/rime-agentd/control.sock");
        s.scratch = PathBuf::from("/tmp/rime-agent/1");
        s.cwd = PathBuf::from("/home/tester/Projects/demo");
        s.rw = vec![PathBuf::from("/home/tester/Projects/demo")];
        s.ro = vec![PathBuf::from("/home/tester/.cargo")];
        s
    }

    fn argv(s: &SandboxSpec) -> Vec<String> {
        build_argv(s, "claude", &["--help".to_string()]).expect("build")
    }

    /// Index of the first occurrence of `needle` in `argv`.
    fn pos(argv: &[String], needle: &str) -> Option<usize> {
        argv.iter().position(|a| a == needle)
    }

    /// Whether `argv` contains the three-token sequence `op src dst`.
    fn has_bind(argv: &[String], op: &str, path: &str) -> bool {
        argv.windows(3)
            .any(|w| w[0] == op && w[1] == path && w[2] == path)
    }

    #[test]
    fn unrestricted_does_not_wrap_the_command_at_all() {
        let mut s = spec();
        s.policy.sandbox = SandboxPolicy::Unrestricted;
        let a = build_argv(&s, "claude", &["hello".into()]).unwrap();
        assert_eq!(a, vec!["claude".to_string(), "hello".to_string()]);
        assert!(!a.iter().any(|x| x.contains("bwrap")));
    }

    #[test]
    fn confined_argv_starts_with_the_absolute_bwrap_path() {
        let a = argv(&spec());
        assert_eq!(a[0], rime_paths::BWRAP);
    }

    #[test]
    fn root_is_bound_read_only_before_anything_overlays_it() {
        let a = argv(&spec());
        let ro_root = a
            .windows(3)
            .position(|w| w[0] == "--ro-bind" && w[1] == "/" && w[2] == "/")
            .expect("root ro-bind");
        let home_tmpfs = a
            .windows(2)
            .position(|w| w[0] == "--tmpfs" && w[1] == "/home/tester")
            .expect("home tmpfs");
        assert!(
            ro_root < home_tmpfs,
            "the read-only root must be laid down first, else the masks are overwritten"
        );
    }

    #[test]
    fn home_and_runtime_dir_are_masked() {
        let a = argv(&spec());
        assert!(a.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == "/home/tester"));
        assert!(a
            .windows(2)
            .any(|w| w[0] == "--tmpfs" && w[1] == "/run/user/1000"));
    }

    #[test]
    fn run_is_masked_so_the_system_bus_is_unreachable() {
        // The escalation this closes: /run/dbus/system_bus_socket is mode 0666,
        // rimed is on that bus, and its mutating methods are passwordless for
        // an active local user. A confined session is an active local user.
        let a = argv(&spec());
        assert!(
            a.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == "/run"),
            "/run must be masked, got {a:?}"
        );
        // And nothing binds the socket back. Asserted by absence, because the
        // whole point is that no code path mentions it.
        assert!(!a.join(" ").contains("system_bus_socket"));
    }

    #[test]
    fn the_run_mask_comes_before_everything_bound_back_into_it() {
        // $XDG_RUNTIME_DIR and the control socket both live under /run, so a
        // /run tmpfs emitted after either one silently erases it — and the
        // symptom is a session that cannot publish its own events, far from
        // the cause.
        let mut s = spec();
        s.run_ro = vec![PathBuf::from("/run/systemd/resolve/stub-resolv.conf")];
        let a = argv(&s);
        let run = a
            .windows(2)
            .position(|w| w[0] == "--tmpfs" && w[1] == "/run")
            .expect("run tmpfs");
        let sock = a
            .windows(3)
            .position(|w| {
                w[0] == "--bind-try" && w[1] == "/run/user/1000/rime-agentd/control.sock"
            })
            .expect("control socket bind");
        let resolv = a
            .windows(3)
            .position(|w| {
                w[0] == "--ro-bind-try" && w[1] == "/run/systemd/resolve/stub-resolv.conf"
            })
            .expect("resolv bind");
        assert!(run < sock, "the /run tmpfs would erase the control socket");
        assert!(run < resolv, "the /run tmpfs would erase the resolver bind");
    }

    #[test]
    fn the_resolver_is_bound_back_read_only_and_only_when_it_is_under_run() {
        // Read-only: a session that can rewrite the resolver configuration can
        // redirect every name lookup the rest of the machine makes.
        let mut s = spec();
        s.run_ro = vec![PathBuf::from("/run/systemd/resolve/stub-resolv.conf")];
        let a = argv(&s);
        assert!(has_bind(
            &a,
            "--ro-bind-try",
            "/run/systemd/resolve/stub-resolv.conf"
        ));
        assert!(!has_bind(
            &a,
            "--bind-try",
            "/run/systemd/resolve/stub-resolv.conf"
        ));

        // A machine whose /etc/resolv.conf is a plain file needs no bind: the
        // read-only root still covers /etc.
        let mut s = spec();
        s.run_ro = Vec::new();
        let a = argv(&s);
        assert!(!a.join(" ").contains("resolv.conf"));
    }

    /// Whether a real sandbox can be built and probed on this machine.
    ///
    /// Skipped rather than faked in CI, where there is no system bus. An argv
    /// assertion proves the flag is emitted; only running it proves the flag
    /// works, and those are different claims.
    fn can_probe_the_bus() -> Option<&'static str> {
        let busctl = "/usr/bin/busctl";
        if !bwrap_path().exists() {
            return None;
        }
        if !Path::new(busctl).exists() {
            return None;
        }
        if !Path::new("/run/dbus/system_bus_socket").exists() {
            return None;
        }
        Some(busctl)
    }

    /// A spec rooted in this machine's real paths, so bwrap can actually mount
    /// it. `cwd` is `/` because a per-session workdir does not exist here.
    fn live_spec() -> SandboxSpec {
        let home = crate::paths::home();
        let mut s = SandboxSpec::new(AgentPolicy::default(), home, crate::paths::runtime_dir());
        s.run_ro = resolv_binds();
        s.cwd = PathBuf::from("/");
        s
    }

    #[test]
    fn the_masked_run_really_does_block_the_system_bus() {
        let Some(busctl) = can_probe_the_bus() else {
            eprintln!("SKIP: no bwrap, no busctl, or no system bus on this machine");
            return;
        };

        // The probe is READ-ONLY: `busctl list` enumerates bus names and
        // changes nothing. A test that called a mutating method to prove
        // reachability would be the same mistake as the display suite applying
        // a layout to the live desktop — it succeeded, and that was the bug.
        let probe = ["--system", "--no-pager", "list"];

        // 1. The negative control FIRST, so a broken probe cannot masquerade
        //    as a working guard. This is the pre-fix shape: everything the real
        //    argv has except the /run tmpfs.
        let mut before: Vec<String> = [
            bwrap_path().to_string_lossy().as_ref(),
            "--ro-bind", "/", "/",
            "--proc", "/proc",
            "--dev", "/dev",
            "--tmpfs", "/tmp",
            "--unshare-pid", "--unshare-ipc", "--unshare-uts",
            "--die-with-parent",
            "--",
            busctl,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        before.extend(probe.iter().map(|s| s.to_string()));

        let control = std::process::Command::new(&before[0])
            .args(&before[1..])
            .output()
            .expect("running the negative control");
        assert!(
            control.status.success(),
            "WITHOUT the /run mask the bus must be reachable, or this test proves \
             nothing about the mask. stderr: {}",
            String::from_utf8_lossy(&control.stderr)
        );

        // 2. The real argv. Same probe, same machine, one mount different.
        let argv = build_argv(&live_spec(), busctl, &probe.map(String::from)).expect("build");
        let confined = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .expect("running the confined probe");
        let stderr = String::from_utf8_lossy(&confined.stderr);
        assert!(
            !confined.status.success(),
            "the system bus was REACHABLE from inside the sandbox — this is the \
             rimed escalation path. stdout: {}",
            String::from_utf8_lossy(&confined.stdout)
        );
        assert!(
            stderr.contains("Failed to connect") || stderr.contains("No such file"),
            "expected a connect failure, got: {stderr}"
        );
    }

    #[test]
    fn a_confined_session_can_still_resolve_names() {
        // The regression that would get the fix above reverted. Masking /run
        // breaks /etc/resolv.conf, which is a symlink into it, and a sandbox
        // with no DNS is a sandbox nobody will keep switched on.
        //
        // `getent hosts` is used rather than a network request: it exercises
        // the resolver path this bind exists for without needing the machine to
        // be online, and localhost always resolves.
        if !bwrap_path().exists() || !Path::new("/usr/bin/getent").exists() {
            eprintln!("SKIP: no bwrap or no getent");
            return;
        }
        let argv = build_argv(
            &live_spec(),
            "/usr/bin/getent",
            &["hosts".to_string(), "localhost".to_string()],
        )
        .expect("build");
        let out = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .expect("running getent");
        assert!(
            out.status.success(),
            "name resolution broke inside the sandbox. stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn strict_policy_still_builds_a_sandbox_that_runs() {
        // strict adds --unshare-net on top of the /run mask. A `-try` bind
        // against a path that cannot be resolved in a network-isolated
        // namespace fails differently from the project case, so this asserts
        // the sandbox still STARTS rather than anything about the network.
        if !bwrap_path().exists() {
            eprintln!("SKIP: no bwrap");
            return;
        }
        let mut s = live_spec();
        s.policy.sandbox = SandboxPolicy::Strict;
        // /bin/sh, not /usr/bin/true: the one program path both the bootc image and NixOS have.
        let argv = build_argv(&s, "/bin/sh", &["-c".to_string(), "exit 0".to_string()]).expect("build");
        let out = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .expect("running true");
        assert!(
            out.status.success(),
            "a strict sandbox failed to start. stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn resolv_binds_returns_only_paths_under_run() {
        // Whatever this machine's configuration is, the contract holds: every
        // returned path is absolute and under /run, because a target elsewhere
        // is already covered by the read-only root and binding it would be a
        // second, unnecessary hole.
        for p in resolv_binds() {
            assert!(p.is_absolute(), "{p:?}");
            assert!(p.starts_with("/run"), "{p:?}");
        }
    }

    #[test]
    fn the_ssh_agent_socket_is_unreachable() {
        // §4: "protect SSH keys and API credentials from arbitrary file reads."
        // A reachable ssh-agent is as good as the key: a confined session could
        // sign anything with it without ever reading ~/.ssh.
        //
        // It is unreachable because SSH_AUTH_SOCK conventionally lives under
        // $XDG_RUNTIME_DIR, which is masked — and now also under /run, which is
        // masked too. Asserted rather than assumed, because the /run tmpfs was
        // added AFTER the runtime-dir one and the ordering had to be gone
        // through again; a reordering that put the runtime-dir mask first would
        // have left this hole open with no visible symptom.
        let mut s = spec();
        // The two shapes an agent socket really takes.
        for sock in [
            "/run/user/1000/keyring/ssh",
            "/run/user/1000/gcr/ssh",
            "/tmp/ssh-XXXX/agent.1234",
        ] {
            let a = argv(&s);
            assert!(
                !a.join(" ").contains(sock),
                "{sock} must not be bound: {a:?}"
            );
        }
        // And the directories they live in are masked, not merely unmentioned.
        let a = argv(&s);
        assert!(a.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == "/run"));
        assert!(a
            .windows(2)
            .any(|w| w[0] == "--tmpfs" && w[1] == "/run/user/1000"));
        assert!(a.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == "/tmp"));

        // The one thing bound back under the runtime dir is the control socket,
        // by exact path. Nothing else in $XDG_RUNTIME_DIR is reachable.
        s.control_socket = PathBuf::from("/run/user/1000/rime-agentd/control.sock");
        let a = argv(&s);
        let bound: Vec<&str> = a
            .windows(3)
            .filter(|w| (w[0] == "--bind-try" || w[0] == "--bind") && w[1].starts_with("/run/user/"))
            .map(|w| w[1].as_str())
            .collect();
        assert_eq!(
            bound,
            vec!["/run/user/1000/rime-agentd/control.sock"],
            "only the control socket may be bound back under the runtime dir"
        );
    }

    #[test]
    fn the_secret_store_is_unreachable() {
        // Two stores, and neither is reachable for a different reason.
        //
        // `rime-secretd` owns credentials now and keeps them in
        // /var/lib/rime-secretd, which is root-owned and would be unreadable
        // even if it were bound — but it is not bound, and P0-002's leftovers
        // from the old broker are still under $XDG_STATE_HOME on an upgraded
        // machine. $HOME is masked, so those stay out of reach as well.
        let a = argv(&spec()).join(" ");
        for path in [
            "/var/lib/rime-secretd",
            "/home/tester/.local/state/rime/agent/secrets",
            // The same two under their names before the rename, which an
            // upgraded machine can still carry.
            "/var/lib/apex-secretd",  // rime-rename: keep
            "/home/tester/.local/state/apex/agent/secrets",  // rime-rename: keep
            "/home/tester/.local/state",
        ] {
            assert!(!a.contains(path), "{path} must not be bound: {a}");
        }
    }

    #[test]
    fn secrets_are_never_bound_back() {
        // The point of default-deny: no allowlist entry mentions these, so no
        // bind for them can appear anywhere in the argv.
        let a = argv(&spec()).join(" ");
        for secret in [
            "/home/tester/.ssh",
            "/home/tester/.gnupg",
            "/home/tester/.aws",
            "/home/tester/.mozilla",
            "/home/tester/.config/google-chrome",
            "/home/tester/Documents",
        ] {
            assert!(!a.contains(secret), "{secret} must not appear in {a}");
        }
    }

    #[test]
    fn the_project_is_writable_and_the_toolchain_cache_is_not() {
        let a = argv(&spec());
        assert!(has_bind(&a, "--bind-try", "/home/tester/Projects/demo"));
        assert!(has_bind(&a, "--ro-bind-try", "/home/tester/.cargo"));
        assert!(!has_bind(&a, "--bind-try", "/home/tester/.cargo"));
    }

    #[test]
    fn the_masked_home_is_laid_down_before_the_paths_bound_back_into_it() {
        let a = argv(&spec());
        let tmpfs = a
            .windows(2)
            .position(|w| w[0] == "--tmpfs" && w[1] == "/home/tester")
            .unwrap();
        let project = a
            .windows(3)
            .position(|w| w[0] == "--bind-try" && w[1] == "/home/tester/Projects/demo")
            .unwrap();
        let cargo = a
            .windows(3)
            .position(|w| w[0] == "--ro-bind-try" && w[1] == "/home/tester/.cargo")
            .unwrap();
        assert!(tmpfs < project, "tmpfs would erase the project bind");
        assert!(tmpfs < cargo, "tmpfs would erase the cache bind");
    }

    #[test]
    fn a_minimal_dev_removes_camera_and_microphone() {
        // `--dev` builds a fresh devtmpfs with only the standard character
        // devices; there is no bind that could bring /dev/video* or /dev/snd
        // back.
        let a = argv(&spec());
        assert!(a.windows(2).any(|w| w[0] == "--dev" && w[1] == "/dev"));
        let joined = a.join(" ");
        assert!(!joined.contains("/dev/video"));
        assert!(!joined.contains("/dev/snd"));
        assert!(!joined.contains("--dev-bind"));
    }

    #[test]
    fn project_policy_keeps_the_network_and_strict_removes_it() {
        let mut s = spec();
        s.policy.sandbox = SandboxPolicy::Project;
        assert!(pos(&argv(&s), "--unshare-net").is_none());
        s.policy.sandbox = SandboxPolicy::Strict;
        assert!(pos(&argv(&s), "--unshare-net").is_some());
    }

    #[test]
    fn the_network_namespace_follows_the_network_dimension_not_the_sandbox_one() {
        // §3.1 splits these. `strict` still removes the network — it forces
        // the dimension to `offline` — but the argv reads the dimension, which
        // is what lets the other network modes exist without touching the
        // sandbox modes.
        let mut s = spec();
        s.policy.sandbox = SandboxPolicy::Project;
        s.policy.network = NetworkPolicy::Offline;
        assert!(pos(&argv(&s), "--unshare-net").is_some());

        s.policy.network = NetworkPolicy::Open;
        assert!(pos(&argv(&s), "--unshare-net").is_none());
    }

    #[test]
    fn open_is_the_only_mode_that_keeps_the_host_network_namespace() {
        // The kernel half of every network mode, asserted over the whole value
        // set rather than one variant at a time — a mode added later without a
        // decision about its namespace fails here instead of shipping with the
        // host's network.
        for network in NetworkPolicy::ALL {
            // Built from the allowlisted spec so every mode has what it needs;
            // the bridge is ignored by the three that are not allowlisted.
            let mut s = allowlisted();
            s.policy.network = *network;
            assert_eq!(
                pos(&argv(&s), "--unshare-net").is_some(),
                *network != NetworkPolicy::Open,
                "{network} got the wrong network namespace"
            );
        }
    }

    /// A spec in `allowlist` mode, with the bridge the mode requires.
    fn allowlisted() -> SandboxSpec {
        let mut s = spec();
        s.policy.sandbox = SandboxPolicy::Project;
        s.policy.network = NetworkPolicy::Allowlist;
        s.egress = Some(EgressBridge {
            program: PathBuf::from("/usr/bin/rime-agentd"),
            socket: PathBuf::from("/tmp/rime-agent/1/egress.sock"),
            port: BRIDGE_PORT,
        });
        s
    }

    #[test]
    fn an_allowlisted_session_has_no_network_of_its_own_and_runs_under_the_bridge() {
        // The two halves of the mode, in the argv that runs. The namespace is
        // what makes it enforcement rather than a request; the bridge is the
        // one route back, and it is the session's parent process because the
        // loopback a client can reach is the session's own.
        let a = argv(&allowlisted());
        assert!(pos(&a, "--unshare-net").is_some(), "{a:?}");

        let sep = pos(&a, "--").expect("separator");
        assert_eq!(a[sep + 1], "/usr/bin/rime-agentd");
        assert_eq!(a[sep + 2], BRIDGE_FLAG);
        assert_eq!(a[sep + 3], "/tmp/rime-agent/1/egress.sock");
        assert_eq!(a[sep + 4], BRIDGE_PORT.to_string());
        // A second separator, so an agent argument that looks like a bridge
        // flag stays an agent argument.
        assert_eq!(a[sep + 5], "--");
        assert_eq!(a[sep + 6], "claude");
        assert_eq!(a[sep + 7], "--help");
        assert_eq!(sep + 8, a.len());
    }

    #[test]
    fn an_allowlisted_session_without_a_bridge_is_refused_not_run() {
        // The fail-closed case that matters most. Without this the argv comes
        // back as an ordinary confined session with `--unshare-net` and no way
        // out — which reports `allowlist` and behaves as `offline`, so the
        // user believes they have a network policy and the agent believes the
        // machine is offline.
        let mut s = allowlisted();
        s.egress = None;
        assert_eq!(
            build_argv(&s, "claude", &[]).unwrap_err(),
            SandboxError::EgressUnavailable
        );
    }

    #[test]
    fn a_network_mode_needing_a_namespace_is_refused_on_the_unconfined_path() {
        // `build_argv` returns the bare command for an unrestricted session,
        // so without this check every mode but `open` would come back as a
        // command running on the host's network. The policy refuses that pair
        // too; this is the last place the mistake can still be caught.
        for network in NetworkPolicy::ALL {
            let mut s = spec();
            s.policy.sandbox = SandboxPolicy::Unrestricted;
            s.policy.network = *network;
            let built = build_argv(&s, "claude", &[]);
            if *network == NetworkPolicy::Open {
                assert!(built.is_ok(), "{network}");
            } else {
                assert_eq!(
                    built.unwrap_err(),
                    SandboxError::NetworkWithoutNamespace(*network),
                    "an unconfined session was built with {network}"
                );
            }
        }
    }

    #[test]
    fn only_an_allowlisted_session_is_told_where_the_proxy_is() {
        // The variables are not the enforcement — the namespace is — but a
        // client that does not know about the bridge cannot use it, and one
        // told about a bridge that is not there would fail every request.
        let names = [
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
            "ALL_PROXY",
            "all_proxy",
        ];
        let env = resolved_env(&allowlisted());
        for name in names {
            assert_eq!(
                env.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()),
                Some(format!("http://127.0.0.1:{BRIDGE_PORT}").as_str()),
                "{name}"
            );
        }

        // Every other mode gets none of them, including one that was handed a
        // bridge it is not supposed to use.
        for network in [
            NetworkPolicy::Open,
            NetworkPolicy::Offline,
            NetworkPolicy::Brokered,
        ] {
            let mut s = allowlisted();
            s.policy.network = network;
            let env = resolved_env(&s);
            for name in names {
                assert!(
                    !env.iter().any(|(k, _)| k == name),
                    "{network} was told about a proxy"
                );
            }
        }
    }

    #[test]
    fn the_proxy_variables_cannot_be_pointed_somewhere_else_by_an_adapter() {
        // They come first, and first wins in `resolved_env`. An adapter that
        // set HTTPS_PROXY — or a daemon environment carrying one — must not
        // be able to route a confined session's traffic past the bridge.
        let mut s = allowlisted();
        s.env_set = vec![("HTTPS_PROXY".into(), "http://elsewhere.example:8080".into())];
        s.env_pass = vec!["HTTPS_PROXY".into()];
        let env = resolved_env(&s);
        let found: Vec<&String> = env
            .iter()
            .filter(|(k, _)| k == "HTTPS_PROXY")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(found.len(), 1, "{env:?}");
        assert_eq!(found[0], &format!("http://127.0.0.1:{BRIDGE_PORT}"));
    }

    #[test]
    fn brokered_is_offline_in_the_kernel_and_differs_only_above_it() {
        // Worth asserting because it is the honest description of the mode:
        // the confinement is identical, and what separates them is what
        // rime-agentd will do over the control socket. A future edit that gave
        // brokered a weaker namespace would be a real downgrade, and it would
        // be invisible in a test that only checked for `--unshare-net`.
        let mut brokered = spec();
        brokered.policy.sandbox = SandboxPolicy::Project;
        brokered.policy.network = NetworkPolicy::Brokered;

        let mut offline = spec();
        offline.policy.sandbox = SandboxPolicy::Project;
        offline.policy.network = NetworkPolicy::Offline;

        assert_eq!(argv(&brokered), argv(&offline));
    }

    #[test]
    fn strict_is_project_plus_offline_and_produces_the_same_argv() {
        // The two spellings must be one thing, or `strict` and the split
        // dimensions would drift into two subtly different sandboxes.
        let mut strict = spec();
        strict.policy.sandbox = SandboxPolicy::Strict;

        let mut split = spec();
        split.policy.sandbox = SandboxPolicy::Project;
        split.policy.network = NetworkPolicy::Offline;

        assert_eq!(argv(&strict), argv(&split));
    }

    #[test]
    fn a_strict_spec_cannot_be_built_with_the_network_left_open() {
        // The floor, asserted where it is enforced rather than only where it
        // is computed: a caller that sets `network: Open` on a strict spec
        // still gets a network-isolated sandbox.
        let mut s = spec();
        s.policy.sandbox = SandboxPolicy::Strict;
        for network in NetworkPolicy::ALL {
            s.policy.network = *network;
            assert!(
                pos(&argv(&s), "--unshare-net").is_some(),
                "strict lost its network isolation with network={network}"
            );
        }
    }

    #[test]
    fn the_agent_native_permission_mode_changes_nothing_about_the_confinement() {
        // P0-004 criterion 2, at the level where it is actually enforced.
        // A field assertion proves the struct; this proves the argv that runs.
        let baseline = argv(&spec());
        for native in crate::policy::NativeMode::ALL {
            let mut s = spec();
            s.policy.native = *native;
            assert_eq!(
                argv(&s),
                baseline,
                "the {native} native mode changed the sandbox argv"
            );
        }
    }

    #[test]
    fn strict_keeps_every_project_restriction() {
        let mut s = spec();
        s.policy.sandbox = SandboxPolicy::Strict;
        let a = argv(&s);
        assert!(a.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == "/home/tester"));
        assert!(has_bind(&a, "--bind-try", "/home/tester/Projects/demo"));
        assert!(pos(&a, "--unshare-pid").is_some());
    }

    #[test]
    fn a_redacted_copy_is_bound_over_the_file_it_replaces() {
        // The whole mechanism in one assertion: the real settings file is
        // bound, and then the copy lands on the same path. Reversed, the
        // session reads the original and the credential is back.
        let mut s = spec();
        let real = PathBuf::from("/home/tester/.claude/settings.json");
        let copy = PathBuf::from("/tmp/rime-agent/1/claude-settings-redacted.json");
        s.ro.push(real.clone());
        s.ro_at.push((copy.clone(), real.clone()));
        let a = argv(&s);

        let original = a
            .windows(3)
            .position(|w| w[0] == "--ro-bind-try" && w[1] == real.to_str().unwrap())
            .expect("the real settings file is bound");
        let over = a
            .windows(3)
            .position(|w| {
                w[0] == "--ro-bind-try"
                    && w[1] == copy.to_str().unwrap()
                    && w[2] == real.to_str().unwrap()
            })
            .expect("the copy is bound at the real path");
        assert!(
            original < over,
            "the copy must come after what it replaces: {a:?}"
        );
    }

    #[test]
    fn a_redacted_copy_is_bound_before_the_credential_masks() {
        // Step 8 is "nothing bound above can bring a credential back", and a
        // bind that ran after it would be exactly that.
        let mut s = spec();
        s.ro_at.push((
            PathBuf::from("/tmp/rime-agent/1/copy.json"),
            PathBuf::from("/home/tester/.claude/settings.json"),
        ));
        s.mask.push(PathBuf::from("/home/tester/.cargo/credentials.toml"));
        let a = argv(&s);
        let over = a
            .windows(3)
            .position(|w| w[1] == "/tmp/rime-agent/1/copy.json")
            .expect("the copy");
        let mask = a
            .windows(3)
            .position(|w| w[1] == "/dev/null")
            .expect("the mask");
        assert!(over < mask, "{a:?}");
    }

    #[test]
    fn an_empty_redacted_copy_entry_binds_nothing() {
        // Same rule as every other list here: an empty path would become
        // `--ro-bind-try "" ""`, which bwrap reads as a bind of the working
        // directory.
        let mut s = spec();
        s.ro_at.push((PathBuf::new(), PathBuf::from("/home/tester/x")));
        s.ro_at.push((PathBuf::from("/tmp/y"), PathBuf::new()));
        let a = argv(&s);
        assert!(!a.iter().any(|x| x.is_empty()), "{a:?}");
        assert!(!a.iter().any(|x| x == "/tmp/y"), "{a:?}");
    }

    #[test]
    fn pid_namespace_is_always_unshared_when_confined() {
        assert!(pos(&argv(&spec()), "--unshare-pid").is_some());
    }

    #[test]
    fn no_new_session_so_the_pty_stays_the_controlling_terminal() {
        // `--new-session` would call setsid() and detach the PTY, breaking
        // every TUI agent. preflight() covers the TIOCSTI risk instead.
        assert!(pos(&argv(&spec()), "--new-session").is_none());
    }

    #[test]
    fn the_command_is_separated_from_bwrap_options_by_a_double_dash() {
        let a = argv(&spec());
        let sep = pos(&a, "--").expect("separator");
        assert_eq!(a[sep + 1], "claude");
        assert_eq!(a[sep + 2], "--help");
        assert_eq!(sep + 3, a.len());
    }

    #[test]
    fn an_argument_that_looks_like_a_bwrap_flag_is_not_interpreted_as_one() {
        // Everything after `--` belongs to the agent. A prompt of
        // "--unshare-net" must reach the agent, not reconfigure the sandbox.
        let s = spec();
        let a = build_argv(&s, "claude", &["--unshare-net".to_string()]).unwrap();
        let sep = pos(&a, "--").unwrap();
        assert_eq!(a[sep + 2], "--unshare-net");
        assert!(
            a[..sep].iter().all(|x| x != "--unshare-net"),
            "the agent argument leaked into the sandbox options"
        );
    }

    #[test]
    fn the_environment_is_cleared_before_anything_is_set() {
        let a = argv(&spec());
        let clear = pos(&a, "--clearenv").expect("clearenv");
        let first_setenv = pos(&a, "--setenv");
        if let Some(set) = first_setenv {
            assert!(clear < set, "--clearenv must precede every --setenv");
        }
    }

    #[test]
    fn explicit_values_beat_inherited_ones() {
        let mut s = spec();
        s.env_set = vec![("TERM".into(), "dumb".into())];
        s.env_pass = vec!["TERM".into()];
        let env = resolved_env(&s);
        let terms: Vec<_> = env.iter().filter(|(k, _)| k == "TERM").collect();
        assert_eq!(terms.len(), 1, "TERM set twice: {env:?}");
        assert_eq!(terms[0].1, "dumb");
    }

    #[test]
    fn undeclared_credentials_are_not_inherited() {
        let mut s = spec();
        s.env_pass = vec!["ANTHROPIC_API_KEY".into()];
        let env = resolved_env(&s);
        // GITHUB_TOKEN was never declared by the adapter, so even if the user's
        // shell exports one it cannot appear.
        assert!(!env.iter().any(|(k, _)| k == "GITHUB_TOKEN"));
        assert!(!env.iter().any(|(k, _)| k == "AWS_SECRET_ACCESS_KEY"));
        assert!(!env.iter().any(|(k, _)| k == "SSH_AUTH_SOCK"));
    }

    #[test]
    fn resolved_env_is_sorted_so_the_argv_is_deterministic() {
        let mut s = spec();
        s.env_set = vec![
            ("ZZZ".into(), "1".into()),
            ("AAA".into(), "2".into()),
            ("MMM".into(), "3".into()),
        ];
        let names: Vec<_> = resolved_env(&s).into_iter().map(|(k, _)| k).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn writable_binds_that_would_defeat_the_policy_are_refused() {
        for bad in [
            "/",
            "/usr",
            "/etc",
            "/boot",
            "/usr/",
            "/home/tester",
            "/var/lib/rime",
            "/var/lib/apex",  // rime-rename: keep (the pre-rename state directory)
        ] {
            let mut s = spec();
            s.rw = vec![PathBuf::from(bad)];
            let err = build_argv(&s, "claude", &[]).unwrap_err();
            assert!(
                matches!(err, SandboxError::ForbiddenWritable(_)),
                "{bad} was accepted as writable: {err:?}"
            );
        }
    }

    #[test]
    fn a_subdirectory_of_home_is_still_allowed_to_be_writable() {
        let mut s = spec();
        s.rw = vec![PathBuf::from("/home/tester/Projects/demo")];
        assert!(build_argv(&s, "claude", &[]).is_ok());
    }

    #[test]
    fn relative_paths_are_refused() {
        let mut s = spec();
        s.rw = vec![PathBuf::from("relative/path")];
        assert!(matches!(
            build_argv(&s, "claude", &[]).unwrap_err(),
            SandboxError::NotAbsolute(_)
        ));

        let mut s = spec();
        s.cwd = PathBuf::from("relative");
        assert!(matches!(
            build_argv(&s, "claude", &[]).unwrap_err(),
            SandboxError::NotAbsolute(_)
        ));
    }

    #[test]
    fn the_control_socket_is_bound_writable_so_events_can_be_published() {
        let a = argv(&spec());
        assert!(has_bind(
            &a,
            "--bind-try",
            "/run/user/1000/rime-agentd/control.sock"
        ));
    }

    #[test]
    fn the_secret_service_socket_is_not_bound_into_a_confined_session() {
        // P0-002 puts the credential store behind `rime-secretd`, whose socket
        // is `/run/rime-secretd/control.sock`. A confined session has no
        // business opening it: brokered use goes through `rime-agentd`, which
        // is outside the sandbox and is the only thing that knows which session
        // is asking. Talking to the service directly would let a session
        // present itself as an unsessioned caller.
        //
        // The `/run` tmpfs already achieves this — nothing under `/run` is
        // visible unless it is bound back, and only the agent runtime's own
        // socket is. Asserted rather than assumed, because the whole reason
        // `/run` is masked is that a `--ro-bind / /` once made every socket in
        // it reachable, and the fix is one line away from being reverted.
        let a = argv(&spec());
        assert!(a.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == "/run"));
        assert!(
            !a.iter().any(|arg| arg.contains("rime-secretd")),
            "the secret service socket reached a confined session's argv: {a:?}"
        );
    }

    #[test]
    fn preflight_is_a_no_op_for_unrestricted() {
        assert_eq!(preflight(SandboxPolicy::Unrestricted), Ok(()));
    }

    #[test]
    fn sandbox_errors_name_the_escape_hatch() {
        // A user who hits this needs to know what to do next, not just that
        // something failed.
        for err in [SandboxError::MissingBwrap, SandboxError::TiocstiEnabled] {
            assert!(
                err.to_string().contains("unrestricted"),
                "unhelpful message: {err}"
            );
        }
    }
}
