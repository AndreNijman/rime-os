//! The `rime-agentd` control protocol.
//!
//! Newline-delimited JSON over a `SOCK_STREAM` Unix socket. One request per
//! line, one response per line. JSON rather than the repo's usual TOML because
//! Rime Shell is the third consumer after the daemon and the CLI, and QML
//! parses JSON natively while it has no TOML reader at all.
//!
//! `Attach` is the one verb that changes the shape of the connection: the
//! daemon answers with a normal response line and then the *same* connection
//! becomes a raw bidirectional pipe to the session's PTY. Terminal resizes do
//! not travel down that pipe — they arrive as an ordinary [`Request::Resize`]
//! on a second, short-lived connection. Multiplexing control frames into a byte
//! stream that must stay transparent to arbitrary terminal output is how you
//! end up corrupting somebody's editor.
//!
//! This is a stability surface. Rime Shell reads [`SessionInfo`] to draw the
//! Agent Center, so field renames are breaking changes and need the same care
//! as `org.rimeos.Rimed1`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::grant::GrantKind;
use crate::origin::OriginSource;
use crate::policy::{AgentPolicy, RequestOrigin};

/// Protocol revision. Bumped when a change is not backward compatible; the
/// daemon reports it in [`Response::Hello`] so a mismatched CLI can say so
/// plainly instead of failing on a missing field.
///
/// 2 — the six permission dimensions (§3.1). The added keys are flattened
/// alongside `sandbox` rather than nested under it, so an older daemon still
/// reads a new client's `sandbox` correctly. What it does *not* read is the
/// other five: a `--network offline` an old daemon ignores is a session that
/// runs with the network, which is exactly the fail-open a version number
/// exists to catch. The CLI compares this against [`Response::Hello`] and
/// refuses to send a non-default dimension to a daemon that predates it.
///
/// 3 — `request_origin` (§7). Same failure shape and the worse instance of
/// it: a daemon that predates this drops a declared `claude-remote-control`
/// and records the session as whatever it observed, which is local. A remote
/// session filed under a local origin is precisely the thing
/// `request_origin` exists to prevent, so the CLI refuses to send a
/// declaration to a daemon below [`REQUEST_ORIGIN_VERSION`].
///
/// 4 — the credential store left this daemon (§11, P0-002). `SecretGrant` and
/// `SecretGrants` are gone from this protocol, because a grant is now a change
/// to `rime-secretd`'s own store and the CLI asks it directly; `Brokered`
/// gained `audit_id` and `endpoint`. The failure this guards is quieter than
/// the two above and still worth naming: a daemon below this reads its OWN old
/// store, so a credential added to the secret service is simply not found and
/// the user is told they never stored it.
///
/// 5 — capabilities became generic (§13.2, §14, P1-001), `SecretUse` began
/// carrying a message body (§10, P0-003), and system access became a grant
/// (§4.4, §4.5, P0-006/P0-007). Three changes, one revision, because they
/// landed together.
///
/// `SecretUse` used to carry git's arguments as fields — `capability`,
/// `remote`, `branch` — which meant every provider §14 names would have had to
/// widen this protocol. It now carries an operation id, a resource, a declared
/// parameter map and an optional body, so `cloudflare.worker.deploy` and
/// `mcp.request` both need nothing here. A daemon below this does not
/// understand `operation` and answers as though no capability was named, so
/// the CLI refuses to ask one.
///
/// `--system-access` and `--unsafe-everything` used to be refused by every
/// build, so a client could send them to any daemon and get the same honest
/// no. Now they are a request for a grant, carrying a `ttl_ms` a daemon below
/// this drops — and a break-glass session that started with no TTL would be a
/// break-glass session that never expires, which is the one thing §3.4 forbids
/// outright. The CLI refuses to send either mode to a daemon below
/// [`SYSTEM_GRANT_VERSION`].
///
/// Still 5 after `DeclareOrigin` grew `actor` and began latching a
/// non-session connection, and the reason is the rule the numbers above are
/// applying rather than an exception to it. A daemon below this refuses a
/// declaration from a connection that is not a session — loudly, with
/// `permission_denied` — so a proxy that needs one cannot get a silent
/// nothing and carry on forwarding remote requests under a local origin. The
/// dropped key beside it is `actor`, which is a display string. A version
/// number exists to catch a fail-open, and this change fails closed on its
/// own.
/// 6 — a system-access grant can be narrowed to named verbs (§3.3, P0-007).
///
/// `RunRequest::capabilities`, and the one wire change on this list whose
/// dropped-key failure is a WIDENING rather than a loss. Every other field
/// here, dropped by a daemon that predates it, costs the caller something they
/// asked for. This one costs them something they asked to give up: a daemon
/// below this ignores the key and issues a grant covering the whole privilege
/// vocabulary, so `--capabilities install` would silently authorise `rollback`
/// and `update` as well. The CLI refuses to send it to a daemon below
/// [`SCOPED_GRANT_VERSION`].
/// 7 — a session may name which MCP connectors it gets (§10, P1-028).
///
/// Dimension 7, `policy.connectors`, and it fails open in the same direction
/// `--capabilities` does: a daemon below this drops the key, writes no curated
/// MCP configuration and starts the agent with every connector the machine
/// defines — so `--connectors none` would read as a session with no connectors
/// while the cloud plane was fully reachable. The CLI refuses to send it to a
/// daemon below [`CONNECTOR_POLICY_VERSION`].
/// 8 — a session may name which plugins it loads (P1-026).
///
/// Dimension 8, `policy.plugins`, and it fails open in exactly the direction
/// the two above it do, which is why it is a revision of its own rather than a
/// key a newer client may hopefully send: a daemon below this drops it, writes
/// no `enabledPlugins` block into the settings document, and starts the agent
/// with every plugin the machine has enabled — so `--plugins none` would read
/// as a session with no plugins while every one of their `SessionStart` hooks
/// ran. The CLI refuses to send it to a daemon below [`PLUGIN_POLICY_VERSION`].
/// 9 — a session may reach LESS than the runtime allowlist allows (P2-012).
///
/// [`RunRequest::allow`], and the fourth widening on this list. Until this
/// revision the daemon snapshotted the runtime's whole allowlist when a
/// session started and there was no way on the wire to ask for less, so every
/// confined session could reach every destination the machine had ever been
/// told to permit — including a browser capsule started to visit exactly one
/// host. A daemon below this drops the field and does precisely that, which is
/// the caller asking to give something up and being handed it anyway. The CLI
/// refuses to send it to a daemon below [`SESSION_ALLOWLIST_VERSION`].
///
/// The narrowing is validated rather than trusted: every line has to be
/// covered by a rule the runtime already carries
/// ([`crate::destination::Allowlist::narrow`]), so this field can only ever
/// subtract. That is what makes it safe for a session to name its own
/// destinations at all.
/// 10 — a session's browser may be told to trust ONE private CA (P2-012, the
/// intranet gap).
///
/// [`RunRequest::trust_ca`], and the first entry on this list whose dropped
/// key fails CLOSED. A daemon below this ignores the path, the capsule's
/// browser keeps the machine's trust anchors, and the intranet host's
/// certificate is refused. Nothing is widened by the drop — so the argument
/// for a number of its own is not the usual one, and it is worth writing down
/// rather than assuming:
///
/// [`RunRequest::second_factor`] is a wire field that deliberately did NOT
/// take a revision, and its reason is that an old daemon which drops it still
/// produces a loud refusal — the elevation fails for want of a local origin,
/// in front of the person who asked. This field has no such refusal behind it.
/// Firefox meets an untrusted chain by sitting on it: the measurement in
/// `docs/browser-capsule-auth.md` recorded a control profile that refused the
/// handshake and stayed refusing until it was killed. So a capsule whose
/// `trust_ca` was dropped renders nothing, says nothing, and is stopped by
/// `rime browser`'s `--timeout` minutes later — after which the caller is
/// told their capsule "did not finish" and their screenshot was not produced.
/// A silence that costs five minutes and names the wrong cause is worth a
/// number, and the number is the only thing
/// `check_daemon_understands` can consume: it runs BEFORE `Run`, which is the
/// last moment at which nothing has been started.
///
/// 11 — a session's ONE pinned destination is authenticated by the runtime,
/// which the capsule is never given the credential for (P2-012, route B).
///
/// [`RunRequest::present`]. The question `docs/browser-capsule-auth.md` put to
/// the owner — may the runtime read the plaintext of a capsule's connection to
/// the one destination it was pinned to — was answered yes, and this is the
/// number that answer costs. It was written down as 10 in that document while
/// [`RunRequest::trust_ca`] did not exist yet; gap 5 took 10, so route B is 11.
///
/// The dropped key here fails OPEN, which is the ordinary reason a guard
/// exists and is not the browser CA's reason one revision below: a daemon that
/// ignores `present` tunnels the capsule's `CONNECT` untouched, presents
/// nothing, and hands back whatever the site says to an unauthenticated
/// request. That is a 401 in a screenshot, or — worse, for a site that serves
/// a public page to anonymous callers — a capsule that renders something
/// plausible and is not logged in at all.
pub const PROTOCOL_VERSION: u32 = 11;

/// The revision at which the credential store moved to `rime-secretd`.
///
/// Named for the same reason the two below it are: the check is a boundary,
/// and a bare `< 4` in the CLI is one careless edit away from meaning nothing.
pub const BROKERED_SECRET_SERVICE_VERSION: u32 = 4;

/// The revision at which a capability stopped being git-shaped.
///
/// Below this, `SecretUse` has `capability`/`remote`/`branch` and no
/// `operation`, so a request from a current CLI deserialises into a request for
/// nothing. Named for the same reason as the three around it.
pub const GENERIC_CAPABILITY_VERSION: u32 = 5;

/// The revision at which `SecretUse` began carrying a message body.
///
/// `rime mcp bridge` checks it. A daemon below this parses the request, ignores
/// the field it has never heard of, and forwards a capability with no message —
/// which the secret service refuses, correctly, with an error about an empty
/// message that says nothing about the actual cause. Named so the bridge can
/// say the actual cause instead.
///
/// The same number as [`GENERIC_CAPABILITY_VERSION`], and defined as it rather
/// than written out: generic capabilities and the message body shipped in one
/// revision, so there is one wire change and two reasons a daemon below it
/// cannot serve this request. Two names, because the two reasons are what a
/// reader of either guard needs to know.
pub const MCP_BRIDGE_VERSION: u32 = GENERIC_CAPABILITY_VERSION;

/// The revision that first issued system-access grants.
///
/// `rime agent run --system-access session` and `--unsafe-everything` check
/// it. Below this both modes are refused by the daemon outright, so a client
/// that sent one anyway would get a refusal about the mode rather than about
/// the daemon — and the `ttl_ms` beside it would be dropped in silence, which
/// is a break-glass session with no expiry.
///
/// The same number as [`GENERIC_CAPABILITY_VERSION`], and defined as it for
/// the same reason [`MCP_BRIDGE_VERSION`] is: the three changes shipped in one
/// revision, so there is one wire change and three reasons a daemon below it
/// cannot serve a current client. Three names, because the three reasons are
/// what a reader of any one guard needs to know.
pub const SYSTEM_GRANT_VERSION: u32 = GENERIC_CAPABILITY_VERSION;

/// The revision that first narrowed a grant to named verbs (P0-007).
///
/// `rime agent run --capabilities` checks it, and it is the guard whose
/// absence would be worst: below this the key is dropped and the daemon
/// issues the grant it always issued, which covers every verb. A flag that
/// narrows nothing while reading as though it did is the exact fail-open the
/// numbers on this list exist to catch, so the CLI refuses rather than sends
/// it and hopes.
///
/// Its OWN number rather than another alias of [`GENERIC_CAPABILITY_VERSION`],
/// because it is a separate wire change in a separate release — the three
/// aliases above really did ship together and this did not.
pub const SCOPED_GRANT_VERSION: u32 = 6;

/// The guards arrive in order, checked when the crate compiles rather than
/// when a test runs: they are facts about three constants, and a revision
/// numbered behind the one before it would make a `<` comparison in the CLI
/// mean something nobody intended.
const _: () = assert!(POLICY_DIMENSIONS_VERSION < REQUEST_ORIGIN_VERSION);
const _: () = assert!(REQUEST_ORIGIN_VERSION < BROKERED_SECRET_SERVICE_VERSION);
const _: () = assert!(BROKERED_SECRET_SERVICE_VERSION < GENERIC_CAPABILITY_VERSION);
// Not `<`: the three are one revision, and a guard claiming otherwise would
// make a later `<` comparison in the bridge or the CLI mean something nobody
// intended.
const _: () = assert!(MCP_BRIDGE_VERSION == GENERIC_CAPABILITY_VERSION);
const _: () = assert!(SYSTEM_GRANT_VERSION == GENERIC_CAPABILITY_VERSION);
// `<`, not `==`: narrowing a grant arrived a revision after grants did, and a
// guard claiming they are the same revision would let `--capabilities` reach a
// daemon that drops it and grants everything.
const _: () = assert!(SYSTEM_GRANT_VERSION < SCOPED_GRANT_VERSION);
// Dimension 7 is a later revision than scoped grants, so a daemon can accept
// `--capabilities` and still drop `connectors`.
const _: () = assert!(SCOPED_GRANT_VERSION < CONNECTOR_POLICY_VERSION);
// And dimension 8 is later again: a daemon can confine the connectors it keeps
// and still know nothing about which plugins the session was meant to load.
const _: () = assert!(CONNECTOR_POLICY_VERSION < PLUGIN_POLICY_VERSION);
// Later again, and the ordering is what makes the CLI's per-setting refusal
// mean anything: a daemon can honour every dimension and still snapshot the
// runtime's whole allowlist for a session that asked for one host.
const _: () = assert!(PLUGIN_POLICY_VERSION < SESSION_ALLOWLIST_VERSION);
// And later again: a daemon can narrow a session's allowlist to one host and
// still know nothing about installing a CA for the browser that visits it.
// The two arrived in consecutive revisions and are not the same wire change.
const _: () = assert!(SESSION_ALLOWLIST_VERSION < BROWSER_CA_VERSION);
// And later again: a daemon can install a private CA for a capsule's browser
// and still know nothing about terminating that capsule's TLS and asking the
// secret service to put a credential on the request.
const _: () = assert!(BROWSER_CA_VERSION < BROWSER_PRESENT_VERSION);

/// The revision that first carried the six dimensions.
///
/// Named rather than written as a literal at the comparison, because the check
/// is a security boundary and a bare `< 2` in the CLI is one careless edit away
/// from meaning nothing.
pub const POLICY_DIMENSIONS_VERSION: u32 = 2;

/// The revision that first carried `request_origin`, for the same reason.
pub const REQUEST_ORIGIN_VERSION: u32 = 3;

/// The revision that first carried dimension 7, the connector policy.
///
/// Its own number rather than an alias of [`POLICY_DIMENSIONS_VERSION`]: the
/// six arrived together in revision 2 and this one did not, so a daemon can
/// understand `--network offline` and still ignore `--connectors`.
pub const CONNECTOR_POLICY_VERSION: u32 = 7;

/// The revision that first carried dimension 8, the plugin policy.
///
/// Its own number, for [`CONNECTOR_POLICY_VERSION`]'s reason one dimension
/// over: dimension 7 arrived in revision 7 and this did not, so a daemon can
/// honour `--connectors none` and still load every plugin's hooks.
pub const PLUGIN_POLICY_VERSION: u32 = 8;

/// The revision that first let a session narrow its own allowlist (P2-012).
///
/// `rime agent run --allow` and `rime browser run --allow` check it, and it
/// belongs beside [`SCOPED_GRANT_VERSION`] rather than beside the dimensions:
/// its dropped key makes the session reach MORE than was asked for, not less.
/// A daemon below this ignores the list and snapshots the runtime's whole
/// allowlist, so a capsule started to visit one host runs with every
/// destination the machine permits and nothing anywhere says so.
///
/// Its own number rather than an alias of [`PLUGIN_POLICY_VERSION`], because a
/// daemon can honour `--plugins none` and still know nothing about this.
pub const SESSION_ALLOWLIST_VERSION: u32 = 9;

