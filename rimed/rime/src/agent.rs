//! `rime agent` and `rime project` — the user-facing half of the agent runtime.
//!
//! Every verb here is a thin client over `rime-agentd`'s control socket. The
//! CLI never spawns an agent itself and never holds session state, so the
//! runtime remains the single owner of every PTY and `rime agent list` says the
//! same thing whether it is asked by the terminal, by a keybind or by Rime
//! Shell.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use rime_agent_core::client::{self, Client};
use rime_agent_core::grant::{GrantKind, SystemGrant};
use rime_agent_core::policy::{
    AgentPolicy, ConnectorPolicy, NativeMode, NetworkPolicy, OriginPolicy, PluginPolicy,
    PolicyPreset, RequestOrigin, SecretPolicy, SystemAccess,
};
use rime_agent_core::protocol::{
    AgentState, Request, Response, RunRequest, SandboxPolicy, SessionInfo, BROWSER_CA_VERSION,
    BROWSER_PRESENT_VERSION,
    CONNECTOR_POLICY_VERSION, PLUGIN_POLICY_VERSION, POLICY_DIMENSIONS_VERSION,
    REQUEST_ORIGIN_VERSION, SCOPED_GRANT_VERSION, SESSION_ALLOWLIST_VERSION,
    SYSTEM_GRANT_VERSION,
};
use rime_agent_core::hook::{self as hook_core, HookEvent};
use rime_agent_core::paths;
use rime_agent_core::statusline as statusline_core;
use rime_agent_core::term::{self, RawMode, WinSize};
use rime_agent_core::webauthn;
use rime_agent_core::worktree::{ConflictState, TestState};
use rime_agent_core::{adapter, checkpoint, config, git, handoff, layout, mux, profile, project};
use clap::{Args, Subcommand};

use crate::ops;

/// `rime agent <verb>`.
/// How much of the outgoing transcript a handoff packet carries by default.
///
/// Named rather than inline because `rime task handoff` delegates to this verb
/// and has to pass the same number: two spellings of one default drift, and the
/// symptom would be a packet with a different amount of evidence in it
/// depending on which of two commands the user typed.
pub(crate) const HANDOFF_TRANSCRIPT_BYTES: usize = 16 * 1024;

#[derive(Subcommand)]
pub enum AgentCmd {
    /// Start an agent on a managed terminal and attach to it.
    ///
    /// The real upstream binary runs in a real PTY; Rime owns the terminal so
    /// the session survives this window closing. Detach with the detach key
    /// (ctrl-] by default) and reattach later with `rime agent attach`.
    ///
    /// Boxed for `rimed_core::ai::Response::Status`'s reason: `RunArgs` is
    /// four times the size of the next largest variant here and clippy's
    /// `large_enum_variant` is right. It crossed the threshold when
    /// `--trust-ca` was added, so the box is that flag's cost and is recorded
    /// as one rather than as tidying somebody did along the way.
    Run(Box<RunArgs>),
    /// List sessions.
    List {
        /// Include sessions that have already finished.
        #[arg(long, short)]
        all: bool,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
        /// List the sessions on a trusted device instead (§20).
        #[arg(long, value_name = "HOST")]
        host: Option<String>,
    },
    /// Reattach to a session's terminal.
    Attach {
        id: u32,
        /// Do not repaint the scrollback first.
        #[arg(long)]
        no_replay: bool,
        /// Attach to a session on a trusted device (§20).
        ///
        /// This is §20's "continue a terminal or agent session elsewhere": the
        /// session keeps running there, and this becomes a view onto it. The id
        /// is the remote's, which is why `rime agent list --host` exists.
        #[arg(long, value_name = "HOST")]
        host: Option<String>,
    },
    /// Write a handoff packet for a session and continue the work elsewhere.
    ///
    /// §16: Claude runs out of context or quota and the work has to carry on
    /// under a different agent. This reads what the runtime knows about the
    /// session — worktree, checkpoint, changed files, transcript tail, the
    /// grants it held — writes it as a document into the project's `.rime/`,
    /// and starts the target agent pointed at that document.
    ///
    /// Three of §16's nine fields have no producer in this build. They are
    /// written as absent WITH THE REASON, never guessed: a plausible plan the
    /// next agent cannot check is worse than a blank it can see.
    Handoff {
        /// Session to hand off. Defaults to the most recent one in this project.
        id: Option<u32>,
        /// Adapter to hand it to (`codex`, `opencode`, `gemini`, `claude`).
        #[arg(long, short = 't', value_name = "AGENT")]
        to: String,
        /// Write the packet and stop, without starting anything.
        #[arg(long)]
        no_start: bool,
        /// How many bytes of the outgoing transcript to carry.
        #[arg(long, default_value_t = HANDOFF_TRANSCRIPT_BYTES)]
        transcript_bytes: usize,
    },
    /// Type text into a session's terminal.
    ///
    /// The text lands in the agent's prompt exactly as if it had been typed,
    /// and stays there. Add --submit to send it. That is deliberate: the words
    /// may have come from somewhere less certain than a keyboard, and reading
    /// them before they become an instruction is the difference between a
    /// typo and a command.
    ///
    /// Rime Shell's push-to-talk route is the other caller. A session cannot
    /// call this on another session.
    Input {
        id: u32,
        /// The text to type. Several words are joined with single spaces, so
        /// quoting is optional.
        #[arg(required = true, num_args = 1.., value_name = "TEXT")]
        text: Vec<String>,
        /// Press Enter after it, so the agent acts on the line.
        ///
        /// Sent as its own keystroke a moment after the text, never glued to
        /// the end of it: an agent reads a long burst that ends in Enter as a
        /// paste, and a pasted Enter is a newline in the prompt.
        #[arg(long)]
        submit: bool,
    },
    /// Give a session a name, or take it away.
    ///
    /// The name is what `rime agent list`, the Agent Center and Rime Remote
    /// show before anything else — before the agent's own terminal title, and
    /// before `<agent> · <project>`. At most 64 characters, and no control
    /// characters: a name that could rewrite the line it is printed on is
    /// refused, not cleaned. Only a person can name a session; a session
    /// cannot rename itself or another.
    Rename {
        id: u32,
        /// The new name. Several words are joined with single spaces, so
        /// quoting is optional.
        #[arg(
            num_args = 1..,
            value_name = "NAME",
            required_unless_present = "clear",
            conflicts_with = "clear"
        )]
        name: Vec<String>,
        /// Remove the name instead.
        #[arg(long)]
        clear: bool,
        /// Rename a session on a trusted device (§20).
        #[arg(long, value_name = "HOST")]
        host: Option<String>,
    },
    /// Suspend a session and everything it started.
    Pause { id: u32 },
    /// Resume a paused session.
    Resume { id: u32 },
    /// Stop a session.
    Kill {
        id: u32,
        /// int | term | kill. Default term, which lets the agent clean up.
        #[arg(long, default_value = "term")]
        signal: String,
    },
    /// Hand a file to a running session: a screenshot, a log, a crash dump.
    ///
    /// The runtime copies it somewhere the session can read — a confined
    /// session cannot see `~/Pictures` — and types that path into the
    /// session's terminal. It does NOT press Enter: the path is left on the
    /// agent's input line and you send it, which is what keeps a person in the
    /// loop when the channel a file arrives on is the same one your keyboard
    /// uses.
    ///
    /// The session id is required and never guessed. Typing into the wrong
    /// agent is worse than typing a number.
    Send {
        id: u32,
        /// Files to hand over, in the order given.
        #[arg(value_name = "FILE")]
        files: Vec<String>,
        /// Hand over the newest screenshot instead of naming it.
        ///
        /// Press Print, then run this. It reads the directory Rime Shell's
        /// screenshot keybind writes to (`~/Pictures/Screenshots`), so it
        /// takes no picture itself and opens no selection overlay.
        #[arg(long)]
        last_screenshot: bool,
        /// Machine-readable output, one object per file.
        #[arg(long)]
        json: bool,
    },
    /// Print a session's transcript.
    Logs {
        id: u32,
        /// How many bytes of the tail to show.
        #[arg(long, default_value_t = 64 * 1024)]
        bytes: usize,
    },
    /// Per-worktree status: tests, conflicts, diff and local readiness.
    ///
    /// Answers the four questions worth asking about an agent worktree before
    /// touching it — has it got a diff, would it merge back, what happened to
    /// the tests, is it ready to hand over — without running anything in a
    /// worktree somebody else is working in.
    Worktrees {
        /// One project by slug, instead of every remembered project.
        #[arg(long, value_name = "SLUG")]
        project: Option<String>,
        /// Machine-readable output, one object per worktree.
        #[arg(long)]
        json: bool,
    },
    /// Show one session in detail, or the runtime's own status.
    Status { id: Option<u32> },
    /// Show or set the agent that `a` and an unqualified run use.
    Default { agent: Option<String> },
    /// Show or change where an `--network allowlist` session may connect.
    ///
    /// A destination is a host, or a host and a port: `api.example.com`,
    /// `*.example.com`, `git.example.com:22`. A rule with no port means 443.
    /// With no argument this prints the list.
    ///
    /// The list is the runtime's, not a session's — an agent that could name
    /// its own destinations would be writing its own allowlist. A change
    /// applies to sessions started after it; one already running keeps the
    /// list it was started with.
    Allow {
        /// The destination to add. Omit to list what is allowed.
        destination: Option<String>,
        /// Remove it instead of adding it.
        #[arg(long)]
        remove: bool,
    },
    /// Show or change what a screen lock does to running work (§7).
    ///
    /// §7: "ordinary agents may continue; Remote Control may continue if
    /// configured; short-lived root grants should default to revocation; user
    /// policy may override." This is the override, and with no flags it
    /// prints what the machine will do — and what the screen is doing now.
    ///
    /// The runtime picks a change up on its next few-second tick; nothing has
    /// to be restarted.
    Lock {
        /// Ordinary agent sessions on a locked screen: continue | hold.
        #[arg(long, value_name = "WHAT", value_parser = parse_continues)]
        agents: Option<bool>,
        /// Remote Control sessions on a locked screen: continue | hold.
        #[arg(long, value_name = "WHAT", value_parser = parse_continues)]
        remote: Option<bool>,
        /// Short-lived root grants when the screen locks: revoke | keep.
        #[arg(long, value_name = "WHAT", value_parser = parse_revokes)]
        root_grants: Option<bool>,
    },
    /// System-access grants: what has been granted, and to what (§4.4, §4.5).
    ///
    /// §3.4 asks that "revocation control always be visible", which starts
    /// with the grants themselves being visible. Every grant this machine has
    /// ever issued is listed, with how each one ended — including the ones
    /// that ended because the machine rebooted, which is the answer to "was
    /// that break-glass window still open?".
    Grants {
        /// Only the ones still in force.
        #[arg(long)]
        active: bool,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Take a system-access grant back before its window runs out.
    ///
    /// Immediate, and it asks for no password: giving up privilege is free.
    /// A break-glass session whose grant is revoked keeps running — its
    /// `no_new_privs` was cleared at exec and cannot be put back — so the
    /// runtime ends it, the same way it does when the window runs out.
    RevokeGrant { id: u32 },
    /// Extend a grant that is still in force, with a fresh password.
    ///
    /// Not an extension of the old consent: the window is recomputed from now
    /// and the same local authentication is asked for again. A managed
    /// session cannot renew its own grant, and cannot renew anybody's — the
    /// runtime refuses any connection that resolves to a session, from the
    /// kernel's view of it rather than from anything the request says.
    RenewGrant {
        id: u32,
        /// The new window, from now: `15m`, `1h`.
        #[arg(long, value_name = "DURATION", value_parser = parse_ttl)]
        ttl: u64,
    },
    /// List the agents this runtime can launch.
    Adapters,
    /// Inspect, check and carry an agent's own configuration (§5).
    ///
    /// An agent installation is a profile, not just a binary: instructions,
    /// settings, commands, skills, plugins and MCP definitions decide what the
    /// binary does. These verbs say which of that is the same on any machine
    /// and which belongs to this one.
    Profile {
        #[command(subcommand)]
        cmd: ProfileCmd,
    },
    /// What an agent changed since its checkpoint.
    Diff {
        /// Session id. Defaults to the most recent session in this project.
        id: Option<u32>,
        /// Names only, no patch.
        #[arg(long)]
        stat: bool,
    },
    /// Restore the project to a session's checkpoint.
    Undo {
        /// Session id. Defaults to the most recent session in this project.
        id: Option<u32>,
        /// Undo to a specific checkpoint instead.
        #[arg(long, conflicts_with = "id")]
        checkpoint: Option<String>,
        /// Do not ask for confirmation.
        #[arg(long, short)]
        yes: bool,
    },
    /// Capture a checkpoint of the current project now.
    Checkpoint {
        /// What this checkpoint is for.
        label: Option<String>,
    },
    /// Publish a state change for a session.
    ///
    /// This is the open agent event protocol. A process running inside a
    /// session already knows its id from `$RIME_AGENT_SESSION`, so an agent
    /// hook needs no arguments beyond the state.
    Event {
        /// working | waiting_for_user | permission_request | complete | failed
        state: String,
        /// Session id. Defaults to `$RIME_AGENT_SESSION`.
        #[arg(long)]
        session: Option<u32>,
        /// Text shown alongside the state.
        #[arg(long)]
        detail: Option<String>,
    },
    /// Report one of Claude's own lifecycle events (§6.1), and for
    /// `pre_tool_use` ask Rime whether the tool call is one this session's
    /// sandbox would refuse (§6.2).
    ///
    /// Not meant to be typed. `rime-agentd` writes a settings file that runs
    /// this once per event, and the payload arrives on stdin as the JSON
    /// document Claude produces. It takes no session id: `$RIME_AGENT_SESSION`
    /// is set by the sandbox, and an id on the command line would be an id the
    /// agent could edit into another session's.
    ///
    /// It always exits 0 and prints nothing it does not have to. A hook that
    /// failed loudly would be a broken daemon stopping the agent from working.
    #[command(hide = true)]
    Hook {
        /// session_start | pre_tool_use | post_tool_use | stop | …
        event: String,
    },
    /// Claude's status line, wrapped (§P1-021).
    ///
    /// Not meant to be typed either. `rime-agentd` points a managed session's
    /// `statusLine` here, the payload arrives on stdin as the JSON document
    /// Claude produces, and what this prints is what appears under the prompt.
    ///
    /// It does two things and the ORDER is the point. First it runs the user's
    /// own status-line command with the same payload and copies its output —
    /// so the terminal status line is byte-for-byte what it was, which is the
    /// criterion. Second it publishes the model, context and rate-limit
    /// numbers to the daemon, which is the only way any of them reach the
    /// Agent Center.
    ///
    /// Always exits 0. A status line that failed would be a broken daemon
    /// putting an error where the user's prompt used to be.
    #[command(hide = true)]
    Statusline,
    /// Narrow where this session says it is driven from (§7).
    ///
    /// Run from inside a managed session — the runtime works out which session
    /// that is from the connection, so there is no id to pass and no way to
    /// speak about another session.
    ///
    /// Run from anywhere else it narrows the *connection* instead, for as long
    /// as that connection is open. That is only useful to a program holding
    /// the socket open across several requests, which is what `rime-remoted`
    /// does; one `rime agent origin` from a shell narrows a connection that
    /// closes immediately afterwards, and the command says so.
    ///
    /// The declaration can only ever cost the caller something. A local
    /// session may hand itself to Remote Control; nothing may declare itself
    /// local, and a Remote Control session may not declare its way back out.
    Origin {
        /// claude-remote-control | scheduled-job | mcp | subagent | cloud-job
        origin: String,
        /// Which remote actor this is being declared for: a paired device id,
        /// a host name, a job name. Recorded beside the origin on sessions and
        /// privilege requests. Never a key or a token — it is printed on the
        /// prompt a human reads before approving root.
        #[arg(long)]
        actor: Option<String>,
    },
    /// Forget a finished session and delete its transcript.
    Rm { id: u32 },
    /// Forget every finished session.
    Prune,
    /// Turn the per-user agent runtime on for this account.
    ///
    /// The runtime is a `systemd --user` service, opt-in by design. This is the
    /// one command that opts in, so it never matters whether you remember the
    /// `systemctl --user` incantation. Run as root it first gives root a
    /// lingering systemd user instance (root has none by default) and then
    /// prints what running agents as root costs.
    Enable,
    /// The security keys that can answer a remote elevation (§7).
    ///
    /// §7 reserves root for a human at this machine. `--origin-policy remote`
    /// is the owner's opt-out, and what it costs is a touch on one of these
    /// keys. An empty store is what makes that policy refuse to start, so
    /// enrolling one is the first step of turning it on.
    Key {
        #[command(subcommand)]
        cmd: KeyCmd,
    },
}

/// `rime agent key <verb>`.
#[derive(Subcommand)]
pub enum KeyCmd {
    /// Enrol a security key from what `fido2-cred -V` printed.
    ///
    /// The key is plugged into whatever machine the owner is at, which by
    /// construction is not necessarily this one, so this takes the *output*
    /// rather than talking to the device:
    ///
    ///   fido2-cred -M -rk -i params /dev/hidraw0 | fido2-cred -V -o cred.txt
    ///   rime agent key add --label yubikey --rp-id rime.local --from cred.txt
    ///
    /// `fido2-cred -V` prints the credential id and then a PEM public key.
    /// Both are stored as printed, so an operator can compare the file with
    /// the paste.
    Add {
        /// What to call it. Named in a refusal, and in `--credential`.
        #[arg(long, value_name = "NAME")]
        label: String,
        /// The relying party id the credential was created for. It is not this
        /// machine's hostname unless that is what was passed to `fido2-cred`;
        /// an assertion for a different one is refused.
        #[arg(long, value_name = "ID")]
        rp_id: String,
        /// The file `fido2-cred -V` wrote. Omitted reads standard input.
        #[arg(long, value_name = "PATH")]
        from: Option<PathBuf>,
    },
    /// Every enrolled key.
    List,
}

