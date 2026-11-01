//! §10.2 — one confinement per MCP server, rather than the session's.
//!
//! An MCP server is a program the agent starts, and today it starts inside the
//! agent's own sandbox and gets everything that sandbox has: the project
//! writable, the network open, the toolchain caches, the agent's profile. A
//! memory server that stores notes in one file therefore runs with write access
//! to the source tree, and `npx -y @modelcontextprotocol/server-memory` is a
//! package fetched from a registry at first run — third-party code with the
//! agent's reach.
//!
//! ```text
//! "memory": {"command": "rime",
//!            "args": ["mcp", "run", "memory", "--",
//!                     "npx", "-y", "@modelcontextprotocol/server-memory"]}
//! ```
//!
//! [`build`] turns a policy into a [`SandboxSpec`] and hands it to
//! [`rime_agent_core::sandbox::build_argv`] — the same function that confines an
//! agent session, and the only one. A second bubblewrap profile in this
//! codebase would be a second thing to get wrong, and the argv it produced
//! would drift from the one that is tested.
//!
//! ## The three dimensions, and what each is actually worth
//!
//! **Filesystem.** Default-deny, and it costs nothing extra: `build_argv`
//! already masks `$HOME`, `/run` and `$XDG_RUNTIME_DIR` and binds back only
//! what a spec lists. What this adds is a *different* home — a private
//! directory per server, under the runtime's state — so a server that writes
//! beside itself writes there and nowhere the agent or the next server can see.
//! The project root is not bound unless the policy asks.
//!
//! **Network.** Default `--unshare-net`, which is the whole of the kernel
//! enforcement. `network = true` gives the server the *parent's* namespace, and
//! that is a ceiling rather than a grant: a namespace cannot be un-shared
//! upward, so a server declared `network = true` inside an offline session
//! still has none. An MCP server cannot be used as a way out of a session that
//! was confined without one.
//!
//! **Secrets.** The honest answer is narrower than the word suggests. The
//! sidecar runs as the agent's own account, so neither `rime-agentd` nor
//! `rime-secretd` can tell it apart from the agent — a credential it could
//! fetch is a credential the agent could fetch, and `/proc/<pid>/environ` is
//! readable to the same uid either way. **Per-MCP identity at the broker does
//! not exist**, and nothing here pretends otherwise. What *is* enforceable is
//! reachability: the daemons' sockets live under `/run` and
//! `$XDG_RUNTIME_DIR`, both masked, so by default a server cannot open either.
//! `broker = true` binds back the one socket a confined process is ever given —
//! `rime-agentd`'s, under `$XDG_RUNTIME_DIR` — and the grant table then decides
//! as it always did. `rime-secretd`'s own socket is not bound and must not be:
//! a session does not get it either, and an MCP server holding a door into the
//! secret daemon that the agent starting it lacks is the inverse of a sandbox.
//!
//! ## What this is a boundary against, and what it is not
//!
//! It confines the MCP server's own code. It is **not** a boundary against a
//! hostile agent: `~/.claude.json` is bound writable into a session, because
//! Claude records onboarding state in it, so a session can rewrite a definition
//! to drop the wrapper — and could have run the same program directly in any
//! case. Closing that means starting the agent with `--strict-mcp-config
//! --mcp-config <a file the daemon wrote>`, which is a change to how sessions
//! are launched and is named as a remainder rather than half-done here.
//!
//! The policy file is read-only inside a session for the same reason it is
//! worth having: an agent that could rewrite it could widen every server's
//! confinement at once.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rime_agent_core::policy::{AgentPolicy, NetworkPolicy};
use rime_agent_core::protocol::SandboxPolicy;
use rime_agent_core::sandbox::{self, SandboxSpec};

/// Where a policy was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Nobody wrote one, so the server gets the tightest thing that runs.
    Default,
    /// A file. The user's own wins over the machine's.
    File(PathBuf),
}

impl Source {
    pub fn describe(&self) -> String {
        match self {
            Source::Default => "the default, because no policy file names this server".to_string(),
            Source::File(p) => p.display().to_string(),
        }
    }
}