/// The revision that first let a session's browser be told to trust one
/// private CA (P2-012, [`RunRequest::trust_ca`]).
///
/// `rime agent run --trust-ca` and `rime browser run --trust-ca` check it. The
/// only guard on this list whose field fails CLOSED when it is dropped, and
/// the reason it exists anyway is written out at [`PROTOCOL_VERSION`]: the
/// closed failure is a browser that sits silently on a refused handshake until
/// a timeout kills it, which names the wrong cause for five minutes. Every
/// other guard here is catching a session that ran WIDER than was asked for;
/// this one is catching a session that ran narrower and could not say so.
pub const BROWSER_CA_VERSION: u32 = 10;

/// The revision that first let the runtime authenticate a session's one pinned
/// destination (P2-012, [`RunRequest::present`]).
///
/// `rime agent run --present` and `rime browser run --present` check it. Its
/// own number rather than an alias of [`BROWSER_CA_VERSION`], because a daemon
/// can install a CA for a capsule's browser and know nothing about terminating
/// TLS for a destination and asking `rime-secretd` to authenticate it — the
/// two arrived in consecutive revisions and are not the same wire change.
///
/// Back to the ordinary direction after the exception one revision below: a
/// dropped key here makes the session run WIDER than was asked for, in the
/// sense that matters — it runs unauthenticated, against a site the caller
/// believes it is logged in to.
pub const BROWSER_PRESENT_VERSION: u32 = 11;

/// What this daemon can do that an older one of the same protocol revision
/// cannot, as [`Response::Hello`] lists it (`docs/remote-live-contract.md`
/// §1.1).
///
/// A list of words and not a revision, because none of these is a change a
/// version number exists for. Every guard above catches a key an old daemon
/// DROPS while the session runs wider than was asked for; each of these is a
/// verb an old daemon answers `bad_request`, or a field it ignores while losing
/// a convenience — a name not shown, a reply typed and not sent. The rule the
/// comments on [`Request::Event`] set out is that such a change is not a
/// bump, and three branches editing this file at once is the reason the rule
/// exists.
///
/// What the list buys a client is knowing BEFORE it asks. A phone that sees no
/// `input_submit` sends the text and the carriage return as two requests
/// itself; one that sees no `peek` does not draw a preview it would have to
/// take away again. An absent `features` key is an old daemon, and means none.
///
/// Exactly the features this build implements, in the contract's order.
/// `every_advertised_feature_is_the_contracts` pins the strings, because a
/// client matches on them byte for byte.
pub const FEATURES: &[&str] = &[
    "input_submit",
    "rename",
    "peek",
    "session_name",
    "session_title",
    "size_restore",
    "clipboard_write",
    "save_file",
];

/// What a session is doing. The five user-facing values come straight from the
/// roadmap's agent event protocol; `Starting` and `Exited` are the lifecycle
/// bookends the runtime itself owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// Spawned, no output observed yet.
    Starting,
    /// Producing output.
    Working,
    /// Quiet, still alive — most likely waiting on the human.
    WaitingForUser,
    /// Asked for a permission decision.
    PermissionRequest,
    /// Finished successfully.
    Complete,
    /// Finished unsuccessfully.
    Failed,
    /// Process is gone and the exit status has been recorded.
    Exited,
}

impl AgentState {
    /// The wire/display name.
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentState::Starting => "starting",
            AgentState::Working => "working",
            AgentState::WaitingForUser => "waiting_for_user",
            AgentState::PermissionRequest => "permission_request",
            AgentState::Complete => "complete",
            AgentState::Failed => "failed",
            AgentState::Exited => "exited",
        }
    }

    /// Parse a state published by a cooperating client (`rime agent event`).
    /// Unknown names are rejected rather than mapped to a default, so a typo in
    /// a user's agent hook surfaces as an error instead of silently reporting
    /// the wrong thing in the Agent Center.
    pub fn parse(s: &str) -> Option<AgentState> {
        match s {
            "starting" => Some(AgentState::Starting),
            "working" => Some(AgentState::Working),
            "waiting_for_user" | "waiting" => Some(AgentState::WaitingForUser),
            "permission_request" | "permission" => Some(AgentState::PermissionRequest),
            "complete" | "completed" | "done" => Some(AgentState::Complete),
            "failed" | "error" => Some(AgentState::Failed),
            "exited" => Some(AgentState::Exited),
            _ => None,
        }
    }

    /// True once the process is gone. Terminal states are never overwritten by
    /// a late observation from the output detector.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            AgentState::Complete | AgentState::Failed | AgentState::Exited
        )
    }
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `pad`, not `write_str`: these are printed in aligned columns, and
        // width specifiers are silently ignored by a Display impl that writes
        // directly.
        f.pad(self.as_str())
    }
}

/// Dimension 2 of §3.1: how much of the filesystem and process table a session
/// may reach. See `sandbox.rs` for what each one actually builds, and
/// `policy.rs` for the other five dimensions this one is deliberately not
/// coupled to.
///
/// It stays in `protocol.rs` because it is the one dimension that predates the
/// split and is therefore a wire-compatibility surface in its own right;
/// `policy::SandboxPolicy` re-exports it so the six can be reached from one
/// place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SandboxPolicy {
    /// No confinement. The escape hatch — an agent runs exactly as the user
    /// would have run it by hand.
    Unrestricted,
    /// The default. Project files writable, the rest of `$HOME` invisible,
    /// `/usr` read-only, no camera or microphone, network allowed.
    #[default]
    Project,
    /// `Project` with the network removed.
    Strict,
}

impl SandboxPolicy {
    /// Every value, so a test that has to hold for all of them can say so
    /// rather than listing three and missing the fourth somebody adds.
    pub const ALL: &'static [SandboxPolicy] = &[
        SandboxPolicy::Unrestricted,
        SandboxPolicy::Project,
        SandboxPolicy::Strict,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            SandboxPolicy::Unrestricted => "unrestricted",
            SandboxPolicy::Project => "project",
            SandboxPolicy::Strict => "strict",
        }
    }

    pub fn parse(s: &str) -> Option<SandboxPolicy> {
        match s {
            "unrestricted" | "none" | "off" => Some(SandboxPolicy::Unrestricted),
            "project" => Some(SandboxPolicy::Project),
            "strict" => Some(SandboxPolicy::Strict),
            _ => None,
        }
    }

    /// Whether this policy runs the process under `bwrap` at all.
    pub fn is_confined(&self) -> bool {
        !matches!(self, SandboxPolicy::Unrestricted)
    }
}

impl std::fmt::Display for SandboxPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.as_str())
    }
}

/// Everything the CLI and the shell need to describe one session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: u32,
    /// Adapter id (`claude`, `codex`, `generic`, …).
    pub agent: String,
    /// The program actually executed, before sandbox wrapping.
    pub program: String,
    pub args: Vec<String>,
    /// Working directory inside the session.
    pub cwd: String,
    /// Project root, when `cwd` sits inside a detected project.
    pub project: Option<String>,
    /// Project name for display, when known.
    pub project_name: Option<String>,
    /// What a PERSON called this session (§1.3): `rime agent run --name`, or
    /// `rename` from the CLI, the Shell or a phone.
    ///
    /// Never taken from anything the agent writes, which is the difference
    /// between this and [`SessionInfo::title`] and the reason they are two
    /// fields. Checked by [`crate::session::session_name`] on the way in, by
    /// the CLI and again by the daemon.
    ///
    /// `#[serde(default)]` so a record written before names existed still
    /// loads, as a session nobody has named. ALWAYS serialised, `null`
    /// included, and deliberately without `skip_serializing_if`: a client
    /// tells "this daemon has names and none is set" (`"name": null`) from
    /// "this daemon predates names" (no key) by exactly that difference, and
    /// it decides whether to offer Rename from it.
    #[serde(default)]
    pub name: Option<String>,
    /// The agent's own terminal title (OSC 0 / OSC 2), with its spinner glyphs
    /// stripped ([`crate::session::clean_title`]).
    ///
    /// Display only. It is text the agent chose, so nothing reads it to decide
    /// anything; the order a client shows a session by is `name`, then this,
    /// then `<agent> · <project>`, so a person's name always wins over the
    /// agent's.
    ///
    /// Serialised the way `name` is, for `name`'s reason.
    #[serde(default)]
    pub title: Option<String>,
    /// Git worktree this session was given, when it was created with one.
    pub worktree: Option<String>,
    pub state: AgentState,
    /// Free-text detail attached to the current state by a published event.
    pub detail: Option<String>,
    /// Whether the session is stopped (`rime agent pause`).
    ///
    /// A separate field rather than a value inside `detail`, which is where it
    /// started. `detail` is free text that any cooperating client can write
    /// with `rime agent event --detail`, so an agent could set it to "paused"
    /// and make the Agent Center offer Resume on a running session — a control
    /// that reads the wrong state is worse than no control. This one only the
    /// runtime writes, and only when it has actually delivered the signal.
    ///
    /// `#[serde(default)]` so a record written by an older daemon still loads;
    /// the default is "not paused", which is what an absent field meant.
    #[serde(default)]
    pub paused: bool,
    /// The seven permission dimensions this session actually runs under,
    /// already normalised by the daemon.
    ///
    /// Flattened, not nested: `sandbox` stays a top-level key, so Rime Shell's
    /// existing read of it keeps working and a record written before the split
    /// still loads with the other five at their defaults. Nesting would have
    /// moved the key and silently reported every old session as `project`.
    #[serde(flatten)]
    pub policy: AgentPolicy,
    /// The destinations this session may actually reach, when its network is
    /// `allowlist` (P2-012).
    ///
    /// The point of the field is that a narrowing has to be VISIBLE. Dimension
    /// 5 above says `allowlist` for a session that reaches every destination
    /// the machine permits and for one started to visit a single host, and
    /// those are very different sessions — so a user auditing a running
    /// capsule could read the dimension, see `allowlist`, and learn nothing
    /// about the boundary they asked for. `rime agent status <id>` prints it.
    ///
    /// The list the daemon ENFORCES, not the one the caller asked for: the
    /// same value the egress proxy was started with and the same one §6.2's
    /// policy point is answered against, taken from the one binding all three
    /// come from. A field filled from the request would be a second account of
    /// the confinement, and the case where the two differ is exactly the case
    /// somebody is auditing.
    ///
    /// `None` for every session with no allowlist to show — an `open` or
    /// `offline` one — and for a record written before this field existed.
    /// Deliberately not an empty vector for those: an empty allowlist denies
    /// everything, so "no list" and "a list of nothing" are opposite
    /// statements about where the session can go.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowlist: Option<Vec<String>>,
    /// Where this session is driven from (§7's `request_origin`).
    ///
    /// Established by the daemon when it forked the session, from the
    /// connection that asked for it — never from the request. `None` is a
    /// record written before origin tracking existed, and it is deliberately
    /// not `local-terminal`: an absent field must not read as the origin §7
    /// reserves root for. Policy treats `None` as non-local.
    ///
    /// `request_origin`, not `origin`, because [`AgentPolicy`] is flattened
    /// into this struct and already owns the `origin` key for dimension 6.
    /// The two are different things: dimension 6 is which origins may
    /// authorise elevation, this is which origin is asking.
    #[serde(default)]
    pub request_origin: Option<RequestOrigin>,
    /// How [`SessionInfo::request_origin`] was arrived at.
    #[serde(default)]
    pub origin_source: Option<OriginSource>,
    /// Which remote actor the origin was declared for, when one was named.
    ///
    /// Set from [`Request::DeclareOrigin`]'s `actor` on the connection that
    /// asked for this session — a paired device id for Rime Remote, and
    /// nothing at all for a session started locally. `request_origin` says
    /// *what kind* of thing is driving the session and this says *which one*,
    /// which is the difference between an audit trail that can answer "was it
    /// my phone" and one that cannot.
    ///
    /// Never a key or a token. It is a display string, shown in the Agent
    /// Center beside the origin, and a client that puts a credential here has
    /// put a credential in a world-readable record.
    #[serde(default)]
    pub actor: Option<String>,
    /// The system-access grant this session runs under, when it has one.
    ///
    /// Present exactly when `policy.system` is not `none`, because the daemon
    /// will not start such a session without a grant. It is the id, not the
    /// grant: the record is on disk and the state depends on the clock, so a
    /// field carrying "active" would be stale the moment it was written.
    #[serde(default)]
    pub grant: Option<u32>,
    /// Unix milliseconds at which that grant runs out.
    ///
    /// Carried beside the id so the Agent Center and `rime agent list` can
    /// render a countdown without a second round trip, and so the red
    /// indicator §3.4 asks for can say how much of the window is left. The
    /// grant record remains the authority.
    #[serde(default)]
    pub grant_expires_ms: Option<u64>,
    /// The agent's own report of its own permission mode (dimension 1).
    ///
    /// §4.1 says Rime passes no permission flag and lets the agent's profile
    /// decide, so `policy.native` reads `inherit` for the normal case — which
    /// says what Rime did, not what the agent is doing. Claude reports its
    /// real mode on every hook event, and this is where that lands, so the
    /// Agent Center can show `bypassPermissions` rather than `inherit`.
    ///
    /// Deliberately NOT a permission input. Dimension 1 is the agent's own
    /// layer and Rime does not enforce it; this is the agent describing
    /// itself, in the same class as `detail`, and nothing branches on it.
    /// `None` until the agent has said, and for agents that never do.
    #[serde(default)]
    pub native_observed: Option<String>,
    /// What this session started: subagents, and the processes it forked
    /// (§P1-020).
    ///
    /// `#[serde(default)]` so a record written before the graph existed still
    /// loads, and — the part that matters more — so the shell can tell an
    /// ABSENT list from an empty one. A daemon that predates this key writes
    /// no key at all, and a client that read the absence as "no subagents"
    /// would report a fact it has no evidence for. Empty means the daemon
    /// looked and found none.
    ///
    /// What Claude's own status line last reported about this session
    /// (§P1-021): the model it is on, how full its context window is, and how
    /// much of the account's rate-limit windows is gone.
    ///
    /// `None` until a status line has run — an agent with none configured, an
    /// adapter that has no status line, or a session that has not refreshed
    /// yet. Deliberately not a default-filled struct: "we have never heard"
    /// and "we heard, and everything was zero" are different facts and only
    /// one of them is worth drawing.
    ///
    /// Carries its own `observed_at`, because a status line runs on a timer
    /// and on events, so an observation can be a minute or an hour old
    /// depending on whether the session is doing anything.
    #[serde(default)]
    pub telemetry: Option<crate::statusline::Telemetry>,
    /// Written even when empty, which is the whole point: `skip_serializing_if`
    /// would make a daemon that has the graph and found nothing indistinguishable
    /// from one that does not have it.
    ///
    /// See [`crate::graph`] for why no entry here carries a state.
    #[serde(default)]
    pub children: Vec<crate::graph::ChildInfo>,
    /// PID of the session leader (the sandbox wrapper when confined).
    pub pid: i32,
    /// Unix seconds when the session was created.
    pub started: u64,
    /// Unix seconds of the most recent PTY output or published event.
    pub last_activity: u64,
    /// Exit status, once the process is gone.
    pub exit_code: Option<i32>,
    /// Signal that killed the process, when it died from one.
    pub exit_signal: Option<i32>,
    /// Number of clients currently attached.
    pub attached: u32,
    /// Checkpoint taken before the session started, when one was requested.
    pub checkpoint: Option<String>,
    pub cols: u16,
    pub rows: u16,
    /// How many files have been handed to this session with
    /// [`Request::Inject`].
    ///
    /// Shown by `rime agent info` and by the Agent Center, because typing into
    /// somebody's agent is the kind of thing that should be countable from
    /// outside it. `#[serde(default)]` so a record written by a daemon that
    /// predates this still loads: a missing count reads as none, which is what
    /// it was.
    #[serde(default)]
    pub injected: u32,
    /// The disposable capsule this session runs inside, if any (§P1-037).
    ///
    /// `None` for an ordinary session, which is nearly all of them. Carried so
    /// that `rime agent status` can say the thing a user MUST be able to see
    /// about such a session: its working tree is a copy, and everything in it
    /// is deleted when the session ends unless a `--copy-out` was given.
    ///
    /// `#[serde(default)]`, so a record written before this reads as an
    /// ordinary session — which is what it was.
    #[serde(default)]
    pub capsule: Option<String>,
}

impl SessionInfo {
    /// True while the process is still around.
    pub fn is_live(&self) -> bool {
        self.exit_code.is_none() && self.exit_signal.is_none()
    }