/// `rime agent profile <verb>`.
#[derive(Subcommand)]
pub enum ProfileCmd {
    /// Agents whose profile this runtime understands.
    List {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Show every part of a profile, its class and how a session mounts it.
    Inspect {
        /// Which agent. Defaults to the configured one.
        agent: Option<String>,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Check a profile: config, hooks, plugins, MCP and skills.
    ///
    /// Reads and reports; it repairs nothing. Exits non-zero when it found
    /// something wrong, so it is usable from a script.
    Doctor {
        /// Which agent. Defaults to the configured one.
        agent: Option<String>,
    },
    /// Write the reusable half of a profile to a directory.
    ///
    /// Instructions, skills, commands, settings and MCP definitions. Never
    /// credentials, conversation transcripts, caches or install paths — and
    /// the values of anything the settings put in the environment are replaced
    /// with blanks, so the bundle says which variables are needed without
    /// carrying what is in them.
    Export {
        /// Which agent. Defaults to the configured one.
        agent: Option<String>,
        /// Where to write it. Defaults to ./<agent>-profile.
        #[arg(long, short, value_name = "DIR")]
        to: Option<PathBuf>,
        /// Write into a directory that already has files in it.
        #[arg(long)]
        force: bool,
    },
    /// Bring this machine's profile up to date with an exported one.
    ///
    /// Files are added and updated; nothing local is deleted, and the two
    /// files that hold both reusable and machine-local state are merged key by
    /// key rather than overwritten — so importing a profile cannot remove the
    /// environment values the export refused to carry.
    #[command(name = "sync", visible_alias = "import")]
    Import {
        /// Which agent. Defaults to the configured one.
        agent: Option<String>,
        /// The directory `rime agent profile export` wrote.
        #[arg(long, short, value_name = "DIR")]
        from: PathBuf,
        /// Print what would change and change nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Args)]
pub struct RunArgs {
    /// Opening instruction for the agent.
    pub prompt: Option<String>,
    /// Which agent to run. Defaults to the configured one.
    #[arg(long, short)]
    pub agent: Option<String>,
    /// What to call the session: shown first in `rime agent list`, the Agent
    /// Center and Rime Remote. Defaults to `$RIME_AGENT_NAME`.
    ///
    /// At most 64 characters and no control characters, checked here before
    /// anything starts. `rime agent rename` changes it later.
    #[arg(long, short = 'n', value_name = "NAME")]
    pub name: Option<String>,
    /// strict | project | unrestricted. The Rime filesystem and process
    /// sandbox (dimension 2). Defaults to the configured policy.
    #[arg(long, short, value_parser = parse_sandbox)]
    pub sandbox: Option<SandboxPolicy>,

    // ── §3.1: the other five permission dimensions ──────────────────────────
    //
    // Six separate flags rather than one mode word, because these are six
    // separate controls. A preset below sets several at once for the modes
    // §4 names; these override whatever a preset chose.
    //
    // Parsed into their own types here rather than carried as strings, so an
    // unknown value is refused by the argument parser with the list of real
    // ones, and so nothing downstream can be handed a dimension value that was
    // never checked.
    /// inherit | ask | bypass. The agent's OWN permission mode (dimension 1).
    /// `inherit` leaves the agent's profile alone, which is the default.
    #[arg(long, value_parser = parse_native)]
    pub native: Option<NativeMode>,
    /// §4.2. The agent stops asking for confirmations. The Rime sandbox, root
    /// boundary, secret broker and audit are all untouched.
    #[arg(long, conflicts_with = "native")]
    pub agent_bypass: bool,
    /// none | session | unsafe. The root capability layer (dimension 3).
    #[arg(long, value_parser = parse_system_access)]
    pub system_access: Option<SystemAccess>,
    /// brokered | none | export. The secret capability layer (dimension 4).
    #[arg(long, value_parser = parse_secrets)]
    pub secrets: Option<SecretPolicy>,
    /// open | allowlist | brokered | offline. Network policy (dimension 5).
    #[arg(long, value_parser = parse_network)]
    pub network: Option<NetworkPolicy>,
    /// local | remote. Which origins may authorise elevation (dimension 6).
    #[arg(long, value_parser = parse_origin_policy)]
    pub origin_policy: Option<OriginPolicy>,
    /// all | local | curated | none. Which MCP connectors the session gets
    /// (dimension 7).
    ///
    /// `curated` takes its names from `connector_allow` in the runtime's
    /// configuration and not from this command line, for the reason
    /// `--network allowlist` takes its destinations from there: a list the
    /// confined thing can write is not a boundary.
    #[arg(long, value_parser = parse_connectors)]
    pub connectors: Option<ConnectorPolicy>,
    /// all | curated | none. Which plugins the session loads (dimension 8).
    ///
    /// A plugin's hooks and the scripts beside them are spawned by the agent
    /// itself, before the session has done anything, and no MCP configuration
    /// of any kind is involved — so dimension 7 cannot reach them and this
    /// dimension REMOVES where that one confines. Rime does not own the
    /// agent's process and cannot put a sandbox around a process the agent
    /// spawns; what it can decide is which plugins are loaded at all.
    ///
    /// `curated` takes its names from `plugin_allow` in the runtime's
    /// configuration and not from this command line, for the reason
    /// `--connectors curated` takes its names from there.
    #[arg(long, value_parser = parse_plugins)]
    pub plugins: Option<PluginPolicy>,

    // ── §7: where the session is driven from ────────────────────────────────
    /// Declare where this session is driven from (§7's request_origin).
    ///
    /// Not the same thing as `--origin-policy`, which decides which origins
    /// may authorise elevation. This says which origin THIS session is, and
    /// it can only ever narrow: the daemon establishes the origin from the
    /// connection, and a declaration is accepted only when it gives something
    /// up. The two local origins cannot be declared at all.
    ///
    /// For a wrapper starting a session on somebody else's behalf — a
    /// scheduler, an MCP bridge, Remote Control. A session already running
    /// narrows itself with `rime agent origin` instead.
    #[arg(long, value_name = "ORIGIN", value_parser = parse_request_origin)]
    pub origin: Option<RequestOrigin>,
    /// §4.5 break-glass: take the Rime protections off.
    #[arg(long)]
    pub unsafe_everything: bool,
    /// How long a system-access grant lasts: `15m`, `90s`, `1h`.
    ///
    /// Required with `--unsafe-everything` — §3.4 asks for an "explicit short
    /// TTL", and a default would be the opposite of explicit. Optional with
    /// `--system-access session`, which is a smaller grant and takes a
    /// half-hour default. Refused without either, because a TTL on an
    /// ordinary session bounds nothing.
    ///
    /// A bare number is refused: `15` is fifteen seconds or fifteen minutes
    /// depending on who is reading, and the value is a security window.
    #[arg(long, value_name = "DURATION", value_parser = parse_ttl)]
    pub ttl: Option<u64>,
    /// Which privilege verbs the grant covers: `install,update`.
    ///
    /// §3.3 asks for a grant that is "capability scoped", and without this
    /// every session grant covered all eight verbs — a session that needed to
    /// install one package could also roll the system back. Omitted, that is
    /// still what you get, because it is what the flag's absence has always
    /// meant.
    ///
    /// A name that is not a verb is refused rather than dropped: a grant
    /// quietly covering less than was asked for fails in the middle of a
    /// session instead of here. Refused with `--unsafe-everything`, which
    /// does not go through `rime request` and has no verbs to narrow.
    // No `num_args = 1..`: `prompt` is POSITIONAL, so a greedy multi-value flag
    // would swallow it — `--capabilities install "fix the bug"` would read the
    // prompt as a second verb and the daemon would refuse it as not a verb.
    // The comma delimiter is how more than one is given.
    #[arg(long, value_name = "VERBS", value_delimiter = ',')]
    pub capabilities: Option<Vec<String>>,
    /// Where THIS session may connect: `api.example.com`, `intranet:8443`.
    /// Repeatable, and only with `--network allowlist` (P2-012).
    ///
    /// Omitted, an allowlisted session reaches every destination in the
    /// runtime's own list — which is what a browser capsule started to visit
    /// one host got before this flag existed. Given, the session reaches these
    /// and nothing else.
    ///
    /// It can only ever SUBTRACT. Every line has to be covered by a rule the
    /// runtime already carries, and a line it does not cover is refused with
    /// the `rime agent allow` that would permit it — a session that could name
    /// a destination the machine never permitted would make the runtime's list
    /// advisory. So this is not a second place to write the policy; it is a
    /// narrowing of the one place it is written.
    ///
    /// Refused with an empty value for the same reason `--network offline`
    /// exists: "reach nothing" already has a spelling, and two spellings of one
    /// policy is how they drift apart.
    // No `value_delimiter`: a destination is `host:port`, and a comma-split
    // flag invites `--allow a.example,b.example` to be read as a host with a
    // comma in it on the day the delimiter is removed. Repetition is the
    // spelling, which is also `rime browser --allow`'s.
    #[arg(long, value_name = "DESTINATION")]
    pub allow: Option<Vec<String>>,
    /// A PEM file of certificate authorities this session's BROWSER trusts on
    /// top of the machine's own, for this session only (P2-012).
    ///
    /// The intranet case: a site whose certificate is signed by an
    /// organisation's own root, which is not in the machine's trust store and
    /// should not be added to it for the sake of one automated run.
    ///
    /// It is the browser's trust and not the session's, and the difference is
    /// not a detail: `curl`, `git` and `python` in the same sandbox keep using
    /// the system bundle and still refuse the host. What is installed is a
    /// Firefox enterprise-policy root, because that is the only CA install
    /// route in the image — there is no `certutil`, since `nss-tools` is not
    /// installed.
    ///
    /// It names a FILE and defaults to nothing. Handing a session a CA it did
    /// not have widens what it will believe, and a default would widen it
    /// without anybody typing anything.
    ///
    /// Only with a confined sandbox. An unconfined session has no mount
    /// namespace to install the policy into, so the flag would be accepted and
    /// mean nothing; the daemon refuses the pair too.
    #[arg(long, value_name = "FILE")]
    pub trust_ca: Option<PathBuf>,
    /// Authenticate this session's ONE destination with a stored credential
    /// the session is never given (P2-012, route B).
    ///
    /// It names the CREDENTIAL. Where it is spent is not a second flag: it is
    /// the destination that credential is pinned to, and the daemon requires
    /// `--allow` to be exactly that host and port — a session that names a
    /// credential may reach the destination the credential is for and nothing
    /// else.
    ///
    /// What happens is that the runtime terminates that one connection with a
    /// certificate it mints for the run, hands the plaintext to
    /// `rime-secretd`, and that daemon — the only one that holds credentials —
    /// adds the header and connects to the site. Neither the session nor the
    /// agent runtime ever holds the value.
    ///
    /// Needs a confined sandbox, `--network allowlist`, and a grant:
    /// `rime secret grant <credential> browser.present --everywhere`. Cannot
    /// be combined with `--trust-ca`.
    #[arg(long, value_name = "CREDENTIAL")]
    pub present: Option<String>,
    /// Run in a dedicated git worktree, creating it if needed.
    #[arg(long, short)]
    pub worktree: Option<String>,
    /// Capture a checkpoint first, so `rime agent undo` can put it back.
    #[arg(long, short)]
    pub checkpoint: bool,
    /// Where to run. Defaults to the current directory.
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    /// Run inside a disposable capsule and delete the whole environment when
    /// the session ends (§19).
    ///
    /// The working directory is COPIED in, not shared, so whatever the agent
    /// does to it goes with the environment. That is what makes the state
    /// disposable — and it means nothing comes back unless `--copy-out` says
    /// where to put it.
    ///
    /// A throwaway ENVIRONMENT, not a security boundary. distrobox mounts the
    /// host filesystem at /run/host in every capsule and the process runs as
    /// your own uid, so code in there can still reach your real home. For
    /// confinement — $HOME masked, ~/.ssh unreachable — use `--sandbox`
    /// instead; the two are refused together rather than pretending to
    /// combine. `rime disposable plan` prints the whole boundary.
    #[arg(long)]
    pub disposable: bool,
    /// Where the capsule's ~/out is copied when it closes. Needs
    /// `--disposable`.
    ///
    /// Without it NOTHING leaves the environment. An agent that should hand
    /// work back writes it to ~/out inside.
    #[arg(long, value_name = "DIR", requires = "disposable")]
    pub copy_out: Option<String>,
    /// Start it and return, instead of attaching.
    #[arg(long, short)]
    pub detach: bool,
    /// Arguments passed straight to the agent binary. With `--agent generic`
    /// the first one is the program to run.
    #[arg(last = true)]
    pub args: Vec<String>,

    // ── §20: run it on another device instead ───────────────────────────────
    /// Run the agent on this trusted device instead of here.
    ///
    /// The whole invocation is forwarded to that machine's own
    /// `rime agent run`, which applies its own sandbox policy, default agent
    /// and checkpointing. Reconstructing those decisions locally would mean
    /// two implementations of one policy, and the remote's is the one that
    /// matters because that is where the agent actually runs.
    #[arg(long, value_name = "HOST")]
    pub host: Option<String>,
    /// The project directory on the remote, when it is not the same absolute
    /// path as here. Skips the same-repository check.
    #[arg(long, value_name = "PATH", requires = "host")]
    pub remote_path: Option<String>,
    /// Run remotely even though this worktree has uncommitted changes.
    ///
    /// They are NOT sent: the remote works from its own checkout.
    #[arg(long, requires = "host")]
    pub allow_dirty: bool,
}

/// The environment variable a session's name is taken from when `--name` is
/// not given (§1.4).
///
/// For a wrapper that knows what the work is called — a script that starts one
/// agent per ticket — and would otherwise have to rebuild every `a` it calls.
pub const NAME_ENV: &str = "RIME_AGENT_NAME";

impl RunArgs {
    /// The name this invocation asks for: `--name`, else `$RIME_AGENT_NAME`,
    /// unchecked. An empty variable is no name rather than a request to clear
    /// one, which at `run` would mean the same thing anyway.
    fn name_or_env(&self) -> Option<String> {
        self.name.clone().or_else(|| {
            std::env::var(NAME_ENV)
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
    }

    /// [`RunArgs::name_or_env`], checked by the validator the daemon uses, so
    /// a name the daemon would refuse is refused here, before a worktree, a
    /// checkpoint or a remote login — and says which of the two it came from.
    pub fn validated_name(&self) -> Result<Option<String>> {
        let from_env = self.name.is_none();
        let Some(raw) = self.name_or_env() else {
            return Ok(None);
        };
        rime_agent_core::session::session_name(&raw).map_err(|why| {
            if from_env {
                anyhow!("${NAME_ENV}: {why}")
            } else {
                anyhow!("--name: {why}")
            }
        })
    }

    /// Rebuild the flags this invocation carried, for forwarding to a remote
    /// `rime agent run`.
    ///
    /// Reconstructed from the parsed struct rather than taken from
    /// `std::env::args`, so a flag that clap normalised or defaulted is
    /// forwarded in its normalised form — and so the local-only flags
    /// (`--host`, `--remote-path`, `--allow-dirty`) cannot leak into the
    /// remote command and make it try to dispatch again.
    pub fn forward_argv(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(p) = &self.prompt {
            out.push(p.clone());
        }
        // The EFFECTIVE name, so `$RIME_AGENT_NAME` goes with the run: the
        // remote command runs in a login on another machine, which never saw
        // this shell's environment.
        for (flag, value) in [
            ("--agent", self.agent.clone()),
            ("--name", self.name_or_env()),
            ("--worktree", self.worktree.clone()),
        ] {
            if let Some(v) = value {
                out.push(flag.to_string());
                out.push(v);
            }
        }
        // Every permission dimension, or a remote run would silently get the
        // remote's defaults instead of what was asked for. Losing
        // `--network offline` on the way to another machine is the kind of
        // omission nobody notices until it matters.
        for (flag, value) in [
            ("--sandbox", self.sandbox.map(|v| v.as_str())),
            ("--native", self.native.map(|v| v.as_str())),
            ("--system-access", self.system_access.map(|v| v.as_str())),
            ("--secrets", self.secrets.map(|v| v.as_str())),
            ("--network", self.network.map(|v| v.as_str())),
            ("--origin-policy", self.origin_policy.map(|v| v.as_str())),
            ("--connectors", self.connectors.map(|v| v.as_str())),
            ("--plugins", self.plugins.map(|v| v.as_str())),
            ("--origin", self.origin.map(|v| v.as_str())),
        ] {
            if let Some(v) = value {
                out.push(flag.to_string());
                out.push(v.to_string());
            }
        }
        if self.agent_bypass {
            out.push("--agent-bypass".to_string());
        }
        if self.unsafe_everything {
            out.push("--unsafe-everything".to_string());
        }
        // Forwarded in its parsed form, in milliseconds' worth of seconds, so
        // the remote applies the window that was asked for rather than its own
        // default. Losing a `--ttl` on the way to another machine would leave
        // a break-glass session there with a longer window than the user typed
        // — or, with `--unsafe-everything`, refuse outright, which is at least
        // loud. This is the quiet half, so it is forwarded.
        if let Some(ms) = self.ttl {
            out.push("--ttl".to_string());
            out.push(format!("{}s", ms / 1000));
        }
        // Forwarded for a stronger reason than the TTL is. Losing a `--ttl` on
        // the way to another machine leaves a longer window than was typed;
        // losing this leaves a grant over every verb on a machine the user is
        // not sitting at. A narrowing that only applies locally is not a
        // narrowing.
        if let Some(caps) = &self.capabilities {
            out.push("--capabilities".to_string());
            out.push(caps.join(","));
        }
        if self.checkpoint {
            out.push("--checkpoint".to_string());
        }
        if self.detach {
            out.push("--detach".to_string());
        }
        // `--cwd` is deliberately NOT forwarded: the remote command already
        // cds into the resolved project directory, and a local path would be
        // meaningless or wrong there.
        if !self.args.is_empty() {
            out.push("--".to_string());
            out.extend(self.args.iter().cloned());
        }
        out
    }
}

/// `rime project <verb>`.
#[derive(Subcommand)]
pub enum ProjectCmd {
    /// Projects the runtime has seen, most recent first.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Describe the project containing the current directory.
    Info,
    /// Agent worktrees of the current project.
    Worktrees,
    /// Checkpoints recorded for the current project.
    Checkpoints,
    /// Remove an agent worktree and its branch.
    Remove {
        name: String,
        /// Keep the branch.
        #[arg(long)]
        keep_branch: bool,
        /// §13.13: destroy the Cloudflare resources this worktree owns first.
        ///
        /// The plan is printed before anything is destroyed, and the worktree
        /// is removed only if every destroyable thing went. Without this the
        /// worktree is removed and whatever it created at Cloudflare stays up —
        /// `rime cf preview plan` inside it says what that would be.
        #[arg(long)]
        destroy_preview: bool,
    },
    /// Stop tracking a project. The checkout is never touched.
    Forget { slug: String },
    /// §36's `[identity.*]`: which account this project may act as.
    ///
    /// Prints all four sections whether the project binds them or not, and —
    /// the part that matters — whether anything in this build actually checks
    /// each one. A report that listed a binding without saying it is
    /// unenforced would tell you an ssh host group protects something.
    Identity {
        #[arg(long)]
        json: bool,
    },
    /// The capsule (§8) this project's work belongs in.
    ///
    /// With no argument it reports the binding, and suggests an image alias
    /// when there is none. The suggestion is printed and never acted on:
    /// creating a container because a `package.json` exists would be a
    /// surprise measured in gigabytes.
    Env {
        /// Bind this project to a capsule. `rime env list` shows the ones you
        /// have; `rime env create <name>` makes one.
        #[arg(value_name = "CAPSULE")]
        name: Option<String>,
        /// Remove the binding. The capsule itself is untouched.
        #[arg(long, conflicts_with = "name")]
        clear: bool,
    },
    /// Go to a project: switch to the workspace its windows are on.
    ///
    /// §6's "allow switching by project, not only by numeric workspace". Needs
    /// a saved layout, because that is what records which workspace a project
    /// lives on — `rime project layout save` first.
    Switch {
        /// Project name or slug. Defaults to the one containing the current
        /// directory.
        name: Option<String>,
    },
    /// Remember or restore the windows and terminals of a project (§6).
    ///
    /// A saved layout stores how to RECREATE each window — its argv, its
    /// working directory and the workspace it was on — not a window handle,
    /// which no compositor honours after a restart.
    ///
    /// Which windows count is decided from the working directory of the
    /// process tree behind each one, never from the title: a title is whatever
    /// an application chose to print.
    Layout {
        #[command(subcommand)]
        cmd: LayoutCmd,
    },
}

/// `rime project layout <verb>`.
#[derive(Subcommand)]
pub enum LayoutCmd {
    /// Capture the windows currently working inside this project.
    Save,
    /// Show the saved layout.
    Show {
        #[arg(long)]
        json: bool,
    },
    /// Reopen the saved layout.
    ///
    /// Deliberately a command and not a login hook: a session that reopens
    /// fourteen windows nobody asked for is worse than one that reopens none.
    Restore {
        /// Print what would be started, and start nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Discard the saved layout.
    Forget,
    /// The terminal layout templates, and what each one opens.
    Templates,
    /// Open this project's terminal layout in tmux or zellij.
    ///
    /// An editor beside an agent beside a terminal, or several agents side by
    /// side. The multiplexer is a VIEWPORT: every agent pane attaches to a
    /// session rime-agentd owns, so closing the multiplexer leaves the agents
    /// running and reopening finds them again.
    ///
    /// Reopening never rebuilds a session that is already there — it attaches
    /// to it.
    Open {
        /// Template name; `rime project layout templates` lists them. Defaults
        /// to the one last opened for this project, then to `dev`.
        template: Option<String>,
        /// tmux or zellij. Defaults to $RIME_MUX, then to whichever is
        /// installed.
        #[arg(long)]
        mux: Option<String>,
        /// How many agent panes, for a template that repeats one.
        #[arg(long, default_value_t = 1)]
        agents: usize,
        /// Print the panes that would be opened, and open nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

// ── agent verbs ─────────────────────────────────────────────────────────────

pub fn agent(cmd: AgentCmd) -> i32 {
    let result = match cmd {
        AgentCmd::Run(args) => match &args.host {
            // §20. Checked before anything local happens, so a remote run
            // never starts a local session as a side effect.
            Some(host) => {
                // Checked here as well as by the remote, so a bad name is
                // refused before an ssh login rather than after one.
                if let Err(e) = args.validated_name() {
                    return report(Err(e));
                }
                let (h, rp, ad) = (host.clone(), args.remote_path.clone(), args.allow_dirty);
                let forward = args.forward_argv();
                // `Result<i32>`, matching the local arm: this function's
                // contract is the exit code, and the surrounding dispatcher
                // owns the error printing.
                crate::dispatch::agent_run_remote(&h, rp.as_deref(), ad, &forward).map(|()| 0)
            }
            None => run(*args),
        },
        AgentCmd::List { all, json, host } => match host {
            Some(h) => {
                let mut argv = vec!["agent".to_string(), "list".to_string()];
                if all {
                    argv.push("--all".to_string());
                }
                if json {
                    argv.push("--json".to_string());
                }
                // No tty: this is output to be read or parsed, and asking for
                // one would make ssh warn when stdin is a pipe.
                crate::dispatch::forward_to_host(
                    &h,
                    &argv,
                    rimed_core::host::Tty::None,
                    Some(crate::dispatch::Capability::Agentd),
                )
                .map(|()| 0)
            }
            None => list(all, json),
        },
        AgentCmd::Attach { id, no_replay, host } => match host {
            Some(h) => {
                let mut argv =
                    vec!["agent".to_string(), "attach".to_string(), id.to_string()];
                if no_replay {
                    argv.push("--no-replay".to_string());
                }
                // A terminal, always: attaching is the interactive case, and
                // the remote pty has to be forwarded for it to be usable.
                crate::dispatch::forward_to_host(
                    &h,
                    &argv,
                    rimed_core::host::Tty::Interactive,
                    Some(crate::dispatch::Capability::Agentd),
                )
                .map(|()| 0)
            }
            None => attach(id, !no_replay),
        },
        AgentCmd::Input { id, text, submit } => input(id, &text.join(" "), submit),
        AgentCmd::Rename {
            id,
            name,
            clear,
            host,
        } => match host {
            Some(h) => {
                let mut argv = vec!["agent".to_string(), "rename".to_string(), id.to_string()];
                if clear {
                    argv.push("--clear".to_string());
                } else {
                    argv.extend(name);
                }
                // No tty, as for `list`: nothing here is interactive.
                crate::dispatch::forward_to_host(
                    &h,
                    &argv,
                    rimed_core::host::Tty::None,
                    Some(crate::dispatch::Capability::Agentd),
                )
                .map(|()| 0)
            }
            None => rename(id, (!clear).then(|| name.join(" "))),
        },
        AgentCmd::Handoff { id, to, no_start, transcript_bytes } =>
            handoff(id, &to, no_start, transcript_bytes),
        AgentCmd::Pause { id } => signal(id, "stop", "paused"),
        AgentCmd::Resume { id } => signal(id, "cont", "resumed"),
        AgentCmd::Kill { id, signal: sig } => signal(id, &sig, "signalled"),
        AgentCmd::Send {
            id,
            files,
            last_screenshot,
            json,
        } => send(id, files, last_screenshot, json),
        AgentCmd::Logs { id, bytes } => logs(id, bytes),
        AgentCmd::Worktrees { project, json } => worktrees(project, json),
        AgentCmd::Status { id } => status(id),
        AgentCmd::Default { agent } => default_agent(agent),
        AgentCmd::Allow {
            destination,
            remove,
        } => allow(destination, remove),
        AgentCmd::Lock {
            agents,
            remote,
            root_grants,
        } => lock_policy(agents, remote, root_grants),
        AgentCmd::Grants { active, json } => grants(active, json),
        AgentCmd::RevokeGrant { id } => revoke_grant(id),
        AgentCmd::RenewGrant { id, ttl } => renew_grant(id, ttl),
        AgentCmd::Adapters => adapters(),
        AgentCmd::Profile { cmd } => profile_cmd(cmd),
        AgentCmd::Diff { id, stat } => diff(id, stat),
        AgentCmd::Undo {
            id,
            checkpoint: cp,
            yes,
        } => undo(id, cp, yes),
        AgentCmd::Checkpoint { label } => make_checkpoint(label),
        AgentCmd::Event {
            state,
            session,
            detail,
        } => event(state, session, detail),
        AgentCmd::Hook { event } => return hook(&event),
        AgentCmd::Statusline => return statusline(),
        AgentCmd::Origin { origin, actor } => declare_origin(&origin, actor),
        AgentCmd::Rm { id } => remove(id),
        AgentCmd::Prune => prune(),
        AgentCmd::Enable => enable(),
        AgentCmd::Key { cmd } => match cmd {
            KeyCmd::Add {
                label,
                rp_id,
                from,
            } => key_add(&label, &rp_id, from.as_deref()),
            KeyCmd::List => key_list(),
        },
    };
    report(result)
}

fn report(result: Result<i32>) -> i32 {
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("rime: {e:#}");
            1
        }
    }
}

/// The `value_parser`s for the six dimension flags.
///
/// One per dimension rather than one generic helper, because each has to name
/// its own values in the refusal: a user who mistypes `--network allow` needs
/// to be shown `allowlist`, not told that something was invalid.
macro_rules! dimension_parser {
    ($name:ident, $ty:ty, $choices:literal) => {
        fn $name(s: &str) -> std::result::Result<$ty, String> {
            <$ty>::parse(s).ok_or_else(|| format!("use {}", $choices))
        }
    };
}

dimension_parser!(parse_native, NativeMode, "inherit, ask or bypass");
dimension_parser!(parse_sandbox, SandboxPolicy, "strict, project or unrestricted");
dimension_parser!(parse_system_access, SystemAccess, "none, session or unsafe");
dimension_parser!(parse_secrets, SecretPolicy, "brokered, none or export");
dimension_parser!(parse_network, NetworkPolicy, "open, allowlist, brokered or offline");
dimension_parser!(parse_origin_policy, OriginPolicy, "local or remote");
dimension_parser!(
    parse_connectors,
    ConnectorPolicy,
    "all, local, curated or none"
);
dimension_parser!(parse_plugins, PluginPolicy, "all, curated or none");

/// `--agents` and `--remote` on `rime agent lock`.
///
/// §7 words both rules as "may continue", so the value is the sentence rather
/// than a bare true/false: `--agents hold` says what will happen, where
/// `--agents false` would leave the reader working out which way round it is.
fn parse_continues(s: &str) -> std::result::Result<bool, String> {
    match s {
        "continue" | "continues" | "run" | "keep-running" => Ok(true),
        "hold" | "held" | "pause" | "stop" => Ok(false),
        _ => Err("use continue or hold".to_string()),
    }
}

/// `--root-grants` on `rime agent lock`.
fn parse_revokes(s: &str) -> std::result::Result<bool, String> {
    match s {
        "revoke" | "revoked" => Ok(true),
        "keep" | "kept" | "hold" => Ok(false),
        _ => Err("use revoke or keep".to_string()),
    }
}

/// `--ttl`, in milliseconds.
///
/// Only the shape is checked here. Whether the window is issuable — the caps,
/// the zero, and whether break-glass may default — is
/// [`rime_agent_core::grant::ttl_for`], because it depends on which mode was
/// asked for and the daemon has to apply the same rule to a client that
/// skipped this.
fn parse_ttl(s: &str) -> std::result::Result<u64, String> {
    rime_agent_core::grant::parse_ttl(s).map_err(|e| e.to_string())
}

/// `--origin`, which is not a dimension and does not accept every value.
///
/// The two local origins parse — they are real §7 names and `RequestOrigin`
/// has to read them back off a record — but they are refused here, at the
/// only place a human types one. A flag that accepted `local-terminal` and
/// left the daemon to refuse it would read like something that works.
fn parse_request_origin(s: &str) -> std::result::Result<RequestOrigin, String> {
    let declarable = || {
        RequestOrigin::ALL
            .iter()
            .filter(|o| o.may_be_declared())
            .map(|o| o.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match RequestOrigin::parse(s) {
        Some(o) if o.may_be_declared() => Ok(o),
        Some(o) => Err(format!(
            "{o} is established by the runtime from the connection, not declared; use {}",
            declarable()
        )),
        None => Err(format!("use {}", declarable())),
    }
}

/// Resolve the six permission dimensions for one invocation.
///
/// Three layers, each overriding the one before it: the configured defaults,
/// then the preset a mode flag names, then each explicit dimension flag. So
/// `--unsafe-everything --secrets none` is break-glass with the secret layer
/// still off, and no combination is unreachable because a preset claimed it.
///
/// Pure, so the resolution order is asserted directly rather than inferred
/// from what a session ended up with.
pub fn resolve_policy(cfg: &config::Config, args: &RunArgs) -> Result<AgentPolicy> {
    if args.unsafe_everything && args.agent_bypass {
        bail!("--unsafe-everything already turns the agent's own confirmations off");
    }

    // 1. the configured defaults.
    let mut policy = cfg.policy();

    // 2. the preset, when a mode flag named one.
    if args.unsafe_everything {
        policy = PolicyPreset::UnsafeEverything.policy();
    } else if args.agent_bypass {
        // Only dimension 1 moves. §4.2's whole point is that the other five
        // stay where the user's configuration left them, so this is not the
        // preset's full six-tuple.
        policy.native = NativeMode::Bypass;
    }

    // 3. the explicit flags, which win over everything.
    if let Some(v) = args.native {
        policy.native = v;
    }
    if let Some(v) = args.sandbox {
        policy.sandbox = v;
    }
    if let Some(v) = args.system_access {
        policy.system = v;
    }
    if let Some(v) = args.secrets {
        policy.secrets = v;
    }
    if let Some(v) = args.network {
        // The contradiction is caught here rather than in the policy type,
        // because only the CLI can tell "the user typed --network open" from
        // "an older client sent no network key at all". Silently tightening
        // would leave somebody believing they had a network they did not.
        if policy.sandbox == SandboxPolicy::Strict && v != NetworkPolicy::Offline {
            bail!(
                "`--sandbox strict` removes the network, so it cannot be combined with \
                 `--network {v}`; use `--sandbox project --network {v}` instead"
            );
        }
        policy.network = v;
    }
    if let Some(v) = args.origin_policy {
        policy.origin = v;
    }
    if let Some(v) = args.connectors {
        policy.connectors = v;
    }
    if let Some(v) = args.plugins {
        policy.plugins = v;
    }

    // Refuse anything this build cannot enforce, here as well as in the
    // daemon: the message is better in front of the user who typed the flag.
    // The allowlist goes in because `--network allowlist` with nothing on it
    // is a refusal too, and the user who typed the flag is the one who can
    // fix it.
    policy.validate_for(&cfg.allowlist(), &cfg.connector_allow, &cfg.plugin_allow)?;
    Ok(policy.normalised())
}

/// The dimensions this invocation moved away from the safe default.
///
/// Used for the version check and for the banner: only a dimension that is
/// doing something needs saying.
fn non_default_dimensions(policy: &AgentPolicy) -> Vec<(&'static str, &'static str)> {
    let base = AgentPolicy::default();
    policy
        .dimensions()
        .into_iter()
        .zip(base.dimensions())
        .filter(|((_, got), (_, want))| got != want)
        .map(|((name, got), _)| (name, got))
        .collect()
}

fn run(args: RunArgs) -> Result<i32> {
    let cfg = config::Config::load();
    let cwd = match args.cwd.as_ref() {
        Some(dir) => dir
            .canonicalize()
            .with_context(|| format!("{} does not exist", dir.display()))?,
        None => std::env::current_dir().context("reading the current directory")?,
    };

    let policy = resolve_policy(&cfg, &args)?;

    // P2-012. The daemon refuses this pair too, and has to: a client is not a
    // boundary. It is refused here as well because the message is better in
    // front of the user who typed the flag, and because the local check is
    // what stops an `open` session being started, running, and only then
    // being told the destinations it named meant nothing.
    //
    // `effective_network()`, not `policy.network`, and the same call the
    // daemon makes: `--sandbox strict` forces `Offline` whatever `--network`
    // said, and a check that read the field would accept `--allow` for a
    // session with no network at all.
    if let Some(lines) = args.allow.as_ref() {
        if policy.effective_network() != NetworkPolicy::Allowlist {
            bail!(
                "`--allow` names destinations for a session whose network is `{}`, which \
                 has no allowlist to narrow; add `--network allowlist`, or drop `--allow`",
                policy.effective_network()
            );
        }
        if lines.is_empty() {
            bail!(
                "`--allow` with nothing on it would mean a session that reaches nothing, \
                 which is `--network offline`; one policy should have one spelling"
            );
        }
    }

    // P2-012's gap 5. Refused here as well as in the daemon for `--allow`'s
    // reason — the message belongs in front of the person who typed the flag —
    // and resolved here because the wire carries an absolute path: a relative
    // one would be resolved against the DAEMON's working directory, which is
    // not the caller's, and would name a different file or no file at all.
    //
    // `canonicalize` rather than a manual join, so a path that does not exist
    // fails now, by name, instead of arriving at the daemon as a refusal about
    // a file the user cannot see from the message.
    let trust_ca = match args.trust_ca.as_ref() {
        None => None,
        Some(file) => {
            if !policy.sandbox.is_confined() {
                bail!(
                    "`--trust-ca` installs a certificate authority inside the session's mount \
                     namespace, and `--sandbox {}` does not build one — the flag would be \
                     accepted and change nothing; add `--sandbox project`, or drop the \
                     `--trust-ca`",
                    policy.sandbox
                );
            }
            let abs = file
                .canonicalize()
                .with_context(|| format!("--trust-ca {} cannot be read", file.display()))?;
            Some(abs.to_string_lossy().into_owned())
        }
    };

    // Before anything is created, for the reason the daemon checks it before
    // anything is: a refused name should cost the person nothing.
    let name = args.validated_name()?;

    let size = term::stdout_window_size();
    let request = RunRequest {
        agent: args.agent.clone(),
        name,
        prompt: args.prompt.clone(),
        args: args.args.clone(),
        cwd: cwd.to_string_lossy().into_owned(),
        policy,
        request_origin: args.origin,
        worktree: args.worktree.clone(),
        checkpoint: args.checkpoint,
        ttl_ms: args.ttl,
        capabilities: args.capabilities.clone(),
        allow: args.allow.clone(),
        trust_ca,
        present: args.present.clone(),
        // Nothing to send yet: collecting an assertion needs a challenge to
        // have been asked for, and the command that asks for one is the next
        // commit. The daemon's reader landed with this one so that the gate
        // and the field it reads arrive together.
        second_factor: None,
        cols: size.cols,
        rows: size.rows,
        env: Vec::new(),
        disposable: args.disposable,
        copy_out: args.copy_out.clone(),
    };

    let mut c = Client::connect()?;
    check_daemon_understands(
        &mut c,
        &policy,
        args.origin,
        args.capabilities.is_some(),
        args.allow.is_some(),
        args.trust_ca.is_some(),
        args.present.is_some(),
    )?;
    let info = match c.call(&Request::Run(Box::new(request)))? {
        Response::Session(info) => *info,
        other => bail!("unexpected reply: {other:?}"),
    };

    if let Some(wt) = &info.worktree {
        eprintln!("rime: worktree {wt} at {}", info.cwd);
    }
    if let Some(cp) = &info.checkpoint {
        eprintln!("rime: checkpoint {cp} — undo with `rime agent undo {}`", info.id);
    }

    if args.detach {
        match &info.name {
            Some(name) => println!(
                "session {} \"{name}\" — {} in {}",
                info.id,
                info.agent,
                short_path(&info.cwd)
            ),
            None => println!(
                "session {} — {} in {}",
                info.id,
                info.agent,
                short_path(&info.cwd)
            ),
        }
        println!("attach with: rime agent attach {}", info.id);
        return Ok(0);
    }

    // §3.4's "prominent red indicator", in the surface a terminal user is
    // looking at. The Agent Center shows the same thing from the same field.
    if let (Some(grant), Some(expires)) = (info.grant, info.grant_expires_ms) {
        let left = expires.saturating_sub(rime_agent_core::request::now_ms());
        let window = rime_agent_core::grant::format_ms(left);
        if info.policy.system == SystemAccess::Unsafe {
            eprintln!(
                "{}",
                red(&format!(
                    "rime: BREAK-GLASS — grant {grant} removes the Rime root boundary for \
                     {window}. This session can become root. It ends when the window does; \
                     `rime agent revoke-grant {grant}` ends it now"
                ))
            );
        } else {
            eprintln!(
                "rime: system-access grant {grant} for {window} — the privilege operations \
                 this session files are pre-approved until it runs out; \
                 `rime agent revoke-grant {grant}` ends it now"
            );
        }
    }
    eprintln!(
        "rime: session {} ({}, {}) — detach with {}",
        info.id,
        info.agent,
        describe_policy(&info.policy),
        cfg.detach_key
    );
    attach_session(info.id, true, &cfg)
}

/// One line naming the sandbox and anything else that is not at its default.
///
/// §4.1's second criterion — "Rime does not duplicate the dangerous-mode
/// warning" — is a property of this function and of what surrounds it. Claude
/// prints its own banner when it is in `bypassPermissions`, every launch, and
/// a user who has set that as their profile default has already agreed to see
/// it. A second Rime warning saying the same thing would be noise on every
/// run, and noise on every run is how a warning stops being read — including
/// the one warning here that is worth reading, which is break-glass.
///
/// So this line states what Rime's OWN layers are, in Rime's own vocabulary,
/// and says nothing evaluative about dimension 1. `native bypass` appears in
/// it when the user asked Rime for that mode, as a fact among five other
/// facts. It is a status line, not a caution; `dimension_warnings_are_rimes_own`
/// holds it to that.
fn describe_policy(policy: &AgentPolicy) -> String {
    let mut parts = vec![format!("sandbox {}", policy.sandbox)];
    for (name, value) in non_default_dimensions(policy) {
        if name != "sandbox" {
            parts.push(format!("{name} {value}"));
        }
    }
    parts.join(", ")
}

/// Refuse to send a dimension a daemon that old would drop.
///
/// A daemon predating the split reads `sandbox` and ignores the other seven
/// keys, so `--network offline` would come back as a session with a network
/// and nothing anywhere would say so. That is the fail-open a protocol version
/// exists to catch, and the check is skipped entirely when every dimension is
/// at its default, so an all-defaults run still works against an old daemon.
///
/// The table of what could be dropped is [`settings_a_daemon_could_drop`];
/// this is the half that needs a socket.
fn check_daemon_understands(
    c: &mut Client,
    policy: &AgentPolicy,
    origin: Option<RequestOrigin>,
    scoped: bool,
    narrowed: bool,
    browser_ca: bool,
    present: bool,
) -> Result<()> {
    let needs = settings_a_daemon_could_drop(policy, origin, scoped, narrowed, browser_ca, present);
    if needs.is_empty() {
        return Ok(());
    }
    let Response::Hello { version, .. } = c.call(&Request::Hello)? else {
        // A daemon that cannot answer Hello is one this cannot reason about,
        // and guessing in the permissive direction is the whole failure mode.
        bail!("the agent runtime did not answer the protocol handshake");
    };
    let ignored: Vec<&str> = needs
        .iter()
        .filter(|(_, since)| version < *since)
        .map(|(name, _)| *name)
        .collect();
    if !ignored.is_empty() {
        bail!(
            "the running agent runtime speaks protocol {version} and would ignore {}; \
             restart it with `systemctl --user restart rime-agentd` so the setting takes \
             effect",
            ignored.join(", ")
        );
    }
    Ok(())
}

/// Every setting this invocation asked for, paired with the protocol revision
/// that first carried it.
///
/// Split out of [`check_daemon_understands`] so the mapping can be asserted
/// without a daemon: the function around it needs a live socket, and a rule
/// that can only be exercised against a running runtime is a rule nobody
/// checks. A missing entry here is silent by construction — the setting is
/// sent, the old daemon drops it, and the session runs wider than was asked
/// for — so the table is the thing worth testing.
fn settings_a_daemon_could_drop(
    policy: &AgentPolicy,
    origin: Option<RequestOrigin>,
    scoped: bool,
    narrowed: bool,
    browser_ca: bool,
    present: bool,
) -> Vec<(&'static str, u32)> {
    let moved = non_default_dimensions(policy);
    let mut needs: Vec<(&'static str, u32)> = moved
        .iter()
        .map(|(name, _)| *name)
        // `sandbox` predates the split; `connectors` postdates it by four
        // revisions and `plugins` by five — mapping either onto the version
        // the first six arrived in would tell a protocol-6 daemon it
        // understood a key it drops, which is the exact fail-open this
        // function exists for. Both are pushed below with their own revision.
        .filter(|name| !matches!(*name, "sandbox" | "connectors" | "plugins"))
        .map(|name| (name, POLICY_DIMENSIONS_VERSION))
        .collect();
    // Dimension 7, and its dropped key is a WIDENING like `--capabilities`:
    // a daemon below this writes no curated configuration at all, so
    // `--connectors none` comes back as a session holding every connector on
    // the machine.
    if policy.connectors != ConnectorPolicy::default() {
        needs.push(("--connectors", CONNECTOR_POLICY_VERSION));
    }
    // Dimension 8, one revision later again and the same kind of widening,
    // but quieter: a daemon below this writes no `enabledPlugins` block, so
    // `--plugins none` comes back as a session that loaded every plugin the
    // machine has enabled and ran each one's `SessionStart` hook before the
    // first prompt. Nothing in the transcript would say so.
    if policy.plugins != PluginPolicy::default() {
        needs.push(("--plugins", PLUGIN_POLICY_VERSION));
    }
    // A declared origin is the same failure and a worse one. A daemon that
    // predates it drops the key and records whatever it observed, which for
    // Remote Control is the local origin §7 reserves root for.
    if origin.is_some() {
        needs.push(("--origin", REQUEST_ORIGIN_VERSION));
    }
    // The worst of the three, which is why it is checked even though `system`
    // is already in `moved`. A daemon below this refuses both elevated modes
    // outright, so the failure is loud — but it also drops `ttl_ms`, and a
    // future daemon that accepted the mode while ignoring the window would
    // give a break-glass session no expiry at all. Named separately so the
    // refusal says which setting would be lost.
    if policy.needs_grant().is_some() {
        needs.push(("--system-access", SYSTEM_GRANT_VERSION));
        needs.push(("--ttl", SYSTEM_GRANT_VERSION));
    }
    // The only entry on this list whose dropped key makes the session MORE
    // privileged than was asked for: a daemon below this ignores the narrowing
    // and issues a grant over every verb. Checked separately from the two
    // above because it arrived a revision later, so a daemon can understand
    // `--system-access` and still not understand this.
    if scoped {
        needs.push(("--capabilities", SCOPED_GRANT_VERSION));
    }
    // And the second entry whose dropped key WIDENS, a revision later again
    // (P2-012). A daemon below this ignores `allow` and snapshots the
    // runtime's whole allowlist, so a capsule started to visit one host runs
    // with every destination the machine permits — and unlike `--capabilities`
    // there is no later refusal to catch it: the session starts, the browser
    // renders the page it was pointed at, and the widening is invisible
    // because nothing in the run tries the other destinations.
    //
    // Checked on `narrowed` rather than on `policy.network`, because
    // `--network allowlist` is on `moved` already and understanding that
    // dimension says nothing about understanding this key.
    if narrowed {
        needs.push(("--allow", SESSION_ALLOWLIST_VERSION));
    }
    // The one entry here whose dropped key makes the session LESS capable
    // rather than more, and it is on this table anyway (P2-012, gap 5). A
    // daemon below this ignores `trust_ca`, the capsule's browser keeps the
    // machine's trust anchors, and the intranet host's certificate is refused
    // — nothing is widened. What is lost is the ability to SAY so: Firefox
    // answers an untrusted chain by sitting on it, so the caller waits out
    // `rime browser --timeout` and is then told the capsule did not finish.
    // Every other entry is on this table to stop a silent widening; this one
    // is here to stop a silent five minutes.
    if browser_ca {
        needs.push(("--trust-ca", BROWSER_CA_VERSION));
    }
    // And back to the ordinary direction one revision later (P2-012, route B).
    // A daemon below this drops `present`, tunnels the capsule's CONNECT
    // untouched, and the capsule visits the site as nobody — which for a site
    // that serves a public page to anonymous callers is a screenshot that
    // looks right and is not logged in. Checked separately from `--trust-ca`
    // because a daemon can install a CA for a capsule's browser and know
    // nothing about terminating TLS for it.
    if present {
        needs.push(("--present", BROWSER_PRESENT_VERSION));
    }
    needs
}

fn list(all: bool, json: bool) -> Result<i32> {
    let sessions = client::sessions()?;
    let shown: Vec<&SessionInfo> = sessions
        .iter()
        .filter(|s| all || s.is_live())
        .collect();

    if json {
        println!("{}", serde_json::to_string_pretty(&shown)?);
        return Ok(0);
    }

    if shown.is_empty() {
        println!(
            "no {}sessions. start one with `rime agent run`",
            if all { "" } else { "running " }
        );
        return Ok(0);
    }

    println!(
        "{:>3}  {:<24} {:<10} {:<20} {:<22} {:<12} WHERE",
        "ID", "NAME", "AGENT", "STATE", "PROJECT", "SANDBOX"
    );
    for s in shown {
        let state = match s.exit_summary() {
            Some(summary) if !s.is_live() => summary,
            _ => s.state.to_string(),
        };
        let project = s
            .project_name
            .clone()
            .or_else(|| s.worktree.clone())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:>3}  {:<24} {:<10} {:<20} {:<22} {:<12} {}",
            s.id,
            truncate(&list_name(s), 24),
            truncate(&s.agent, 10),
            // 20 fits the longest real value, "killed by signal 15".
            truncate(&state, 20),
            truncate(&project, 22),
            s.policy.sandbox,
            short_path(&s.cwd)
        );
    }
    Ok(0)
}

/// What the NAME column shows (§1.3's order): the name a person gave the
/// session, else the agent's own terminal title, else `-` — the columns beside
/// it already are the `<agent> · <project>` a client falls back to.
fn list_name(s: &SessionInfo) -> String {
    s.name
        .clone()
        .or_else(|| s.title.clone())
        .unwrap_or_else(|| "-".to_string())
}

/// One line naming a session, in §1.3's order: the name a person gave it, the
/// agent's own title, and otherwise `<agent> · <project or directory>` — the
/// label every client showed before names existed.
fn session_label(s: &SessionInfo) -> String {
    if let Some(name) = s.name.as_deref().or(s.title.as_deref()) {
        return name.to_string();
    }
    let place = s.project_name.clone().unwrap_or_else(|| {
        Path::new(&s.cwd)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| s.cwd.clone())
    });
    format!("{} · {place}", s.agent)
}

fn attach(id: u32, replay: bool) -> Result<i32> {
    let cfg = config::Config::load();
    attach_session(id, replay, &cfg)
}

/// Take over a session's terminal until the user detaches or it ends.
fn attach_session(id: u32, replay: bool, cfg: &config::Config) -> Result<i32> {
    let size = term::stdout_window_size();
    let replay_bytes = if replay {
        rime_agent_core::session::SCROLLBACK_BYTES
    } else {
        0
    };

    let mut c = Client::connect()?;
    match c.call(&Request::Attach {
        id,
        cols: size.cols,
        rows: size.rows,
        replay: replay_bytes,
    })? {
        Response::Attached { .. } => {}
        other => bail!("unexpected reply: {other:?}"),
    }
    c.clear_timeouts();

    // Anything the daemon sent alongside the response line is already session
    // output and must be printed before the live stream.
    let prelude = c.take_buffered();
    let read_half = c.try_clone_stream()?;
    let write_half = c.into_raw()?;

    // Raw mode for the duration, restored by the guard however this exits.
    let _raw = RawMode::enter(libc::STDIN_FILENO)?;
    install_winch_forwarder(id, size);

    let detached = client::relay(read_half, write_half, &prelude, cfg.detach_byte())?;

    // Leave the cursor somewhere sane: a TUI that was mid-repaint when the user
    // detached would otherwise leave the shell prompt in the middle of a line.
    print!("\r\n");
    std::io::stdout().flush().ok();

    if detached {
        eprintln!("rime: detached from session {id} (still running — `rime agent attach {id}`)");
        return Ok(0);
    }

    match client::session(id) {
        Ok(info) => {
            if let Some(summary) = info.exit_summary() {
                eprintln!("rime: session {id} {summary}");
            }
            Ok(info.exit_code.unwrap_or(0))
        }
        Err(_) => Ok(0),
    }
}

/// Forward terminal resizes to the session on their own short-lived
/// connections, so the raw PTY stream stays byte-transparent.
fn install_winch_forwarder(id: u32, initial: WinSize) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    static RESIZED: AtomicBool = AtomicBool::new(false);

    extern "C" fn on_winch(_sig: libc::c_int) {
        // Async-signal-safe: one relaxed atomic store and nothing else.
        RESIZED.store(true, Ordering::Relaxed);
    }
    // Safe: installing a handler that only touches an atomic. Cast through a
    // function pointer rather than straight from the function item, which is
    // what `fn_to_numeric_cast_any` warns about.
    let handler = on_winch as extern "C" fn(libc::c_int);
    unsafe {
        libc::signal(libc::SIGWINCH, handler as libc::sighandler_t);
    }

    let last = Arc::new(std::sync::Mutex::new(initial));
    std::thread::Builder::new()
        .name("rime-winch".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(120));
            if !RESIZED.swap(false, Ordering::Relaxed) {
                continue;
            }
            let size = term::stdout_window_size();
            {
                let mut guard = last.lock().expect("winch lock");
                if *guard == size {
                    continue;
                }
                *guard = size;
            }
            if let Ok(mut c) = Client::connect() {
                let _ = c.request(&Request::Resize {
                    id,
                    cols: size.cols,
                    rows: size.rows,
                });
            }
        })
        .ok();
}

/// Where a session's handoff packet goes.
///
/// Inside the project, not under `$XDG_STATE_HOME`, and that is forced rather
/// than chosen: the receiving session is sandboxed, and under `--sandbox
/// project` the rest of `$HOME` is not hidden but ABSENT. A packet in the
/// runtime's own state directory would be handed to an agent that cannot open
/// it, and the failure would look like the agent ignoring instructions.
///
/// One file per (session, target), overwritten. A second handoff of the same
/// session to the same agent is a retry, and a directory filling with
/// timestamped near-duplicates is how an agent ends up reading the wrong one.
fn handoff_path(root: &Path, id: u32, to: &str) -> PathBuf {
    root.join(".rime")
        .join("handoff")
        .join(format!("session-{id}-to-{to}.md"))
}

/// The opening instruction the receiving agent gets.
///
/// It says READ THE FILE FIRST, and it says what the file is. An agent handed
/// a path with no explanation treats it as one input among many; the whole
/// point of §16 is that this is the state of the work.
///
/// Split out so it can be tested without a daemon, and so the words the next
/// agent acts on are in one place rather than inline in a request builder.
fn handoff_prompt(path: &Path) -> String {
    format!(
        "Read {} before doing anything else. It is a handoff packet: another agent was \
         working on this and stopped, and that file is everything the runtime knows about \
         where it got to. Sections that say the runtime could not supply them are gaps in \
         the tooling, not statements that there was nothing there. Continue the work it \
         describes.",
        path.display()
    )
}

/// The files that changed since the session's checkpoint.
///
/// The same comparison `rime agent diff` makes and for the same reason: tree
/// against tree, never tree against working tree, because `git diff <commit>`
/// only considers tracked paths and a file the agent CREATED is exactly what
/// the next agent needs to know about.
///
/// `Ok(None)` means there was nothing to compare against, which is a different
/// answer from `Ok(Some(vec![]))` — "nothing changed" — and the packet keeps
/// them apart.
///
/// The BASE is returned alongside the files, not just used and discarded. When
/// the session has no checkpoint of its own the comparison falls back to the
/// project's most recent one, and a list of changed files measured against a
/// base the packet never names is a number without a unit: the next agent
/// cannot tell whether `src/main.rs` changed during this session's work or
/// during somebody else's, last week.
fn handoff_changes(
    root: &Path,
    session: &SessionInfo,
) -> Result<Option<(checkpoint::Checkpoint, Vec<String>)>> {
    let base = match session.checkpoint.as_deref() {
        Some(cp) => Some(checkpoint::find(root, cp)?),
        None => checkpoint::latest(root)?,
    };
    let Some(base) = base else {
        return Ok(None);
    };
    let now = checkpoint::current_tree(root)?;
    let text = git::git(root, &["diff", "--name-only", &base.commit, &now, "--"])?;
    let files = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();
    Ok(Some((base, files)))
}

/// The privilege verbs pre-approved for the outgoing session's PROJECT.
///
/// These are the grants the incoming agent INHERITS, and that is measured
/// rather than assumed: `request::Grants::allows(project, verb)` matches on
/// the project root alone — no session, no expiry, no boot id — and a handoff
/// starts the new session in the outgoing session's `cwd`, so it is the same
/// project. Read the second function below before writing prose about either:
/// §16 has one heading for grants and this runtime has two families, whose
/// transfer semantics are opposites.
///
/// Best-effort on purpose: a runtime that cannot answer must not stop a
/// handoff, because the packet is still worth having without this section. The
/// distinction the packet needs is between "asked and there were none" and
/// "could not ask", so the failure returns `None` and the reason is recorded.
/// Collapsing those two would hand the receiving agent a failed lookup dressed
/// as a fact about its own authority.
/// The worktree row the daemon attributes the outgoing session to.
///
/// Matched on `WorktreeStatus.sessions`, which is the daemon's own
/// session-to-worktree attribution, rather than on a path comparison here.
/// `worktree::statuses` resolves a session's cwd to the DEEPEST worktree
/// containing it, so a path match written at this end would disagree with the
/// daemon for exactly the nested case that attribution exists to settle.
///
/// `None` asks for every remembered project. The session id is unique across
/// them, so it costs one wider reply and removes a slug-resolution step that
/// could fail on its own and be mistaken for "no tests recorded".
fn handoff_worktree_row(session: u32) -> Option<rime_agent_core::worktree::WorktreeStatus> {
    client::worktrees(None)
        .ok()?
        .into_iter()
        .find(|w| w.sessions.contains(&session))
}

fn handoff_project_grants(project: Option<&str>) -> Option<Vec<String>> {
    let project = project?;
    let reply = client::call(&Request::Grants).ok()?;
    match reply {
        Response::Grants { projects } => Some(projects.get(project).cloned().unwrap_or_default()),
        _ => None,
    }
}

/// The system-access grants the OUTGOING session holds, as the daemon says.
///
/// Filtered to that session, because `SystemGrant.session` is the field that
/// makes it a grant and not a standing root capability (§3.3) — and it is
/// exactly why none of these reaches the incoming agent. A grant belonging to
/// a sibling session is not this handoff's business and listing it would read
/// as inherited.
///
/// The daemon's own state word and sentence are carried rather than
/// re-derived, for the reason written on `fetch_grants`: the state depends on
/// the running kernel's boot id, so a client computing it could disagree with
/// the daemon that issued the grant.
fn handoff_system_grants(session: u32) -> Option<Vec<String>> {
    let (grants, states) = fetch_grants().ok()?;
    Some(
        grants
            .iter()
            .zip(states.iter())
            .filter(|(g, _)| g.session == session)
            .map(|(g, (state, said))| {
                // Break-glass carries no capability list because it does not
                // work through `request` at all; "-" there would read as a gap
                // in the record rather than as the point of the mode. Same
                // wording as `rime agent grants`, so the two agree.
                let covers = if g.capabilities.is_empty() {
                    "root inside the session (sudo)".to_string()
                } else {
                    g.capabilities.join(", ")
                };
                format!("`#{} {}` — {covers} ({state}: {said})", g.id, g.kind)
            })
            .collect(),
    )
}

fn handoff(id: Option<u32>, to: &str, no_start: bool, transcript_bytes: usize) -> Result<i32> {
    // The target has to be an adapter this runtime can launch, checked BEFORE
    // anything is written. A packet for an agent that does not exist is a file
    // nobody will ever read, and the error belongs in front of the user rather
    // than after a successful-looking write.
    let target = adapter::by_id(to).with_context(|| {
        format!(
            "no agent adapter named {to:?}; `rime agent adapters` lists them"
        )
    })?;

    let (dir, session) = session_context(id)?;
    let session = session.context(
        "no session to hand off. Name one with `rime agent handoff <id> --to <agent>`, or run \
         this from a project that has one",
    )?;

    let root = git::toplevel(&dir).with_context(|| {
        format!(
            "{} is not inside a git repository, so there is nowhere in the project to put the \
             packet where a sandboxed session could read it",
            dir.display()
        )
    })?;

    let mut unavailable = handoff::Handoff::structural_gaps();

    // The base the changed-file list is measured against, when it is not the
    // session's own checkpoint. Carried into the `checkpoint` section's reason
    // so the two sections cannot contradict each other.
    let mut fallback_base: Option<String> = None;

    let changed = match handoff_changes(&root, &session) {
        Ok(Some((base, files))) => {
            if session.checkpoint.is_none() {
                fallback_base = Some(format!("{} ({})", base.id, base.label));
            }
            Some(files)
        }
        Ok(None) => {
            unavailable.push(handoff::Missing::new(
                "changed files",
                "This project has no checkpoint, so there is no before-state to compare \
                 against. `rime agent run --checkpoint` is what makes this answerable.",
            ));
            None
        }
        Err(e) => {
            unavailable.push(handoff::Missing::new(
                "changed files",
                &format!("The comparison against the checkpoint failed: {e}."),
            ));
            None
        }
    };

    let transcript = match client::logs(session.id, transcript_bytes) {
        Ok(t) if !t.trim().is_empty() => Some(t),
        Ok(_) => {
            unavailable.push(handoff::Missing::new(
                "important transcript summary",
                "The session's transcript is empty.",
            ));
            None
        }
        Err(e) => {
            unavailable.push(handoff::Missing::new(
                "important transcript summary",
                &format!("The transcript could not be read: {e}."),
            ));
            None
        }
    };

    // The test state is the daemon's observation, not a suite run from here:
    // a handoff that ran somebody's tests would take minutes and change the
    // tree it is reporting on.
    let test_state = match handoff_worktree_row(session.id) {
        Some(row) => Some(handoff::describe_tests(&row.tests, row.head.as_deref())),
        None => {
            unavailable.push(handoff::Missing::new(
                "test state",
                "The runtime has a per-worktree test record, but it did not return a row \
                 for this session. That is a failed lookup and not an observation: it does \
                 not mean no suite has been run here. `rime agent worktrees` asks the same \
                 question directly.",
            ));
            None
        }
    };

    let project_grants = handoff_project_grants(session.project.as_deref());
    if project_grants.is_none() {
        unavailable.push(handoff::Missing::new(
            "project grants",
            "The runtime did not answer the project-grant query. This says nothing \
             about whether anything is pre-approved here — ask with `rime request \
             grants` before assuming either way.",
        ));
    }

    let system_grants = handoff_system_grants(session.id);
    if system_grants.is_none() {
        unavailable.push(handoff::Missing::new(
            "system grants",
            "The runtime did not answer the system-grant query, so this says nothing \
             about what the outgoing session held.",
        ));
    }

    if session.checkpoint.is_none() {
        // Two different sentences, because the two cases leave the reader in
        // different positions. With a fallback base the changed-file list
        // above is real but is measured from somewhere the session did not
        // choose; without one there is no list at all.
        unavailable.push(handoff::Missing::new(
            "checkpoint",
            &match fallback_base.as_deref() {
                Some(base) => format!(
                    "This session was not started with `--checkpoint`, so it has no \
                     before-state of its own. The changed files above are measured \
                     against the project's most recent checkpoint, `{base}`, which was \
                     taken by something else — so that list may include work this \
                     session did not do, and may omit work it did before that \
                     checkpoint.",
                ),
                None => "This session was not started with `--checkpoint`, so there is \
                     no recorded before-state of its own."
                    .to_string(),
            },
        ));
    }

    let packet = handoff::Handoff {
        version: handoff::HANDOFF_VERSION,
        from_session: session.id,
        from_agent: session.agent.clone(),
        to_agent: target.id.to_string(),
        created_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        project: session.project.clone(),
        worktree: session.worktree.clone(),
        cwd: session.cwd.clone(),
        argv: {
            let mut v = vec![session.program.clone()];
            v.extend(session.args.iter().cloned());
            v
        },
        goal: None,
        plan: None,
        changed_files: changed,
        test_state,
        transcript,
        memory_slug: None,
        checkpoint: session.checkpoint.clone(),
        project_grants,
        system_grants,
        unavailable,
    };

    // `.rime/` goes into `.git/info/exclude` and not the user's `.gitignore`,
    // through the same helper `rime agent run --worktree` uses. A handoff that
    // left an untracked file showing up in the next `git status` would be this
    // tool making a mess in somebody's repository.
    if let Some(p) = project::detect(&root) {
        project::ensure_ignored(&p).ok();
    }

    let path = handoff_path(&root, session.id, target.id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, packet.markdown())
        .with_context(|| format!("writing {}", path.display()))?;

    // stdout is the path and nothing else, so this composes. Everything a
    // person reads goes to stderr.
    println!("{}", path.display());
    let gaps = packet.unavailable.len();
    eprintln!(
        "rime: handoff packet for session {} written for {} ({} of {} fields this build \
         cannot supply are marked absent with their reason)",
        session.id,
        target.id,
        gaps,
        handoff::FIELDS.len()
    );

    if no_start {
        eprintln!("rime: --no-start, so nothing was launched. Start it yourself with:");
        eprintln!("rime:   rime agent run -a {} --cwd {}", target.id, session.cwd);
        return Ok(0);
    }

    // Started in the OUTGOING session's directory, which is its worktree when
    // it had one. `worktree:` is deliberately not passed: that would create a
    // second worktree and hand the next agent an empty one, when the whole
    // point is to continue in the tree the work is already in.
    let req = Request::Run(Box::new(RunRequest {
        agent: Some(target.id.to_string()),
        // The name a person gave the work goes with the work. It is the one
        // thing on the outgoing session that was never the agent's to choose,
        // and the Agent Center row for the continuation should read the same.
        name: session.name.clone(),
        prompt: Some(handoff_prompt(&path)),
        args: vec![],
        cwd: session.cwd.clone(),
        policy: session.policy,
        request_origin: None,
        worktree: None,
        checkpoint: false,
        ttl_ms: None,
        // A handoff carries the outgoing session's POLICY, not its grant: the
        // new session asks for whatever dimension 3 the policy names and gets
        // its own human decision. There is nothing to narrow here, and
        // inheriting the old grant's verbs would be inheriting the grant.
        capabilities: None,
        allow: None,
        // A handoff continues work in a terminal, not in a browser capsule,
        // and the outgoing session's `SessionInfo` does not carry the CA it
        // was given — so there is nothing here to inherit and nothing that
        // could be inherited by accident.
        trust_ca: None,
        present: None,
        cols: 80,
        rows: 24,
        env: vec![],
        // A handoff continues real work in the outgoing session's own tree, so
        // the incoming session is an ordinary one. `disposable: true` would run
        // it in a throwaway capsule whose writes are discarded unless
        // `copy_out` names somewhere, which is the opposite of continuing.
        disposable: false,
        copy_out: None,
        // A handoff cannot carry one, and that is not an omission. P0-014's
        // receipt is single-use and signed over *this* elevation — the
        // session, the grant kind and the window are inside the signed bytes —
        // so there is nothing here that a touch collected for the outgoing
        // session could authorise. `session.policy` is inherited, so if it
        // carries `--system-access session` from a non-local origin the new
        // session is refused for a missing factor, which is the right answer:
        // a second root session is a second thing for a human to agree to.
        second_factor: None,
    }));
    // Deliberately NOT `client::call(&req)?`. The `?` would return the error
    // up to the top-level handler, which prints it and knows nothing about the
    // packet — so the one thing the user still has, a written document and its
    // path, would go unmentioned at exactly the moment they need to be told
    // how to carry on by hand. A failed launch has two shapes, a transport
    // error and a reply that is not a session, and both leave the packet on
    // disk; they get one message.
    let started = client::call(&req);
    let refusal = match started {
        Ok(Response::Session(info)) => {
            eprintln!(
                "rime: session {} started under {} in {}",
                info.id, info.agent, info.cwd
            );
            eprintln!("rime: attach to it with `rime agent attach {}`", info.id);
            return Ok(0);
        }
        Ok(other) => format!("{other:?}"),
        Err(e) => format!("{e}"),
    };
    eprintln!(
        "rime: the packet is written at {} but the {} session did not start: {refusal}",
        path.display(),
        target.id
    );
    eprintln!(
        "rime: nothing is lost — start it yourself with `rime agent run -a {} --cwd {}` and \
         tell it to read that file first",
        target.id, session.cwd
    );
    Ok(1)
}

/// How long `rime agent input --submit` waits between the text and Enter when
/// the daemon is too old to do it itself (§1.2's fallback).
///
/// Longer than the daemon's own `SUBMIT_GAP` on purpose: the daemon's pause is
/// between two writes to one descriptor, this one is between two requests,
/// each a connection and a round trip, and the contract gives the phone's
/// fallback the same 100 ms.
const CLIENT_SUBMIT_GAP: std::time::Duration = std::time::Duration::from_millis(100);

/// The requests `rime agent input` sends, in order.
///
/// Nothing is ever appended to the text — the carriage return that means
/// Enter is a separate thing in both shapes, because a long burst ending in CR
/// is read by an agent TUI as a paste and not sent (measured against Claude
/// Code, 2026-09-30):
///
/// * a daemon that advertises `input_submit` gets ONE request with `submit`
///   set, and writes the CR itself, a `SUBMIT_GAP` after the text;
/// * an older one gets the text, and then — after [`CLIENT_SUBMIT_GAP`],
///   which the caller sleeps — a second request carrying `\r` alone. An old
///   daemon IGNORES a `submit` key it has never heard of, so sending it the
///   flag would type the text, press nothing, and let this command report
///   "sent".
///
/// CR and not LF: CR is the byte a terminal sends for Enter, which is what a
/// TUI reading its input raw treats as Enter; in cooked mode ICRNL makes the
/// two the same, so the cooked-mode tests in rime-agentd do not discriminate.
///
/// Pure, so the shape is tested without a daemon.
fn input_requests(id: u32, text: &str, submit: bool, daemon_submits: bool) -> Vec<Request> {
    let typed = Request::Input {
        id,
        data: text.to_string(),
        submit: submit && daemon_submits,
    };
    if submit && !daemon_submits {
        return vec![
            typed,
            Request::Input {
                id,
                data: "\r".to_string(),
                submit: false,
            },
        ];
    }
    vec![typed]
}

/// Whether the running daemon lists `feature` in its hello.
///
/// An answer that is not a hello — or no answer — is "no": guessing yes would
/// send a daemon a field it drops, which is the silent failure this asks to
/// avoid.
fn daemon_has(feature: &str) -> Result<bool> {
    Ok(match client::call(&Request::Hello)? {
        Response::Hello { features, .. } => features.iter().any(|f| f == feature),
        _ => false,
    })
}

fn input(id: u32, text: &str, submit: bool) -> Result<i32> {
    // Only asked when it matters: an unsubmitted input is the same request to
    // every daemon, so it costs no extra round trip.
    let daemon_submits = submit && daemon_has("input_submit")?;
    let requests = input_requests(id, text, submit, daemon_submits);
    let last = requests.len() - 1;
    for (i, req) in requests.iter().enumerate() {
        client::call(req)?;
        if i < last {
            std::thread::sleep(CLIENT_SUBMIT_GAP);
        }
    }
    // On stderr, so a script's stdout stays empty. Says whether Enter was
    // pressed, because "nothing happened" and "it is sitting in the prompt"
    // look the same from outside the session and want different next steps.
    if submit {
        eprintln!("rime: sent to session {id}");
    } else {
        eprintln!("rime: typed into session {id}, not sent; add --submit to send it");
    }
    Ok(0)
}

/// `rime agent rename <id> <name>` and `--clear` (§1.5).
///
/// Checked here with the validator the daemon uses, so a refusal names the
/// problem before a request is sent; the daemon checks again, because it is
/// the boundary. `None` — `--clear`, or a name that is only whitespace — is
/// sent as `null`, which clears.
fn rename(id: u32, name: Option<String>) -> Result<i32> {
    let name = rime_agent_core::session::session_name_opt(name.as_deref())
        .map_err(|why| anyhow!("{why}"))?;
    let info = match client::call(&Request::Rename {
        id,
        name: name.clone(),
    })? {
        Response::Session(info) => *info,
        other => bail!("unexpected reply: {other:?}"),
    };
    match info.name {
        Some(n) => eprintln!("rime: session {id} is now called \"{n}\""),
        None => eprintln!("rime: session {id} has no name now"),
    }
    Ok(0)
}

fn signal(id: u32, name: &str, past_tense: &str) -> Result<i32> {
    client::call(&Request::Signal {
        id,
        signal: name.to_string(),
    })?;
    eprintln!("rime: session {id} {past_tense}");
    Ok(0)
}

/// Where Rime Shell's screenshot keybind puts its files.
///
/// The same directory `src/scripts/screenshot.sh` writes to in rime-shell, and
/// the path is spelled here rather than asked of the shell because this command
/// has to work on a machine running any compositor, or none. The environment
/// override is what lets the suite point it at a directory of its own; it is
/// the same device `rime-disposable` uses for `RIME_DISPOSABLE_ROOT`.
fn screenshot_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("RIME_SCREENSHOT_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    rime_agent_core::paths::home().join("Pictures/Screenshots")
}

/// The most recently modified regular file in the screenshots directory.
///
/// By modification time and not by name: the name carries a timestamp, but it
/// is the timestamp of the capture rather than of the file, and a screenshot
/// edited after it was taken is still the one the user is looking at.
fn newest_screenshot() -> Result<PathBuf> {
    let dir = screenshot_dir();
    let entries = std::fs::read_dir(&dir)
        .with_context(|| format!("no screenshots to hand over: cannot read {}", dir.display()))?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let Ok(when) = meta.modified() else { continue };
        let better = match &best {
            None => true,
            Some((best_when, _)) => when > *best_when,
        };
        if better {
            best = Some((when, entry.path()));
        }
    }
    best.map(|(_, path)| path).ok_or_else(|| {
        anyhow!(
            "{} holds no screenshots yet. Press Print to take one",
            dir.display()
        )
    })
}

/// `rime agent send`.
fn send(id: u32, files: Vec<String>, last_screenshot: bool, json: bool) -> Result<i32> {
    let mut sources: Vec<PathBuf> = Vec::new();
    if last_screenshot {
        sources.push(newest_screenshot()?);
    }
    for f in &files {
        // Canonicalised here rather than in the daemon: the daemon's working
        // directory is not yours, so a relative path would name a different
        // file there — and it is refused there, so this is where a plain
        // `rime agent send 3 shot.png` has to become a path.
        sources.push(
            std::fs::canonicalize(f).with_context(|| format!("cannot hand over {f}"))?,
        );
    }
    if sources.is_empty() {
        bail!("name a file to hand over, or --last-screenshot");
    }

    let mut client = Client::connect()?;
    let mut bracketed_anywhere = false;
    for source in &sources {
        let resp = client.call(&Request::Inject {
            id,
            source: source.to_string_lossy().into_owned(),
        })?;
        let Response::Injected {
            path, bracketed, ..
        } = resp
        else {
            bail!("the runtime answered something other than an injection: {resp:?}");
        };
        bracketed_anywhere |= bracketed;
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "session": id,
                    "source": source.to_string_lossy(),
                    "path": path,
                    "bracketed": bracketed,
                })
            );
        } else {
            eprintln!("rime: {} -> session {id} as {path}", source.display());
        }
    }
    if !json {
        // Said every time, and deliberately. A user who believes the agent has
        // already been asked will wait for an answer that is not coming.
        eprintln!(
            "rime: {} typed into session {id}'s terminal and NOT entered — press Enter there{}",
            if sources.len() == 1 {
                "path".to_string()
            } else {
                format!("{} paths", sources.len())
            },
            if bracketed_anywhere {
                ". That agent reads pasted text as a paste"
            } else {
                ""
            }
        );
    }
    Ok(0)
}