/// What one MCP server may reach.
///
/// Every field defaults to the closed answer, so a policy file only ever
/// widens and a missing one is the tightest thing that still runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPolicy {
    pub name: String,
    /// Whether the server keeps the parent's network namespace.
    pub network: bool,
    /// Whether the project root is bound writable.
    pub project: bool,
    /// Whether the secret service's socket is reachable.
    pub broker: bool,
    /// Extra read-only paths.
    pub read: Vec<PathBuf>,
    /// Extra writable paths.
    pub write: Vec<PathBuf>,
    /// Variable names carried through from the parent. Nothing else is.
    pub env: Vec<String>,
    pub source: Source,
}

impl McpPolicy {
    /// The closed answer, which is what a server with no policy file gets.
    pub fn closed(name: &str) -> McpPolicy {
        McpPolicy {
            name: name.to_string(),
            network: false,
            project: false,
            broker: false,
            read: Vec::new(),
            write: Vec::new(),
            env: Vec::new(),
            source: Source::Default,
        }
    }

    /// In words, for `rime mcp policy` and for `rime mcp list`.
    pub fn describe(&self) -> Vec<String> {
        let mut out = vec![
            format!(
                "network     {}",
                if self.network {
                    "the parent's, which is a ceiling and not a grant"
                } else {
                    "none — its own empty namespace"
                }
            ),
            format!(
                "filesystem  a private home{}{}",
                if self.project { ", the project" } else { "" },
                match (self.read.len(), self.write.len()) {
                    (0, 0) => String::new(),
                    (r, w) => format!(", {r} read-only and {w} writable path(s) it names"),
                }
            ),
            format!(
                "secrets     {}",
                if self.broker {
                    "may reach rime-agentd, where this project's grants decide"
                } else {
                    "cannot reach rime-agentd or rime-secretd at all"
                }
            ),
        ];
        if !self.env.is_empty() {
            out.push(format!("environment {}", self.env.join(", ")));
        }
        out
    }
}

/// Where policies are read from, tightest source last.
///
/// The user's own wins. The machine-wide file is a default an image or an
/// administrator can ship for a server people commonly run, and the point of a
/// per-MCP policy is that the person running the server decides what it may
/// reach.
pub const MACHINE_DIR: &str = "/etc/rime/mcp";

/// The user's policy directory.
///
/// Under `$XDG_CONFIG_HOME`, and bound **read-only** into a confined session by
/// the agent profile — so a session can read the policy its own MCP servers run
/// under and cannot widen it.
pub fn user_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| super::home().join(".config"));
    base.join("rime/mcp")
}

/// The policy for one server.
///
/// A file that does not parse is an error and never a silent fallback to the
/// default: the default is *tighter*, so falling back would break the server
/// and blame the server.
pub fn load(name: &str) -> Result<McpPolicy> {
    for dir in [user_dir(), PathBuf::from(MACHINE_DIR)] {
        let file = dir.join(format!("{name}.toml"));
        match std::fs::read_to_string(&file) {
            Ok(text) => {
                return parse(name, &text, Source::File(file.clone()))
                    .with_context(|| format!("reading {}", file.display()))
            }
            // Anything but "not there" is worth failing on: an unreadable
            // policy is not an absent one, and treating it as absent is the
            // defect class this codebase has already been through once.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => bail!("cannot read {}: {e}", file.display()),
        }
    }
    Ok(McpPolicy::closed(name))
}

/// Parse a policy document.
///
/// An unknown key is refused rather than ignored, for the reason
/// `OperationSpec::check` refuses an undeclared parameter: a policy with
/// `netwrok = true` in it that started the server with no network would read as
/// a setting that had been applied.
pub fn parse(name: &str, text: &str, source: Source) -> Result<McpPolicy> {
    let doc: toml::Value = toml::from_str(text).context("this is not a TOML document")?;
    let table = doc
        .as_table()
        .ok_or_else(|| anyhow::anyhow!("a policy is a table of settings"))?;

    let mut policy = McpPolicy::closed(name);
    policy.source = source;
    for (key, value) in table {
        match key.as_str() {
            "network" => policy.network = boolean(key, value)?,
            "project" => policy.project = boolean(key, value)?,
            "broker" => policy.broker = boolean(key, value)?,
            "read" => policy.read = paths(key, value)?,
            "write" => policy.write = paths(key, value)?,
            "env" => policy.env = names(key, value)?,
            other => bail!(
                "'{other}' is not a setting this understands. The ones there are: \
                 network, project, broker, read, write, env"
            ),
        }
    }
    Ok(policy)
}

