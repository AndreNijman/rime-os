//! Agent adapters.
//!
//! An adapter is a small description of one upstream CLI: what binary to run,
//! how it takes a prompt, which of its own files it needs when the sandbox has
//! masked the home directory, and which environment variables carry its
//! credential. It is deliberately *not* a wrapper — the runtime launches the
//! real `claude`, `opencode`, `codex` or `gemini` binary and gets out of the
//! way, exactly as the roadmap's non-negotiable rules require.
//!
//! ## Why the home allowlist is per-adapter
//!
//! The sandbox masks `$HOME`. Everything an agent needs from it therefore has
//! to be named. That includes things it is easy to forget: `opencode` and
//! `codex` install into `~/.local/bin` as symlinks into
//! `~/.local/lib/node_modules`, so without both entries the sandbox masks the
//! binary the session is trying to run, or the package it points at.
//!
//! ## State detection
//!
//! No adapter parses terminal output for meaning. Detection is the generic
//! path in [`crate::session`] — bell, OSC notifications, prompt markers, idle
//! and exit status — plus whatever the agent publishes through
//! `rime agent event`. Recognising a permission prompt by pattern-matching a
//! TUI's output would break the first time upstream changed a string, and
//! would report the wrong thing rather than nothing.
//!
//! ## The agent's own permission mode
//!
//! Dimension 1 of §3.1, and the only dimension that is a property of the
//! upstream CLI rather than of Rime. Each adapter declares the arguments that
//! select it, because every one of these tools spells it differently, and an
//! adapter that cannot express a mode says so instead of silently ignoring it:
//! a `--agent-bypass` that quietly did nothing would leave the user believing
//! confirmations were off while the agent kept asking, or the reverse.
//!
//! The arguments below were read from the installed binaries' `--help`, not
//! from memory. `codex` deliberately gets `-a never` rather than
//! `--dangerously-bypass-approvals-and-sandbox`: that flag also removes
//! *codex's own* sandbox, and dimension 1 is the approval policy. Removing a
//! confinement layer nobody asked about is the exact collapse §3.1 forbids.

use std::path::PathBuf;

use crate::policy::NativeMode;
use crate::sandbox::SandboxSpec;

/// Toolchain state shared by every adapter, relative to `$HOME`.
///
/// Writable, because a build that cannot populate its cache is a build that
/// fails: `cargo` writes to `~/.cargo/registry`, `npm` to `~/.npm`, `go` to
/// its module cache.
const TOOLCHAIN_RW: &[&str] = &[
    ".cargo",
    ".rustup",
    ".npm",
    ".cache/pip",
    ".cache/uv",
    ".cache/go-build",
    ".local/share/pnpm",
    ".local/share/uv",
    ".local/share/virtualenvs",
    "go/pkg/mod",
];

/// Read-only home state every adapter gets: the tools themselves and the
/// configuration a build reads but must not rewrite.
///
/// `.local/bin` and `.local/lib/node_modules` are one fact, not two. An npm
/// install under `~/.local` puts the package in `lib/node_modules/<pkg>` and
/// leaves `bin/<name>` as a symlink into it, so binding the bin directory alone
/// binds a link with nothing on the far side: `bwrap: execvp opencode: No such
/// file or directory`, before the agent has run a line. `claude` once escaped
/// that only because the image shipped a root-owned `/usr/bin/claude` that
/// `--ro-bind / /` covered — which also meant a confined `claude` session ran
/// the image's copy rather than the user's, contradicting the promise that a
/// user's own build wins. Since 2026-10-04 the image ships none, and
/// `rime install claude-code` puts it under `~/.local` like every other npm
/// agent, so `claude` needs this binding exactly as `opencode` does.
///
/// The module directory and not `.local/lib`, which on an ordinary machine also
/// holds `python3.N/site-packages` and whatever else a user has installed
/// there. Read-only in either case, but the sandbox is default-deny and the
/// module tree is the whole of what a symlinked CLI needs.
const TOOLCHAIN_RO: &[&str] = &[
    ".local/bin",
    ".local/lib/node_modules",
    ".gitconfig",
    ".config/git",
    ".local/share/mise",
    ".asdf",
    // The per-MCP sandbox policies (§10.2). `rime mcp run` reads one inside the
    // session, because the agent is what starts an MCP server — and without
    // this the file is behind the home mask, so every server would silently get
    // the default and a policy the user wrote would do nothing.
    //
    // Read-only, and that is the point rather than an accident: a session that
    // could write here could widen the confinement of every MCP server it
    // starts, in one file, without touching a definition anybody would look at.
    ".config/rime/mcp",
];