fn logs(id: u32, bytes: usize) -> Result<i32> {
    let text = client::logs(id, bytes)?;
    print!("{text}");
    if !text.ends_with('\n') {
        println!();
    }
    Ok(0)
}

fn status(id: Option<u32>) -> Result<i32> {
    match id {
        Some(id) => {
            let s = client::session(id)?;
            print_session(&s);
            Ok(0)
        }
        None => {
            let cfg = config::Config::load();
            let running = Client::is_running();
            println!("runtime      {}", if running { "running" } else { "stopped" });
            println!("socket       {}", client::socket_path().display());
            println!("default      {}", cfg.default_agent);
            for (name, value) in cfg.policy().dimensions() {
                println!("{name:<12} {value}");
            }
            println!("detach key   {}", cfg.detach_key);
            if !running {
                println!();
                println!("enable it with: rime agent enable");
                return Ok(1);
            }
            let sessions = client::sessions()?;
            let live = sessions.iter().filter(|s| s.is_live()).count();
            println!("sessions     {live} running, {} recorded", sessions.len());
            Ok(0)
        }
    }
}

/// `rime agent enable` — turn the per-user runtime on for whoever runs it.
///
/// The runtime is opt-in by design: a per-user daemon holding PTYs is not
/// something every account should start. This is the single command that opts
/// in, so `rime agent` and the `not running` message can point at one thing
/// instead of a `systemctl --user` line the user has to get right — and, for
/// root, a line that does not work at all until root has a lingering user
/// instance.
fn enable() -> Result<i32> {
    if rime_agent_core::paths::uid() == 0 {
        enable_root()
    } else {
        enable_user()
    }
}