    /// Human-readable exit summary, or `None` while still running.
    pub fn exit_summary(&self) -> Option<String> {
        if let Some(sig) = self.exit_signal {
            return Some(format!("killed by signal {sig}"));
        }
        self.exit_code.map(|c| {
            if c == 0 {
                "exited 0".to_string()
            } else {
                format!("exited {c}")
            }
        })
    }
}

/// A control request. `cmd` is the tag, so the wire form reads
/// `{"cmd":"attach","id":4,...}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Protocol handshake. Cheap, and the only request a mismatched client can
    /// rely on.
    Hello,
    /// Start a session.
    ///
    /// Boxed for the reason `AgentCmd::Run` is in the CLI: the parameters of a
    /// session are by far the largest variant here, and clippy's
    /// `large_enum_variant` is right. It crossed the threshold when `name` was
    /// added (§1.4). The wire is unchanged — serde serialises a box as what it
    /// holds.
    Run(Box<RunRequest>),
    /// Every session the daemon knows about, newest last.
    List,
    /// One session.
    Info { id: u32 },
    /// Take over the session's PTY. The response line is followed by raw bytes.
    Attach {
        id: u32,
        cols: u16,
        rows: u16,
        /// Replay up to this many bytes of scrollback before live output.
        /// Zero means "no replay".
        #[serde(default = "default_replay")]
        replay: usize,
    },
    /// Tell the PTY its window changed. Sent on its own connection.
    Resize { id: u32, cols: u16, rows: u16 },
    /// Hand a file to a running session: copy it somewhere the session can
    /// read, and type that path into its PTY (P1-035).
    ///
    /// `source` is a path on the HOST, read by the daemon with the daemon's
    /// own access. That is the point — a confined session cannot see
    /// `~/Pictures/Screenshots`, and the daemon can — and it is also the
    /// reason this verb is refused to a caller that resolves to a managed
    /// session. A session allowed to ask for this could name `~/.ssh/id_ed25519`
    /// and have the daemon carry it across the sandbox boundary for it.
    ///
    /// Nothing about the destination or the typed text comes from the caller;
    /// see [`crate::inject`] for what is written and why.
    ///
    /// Not a protocol bump, for the reason [`Request::ToolCheck`] is not: a
    /// daemon that predates this answers "unknown request" and the CLI says so.
    /// The failure loses a feature, never a restriction.
    Inject { id: u32, source: String },
    /// Hand a file to a running session whose bytes are **not on this
    /// machine** (P1-059 criterion 2).
    ///
    /// The second takeover verb. Like [`Request::Attach`], the response line
    /// is followed by raw bytes rather than by another request: the daemon
    /// answers [`Response::Receiving`] and then reads exactly `len` bytes from
    /// the same connection, after which it answers a second time with the
    /// [`Response::Injected`] that [`Request::Inject`] would have produced.
    /// `rime-agentd/src/main.rs` routes it out of the request loop beside
    /// `Attach`, and `rime-remoted` opens a channel for it the way it opens
    /// one for a terminal.
    ///
    /// ## Why it is a separate verb and not a field on `Inject`
    ///
    /// `Inject`'s `source` is a path on the host and the daemon reads it with
    /// the daemon's own access, outside every sandbox. That is the point of
    /// that verb and the reason it is refused to a session and to a non-local
    /// origin. This verb reads **no host path at all** — the bytes arrive on
    /// the connection — so the refusal that protects `Inject` would refuse the
    /// only caller this one exists for. Sharing a verb would have meant one
    /// gate answering two different questions, which is how a gate ends up
    /// answering neither.
    ///
    /// ## What the caller may decide, which is almost nothing
    ///
    /// `name` is a *name*, never a path: [`crate::inject::safe_name`] reduces
    /// it to an alphabet with no `/`, no space, no quote and no `$`, so a
    /// caller that sends `../../.ssh/authorized_keys` gets a file called
    /// `.._.._.ssh_authorized_keys` in the session's inbox. The directory, the
    /// sequence number and the typed text are the daemon's, exactly as they
    /// are for `Inject`.
    ///
    /// `len` is the number of bytes that follow. It is **checked before the
    /// takeover reply** against the same cap `Inject` applies to a file, so an
    /// oversize upload is refused before any of it crosses the wire, and
    /// bounded again while reading, because a declared length is a claim.
    ///
    /// ## Who is refused
    ///
    /// A caller that resolves to a managed session, and one that cannot be
    /// classified — `refuse_input`'s predicate, not `Inject`'s. This verb ends
    /// by typing a path into another session's terminal, which is precisely
    /// what [`Request::Input`] does, and an agent that could do it could ask a
    /// sibling to spend a grant the caller was not given.
    ///
    /// Not a protocol bump, by the criterion on [`Request::Event`]: a daemon
    /// that predates this answers "unparseable request", the client reports
    /// that it could not deliver, and no bytes have been sent. The failure
    /// loses a feature, never a restriction.
    Receive {
        id: u32,
        /// What the file should be called. Reduced, never used verbatim.
        name: String,
        /// How many bytes follow the takeover reply.
        len: u64,
    },
    /// Write text into a live session's terminal.
    ///
    /// The same write [`Request::Attach`] already performs, without the read
    /// half. `handle_attach` turns its connection into the session's terminal
    /// and pumps the client's stdin into the PTY master; a client that has one
    /// thing to say and nothing to display needs only that half. Rime Shell's
    /// push-to-talk route is the first: it holds a transcript and owns no
    /// terminal.
    ///
    /// `data` is written verbatim and no byte is added to it. Whether the line
    /// is SENT is the caller's decision, because it is the difference between
    /// putting words in a prompt and making an agent act on them: `submit`
    /// presses Enter after the text, and without it the text waits in the
    /// prompt for a person.
    ///
    /// ## Why `submit` is a flag and not a `\r` on the end of `data`
    ///
    /// It used to be the byte, appended by the caller, and it was measured to
    /// fail: an agent TUI that sees a long burst ending in CR treats the whole
    /// burst as a PASTE, so the CR becomes a newline in the prompt and nothing
    /// is sent (Claude Code 2.1.283, a ~250-character reply from a phone,
    /// 2026-09-30). The same CR written on its own a moment later is a key
    /// press. So with `submit` the daemon writes `data`, waits
    /// [`crate::session::SUBMIT_GAP`] WITHOUT holding the session lock, and
    /// writes `"\r"` as a separate write. `data` should not itself end in a
    /// line terminator when `submit` is set; the daemon strips nothing.
    ///
    /// `#[serde(default)]`: every client written before the flag sends none
    /// and means none. A daemon that predates it IGNORES the key, so the text
    /// arrives unsent — which is why the flag is advertised in
    /// [`FEATURES`] as `input_submit`, and why a client that does not see it
    /// there sends the CR itself, as a second request after a short pause.
    ///
    /// Refused when the caller is itself a managed session. Every other verb
    /// on this socket is either a question or an action on the caller's own
    /// session; this one puts words in another agent's mouth, and hooks run
    /// inside the sandbox with reach to this socket.
    ///
    /// Not a protocol bump, by the criterion on [`Request::Event`] below: a
    /// daemon that predates this answers "unparseable request", the client
    /// reports that it could not deliver, and nothing has been typed. The
    /// failure loses a message, never a restriction.
    Input {
        id: u32,
        data: String,
        /// Press Enter after `data`, as a separate write. See above.
        #[serde(default)]
        submit: bool,
    },
    /// Give a live session a name, or take its name away (§1.5).
    ///
    /// `name: null` — or no `name` at all, or one that is only whitespace —
    /// clears it. Checked by [`crate::session::session_name`]: at most 64
    /// characters, no control characters, and refused rather than cleaned.
    /// Answered with the updated [`Response::Session`], so a client redraws
    /// from what the daemon recorded rather than from what it sent.
    ///
    /// Refused to exactly the callers [`Request::Input`] is refused to — a
    /// managed session, and a connection that cannot be classified. Only a
    /// person names a session. An agent that could rename a sibling could
    /// make the Agent Center show one session under another's name, and a
    /// person approving something from a phone reads that name.
    ///
    /// Not a protocol bump, by [`Request::Event`]'s criterion: a daemon that
    /// predates it answers "unparseable request" and nothing was renamed.
    Rename {
        id: u32,
        #[serde(default)]
        name: Option<String>,
    },
    /// The tail of a session's output, without attaching to it (§1.6).
    ///
    /// Read from the in-memory scrollback ring, never the on-disk transcript
    /// (which stops at 32 MiB, so a long session's transcript tail is not its
    /// screen). No resize, no attach, and `attached` does not move: a phone
    /// drawing the last lines of five sessions in a list is not five people
    /// looking at five terminals, and must not shrink any of them.
    ///
    /// `bytes` is capped at [`crate::session::PEEK_MAX`] and defaults to it.
    /// The tail may begin in the middle of an escape sequence or of a UTF-8
    /// character; a client feeds it to a terminal emulator at the reply's
    /// `cols`×`rows` and shows the bottom lines, which is what makes a
    /// mid-sequence start harmless.
    ///
    /// Refused to exactly the callers [`Request::Input`] is refused to. An
    /// agent reading another agent's screen is reading whatever that one was
    /// shown — a diff, a secret it printed, a prompt it is answering — and
    /// agents do not read each other's screens.
    Peek {
        id: u32,
        #[serde(default)]
        bytes: Option<usize>,
    },
    /// Read what is on the machine's clipboard right now (P1-059 criterion 3,
    /// the receive half).
    ///
    /// The only verb in this vocabulary that carries something OUT of the
    /// machine that no session produced. Everything else on this socket either
    /// asks about sessions, or pushes into one; this reaches past the sessions
    /// entirely, to a selection a person made with a mouse.
    ///
    /// ## What it is not
    ///
    /// It is not `rime send --clipboard`. That is ssh to another Linux host in
    /// the §20 registry, `wl-paste` here and `wl-copy` there, host to host. It
    /// shares a word and nothing else: a phone is not an ssh destination
    /// running `wl-copy`, and three rounds of Android work called this verb
    /// missing because the similar name was read as the same feature.
    ///
    /// It is also not a session verb, which is why it carries no `id`. There
    /// is nothing to name: one clipboard per Wayland seat, and the daemon
    /// either has a display to read it from or says so.
    ///
    /// ## Who is refused
    ///
    /// [`Request::Input`]'s predicate — a caller that resolves to a managed
    /// session, and one that cannot be classified at all. The first refusal
    /// is the one that matters and it is not a
    /// formality: a person copies a password out of a password manager several
    /// times a day, and an agent that could read this would have a thirty-
    /// second window on every one of them, outside every sandbox, with no
    /// grant asked for and nothing on screen. A phone the user paired and
    /// unlocked is a person; an agent is not.
    ///
    /// ## Why there is a cap and what it is made of
    ///
    /// The reply is one line of this protocol, and for a remote caller that
    /// line is one `rime_remote_core::wire::Frame::Control` — at most 65514
    /// bytes, which `Frame::encode` refuses rather than truncates. In
    /// `rime-remoted` that refusal is a `?` on the send, so an oversize reply
    /// would drop the **whole device connection**, terminal included, rather
    /// than fail one request. `serde_json` escapes a C0 control byte as
    /// `\uXXXX`, six bytes for one, so the worst case is six times the raw
    /// size and the daemon refuses anything over `clipboard::MAX_BYTES`
    /// (`rime-agentd`), a number chosen so that six times it still fits in one
    /// frame. A clipboard past that is refused with its size named, not
    /// silently clipped: a person pasting half of what they copied into an
    /// agent is worse served than one who is told to send a file instead.
    ///
    /// Not a protocol bump, by the criterion on [`Request::Event`]: a daemon
    /// that predates this answers "unparseable request", the phone says the
    /// machine is too old, and nothing has been read. The failure loses a
    /// feature, never a restriction.
    Clipboard,
    /// Set the machine's clipboard to text a person sent from a paired phone
    /// (`docs/remote-live-contract.md` §1.8).
    ///
    /// The opposite direction from [`Request::Clipboard`], and refused to the
    /// same callers: a managed session, and a connection that cannot be
    /// classified. An agent that could set the clipboard could change what a
    /// person pastes into a terminal a moment later, outside every sandbox.
    ///
    /// `text` is at most `clipboard::MAX_BYTES` (`rime-agentd`) and may not be
    /// empty. It is carried verbatim; nothing is trimmed or appended.
    ///
    /// Not a protocol bump, by [`Request::Event`]'s criterion: a daemon that
    /// predates it answers "unparseable request" and the clipboard is as it
    /// was. The phone offers it only when `hello.features` has
    /// `clipboard_write`.
    ClipboardWrite { text: String },
    /// Save a file a person shared from a paired phone into the machine's
    /// Downloads folder (`docs/remote-live-contract.md` §1.8).
    ///
    /// The third takeover verb, shaped like [`Request::Receive`]: the daemon
    /// answers [`Response::Saving`], reads exactly `len` bytes from the same
    /// connection, then answers [`Response::Saved`]. `rime-agentd` routes it
    /// out of the request loop beside `Receive`, and `rime-remoted` refuses it
    /// on channel zero.
    ///
    /// `name` is a name, never a path, and the caller chooses nothing else:
    /// not the folder, not whether an existing file is replaced (it never
    /// is). Refused to the callers [`Request::ClipboardWrite`] is refused to.
    SaveFile {
        /// What the file should be called. Reduced, never used as a path.
        name: String,
        /// How many bytes follow the takeover reply.
        len: u64,
    },
    /// Deliver a signal by name (`int`, `term`, `kill`, `stop`, `cont`).
    Signal { id: u32, signal: String },
    /// Publish a state transition. This is the open event protocol: any client
    /// that knows its session id can report what it is doing.
    ///
    /// Both payload fields are optional, and the two absences mean different
    /// things. No `state` is an event that records something without changing
    /// what the session is doing — a task created, a compaction, a config
    /// change; §6.1 asks for those and none of them is a state. No `event` is
    /// the original form, which every existing client still sends and which
    /// still works: a bare state with no lifecycle name attached.
    ///
    /// Deliberately additive rather than a new request. A daemon that predates
    /// this drops `event`, keeps `state`, and reports exactly what it reported
    /// before — the version guards above exist for changes where an old daemon
    /// dropping a key loses a *restriction*, and this one loses only detail.
    /// It is therefore not a protocol bump, which matters when three branches
    /// are open on this file at once.
    Event {
        id: u32,
        #[serde(default)]
        state: Option<String>,
        /// One of [`crate::hook::HookEvent`]'s names, when the publisher has a
        /// lifecycle event rather than an opinion about state.
        #[serde(default)]
        event: Option<String>,
        #[serde(default)]
        detail: Option<String>,
        /// The agent's own report of its own permission mode (§4.1).
        ///
        /// Claude puts `permission_mode` on every hook payload, so the bridge
        /// carries it here and the daemon records it on
        /// [`SessionInfo::native_observed`]. It is what makes dimension 1
        /// visible in the Agent Center: `policy.native` says what Rime did,
        /// which for the default is *nothing*, and "inherit" is not a
        /// permission mode a user recognises.
        ///
        /// Not a permission input, and no daemon behaviour branches on it.
        /// Dimension 1 is the agent's own layer; Rime neither enforces nor
        /// second-guesses it, so an agent reporting it is the authority on it
        /// in exactly the way an agent reporting its own `detail` is.
        #[serde(default)]
        native: Option<String>,
        /// Which subagent this event is about (§P1-020).
        ///
        /// Set only on `subagent_start` and `subagent_stop`, from the
        /// `agent_id` Claude puts on the payload. Absent everywhere else, and
        /// absent on those two when the harness sent no id — see
        /// [`crate::graph::subagent_started`] for what happens then.
        ///
        /// Additive for the same reason `event` and `native` are: a daemon
        /// that predates this drops the key and records exactly what it
        /// recorded before, which is a session with no graph rather than a
        /// session whose confinement is looser than the client believes. It is
        /// not a protocol bump.
        #[serde(default)]
        agent_id: Option<String>,
        /// What kind of subagent it is: `Explore`, `general-purpose`, the name
        /// of a project's own agent definition. Display only.
        #[serde(default)]
        agent_type: Option<String>,
        /// A test run the bridge saw start or finish (§P1-036).
        ///
        /// Another optional key on this request, and not a protocol bump for
        /// the reason the others are not: a daemon that predates it ignores
        /// the field and records the event exactly as it always did. The
        /// failure loses a status line, never a restriction.
        #[serde(default)]
        test: Option<crate::worktree::TestNote>,
    },
    /// Publish what Claude's status line reported (§P1-021).
    ///
    /// A request of its own rather than another optional field on
    /// [`Request::Event`], because it is not an event: nothing happened, a
    /// timer fired and a program described the session. Folding it into
    /// `Event` would mean `last_activity` moved every minute for a session
    /// nobody is using, and the idle rule that decides `waiting_for_user`
    /// reads exactly that field.
    ///
    /// A daemon that predates this answers with a parse error, which
    /// `rime agent statusline` swallows — the status line still prints, and
    /// the Agent Center shows no telemetry rather than the session showing no
    /// status line.
    Telemetry {
        id: u32,
        telemetry: Box<crate::statusline::Telemetry>,
    },
    /// Ask whether a tool call this session is about to make is one its own
    /// confinement would refuse (§6.2).
    ///
    /// Asked by `rime agent hook pre_tool_use`, which runs inside the sandbox
    /// and so knows three environment variables and nothing about the mounts.
    /// The daemon holds the spec it built, so the decision is made where the
    /// evidence is.
    ///
    /// Not a protocol bump, for the same reason the optional keys on
    /// [`Request::Event`] are not: a daemon that predates this answers "unknown
    /// request", the hook prints nothing, Claude proceeds, and the sandbox that
    /// daemon did build refuses the operation exactly as it always would have.
    /// The failure loses a message, never a restriction — which is the test the
    /// version guards above are applying.
    ToolCheck {
        id: u32,
        tool_name: String,
        /// The tool's own arguments, verbatim from Claude. Untyped because
        /// every tool shapes them differently and a typed union would break on
        /// the next tool added upstream.
        #[serde(default)]
        tool_input: serde_json::Value,
    },
    /// Per-worktree status for a remembered project: tests, conflicts, diff
    /// and local readiness (§P1-036).
    ///
    /// ## Why this takes a SLUG and not a path
    ///
    /// The obvious signature is `{ path: String }`, and it would be a hole.
    /// Answering this request makes the daemon run git in the named directory,
    /// including `merge-tree --write-tree`, which WRITES objects. A confined
    /// session has the control socket bound in so that it can publish events,
    /// so a path-keyed version would let any session point the daemon at a
    /// repository of its choosing and have it write there. Keyed on a
    /// remembered project's slug instead, the set of directories reachable
    /// through this request is exactly the set the user already chose to
    /// remember, and the daemon resolves the path itself.
    ///
    /// Not a protocol bump: an older daemon answers `BadRequest` for an
    /// unknown `cmd` and the connection survives, so a new client against an
    /// old daemon loses this listing and nothing else.
    Worktrees {
        /// A project slug (`project::Project::slug`), or `None` for every
        /// project the user has remembered.
        #[serde(default)]
        project: Option<String>,
    },
    /// Every project the runtime remembers, most recently opened first.
    ///
    /// The cheap half of [`Request::Worktrees`], and a separate verb for
    /// exactly that reason. Answering `worktrees` runs git in every remembered
    /// project — `git worktree list`, a rev walk, and `merge-tree
    /// --write-tree`, which writes objects — which is seconds on a machine
    /// with a few large checkouts. This one reads
    /// [`crate::project::list`]: one small JSON record per project off
    /// `$XDG_STATE_HOME/rime/agent/projects`, no subprocess at all. So a
    /// client that wants a *picker* — "which project shall I start an agent
    /// in" — asks this, and asks `worktrees` only once the user has chosen
    /// one.
    ///
    /// ## What it discloses, and why that is not a widening
    ///
    /// Absolute repository roots, directory names, detected toolchains and a
    /// capsule binding. `worktrees` already answers with the root and the path
    /// of every worktree under it, with no origin check, so nothing here is
    /// reachable to a caller that could not already reach it — this is the
    /// same set, without the git. A caller that could learn a path from this
    /// and not from `worktrees` does not exist.
    ///
    /// ## It has a write in it, stated rather than discovered
    ///
    /// [`crate::project::list`] deletes the record of a project whose
    /// directory has gone. That is a write, performed while answering a read,
    /// and a remote caller can cause it. It is bounded to forgetting a
    /// registration for a directory that is no longer there — the same thing
    /// the next local `rime agent run` would do — and `worktrees` already
    /// calls the same function.
    Projects,
    /// The agent profiles (§5) this runtime understands, and how each stands
    /// on this machine.
    ///
    /// One row per adapter [`Request::Hello`] lists, so a client can render
    /// the picker it already has without a second vocabulary. What the row
    /// adds to the bare id is the four things a client cannot derive and has
    /// had to guess:
    ///
    /// * whether the adapter's **program is on this machine's `PATH`** — a
    ///   phone that offers `codex` on a machine with no `codex` offers a
    ///   button whose only outcome is the daemon's refusal;
    /// * whether the adapter **needs a program named by the caller**
    ///   (`generic`), which until now every client hard-coded as
    ///   `id == "generic"` because the daemon published no flag;
    /// * whether Rime **describes a profile** for it at all, and whether that
    ///   profile is **installed here**;
    /// * how many parts of it are reusable, mixed, machine-local and secret.
    ///
    /// ## No content, only counts — and that is the audit
    ///
    /// [`crate::profile::doctor`]'s sections name the configured model, the
    /// enabled plugins, the marketplaces and the MCP servers, and its
    /// `problems` name skill directories and plugins. None of that crosses
    /// this wire. The reply carries counts by class and a home-relative
    /// directory name, which is what a picker needs and is the whole of it. In
    /// particular `secret` is a COUNT of credential-class entries present and
    /// never a name and never a value — `profile::plan_export` is where the
    /// values are kept out of a bundle, and this verb does not go near them.
    Profiles,
    /// Read the tail of a session's transcript.
    Logs {
        id: u32,
        #[serde(default = "default_log_bytes")]
        bytes: usize,
    },
    /// Forget an exited session (and delete its transcript).
    Remove { id: u32 },
    /// Forget every exited session.
    Prune,

    /// Narrow the calling session's own origin (§7).
    ///
    /// No session id, for the same reason the privilege verbs have none: the
    /// daemon resolves the session from the connection's peer credentials, so
    /// a session can only ever speak about itself. `origin` must be one
    /// [`RequestOrigin::may_be_declared`] accepts and must be at least as
    /// restricted as what the session already has — a session cannot declare
    /// its way back to local, and a Remote Control session cannot declare its
    /// way out of the lock gate.
    ///
    /// This is what Remote Control uses. It is enabled after `claude` has
    /// started, so the session was genuinely local when it was created and
    /// nothing observable about the connection ever changes.
    ///
    /// A connection that is **not** a session declares for the connection
    /// instead of for a session record, and the declaration then stands for
    /// every later request on that connection. That is the case a remote
    /// proxy is in: `rime-remoted` terminates a paired device's channel and
    /// forwards what it carries, and without this the origin of everything it
    /// forwards would be whatever its own cgroup happens to say — which for a
    /// service started from a login session is `local-terminal`, the origin §7
    /// reserves root approval for. The latch is checked by
    /// [`crate::origin::may_declare`] against the live observation on every
    /// request, so it can only ever cost the connection something and can
    /// never reach a local origin.
    DeclareOrigin {
        origin: String,
        /// Who, on the far side of that connection, this is being declared
        /// for: a paired device id, an fqdn, a job name.
        ///
        /// Recorded beside the origin on sessions and privilege requests, so
        /// an audit trail says *which* remote device asked rather than only
        /// that a remote something did. Never a key, a token or a secret —
        /// it is written to records the user reads, and to the audit log.
        ///
        /// `#[serde(default)]` so every existing client keeps working: an
        /// absent actor is "the origin is all that is known", which is what
        /// every record written before this said.
        #[serde(default)]
        actor: Option<String>,
    },

    // ── privilege requests (§4) ─────────────────────────────────────────────
    //
    // Note what is absent: no session id. The daemon resolves the asking
    // session from the connection's peer credentials, because
    // `$RIME_AGENT_SESSION` lives inside a sandbox the agent controls and
    // anything authorised by a client-supplied id is authorised by the agent.
    /// Ask for a privileged operation. `verb` must name one of
    /// [`crate::request::Verb::names`]; the daemon parses and validates it.
    PrivilegeRequest {
        verb: String,
        #[serde(default)]
        args: Vec<String>,
        reason: String,
    },
    /// Every privilege request on record.
    Requests,
    /// Record a human's decision on one. Refused when the connection belongs
    /// to a session — an agent may not approve itself.
    Decide { id: u32, decision: String },
    /// Report that an approved request has been run, with its exit status.
    RequestExecuted { id: u32, exit_code: i32 },
    /// Per-project grants.
    Grants,
    /// Drop a grant, or every grant for the project when `key` is absent.
    Revoke {
        project: String,
        #[serde(default)]
        key: Option<String>,
    },

    // ── system-access grants (§4.4, §4.5) ───────────────────────────────────
    //
    // Named `SystemGrant*` and not `Grant*`: the four verbs above are
    // P0-013's per-project verb grants, which are a different thing with a
    // different store, and one vocabulary meaning two things is how a client
    // ends up revoking the wrong one.
    //
    // None of these carries a session id either, for the same reason
    // `PrivilegeRequest` does not.
    /// Every system-access grant on record, with the state each is in now.
    SystemGrants,
    /// Take a grant back before its TTL runs out.
    ///
    /// Refused when the connection belongs to a managed session: a session
    /// revoking its own grant is harmless, but a session revoking ANOTHER
    /// session's is not, and the daemon does not have to tell the two apart
    /// if neither is allowed.
    RevokeSystemGrant { id: u32 },
    /// Extend a grant that is still active.
    ///
    /// P0-007's fourth criterion — "the agent cannot renew its own grant" —
    /// is enforced here, and it is the reason this verb exists at all rather
    /// than renewal being a side effect of asking again. The refusal is not
    /// "are you the user": the agent *is* the user. It is that the connection
    /// resolves, through `SO_PEERCRED` and `/proc` ancestry, to a managed
    /// session — and nothing running inside one can present a connection that
    /// does not.
    RenewSystemGrant {
        id: u32,
        /// The new window, from now. Bounded like any other, and it is a
        /// fresh authentication rather than an extension of the old consent.
        ttl_ms: u64,
        /// A security key's answer, when the session that holds this grant is
        /// driven from an origin §7 does not give the local column to. See
        /// [`RunRequest::second_factor`], which this follows exactly — down to
        /// not bumping the protocol.
        ///
        /// The challenge it answers must have been issued for **this session**
        /// (`session: Some(id)`), not for a session being started:
        /// `webauthn::SecondFactor::may_answer_for` compares the scope in both
        /// directions, so a touch collected while starting a session cannot
        /// renew an existing grant.
        #[serde(default)]
        second_factor: Option<SubmittedFactor>,
    },

    // ── the secret broker (§4) ──────────────────────────────────────────────
    //
    // Note what `SecretUse` does NOT carry: a session id, and a URL. The
    // session comes from the connection's peer credentials, and the resource is
    // a NAME the provider resolves for itself — a URL would let a session
    // choose where its token gets sent.
    //
    // Note also what it does not carry any more: git. `capability`, `remote`
    // and `branch` were one provider's arguments in a protocol every provider
    // has to fit through, and P1-001 replaced them with an operation id, a
    // resource and a declared parameter map. This is what "provider plugins can
    // be added without changing agent core" means concretely — a Cloudflare
    // worker deployment is `operation: "cloudflare.worker.deploy"` and needs no
    // line here.
    /// Ask the broker to perform a capability. The token never comes back.
    SecretUse {
        service: String,
        /// The §13.2 operation id: `git.push`, `cloudflare.worker.deploy`.
        operation: String,
        /// What it acts on, as a NAME. Empty for an operation that names
        /// nothing, such as reading an account.
        #[serde(default)]
        resource: String,
        /// The operation's own arguments. Checked by `rime-secretd` against
        /// what the provider declared; an undeclared one is refused, so this
        /// map is not a way to smuggle a command line through.
        #[serde(default)]
        params: BTreeMap<String, String>,
        /// The message a capability carries, for the one operation that carries
        /// a message: `mcp.request`.
        ///
        /// A field here and raw bytes on the secret service's own wire, which
        /// is not an inconsistency. That protocol caps a request line because
        /// any local process may write one; this one does not, and a second
        /// framing convention on a socket Rime Shell also parses would be a
        /// compatibility surface for no gain. What both refuse is a credential
        /// in a serialisable type, and a JSON-RPC message the caller wrote is
        /// not one.
        ///
        /// Separate from `params` deliberately. A parameter is declared by the
        /// operation and checked against its syntax; a message body is opaque
        /// bytes the provider forwards, and putting it in the map would mean
        /// declaring a parameter whose value nothing can validate.
        #[serde(default)]
        body: Option<String>,
        /// The caller's project root.
        ///
        /// Honoured ONLY when the peer is not a managed session. A session's
        /// project is whatever the daemon recorded when it forked it, and this
        /// field is ignored for one — otherwise a confined agent could claim
        /// to be in a project whose capabilities it was never granted.
        ///
        /// It has to be sent because the daemon cannot see the caller's
        /// working directory: resolving it from `current_dir()` gave the
        /// DAEMON's cwd, so every grant silently failed to match.
        #[serde(default)]
        project: Option<String>,
    },

    // ── §7's remote elevation (P0-014) ──────────────────────────────────────
    /// Ask for a challenge a security key can answer.
    ///
    /// §7 gives root capability and unsafe-everything "local auth" locally and
    /// "local approval required" from everywhere else, and `OriginPolicy`'s
    /// `remote_elevation_allowed` is the owner's opt-out. What the opt-out
    /// costs is a touch on a security key, and this is how the daemon says
    /// what to touch it over: it issues a nonce, keeps it in memory, and hands
    /// back the exact bytes the key must sign.
    ///
    /// **Not a protocol bump**, for the reason [`Request::Event`] and
    /// [`Request::ToolCheck`] both record: a daemon that has never heard of
    /// this `cmd` fails to deserialise it and answers
    /// [`ErrorKind::BadRequest`], which is refusing to elevate. The failure
    /// mode of an unknown verb here is losing an elevation, never gaining one,
    /// and that is the test those two set. Bumping would also claim revision 6
    /// while three branches are open on this file.
    ElevationChallenge {
        /// The session to be elevated, or `None` for one that does not exist
        /// yet — which is the *primary* case, because
        /// `privilege::authorise_grant` runs before a session id is reserved.
        /// See `webauthn::Challenge::session`.
        #[serde(default)]
        session: Option<u32>,
        /// Which of §4's two elevated modes the touch will be consent to. Part
        /// of what the key signs: a touch for a capability grant must not buy
        /// break-glass.
        kind: GrantKind,
        /// The window being asked for. Also signed over, so a touch for a
        /// minute cannot authorise eight hours.
        ttl_ms: u64,
        /// Which enrolled key, by label. Omitted when only one is enrolled;
        /// with several enrolled and none named the daemon refuses and lists
        /// them rather than picking.
        #[serde(default)]
        credential: Option<String>,
    },
}