/// Credential files that live inside an allowlisted directory and are blanked
/// out again afterwards. Relative to `$HOME`.
///
/// These are the cases default-deny cannot reach on its own: a toolchain cache
/// the agent genuinely needs that happens to store a token beside it.
const CREDENTIAL_MASKS: &[&str] = &[
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".npmrc",
    ".config/git/credentials",
];

/// One upstream agent CLI.
#[derive(Debug, Clone)]
pub struct Adapter {
    /// Stable short name used by `rime agent run <id>` and stored in session
    /// records.
    pub id: &'static str,
    /// Human-facing name.
    pub display: &'static str,
    /// The binary to execute. Resolved through `PATH` at spawn time, so a
    /// user's own build of an agent still wins.
    pub program: &'static str,
    /// Home-relative paths this agent needs to write (its own session store).
    pub home_rw: &'static [&'static str],
    /// Home-relative paths this agent needs to read.
    pub home_ro: &'static [&'static str],
    /// Environment variables carrying this agent's credentials or endpoint
    /// configuration, inherited when present. Nothing else is inherited.
    pub env_pass: &'static [&'static str],
    /// Arguments that put this agent into [`NativeMode::Bypass`]. Empty when
    /// its CLI has no such control.
    pub native_bypass: &'static [&'static str],
    /// Arguments that put this agent into [`NativeMode::Ask`]. Empty when its
    /// CLI has no such control, which for most of them means asking is already
    /// the default and there is nothing to select.
    pub native_ask: &'static [&'static str],
    /// Whether upstream itself refuses its bypass mode when the process is
    /// root.
    ///
    /// Claude does — measured: it exits with "cannot be used with root/sudo
    /// privileges for security reasons". The runtime supports running as root,
    /// so without this the user would get an unexplained upstream error
    /// instead of a refusal that says which of the two rules stopped them.
    pub native_bypass_refused_as_root: bool,
    /// Whether this agent publishes a hook lifecycle Rime can subscribe to
    /// (§6.1), and can be pointed at a settings file that does the subscribing.
    ///
    /// One adapter today. It is a field rather than an `id == "claude"` test
    /// because the next agent to grow hooks should be one line here, and
    /// because the sandbox and argv code that reads it should not have to know
    /// which agent it is looking at.
    pub hooks: bool,
    /// Whether this agent can be told to use one MCP configuration and ignore
    /// every other (§10.2, P1-026/P1-028), and can therefore be handed the
    /// curated document [`crate::mcpconf`] builds.
    ///
    /// One adapter today, for the same reason `hooks` is a field: the flag was
    /// read from the installed binary's `--help` and then measured — with
    /// `--strict-mcp-config`, a sentinel server defined by an enabled plugin
    /// does not start, and only the ones in the handed file do. An adapter
    /// where that has not been measured gets `false`, and a session on it is
    /// reported as having whatever connectors its own configuration gives it
    /// rather than as curated.
    pub strict_mcp: bool,
}