fn boolean(key: &str, value: &toml::Value) -> Result<bool> {
    value
        .as_bool()
        .ok_or_else(|| anyhow::anyhow!("'{key}' is true or false"))
}

fn paths(key: &str, value: &toml::Value) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for item in value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("'{key}' is a list of paths"))?
    {
        let text = item
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("every entry in '{key}' is a path"))?;
        let expanded = match text.strip_prefix("~/") {
            Some(rest) => super::home().join(rest),
            None => PathBuf::from(text),
        };
        if !expanded.is_absolute() {
            bail!(
                "'{text}' in '{key}' is not an absolute path, and a relative one would mean \
                 something different depending on where the agent happened to start"
            );
        }
        out.push(expanded);
    }
    Ok(out)
}

fn names(key: &str, value: &toml::Value) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for item in value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("'{key}' is a list of variable names"))?
    {
        let text = item
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("every entry in '{key}' is a variable name"))?;
        if text.is_empty() || !text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            bail!("'{text}' is not a variable name");
        }
        out.push(text.to_string());
    }
    Ok(out)
}

/// The private directory one server gets as its home.
///
/// Per server, under the runtime's own state, so two servers cannot read each
/// other's working files and neither can reach the agent's profile. `npx` needs
/// a writable `~/.npm` and a memory server writes its store beside itself;
/// both land here.
pub fn private_home(name: &str) -> PathBuf {
    rime_agent_core::paths::state_dir().join("mcp").join(name)
}

/// The spec one MCP server runs under.
///
/// Two homes, and the distinction is the whole of the filesystem dimension.
/// `user_home` is the account's real one, and it is here to be **masked** — the
/// tmpfs `build_argv` puts over it is what makes the agent's profile, the
/// browser profiles and `~/.ssh` unreachable. `server_home` is the private
/// directory this server gets instead, bound back writable on top of that mask
/// and handed over as `HOME`.
///
/// Pure, apart from the paths it is handed: everything that touches the
/// filesystem is the caller's, so the argv can be asserted exhaustively the way
/// the session sandbox's is.
pub fn build(
    policy: &McpPolicy,
    user_home: &Path,
    server_home: &Path,
    runtime_dir: &Path,
    project: Option<&Path>,
) -> SandboxSpec {
    let agent = AgentPolicy {
        sandbox: SandboxPolicy::Project,
        network: if policy.network {
            NetworkPolicy::Open
        } else {
            NetworkPolicy::Offline
        },
        ..AgentPolicy::default()
    };
    let mut spec = SandboxSpec::new(agent, user_home.to_path_buf(), runtime_dir.to_path_buf());

    // Bound back on top of the tmpfs that masks the account's home, so what the
    // server writes survives a restart and stays out of everything else. It has
    // to be a directory *inside* the masked home or somewhere else entirely:
    // `build_argv` refuses a writable bind of the home itself, because that
    // would hand back everything the mask just removed.
    spec.rw.push(server_home.to_path_buf());
    spec.rw.extend(policy.write.iter().cloned());
    if policy.project {
        spec.rw.extend(project.map(Path::to_path_buf));
    }
    spec.ro.extend(policy.read.iter().cloned());
    spec.cwd = server_home.to_path_buf();

    // Name resolution, and only when there is a network to resolve for.
    if policy.network {
        spec.run_ro = sandbox::resolv_binds();
    }
    // The way into the broker is the agent runtime's control socket, under
    // `$XDG_RUNTIME_DIR`, which `build_argv` masks with a tmpfs. Binding it
    // back is the whole of the `broker` dimension: `rime secret use` and
    // `rime mcp bridge` both connect there, `rime-agentd` stamps the request
    // and forwards it, and the grant table decides everything after that —
    // exactly as it does for a session, which is bound the same socket by
    // `rime-agentd`'s own session builder.
    //
    // Not `rime_secret_core::paths::socket()`. That is the secret daemon's own
    // socket, and a confined agent session is never given it: the daemon is
    // reached *through* the runtime, not beside it. Binding it here would have
    // handed an MCP server a door into rime-secretd that the agent starting the
    // server does not have, which is the inverse of what a per-server sandbox
    // is for.
    if policy.broker {
        spec.control_socket = rime_agent_core::paths::control_socket_in(runtime_dir);
    }

    spec.env_set = vec![
        ("HOME".to_string(), server_home.to_string_lossy().into_owned()),
        (
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_else(|_| rime_paths::MINIMAL_PATH.to_string()),
        ),
        // Pointed inside the private home, so a toolchain that writes to an
        // XDG directory writes to one that exists and is the server's own.
        (
            "XDG_CACHE_HOME".to_string(),
            server_home.join("cache").to_string_lossy().into_owned(),
        ),
        (
            "XDG_CONFIG_HOME".to_string(),
            server_home.join("config").to_string_lossy().into_owned(),
        ),
        (
            "XDG_DATA_HOME".to_string(),
            server_home.join("data").to_string_lossy().into_owned(),
        ),
        (
            "XDG_STATE_HOME".to_string(),
            server_home.join("state").to_string_lossy().into_owned(),
        ),
    ];
    spec.env_pass = policy.env.clone();
    spec
}