/// The ordinary case: an interactive login already has a running user instance
/// and an `$XDG_RUNTIME_DIR`, so this is exactly the documented one-liner.
fn enable_user() -> Result<i32> {
    let code = run_tool("systemctl", &["--user", "enable", "--now", "rime-agentd"], None)?;
    if code == 0 {
        println!("agent runtime enabled. `a` and `rime agent run` work now.");
    }
    Ok(code)
}

/// Root has no `systemd --user` instance unless it lingers, and no
/// `$XDG_RUNTIME_DIR` in a sudo shell, so the ordinary line fails with a bus
/// error. Give root a lingering instance, wait for its runtime directory to
/// appear, then enable the service against it.
///
/// A deliberate, user-invoked choice, not a default, and it says what it costs:
/// an agent started by a root runtime runs as root. The per-session sandbox
/// still keeps `/` read-only and masks the home, but the working directory is
/// writable, so a root agent can write as root wherever it is started. Running
/// the upstream agent (`claude`, `opencode`, …) directly as root is strictly
/// less confined than this; a normal user account is safer than either.
fn enable_root() -> Result<i32> {
    let code = run_tool("loginctl", &["enable-linger", "root"], None)?;
    if code != 0 {
        bail!("could not enable a lingering systemd user instance for root");
    }
    // enable-linger brings user@0 up asynchronously; nudge it, then wait for the
    // runtime directory where the control socket will live.
    run_tool("systemctl", &["start", "user@0.service"], None).ok();
    let runtime = Path::new("/run/user/0");
    for _ in 0..50 {
        if runtime.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if !runtime.exists() {
        bail!("root's systemd user instance did not come up (/run/user/0 is absent)");
    }
    // XDG_RUNTIME_DIR is how `systemctl --user` finds the bus; a sudo shell does
    // not set it, so point it at root's own instance explicitly.
    let code = run_tool(
        "systemctl",
        &["--user", "enable", "--now", "rime-agentd"],
        Some(("XDG_RUNTIME_DIR", "/run/user/0")),
    )?;
    if code == 0 {
        println!("agent runtime enabled for root.");
        println!();
        println!("note: agents started here run AS ROOT. The per-session sandbox keeps /");
        println!("read-only and masks the home, but the working directory is writable, so a");
        println!("root agent can write as root wherever you start it. A normal user account");
        println!("is safer. To undo:  systemctl --user disable --now rime-agentd");
        println!("                    loginctl disable-linger root");
    }
    Ok(code)
}

/// Spawn a helper, inheriting stdio, optionally with one extra environment
/// variable. Returns its exit code.
fn run_tool(program: &str, args: &[&str], env: Option<(&str, &str)>) -> Result<i32> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some((k, v)) = env {
        cmd.env(k, v);
    }
    let status = cmd.status().with_context(|| format!("running {program}"))?;
    Ok(status.code().unwrap_or(1))
}

fn print_session(s: &SessionInfo) {
    println!("session      {}", s.id);
    // First, in §1.3's order, so the line a person scans for is at the top.
    // The name and the title get lines of their own when both are there: one
    // is what a person called the work, the other what the agent calls it, and
    // hiding either would make the other look like the only one.
    println!("label        {}", session_label(s));
    if let Some(name) = &s.name {
        println!("name         {name}");
    }
    if let Some(title) = &s.title {
        println!("title        {title}");
    }
    println!("agent        {}", s.agent);
    println!(
        "command      {} {}",
        s.program,
        s.args.join(" ")
    );
    println!("state        {}", s.state);
    if let Some(detail) = &s.detail {
        println!("detail       {detail}");
    }
    for (name, value) in s.policy.dimensions() {
        println!("{name:<12} {value}");
    }
    // Right under the dimensions, because it is the one that says what
    // `network allowlist` MEANS for this session. A capsule started to visit
    // one host and a session that can reach everything the machine permits
    // print the same dimension line, and this is the line that tells them
    // apart (P2-012).
    if let Some(lines) = &s.allowlist {
        println!("destinations {}", lines.join(", "));
    }
    println!("cwd          {}", s.cwd);
    if let Some(p) = &s.project_name {
        println!("project      {p}");
    }
    if let Some(w) = &s.worktree {
        println!("worktree     {w}");
    }
    if let Some(c) = &s.checkpoint {
        println!("checkpoint   {c}");
    }
    // Said in full, not as a one-word flag. A user looking at this session
    // needs to know two things a capsule name does not convey on its own:
    // the working tree is a COPY, and it is deleted when the session ends.
    if let Some(capsule) = &s.capsule {
        println!("capsule      {capsule} (disposable)");
        println!(
            "             the working tree here is a COPY, and this environment is \
             deleted when the session ends"
        );
    }
    println!("pid          {}", s.pid);
    println!("terminal     {}x{}", s.cols, s.rows);
    println!("attached     {}", s.attached);
    // Only when there have been some. A line of "files 0" on every session
    // would be noise on every session that has never been handed one, which
    // is almost all of them.
    if s.injected > 0 {
        println!("files sent   {}", s.injected);
    }
    if let Some(summary) = s.exit_summary() {
        println!("outcome      {summary}");
    }
}