impl Request {
    /// Whether answering this can block on a person at a keyboard.
    ///
    /// §4.4 and §4.5 both authenticate, and polkit dispatches the challenge to
    /// the login session's own agent — a dialog on the desktop, not in the
    /// PTY. The daemon sits in `pkcheck` until the person answers it, which is
    /// as long as they take. The control socket's ordinary read timeout is
    /// generous but finite, and a person who walks away from the dialog would
    /// otherwise get a socket error from the CLI while the dialog is still on
    /// screen and the grant is still being decided behind it — a failure
    /// message about the wrong thing entirely.
    ///
    /// Erring towards `true` costs nothing: it removes a deadline from one
    /// request, and the daemon still answers when it is done.
    pub fn waits_on_a_human(&self) -> bool {
        match self {
            Request::Run(r) => r.policy.needs_grant().is_some(),
            Request::RenewSystemGrant { .. } => true,
            _ => false,
        }
    }
}

fn default_replay() -> usize {
    crate::session::SCROLLBACK_BYTES
}

fn default_log_bytes() -> usize {
    64 * 1024
}

/// A security key's answer to an elevation challenge (§7, P0-014).
///
/// What a client sends is the *answer*, never the verdict. A
/// [`crate::webauthn::SecondFactor`] has no public constructor and no public
/// field, so it cannot be serialised into this protocol at all: the daemon has
/// to mint one by redeeming the challenge itself and checking the signature.
/// That is the whole reason the receipt is a type rather than a `bool` — a
/// `bool` on this wire would be a client asserting that it had been verified.
///
/// The three fields are exactly what a human at another machine can copy back
/// from the reply they were given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmittedFactor {
    /// The nonce from [`Response::ElevationChallenge`], naming which challenge
    /// this answers. Spent on arrival, whatever the daemon decides next.
    pub nonce: String,
    /// Which enrolled key answered, by label — the name `rime agent key list`
    /// prints.
    pub credential: String,
    /// The four lines `fido2-assert -G` printed, verbatim: base64 client data
    /// hash, relying party id, base64 authenticator data (the CBOR byte
    /// string, as libfido2 prints it) and base64 DER signature.
    pub assertion: String,
}