/// Every adapter the runtime knows, in listing order.
///
/// `generic` is last and is the fallback for any binary without a specific
/// entry — the roadmap's `GenericPTYAdapter`. It is what makes "never require
/// a specific agent" true rather than aspirational.
pub const ADAPTERS: &[Adapter] = &[
    Adapter {
        id: "claude",
        display: "Claude Code",
        program: "claude",
        // Empty because `crate::profile` describes this one path by path: its
        // instructions, skills and commands go in read-only and its session and
        // plugin state goes in writable. `.claude` here would bind the profile
        // whole and writable, which is what P0-010 replaced.
        home_rw: &[],
        home_ro: &[],
        env_pass: &[
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_MODEL",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
        ],
        native_bypass: &["--permission-mode", "bypassPermissions"],
        native_ask: &["--permission-mode", "manual"],
        native_bypass_refused_as_root: true,
        hooks: true,
        strict_mcp: true,
    },
    Adapter {
        id: "opencode",
        display: "OpenCode",
        program: "opencode",
        home_rw: &[".local/share/opencode", ".config/opencode", ".cache/opencode"],
        home_ro: &[],
        env_pass: &[
            "OPENCODE_API_KEY",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "OPENROUTER_API_KEY",
        ],
        native_bypass: &["--auto"],
        native_ask: &[],
        native_bypass_refused_as_root: false,
        hooks: false,
        strict_mcp: false,
    },
    Adapter {
        id: "codex",
        display: "Codex CLI",
        program: "codex",
        home_rw: &[".codex"],
        home_ro: &[],
        env_pass: &["OPENAI_API_KEY", "OPENAI_BASE_URL", "CODEX_API_KEY"],
        // Not `--dangerously-bypass-approvals-and-sandbox`: that also removes
        // codex's own sandbox, and dimension 1 is the approval policy alone.
        native_bypass: &["-a", "never"],
        native_ask: &["-a", "on-request"],
        native_bypass_refused_as_root: false,
        hooks: false,
        strict_mcp: false,
    },
    Adapter {
        id: "gemini",
        display: "Gemini CLI",
        program: "gemini",
        home_rw: &[".gemini", ".config/google-generativeai"],
        home_ro: &[],
        env_pass: &["GEMINI_API_KEY", "GOOGLE_API_KEY", "GOOGLE_GENAI_USE_VERTEXAI"],
        // Left empty rather than guessed: the binary was not installed on the
        // machine the others were read from, and a wrong flag here is a
        // session that will not start.
        native_bypass: &[],
        native_ask: &[],
        native_bypass_refused_as_root: false,
        hooks: false,
        strict_mcp: false,
    },
    Adapter {
        id: "kimi",
        display: "Kimi CLI",
        program: "kimi",
        home_rw: &[".kimi", ".config/kimi"],
        home_ro: &[],
        env_pass: &["KIMI_API_KEY", "MOONSHOT_API_KEY"],
        // `--auto`, not `-y/--yolo`: yolo still lets the agent ask questions.
        native_bypass: &["--auto"],
        native_ask: &[],
        native_bypass_refused_as_root: false,
        hooks: false,
        strict_mcp: false,
    },
    Adapter {
        id: "generic",
        display: "Generic PTY",
        program: "",
        home_rw: &[],
        home_ro: &[],
        env_pass: &[],
        native_bypass: &[],
        native_ask: &[],
        native_bypass_refused_as_root: false,
        hooks: false,
        strict_mcp: false,
    },
];

/// The adapter used when the user has expressed no preference.
pub const DEFAULT_AGENT: &str = "claude";

/// Look an adapter up by id.
pub fn by_id(id: &str) -> Option<&'static Adapter> {
    ADAPTERS.iter().find(|a| a.id == id)
}

/// The `generic` adapter, which is guaranteed to exist.
pub fn generic() -> &'static Adapter {
    by_id("generic").expect("the generic adapter is compiled in")
}

/// Recognise an already-running program as one of the known agents, for
/// sessions the runtime adopts rather than starts.
pub fn by_program(program: &str) -> &'static Adapter {
    let base = program.rsplit('/').next().unwrap_or(program);
    ADAPTERS
        .iter()
        .find(|a| !a.program.is_empty() && a.program == base)
        .unwrap_or_else(generic)
}