fn default_agent(agent: Option<String>) -> Result<i32> {
    let (mut cfg, notes) = config::load_reporting();
    for note in &notes {
        eprintln!("rime: {note}");
    }
    let Some(agent) = agent else {
        println!("{}", cfg.default_agent);
        return Ok(0);
    };
    if adapter::by_id(&agent).is_none() {
        bail!(
            "no agent named {agent:?}. known agents: {}",
            adapter::ids().join(", ")
        );
    }
    cfg.default_agent = agent.clone();
    cfg.save()?;
    println!("default agent is now {agent}");
    Ok(0)
}

/// `rime agent lock [--agents …] [--remote …] [--root-grants …]`.
///
/// With no flags it reports, and the report leads with what the screen is
/// actually doing — read from logind, which is the half of this that did not
/// exist until rime-shell started calling `SetLockedHint`. A settings page
/// that could not say whether the mechanism was working would be the same
/// switch-with-no-wire this policy used to be.
fn lock_policy(
    agents: Option<bool>,
    remote: Option<bool>,
    root_grants: Option<bool>,
) -> Result<i32> {
    use rime_agent_core::lock::{LockObserver, Loginctl};

    let (mut cfg, notes) = config::load_reporting();
    for note in &notes {
        eprintln!("rime: {note}");
    }

    if agents.is_none() && remote.is_none() && root_grants.is_none() {
        let state = Loginctl::new().observe();
        println!("screen                   {state}");
        println!(
            "ordinary agents          {}",
            if cfg.lock.agents_continue {
                "continue"
            } else {
                "hold"
            }
        );
        println!(
            "Remote Control           {}",
            if cfg.lock.remote_control_continues {
                "continue"
            } else {
                "hold"
            }
        );
        println!(
            "short-lived root grants  {}",
            if cfg.lock.revoke_root_grants {
                "revoke"
            } else {
                "keep"
            }
        );
        if !cfg.lock.remote_control_continues {
            println!(
                "\n§7 lets Remote Control past a lock only when it is configured to:\n  \
                 rime agent lock --remote continue"
            );
        }
        return Ok(0);
    }

    if let Some(v) = agents {
        cfg.lock.agents_continue = v;
    }
    if let Some(v) = remote {
        cfg.lock.remote_control_continues = v;
    }
    if let Some(v) = root_grants {
        cfg.lock.revoke_root_grants = v;
    }
    cfg.save()?;

    let p = cfg.lock;
    println!(
        "on a locked screen: ordinary agents {}, Remote Control {}, short-lived root grants {}",
        if p.agents_continue { "continue" } else { "are held" },
        if p.remote_control_continues {
            "continues"
        } else {
            "is held"
        },
        if p.revoke_root_grants {
            "are revoked"
        } else {
            "are kept"
        }
    );
    Ok(0)
}

/// `rime agent allow [DESTINATION] [--remove]`.
///
/// The rule is parsed before it is stored, so a line that would empty the
/// whole allowlist on the next load is refused here, with the reason, rather
/// than written and then silently dropped.
fn allow(destination: Option<String>, remove: bool) -> Result<i32> {
    use rime_agent_core::destination::Rule;

    let (mut cfg, notes) = config::load_reporting();
    for note in &notes {
        eprintln!("rime: {note}");
    }

    let Some(destination) = destination else {
        if cfg.network_allow.is_empty() {
            println!(
                "nothing is allowed, so `--network allowlist` has nothing to reach.\n\
                 add a destination with `rime agent allow <host>`"
            );
            return Ok(0);
        }
        for line in cfg.allowlist().lines() {
            println!("{line}");
        }
        return Ok(0);
    };

    // Normalised through the parser, so `API.Example.COM.` and
    // `api.example.com` cannot both end up in the file as separate rules that
    // mean one thing.
    let rule = Rule::parse(&destination).map_err(|e| anyhow::anyhow!("{e}"))?;
    let line = rule.as_line();

    if remove {
        let before = cfg.network_allow.len();
        cfg.network_allow
            .retain(|d| Rule::parse(d).map(|r| r.as_line()) != Ok(line.clone()));
        if cfg.network_allow.len() == before {
            bail!("{line} is not on the allowlist");
        }
        cfg.save()?;
        println!("{line} removed");
        return Ok(0);
    }

    if cfg.allowlist().lines().contains(&line) {
        println!("{line} is already allowed");
        return Ok(0);
    }
    cfg.network_allow.push(line.clone());
    cfg.save()?;
    println!("{line} allowed for `--network allowlist` sessions started from now on");
    Ok(0)
}

// ── system-access grants (§4.4, §4.5) ───────────────────────────────────────

/// The escape that makes break-glass unmissable, and the one that undoes it.
///
/// §3.4 asks for a "prominent red Agent Center indicator". The Agent Center is
/// Rime Shell's, and it reads `SessionInfo.grant`; this is the same statement
/// in the place a terminal user is actually looking. Colour is a strong claim
/// to make about somebody's terminal, so it is made only when stderr is a
/// terminal and `NO_COLOR` is unset — and every message that uses it still
/// says the words, so a pipe, a log file and a screen reader all carry the
/// same information.
const RED: &str = "\x1b[1;31m";
const OFF: &str = "\x1b[0m";

fn red(text: &str) -> String {
    let colour = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    if colour {
        format!("{RED}{text}{OFF}")
    } else {
        text.to_string()
    }
}

/// The grants, and for each one the state word and the sentence.
///
/// Parallel vectors because that is the wire shape: the daemon computes the
/// state, since it depends on the running kernel's boot id and a client
/// deriving it could get a different answer from the daemon that issued the
/// grant.
type GrantListing = (Vec<SystemGrant>, Vec<(String, String)>);

/// Ask the daemon for its grants.
fn fetch_grants() -> Result<GrantListing> {
    let mut c = Client::connect()?;
    match c.call(&Request::SystemGrants)? {
        Response::SystemGrants { grants, states } => Ok((grants, states)),
        Response::Error { message, .. } => bail!(message),
        other => bail!("unexpected reply: {other:?}"),
    }
}

/// `rime agent worktrees` — the four questions, per worktree (§P1-036).
///
/// The daemon answers, not this process, and that is deliberate: it holds the
/// record of which sessions are where and of the test runs it watched go past,
/// and it resolves the project slug to a path itself so that no caller names a
/// directory for it to run git in.
fn worktrees(project: Option<String>, json: bool) -> Result<i32> {
    let rows = client::worktrees(project)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(0);
    }

    if rows.is_empty() {
        println!(
            "no worktrees. A project is remembered when an agent runs in it, \
             and `rime agent run --worktree <name>` gives that agent a \
             worktree of its own"
        );
        return Ok(0);
    }

    println!(
        "{:<22} {:<26} {:>9}  {:<11} {:<11} READY",
        "WORKTREE", "BRANCH", "DIFF", "CONFLICTS", "TESTS"
    );
    for w in &rows {
        let branch = w.branch.as_deref().unwrap_or("(detached)");
        let diff = if w.diff.files == 0 {
            // A worktree with no committed delta and a dirty tree has
            // something in it; "-" would read as "nothing here".
            if w.dirty {
                "dirty".to_string()
            } else {
                "-".to_string()
            }
        } else {
            format!("{}f +{}/-{}", w.diff.files, w.diff.insertions, w.diff.deletions)
        };
        let conflicts = match &w.conflicts {
            ConflictState::Clean => "clean".to_string(),
            ConflictState::Conflicted { paths } => format!("{} file(s)", paths.len()),
            ConflictState::Unknown { .. } => "unknown".to_string(),
            ConflictState::NotApplicable => "-".to_string(),
        };
        let ready = if w.ready.ready_to_propose {
            "yes".to_string()
        } else {
            // The first blocker, because it is the one to fix first and the
            // whole list is in `--json`.
            w.ready
                .blockers
                .first()
                .cloned()
                .unwrap_or_else(|| "no".to_string())
        };
        println!(
            "{:<22} {:<26} {:>9}  {:<11} {:<11} {}",
            clip(&w.name, 22),
            clip(branch, 26),
            diff,
            conflicts,
            w.tests.as_str(),
            ready
        );
    }

    // Said once, at the bottom, rather than implied by a column heading that
    // cannot carry it: this is the last test run the runtime SAW, and the
    // runtime does not run anybody's suite to answer a status query.
    if rows.iter().any(|w| w.tests != TestState::Unobserved) {
        println!(
            "\nTESTS is the last run Rime observed going past, not a fresh result — \
             the tree may have moved since."
        );
    } else {
        println!(
            "\nTESTS is 'unobserved' until a test run happens inside a managed session. \
             Rime never runs a suite itself to answer this."
        );
    }
    Ok(0)
}

/// Trim a cell to fit, with an ellipsis rather than a hard cut.
fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let keep: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{keep}…")
}

/// How the human proved they were there, for the listing's own column.
///
/// §7 has two columns and P0-014 gave them two different authentications: a
/// password at this keyboard, or a touch on an enrolled security key from a
/// remote origin the owner opted in for. An audit listing that showed neither
/// would answer "on whose authority" the same way for both, so the field the
/// daemon already records is surfaced here rather than only in `--json`.
///
/// The stored value is either a polkit action id or `security-key:<label>`;
/// the prefix is what keeps a key labelled after an action id from reading as
/// a password (see `grants.rs`, `Authenticated::recorded_as`).
fn authorised_by(grant: &SystemGrant) -> String {
    match grant.authenticated_by.strip_prefix("security-key:") {
        Some(label) => format!("key {label}"),
        None => "password".to_string(),
    }
}