/// The parameters of a new session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRequest {
    /// Adapter id, or `None` to use the configured default agent.
    #[serde(default)]
    pub agent: Option<String>,
    /// What to call the session (§1.4): `rime agent run --name`, or
    /// `$RIME_AGENT_NAME`, or the worktree `aw` was given.
    ///
    /// Checked by [`crate::session::session_name`] in the daemon as well as
    /// in the CLI — a client is not a boundary — and before anything is
    /// created, so a refused name leaves no reserved id behind.
    ///
    /// NOT on the CLI's table of settings a daemon could drop, and not a
    /// protocol bump: a daemon that predates it ignores the key and starts
    /// the same session with no name. That loses a label, never a
    /// restriction, which is the test the version guards apply.
    #[serde(default)]
    pub name: Option<String>,
    /// Prompt handed to the agent, when it takes one.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Extra arguments appended after the adapter's own.
    #[serde(default)]
    pub args: Vec<String>,
    /// Run here. Must be absolute.
    pub cwd: String,
    /// The seven permission dimensions, flattened for the same reason as
    /// [`SessionInfo::policy`]: an older client sends `{"sandbox":"strict"}`
    /// and nothing else, and that must keep meaning what it meant.
    ///
    /// The daemon normalises and validates this; it never trusts it as the
    /// final word, because a client is free to send any combination.
    #[serde(flatten)]
    pub policy: AgentPolicy,
    /// A declared origin for the new session (§7).
    ///
    /// `None` — the usual case — means "observe it", and the daemon derives
    /// the origin from the connection asking. A value here is a *declaration*,
    /// checked by [`crate::origin::may_declare`] against what was observed and
    /// only ever accepted when it gives something up. A local origin is never
    /// accepted here, whatever is sent.
    #[serde(default)]
    pub request_origin: Option<RequestOrigin>,
    /// Create/reuse this git worktree under the project and run there.
    #[serde(default)]
    pub worktree: Option<String>,
    /// Take a checkpoint before starting.
    #[serde(default)]
    pub checkpoint: bool,
    /// How long a system-access grant should last, in milliseconds (§3.4's
    /// `--ttl`).
    ///
    /// Meaningless without an elevated `policy.system`, and the daemon refuses
    /// the pair rather than ignoring it: a `--ttl` on an ordinary session is a
    /// user who believes they asked for something they did not.
    ///
    /// `None` with `--system-access session` takes the default window;
    /// `None` with `--unsafe-everything` is refused, because §3.4 asks for the
    /// window to be explicit. See [`crate::grant::ttl_for`].
    #[serde(default)]
    pub ttl_ms: Option<u64>,
    /// Which privilege verbs the grant should cover (§3.3's "capability
    /// scoped", P0-007's third criterion).
    ///
    /// `None` is the whole vocabulary, which is what every session grant
    /// covered before this field existed — so an older client that does not
    /// send it gets exactly the grant it used to get. `Some` narrows, and is
    /// validated by [`crate::grant::capabilities_for`] rather than trusted:
    /// a name that is not a verb is refused, not dropped, because a grant
    /// quietly covering less than was asked for fails in the middle of a
    /// session rather than in front of the person who typed it.
    ///
    /// Refused with `--unsafe-everything`, which does not go through
    /// `rime request` and therefore has no verbs to narrow.
    #[serde(default)]
    pub capabilities: Option<Vec<String>>,
    /// The destinations THIS session may reach, when it should be fewer than
    /// the runtime allows (P2-012, protocol 9).
    ///
    /// `None` is every rule in the runtime's own allowlist, which is what
    /// every allowlisted session got before this field existed. `Some` is a
    /// narrowing and nothing else: each line is parsed, and then checked to be
    /// covered by a rule the runtime already carries
    /// ([`crate::destination::Allowlist::narrow`]). A line the runtime does
    /// not cover is REFUSED rather than dropped — a session that could name a
    /// destination the machine never permitted would make the allowlist
    /// advisory, and a session silently given less than it named would fail
    /// minutes later as a network error inside the agent.
    ///
    /// `Some(vec![])` is refused too. An empty list would mean "no
    /// destinations", which is `--network offline` written a second way, and
    /// two spellings of one policy is how they drift apart.
    ///
    /// Meaningless without `network = allowlist`, and the daemon refuses the
    /// pair rather than ignoring it, for [`RunRequest::ttl_ms`]'s reason: a
    /// caller who listed destinations for an `open` session believes they
    /// asked for a boundary they did not get.
    ///
    /// The narrowed list — not the runtime's — is what the egress proxy
    /// enforces and what is recorded as the session's confinement, so §6.2's
    /// policy point judges a tool call against the destinations the session
    /// can actually reach.
    #[serde(default)]
    pub allow: Option<Vec<String>>,
    /// One private CA this session's BROWSER should trust, as an absolute path
    /// to a PEM file on the host (P2-012, protocol 10).
    ///
    /// The case it exists for is the intranet one: a capsule sent to a site
    /// whose certificate is signed by an organisation's own root, which is not
    /// in the machine's trust store and must not be added to it for the sake
    /// of one automated run.
    ///
    /// ## It is the browser's trust, not the session's
    ///
    /// The name is deliberately narrow and the doc has to be narrower still,
    /// because the obvious reading is wrong: `curl`, `git` and `python` in the
    /// same sandbox keep using the system bundle and still refuse the host.
    /// What this installs is a **Firefox enterprise-policy** root, which is
    /// the only CA install route the image has — `nss-tools` is not
    /// installed, so there is no `certutil` to add one to an NSS database.
    /// See `docs/browser-capsule-auth.md`, where the mechanism is measured
    /// with a control that refuses the same server.
    ///
    /// ## What the daemon does with it, and why the daemon does it
    ///
    /// It reads the file, refuses anything that is not certificates alone,
    /// copies it where the session can read and not write, MERGES an
    /// `Certificates.Install` entry into a copy of the machine's own
    /// `/etc/firefox/policies/policies.json`, and binds that copy over the
    /// real one inside the session's mount namespace. The host's file is never
    /// touched and no other process on the machine sees the added root.
    ///
    /// The daemon rather than the caller, because the alternative shape — a
    /// wire field naming a file and a path to bind it over — would let any
    /// client shadow any path inside a session's namespace, which is a much
    /// larger thing than trusting a CA and would be reviewed as one.
    ///
    /// Meaningless without a confined sandbox: an unconfined session has no
    /// mount namespace to bind into, so the field would be accepted and mean
    /// nothing. The daemon refuses the pair rather than ignoring it, for
    /// [`RunRequest::ttl_ms`]'s reason.
    ///
    /// A `PROTOCOL_VERSION` bump of its own ([`BROWSER_CA_VERSION`]) even
    /// though a dropped key fails CLOSED, which is the opposite of every other
    /// guarded field here. The argument is at [`PROTOCOL_VERSION`]: a dropped
    /// key here is a browser that sits silently on a refused handshake until
    /// `rime browser`'s timeout kills it, and a five-minute silence naming the
    /// wrong cause is worth the number.
    #[serde(default)]
    pub trust_ca: Option<String>,
    /// The stored credential this session's ONE pinned destination should be
    /// authenticated with (P2-012, route B, protocol 11).
    ///
    /// Named by the credential and nothing else. Where it is spent is not a
    /// second field: it is the destination that credential is pinned to, which
    /// the daemon reads out of the secret service and then requires the
    /// session's narrowed allowlist to be — exactly one rule, that host, that
    /// port. A second wire field naming the destination would be a second
    /// thing that can disagree with the first, and the disagreement would be
    /// resolved in favour of whichever one an attacker got to write.
    ///
    /// ## What the daemon does with it
    ///
    /// It mints a CA and one leaf for that destination, installs the CA in the
    /// capsule's browser the way [`RunRequest::trust_ca`] does, and terminates
    /// that one `CONNECT` instead of tunnelling it. The plaintext goes to
    /// `rime-secretd`, which adds the credential's header and originates its
    /// own connection to the site. **The session is never given the
    /// credential, and neither is this daemon** — see
    /// `rime-agentd/src/intercept.rs` for why that second half is the design
    /// and not an accident of it.
    ///
    /// Every other destination stays an opaque tunnel. There are none, for a
    /// session the daemon accepted: the guard requires the narrowed allowlist
    /// to be that one destination.
    ///
    /// Meaningless without a confined sandbox and without `network =
    /// allowlist`, and refused with [`RunRequest::trust_ca`] rather than
    /// combined: a capsule with `present` has one destination and the daemon
    /// terminates it, so a caller-supplied root would be a root for a
    /// connection that no longer exists. The daemon refuses each pair rather
    /// than ignoring it, for [`RunRequest::ttl_ms`]'s reason.
    ///
    /// A `PROTOCOL_VERSION` bump of its own ([`BROWSER_PRESENT_VERSION`]) for
    /// the ordinary reason: a daemon below it drops the key, tunnels the
    /// `CONNECT` untouched, and the capsule visits the site as nobody.
    #[serde(default)]
    pub present: Option<String>,
    /// A security key's answer, for a session asking to elevate from an origin
    /// §7 does not give the local column to (P0-014).
    ///
    /// `None` is the ordinary case, and it is what every local request is:
    /// [`crate::webauthn::may_elevate`] declines to have an opinion about a
    /// local origin, so a session started by a human at this machine never
    /// needs one. From anywhere else this is what `--origin-policy remote`
    /// costs.
    ///
    /// **Not a `PROTOCOL_VERSION` bump**, for the reason
    /// [`Request::ElevationChallenge`] records: a daemon that predates the
    /// field ignores it and then refuses the elevation for want of a local
    /// origin. The failure mode of an unknown field here is losing an
    /// elevation, never gaining one.
    #[serde(default)]
    pub second_factor: Option<SubmittedFactor>,
    pub cols: u16,
    pub rows: u16,
    /// Environment additions, applied after the sandbox is built.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// Run this session inside a DISPOSABLE CAPSULE and delete the whole
    /// environment when it closes (§19, §P1-037).
    ///
    /// The working directory is COPIED into the capsule's throwaway home, not
    /// bound, so whatever the agent does to it goes with the environment —
    /// which is what "discard state" means here. Nothing leaves unless
    /// [`RunRequest::copy_out`] names somewhere for it to go.
    ///
    /// ## A throwaway environment, NOT a security boundary
    ///
    /// distrobox mounts the host's root filesystem at `/run/host` inside every
    /// capsule — that is how `distrobox-export` reaches back out, and there is
    /// no flag that removes it — and the process runs as the user's own uid.
    /// So code in there can read and write the real HOME. What is disposable
    /// is the ENVIRONMENT: its packages, its home, its state.
    ///
    /// For confinement — `$HOME` masked, `~/.ssh` unreachable, the environment
    /// rebuilt from an allowlist — the mechanism is `policy.sandbox`. The two
    /// are REFUSED together rather than combined: bwrap wrapping the capsule
    /// engine would confine the container client and not the agent, so the
    /// pair reads as "confined and disposable" and delivers neither.
    ///
    /// Optional and `#[serde(default)]`, the ToolCheck precedent: a daemon
    /// that predates it ignores the field and starts an ordinary session. That
    /// loses the environment, never a restriction — the failure is a session
    /// on the host, which is what the caller would have got anyway.
    #[serde(default)]
    pub disposable: bool,
    /// Where `~/out` inside a disposable capsule is copied when it closes.
    ///
    /// `None` — the default — means NOTHING leaves. Meaningless without
    /// [`RunRequest::disposable`], and the daemon refuses the pair rather than
    /// ignoring it, for the reason `ttl_ms` is refused on an ordinary session:
    /// a caller who named a destination believes they asked for something.
    #[serde(default)]
    pub copy_out: Option<String>,
}