/// Every adapter id, for `--help` text and shell completion.
pub fn ids() -> Vec<&'static str> {
    ADAPTERS.iter().map(|a| a.id).collect()
}

impl Adapter {
    /// The program this adapter runs. For `generic` the caller supplies it,
    /// since the whole point is that it launches anything.
    pub fn resolve_program(&self, explicit: Option<&str>) -> Option<String> {
        match explicit {
            Some(p) if !p.is_empty() => Some(p.to_string()),
            _ if !self.program.is_empty() => Some(self.program.to_string()),
            _ => None,
        }
    }

    /// The arguments that select `mode` for this agent, or `None` when this
    /// adapter has no way to express it.
    ///
    /// [`NativeMode::Inherit`] is always expressible and is always the empty
    /// list: §4.1 says Rime passes nothing and lets the agent's own profile
    /// decide, so "inherit" is the absence of a flag rather than a flag.
    pub fn native_mode_args(&self, mode: NativeMode) -> Option<Vec<String>> {
        let flags = match mode {
            NativeMode::Inherit => return Some(Vec::new()),
            NativeMode::Bypass => self.native_bypass,
            NativeMode::Ask => self.native_ask,
        };
        if flags.is_empty() {
            return None;
        }
        Some(flags.iter().map(|s| s.to_string()).collect())
    }

    /// Why this adapter will not run in `mode`, or `None` when it will.
    ///
    /// A pure function of the adapter, the mode and whether the runtime is
    /// root, so the refusal is testable without spawning anything.
    pub fn refuses_native_mode(&self, mode: NativeMode, as_root: bool) -> Option<String> {
        if self.native_mode_args(mode).is_none() {
            return Some(format!(
                "{} has no flag for the {mode} permission mode, so Rime cannot select it; \
                 run with `--native inherit` and set it in the agent's own configuration",
                self.display
            ));
        }
        if as_root && mode == NativeMode::Bypass && self.native_bypass_refused_as_root {
            return Some(format!(
                "{} refuses its bypass permission mode when it runs as root, so the session \
                 would exit immediately; run the agent as your own user, or use \
                 `--native inherit`",
                self.display
            ));
        }
        None
    }

    /// Build the argument list.
    ///
    /// A prompt is passed as a single trailing positional argument, which is
    /// the form every one of these CLIs accepts for an opening instruction.
    /// No flags are invented beyond the permission mode the policy asked for,
    /// and that one comes first so anything in `extra` — which the user
    /// controls — still overrides it.
    pub fn build_args(
        &self,
        native: NativeMode,
        prompt: Option<&str>,
        extra: &[String],
    ) -> Vec<String> {
        let mut args: Vec<String> = self.native_mode_args(native).unwrap_or_default();
        args.extend(extra.iter().cloned());
        if let Some(p) = prompt {
            if !p.is_empty() {
                args.push(p.to_string());
            }
        }
        args
    }