/// `rime mcp run <name> -- <program> [args…]`.
pub fn run(name: &str, command: &[String]) -> Result<i32> {
    let Some((program, args)) = command.split_first() else {
        bail!(
            "nothing to run. This wraps the server's own command:\n  \
             rime mcp run {name} -- npx -y @modelcontextprotocol/server-memory"
        );
    };
    let policy = load(name)?;
    let home = private_home(name);
    std::fs::create_dir_all(&home)
        .with_context(|| format!("creating {}", home.display()))?;
    restrict(&home)?;

    // Fails closed: a machine without bubblewrap does not get an unconfined MCP
    // server, it gets an error. The session sandbox makes the same choice, and
    // for a sidecar the stakes are the same.
    sandbox::preflight(policy.sandbox_policy())
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("this MCP server cannot be confined")?;

    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_default();
    let project = std::env::current_dir()
        .ok()
        .and_then(|cwd| rime_agent_core::project::detect(&cwd))
        .map(|p| PathBuf::from(p.root));
    if policy.project && project.is_none() {
        // Said rather than silently dropped: a policy that asked for the
        // project and did not get it is a server that will fail to find its
        // files, and the reason would otherwise be invisible.
        eprintln!(
            "rime mcp run: '{name}' asks for the project and this directory is not inside one"
        );
    }

    let mut spec = build(&policy, &super::home(), &home, &runtime_dir, project.as_deref());
    // The runtime resolves a spec's paths before handing it over, because bwrap
    // refuses to mount on a path that traverses a symlink and an atomic OS
    // reaches every home through one (`/home -> var/home`).
    spec.home = sandbox::real_target(&spec.home);
    spec.rw = spec.rw.iter().map(|p| sandbox::real_target(p)).collect();
    spec.ro = spec.ro.iter().map(|p| sandbox::real_target(p)).collect();
    spec.cwd = sandbox::real_target(&spec.cwd);
    // The same resolution `rime-agentd` applies to a session's copy of this
    // path, and for the same reason: an empty one must stay empty, because
    // `build_argv` reads that as "no socket" and a resolved "" would become a
    // bind of the current directory.
    if !spec.control_socket.as_os_str().is_empty() {
        spec.control_socket = sandbox::real_target(&spec.control_socket);
    }

    let argv = sandbox::build_argv(&spec, program, args).map_err(|e| anyhow::anyhow!("{e}"))?;
    exec(&argv)
}

impl McpPolicy {
    /// The sandbox dimension this policy runs at. Always confined: an MCP
    /// server that ran unconfined would be the thing P1-019 exists to stop.
    pub fn sandbox_policy(&self) -> SandboxPolicy {
        SandboxPolicy::Project
    }
}