/// A control response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Response {
    Hello {
        version: u32,
        /// Adapter ids the daemon can launch.
        agents: Vec<String>,
        /// Configured default agent.
        default_agent: String,
        /// What this daemon can do beyond its revision: [`FEATURES`].
        ///
        /// `#[serde(default)]`, so a client reading an older daemon's hello
        /// gets an empty list — "none" — rather than a parse failure. A
        /// client uses a feature only when its word is here.
        #[serde(default)]
        features: Vec<String>,
    },
    /// A session was created or inspected.
    Session(Box<SessionInfo>),
    /// A list of sessions.
    ///
    /// A struct variant, not a newtype around the `Vec`: serde's
    /// internally-tagged representation cannot serialize a newtype variant
    /// containing a sequence, and fails at runtime rather than at compile
    /// time. `every_response_variant_round_trips` pins this.
    Sessions { sessions: Vec<SessionInfo> },
    /// Attach accepted; the connection is now a raw PTY pipe.
    Attached { id: u32 },
    /// [`Request::Receive`] accepted; the connection is now a byte sink.
    ///
    /// `len` is echoed rather than assumed: it is the number of bytes the
    /// daemon has committed to read next, and a client that reads this line
    /// knows the cap was not exceeded and that the count it declared is the
    /// count the daemon agreed to. Everything refusable about the upload — the
    /// caller, the name, the length, the session — has already been decided
    /// when this is written, so a client that sees it may start sending.
    Receiving { id: u32, len: u64 },
    /// A file was handed to a session.
    ///
    /// `path` is both where the copy landed and, verbatim, the text written to
    /// the session's PTY — one field rather than two, so a caller cannot be
    /// shown a path different from the one the agent was given.
    Injected {
        id: u32,
        path: String,
        /// Whether the text was wrapped as a bracketed paste, which happens
        /// only when the program on that PTY has asked for the mode. Reported
        /// so `rime agent send` can say whether the agent will see it as a
        /// paste or as typing.
        bracketed: bool,
    },
    /// What was on the machine's clipboard when [`Request::Clipboard`] asked.
    ///
    /// `text` is the selection, verbatim, with no trailing newline added —
    /// `wl-paste --no-newline`, because a newline this daemon appended would
    /// be a newline the phone pastes into a terminal, and on a terminal that
    /// is the return key.
    ///
    /// An empty string is a real answer and means the clipboard is empty. It
    /// is not an error, and reporting it as one would have the phone say the
    /// machine refused when the machine simply had nothing.
    ///
    /// There is no `bytes` field beside it on purpose. The reply carries the
    /// content, so a count the client could derive is a second number to
    /// disagree with the first — which is the reason
    /// [`Response::Injected`]'s `path` is one field and not two.
    Clipboard { text: String },
    /// [`Request::ClipboardWrite`] set the clipboard. `bytes` is how much
    /// text it now holds, so a phone can say so without trusting its own
    /// count of what it sent.
    ClipboardWritten { bytes: usize },
    /// [`Request::SaveFile`] accepted; the connection is now a byte sink for
    /// exactly `len` bytes. Everything refusable was refused before this.
    Saving { len: u64 },
    /// A shared file is in the Downloads folder.
    ///
    /// `name` is what it is called there, which differs from the name asked
    /// for when that one was reduced or already taken; `path` is the whole
    /// path, for a client that shows where it went.
    Saved { name: String, path: String },
    /// Per-worktree status, main tree first, projects in listing order.
    Worktrees {
        worktrees: Vec<crate::worktree::WorktreeStatus>,
    },
    /// Every remembered project, most recently opened first.
    ///
    /// A struct variant for `Sessions`' reason: serde's internally-tagged
    /// representation cannot serialize a newtype variant wrapping a sequence,
    /// and it fails at runtime rather than at compile time.
    ///
    /// [`crate::project::Project`] is sent as it is stored rather than
    /// reshaped into a wire twin. A twin would be a second definition of the
    /// same record to keep in step, and the stored one is already the shape
    /// `rime project list --json` prints.
    Projects {
        projects: Vec<crate::project::Project>,
    },
    /// One row per adapter: its program, and its profile as it stands here.
    Profiles {
        profiles: Vec<crate::profile::ProfileSummary>,
    },
    Logs {
        id: u32,
        /// UTF-8 lossy transcript tail.
        text: String,
    },
    /// The tail of a session's output ([`Request::Peek`]).
    Peek {
        id: u32,
        /// The raw PTY bytes, base64 with the standard alphabet and padding.
        ///
        /// Base64 and not a string, because the bytes are a terminal's and not
        /// text: a tail can start inside a UTF-8 character, which a lossy
        /// decode would turn into U+FFFD, and every control byte JSON-escapes
        /// to six. 8192 bytes of base64 is at most 10924 characters.
        data: String,
        /// The size the PTY is right now, which is the grid those bytes were
        /// drawn for. A client renders them at this size and reads the bottom
        /// of the result.
        cols: u16,
        rows: u16,
        state: AgentState,
    },
    /// A privilege request was filed, decided or executed.
    Request(Box<crate::request::PrivilegeRequest>),
    /// A list of privilege requests.
    ///
    /// A struct variant for the same reason as `Sessions`: serde's
    /// internally-tagged representation cannot serialize a newtype variant
    /// wrapping a sequence, and it fails at runtime rather than at compile
    /// time.
    Requests {
        requests: Vec<crate::request::PrivilegeRequest>,
    },
    /// Per-project grants: project root -> grant keys.
    Grants {
        projects: std::collections::BTreeMap<String, Vec<String>>,
    },
    /// System-access grants, each with the state it is in right now.
    ///
    /// The state is computed by the daemon and sent, rather than left for the
    /// client to derive: it depends on the running kernel's boot id, and a
    /// client deriving it would have to read `/proc` itself and could get a
    /// different answer from the daemon that issued the grant.
    SystemGrants {
        grants: Vec<crate::grant::SystemGrant>,
        /// Same order as `grants`: the state word, and the sentence.
        states: Vec<(String, String)>,
    },
    /// A brokered capability ran. Carries the RESULT, never the credential.
    Brokered {
        service: String,
        capability: String,
        /// The operation in words, for the log and the transcript.
        detail: String,
        /// The secret service's audit id for this operation, so a report can
        /// cite the trail entry rather than describing it.
        #[serde(default)]
        audit_id: String,
        /// Scheme and host the credential was sent to, as `rime-secretd`
        /// resolved it from the repository. The caller sees where its operation
        /// went without being able to choose it.
        #[serde(default)]
        endpoint: String,
        exit_code: i32,
        /// git's own output, with the credential scrubbed out.
        output: String,
    },
    /// The §6.2 policy point's answer to one `PreToolUse`.
    ///
    /// `deny` absent is an allow, and absent is also what every failure on the
    /// way here produces — an unreachable daemon, a session that has gone, a
    /// request an older daemon does not know. A hook that cannot get an answer
    /// must not invent a refusal, and it does not need to: the sandbox is what
    /// enforces this, and it is still there.
    ToolDecision {
        #[serde(default)]
        deny: Option<String>,
    },
    /// A challenge to be signed by a security key, and how to sign it.
    ///
    /// Everything a human at another machine needs, because that is where the
    /// key is: the remote-elevation path exists precisely for the case where
    /// nobody is at this one.
    ElevationChallenge {
        /// The nonce that names this challenge, base64. Sent back with the
        /// assertion so the daemon knows which challenge was answered.
        nonce: String,
        /// The exact bytes the key must sign over, base64.
        ///
        /// The client data itself and not its hash: `fido2-assert -w` takes
        /// client data and hashes it, and a caller handed only a digest could
        /// not use that mode. `webauthn::Challenge::binding` is what produced
        /// them, and no two challenges can produce the same bytes.
        binding: String,
        /// The label of the key that has to answer.
        credential: String,
        /// That key's credential id, base64. `fido2-assert` is asked for an
        /// assertion by id, and does not print it.
        credential_id: String,
        /// The relying party the credential was enrolled against.
        rp_id: String,
        /// When the challenge stops being good for anything.
        expires_ms: u64,
        /// The commands to run where the key is, for a human to read. Printed
        /// rather than executed: by construction the key is not plugged into
        /// this machine.
        instructions: String,
    },
    /// Verb succeeded and has nothing to say.
    Ok,
    /// Verb failed. `kind` is stable enough to branch on; `message` is for
    /// humans.
    Error { kind: ErrorKind, message: String },
}

/// Failure categories a client may reasonably branch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// No session with that id.
    NoSuchSession,
    /// The session exists but has already exited.
    SessionExited,
    /// Unknown adapter name.
    NoSuchAgent,
    /// The request was malformed or self-contradictory.
    BadRequest,
    /// The sandbox could not be built as requested. Never downgraded silently.
    SandboxUnavailable,
    /// A policy dimension this build cannot enforce was asked for. Distinct
    /// from [`ErrorKind::SandboxUnavailable`], whose remedy is "re-run with
    /// `--sandbox unrestricted`" — advice that would be actively wrong for a
    /// system-access refusal, since loosening the sandbox is not what the user
    /// was denied.
    PolicyRefused,
    /// No privilege request with that id.
    NoSuchRequest,
    /// The caller is not allowed to do this — notably, a session trying to
    /// decide its own privilege request.
    PermissionDenied,
    /// Anything else, including OS errors.
    Internal,
}

impl Response {
    /// Build an error response.
    pub fn error(kind: ErrorKind, message: impl Into<String>) -> Response {
        Response::Error {
            kind,
            message: message.into(),
        }
    }