    /// The agent's own profile, when Rime has a description of one.
    ///
    /// An adapter without one keeps the whole-directory `home_rw` behaviour:
    /// `codex` and `gemini` have not been read off a real installation, and
    /// guessing which half of `~/.codex` is a session store would produce
    /// exactly the failure P0-010 exists to avoid.
    pub fn profile(&self) -> Option<&'static crate::profile::Profile> {
        crate::profile::by_agent(self.id)
    }

    /// The arguments that point this agent at Rime's hook subscriptions.
    ///
    /// Empty for an adapter that has no hooks, so a caller can append the
    /// result unconditionally. `--settings` *adds* a settings source rather
    /// than replacing the user's — measured against the installed binary's
    /// `--help` — so Andre's own hooks keep running beside Rime's.
    pub fn hook_settings_args(&self, settings: &std::path::Path) -> Vec<String> {
        if !self.hooks {
            return Vec::new();
        }
        vec![
            "--settings".to_string(),
            settings.to_string_lossy().into_owned(),
        ]
    }

    /// The arguments that point this agent at a curated MCP configuration and
    /// nothing else.
    ///
    /// Empty for an adapter that cannot be told, so a caller can append the
    /// result unconditionally — and empty is the honest answer there rather
    /// than a `--mcp-config` on its own, which for Claude *adds* a source
    /// instead of replacing them. The two flags travel together for exactly
    /// that reason: `--mcp-config` alone would leave every other surface in
    /// place and the session would get the plugin's unwrapped definition
    /// beside Rime's wrapped one.
    pub fn mcp_config_args(&self, config: &std::path::Path) -> Vec<String> {
        if !self.strict_mcp {
            return Vec::new();
        }
        vec![
            "--strict-mcp-config".to_string(),
            "--mcp-config".to_string(),
            config.to_string_lossy().into_owned(),
        ]
    }

    /// Add this adapter's home requirements, the shared toolchain state and the
    /// credential masks to a sandbox spec.
    ///
    /// ## The profile goes in path by path, not as a directory (P0-010)
    ///
    /// Binding `~/.claude` writable hands a confined session its own
    /// instructions, skills, commands and slash commands to rewrite — and a
    /// session that can edit `CLAUDE.md` can edit what the next session is told
    /// to do. The profile table already says which of the directory is
    /// reusable, so the mounts come from that one table:
    /// [`crate::profile::Profile::mounts`] returns the read-only paths and the
    /// writable ones, and the disjointness is asserted there rather than
    /// re-decided here.
    ///
    /// What is left is the profile root itself, which nothing binds. `$HOME` is
    /// a tmpfs and `bwrap` creates the mount points it needs, so `~/.claude`
    /// exists inside the session as an empty writable directory with the listed
    /// entries mounted into it. That is the runtime overlay: a file Claude
    /// invents there — a log, a directory a later release adds — is writable,
    /// is private to the session, and is gone when the session ends. Nothing
    /// unlisted reaches the real profile, and nothing unlisted fails to write.
    pub fn apply_sandbox(&self, spec: &mut SandboxSpec) {
        let home = spec.home.clone();
        let join = |rel: &str| -> PathBuf { home.join(rel) };

        for rel in TOOLCHAIN_RW.iter().chain(self.home_rw.iter()) {
            let p = join(rel);
            if !spec.rw.contains(&p) {
                spec.rw.push(p);
            }
        }
        for rel in TOOLCHAIN_RO.iter().chain(self.home_ro.iter()) {
            let p = join(rel);
            if !spec.ro.contains(&p) {
                spec.ro.push(p);
            }
        }
        if let Some(profile) = self.profile() {
            let (ro, rw) = profile.mounts();
            for rel in &ro {
                let p = join(rel);
                if !spec.ro.contains(&p) {
                    spec.ro.push(p);
                }
            }
            for rel in &rw {
                let p = join(rel);
                if !spec.rw.contains(&p) {
                    spec.rw.push(p);
                }
            }
        }
        for rel in CREDENTIAL_MASKS {
            let p = join(rel);
            if !spec.mask.contains(&p) {
                spec.mask.push(p);
            }
        }
        for name in self.env_pass {
            let name = name.to_string();
            if !spec.env_pass.contains(&name) {
                spec.env_pass.push(name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> SandboxSpec {
        SandboxSpec::new(
            crate::policy::AgentPolicy::default(),
            PathBuf::from("/home/tester"),
            PathBuf::from("/run/user/1000"),
        )
    }

    #[test]
    fn every_adapter_id_is_unique() {
        let mut ids = ids();
        let count = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate adapter id");
    }

    #[test]
    fn the_default_agent_exists() {
        assert!(by_id(DEFAULT_AGENT).is_some());
    }

    #[test]
    fn the_generic_adapter_exists_and_has_no_program_of_its_own() {
        let g = generic();
        assert_eq!(g.id, "generic");
        assert!(g.program.is_empty());
        assert_eq!(g.resolve_program(None), None);
        assert_eq!(g.resolve_program(Some("htop")), Some("htop".to_string()));
    }

    #[test]
    fn an_unknown_program_falls_back_to_generic() {
        assert_eq!(by_program("some-new-agent").id, "generic");
        assert_eq!(by_program("").id, "generic");
    }

    #[test]
    fn a_known_program_is_recognised_through_an_absolute_path() {
        assert_eq!(by_program("/usr/bin/claude").id, "claude");
        assert_eq!(by_program("/home/andre/.local/bin/codex").id, "codex");
        assert_eq!(by_program("opencode").id, "opencode");
    }

    #[test]
    fn a_prompt_becomes_the_last_positional_argument() {
        let a = by_id("claude").unwrap();
        let inherit = NativeMode::Inherit;
        assert_eq!(
            a.build_args(inherit, Some("fix the tests"), &[]),
            ["fix the tests"]
        );
        assert_eq!(
            a.build_args(inherit, Some("go"), &["--verbose".to_string()]),
            ["--verbose", "go"]
        );
        assert!(a.build_args(inherit, None, &[]).is_empty());
        // An empty prompt is not an argument.
        assert!(a.build_args(inherit, Some(""), &[]).is_empty());
    }

    #[test]
    fn inherit_passes_no_permission_flag_at_all() {
        // §4.1: "If Claude's profile defaults to bypassPermissions, Rime should
        // not override it." The only way to not override a setting is to say
        // nothing about it, so inherit must add no argument for any adapter.
        for a in ADAPTERS {
            assert_eq!(
                a.native_mode_args(NativeMode::Inherit),
                Some(Vec::new()),
                "{} added an argument for inherit",
                a.id
            );
            assert!(a.build_args(NativeMode::Inherit, None, &[]).is_empty(), "{}", a.id);
        }
    }

    #[test]
    fn the_permission_mode_comes_first_so_the_user_can_still_override_it() {
        let a = by_id("claude").unwrap();
        let args = a.build_args(
            NativeMode::Bypass,
            Some("go"),
            &["--permission-mode".to_string(), "plan".to_string()],
        );
        assert_eq!(
            args,
            [
                "--permission-mode",
                "bypassPermissions",
                "--permission-mode",
                "plan",
                "go"
            ]
        );
    }

    #[test]
    fn an_adapter_that_cannot_express_a_mode_refuses_it_instead_of_ignoring_it() {
        // Silently dropping the flag would leave the user believing
        // confirmations were off while the agent kept asking.
        let gemini = by_id("gemini").unwrap();
        assert_eq!(gemini.native_mode_args(NativeMode::Bypass), None);
        let why = gemini
            .refuses_native_mode(NativeMode::Bypass, false)
            .expect("a refusal");
        assert!(why.contains("inherit"), "{why}");

        // opencode can bypass but has no flag for forcing prompts.
        let opencode = by_id("opencode").unwrap();
        assert!(opencode.refuses_native_mode(NativeMode::Bypass, false).is_none());
        assert!(opencode.refuses_native_mode(NativeMode::Ask, false).is_some());

        // And inherit is never refused, by anything, ever.
        for a in ADAPTERS {
            for as_root in [false, true] {
                assert!(
                    a.refuses_native_mode(NativeMode::Inherit, as_root).is_none(),
                    "{} refused inherit",
                    a.id
                );
            }
        }
    }

    #[test]
    fn claudes_own_refusal_of_bypass_as_root_is_reported_before_the_session_starts() {
        // Measured: `claude --permission-mode bypassPermissions` under sudo
        // prints "cannot be used with root/sudo privileges for security
        // reasons" and exits. The runtime supports root sessions, so the user
        // needs to be told which rule stopped them rather than watching a
        // session die on start.
        let claude = by_id("claude").unwrap();
        assert!(claude.refuses_native_mode(NativeMode::Bypass, false).is_none());
        let why = claude
            .refuses_native_mode(NativeMode::Bypass, true)
            .expect("a refusal as root");
        assert!(why.contains("root"), "{why}");
        // The restriction is upstream's and applies to bypass alone.
        assert!(claude.refuses_native_mode(NativeMode::Ask, true).is_none());
    }

    #[test]
    fn local_bin_is_allowed_or_agents_installed_there_cannot_run() {
        // opencode and codex install to ~/.local/bin. The home tmpfs would
        // otherwise mask the very binary the session is trying to execute.
        let mut s = spec();
        by_id("opencode").unwrap().apply_sandbox(&mut s);
        assert!(s.ro.contains(&PathBuf::from("/home/tester/.local/bin")));
    }

    #[test]
    fn a_symlinked_agent_reaches_the_package_its_bin_entry_points_at() {
        // `~/.local/bin/opencode` is a symlink into
        // `~/.local/lib/node_modules/opencode-ai/bin`. Binding only the bin
        // directory left the link dangling inside the home tmpfs, and bwrap
        // exited 1 with "execvp opencode: No such file or directory".
        use crate::sandbox::build_argv;

        let modules = "/home/tester/.local/lib/node_modules";
        for agent in ["opencode", "codex"] {
            let mut s = spec();
            s.cwd = PathBuf::from("/home/tester/p");
            by_id(agent).unwrap().apply_sandbox(&mut s);
            let argv = build_argv(&s, agent, &[]).unwrap();

            assert!(
                argv.windows(3)
                    .any(|w| w == ["--ro-bind-try", modules, modules]),
                "{agent} has no read-only bind of the module tree"
            );
            // Read-only and nothing else. A package tree an agent can rewrite
            // is a package tree it can backdoor for the next session.
            assert!(
                !argv
                    .windows(2)
                    .any(|w| w[0] == "--bind-try" && w[1] == modules),
                "{agent} got the module tree writable"
            );
            // And no wider than the module tree. ~/.local/lib also carries
            // python3.N/site-packages and whatever else the user put there,
            // none of which an agent needs in order to start.
            assert!(
                !argv.iter().any(|arg| arg == "/home/tester/.local/lib"),
                "{agent} widened the allowlist to the whole of ~/.local/lib"
            );
        }
    }

    #[test]
    fn the_reusable_half_of_a_profile_is_read_only_and_the_session_half_is_not() {
        // P0-010 criteria 1 and 2. Both come from the one profile table, so a
        // path cannot be exportable here and rewritable there.
        let mut s = spec();
        by_id("claude").unwrap().apply_sandbox(&mut s);
        for rel in [
            ".claude/CLAUDE.md",
            ".claude/settings.json",
            ".claude/skills",
            ".claude/commands",
            ".claude/agents",
            ".claude/plugins/known_marketplaces.json",
        ] {
            let p = PathBuf::from(format!("/home/tester/{rel}"));
            assert!(s.ro.contains(&p), "{rel} is not read-only");
            assert!(!s.rw.contains(&p), "{rel} is writable");
        }
        for rel in [
            ".claude/projects",
            ".claude/shell-snapshots",
            ".claude/todos",
            ".claude/plugins/cache",
            ".claude/plugins/data",
            ".claude/plugins/installed_plugins.json",
            ".claude.json",
        ] {
            let p = PathBuf::from(format!("/home/tester/{rel}"));
            assert!(s.rw.contains(&p), "{rel} is not writable");
            assert!(!s.ro.contains(&p), "{rel} is read-only");
        }
    }

    #[test]
    fn the_profile_directory_itself_is_never_bound() {
        // What makes the rest of it a runtime overlay: $HOME is a tmpfs and
        // bwrap creates the mount points, so ~/.claude is an empty writable
        // directory with the listed entries mounted into it. Anything Claude
        // invents there is writable, private to the session and gone with it.
        // Binding the directory would put all of that on the real profile.
        let mut s = spec();
        by_id("claude").unwrap().apply_sandbox(&mut s);
        let root = PathBuf::from("/home/tester/.claude");
        assert!(!s.rw.contains(&root), "the whole profile is writable");
        assert!(!s.ro.contains(&root), "the whole profile is bound");
    }

    #[test]
    fn an_adapter_without_a_profile_keeps_its_directory() {
        // codex and gemini have not been read off a real installation, and
        // guessing which half of ~/.codex is a session store would produce
        // exactly the failure P0-010 exists to avoid.
        let mut s = spec();
        by_id("codex").unwrap().apply_sandbox(&mut s);
        assert!(by_id("codex").unwrap().profile().is_none());
        assert!(s.rw.contains(&PathBuf::from("/home/tester/.codex")));
    }

    #[test]
    fn one_adapter_does_not_get_another_adapters_configuration() {
        let mut s = spec();
        by_id("claude").unwrap().apply_sandbox(&mut s);
        assert!(!s.rw.contains(&PathBuf::from("/home/tester/.codex")));
        assert!(!s.rw.contains(&PathBuf::from("/home/tester/.gemini")));
        assert!(!s.env_pass.contains(&"OPENAI_API_KEY".to_string()));
    }

    #[test]
    fn toolchain_caches_are_writable_because_a_build_writes_them() {
        let mut s = spec();
        generic().apply_sandbox(&mut s);
        for rel in [".cargo", ".npm", "go/pkg/mod"] {
            assert!(
                s.rw.contains(&PathBuf::from(format!("/home/tester/{rel}"))),
                "{rel} must be writable"
            );
        }
    }

    #[test]
    fn credential_files_inside_allowed_directories_are_masked() {
        let mut s = spec();
        by_id("claude").unwrap().apply_sandbox(&mut s);
        // ~/.cargo is writable for the registry cache, so the token beside it
        // has to be blanked explicitly.
        assert!(s.rw.contains(&PathBuf::from("/home/tester/.cargo")));
        assert!(s
            .mask
            .contains(&PathBuf::from("/home/tester/.cargo/credentials.toml")));
        assert!(s.mask.contains(&PathBuf::from("/home/tester/.npmrc")));
    }

    #[test]
    fn masks_survive_into_the_built_argv_after_the_binds() {
        use crate::sandbox::build_argv;
        let mut s = spec();
        s.cwd = PathBuf::from("/home/tester/p");
        by_id("claude").unwrap().apply_sandbox(&mut s);
        let argv = build_argv(&s, "claude", &[]).unwrap();

        let cargo_bind = argv
            .windows(3)
            .position(|w| w[0] == "--bind-try" && w[1] == "/home/tester/.cargo")
            .expect("cargo bind");
        let cred_mask = argv
            .windows(3)
            .position(|w| {
                w[0] == "--ro-bind-try"
                    && w[1] == "/dev/null"
                    && w[2] == "/home/tester/.cargo/credentials.toml"
            })
            .expect("credential mask");
        assert!(
            cargo_bind < cred_mask,
            "the mask must come after the bind that would otherwise expose it"
        );
    }

    #[test]
    fn no_adapter_declares_a_secret_that_is_not_its_own() {
        // A stray SSH_AUTH_SOCK or GITHUB_TOKEN in a passthrough list would
        // quietly hand every session a credential the sandbox is masking.
        for a in ADAPTERS {
            for name in a.env_pass {
                assert!(
                    !matches!(*name, "SSH_AUTH_SOCK" | "GPG_AGENT_INFO" | "GITHUB_TOKEN"),
                    "{} declares {name}",
                    a.id
                );
            }
        }
    }

    #[test]
    fn applying_the_same_adapter_twice_does_not_duplicate_entries() {
        let mut s = spec();
        let a = by_id("claude").unwrap();
        a.apply_sandbox(&mut s);
        let rw = s.rw.len();
        let masks = s.mask.len();
        let env = s.env_pass.len();
        a.apply_sandbox(&mut s);
        assert_eq!(s.rw.len(), rw);
        assert_eq!(s.mask.len(), masks);
        assert_eq!(s.env_pass.len(), env);
    }
}