fn grants(active_only: bool, json: bool) -> Result<i32> {
    let (grants, states) = fetch_grants()?;
    let rows: Vec<_> = grants
        .iter()
        .zip(states.iter())
        .filter(|(_, (state, _))| !active_only || state == "active")
        .collect();

    if json {
        let out: Vec<serde_json::Value> = rows
            .iter()
            .map(|(g, (state, said))| {
                let mut v = serde_json::to_value(g).unwrap_or_default();
                if let Some(o) = v.as_object_mut() {
                    o.insert("state".into(), state.clone().into());
                    o.insert("summary".into(), said.clone().into());
                }
                v
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(0);
    }

    if rows.is_empty() {
        println!(
            "no {}system-access grants. `rime agent run --system-access session` or \
             `--unsafe-everything --ttl 15m` asks for one. Locally that takes a password; \
             from a remote origin it takes a touch on an enrolled security key, and only \
             if the session was started with `--origin-policy remote`",
            if active_only { "active " } else { "" }
        );
        return Ok(0);
    }

    println!(
        "{:>3}  {:<13} {:>7}  {:<21} {:<8} {:<20} WHAT IT COVERS",
        "ID", "MODE", "SESSION", "STATE", "WINDOW", "AUTHORISED BY"
    );
    for (g, (state, _)) in &rows {
        let window = rime_agent_core::grant::format_ms(g.expires_ms.saturating_sub(g.issued_ms));
        let covers = if g.capabilities.is_empty() {
            // Break-glass carries none, and saying "-" would read as a gap in
            // the record rather than as the point of the mode.
            "root inside the session (sudo)".to_string()
        } else {
            g.capabilities.join(", ")
        };
        let line = format!(
            "{:>3}  {:<13} {:>7}  {:<21} {:<8} {:<20} {}",
            g.id,
            g.kind,
            g.session,
            state,
            window,
            authorised_by(g),
            covers
        );
        // Only an active break-glass grant is red. A grant that has ended is
        // history, and colouring history teaches people to ignore the colour.
        if state == "active" && g.kind == GrantKind::BreakGlass {
            println!("{}", red(&line));
        } else {
            println!("{line}");
        }
    }
    // The sentence, under the table, for anything that is not simply active:
    // "ended at the reboot" and "ended with the runtime" are the answers §3.4
    // asks the machine to be able to give, and a STATE column alone gives
    // them too quietly.
    for (_, (state, said)) in &rows {
        if state != "active" {
            println!("  {said}");
        }
    }
    if rows.iter().any(|(_, (s, _))| s == "active") {
        println!("\nrevoke one with `rime agent revoke-grant <id>`");
    }
    Ok(0)
}

fn revoke_grant(id: u32) -> Result<i32> {
    let mut c = Client::connect()?;
    match c.call(&Request::RevokeSystemGrant { id })? {
        Response::SystemGrants { states, .. } => {
            for (_, said) in states {
                println!("{said}");
            }
            Ok(0)
        }
        Response::Error { message, .. } => bail!(message),
        other => bail!("unexpected reply: {other:?}"),
    }
}

fn renew_grant(id: u32, ttl_ms: u64) -> Result<i32> {
    let mut c = Client::connect()?;
    match c.call(&Request::RenewSystemGrant { id, ttl_ms, second_factor: None })? {
        Response::SystemGrants { states, .. } => {
            for (_, said) in states {
                println!("{said}");
            }
            Ok(0)
        }
        Response::Error { message, .. } => bail!(message),
        other => bail!("unexpected reply: {other:?}"),
    }
}

fn adapters() -> Result<i32> {
    let cfg = config::Config::load();
    println!("{:<10} {:<16} {:<12} PROGRAM", "ID", "AGENT", "INSTALLED");
    for a in adapter::ADAPTERS {
        let installed = if a.program.is_empty() {
            "n/a".to_string()
        } else if which(a.program).is_some() {
            "yes".to_string()
        } else {
            "no".to_string()
        };
        let marker = if a.id == cfg.default_agent { "*" } else { " " };
        println!(
            "{marker}{:<9} {:<16} {:<12} {}",
            a.id,
            a.display,
            installed,
            if a.program.is_empty() {
                "(caller supplies)"
            } else {
                a.program
            }
        );
    }
    Ok(0)
}

// ── agent profiles (§5) ─────────────────────────────────────────────────────

fn profile_cmd(cmd: ProfileCmd) -> Result<i32> {
    match cmd {
        ProfileCmd::List { json } => profile_list(json),
        ProfileCmd::Inspect { agent, json } => profile_inspect(agent, json),
        ProfileCmd::Doctor { agent } => profile_doctor(agent),
        ProfileCmd::Export { agent, to, force } => profile_export(agent, to, force),
        ProfileCmd::Import {
            agent,
            from,
            dry_run,
        } => profile_import(agent, from, dry_run),
    }
}

/// Resolve the agent name a profile verb was given.
///
/// An adapter without a profile is refused by name rather than by an empty
/// table: `rime agent profile doctor codex` reporting nothing wrong would be a
/// worse answer than saying Rime does not know what a codex profile is.
fn profile_for(agent: Option<String>) -> Result<&'static profile::Profile> {
    let id = agent.unwrap_or_else(|| config::Config::load().default_agent);
    profile::by_agent(&id).with_context(|| {
        let known: Vec<&str> = profile::PROFILES.iter().map(|p| p.agent).collect();
        format!(
            "Rime has no profile description for {id}; it knows: {}",
            known.join(", ")
        )
    })
}

fn profile_list(json: bool) -> Result<i32> {
    let home = rime_agent_core::paths::home();
    if json {
        let all: Vec<_> = profile::PROFILES
            .iter()
            .map(|p| profile::summary(p, &home))
            .collect();
        println!("{}", serde_json::to_string_pretty(&all)?);
        return Ok(0);
    }
    println!(
        "{:<10} {:<24} {:<10} REUSABLE / LOCAL / SECRET",
        "AGENT", "ROOT", "INSTALLED"
    );
    for p in profile::PROFILES {
        let s = profile::summary(p, &home);
        let n = |k: &str| s.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        println!(
            "{:<10} {:<24} {:<10} {} / {} / {}",
            p.agent,
            s["root"].as_str().unwrap_or_default(),
            if s["installed"] == true { "yes" } else { "no" },
            n("reusable") + n("mixed"),
            n("machine_local"),
            n("secret"),
        );
    }
    Ok(0)
}

fn profile_inspect(agent: Option<String>, json: bool) -> Result<i32> {
    let p = profile_for(agent)?;
    let home = rime_agent_core::paths::home();
    let found = profile::inspect(p, &home);
    if json {
        let rows: Vec<_> = found
            .iter()
            .map(|f| {
                serde_json::json!({
                    "path": f.path,
                    "class": f.class.as_str(),
                    "mount": match f.mount {
                        profile::Mount::ReadOnly => "read-only",
                        profile::Mount::Writable => "writable",
                    },
                    "role": f.role.as_str(),
                    "present": f.present,
                    "files": f.files,
                    "what": f.what,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(0);
    }
    println!("{} — {}", p.display, profile::display_home(&p.root_dir(&home), &home));
    println!();
    println!(
        "{:<42} {:<14} {:<10} {:<14} FILES",
        "PATH", "CLASS", "MOUNT", "ROLE"
    );
    for f in &found {
        println!(
            "{:<42} {:<14} {:<10} {:<14} {}",
            f.path,
            f.class.as_str(),
            match f.mount {
                profile::Mount::ReadOnly => "read-only",
                profile::Mount::Writable => "writable",
            },
            f.role.as_str(),
            if f.present {
                f.files.to_string()
            } else {
                "-".to_string()
            },
        );
    }
    println!();
    println!("reusable and mixed are what `rime agent profile export` carries.");
    println!("mixed files are carried in part: see `rime agent profile export --help`.");
    Ok(0)
}

fn profile_doctor(agent: Option<String>) -> Result<i32> {
    let p = profile_for(agent)?;
    let home = rime_agent_core::paths::home();
    let report = profile::doctor(p, &home);

    println!(
        "{} — {}",
        p.display,
        profile::display_home(&p.root_dir(&home), &home)
    );
    for section in &report.sections {
        println!();
        println!("{}", section.title);
        for line in &section.lines {
            println!("  {line}");
        }
    }
    println!();
    if report.problems.is_empty() {
        println!("no problems found");
        return Ok(0);
    }
    println!(
        "{} problem{}",
        report.problems.len(),
        if report.problems.len() == 1 { "" } else { "s" }
    );
    for problem in &report.problems {
        println!("  {problem}");
    }
    Ok(1)
}

fn profile_export(agent: Option<String>, to: Option<PathBuf>, force: bool) -> Result<i32> {
    let p = profile_for(agent)?;
    let home = rime_agent_core::paths::home();
    let dest = to.unwrap_or_else(|| PathBuf::from(format!("{}-profile", p.agent)));

    if !force {
        let occupied = std::fs::read_dir(&dest)
            .map(|mut it| it.next().is_some())
            .unwrap_or(false);
        if occupied {
            bail!(
                "{} already has files in it; pass --force to write into it anyway",
                dest.display()
            );
        }
    }

    let report = profile::export(p, &home, &dest)
        .with_context(|| format!("exporting the {} profile", p.agent))?;
    println!(
        "wrote {} file{} ({}) to {}",
        report.files,
        if report.files == 1 { "" } else { "s" },
        human_bytes(report.bytes),
        report.dest.display()
    );
    for (path, gave_up) in &report.edited {
        println!("  redacted  {}  ({gave_up})", path.display());
    }
    for (path, class) in &report.excluded {
        println!(
            "  left      {}  ({class})",
            profile::display_home(path, &home)
        );
    }
    Ok(0)
}

fn profile_import(agent: Option<String>, from: PathBuf, dry_run: bool) -> Result<i32> {
    let p = profile_for(agent)?;
    let home = rime_agent_core::paths::home();
    let changes = if dry_run {
        profile::plan_import(p, &from, &home)
    } else {
        profile::import(p, &from, &home)
    }
    .with_context(|| format!("importing {}", from.display()))?;

    let writes = changes
        .iter()
        .filter(|c| !matches!(c, profile::Change::Unfilled(_, _)))
        .count();
    if writes == 0 {
        println!("nothing to change: this machine already has what the bundle carries");
    } else {
        println!(
            "{} {} file{}",
            if dry_run { "would change" } else { "changed" },
            writes,
            if writes == 1 { "" } else { "s" }
        );
    }
    for change in &changes {
        match change {
            profile::Change::Unfilled(_, _) => {}
            other => println!("  {other}"),
        }
    }
    let unfilled: Vec<&profile::Change> = changes
        .iter()
        .filter(|c| matches!(c, profile::Change::Unfilled(_, _)))
        .collect();
    if !unfilled.is_empty() {
        println!();
        println!("set these yourself — the export carried the names, not the values:");
        for change in unfilled {
            println!("  {change}");
        }
    }
    Ok(0)
}

/// Bytes as a person reads them. Binary units, because that is what `du -h`
/// and every file manager on this machine show.
fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn which(program: &str) -> Option<PathBuf> {
    // A name containing a slash is a path, not something to look up: $TERMINAL
    // is frequently set to /usr/bin/something, and searching PATH for a string
    // with a slash in it never matches.
    if program.contains('/') {
        let p = PathBuf::from(program);
        return p.is_file().then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(program))
        .find(|p| p.is_file())
}

fn event(state: String, session: Option<u32>, detail: Option<String>) -> Result<i32> {
    let id = session
        .or_else(client::current_session)
        .context("no session id: pass --session, or run this inside an agent session")?;
    if AgentState::parse(&state).is_none() {
        bail!(
            "unknown state {state:?}; use working, waiting_for_user, \
             permission_request, complete or failed"
        );
    }
    client::publish_event(id, &state, detail)?;
    Ok(0)
}

/// `rime agent hook <event>` — the bridge from Claude's own lifecycle.
///
/// Returns an exit code directly rather than a `Result`, because there is only
/// one: **0**, always. Claude reads a non-zero exit from a `PreToolUse` as a
/// reason to stop the tool call, and 2 as a reason to block it and hand the
/// agent the stderr — so a daemon that is down, a payload that will not parse
/// or an event name from a newer settings file must not become a refusal. Every
/// one of those is a hook that said nothing, and a hook that says nothing
/// leaves the sandbox doing the enforcing, which is where §6.2 puts it anyway.
///
/// Diagnostics go to stderr, which Claude shows in its transcript in verbose
/// mode and otherwise discards. stdout carries the deny document and nothing
/// else — anything else there would be read as a malformed hook response.
fn hook(event: &str) -> i32 {
    let Some(parsed) = HookEvent::parse(event) else {
        eprintln!("rime agent hook: unknown event {event:?}; ignoring");
        return 0;
    };
    let payload = read_payload();

    // The policy point first, because its answer decides whether the tool runs
    // at all and it is the only part with a deadline the agent can feel.
    if parsed.is_policy_point() {
        if let Some(doc) = policy_decision(&payload) {
            println!("{doc}");
        }
    }

    let Some(id) = client::current_session() else {
        // Not inside a managed session: a user's own `~/.claude/settings.json`
        // pointing here, or a nested agent. Nothing to report to.
        return 0;
    };
    let observation = hook_core::observe(parsed, &payload);
    if let Err(e) = client::publish_hook(id, &observation) {
        eprintln!("rime agent hook: {parsed} not published: {e:#}");
    }
    0
}

/// `rime agent statusline` — the wrapper around the user's own status line.
///
/// Returns an exit code directly rather than a `Result`, for the same reason
/// [`hook`] does: there is only one, and it is 0. Claude treats a non-zero
/// exit from a status line as an error, and every failure here — no daemon, a
/// payload that will not parse, a user command that is not installed — must
/// leave the prompt looking exactly as it did.
///
/// ## Why the user's own command runs FIRST
///
/// It is what the person sees. The publish is a round trip to a Unix socket
/// and the daemon may be busy or gone; doing it first would put its latency in
/// front of every status-line refresh, and a daemon that hangs would blank the
/// line rather than merely lose a measurement.
fn statusline() -> i32 {
    use std::io::{Read, Write};

    // Bounded for the reason `read_payload` is: it is a document the agent's
    // own state ends up inside, and this process has no reason to hold a large
    // one. Generous, because the status-line payload carries the whole
    // workspace description and a `pr` block.
    const MAX_PAYLOAD: u64 = 1024 * 1024;
    let mut raw = Vec::new();
    let _ = std::io::stdin()
        .take(MAX_PAYLOAD)
        .read_to_end(&mut raw);

    // 1. The user's line, unchanged. `project_dir` rather than `current_dir`,
    //    because that is the root a project's own `.claude/settings.json` sits
    //    at and the daemon read the same three sources in the same order when
    //    it wrote the overlay.
    let doc: serde_json::Value = serde_json::from_slice(&raw).unwrap_or(serde_json::Value::Null);
    let project = doc
        .get("workspace")
        .and_then(|w| w.get("project_dir"))
        .and_then(|v| v.as_str())
        .map(PathBuf::from);
    let user = statusline_core::user_status_line(&paths::home(), project.as_deref());
    if let Some(command) = user.as_ref().and_then(|u| u.command.as_deref()) {
        if let Some(out) = statusline_core::chain(command, &raw) {
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(&out);
            let _ = stdout.flush();
        }
    }

    // 2. The measurement. Nothing below this line can change what was printed.
    let Some(id) = client::current_session() else {
        return 0;
    };
    let telemetry = statusline_core::parse(&doc, now_secs());
    if telemetry.is_empty() {
        // Nothing worth a round trip. A status line runs once a minute per
        // session, and publishing an empty record would rewrite every
        // session's file on a timer to say nothing.
        return 0;
    }
    if let Err(e) = client::publish_telemetry(id, &telemetry) {
        // Including a daemon that predates this request and answered with a
        // parse error. The status line has already printed; this is a
        // measurement that did not arrive.
        eprintln!("rime agent statusline: not published: {e:#}");
    }
    0
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Read the payload Claude writes to a hook's stdin.
///
/// An unreadable or unparseable document is an empty payload, not an error:
/// the event still happened and the state it implies does not depend on the
/// detail. Bounded, because this is a document an agent's own tool arguments
/// end up inside and the process has no reason to hold a large one.
fn read_payload() -> hook_core::Payload {
    use std::io::Read;
    const MAX_PAYLOAD: u64 = 1024 * 1024;
    let mut text = String::new();
    if std::io::stdin()
        .take(MAX_PAYLOAD)
        .read_to_string(&mut text)
        .is_err()
    {
        return hook_core::Payload::default();
    }
    serde_json::from_str(&text).unwrap_or_default()
}

/// Ask the daemon whether this tool call is one the sandbox would refuse, and
/// render the answer as the document Claude expects.
///
/// `None` for anything but a refusal, including every failure. See [`hook`].
fn policy_decision(payload: &hook_core::Payload) -> Option<String> {
    let id = client::current_session()?;
    let tool_name = payload.tool_name.clone()?;
    let reply = client::call(&Request::ToolCheck {
        id,
        tool_name,
        tool_input: payload.tool_input.clone(),
    });
    let deny = match reply {
        Ok(Response::ToolDecision { deny }) => deny?,
        // An older daemon that does not know this request, or no daemon at all.
        Ok(_) | Err(_) => return None,
    };
    Some(hook_core::deny_document(&deny).to_string())
}

/// `rime agent origin <origin>` — narrow the calling session's own origin.
///
/// The session is resolved by the daemon from the connection, so this cannot
/// be pointed at another session and does not read `$RIME_AGENT_SESSION`.
/// Handing a session id to a verb that changes a permission-relevant property
/// is exactly what the privilege verbs avoid, and for the same reason.
fn declare_origin(origin: &str, actor: Option<String>) -> Result<i32> {
    let wanted = parse_request_origin(origin).map_err(|e| anyhow::anyhow!("{e}"))?;
    match client::call(&Request::DeclareOrigin {
        origin: wanted.as_str().to_string(),
        actor,
    })? {
        Response::Session(info) => {
            eprintln!(
                "rime: session {} is now {} (declared)",
                info.id,
                info.request_origin
                    .map(|o| o.to_string())
                    .unwrap_or_else(|| "unrecorded".into())
            );
            Ok(0)
        }
        // The connection case. Said plainly rather than reported as a success,
        // because from a shell it IS a no-op: `client::call` opens a
        // connection, sends one request and closes it, so the latch it just
        // set is gone before the next command runs. A program that holds the
        // socket is the only caller this helps, and a user typing it deserves
        // to be told that rather than left believing something was recorded.
        Response::Ok => {
            eprintln!(
                "rime: this connection is now {wanted}, and it closes when this command exits — \
                 a declaration on a connection lasts only as long as the connection"
            );
            Ok(0)
        }
        Response::Error { message, .. } => {
            eprintln!("rime: {message}");
            Ok(1)
        }
        other => bail!("unexpected reply: {other:?}"),
    }
}

fn remove(id: u32) -> Result<i32> {
    client::call(&Request::Remove { id })?;
    eprintln!("rime: session {id} removed");
    Ok(0)
}

fn prune() -> Result<i32> {
    client::call(&Request::Prune)?;
    eprintln!("rime: finished sessions removed");
    Ok(0)
}

// ── checkpoints ─────────────────────────────────────────────────────────────

/// The directory a checkpoint verb operates on, and the session it came from.
fn session_context(id: Option<u32>) -> Result<(PathBuf, Option<SessionInfo>)> {
    let cwd = std::env::current_dir().context("reading the current directory")?;
    match id {
        Some(id) => {
            let s = client::session(id)?;
            Ok((PathBuf::from(&s.cwd), Some(s)))
        }
        None => {
            // The most recent session whose working directory is this project.
            let root = git::toplevel(&cwd);
            let latest = client::sessions()
                .unwrap_or_default()
                .into_iter()
                .rfind(|s| match &root {
                    Some(r) => Path::new(&s.cwd).starts_with(r),
                    None => false,
                });
            Ok((cwd, latest))
        }
    }
}

fn make_checkpoint(label: Option<String>) -> Result<i32> {
    let cwd = std::env::current_dir()?;
    let label = label.unwrap_or_else(|| "manual".to_string());
    let cp = checkpoint::create(&cwd, &label, None)?;
    println!("checkpoint {} ({})", cp.id, cp.short_commit());
    println!("restore with: rime agent undo --checkpoint {}", cp.id);
    Ok(0)
}

fn diff(id: Option<u32>, stat: bool) -> Result<i32> {
    let (dir, session) = session_context(id)?;
    let root = git::toplevel(&dir)
        .with_context(|| format!("{} is not inside a git repository", dir.display()))?;

    // Prefer the session's own checkpoint; fall back to the newest one.
    let base = match session.as_ref().and_then(|s| s.checkpoint.clone()) {
        Some(cp_id) => Some(checkpoint::find(&root, &cp_id)?),
        None => checkpoint::latest(&root)?,
    };

    let Some(base) = base else {
        eprintln!(
            "rime: no checkpoint for this project — showing uncommitted changes instead.\n\
             rime: run agents with `--checkpoint` to get a precise before-and-after."
        );
        return run_git(&root, &["diff", "--"]);
    };

    eprintln!(
        "rime: diff against checkpoint {} ({})",
        base.id, base.label
    );

    // Diff tree-against-tree, not tree-against-working-tree. `git diff <commit>`
    // only considers tracked paths, so a file the agent created would be
    // missing from the diff of what the agent did — which is precisely the
    // question being asked.
    let now = checkpoint::current_tree(&root)?;
    if stat {
        run_git(&root, &["diff", "--stat", &base.commit, &now, "--"])
    } else {
        run_git(&root, &["diff", &base.commit, &now, "--"])
    }
}

fn run_git(dir: &Path, args: &[&str]) -> Result<i32> {
    let status = Command::new("git")
        .current_dir(dir)
        .args(args)
        .status()
        .context("running git")?;
    Ok(status.code().unwrap_or(1))
}

fn undo(id: Option<u32>, explicit: Option<String>, yes: bool) -> Result<i32> {
    let (dir, session) = session_context(id)?;
    let root = git::toplevel(&dir)
        .with_context(|| format!("{} is not inside a git repository", dir.display()))?;

    let target = match explicit {
        Some(cp_id) => checkpoint::find(&root, &cp_id)?,
        None => match session.as_ref().and_then(|s| s.checkpoint.clone()) {
            Some(cp_id) => checkpoint::find(&root, &cp_id)?,
            None => checkpoint::latest(&root)?.context(
                "no checkpoint for this project.\n\
                 run agents with `rime agent run --checkpoint`, or capture one now with \
                 `rime agent checkpoint`",
            )?,
        },
    };

    if !yes {
        eprintln!(
            "rime: restore {} to checkpoint {} ({}) taken {}?",
            root.display(),
            target.id,
            target.label,
            format_age(target.created)
        );
        eprintln!("rime: uncommitted work since then will be replaced. A safety checkpoint is taken first.");
        if !confirm()? {
            eprintln!("rime: nothing changed");
            return Ok(1);
        }
    }

    let report = checkpoint::restore(&root, &target)?;
    println!("restored to checkpoint {}", report.restored.id);
    println!("safety checkpoint {} — `rime agent undo --checkpoint {}` puts it back",
        report.safety.id, report.safety.id);
    if !report.removed.is_empty() {
        println!(
            "removed {} file(s) created after the checkpoint:",
            report.removed.len()
        );
        for f in report.removed.iter().take(20) {
            println!("  - {f}");
        }
        if report.removed.len() > 20 {
            println!("  … and {} more", report.removed.len() - 20);
        }
    }
    if report.head_moved {
        println!(
            "HEAD moved back to {}",
            &report.restored.head.clone().unwrap_or_default()[..12.min(
                report.restored.head.as_ref().map(|h| h.len()).unwrap_or(0)
            )]
        );
    }
    if !report.packages.is_empty() {
        println!();
        println!("packages changed since the checkpoint (not undone automatically):");
        for p in &report.packages.added {
            println!("  + {p}");
        }
        for p in &report.packages.removed {
            println!("  - {p}");
        }
        if let Some(cmd) = report.packages.undo_command() {
            println!("to remove the added ones: {cmd}");
        }
    }
    Ok(0)
}

/// Ask for a yes/no on the terminal. A non-interactive caller must pass
/// `--yes`; assuming consent from a script would be how somebody loses work.
fn confirm() -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("not a terminal; pass --yes to confirm non-interactively");
    }
    eprint!("rime: type 'yes' to continue: ");
    std::io::stderr().flush().ok();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(answer.trim().eq_ignore_ascii_case("yes"))
}

// ── project verbs ───────────────────────────────────────────────────────────

/// `rime project identity` — §36, P2-013.
///
/// Read as the invoking account against its own project, so the answer is the
/// one `rime-secretd` would get: the same `ProjectConfig` reader, the same
/// `O_NOFOLLOW` walk, the same owner check. A file that cannot be read is an
/// error here too, and never "this project binds nothing".
fn project_identity(json: bool) -> Result<i32> {
    use rime_secret_core::identity::Identities;

    let root = current_project()?;
    let uid = unsafe { libc::getuid() };
    let name = std::env::var("USER").unwrap_or_else(|_| format!("uid {uid}"));
    let identities = Identities::read_or_unbound(std::path::Path::new(&root.root), uid, &name)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let report = identities.report();

    if json {
        let rows: Vec<serde_json::Value> = report
            .values()
            .map(|bound| {
                serde_json::json!({
                    "identity": bound.kind.as_str(),
                    "binds": bound.binds,
                    "enforced": bound.enforced,
                    "enforcedBy": bound.enforced_by,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({ "project": root.root, "identities": rows })
        );
        return Ok(0);
    }

    println!("{}\n", root.root);
    for bound in report.values() {
        match &bound.binds {
            Some(binds) => println!(
                "  {:<11} {binds}{}",
                bound.kind.as_str(),
                if bound.enforced {
                    ""
                } else {
                    "   (NOT ENFORCED)"
                }
            ),
            None => println!("  {:<11} not bound", bound.kind.as_str()),
        }
        println!("              checked by: {}", bound.enforced_by);
    }
    Ok(0)
}

pub fn project_cmd(cmd: ProjectCmd) -> i32 {
    let result = match cmd {
        ProjectCmd::List { json } => project_list(json),
        ProjectCmd::Info => project_info(),
        ProjectCmd::Worktrees => project_worktrees(),
        ProjectCmd::Checkpoints => project_checkpoints(),
        ProjectCmd::Remove {
            name,
            keep_branch,
            destroy_preview,
        } => project_remove(name, !keep_branch, destroy_preview),
        ProjectCmd::Forget { slug } => {
            project::forget(&slug).map(|()| {
                println!("forgot {slug}");
                0
            })
        }
        ProjectCmd::Identity { json } => project_identity(json),
        ProjectCmd::Switch { name } => project_switch(name),
        ProjectCmd::Env { name, clear } => project_env(name, clear),
        ProjectCmd::Layout { cmd } => match cmd {
            LayoutCmd::Save => layout_save(),
            LayoutCmd::Show { json } => layout_show(json),
            LayoutCmd::Restore { dry_run } => layout_restore(dry_run),
            LayoutCmd::Forget => layout_forget(),
            LayoutCmd::Templates => layout_templates(),
            LayoutCmd::Open { template, mux, agents, dry_run } => {
                layout_open(template, mux, agents, dry_run)
            }
        },
    };
    report(result)
}

// ── project layouts (§6) ────────────────────────────────────────────────────

/// Where the compositor adapter lives. A fixed path, like the sandbox's bwrap:
/// resolving it through `PATH` would let a shadowing script decide what "the
/// windows of this project" means.
const WINDOW_ADAPTER: &str = rime_paths::RIME_PROJECT_WINDOWS;

fn window_adapter() -> String {
    // Overridable for development only, and named so it is obvious in a process
    // list. The image installs the real one.
    std::env::var("RIME_WINDOW_ADAPTER").unwrap_or_else(|_| WINDOW_ADAPTER.to_string())
}

fn layout_save() -> Result<i32> {
    let p = current_project()?;
    let adapter = window_adapter();
    let out = Command::new(&adapter)
        .arg("list")
        .output()
        .with_context(|| format!("running {adapter} list"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        bail!(
            "cannot enumerate windows: {}",
            if err.is_empty() { "the compositor has no window query".into() } else { err }
        );
    }
    let reports: Vec<layout::WindowReport> = serde_json::from_slice(&out.stdout)
        .context("parsing the window list")?;

    let children = layout::child_map();
    let captured = layout::capture(&reports, Path::new(&p.root), &children);
    if captured.is_empty() {
        // Not an error, and not silently overwriting the previous layout with
        // nothing: a capture that found no windows is far more likely to mean
        // "the adapter reported no pids" than "this project genuinely has no
        // windows open right now".
        println!(
            "no windows are working inside {} — nothing saved, and the previous \n\
             layout (if any) is untouched",
            p.name
        );
        return Ok(1);
    }
    layout::save(&p.slug, &captured)?;
    // Register the project too. Saving a layout is a strong statement that this
    // is somewhere you work, and without it the project is absent from
    // `rime project list` — which makes `rime project switch <name>` unable to
    // find it, i.e. the §6 feature this layout exists for does not work from
    // anywhere but inside the project.
    project::remember(&p)?;
    println!(
        "saved {} window(s) across workspace(s) {} for {}",
        captured.entries.len(),
        captured.workspaces().join(", "),
        p.name
    );
    Ok(0)
}

fn layout_show(json: bool) -> Result<i32> {
    let p = current_project()?;
    let Some(l) = layout::load(&p.slug) else {
        if json {
            println!("null");
        } else {
            println!("no layout saved for {} — capture one with `rime project layout save`", p.name);
        }
        return Ok(0);
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&l)?);
        return Ok(0);
    }
    if let Some(t) = &l.template {
        println!(
            "terminal template: {t} in {} — reopen with `rime project layout open`",
            l.mux.as_deref().unwrap_or("tmux")
        );
        if l.entries.is_empty() {
            return Ok(0);
        }
        println!();
    }
    println!("{:<4} {:<14} {:<10} COMMAND", "WS", "APP", "KIND");
    for e in &l.entries {
        println!(
            "{:<4} {:<14} {:<10} {}",
            if e.workspace.is_empty() { "-" } else { &e.workspace },
            e.app_id,
            if e.terminal { "terminal" } else { "app" },
            e.argv.join(" ")
        );
    }
    Ok(0)
}

fn layout_restore(dry_run: bool) -> Result<i32> {
    let p = current_project()?;
    let Some(l) = layout::load(&p.slug) else {
        println!("no layout saved for {}", p.name);
        return Ok(1);
    };
    if l.entries.is_empty() {
        // A record can hold a terminal template and no captured windows, which
        // is what `layout open` alone leaves behind. Saying "started 0 windows"
        // there would read as a failure rather than as nothing having been
        // captured yet.
        println!(
            "no windows are saved for {} — `rime project layout save` captures them \n\
             while they are open. Its terminal template opens with \
             `rime project layout open`.",
            p.name
        );
        return Ok(1);
    }

    let term = layout::choose_terminal(
        std::env::var("TERMINAL").ok().as_deref(),
        |name| which(name).is_some(),
    );
    if term.is_none() {
        eprintln!(
            "rime: no terminal emulator found; terminal windows will be restored \n\
             with the command they were originally started by"
        );
    }
    let term = term.unwrap_or_default();

    let mut started = 0;
    let mut failed = 0;
    for e in &l.entries {
        let argv = layout::restore_argv(e, &term);
        if argv.is_empty() {
            continue;
        }
        if dry_run {
            println!("would run (ws {}): {}", e.workspace, argv.join(" "));
            started += 1;
            continue;
        }
        // No shell, ever. A layout file is a list of argv vectors that this
        // executes, so it is executed as a vector — nothing in a stored entry
        // can be a shell metacharacter because nothing parses it as one.
        let spawned = Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(&e.cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match spawned {
            Ok(_) => started += 1,
            Err(err) => {
                eprintln!("rime: cannot start {}: {err}", argv[0]);
                failed += 1;
            }
        }
    }

    if dry_run {
        println!("{started} window(s) would be restored");
        return Ok(0);
    }
    println!("started {started} window(s){}", if failed > 0 { format!(", {failed} failed") } else { String::new() });
    // Placement is best-effort and deliberately not fatal. A window has to
    // exist before it can be moved, and it does not exist until its process
    // has mapped a surface — which is asynchronous and unbounded. Restoring
    // the windows is the valuable part; getting them onto the right workspaces
    // is a nicety that would otherwise hold the command open for seconds
    // guessing at startup times.
    let workspaces = l.workspaces();
    if !workspaces.is_empty() {
        println!(
            "workspaces in this layout: {} — windows open where the compositor \n\
             puts them; run `rime project layout show` to see the intended split",
            workspaces.join(", ")
        );
    }
    if failed > 0 { Ok(1) } else { Ok(0) }
}

/// Go to a project's workspace.
///
/// The workspace comes from the saved layout, not from anything live: a project
/// does not own a workspace, it merely has windows that were on one. Which
/// means this needs a layout, and says so rather than guessing at the current
/// workspace.
///
/// Where a layout spans several workspaces, the one with the most windows wins.
/// That is a choice, not an obvious truth — the alternative is the first one
/// captured — and the most-populated one is what a person means by "where the
/// project is".
fn project_switch(name: Option<String>) -> Result<i32> {
    let p = match name {
        Some(n) => project::list()
            .into_iter()
            .find(|p| p.name == n || p.slug == n)
            .with_context(|| format!("no known project called {n:?}"))?,
        None => current_project()?,
    };

    let Some(l) = layout::load(&p.slug) else {
        bail!(
            "no layout saved for {}, so there is nothing recording which \n\
             workspace it lives on. Capture one with `rime project layout save` \n\
             while its windows are open.",
            p.name
        );
    };

    // Count windows per workspace, then take the most populated. BTreeMap so
    // ties break on the workspace name rather than on hash order — a command
    // that sends you somewhere different each time it is run is worse than one
    // that sends you somewhere arguable.
    let mut counts: std::collections::BTreeMap<&str, usize> =
        std::collections::BTreeMap::new();
    for e in &l.entries {
        if !e.workspace.is_empty() {
            *counts.entry(e.workspace.as_str()).or_default() += 1;
        }
    }
    let Some((workspace, count)) = counts.iter().max_by_key(|(_, n)| **n).map(|(w, n)| (*w, *n))
    else {
        bail!(
            "the saved layout for {} records no workspace. Either no windows were \n\
             captured yet (`rime project layout save`), or the compositor it was \n\
             captured under does not report one (labwc does not).",
            p.name
        );
    };

    let adapter = window_adapter();
    let status = Command::new(&adapter)
        .args(["workspace", workspace])
        .status()
        .with_context(|| format!("running {adapter} workspace {workspace}"))?;
    if !status.success() {
        bail!(
            "could not switch to workspace {workspace}: this compositor has no \n\
             workspace-switch verb (labwc exposes no IPC at all)"
        );
    }
    println!(
        "{} — workspace {} ({} of {} window(s))",
        p.name,
        workspace,
        count,
        l.entries.len()
    );
    Ok(0)
}

fn layout_forget() -> Result<i32> {
    let p = current_project()?;
    layout::forget(&p.slug)?;
    println!("discarded the saved layout for {}", p.name);
    Ok(0)
}

// ── terminal layout templates ───────────────────────────────────────────────
//
// The multiplexer half of a project's layout. `layout save` captures the
// desktop windows a project has open; this opens the SHAPE its terminal work
// takes, from a named template. Same command tree, same record, same `forget`.
//
// Every agent pane runs `rime agent attach` or `rime agent run` — the daemon
// owns each agent's PTY and the multiplexer is a viewport onto sessions it
// owns, never a host for them. The reasoning is in `rime_agent_core::mux`.

/// Where the multiplexer adapter lives. A fixed path, like the window adapter
/// and the sandbox's bwrap: resolving it through `PATH` would let a shadowing
/// script decide what "open my project layout" runs.
const MUX_ADAPTER: &str = rime_paths::RIME_MUX;

fn mux_adapter() -> String {
    std::env::var("RIME_MUX_ADAPTER").unwrap_or_else(|_| MUX_ADAPTER.to_string())
}

fn layout_templates() -> Result<i32> {
    println!("{:<10} {:<16} OPENS", "NAME", "ARRANGEMENT");
    for t in mux::TEMPLATES {
        println!(
            "{:<10} {:<16} {}{}",
            t.name,
            t.arrangement.as_str(),
            t.summary,
            if t.repeats_agent { "  (--agents N)" } else { "" }
        );
    }
    Ok(0)
}

/// This project's live session ids, most recent first.
///
/// Most recent first because a template with one agent pane should land on the
/// agent you were last talking to. A daemon that is not running is not an
/// error here: it means there is nothing to attach to, so every agent pane
/// starts a session instead — and `rime agent run` will report the daemon
/// being down in the pane, which is where somebody can act on it.
fn live_project_sessions(root: &str) -> Vec<u32> {
    let mut mine: Vec<&SessionInfo> = Vec::new();
    let sessions = client::sessions().unwrap_or_default();
    for s in &sessions {
        if s.is_live() && s.project.as_deref() == Some(root) {
            mine.push(s);
        }
    }
    mine.sort_by(|a, b| b.started.cmp(&a.started).then(b.id.cmp(&a.id)));
    mine.iter().map(|s| s.id).collect()
}

fn layout_open(
    template: Option<String>,
    requested_mux: Option<String>,
    agents: usize,
    dry_run: bool,
) -> Result<i32> {
    let p = current_project()?;
    let stored = layout::load(&p.slug);

    // No argument reopens what this project was last opened as, so the second
    // time is just `rime project layout open`.
    let name = template
        .or_else(|| stored.as_ref().and_then(|l| l.template.clone()))
        .unwrap_or_else(|| "dev".to_string());
    let Some(t) = mux::template(&name) else {
        bail!(
            "no template called {name} — `rime project layout templates` lists them"
        );
    };

    // `APEX_MUX` is the preference's name before the rename; a profile that
    // still exports it keeps choosing the same multiplexer.
    let preferred = requested_mux
        .or_else(|| std::env::var("RIME_MUX").ok().filter(|m| !m.is_empty()))
        .or_else(|| std::env::var("APEX_MUX").ok())  // rime-rename: keep (the preference's name before the rename)
        .filter(|m| !m.is_empty());
    let backend = mux::choose_backend(preferred.as_deref(), |n| which(n).is_some())
        .map_err(|e| anyhow!(e))?;

    let editor = mux::choose_editor(
        std::env::var("VISUAL").ok().as_deref(),
        std::env::var("EDITOR").ok().as_deref(),
        |n| which(n).is_some(),
    );
    if editor.is_none() {
        eprintln!(
            "rime: no editor found; the editor pane will be a shell. \n\
             set $VISUAL, or install one of: {}",
            mux::EDITOR_CANDIDATES.join(", ")
        );
    }

    let live = live_project_sessions(&p.root);
    let plan = mux::build(
        t,
        &p.name,
        Path::new(&p.root),
        editor.as_deref(),
        &live,
        agents.max(1),
    );

    let adapter = mux_adapter();
    let existing = Command::new(&adapter)
        .args(["has", &backend, &plan.session])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if dry_run {
        println!(
            "{} in {}: {} ({})",
            plan.session,
            backend,
            if existing { "already open — would attach to it" } else { "would be built" },
            plan.arrangement
        );
        for pane in &plan.panes {
            println!(
                "  {:<10} {}",
                pane.title,
                if pane.argv.is_empty() { "<shell>".to_string() } else { pane.argv.join(" ") }
            );
        }
        return Ok(0);
    }

    // The plan goes to the adapter as a FILE and not on stdin: opening ends by
    // handing the terminal to tmux or zellij, so stdin has to still be the
    // terminal. It lands beside the layout record, 0700, and is left there —
    // it is also the honest answer to "what did that last open actually run".
    let text = mux::encode(&plan).map_err(|e| anyhow!(e))?;
    let plan_path = layout::layout_path(&p.slug).with_extension("plan");
    let dir = plan_path.parent().context("layout path has no parent")?;
    rime_agent_core::paths::ensure_private_dir(dir)?;
    std::fs::write(&plan_path, text.as_bytes())
        .with_context(|| format!("writing {}", plan_path.display()))?;

    // Remember the template on the project's ONE layout record, so reopening
    // needs no argument and `layout show` reports both halves.
    let mut record = stored.unwrap_or_default();
    record.template = Some(t.name.to_string());
    record.mux = Some(backend.clone());
    layout::save(&p.slug, &record)?;
    project::remember(&p)?;

    if existing {
        println!("{} is already open — attaching", plan.session);
    } else {
        println!(
            "opening {} in {}: {}",
            plan.session,
            backend,
            plan.panes
                .iter()
                .map(|p| p.title.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let status = Command::new(&adapter)
        .args([
            "open",
            &backend,
            &plan.session,
            plan.arrangement,
            &plan_path.to_string_lossy(),
        ])
        .status()
        .with_context(|| format!("running {adapter} open"))?;
    Ok(status.code().unwrap_or(1))
}

fn current_project() -> Result<project::Project> {
    let cwd = std::env::current_dir()?;
    project::detect(&cwd)
        .with_context(|| format!("{} is not inside a git repository", cwd.display()))
}

fn project_list(json: bool) -> Result<i32> {
    let projects = project::list();
    if json {
        println!("{}", serde_json::to_string_pretty(&projects)?);
        return Ok(0);
    }
    if projects.is_empty() {
        println!("no projects yet — they are recorded the first time an agent runs in one");
        return Ok(0);
    }
    println!("{:<24} {:<20} PATH", "NAME", "TOOLCHAINS");
    for p in projects {
        println!(
            "{:<24} {:<20} {}",
            truncate(&p.name, 24),
            truncate(&p.languages.join(","), 20),
            short_path(&p.root)
        );
    }
    Ok(0)
}

fn project_info() -> Result<i32> {
    let p = current_project()?;
    println!("name         {}", p.name);
    println!("root         {}", p.root);
    println!("slug         {}", p.slug);
    println!(
        "toolchains   {}",
        if p.languages.is_empty() {
            "-".to_string()
        } else {
            p.languages.join(", ")
        }
    );
    println!(
        "capsule      {}",
        p.capsule.clone().unwrap_or_else(|| "-".to_string())
    );
    if let Some(branch) = git::current_branch(Path::new(&p.root)) {
        println!("branch       {branch}");
    }
    let worktrees = project::worktrees(&p).unwrap_or_default();
    println!("worktrees    {}", worktrees.iter().filter(|w| w.is_agent).count());
    println!(
        "checkpoints  {}",
        checkpoint::list(Path::new(&p.root)).unwrap_or_default().len()
    );
    let sessions = client::sessions().unwrap_or_default();
    let mine = sessions
        .iter()
        .filter(|s| s.is_live() && Path::new(&s.cwd).starts_with(&p.root))
        .count();
    println!("sessions     {mine} running");
    Ok(0)
}

/// `rime project env [CAPSULE|--clear]` — §8's binding, from the project side.
///
/// Deliberately does not create anything. A capsule is hundreds of megabytes
/// and belongs to the user's decision; this records which one their work
/// belongs in and says how to make it if it does not exist yet.
fn project_env(name: Option<String>, clear: bool) -> Result<i32> {
    let p = current_project()?;

    if clear {
        project::bind_capsule(&p, None)?;
        println!("{}: no capsule (the capsule itself is untouched)", p.name);
        return Ok(0);
    }

    let Some(name) = name else {
        match &p.capsule {
            Some(c) => {
                println!("{}: {c}", p.name);
                // Naming the capsule is not the same as it existing: a
                // binding survives `rime env rm`, and a stale one that only
                // shows up when a command fails is worse than one reported
                // here.
                if !capsule_exists(c) {
                    println!(
                        "note: no capsule called '{c}' on this machine — \
                         rime env create {c}"
                    );
                }
            }
            None => {
                println!("{}: no capsule", p.name);
                if let Some(alias) = project::suggested_capsule(&p.languages) {
                    println!(
                        "this looks like a {} project; a capsule keeps its toolchain off the host:\n  \
                         rime env create {alias}\n  \
                         rime project env {alias}",
                        p.languages.join("/"),
                    );
                }
            }
        }
        return Ok(0);
    };

    project::bind_capsule(&p, Some(&name))?;
    println!("{}: {name}", p.name);
    if !capsule_exists(&name) {
        println!("note: it does not exist yet — rime env create {name}");
    }
    Ok(0)
}

/// Does `rime env` know this capsule?
///
/// A hint, so it fails open: a machine whose capsule engine is missing or
/// broken must still be able to record a binding. Reported as "exists" when
/// the answer cannot be obtained, because printing "no such capsule" for a
/// capsule that is right there is the more confusing wrong answer.
fn capsule_exists(name: &str) -> bool {
    match Command::new(ops::ENV_ENGINE)
        .args(["info", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(status) => status.success(),
        Err(_) => true,
    }
}

fn project_worktrees() -> Result<i32> {
    let p = current_project()?;
    let worktrees = project::worktrees(&p)?;
    println!("{:<24} {:<28} PATH", "NAME", "BRANCH");
    for w in worktrees {
        println!(
            "{:<24} {:<28} {}",
            truncate(&w.name, 24),
            truncate(w.branch.as_deref().unwrap_or("(detached)"), 28),
            short_path(&w.path.to_string_lossy())
        );
    }
    Ok(0)
}

fn project_checkpoints() -> Result<i32> {
    let p = current_project()?;
    let list = checkpoint::list(Path::new(&p.root))?;
    if list.is_empty() {
        println!("no checkpoints — capture one with `rime agent checkpoint`");
        return Ok(0);
    }
    println!("{:<24} {:<14} {:<12} LABEL", "ID", "COMMIT", "AGE");
    for cp in list {
        println!(
            "{:<24} {:<14} {:<12} {}",
            cp.id,
            cp.short_commit(),
            format_age(cp.created),
            cp.label
        );
    }
    Ok(0)
}

/// Remove an agent worktree, and — on request — what it owns at Cloudflare.
///
/// ## The order is the whole of it
///
/// The cloud half happens FIRST, and the worktree is removed only if it
/// succeeded. `Provider::bind` resolves a name against `rime.toml` inside the
/// project root, and for a worktree that root is the worktree's own directory —
/// so once the directory is gone, no brokered operation can resolve anything
/// for it and the preview is unreachable through the broker. A build that
/// removed the checkout first would leave a hostname up with nothing left that
/// knows how to take it down.
///
/// The same reasoning is why a partial failure stops: a worktree removed after
/// half its resources went is a worktree nobody can finish cleaning up.
///
/// **What this cannot cover:** `git worktree remove` run by hand, which is a
/// thing people do. There is no hook in git for it, so the honest statement is
/// that this verb cleans up and that one does not — and `rime cf preview plan`
/// still works from the project root afterwards, because the trail outlives the
/// directory.
fn project_remove(name: String, delete_branch: bool, destroy_preview: bool) -> Result<i32> {
    let p = current_project()?;
    if destroy_preview {
        let root = p.worktree_path(&name);
        let Some(root) = root.to_str() else {
            bail!("{} is not a path this can name to the secret service", root.display());
        };
        let plan = crate::cloudflare::preview::plan(root)?;
        crate::cloudflare::preview::print(&plan);
        println!();
        let failed = crate::cloudflare::preview::destroy(&plan)?;
        if failed > 0 {
            eprintln!(
                "rime project: {failed} of this worktree's Cloudflare resources \
                 could not be destroyed, so the worktree has been LEFT IN PLACE. \
                 Removing it now would leave them up with nothing that knows how \
                 to take them down."
            );
            return Ok(1);
        }
        let left = plan
            .items
            .iter()
            .filter(|i| i.disposal.leaves_something())
            .count();
        if left > 0 {
            println!(
                "{left} thing{} in the plan above {} not something this build can \
                 destroy. The worktree is being removed anyway; the list stays \
                 true.",
                if left == 1 { "" } else { "s" },
                if left == 1 { "is" } else { "are" }
            );
        }
    }
    project::remove_worktree(&p, &name, delete_branch)?;
    println!(
        "removed worktree {name}{}",
        if delete_branch { " and its branch" } else { "" }
    );
    Ok(0)
}

// ── formatting ──────────────────────────────────────────────────────────────

fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let keep = width.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

/// Replace the home prefix with `~`, the way every other listing does.
fn short_path(path: &str) -> String {
    let home = rime_agent_core::paths::home();
    let home = home.to_string_lossy();
    if !home.is_empty() && path.starts_with(home.as_ref()) {
        return format!("~{}", &path[home.len()..]);
    }
    path.to_string()
}

/// A coarse age, which is all a listing needs.
fn format_age(unix_secs: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let delta = now.saturating_sub(unix_secs);
    match delta {
        0..=59 => format!("{delta}s ago"),
        60..=3599 => format!("{}m ago", delta / 60),
        3600..=86_399 => format!("{}h ago", delta / 3600),
        _ => format!("{}d ago", delta / 86_400),
    }
}

// ── security keys (§7's remote elevation, P0-014) ───────────────────────────

/// Enrol a security key.
///
/// Writes the store directly rather than going through the daemon, and that is
/// a decision rather than a shortcut: the file is under the owner's own
/// `XDG_STATE_HOME`, the owner is the only party whose enrolment means
/// anything, and a protocol verb for it would be a way for a *session* to ask
/// the daemon to trust a new key. There is deliberately no such way.
///
/// The one thing lost by not going through the daemon: a running daemon that
/// verifies an assertion writes the same file back to record a signature
/// counter, so an enrolment racing that write can lose one of the two. The
/// consequence is a counter that reads low or a key that has to be enrolled
/// again — never a key trusted that the owner did not enrol, because both
/// writers only ever write what they were given. Not solved here; a lock
/// belongs beside the store, and it is not what P0-014 is about.
fn key_add(label: &str, rp_id: &str, from: Option<&Path>) -> Result<i32> {
    let printed = match from {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?,
        None => {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
                .context("reading the credential from standard input")?;
            text
        }
    };

    let credential = webauthn::Credential::parse_fido2_cred(label, rp_id, &printed, rime_agent_core::request::now_ms())
        .map_err(|e| anyhow!("{e}"))?;
    let mut store = webauthn::CredentialStore::load();
    store.add(credential).map_err(|e| anyhow!("{e}"))?;
    store.save().map_err(|e| anyhow!("{e}"))?;

    println!("rime: enrolled {label:?} for relying party {rp_id:?}.");
    println!("      {}", webauthn::store_path().display());
    if store.len() == 1 {
        println!();
        println!("      A remote elevation now has a key to ask. It still needs the owner to");
        println!("      allow one: `rime agent run --origin-policy remote ...`.");
    }
    Ok(0)
}

/// Every enrolled key.
///
/// The listing `AssertionError::UnknownCredential` tells the operator to run,
/// which is why it exists: an error naming a command that does not exist is
/// worse than one that names nothing.
fn key_list() -> Result<i32> {
    let store = webauthn::CredentialStore::load();
    if store.is_empty() {
        println!("rime: no security key is enrolled.");
        println!("      `rime agent key add --label <name> --rp-id <id> --from <file>`");
        return Ok(0);
    }
    for c in &store.credentials {
        println!(
            "{:<20} rp={:<28} counter={}",
            c.label, c.rp_id, c.counter
        );
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_listing_says_whether_a_password_or_a_key_authorised_each_grant() {
        // §7's two columns, as an auditor reads them. The daemon records the
        // distinction; this is the half that shows it to a human, and without
        // it `rime agent grants` answers "on whose authority" identically for
        // a password typed here and a touch collected over a network.
        fn grant(authenticated_by: &str) -> SystemGrant {
            SystemGrant {
                id: 1,
                kind: GrantKind::SystemAccess,
                session: 7,
                agent: "claude".into(),
                project: None,
                capabilities: Vec::new(),
                issued_ms: 0,
                expires_ms: 900_000,
                boot_id: "b".into(),
                request_origin: RequestOrigin::LocalTerminal,
                authenticated_by: authenticated_by.into(),
                closed: None,
            }
        }
        assert_eq!(authorised_by(&grant("org.rimeos.agent.system-access")), "password");
        assert_eq!(authorised_by(&grant("org.rimeos.agent.break-glass")), "password");
        assert_eq!(authorised_by(&grant("security-key:yubikey 5c")), "key yubikey 5c");
        // The label cannot borrow the password wording: the prefix is checked,
        // not merely searched for.
        assert_eq!(
            authorised_by(&grant("security-key:org.rimeos.agent.break-glass")),
            "key org.rimeos.agent.break-glass"
        );
    }

    #[test]
    fn the_handoff_prompt_names_the_packet_and_says_to_read_it_first() {
        // The one part of the handoff that the integration tests cannot see:
        // it is only sent when a session actually launches, and launching one
        // needs an installed agent. So the words the next agent acts on are
        // asserted here.
        //
        // All three claims matter. An agent handed a bare path treats it as
        // one input among many, when the point of §16 is that the file is the
        // state of the work. And it must say that an unsupplied section is a
        // gap in the tooling: without that, an agent reads "no plan" as "there
        // was no plan" and starts over.
        let p = handoff_prompt(Path::new("/p/.rime/handoff/session-4-to-codex.md"));
        assert!(
            p.contains("/p/.rime/handoff/session-4-to-codex.md"),
            "the prompt does not name the packet: {p}"
        );
        assert!(
            p.contains("before doing anything else"),
            "the prompt does not put the packet first: {p}"
        );
        assert!(
            p.contains("not statements that there was nothing there"),
            "the prompt does not warn that an absent section is a tooling gap: {p}"
        );
    }

    #[test]
    fn the_packet_path_is_per_session_and_per_target() {
        // One file per (session, target), overwritten on a retry. A directory
        // filling with timestamped near-duplicates is how an agent ends up
        // reading the wrong handoff, and a path that ignored the target would
        // have the second handoff of a session overwrite the first.
        let root = Path::new("/p");
        let a = handoff_path(root, 4, "codex");
        assert_eq!(
            a,
            Path::new("/p/.rime/handoff/session-4-to-codex.md"),
            "the packet path moved; a sandboxed session reads it by this path"
        );
        assert_ne!(a, handoff_path(root, 5, "codex"), "two sessions collided");
        assert_ne!(a, handoff_path(root, 4, "opencode"), "two targets collided");
        // Inside the project. The reason is in `handoff_path`'s own comment:
        // under `--sandbox project` the rest of $HOME is absent, not hidden.
        assert!(a.starts_with(root), "the packet left the project: {a:?}");
    }

    /// A `run` invocation with nothing set, so a test can turn on exactly the
    /// one flag it is about.
    fn run_args() -> RunArgs {
        RunArgs {
            prompt: None,
            agent: None,
            name: None,
            sandbox: None,
            native: None,
            agent_bypass: false,
            system_access: None,
            secrets: None,
            network: None,
            origin_policy: None,
            connectors: None,
            plugins: None,
            origin: None,
            unsafe_everything: false,
            ttl: None,
            capabilities: None,
            allow: None,
            trust_ca: None,
        present: None,
            worktree: None,
            checkpoint: false,
            cwd: None,
            disposable: false,
            copy_out: None,
            detach: false,
            args: Vec::new(),
            host: None,
            remote_path: None,
            allow_dirty: false,
        }
    }

    /// `$RIME_AGENT_NAME` is process-wide and these cases run in threads of
    /// one process: the two that set it are serialised on this, the shape
    /// `gaming.rs` and `blueprint.rs` already use.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn only_input(reqs: &[Request]) -> Vec<(String, bool)> {
        reqs.iter()
            .map(|r| match r {
                Request::Input { data, submit, .. } => (data.clone(), *submit),
                other => panic!("not an input: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn input_without_submit_adds_nothing_at_all() {
        // The default has to be inert. A newline appended "helpfully" here is
        // the whole difference between text waiting in a prompt and an agent
        // acting on words that may have come from a speech-to-text hook.
        for daemon_submits in [true, false] {
            assert_eq!(
                only_input(&input_requests(4, "run the tests", false, daemon_submits)),
                vec![("run the tests".to_string(), false)]
            );
        }
        assert_eq!(only_input(&input_requests(4, "", false, true)), vec![(String::new(), false)]);
        // Text that already ends in a newline is passed through untouched:
        // trimming it would be this function deciding, which is the caller's
        // job in both directions.
        assert_eq!(
            only_input(&input_requests(4, "two lines\n", false, true)),
            vec![("two lines\n".to_string(), false)]
        );
    }

    #[test]
    fn input_with_submit_uses_the_flag_and_never_glues_enter_to_the_text() {
        // §1.2. A daemon that can: one request, the text bare, the flag set.
        // The CR the old code appended is exactly the byte an agent read as
        // part of a paste.
        assert_eq!(
            only_input(&input_requests(4, "run the tests", true, true)),
            vec![("run the tests".to_string(), true)]
        );
        // A daemon that cannot: it would drop the flag in silence, so Enter
        // goes as a request of its own, and still never on the end of the
        // text. CR, not LF, and exactly one.
        assert_eq!(
            only_input(&input_requests(4, "run the tests", true, false)),
            vec![("run the tests".to_string(), false), ("\r".to_string(), false)]
        );
        for daemon_submits in [true, false] {
            for (data, _) in only_input(&input_requests(4, "x", true, daemon_submits)) {
                assert!(data == "\r" || !data.contains('\r'), "{data:?}");
                assert!(!data.contains('\n'), "{data:?}");
            }
        }
    }

    #[test]
    fn a_name_comes_from_the_flag_then_the_environment_and_is_checked_early() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(NAME_ENV);
        assert_eq!(run_args().validated_name().unwrap(), None);

        let flagged = RunArgs {
            name: Some("  auth refactor ".into()),
            ..run_args()
        };
        assert_eq!(flagged.validated_name().unwrap().as_deref(), Some("auth refactor"));

        std::env::set_var(NAME_ENV, "from the env");
        assert_eq!(run_args().validated_name().unwrap().as_deref(), Some("from the env"));
        // The flag wins over the variable.
        assert_eq!(flagged.validated_name().unwrap().as_deref(), Some("auth refactor"));
        // An empty variable is no name, not a refusal.
        std::env::set_var(NAME_ENV, "  ");
        assert_eq!(run_args().validated_name().unwrap(), None);

        // Refused, and the refusal says where the bad name came from.
        std::env::set_var(NAME_ENV, "a\u{1b}[2Jb");
        let why = run_args().validated_name().unwrap_err().to_string();
        assert!(why.starts_with("$RIME_AGENT_NAME"), "{why}");
        let long = RunArgs {
            name: Some("n".repeat(65)),
            ..run_args()
        };
        let why = long.validated_name().unwrap_err().to_string();
        assert!(why.starts_with("--name"), "{why}");
        std::env::remove_var(NAME_ENV);
    }

    #[test]
    fn a_remote_run_carries_the_effective_name_but_a_name_is_not_a_setting_a_daemon_could_drop() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(NAME_ENV);
        let named = RunArgs {
            name: Some("auth refactor".into()),
            ..run_args()
        };
        let argv = named.forward_argv();
        let at = argv.iter().position(|a| a == "--name").expect("--name is forwarded");
        assert_eq!(argv[at + 1], "auth refactor");

        // `$RIME_AGENT_NAME` goes too: the remote login never saw this shell.
        std::env::set_var(NAME_ENV, "from the env");
        let argv = run_args().forward_argv();
        let at = argv.iter().position(|a| a == "--name").expect("the variable is forwarded");
        assert_eq!(argv[at + 1], "from the env");
        std::env::remove_var(NAME_ENV);
        assert!(!run_args().forward_argv().contains(&"--name".to_string()));

        // And it widens nothing when an old daemon drops it, so it is not on
        // the table that makes `run` refuse an old daemon.
        let p = AgentPolicy::default();
        assert!(settings_a_daemon_could_drop(&p, None, false, false, false, false).is_empty());
    }

    #[test]
    fn the_label_is_the_name_then_the_title_then_agent_and_project() {
        let mut s: SessionInfo = serde_json::from_str(
            r#"{"id":4,"agent":"claude","program":"claude","args":[],"cwd":"/home/a/rime-os",
                "project":null,"project_name":null,"worktree":null,"state":"working",
                "detail":null,"paused":false,"pid":1,"started":1,"last_activity":1,
                "exit_code":null,"exit_signal":null,"attached":0,"checkpoint":null,
                "cols":80,"rows":24}"#,
        )
        .expect("a record from before names");
        assert_eq!(session_label(&s), "claude · rime-os");
        assert_eq!(list_name(&s), "-");
        s.project_name = Some("rime".into());
        assert_eq!(session_label(&s), "claude · rime");
        s.title = Some("Rime showcase studio".into());
        assert_eq!(session_label(&s), "Rime showcase studio");
        assert_eq!(list_name(&s), "Rime showcase studio");
        s.name = Some("auth refactor".into());
        assert_eq!(session_label(&s), "auth refactor", "a person's name beats the agent's title");
        assert_eq!(list_name(&s), "auth refactor");
    }

    #[test]
    fn rename_takes_a_name_or_clear_and_not_both() {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            cmd: AgentCmd,
        }
        match Cli::try_parse_from(["agent", "rename", "4", "auth", "refactor"]).unwrap().cmd {
            AgentCmd::Rename { id, name, clear, host } => {
                assert_eq!(id, 4);
                assert_eq!(name.join(" "), "auth refactor");
                assert!(!clear);
                assert_eq!(host, None);
            }
            _ => panic!("not rename"),
        }
        match Cli::try_parse_from(["agent", "rename", "4", "--clear"]).unwrap().cmd {
            AgentCmd::Rename { name, clear, .. } => {
                assert!(clear);
                assert!(name.is_empty());
            }
            _ => panic!("not rename"),
        }
        assert!(Cli::try_parse_from(["agent", "rename", "4"]).is_err(), "neither is refused");
        assert!(
            Cli::try_parse_from(["agent", "rename", "4", "x", "--clear"]).is_err(),
            "both is refused"
        );
        // `-n` on run is the name, and no other flag has it.
        match Cli::try_parse_from(["agent", "run", "-n", "auth refactor", "fix it"]).unwrap().cmd {
            AgentCmd::Run(args) => {
                assert_eq!(args.name.as_deref(), Some("auth refactor"));
                assert_eq!(args.prompt.as_deref(), Some("fix it"));
            }
            _ => panic!("not run"),
        }
    }

    #[test]
    fn no_flags_at_all_resolves_to_the_configured_defaults() {
        let cfg = config::Config::default();
        let p = resolve_policy(&cfg, &run_args()).expect("resolve");
        assert_eq!(p, AgentPolicy::default());
    }

    #[test]
    fn agent_bypass_moves_dimension_one_and_leaves_the_other_five() {
        // §4.2. The flag that most invites a collapse: the obvious "helpful"
        // edit is to have it also loosen the sandbox, and that is criterion 2.
        let cfg = config::Config::default();
        let args = RunArgs {
            agent_bypass: true,
            ..run_args()
        };
        let p = resolve_policy(&cfg, &args).expect("resolve");
        assert_eq!(p.native, NativeMode::Bypass);
        assert_eq!(p, AgentPolicy { native: NativeMode::Bypass, ..AgentPolicy::default() });
    }

    #[test]
    fn a_flag_overrides_the_preset_it_was_given_alongside() {
        // Presets are a starting point, not a fence. Break-glass with the
        // secret layer explicitly off must be expressible, or the named modes
        // would be the only reachable points in the space.
        let cfg = config::Config::default();
        let args = RunArgs {
            unsafe_everything: true,
            secrets: Some(SecretPolicy::None),
            ..run_args()
        };
        let p = resolve_policy(&cfg, &args).expect("resolve");
        // Break-glass set three dimensions; the flag overrode the one it
        // named and left the other two where the preset put them.
        assert_eq!(p.system, SystemAccess::Unsafe);
        assert_eq!(p.sandbox, SandboxPolicy::Unrestricted);
        assert_eq!(p.native, NativeMode::Bypass);
        assert_eq!(p.secrets, SecretPolicy::None, "the flag did not win");
    }

    #[test]
    fn the_configured_default_is_the_starting_point_and_flags_win() {
        let cfg = config::Config {
            sandbox: SandboxPolicy::Strict,
            native: NativeMode::Bypass,
            ..config::Config::default()
        };
        // Nothing given: the configuration stands.
        let p = resolve_policy(&cfg, &run_args()).expect("resolve");
        assert_eq!(p.sandbox, SandboxPolicy::Strict);
        assert_eq!(p.native, NativeMode::Bypass);
        assert_eq!(p.network, NetworkPolicy::Offline, "strict normalises to offline");

        // A flag beats it.
        let args = RunArgs {
            sandbox: Some(SandboxPolicy::Project),
            ..run_args()
        };
        let p = resolve_policy(&cfg, &args).expect("resolve");
        assert_eq!(p.sandbox, SandboxPolicy::Project);
        assert_eq!(p.network, NetworkPolicy::Open);
    }

    #[test]
    fn an_explicit_contradiction_is_refused_rather_than_quietly_tightened() {
        // `--sandbox strict --network open` cannot be honoured. Tightening it
        // in silence would leave somebody believing they had a network; this
        // is the one check the CLI has to make, because only the CLI can tell
        // a typed `--network open` from a client that sent no network key.
        let cfg = config::Config::default();
        let args = RunArgs {
            sandbox: Some(SandboxPolicy::Strict),
            network: Some(NetworkPolicy::Open),
            ..run_args()
        };
        let err = resolve_policy(&cfg, &args).expect_err("a contradiction");
        assert!(err.to_string().contains("removes the network"), "{err}");

        // The same pair spelled compatibly is fine.
        let args = RunArgs {
            sandbox: Some(SandboxPolicy::Strict),
            network: Some(NetworkPolicy::Offline),
            ..run_args()
        };
        assert!(resolve_policy(&cfg, &args).is_ok());
    }

    #[test]
    fn an_unenforceable_dimension_is_refused_in_front_of_the_user_who_typed_it() {
        let cfg = config::Config::default();
        let cases: [(RunArgs, &str); 2] = [
            (
                // Break-glass inside a sandbox that would keep no_new_privs
                // on anyway: bwrap sets it unconditionally, so the pair would
                // report a boundary it had not moved.
                RunArgs {
                    system_access: Some(SystemAccess::Unsafe),
                    sandbox: Some(SandboxPolicy::Project),
                    ..run_args()
                },
                "no_new_privs",
            ),
            (
                RunArgs { secrets: Some(SecretPolicy::Export), ..run_args() },
                "never placed in a session",
            ),
        ];
        for (args, expect) in cases {
            let err = resolve_policy(&cfg, &args).expect_err(expect);
            assert!(err.to_string().contains(expect), "{err}");
        }
    }

    #[test]
    fn remote_elevation_is_no_longer_refused_in_front_of_the_user_who_typed_it() {
        // This assertion FLIPPED in P0-014's last commit, and it is the third
        // case that used to sit in the list above.
        //
        // `--origin-policy remote` was refused at this point because §7 allows
        // remote elevation only behind a security key and nothing in the build
        // could ask for one, so accepting the flag would have been a setting
        // that reads as enabled and enforces nothing. All of it now exists,
        // the daemon's gate consults this very value, and the flag therefore
        // has to parse and be kept.
        //
        // It is kept as its own named test rather than deleted, so that the
        // flip is visible to anyone reading the history of this file rather
        // than being a line that quietly vanished from an array.
        let cfg = config::Config::default();
        let args = RunArgs {
            origin_policy: Some(OriginPolicy::RemoteElevationAllowed),
            ..run_args()
        };
        let policy = resolve_policy(&cfg, &args).expect("remote elevation is buildable now");
        assert_eq!(policy.origin, OriginPolicy::RemoteElevationAllowed);
        // And it moved exactly one dimension: the flag is an opt-in to a
        // second authentication path, not a preset.
        assert_eq!(policy.sandbox, AgentPolicy::default().sandbox);
        assert_eq!(policy.system, AgentPolicy::default().system);
        assert_eq!(policy.secrets, AgentPolicy::default().secrets);
    }

    #[test]
    fn an_allowlist_with_nothing_on_it_is_refused_where_it_can_still_be_fixed() {
        // Fail-closed either way — an empty allowlist denies everything — but
        // a session reporting `allowlist` while reaching nothing is an offline
        // session nobody asked for, and the failure would otherwise arrive
        // minutes later as a network error inside the agent.
        let cfg = config::Config::default();
        assert!(cfg.network_allow.is_empty());
        let args = RunArgs {
            network: Some(NetworkPolicy::Allowlist),
            ..run_args()
        };
        let err = resolve_policy(&cfg, &args).expect_err("an empty allowlist");
        assert!(err.to_string().contains("rime agent allow"), "{err}");

        // With a destination configured it is a mode that runs.
        let cfg = config::Config {
            network_allow: vec!["api.anthropic.com".into()],
            ..config::Config::default()
        };
        let p = resolve_policy(&cfg, &args).expect("resolve");
        assert_eq!(p.network, NetworkPolicy::Allowlist);
        assert_eq!(p.sandbox, SandboxPolicy::Project, "it still needs a namespace");
    }

    #[test]
    fn a_destination_is_normalised_before_it_is_stored() {
        // Otherwise `API.Example.COM.` and `api.example.com` become two rules
        // in the file that mean one thing, and removing one leaves the other.
        use rime_agent_core::destination::Rule;
        assert_eq!(Rule::parse("API.Example.COM.").unwrap().as_line(), "api.example.com");
        assert_eq!(Rule::parse("api.example.com:443").unwrap().as_line(), "api.example.com");
        assert_eq!(Rule::parse("git.example.com:22").unwrap().as_line(), "git.example.com:22");
        // And a rule that would empty the whole list on the next load is
        // refused before it is written.
        assert!(Rule::parse("*.com").is_err());
    }

    #[test]
    fn brokered_egress_is_refused_when_the_broker_has_been_shut() {
        // Two flags that each make sense and cannot both be honoured. Refused
        // where the user typed them, so the message names both rather than
        // arriving from the daemon as a policy error about one.
        let cfg = config::Config::default();
        let args = RunArgs {
            sandbox: Some(SandboxPolicy::Project),
            network: Some(NetworkPolicy::Brokered),
            secrets: Some(SecretPolicy::None),
            ..run_args()
        };
        let err = resolve_policy(&cfg, &args).expect_err("a contradiction");
        assert!(err.to_string().contains("--secrets none"), "{err}");

        // Brokered on its own, under project confinement, is a mode that runs.
        let args = RunArgs {
            sandbox: Some(SandboxPolicy::Project),
            network: Some(NetworkPolicy::Brokered),
            ..run_args()
        };
        let p = resolve_policy(&cfg, &args).expect("resolve");
        assert_eq!(p.network, NetworkPolicy::Brokered);
        assert_eq!(p.secrets, SecretPolicy::Brokered);
    }

    /// Dimension 8 reaches a session from the command line, and a `curated`
    /// run with nothing curated is refused rather than started empty.
    ///
    /// Built in `d02528d3`, wired to the wire in the commit before this one,
    /// and until now reachable only by editing the runtime's configuration
    /// file — which is to say, not by the person typing the command.
    #[test]
    fn the_plugin_dimension_is_reachable_from_the_command_line() {
        let cfg = config::Config::default();

        // The default is untouched: a run that says nothing about plugins gets
        // every plugin the machine has enabled, exactly as before the
        // dimension existed.
        assert_eq!(
            resolve_policy(&cfg, &run_args()).expect("resolve").plugins,
            PluginPolicy::AsConfigured,
        );

        let removed = RunArgs {
            plugins: Some(PluginPolicy::NoPlugins),
            ..run_args()
        };
        assert_eq!(
            resolve_policy(&cfg, &removed).expect("resolve").plugins,
            PluginPolicy::NoPlugins,
            "--plugins none must reach the policy, or the flag is decoration"
        );

        // `curated` with an empty `plugin_allow` is refused HERE, in front of
        // the person who typed it, rather than starting a session with every
        // plugin removed. "Nobody filled this in" and "load nothing" are two
        // different requests; the daemon refuses it too, and this is the copy
        // whose message names the flag.
        let curated = RunArgs {
            plugins: Some(PluginPolicy::Curated),
            ..run_args()
        };
        let err = resolve_policy(&cfg, &curated).expect_err("an empty curated list");
        assert!(
            err.to_string().contains("plugin"),
            "the refusal does not name the dimension: {err}"
        );
        // And with names in the runtime's configuration — never in the request
        // — the same run resolves.
        let filled = config::Config {
            plugin_allow: vec!["rime".to_string()],
            ..config::Config::default()
        };
        assert_eq!(
            resolve_policy(&filled, &curated).expect("resolve").plugins,
            PluginPolicy::Curated,
        );

        // A typo is refused by the argument parser with the real values, so
        // nothing downstream is handed a value that was never checked.
        let err = parse_plugins("some").expect_err("a typo");
        assert!(err.contains("curated"), "{err}");
        assert!(err.contains("none"), "{err}");
        assert_eq!(parse_plugins("none"), Ok(PluginPolicy::NoPlugins));
    }

    #[test]
    fn a_misspelled_dimension_value_names_the_ones_that_exist() {
        // Refused by the argument parser, so nothing downstream is ever handed
        // a dimension value that was not checked.
        let err = parse_network("allow").expect_err("a typo");
        assert!(err.contains("allowlist"), "{err}");
        assert!(err.contains("offline"), "{err}");
        assert_eq!(parse_network("offline"), Ok(NetworkPolicy::Offline));
        assert!(parse_native("yolo").is_err());
        assert!(parse_sandbox("loose").is_err());
    }

    #[test]
    fn a_remote_run_carries_every_dimension_it_was_given() {
        // Losing `--network offline` on the way to another machine would run
        // the agent with a network on a host nobody was watching.
        let args = RunArgs {
            agent_bypass: true,
            native: None,
            sandbox: Some(SandboxPolicy::Project),
            system_access: Some(SystemAccess::Session),
            secrets: Some(SecretPolicy::None),
            network: Some(NetworkPolicy::Offline),
            origin_policy: Some(OriginPolicy::LocalElevationOnly),
            connectors: Some(ConnectorPolicy::NoConnectors),
            plugins: Some(PluginPolicy::NoPlugins),
            host: Some("katana".into()),
            ..run_args()
        };
        let forwarded = args.forward_argv().join(" ");
        for flag in [
            "--sandbox project",
            "--system-access session",
            "--secrets none",
            "--network offline",
            "--origin-policy local_elevation_only",
            // Dimensions 7 and 8. Both are removals, so losing one in transit
            // gives the remote session MORE than was asked for — the far end
            // would load every plugin on it, hooks included, and reach every
            // connector it defines.
            "--connectors none",
            "--plugins none",
            "--agent-bypass",
        ] {
            assert!(forwarded.contains(flag), "{flag} was not forwarded: {forwarded}");
        }
        // The local-only flags still stay behind.
        assert!(!forwarded.contains("--host"), "{forwarded}");
    }

    #[test]
    fn a_narrowed_grant_stays_narrowed_on_the_machine_it_is_forwarded_to() {
        // P0-007 criterion 3, across §20's hop. This is the one flag whose
        // loss in transit makes the REMOTE session more privileged than the
        // person asked for: the far end's `rime agent run` sees no
        // `--capabilities`, and no narrowing means the whole vocabulary. A
        // narrowing that only applies on the machine you are sitting at is not
        // a narrowing.
        let args = RunArgs {
            system_access: Some(SystemAccess::Session),
            ttl: Some(900_000),
            capabilities: Some(vec!["install".into(), "update".into()]),
            host: Some("katana".into()),
            ..run_args()
        };
        let forwarded = args.forward_argv().join(" ");
        assert!(
            forwarded.contains("--capabilities install,update"),
            "the narrowing did not survive the hop: {forwarded}"
        );
        // And nothing invents one when it was not asked for, which would make
        // every forwarded grant look narrowed in the far end's audit.
        let wide = RunArgs {
            system_access: Some(SystemAccess::Session),
            ttl: Some(900_000),
            host: Some("katana".into()),
            ..run_args()
        };
        assert!(
            !wide.forward_argv().join(" ").contains("--capabilities"),
            "a narrowing was invented"
        );
    }

    #[test]
    fn narrowing_a_grant_does_not_swallow_the_prompt_that_follows_it() {
        // `--capabilities` is the only list-valued flag on `rime agent run`,
        // and `prompt` is POSITIONAL. Declared greedy — `num_args = 1..`, which
        // is how this flag was first written — clap gives the flag everything
        // up to the next `-`, so
        //
        //     rime agent run --capabilities install "fix the bug"
        //
        // parses as TWO verbs and NO prompt. The session then dies at the
        // daemon with "fix the bug is not a verb", and the user is told their
        // prompt is not a capability. Nothing else in this file would notice:
        // `forward_argv` above is given a `RunArgs` built by hand, so it
        // asserts what the struct carries and never how it was filled.
        //
        // Parsed through the real `Cli`, because the interaction being
        // asserted is between a flag and a positional and only the whole
        // parser has both.
        use clap::Parser;
        fn parse(argv: &[&str]) -> RunArgs {
            match crate::Cli::try_parse_from(argv).expect("parses").command {
                crate::Cmd::Agent { cmd: AgentCmd::Run(a) } => *a,
                _ => panic!("not `agent run`"),
            }
        }

        let one = parse(&["rime", "agent", "run", "--capabilities", "install", "fix the bug"]);
        assert_eq!(one.capabilities.as_deref(), Some(&["install".to_string()][..]));
        assert_eq!(
            one.prompt.as_deref(),
            Some("fix the bug"),
            "the capability list ate the prompt"
        );

        // More than one verb is the comma — the spelling `forward_argv` emits,
        // so what this parses is what the far end of §20's hop is sent.
        let two = parse(&[
            "rime",
            "agent",
            "run",
            "--capabilities",
            "install,update",
            "fix the bug",
        ]);
        assert_eq!(
            two.capabilities.as_deref(),
            Some(&["install".to_string(), "update".to_string()][..])
        );
        assert_eq!(two.prompt.as_deref(), Some("fix the bug"));

        // And absent stays absent: no narrowing is the whole vocabulary, which
        // is what every session before this flag existed was given.
        assert!(parse(&["rime", "agent", "run", "fix the bug"]).capabilities.is_none());
    }

    #[test]
    fn only_the_dimensions_that_moved_are_announced() {
        let p = AgentPolicy {
            native: NativeMode::Bypass,
            ..AgentPolicy::default()
        };
        assert_eq!(
            non_default_dimensions(&p),
            vec![("native", "bypass")],
            "a banner that listed all six would be noise on every run"
        );
        assert!(non_default_dimensions(&AgentPolicy::default()).is_empty());
        assert_eq!(describe_policy(&p), "sandbox project, native bypass");
    }

    /// Every dimension that arrived after the split is checked against ITS OWN
    /// revision, not the one the first six shipped in.
    ///
    /// This is the fail-open the version check exists for, and it is silent:
    /// a dimension mapped onto `POLICY_DIMENSIONS_VERSION` passes the check
    /// against a protocol-2 daemon, which then drops the key and runs the
    /// session wider than was asked for. `plugins` was in `dimensions()`
    /// before it was in this table, so for one revision `plugins = "none"` in
    /// the runtime configuration did exactly that.
    #[test]
    fn a_late_dimension_is_checked_against_its_own_revision() {
        let with = |f: fn(&mut AgentPolicy)| {
            let mut p = AgentPolicy::default();
            f(&mut p);
            settings_a_daemon_could_drop(&p, None, false, false, false, false)
        };

        // Dimension 8. The flag name is what the refusal prints, so it is part
        // of the assertion: "would ignore plugins" names nothing the user can
        // type.
        assert_eq!(
            with(|p| p.plugins = PluginPolicy::NoPlugins),
            vec![("--plugins", PLUGIN_POLICY_VERSION)],
            "--plugins must carry the plugin policy's own revision"
        );
        // Dimension 7, the same rule one revision down, kept here so a change
        // that fixes one by breaking the other cannot pass.
        assert_eq!(
            with(|p| p.connectors = ConnectorPolicy::NoConnectors),
            vec![("--connectors", CONNECTOR_POLICY_VERSION)],
        );
        // The revisions are genuinely three different numbers, in the order
        // the dimensions arrived: an assertion that happened to read the same
        // constant twice would hold with the table wrong.
        let revisions = vec![
            POLICY_DIMENSIONS_VERSION,
            CONNECTOR_POLICY_VERSION,
            PLUGIN_POLICY_VERSION,
        ];
        let mut distinct = revisions.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(
            distinct, revisions,
            "the three revisions must be distinct and ascending, or the assertions \
             above could pass with two dimensions sharing a guard"
        );

        // A dimension that DID arrive with the split still maps onto it.
        assert_eq!(
            with(|p| p.network = NetworkPolicy::Offline),
            vec![("network", POLICY_DIMENSIONS_VERSION)],
        );
        // `sandbox` predates every revision here and is never checked, so an
        // all-defaults run against an old daemon still starts.
        assert!(settings_a_daemon_could_drop(&AgentPolicy::default(), None, false, false, false, false).is_empty());

        // And the two dimensions travel independently: asking for both names
        // both, at two revisions.
        let both = AgentPolicy {
            connectors: ConnectorPolicy::NoConnectors,
            plugins: PluginPolicy::NoPlugins,
            ..AgentPolicy::default()
        };
        assert_eq!(
            settings_a_daemon_could_drop(&both, None, false, false, false, false),
            vec![
                ("--connectors", CONNECTOR_POLICY_VERSION),
                ("--plugins", PLUGIN_POLICY_VERSION),
            ],
        );
    }

    /// A narrowed allowlist is checked against its own revision, and it is
    /// checked AT ALL (P2-012).
    ///
    /// The second half is the one worth writing down. `--allow` only means
    /// anything with `--network allowlist`, which is already on the table at
    /// `POLICY_DIMENSIONS_VERSION` — so a table that leaned on the dimension
    /// would pass this check against any daemon from revision 2 onwards, drop
    /// the key, and give the session every destination the machine permits.
    /// Nothing later catches it: the session starts, the page the capsule was
    /// pointed at renders, and the widening is invisible because nothing in
    /// the run tries the other destinations.
    #[test]
    fn a_narrowed_allowlist_is_checked_against_its_own_revision() {
        let allowlisted = AgentPolicy {
            network: NetworkPolicy::Allowlist,
            ..AgentPolicy::default()
        };
        // Narrowed: both the dimension and the key, at two different
        // revisions. The dimension alone would be satisfied by a protocol-2
        // daemon.
        assert_eq!(
            settings_a_daemon_could_drop(&allowlisted, None, false, true, false, false),
            vec![
                ("network", POLICY_DIMENSIONS_VERSION),
                ("--allow", SESSION_ALLOWLIST_VERSION),
            ],
            "--allow must carry its own revision, not the one `allowlist` shipped in"
        );
        // Not narrowed: the same policy asks for nothing extra, so an
        // ordinary allowlisted session still starts against an old daemon.
        assert_eq!(
            settings_a_daemon_could_drop(&allowlisted, None, false, false, false, false),
            vec![("network", POLICY_DIMENSIONS_VERSION)],
        );
        // That the revision is genuinely later than the one before it — so
        // the assertion above cannot pass by reading the same constant twice
        // — is a COMPILE-TIME assertion in `protocol.rs`
        // (`const _: () = assert!(PLUGIN_POLICY_VERSION <
        // SESSION_ALLOWLIST_VERSION)`), and is deliberately not restated here:
        // clippy refuses a runtime assertion over two constants, and a test
        // that could be deleted without breaking the build is the weaker of
        // the two guards anyway.
    }

    /// A browser CA is checked against its own revision, and it is checked at
    /// all even though its dropped key fails CLOSED (P2-012, gap 5).
    ///
    /// Every other entry on that table is stopping a session from running
    /// WIDER than was asked for. This one is stopping a silence: a daemon
    /// below the revision drops `trust_ca`, the capsule's browser refuses the
    /// intranet certificate, and Firefox answers an untrusted chain by sitting
    /// on it — so the caller learns nothing until `rime browser --timeout`
    /// stops the capsule and reports that it did not finish. The refusal
    /// happens before `Run`, which is the last moment at which nothing has
    /// been started.
    #[test]
    fn a_browser_ca_is_checked_against_its_own_revision() {
        // Confined, which is the only shape that can carry one, and nothing
        // else moved: the CA is the whole of what this session asks for.
        let confined = AgentPolicy::default();
        assert!(
            confined.sandbox.is_confined(),
            "the default sandbox is the confined one; if that changes this test is asserting \
             nothing about the pair the daemon refuses"
        );
        assert_eq!(
            settings_a_daemon_could_drop(&confined, None, false, false, true, false),
            vec![("--trust-ca", BROWSER_CA_VERSION)],
            "--trust-ca must carry its own revision, and the flag name is what the refusal \
             prints — `would ignore trust_ca` names nothing a user can type"
        );
        // Not asked for: the same policy needs nothing, so an ordinary session
        // still starts against an older daemon.
        assert!(settings_a_daemon_could_drop(&confined, None, false, false, false, false).is_empty());

        // It travels independently of the narrowed allowlist it will almost
        // always be used beside — a browser capsule sends both — and the two
        // are two different revisions. An assertion that read one constant
        // twice would hold with the table wrong.
        let allowlisted = AgentPolicy {
            network: NetworkPolicy::Allowlist,
            ..AgentPolicy::default()
        };
        assert_eq!(
            settings_a_daemon_could_drop(&allowlisted, None, false, true, true, false),
            vec![
                ("network", POLICY_DIMENSIONS_VERSION),
                ("--allow", SESSION_ALLOWLIST_VERSION),
                ("--trust-ca", BROWSER_CA_VERSION),
            ],
        );
        assert_ne!(
            SESSION_ALLOWLIST_VERSION, BROWSER_CA_VERSION,
            "a capsule sends both keys, so a daemon that honours one and drops the other is a \
             real machine; the two guards cannot be the same number"
        );
    }

    /// P2-012 route B. The entry whose omission would be worst on this table
    /// since `--capabilities`: a daemon below the revision drops `present`,
    /// tunnels the capsule's CONNECT untouched, and the capsule visits the
    /// site as nobody. For a site that serves a public page to anonymous
    /// callers that is a screenshot which looks right.
    #[test]
    fn a_presented_credential_is_checked_against_its_own_revision() {
        let allowlisted = AgentPolicy {
            network: NetworkPolicy::Allowlist,
            ..AgentPolicy::default()
        };
        assert_eq!(
            settings_a_daemon_could_drop(&allowlisted, None, false, true, false, true),
            vec![
                ("network", POLICY_DIMENSIONS_VERSION),
                ("--allow", SESSION_ALLOWLIST_VERSION),
                ("--present", BROWSER_PRESENT_VERSION),
            ],
            "--present must carry its own revision, and the flag name is what the refusal \
             prints"
        );
        assert!(
            settings_a_daemon_could_drop(&allowlisted, None, false, true, false, false)
                .iter()
                .all(|(name, _)| *name != "--present"),
            "a session that did not ask for it must not be refused by a daemon that lacks it"
        );
        // The two browser guards are two revisions, so a daemon that installs
        // a CA and cannot terminate a destination is refused for the second
        // and not the first. An assertion reading one constant twice would
        // hold with the table wrong.
        assert_ne!(
            BROWSER_CA_VERSION, BROWSER_PRESENT_VERSION,
            "gap 5 and route B shipped in consecutive revisions; a guard that conflated them \
             would tell a protocol-10 daemon it understood a key it drops"
        );
    }

    #[test]
    fn truncation_keeps_the_column_width() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("exactlyten", 10), "exactlyten");
        assert_eq!(truncate("waytoolongforthis", 10).chars().count(), 10);
        assert!(truncate("waytoolongforthis", 10).ends_with('…'));
    }

    #[test]
    fn truncation_does_not_split_a_multibyte_character() {
        // Slicing by bytes here would panic on a non-ASCII project name.
        let s = "проектснадлиннымименем";
        let out = truncate(s, 8);
        assert_eq!(out.chars().count(), 8);
    }

    #[test]
    fn ages_read_in_the_largest_sensible_unit() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(format_age(now).ends_with("s ago"));
        assert!(format_age(now - 120).starts_with('2'));
        assert!(format_age(now - 120).ends_with("m ago"));
        assert!(format_age(now - 7200).ends_with("h ago"));
        assert!(format_age(now - 172_800).ends_with("d ago"));
    }

    #[test]
    fn a_future_timestamp_does_not_underflow() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(format_age(now + 10_000), "0s ago");
    }

    #[test]
    fn paths_outside_home_are_left_alone() {
        assert_eq!(short_path("/usr/share/rime"), "/usr/share/rime");
    }
}