/// `0700` on the private home, so another account on the machine cannot read
/// what a server writes there.
fn restrict(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("restricting {}", dir.display()))
}

/// Replace this process with the confined server.
///
/// `exec` rather than spawn-and-wait: stdin and stdout are the MCP transport
/// and every process in the middle is one more thing that can buffer, truncate
/// or outlive the conversation.
fn exec(argv: &[String]) -> Result<i32> {
    use std::os::unix::process::CommandExt;
    let Some((program, args)) = argv.split_first() else {
        bail!("nothing to execute");
    };
    let e = std::process::Command::new(program).args(args).exec();
    Err(anyhow::Error::new(e).context(format!("running {program}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_for(policy: &McpPolicy) -> Vec<String> {
        let spec = build(
            policy,
            Path::new("/home/tester"),
            Path::new("/home/tester/.local/state/rime/agent/mcp/memory"),
            Path::new("/run/user/1000"),
            Some(Path::new("/home/tester/Projects/demo")),
        );
        sandbox::build_argv(&spec, "npx", &["-y".into(), "server-memory".into()])
            .expect("an argv")
    }

    fn has_pair(argv: &[String], flag: &str, value: &str) -> bool {
        argv.windows(2).any(|w| w[0] == flag && w[1] == value)
    }

    #[test]
    fn the_default_is_the_safe_one_in_every_dimension() {
        // P1-019's second criterion. Nothing here comes from a policy file:
        // this is what a server whose owner has written nothing gets.
        let argv = spec_for(&McpPolicy::closed("memory"));
        assert!(argv.contains(&"--unshare-net".to_string()), "{argv:?}");
        assert!(argv.contains(&"--clearenv".to_string()), "{argv:?}");
        // The agent's home, the project and the runtime directory are all
        // masked or simply not bound.
        assert!(has_pair(&argv, "--tmpfs", "/run"), "{argv:?}");
        assert!(has_pair(&argv, "--tmpfs", "/run/user/1000"), "{argv:?}");
        assert!(
            !argv.iter().any(|a| a.contains("/home/tester/Projects/demo")),
            "the project reached a server that did not ask for it: {argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a.contains("rime-secretd")),
            "the broker's socket reached a server that did not ask for it: {argv:?}"
        );
        // The account's home is masked, and the only thing bound back inside it
        // is this server's own directory.
        assert!(has_pair(&argv, "--tmpfs", "/home/tester"), "{argv:?}");
        assert!(
            has_pair(
                &argv,
                "--bind-try",
                "/home/tester/.local/state/rime/agent/mcp/memory"
            ),
            "{argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a == "/home/tester/.claude"
                || a == "/home/tester/.ssh"
                || a == "/home/tester/.claude.json"),
            "the agent's own profile reached the server: {argv:?}"
        );
        assert!(
            has_pair(&argv, "--setenv", "HOME")
                && argv.contains(&"/home/tester/.local/state/rime/agent/mcp/memory".to_string()),
            "{argv:?}"
        );
    }

    #[test]
    fn a_server_that_asks_for_the_network_gets_it_and_one_that_does_not_cannot() {
        let mut open = McpPolicy::closed("remote");
        open.network = true;
        let argv = spec_for(&open);
        assert!(
            !argv.contains(&"--unshare-net".to_string()),
            "a declared network was still removed: {argv:?}"
        );
        // And the closed one, so this test fails if the default ever inverts.
        assert!(spec_for(&McpPolicy::closed("remote")).contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn every_widening_has_to_be_asked_for_by_name() {
        // Each dimension on its own, so a test cannot pass because some other
        // field happened to bind the same path.
        let mut project = McpPolicy::closed("m");
        project.project = true;
        assert!(has_pair(&spec_for(&project), "--bind-try", "/home/tester/Projects/demo"));

        let mut broker = McpPolicy::closed("m");
        broker.broker = true;
        let argv = spec_for(&broker);
        // The agent runtime's socket, under the runtime directory `spec_for`
        // passes in — the way into the broker for anything confined. Not the
        // secret daemon's own socket: see the next test.
        assert!(
            has_pair(&argv, "--bind-try", "/run/user/1000/rime-agentd/control.sock"),
            "{argv:?}"
        );

        let mut readable = McpPolicy::closed("m");
        readable.read = vec![PathBuf::from("/usr/share/dict")];
        assert!(has_pair(&spec_for(&readable), "--ro-bind-try", "/usr/share/dict"));

        let mut writable = McpPolicy::closed("m");
        writable.write = vec![PathBuf::from("/home/tester/Notes")];
        assert!(has_pair(&spec_for(&writable), "--bind-try", "/home/tester/Notes"));
    }

    #[test]
    fn the_broker_dimension_opens_the_door_a_session_uses_and_no_other() {
        // The defect this pins. `broker = true` first bound
        // `rime_secret_core::paths::socket()`, and the two halves were both
        // wrong:
        //
        // * `rime secret use` and `rime mcp bridge` connect to **rime-agentd**,
        //   whose socket is under `$XDG_RUNTIME_DIR` and is masked by the
        //   tmpfs — so the one thing `broker = true` promises did not work, and
        //   `--bind-try` made the wrong path fail without a word.
        // * `rime-secretd`'s socket is one a confined agent session is never
        //   given. Binding it handed an MCP server a door into the secret
        //   daemon that the agent starting the server does not have, which is
        //   the inverse of a per-server sandbox.
        let mut broker = McpPolicy::closed("m");
        broker.broker = true;
        let argv = spec_for(&broker);
        let agentd = "/run/user/1000/rime-agentd/control.sock";
        assert!(has_pair(&argv, "--bind-try", agentd), "{argv:?}");
        assert!(
            !argv.iter().any(|a| a.contains("rime-secretd")),
            "the secret daemon's own socket reached a confined server: {argv:?}"
        );
        // And it is the `broker` flag that does it, not something else in the
        // spec: without the flag neither socket is there.
        let closed = spec_for(&McpPolicy::closed("m"));
        assert!(!closed.iter().any(|a| a.contains("rime-agentd")), "{closed:?}");
        assert!(!closed.iter().any(|a| a.contains("rime-secretd")), "{closed:?}");
    }

    #[test]
    fn the_broker_socket_is_the_one_the_session_builder_uses() {
        // Two callers, one path function: `rime-agentd` binds
        // `paths::control_socket()` into a session and this binds
        // `control_socket_in` under the runtime directory it was handed. A
        // test that spelled the path out twice would keep passing if either
        // moved.
        assert_eq!(
            rime_agent_core::paths::control_socket_in(Path::new("/run/user/1000")),
            PathBuf::from("/run/user/1000/rime-agentd/control.sock")
        );
        assert!(rime_agent_core::paths::control_socket()
            .ends_with("rime-agentd/control.sock"));
    }

    #[test]
    fn the_environment_is_the_ones_named_and_nothing_else() {
        // `--clearenv` and then an explicit set, so a token in the agent's
        // environment does not arrive in a server that never asked for it.
        //
        // Asserted on the spec rather than by setting a variable in this
        // process: the test runner is threaded, `std::env::set_var` is
        // process-wide, and a suite that mutated the environment would make
        // every other test's reading of it depend on the order they ran in.
        // That is not a hypothetical — it turned up as one failure in a
        // workspace run that passed when the crate was tested on its own.
        // The live test in `tests/mcp_sidecar_live.rs` sets the variable on the
        // child instead, which is where it belongs, and measures that a real
        // bwrap drops it.
        let mut policy = McpPolicy::closed("m");
        policy.env = vec!["NODE_OPTIONS".to_string()];
        let spec = build(
            &policy,
            Path::new("/home/tester"),
            Path::new("/home/tester/mcp/m"),
            Path::new("/run/user/1000"),
            None,
        );
        assert_eq!(spec.env_pass, vec!["NODE_OPTIONS".to_string()]);
        // And nothing else is offered: the passthrough list is the policy's,
        // not the policy's plus whatever a default added.
        assert!(McpPolicy::closed("m").env.is_empty());

        let argv = spec_for(&policy);
        assert!(argv.contains(&"--clearenv".to_string()), "{argv:?}");
        assert!(has_pair(&argv, "--setenv", "HOME"), "{argv:?}");
        assert!(has_pair(&argv, "--setenv", "PATH"), "{argv:?}");
        // The XDG directories point inside the server's own home, so a
        // toolchain that writes to one writes somewhere that exists.
        for name in ["XDG_CACHE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"] {
            assert!(has_pair(&argv, "--setenv", name), "{name} missing: {argv:?}");
        }
        assert!(
            !argv.iter().any(|a| a.starts_with("/home/tester/.config/")
                || a.starts_with("/home/tester/.cache/")),
            "an XDG directory still pointed at the account's own: {argv:?}"
        );
    }

    #[test]
    fn a_setting_nobody_declared_is_refused_rather_than_ignored() {
        // The failure this closes: `netwrok = true` in a file, a server started
        // with no network, and a policy that reads as if it had been applied.
        let e = parse("m", "netwrok = true\n", Source::Default)
            .expect_err("a typo is not a setting")
            .to_string();
        assert!(e.contains("netwrok"), "{e}");
        assert!(e.contains("network, project, broker, read, write, env"), "{e}");

        for wrong in [
            "network = \"yes\"",
            "read = \"/usr\"",
            "write = [7]",
            "env = [\"NOT A NAME\"]",
            "read = [\"relative/path\"]",
        ] {
            assert!(parse("m", wrong, Source::Default).is_err(), "{wrong}");
        }
    }

    #[test]
    fn a_policy_file_only_ever_widens_and_an_empty_one_changes_nothing() {
        let closed = McpPolicy::closed("m");
        let empty = parse("m", "", Source::Default).expect("an empty policy");
        assert_eq!(empty, closed);

        let full = parse(
            "m",
            "network = true\nproject = true\nbroker = true\n\
             read = [\"/usr/share/dict\"]\nwrite = [\"/tmp/notes\"]\nenv = [\"NODE_OPTIONS\"]\n",
            Source::Default,
        )
        .expect("a full policy");
        assert!(full.network && full.project && full.broker);
        assert_eq!(full.read, vec![PathBuf::from("/usr/share/dict")]);
        assert_eq!(full.write, vec![PathBuf::from("/tmp/notes")]);
        assert_eq!(full.env, vec!["NODE_OPTIONS".to_string()]);
    }

    #[test]
    fn an_unreadable_policy_is_an_error_and_never_a_quiet_default() {
        // The defect class this codebase has already been through: a refused
        // read reported as an absence. Here the absence is *tighter* than the
        // file, so falling back would break the server and blame the server.
        let dir = std::env::temp_dir().join(format!("rime-mcp-policy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("broken.toml");
        std::fs::write(&file, "network = = true").expect("write");
        let e = parse("broken", "network = = true", Source::File(file.clone()))
            .expect_err("not TOML")
            .to_string();
        assert!(e.contains("not a TOML document"), "{e}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn every_server_gets_a_home_of_its_own() {
        // Two servers sharing a home is two servers reading each other's
        // working files, which is the thing per-MCP confinement is for.
        assert_ne!(private_home("memory"), private_home("other"));
        assert!(private_home("memory").ends_with("mcp/memory"));
    }

    #[test]
    fn the_policy_reads_as_a_sentence_a_person_can_check() {
        let lines = McpPolicy::closed("m").describe().join("\n");
        assert!(lines.contains("none — its own empty namespace"), "{lines}");
        assert!(lines.contains("cannot reach rime-agentd or rime-secretd"), "{lines}");
        let mut open = McpPolicy::closed("m");
        open.network = true;
        open.broker = true;
        let lines = open.describe().join("\n");
        assert!(lines.contains("ceiling and not a grant"), "{lines}");
        // Named where a request actually goes: `rime secret use` and `rime mcp
        // bridge` both talk to rime-agentd, which stamps the session and
        // forwards. A line that promised rime-secretd would be describing a
        // socket the sandbox does not bind.
        assert!(lines.contains("may reach rime-agentd"), "{lines}");
        assert!(lines.contains("this project's grants decide"), "{lines}");
    }
}