    /// The error message, if this is an error.
    pub fn as_error(&self) -> Option<(ErrorKind, &str)> {
        match self {
            Response::Error { kind, message } => Some((*kind, message.as_str())),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_names_round_trip() {
        for s in [
            AgentState::Starting,
            AgentState::Working,
            AgentState::WaitingForUser,
            AgentState::PermissionRequest,
            AgentState::Complete,
            AgentState::Failed,
            AgentState::Exited,
        ] {
            assert_eq!(AgentState::parse(s.as_str()), Some(s), "{s}");
        }
    }

    #[test]
    fn unknown_state_is_rejected_not_defaulted() {
        assert_eq!(AgentState::parse("busy"), None);
        assert_eq!(AgentState::parse(""), None);
        assert_eq!(AgentState::parse("Working"), None);
    }

    #[test]
    fn policy_names_round_trip() {
        for p in [
            SandboxPolicy::Unrestricted,
            SandboxPolicy::Project,
            SandboxPolicy::Strict,
        ] {
            assert_eq!(SandboxPolicy::parse(p.as_str()), Some(p), "{p}");
        }
        assert_eq!(SandboxPolicy::parse("loose"), None);
    }

    #[test]
    fn default_policy_is_project_not_unrestricted() {
        // A default that fails open would make every unqualified `rime agent
        // run` an unconfined one.
        assert_eq!(SandboxPolicy::default(), SandboxPolicy::Project);
        assert!(SandboxPolicy::default().is_confined());
    }

    #[test]
    fn display_honours_column_width() {
        // These are printed in aligned tables; a Display impl that ignores the
        // width specifier silently breaks every listing.
        assert_eq!(format!("[{:<10}]", SandboxPolicy::Project), "[project   ]");
        assert_eq!(format!("[{:<18}]", AgentState::Working), "[working           ]");
        assert_eq!(format!("[{:>8}]", SandboxPolicy::Strict), "[  strict]");
    }

    #[test]
    fn terminal_states_are_exactly_the_finished_ones() {
        assert!(AgentState::Complete.is_terminal());
        assert!(AgentState::Failed.is_terminal());
        assert!(AgentState::Exited.is_terminal());
        assert!(!AgentState::Working.is_terminal());
        assert!(!AgentState::WaitingForUser.is_terminal());
        assert!(!AgentState::PermissionRequest.is_terminal());
        assert!(!AgentState::Starting.is_terminal());
    }

    #[test]
    fn requests_serialise_with_a_cmd_tag() {
        let json = serde_json::to_string(&Request::Info { id: 7 }).unwrap();
        assert!(json.contains(r#""cmd":"info""#), "{json}");
        assert!(json.contains(r#""id":7"#), "{json}");
    }

    #[test]
    fn attach_replay_defaults_when_the_field_is_absent() {
        let req: Request = serde_json::from_str(r#"{"cmd":"attach","id":1,"cols":80,"rows":24}"#)
            .expect("parse without replay");
        match req {
            Request::Attach { replay, .. } => {
                assert_eq!(replay, crate::session::SCROLLBACK_BYTES)
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn input_carries_its_text_through_the_wire_byte_for_byte() {
        // The payload is a person's words, so it can hold anything a keyboard
        // or a speech-to-text hook produces: a newline, a carriage return, a
        // quote, a backslash, a tab. The framing is NDJSON, so a raw newline
        // in the serialised line would desynchronise the stream for every
        // request after it, and the bytes typed into the agent's terminal have
        // to be the bytes the caller asked for and no others.
        let text = "say \"hi\"\tthen\\stop\nrun it\r";
        let req = Request::Input {
            id: 4,
            data: text.to_string(),
            submit: false,
        };
        let line = serde_json::to_string(&req).expect("serialise");
        assert!(!line.contains('\n'), "{line} would break NDJSON framing");
        assert!(!line.contains('\r'), "{line} would break NDJSON framing");
        match serde_json::from_str::<Request>(&line).expect("round-trip") {
            Request::Input { id, data, submit } => {
                assert!(!submit);
                assert_eq!(id, 4);
                assert_eq!(data, text, "the payload changed on the wire");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn input_is_tagged_input_and_takes_no_default() {
        // The shell calls `rime agent input <id> <text>` and the CLI builds
        // this; a rename of either the tag or a field is a silent break, since
        // an unknown `cmd` is answered as "unparseable request" and looks to
        // the shell exactly like an old daemon.
        let line = serde_json::to_string(&Request::Input {
            id: 9,
            data: "x".into(),
            submit: false,
        })
        .unwrap();
        assert!(line.contains(r#""cmd":"input""#), "{line}");
        assert!(line.contains(r#""id":9"#), "{line}");
        assert!(line.contains(r#""data":"x""#), "{line}");

        // No `#[serde(default)]` on `data`: an Input with no text is a caller
        // bug, and defaulting it to the empty string would turn that into a
        // successful write of nothing.
        assert!(
            serde_json::from_str::<Request>(r#"{"cmd":"input","id":9}"#).is_err(),
            "an Input without text must not parse"
        );
    }

    #[test]
    fn run_request_sandbox_defaults_to_project_when_omitted() {
        let req: RunRequest =
            serde_json::from_str(r#"{"cwd":"/tmp","cols":80,"rows":24}"#).expect("parse");
        assert_eq!(req.policy.sandbox, SandboxPolicy::Project);
        assert_eq!(req.policy, AgentPolicy::default());
        assert!(req.worktree.is_none());
        assert!(!req.checkpoint);
    }

    #[test]
    fn responses_round_trip_through_json() {
        let r = Response::error(ErrorKind::NoSuchSession, "no session 9");
        let text = serde_json::to_string(&r).unwrap();
        let back: Response = serde_json::from_str(&text).unwrap();
        assert_eq!(
            back.as_error().map(|(k, _)| k),
            Some(ErrorKind::NoSuchSession)
        );
    }

    fn sample_request() -> crate::request::PrivilegeRequest {
        crate::request::PrivilegeRequest {
            id: 7,
            verb: crate::request::Verb::Install {
                packages: vec!["clang".into(), "cmake".into()],
            },
            reason: "Required to compile the project".into(),
            session: Some(4),
            agent: Some("claude".into()),
            project: Some("/home/t/p".into()),
            decision: crate::request::Decision::Pending,
            request_origin: Some(RequestOrigin::LocalTerminal),
            origin_source: Some(OriginSource::Inherited),
            actor: None,
            created_ms: 1_700_000_000_000,
            decided_ms: None,
            executed_ms: None,
            exit_code: None,
            system_grant: None,
        }
    }

    fn sample_session() -> SessionInfo {
        SessionInfo {
            id: 1,
            agent: "claude".into(),
            program: "claude".into(),
            args: vec!["fix it".into()],
            cwd: "/home/t/p".into(),
            project: Some("/home/t/p".into()),
            project_name: Some("p".into()),
            name: None,
            title: None,
            worktree: None,
            state: AgentState::Working,
            detail: None,
            paused: false,
            policy: AgentPolicy::default(),
            allowlist: None,
            request_origin: Some(RequestOrigin::LocalTerminal),
            origin_source: Some(OriginSource::Observed),
            actor: Some("pixel-8-office".into()),
            grant: None,
            grant_expires_ms: None,
            native_observed: None,
            telemetry: None,
        children: Vec::new(),
            pid: 42,
            started: 1,
            last_activity: 2,
            exit_code: None,
            exit_signal: None,
            attached: 1,
            checkpoint: None,
            cols: 80,
            rows: 24,
            injected: 0,
            capsule: None,
        }
    }

    #[test]
    fn every_response_variant_round_trips() {
        // serde's internally-tagged enums reject some shapes only at runtime —
        // a newtype variant wrapping a Vec serialises to an error, not to
        // JSON, and the daemon discovers it by dropping the connection. Every
        // variant is exercised here so the failure is a test, not a hang.
        let variants = vec![
            Response::Hello {
                version: PROTOCOL_VERSION,
                agents: vec!["claude".into(), "generic".into()],
                default_agent: "claude".into(),
                features: FEATURES.iter().map(|f| f.to_string()).collect(),
            },
            Response::Peek {
                id: 3,
                data: "G1swbWhp".into(),
                cols: 120,
                rows: 40,
                state: AgentState::Working,
            },
            Response::Session(Box::new(sample_session())),
            Response::Sessions {
                sessions: vec![sample_session(), sample_session()],
            },
            Response::Attached { id: 3 },
            Response::Receiving { id: 3, len: 4096 },
            Response::Injected {
                id: 3,
                path: "/tmp/rime-agent/3/inbox/001-shot.png".into(),
                bracketed: true,
            },
            Response::Logs {
                id: 3,
                text: "output\n".into(),
            },
            // A clipboard holds whatever was copied, which routinely includes
            // the newline and the quote that NDJSON framing cares about. The
            // assertion below that no serialised response contains a literal
            // newline is the one this variant is here to exercise.
            Response::Clipboard {
                text: "line one\nline two \"quoted\"\n".into(),
            },
            Response::Clipboard { text: String::new() },
            Response::ClipboardWritten { bytes: 12 },
            Response::Saving { len: 2048 },
            Response::Saved {
                name: "IMG_2041 (2).jpg".into(),
                path: "/home/t/Downloads/IMG_2041 (2).jpg".into(),
            },
            // Response::Request is the riskiest shape in this enum: an
            // internally-tagged variant wrapping a struct that itself
            // #[serde(flatten)]s an internally-tagged enum. Both layers use a
            // map representation, which is the one case internal tagging
            // supports — but it is supported at RUNTIME, not by the type
            // system, so it is asserted rather than assumed.
            Response::Request(Box::new(sample_request())),
            Response::Requests {
                requests: vec![sample_request(), sample_request()],
            },
            Response::Grants {
                projects: [("/home/t/p".to_string(), vec!["install:clang".to_string()])]
                    .into_iter()
                    .collect(),
            },
            Response::Brokered {
                service: "github".into(),
                capability: "git-push".into(),
                detail: "git push origin feat/x".into(),
                audit_id: "1a07-1-0".into(),
                endpoint: "https://github.com".into(),
                exit_code: 0,
                output: "Everything up-to-date".into(),
            },
            Response::ElevationChallenge {
                nonce: "bm9uY2U=".into(),
                binding: "YmluZGluZw==".into(),
                credential: "yubikey".into(),
                credential_id: "aWQ=".into(),
                rp_id: "rime-agent.localhost".into(),
                expires_ms: 1_700_000_000_000,
                instructions: "fido2-assert -G -w -p -i /dev/stdin /dev/hidraw0".into(),
            },
            // Two more sequence-carrying struct variants, here for the
            // reason the note above this list gives: an internally-tagged
            // newtype around a Vec serialises to an error at RUNTIME, and the
            // daemon would discover it by dropping the connection.
            Response::Projects { projects: vec![] },
            Response::Projects {
                projects: vec![crate::project::Project {
                    root: "/home/t/p".into(),
                    name: "p".into(),
                    slug: "p-1a2b".into(),
                    languages: vec!["rust".into(), "kotlin".into()],
                    last_opened: 1_700_000_000,
                    capsule: Some("rust".into()),
                }],
            },
            Response::Profiles { profiles: vec![] },
            Response::Profiles {
                profiles: crate::profile::summarise_adapters(std::path::Path::new(
                    "/nonexistent-home-for-a-round-trip",
                )),
            },
            Response::ToolDecision { deny: None },
            Response::ToolDecision {
                deny: Some("no".into()),
            },
            Response::Ok,
            Response::error(ErrorKind::Internal, "boom"),
        ];

        for v in variants {
            let text = serde_json::to_string(&v)
                .unwrap_or_else(|e| panic!("{v:?} does not serialise: {e}"));
            assert!(!text.contains('\n'), "{text} would break NDJSON framing");
            let _: Response = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{v:?} does not round-trip: {e} from {text}"));
        }
    }

    #[test]
    fn the_pause_flag_is_its_own_field_and_defaults_to_not_paused() {
        // It began life as `detail: Some("paused")`, which any cooperating
        // client can write with `rime agent event --detail` — so an agent could
        // make the Agent Center show Resume on a running session. This is the
        // field the UI branches on, and only the runtime writes it.
        let mut s = sample_session();
        assert!(!s.paused, "a fresh session is not paused");

        s.paused = true;
        let text = serde_json::to_string(&s).expect("serialise");
        let back: SessionInfo = serde_json::from_str(&text).expect("deserialise");
        assert!(back.paused, "{text}");

        // A record from an older daemon has no such key. It must load as
        // not-paused rather than failing, which is what `#[serde(default)]`
        // buys — asserted, because dropping the attribute would make every
        // pre-existing session record unreadable.
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        v.as_object_mut().unwrap().remove("paused");
        let old: SessionInfo = serde_json::from_value(v).expect("an old record still loads");
        assert!(!old.paused);

        // And `detail` cannot set it.
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        v["paused"] = serde_json::json!(false);
        v["detail"] = serde_json::json!("paused");
        let spoofed: SessionInfo = serde_json::from_value(v).unwrap();
        assert!(!spoofed.paused, "detail must not be able to claim paused");
    }

    #[test]
    fn a_privilege_request_survives_the_wire_with_its_verb_intact() {
        // Not just "it round-trips": the flattened verb must come back as the
        // same typed value, because the argv that eventually runs is built from
        // it. A shape that serialises but loses the packages would approve one
        // operation and run another.
        let original = sample_request();
        let wire = serde_json::to_string(&Response::Request(Box::new(original.clone())))
            .expect("serialise");
        let back: Response = serde_json::from_str(&wire).expect("deserialise");
        let Response::Request(r) = back else {
            panic!("wrong variant from {wire}");
        };
        assert_eq!(r.verb, original.verb, "{wire}");
        assert_eq!(r.argv(), original.argv());
        assert_eq!(r.reason, original.reason);
        assert_eq!(r.session, original.session);
        // And the tag lands where the protocol says it does.
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["reply"], "request");
        assert_eq!(v["verb"], "install");
    }

    #[test]
    fn a_narrowed_allowlist_travels_as_allow_and_its_absence_is_none() {
        // P2-012, and the assertion is the KEY, not the round trip. Serde
        // round-trips agree with themselves whatever the key is called, so a
        // rename would pass every other test in this file while making every
        // request from an older `rime` unparseable — and the failure mode of
        // a dropped `allow` is a session that reaches MORE than was asked for.
        let req = RunRequest {
            agent: None,
            name: None,
            prompt: None,
            args: vec![],
            cwd: "/home/t/p".into(),
            policy: AgentPolicy {
                network: crate::policy::NetworkPolicy::Allowlist,
                ..AgentPolicy::default()
            },
            request_origin: None,
            worktree: None,
            checkpoint: false,
            ttl_ms: None,
            capabilities: None,
            allow: Some(vec!["api.example.com".into(), "files.example.com:8443".into()]),
            trust_ca: None,
            present: None,
            second_factor: None,
            cols: 80,
            rows: 24,
            env: vec![],
            disposable: false,
            copy_out: None,
        };
        let v: serde_json::Value = serde_json::to_value(&req).expect("serialize");
        assert_eq!(
            v["allow"],
            serde_json::json!(["api.example.com", "files.example.com:8443"]),
            "the wire key is `allow`; a daemon reading any other name narrows nothing"
        );

        // The other direction, which is the one an older client exercises on
        // every single run: no key at all deserializes to `None`, not to an
        // empty list. `Some(vec![])` is refused by the daemon, so a `default`
        // that produced one would refuse every unnarrowed session on a
        // machine where nothing is wrong.
        let mut without = v.clone();
        without.as_object_mut().expect("object").remove("allow");
        let back: RunRequest = serde_json::from_value(without).expect("an older client's request");
        assert_eq!(back.allow, None);

        // And an explicit `null` — which is not what `rime` sends, but is
        // what a hand-written client or a future language binding may — reads
        // the same way rather than failing to parse.
        let mut nulled = v.clone();
        nulled["allow"] = serde_json::Value::Null;
        let back: RunRequest = serde_json::from_value(nulled).expect("an explicit null");
        assert_eq!(back.allow, None);
    }

    #[test]
    fn a_browser_ca_travels_as_trust_ca_and_its_absence_is_none() {
        // P2-012's gap 5, and the assertion is the KEY for the reason the one
        // above it is: a rename passes every round trip in this file while
        // making the field invisible to a daemon that reads the old name.
        // Which for THIS field is not a widening — the capsule trusts less,
        // not more — but it is a capsule that renders nothing and blames the
        // timeout, so the silence is the cost.
        let req = RunRequest {
            agent: None,
            name: None,
            prompt: None,
            args: vec![],
            cwd: "/home/t/p".into(),
            policy: AgentPolicy::default(),
            request_origin: None,
            worktree: None,
            checkpoint: false,
            ttl_ms: None,
            capabilities: None,
            allow: None,
            trust_ca: Some("/home/t/intranet-root.pem".into()),
            present: None,
            second_factor: None,
            cols: 80,
            rows: 24,
            env: vec![],
            disposable: false,
            copy_out: None,
        };
        let v: serde_json::Value = serde_json::to_value(&req).expect("serialize");
        assert_eq!(
            v["trust_ca"],
            serde_json::json!("/home/t/intranet-root.pem"),
            "the wire key is `trust_ca`; a daemon reading any other name installs nothing"
        );

        // The direction every older client exercises on every run: no key at
        // all is `None`, which is a session whose browser trusts exactly what
        // the machine trusts.
        let mut without = v.clone();
        without.as_object_mut().expect("object").remove("trust_ca");
        let back: RunRequest = serde_json::from_value(without).expect("an older client's request");
        assert_eq!(back.trust_ca, None);

        let mut nulled = v.clone();
        nulled["trust_ca"] = serde_json::Value::Null;
        let back: RunRequest = serde_json::from_value(nulled).expect("an explicit null");
        assert_eq!(back.trust_ca, None);
    }

    #[test]
    fn a_presented_credential_travels_as_present_and_its_absence_is_none() {
        // P2-012's route B, and the assertion is the KEY for the reason the
        // one above it is — with the direction back the other way. A rename
        // here is a daemon that tunnels the capsule's CONNECT untouched, so
        // the capsule visits the site as nobody and the screenshot looks
        // plausible. Nothing anywhere says the credential was not presented.
        let req = RunRequest {
            agent: None,
            name: None,
            prompt: None,
            args: vec![],
            cwd: "/home/t/p".into(),
            policy: AgentPolicy::default(),
            request_origin: None,
            worktree: None,
            checkpoint: false,
            ttl_ms: None,
            capabilities: None,
            allow: Some(vec!["intranet.example:443".into()]),
            trust_ca: None,
            present: Some("intranet".into()),
            second_factor: None,
            cols: 80,
            rows: 24,
            env: vec![],
            disposable: false,
            copy_out: None,
        };
        let v: serde_json::Value = serde_json::to_value(&req).expect("serialize");
        assert_eq!(
            v["present"],
            serde_json::json!("intranet"),
            "the wire key is `present`; a daemon reading any other name authenticates nothing"
        );
        // It names the CREDENTIAL and never the destination. A second key for
        // where to spend it would be a second thing that can disagree with the
        // first, and the request would settle the disagreement in favour of
        // whichever one the caller wrote.
        for forbidden in ["present_host", "present_destination", "intercept"] {
            assert!(
                v.get(forbidden).is_none(),
                "the run request grew a {forbidden} field"
            );
        }

        let mut without = v.clone();
        without.as_object_mut().expect("object").remove("present");
        let back: RunRequest = serde_json::from_value(without).expect("an older client's request");
        assert_eq!(back.present, None);

        let mut nulled = v.clone();
        nulled["present"] = serde_json::Value::Null;
        let back: RunRequest = serde_json::from_value(nulled).expect("an explicit null");
        assert_eq!(back.present, None);
    }

    #[test]
    fn only_the_requests_that_authenticate_wait_on_a_person() {
        // The polkit dialog is on the desktop and the person may take a
        // minute or ten; the CLI must not give up on the socket and report a
        // connection failure while the dialog is still up.
        use crate::policy::SystemAccess;
        let run = |system| {
            Request::Run(Box::new(RunRequest {
                agent: None,
                name: None,
                prompt: None,
                args: vec![],
                cwd: "/home/t/p".into(),
                policy: AgentPolicy {
                    system,
                    ..AgentPolicy::default()
                },
                request_origin: None,
                worktree: None,
                checkpoint: false,
                ttl_ms: None,
                capabilities: None,
                allow: None,
                trust_ca: None,
            present: None,
                second_factor: None,
                cols: 80,
                rows: 24,
                env: vec![],
                disposable: false,
                copy_out: None,
            }))
        };
        assert!(run(SystemAccess::Session).waits_on_a_human());
        assert!(run(SystemAccess::Unsafe).waits_on_a_human());
        assert!(!run(SystemAccess::None).waits_on_a_human());
        assert!(Request::RenewSystemGrant {
            id: 1,
            ttl_ms: 60_000,
            second_factor: None
        }
        .waits_on_a_human());
        // Giving privilege up asks nobody, so it keeps its deadline.
        assert!(!Request::RevokeSystemGrant { id: 1 }.waits_on_a_human());
        assert!(!Request::SystemGrants.waits_on_a_human());
        assert!(!Request::List.waits_on_a_human());
    }

    #[test]
    fn every_request_variant_round_trips() {
        let variants = vec![
            Request::Hello,
            Request::ClipboardWrite {
                text: "two\nlines \"quoted\"".into(),
            },
            Request::SaveFile {
                name: "../IMG_2041.jpg".into(),
                len: 2048,
            },
            Request::Inject {
                id: 3,
                source: "/home/t/Pictures/Screenshots/shot.png".into(),
            },
            Request::Run(Box::new(RunRequest {
                agent: Some("claude".into()),
                name: None,
                prompt: Some("go".into()),
                args: vec!["--verbose".into()],
                cwd: "/home/t/p".into(),
                policy: AgentPolicy {
                    sandbox: SandboxPolicy::Strict,
                    ..AgentPolicy::default()
                },
                request_origin: Some(RequestOrigin::RemoteControl),
                worktree: Some("issue-217".into()),
                checkpoint: true,
                ttl_ms: None,
                capabilities: None,
                allow: None,
                trust_ca: None,
            present: None,
                // Carried through the round trip with a value, not `None`:
                // the field is the one thing on this request that a daemon
                // reads to decide whether root is handed out, and a
                // round-trip test that only ever sends `None` would not
                // notice a rename.
                second_factor: Some(SubmittedFactor {
                    nonce: "bm9uY2U=".into(),
                    credential: "yubikey".into(),
                    assertion: "Y2RoCmFwZXgtYWdlbnQubG9jYWxob3N0\nrime-agent.localhost\nWCU=\nMEQ=\n"
                        .into(),
                }),
                cols: 80,
                rows: 24,
                env: vec![("K".into(), "V".into())],
                disposable: false,
                copy_out: None,
            })),
            Request::Run(Box::new(RunRequest {
                // A disposable run, so the two new keys cross the wire in the
                // round-trip too rather than only in their default form.
                agent: Some("claude".into()),
                name: None,
                prompt: Some("review this".into()),
                args: vec![],
                cwd: "/home/t/p".into(),
                policy: AgentPolicy {
                    sandbox: SandboxPolicy::Unrestricted,
                    ..AgentPolicy::default()
                },
                request_origin: None,
                worktree: None,
                checkpoint: false,
                ttl_ms: None,
                capabilities: None,
                allow: None,
                trust_ca: None,
            present: None,
                second_factor: None,
                cols: 80,
                rows: 24,
                env: vec![],
                disposable: true,
                copy_out: Some("/home/t/results".into()),
            })),
            Request::List,
            Request::PrivilegeRequest {
                verb: "install".into(),
                args: vec!["clang".into()],
                reason: "Required to compile the project".into(),
            },
            Request::Requests,
            Request::Decide {
                id: 1,
                decision: "once".into(),
            },
            Request::RequestExecuted { id: 1, exit_code: 0 },
            Request::Grants,
            Request::SystemGrants,
            Request::RevokeSystemGrant { id: 3 },
            Request::RenewSystemGrant {
                id: 3,
                ttl_ms: 900_000,
                second_factor: None,
            },
            Request::ElevationChallenge {
                session: None,
                kind: GrantKind::BreakGlass,
                ttl_ms: 900_000,
                credential: Some("yubikey".into()),
            },
            Request::Revoke {
                project: "/home/t/p".into(),
                key: Some("install:clang".into()),
            },
            Request::SecretUse {
                service: "github".into(),
                operation: "git.push".into(),
                resource: "origin".into(),
                params: BTreeMap::from([("branch".to_string(), "feat/x".to_string())]),
                body: None,
                project: Some("/home/t/p".into()),
            },
            Request::Info { id: 1 },
            Request::Attach {
                id: 1,
                cols: 80,
                rows: 24,
                replay: 1024,
            },
            Request::Resize {
                id: 1,
                cols: 100,
                rows: 30,
            },
            Request::Signal {
                id: 1,
                signal: "term".into(),
            },
            Request::Input {
                id: 1,
                // A transcript with a carriage return in it, because that is
                // the byte most likely to be mangled on the way through JSON —
                // and what an older client still appends in place of `submit`.
                data: "run the tests\r".into(),
                submit: false,
            },
            Request::Input {
                id: 1,
                data: "run the tests".into(),
                submit: true,
            },
            Request::Rename {
                id: 1,
                name: Some("auth refactor".into()),
            },
            Request::Rename { id: 1, name: None },
            Request::Peek { id: 1, bytes: Some(4096) },
            Request::Peek { id: 1, bytes: None },
            Request::Event {
                id: 1,
                state: Some("working".into()),
                event: Some("pre_tool_use".into()),
                detail: Some("d".into()),
                native: Some("bypassPermissions".into()),
                agent_id: None,
                agent_type: None,
                test: Some(crate::worktree::TestNote {
                    phase: crate::worktree::TestPhase::Started,
                    command: "cargo test".into(),
                }),
            },
            Request::Event {
                id: 1,
                state: None,
                event: Some("task_created".into()),
                detail: None,
                native: None,
                agent_id: None,
                agent_type: None,
                test: None,
            },
            Request::Event {
                id: 1,
                state: Some("working".into()),
                event: Some("subagent_start".into()),
                detail: Some("Explore started".into()),
                native: None,
                agent_id: Some("a-1".into()),
                agent_type: Some("Explore".into()),
                test: None,
            },
            Request::ToolCheck {
                id: 1,
                tool_name: "Bash".into(),
                tool_input: serde_json::json!({"command": "ls"}),
            },
            Request::Logs { id: 1, bytes: 100 },
            Request::Remove { id: 1 },
            Request::Prune,
            Request::DeclareOrigin {
                origin: "claude-remote-control".into(),
                actor: Some("pixel-8-office".into()),
            },
            Request::Worktrees { project: None },
            Request::Worktrees {
                project: Some("rime-os".into()),
            },
            // Unit variants, so the round trip is testing that the tag alone
            // is a whole request — `{"cmd":"projects"}` with no other key. A
            // client that sent `{"cmd":"projects","project":null}` instead
            // would still parse, but the shape asserted here is the one the
            // Android builder emits and the one the fixture carries.
            Request::Projects,
            Request::Profiles,
        ];

        for v in variants {
            let text = serde_json::to_string(&v)
                .unwrap_or_else(|e| panic!("{v:?} does not serialise: {e}"));
            assert!(!text.contains('\n'), "{text} would break NDJSON framing");
            let _: Request = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{v:?} does not round-trip: {e} from {text}"));
        }
    }

    #[test]
    fn ndjson_framing_never_embeds_a_newline() {
        // The framing is line-based, so a serialised request that contained a
        // raw newline would desynchronise the stream. serde_json escapes them;
        // this asserts that rather than assuming it.
        let req = Request::Event {
            id: 1,
            state: Some("working".into()),
            event: None,
            detail: Some("line one\nline two".into()),
            native: None,
            agent_id: None,
            agent_type: None,
            test: None,
        };
        let text = serde_json::to_string(&req).unwrap();
        assert!(!text.contains('\n'), "{text}");
        let back: Request = serde_json::from_str(&text).unwrap();
        match back {
            Request::Event { detail, .. } => {
                assert_eq!(detail.as_deref(), Some("line one\nline two"))
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn the_six_dimensions_sit_beside_sandbox_and_not_under_it() {
        // Rime Shell reads `sandbox` from the top level of a session record,
        // and so does every record already on disk. Nesting the dimensions
        // under a `policy` object would have moved that key, and the symptom
        // would have been every existing session quietly listed as `project`.
        use crate::policy::NativeMode;

        let mut s = sample_session();
        s.policy.sandbox = SandboxPolicy::Strict;
        s.policy.native = NativeMode::Bypass;
        let text = serde_json::to_string(&s).expect("serialise");
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["sandbox"], "strict", "{text}");
        assert_eq!(v["native"], "bypass", "{text}");
        assert!(v.get("policy").is_none(), "the policy must not be nested: {text}");

        let back: SessionInfo = serde_json::from_str(&text).expect("deserialise");
        assert_eq!(back.policy, s.policy);
    }

    #[test]
    fn a_session_record_written_before_the_split_still_loads() {
        // Same shape as the `paused` test: strip the keys an older daemon did
        // not write and assert the record comes back at the safe defaults,
        // never loose. A dropped record would lose a running session from the
        // Agent Center; a record that loaded with the network open would be
        // worse.
        use crate::policy::{NetworkPolicy, SecretPolicy, SystemAccess};

        let s = sample_session();
        let text = serde_json::to_string(&s).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        {
            let obj = v.as_object_mut().unwrap();
            for key in ["native", "system", "secrets", "network", "origin"] {
                assert!(obj.remove(key).is_some(), "{key} was never written");
            }
        }
        let old: SessionInfo = serde_json::from_value(v).expect("an old record still loads");
        assert_eq!(old.policy, AgentPolicy::default());
        assert_eq!(old.policy.system, SystemAccess::None);
        assert_eq!(old.policy.secrets, SecretPolicy::Brokered);
        assert_eq!(old.policy.network, NetworkPolicy::Open);
        assert_eq!(old.policy.sandbox, SandboxPolicy::Project);
    }

    #[test]
    fn a_run_request_from_a_pre_split_client_keeps_its_sandbox() {
        // The wire form an older `rime agent run --sandbox strict` sends. It
        // must not become `project`, and it must not become unconfined.
        let req: RunRequest =
            serde_json::from_str(r#"{"cwd":"/tmp","sandbox":"strict","cols":80,"rows":24}"#)
                .expect("parse");
        assert_eq!(req.policy.sandbox, SandboxPolicy::Strict);
        assert_eq!(
            req.policy,
            AgentPolicy { sandbox: SandboxPolicy::Strict, ..AgentPolicy::default() }
        );
    }

    #[test]
    fn an_event_from_a_pre_hook_client_still_publishes_its_state() {
        // The wire form every existing `rime agent event working` sends. The
        // two new keys are additive, so this must keep parsing unchanged —
        // that is the whole reason the hook bridge is not a protocol bump.
        let req: Request =
            serde_json::from_str(r#"{"cmd":"event","id":4,"state":"working"}"#).expect("parse");
        match req {
            Request::Event { id, state, event, detail, .. } => {
                assert_eq!(id, 4);
                assert_eq!(state.as_deref(), Some("working"));
                assert_eq!(event, None);
                assert_eq!(detail, None);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn an_event_may_record_a_lifecycle_without_claiming_a_state() {
        // §6.1 asks for task, compaction, config and worktree events. None of
        // them says what the session is doing, and inventing a state for them
        // would overwrite a truthful one.
        let req: Request =
            serde_json::from_str(r#"{"cmd":"event","id":4,"event":"task_created"}"#)
                .expect("parse");
        match req {
            Request::Event { state, event, .. } => {
                assert_eq!(state, None);
                assert_eq!(event.as_deref(), Some("task_created"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn every_version_guard_names_a_revision_that_exists() {
        // The CLI refuses to send a setting to a daemon older than the
        // revision that introduced it, because an old daemon drops the key
        // and runs without the restriction. Each guard therefore has to name
        // a revision at or below the current one — a guard pointing at a
        // future version would refuse every daemon, and one pointing past the
        // current version cannot be reached at all.
        for (name, since) in [
            ("the six dimensions", POLICY_DIMENSIONS_VERSION),
            ("request_origin", REQUEST_ORIGIN_VERSION),
            ("the secret service", BROKERED_SECRET_SERVICE_VERSION),
            ("generic capabilities", GENERIC_CAPABILITY_VERSION),
            ("the mcp bridge", MCP_BRIDGE_VERSION),
            ("system-access grants", SYSTEM_GRANT_VERSION),
            ("scoped grants", SCOPED_GRANT_VERSION),
            ("the connector policy", CONNECTOR_POLICY_VERSION),
            ("the plugin policy", PLUGIN_POLICY_VERSION),
            ("the session allowlist", SESSION_ALLOWLIST_VERSION),
            ("the browser CA", BROWSER_CA_VERSION),
            ("the presented credential", BROWSER_PRESENT_VERSION),
        ] {
            assert!(
                since <= PROTOCOL_VERSION,
                "{name} claims to arrive in protocol {since}, which is ahead of {PROTOCOL_VERSION}"
            );
            assert!(since > 0, "{name} has no revision");
        }
        // The newest guard is the current revision: adding a wire field
        // without bumping the version is the fail-open these exist to catch.
        assert_eq!(BROWSER_PRESENT_VERSION, PROTOCOL_VERSION);
        // And every older guard stays strictly behind it. `<`, not
        // `== PROTOCOL_VERSION - 1`: three of these shipped as revision 5 and
        // scoped grants as 6, and none of them is going to move again, so
        // pinning them one below the current version would break them all on
        // the next bump for no reason anybody could act on.
        for (name, since) in [
            ("generic capabilities", GENERIC_CAPABILITY_VERSION),
            ("the mcp bridge", MCP_BRIDGE_VERSION),
            ("system-access grants", SYSTEM_GRANT_VERSION),
            ("scoped grants", SCOPED_GRANT_VERSION),
            ("the connector policy", CONNECTOR_POLICY_VERSION),
            ("the plugin policy", PLUGIN_POLICY_VERSION),
            ("the session allowlist", SESSION_ALLOWLIST_VERSION),
            ("the browser CA", BROWSER_CA_VERSION),
        ] {
            assert!(
                since < PROTOCOL_VERSION,
                "the guard for {name} is pinned at {since} and PROTOCOL_VERSION is \
                 {PROTOCOL_VERSION}: a guard for an older revision must stay strictly \
                 behind the current one, or a peer that predates it is told it is current"
            );
        }
    }

    #[test]
    fn session_info_reports_liveness_and_exit() {
        let mut info = SessionInfo {
            id: 1,
            agent: "generic".into(),
            program: "sh".into(),
            args: vec![],
            cwd: "/tmp".into(),
            project: None,
            project_name: None,
            name: None,
            title: None,
            worktree: None,
            state: AgentState::Working,
            detail: None,
            paused: false,
            policy: AgentPolicy::default(),
            allowlist: None,
            request_origin: None,
            origin_source: None,
            actor: None,
            grant: None,
            grant_expires_ms: None,
            native_observed: None,
            telemetry: None,
        children: Vec::new(),
            pid: 123,
            started: 0,
            last_activity: 0,
            exit_code: None,
            exit_signal: None,
            attached: 0,
            checkpoint: None,
            cols: 80,
            rows: 24,
            injected: 0,
            capsule: None,
        };
        assert!(info.is_live());
        assert_eq!(info.exit_summary(), None);

        info.exit_code = Some(2);
        assert!(!info.is_live());
        assert_eq!(info.exit_summary().as_deref(), Some("exited 2"));

        info.exit_code = None;
        info.exit_signal = Some(9);
        assert!(!info.is_live());
        assert_eq!(info.exit_summary().as_deref(), Some("killed by signal 9"));
    }

    // ── docs/remote-live-contract.md §1: the exact JSON the other sides send ──
    //
    // Parsed from the contract's own strings, not from a round trip. A round
    // trip agrees with itself whatever a key is called; the phone, the Shell
    // and rime-remoted were built against these bytes, so these bytes are what
    // has to parse.

    #[test]
    fn every_advertised_feature_is_the_contracts() {
        // A client matches these byte for byte, so a rename here is a phone
        // that silently stops using the feature.
        assert_eq!(
            FEATURES,
            &[
                "input_submit",
                "rename",
                "peek",
                "session_name",
                "session_title",
                "size_restore",
                "clipboard_write",
                "save_file",
            ]
        );
    }

    #[test]
    fn the_contracts_hello_reply_parses_and_an_old_one_means_no_features() {
        let line = r#"{"reply":"hello","version":11,"agents":["claude"],"default_agent":"claude",
 "features":["input_submit","rename","peek","session_name","session_title","size_restore",
 "clipboard_write","save_file"]}"#;
        match serde_json::from_str::<Response>(line).expect("the contract's hello") {
            Response::Hello { version, agents, default_agent, features } => {
                assert_eq!(version, 11);
                assert_eq!(agents, vec!["claude".to_string()]);
                assert_eq!(default_agent, "claude");
                assert_eq!(features, FEATURES);
            }
            other => panic!("wrong variant: {other:?}"),
        }
        // An old daemon's hello has no `features` key, and that is "none",
        // not a reply a new client cannot read.
        let old = r#"{"reply":"hello","version":11,"agents":["claude"],"default_agent":"claude"}"#;
        match serde_json::from_str::<Response>(old).expect("an old daemon's hello") {
            Response::Hello { features, .. } => assert!(features.is_empty()),
            other => panic!("wrong variant: {other:?}"),
        }
        // And the key is written by this build, so a client can find it.
        let v = serde_json::to_value(Response::Hello {
            version: PROTOCOL_VERSION,
            agents: vec![],
            default_agent: "claude".into(),
            features: FEATURES.iter().map(|f| f.to_string()).collect(),
        })
        .unwrap();
        assert_eq!(v["features"][0], "input_submit");
    }

    #[test]
    fn the_contracts_input_with_submit_parses_and_submit_defaults_off() {
        match serde_json::from_str::<Request>(
            r#"{"cmd":"input","id":3,"data":"yes please","submit":true}"#,
        )
        .expect("the contract's input")
        {
            Request::Input { id, data, submit } => {
                assert_eq!(id, 3);
                assert_eq!(data, "yes please", "the daemon must not strip or add a byte");
                assert!(submit);
            }
            other => panic!("wrong variant: {other:?}"),
        }
        // Every client written before the flag: no key, no Enter.
        match serde_json::from_str::<Request>(r#"{"cmd":"input","id":3,"data":"yes\r"}"#).unwrap()
        {
            Request::Input { submit, data, .. } => {
                assert!(!submit);
                assert_eq!(data, "yes\r");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn the_contracts_rename_parses_with_a_name_and_with_null() {
        match serde_json::from_str::<Request>(r#"{"cmd":"rename","id":3,"name":"auth refactor"}"#)
            .expect("rename with a name")
        {
            Request::Rename { id, name } => {
                assert_eq!(id, 3);
                assert_eq!(name.as_deref(), Some("auth refactor"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
        for clear in [r#"{"cmd":"rename","id":3,"name":null}"#, r#"{"cmd":"rename","id":3}"#] {
            match serde_json::from_str::<Request>(clear).expect(clear) {
                Request::Rename { id: 3, name: None } => {}
                other => panic!("{clear} parsed as {other:?}"),
            }
        }
    }

    #[test]
    fn the_contracts_peek_parses_with_and_without_bytes() {
        match serde_json::from_str::<Request>(r#"{"cmd":"peek","id":3,"bytes":8192}"#)
            .expect("peek with bytes")
        {
            Request::Peek { id: 3, bytes: Some(8192) } => {}
            other => panic!("wrong variant: {other:?}"),
        }
        match serde_json::from_str::<Request>(r#"{"cmd":"peek","id":3}"#).expect("peek") {
            Request::Peek { id: 3, bytes: None } => {}
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn the_contracts_peek_reply_parses_and_this_build_writes_the_same_keys() {
        let line = r#"{"reply":"peek","id":3,"data":"G1swbWhp","cols":120,"rows":40,"state":"working"}"#;
        match serde_json::from_str::<Response>(line).expect("the contract's peek reply") {
            Response::Peek { id, data, cols, rows, state } => {
                assert_eq!((id, cols, rows), (3, 120, 40));
                assert_eq!(data, "G1swbWhp");
                assert_eq!(state, AgentState::Working);
            }
            other => panic!("wrong variant: {other:?}"),
        }
        let v = serde_json::to_value(Response::Peek {
            id: 3,
            data: "G1swbWhp".into(),
            cols: 120,
            rows: 40,
            state: AgentState::Working,
        })
        .unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["cols", "data", "id", "reply", "rows", "state"]);
        assert_eq!(v["reply"], "peek");
    }

    #[test]
    fn name_and_title_are_always_on_the_wire_even_when_empty() {
        // `"name": null` is "this daemon has names, none is set"; a missing key
        // is "this daemon predates names". A `skip_serializing_if` would make
        // the two indistinguishable, and a client decides whether to offer
        // Rename from exactly that.
        let v = serde_json::to_value(sample_session()).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(obj.get("name"), Some(&serde_json::Value::Null), "{v}");
        assert_eq!(obj.get("title"), Some(&serde_json::Value::Null), "{v}");

        let mut named = sample_session();
        named.name = Some("auth refactor".into());
        named.title = Some("Rime showcase studio".into());
        let v = serde_json::to_value(&named).unwrap();
        assert_eq!(v["name"], "auth refactor");
        assert_eq!(v["title"], "Rime showcase studio");
        let back: SessionInfo = serde_json::from_value(v).unwrap();
        assert_eq!(back.name.as_deref(), Some("auth refactor"));
        assert_eq!(back.title.as_deref(), Some("Rime showcase studio"));
    }

    #[test]
    fn a_session_record_written_before_names_still_loads() {
        // Every record on every machine today. It must load as a session
        // nobody has named and whose agent never titled itself.
        let mut v = serde_json::to_value(sample_session()).unwrap();
        let obj = v.as_object_mut().unwrap();
        assert!(obj.remove("name").is_some());
        assert!(obj.remove("title").is_some());
        let old: SessionInfo = serde_json::from_value(v).expect("an old record still loads");
        assert_eq!(old.name, None);
        assert_eq!(old.title, None);
    }

    #[test]
    fn a_run_request_carries_its_name_as_name_and_an_older_client_sends_none() {
        let req: RunRequest = serde_json::from_str(
            r#"{"cwd":"/tmp","cols":80,"rows":24,"name":"auth refactor"}"#,
        )
        .expect("a named run");
        assert_eq!(req.name.as_deref(), Some("auth refactor"));
        let req: RunRequest =
            serde_json::from_str(r#"{"cwd":"/tmp","cols":80,"rows":24}"#).expect("an old run");
        assert_eq!(req.name, None);
    }
}
