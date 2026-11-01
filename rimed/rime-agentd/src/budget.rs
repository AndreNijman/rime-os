//! Per-agent resource budgets (roadmap §P2-011).
//!
//! An agent session can be given a cpu, memory, task-count and wall-clock
//! budget by running it inside a transient systemd **user scope**. The whole
//! feature is an argv prefix: `systemd-run --user --scope -q --unit=… -p … --`
//! in front of the argv the session would have run anyway.
//!
//! ## Why a transient scope and not a drop-in
//!
//! `rime-agentd.service` has no `Delegate=`, so it cannot make sub-cgroups of
//! its own. The obvious conclusion — "budgets need a drop-in first" — is
//! wrong, and it was measured: `systemd-run --user --scope` asks the *user
//! manager* to make the cgroup, which needs no delegation, no polkit prompt
//! and no change to the shipped unit. It also **execs in place**: systemd-run
//! creates the scope over the bus and then `execve`s the command in its own
//! process, so the pid the daemon records is still the pid of the real program
//! chain, the process group is unchanged, and everything that counts processes
//! — `peer.rs`'s parent-chain resolution, `rime agent kill`'s killpg — is
//! untouched. That property is what makes the hook one line.
//!
//! ## The polarity, which is the whole design
//!
//! Three of the four controllers are delegated on this class of machine and
//! `io` is not — it is in the root cgroup's `cgroup.controllers` but never in
//! its `subtree_control`, so no `io.max` file exists anywhere under
//! `user.slice`. The rule this module is built around, and the one the rest of
//! this unit kept re-learning the hard way:
//!
//! * a controller that **was read** and is **absent** is a fact. Report it,
//!   carrying the reason, and leave it out of the argv. **Never write a budget
//!   of zero** — an unenforceable limit expressed as `0` is not a conservative
//!   default, it is a kill switch wearing one.
//! * a controllers file that **could not be read** is an unknown, not an
//!   absence. The caller asked for a budget and the daemon cannot tell whether
//!   it would be honoured, so the session is refused rather than started
//!   unbudgeted. A budget that silently is not there is the failure this whole
//!   unit exists to stop shipping.
//!
//! And the file to read is the delegation boundary's **`cgroup.controllers`**,
//! never its `cgroup.subtree_control`: systemd enables a controller in
//! `subtree_control` *on demand*, when a child unit first asks for it, so
//! `subtree_control` answers "cpu is not enforceable here" on a machine where
//! it is — and answers differently five seconds later. Measured on the L16:
//! `app.slice/cgroup.controllers` is `cpu memory pids` while its
//! `subtree_control` is `memory pids`, and `cpu` appears in the latter the
//! moment any child scope asks for `CPUQuota`.

use std::path::{Path, PathBuf};

use rime_agent_core::config::Config;

/// The absolute path to systemd-run.
///
/// Absolute for the same reason `WIPEFS` and `FWUPDMGR` are in the CLI: this
/// prefix is prepended to an agent's argv, so a `PATH` lookup would let
/// anything earlier on the daemon's `PATH` become the program every budgeted
/// session actually execs.
pub const SYSTEMD_RUN: &str = rime_paths::SYSTEMD_RUN;

/// The prefix every scope this module creates carries.
///
/// See [`ScopeName::new`] for why the *shape* of this name is a security
/// property and not decoration.
pub const SCOPE_PREFIX: &str = "rime-agent-";

/// A value, or the reason there is no value.
///
/// Deliberately not `Option`. `Option` has `unwrap_or_default`, and a budget
/// defaulting to zero is precisely the bug this module is written to make
/// unrepresentable. The same type and the same reasoning as
/// `rimed_core::storage::Reading`; it is re-declared here rather than shared
/// because `rime-agentd` must not gain a dependency on the CLI's core crate
/// for a twenty-line enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading<T> {
    /// The read happened and this is what it said.
    Known(T),
    /// The read did not happen, and this is why.
    Unavailable(String),
}

impl<T> Reading<T> {
    /// The reason there is no value, if there is no value.
    pub fn why(&self) -> Option<&str> {
        match self {
            Reading::Known(_) => None,
            Reading::Unavailable(why) => Some(why),
        }
    }
}

/// The four resources a budget can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    Cpu,
    Memory,
    Pids,
    Io,
}

impl Resource {
    /// The cgroup controller that has to be delegated for this to be enforced.
    pub fn controller(self) -> &'static str {
        match self {
            Resource::Cpu => "cpu",
            Resource::Memory => "memory",
            Resource::Pids => "pids",
            Resource::Io => "io",
        }
    }
}

/// What the user asked for.
///
/// Every field is optional and **none of them can be zero** — see
/// [`Budget::parse`]. Wall-clock runtime has no default and never gains one: a
/// coding agent killed mid-thought at an arbitrary wall-clock deadline is a
/// regression, not a safety feature, so `RuntimeMaxSec` exists only when a
/// human wrote it down.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Budget {
    /// `MemoryMax=`, in bytes.
    pub memory_max: Option<u64>,
    /// `CPUQuota=`, in percent. May exceed 100 on a multi-core machine.
    pub cpu_percent: Option<u32>,
    /// `TasksMax=`.
    pub tasks_max: Option<u32>,
    /// `RuntimeMaxSec=`, in seconds.
    pub runtime_secs: Option<u64>,
    /// Bytes per second of read bandwidth. Not enforceable on a machine where
    /// `io` is not delegated, which is every machine this has been measured
    /// on; kept because the refusal has to name something the user wrote.
    pub io_read_bps: Option<u64>,
    /// Bytes per second of write bandwidth. See `io_read_bps`.
    pub io_write_bps: Option<u64>,
}

impl Budget {
    /// True when nothing was asked for, which is the default and the fast path.
    ///
    /// An empty budget means **no wrapper at all**: the argv a session runs is
    /// byte-identical to the argv it ran before this module existed, and not
    /// one syscall is spent deciding that.
    pub fn is_empty(&self) -> bool {
        *self == Budget::default()
    }

    /// Which resources this budget actually names.
    pub fn named(&self) -> Vec<Resource> {
        let mut out = Vec::new();
        if self.cpu_percent.is_some() {
            out.push(Resource::Cpu);
        }
        if self.memory_max.is_some() {
            out.push(Resource::Memory);
        }
        if self.tasks_max.is_some() {
            out.push(Resource::Pids);
        }
        if self.io_read_bps.is_some() || self.io_write_bps.is_some() {
            out.push(Resource::Io);
        }
        out
    }

    /// Read the `budget` stanza out of a configuration's unknown-key map.
    ///
    /// `Config.extra` is `#[serde(flatten)]`, so a top-level `budget` object in
    /// `agent.json` arrives here and **`rime-agent-core` needs no change at
    /// all** — which matters because P1-020 made that crate's files a merge
    /// surface.
    ///
    /// Unlike `Config::load`, a malformed stanza is an **error and not the
    /// default**. The file-level rule ("a corrupt preferences file degrades to
    /// the defaults") is right for a preference and wrong for this: degrading
    /// a mistyped budget to "no budget" hands the user an unconfined agent and
    /// a config file that looks like it says otherwise. A typo must be loud.
    pub fn parse(extra: &serde_json::Map<String, serde_json::Value>) -> Result<Budget, String> {
        let Some(value) = extra.get("budget") else {
            return Ok(Budget::default());
        };
        let Some(obj) = value.as_object() else {
            return Err(format!(
                "the `budget` setting must be an object, not {}",
                kind_of(value)
            ));
        };

        let mut out = Budget::default();
        for (key, v) in obj {
            match key.as_str() {
                "memory_max" => out.memory_max = Some(bytes(key, v)?),
                "cpu_percent" => out.cpu_percent = Some(count(key, v)? as u32),
                "tasks_max" => out.tasks_max = Some(count(key, v)? as u32),
                "runtime_secs" => out.runtime_secs = Some(count(key, v)?),
                "io_read_bps" => out.io_read_bps = Some(bytes(key, v)?),
                "io_write_bps" => out.io_write_bps = Some(bytes(key, v)?),
                other => {
                    return Err(format!(
                        "unknown key {other:?} in the `budget` setting; \
                         known keys are memory_max, cpu_percent, tasks_max, \
                         runtime_secs, io_read_bps, io_write_bps"
                    ))
                }
            }
        }
        Ok(out)
    }
}

fn kind_of(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

/// A plain positive count. Zero is refused — see [`positive`].
fn count(key: &str, v: &serde_json::Value) -> Result<u64, String> {
    let n = v
        .as_u64()
        .ok_or_else(|| format!("`budget.{key}` must be a positive whole number, not {}", kind_of(v)))?;
    positive(key, n)
}

/// A byte count, as a number or as a string with a `K`/`M`/`G`/`T` suffix.
///
/// The suffix form exists because the alternative is asking a human to write
/// `536870912` in a preferences file, and a human who miscounts the digits
/// there gets a budget off by a factor of ten with nothing to notice it by.
fn bytes(key: &str, v: &serde_json::Value) -> Result<u64, String> {
    let n = match v {
        serde_json::Value::Number(_) => v
            .as_u64()
            .ok_or_else(|| format!("`budget.{key}` must be a positive whole number of bytes"))?,
        serde_json::Value::String(s) => parse_size(s)
            .ok_or_else(|| format!("`budget.{key}`: {s:?} is not a size like \"512M\""))?,
        other => {
            return Err(format!(
                "`budget.{key}` must be a number of bytes or a size like \"512M\", not {}",
                kind_of(other)
            ))
        }
    };
    positive(key, n)
}

/// Zero is not a budget.
///
/// `MemoryMax=0` kills the agent at its first allocation and `TasksMax=0` stops
/// it forking at all, so a configuration file that says `0` describes a session
/// that cannot run. Refusing it at parse time is what makes "never a budget of
/// zero" a property of the type rather than a rule later code has to remember.
fn positive(key: &str, n: u64) -> Result<u64, String> {
    if n == 0 {
        return Err(format!(
            "`budget.{key}` is 0, which is not a limit — it is a session that \
             cannot run. Remove the key to leave {key} unbudgeted."
        ));
    }
    Ok(n)
}

/// `"512M"` -> 536870912. Binary multipliers, because that is what systemd's
/// own `MemoryMax=` uses and two units in one file would be a trap.
pub fn parse_size(s: &str) -> Option<u64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let (digits, mult) = match t.chars().last()? {
        'K' | 'k' => (&t[..t.len() - 1], 1024u64),
        'M' | 'm' => (&t[..t.len() - 1], 1024 * 1024),
        'G' | 'g' => (&t[..t.len() - 1], 1024 * 1024 * 1024),
        'T' | 't' => (&t[..t.len() - 1], 1024u64 * 1024 * 1024 * 1024),
        _ => (t, 1),
    };
    let n: u64 = digits.trim().parse().ok()?;
    n.checked_mul(mult)
}

/// The name of the transient scope a session runs in.
///
/// The private field is the point: `new` is the only way to make one, so the
/// validation below cannot be skipped by constructing the struct directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeName(String);

impl ScopeName {
    /// Build the scope name for one session.
    ///
    /// **The shape of this name is a privilege boundary.** `origin::classify`
    /// decides §7 origin by substring over the peer's cgroup path:
    /// `"/session-"` plus `".scope"` means a login session — `LocalTerminal`
    /// or `RimeShell`, the origins §7 reserves root and break-glass for —
    /// while `"/user@"` plus `".service"` means `ScheduledJob`. A transient
    /// user scope is placed under `…/user@<uid>.service/app.slice/<name>.scope`
    /// whoever asks for it, and the name is the **last path component**. So a
    /// scope named `session-99-agent` puts `/session-` into the cgroup path of
    /// every process in it and promotes the whole agent to the origin reserved
    /// for a human sitting at this machine.
    ///
    /// Nothing reachable today supplies such a name — it is built from a fixed
    /// prefix and two integers — but it is one refactor ("name the scope after
    /// the session") away from being reachable, and nothing in `origin.rs`
    /// would notice. Hence the check here, and hence the test that asserts the
    /// produced path still classifies `ScheduledJob`.
    ///
    /// The daemon pid is in the name because session ids are minted per state
    /// directory: two daemons with different `XDG_STATE_HOME`s — the shipped
    /// one and a test harness, which is the normal case on a developer's
    /// machine — both mint session 1, and `--unit=` collides on the one user
    /// manager they share.
    pub fn new(daemon_pid: u32, session_id: u32) -> Result<ScopeName, String> {
        let name = format!("{SCOPE_PREFIX}{daemon_pid}-{session_id}");
        ScopeName::checked(name)
    }

    /// The validation, separated so a test can aim it at a name `new` cannot
    /// currently produce. A refactor that changes `new` keeps this.
    fn checked(name: String) -> Result<ScopeName, String> {
        // The privilege rule goes FIRST, and the order is load-bearing rather
        // than cosmetic. Behind the prefix rule this arm is unreachable — no
        // string can begin both "rime-agent-" and "session-" — so a name that
        // would forge a login session was refused by the PREFIX rule, with the
        // prefix rule's message, and nothing could tell the two apart. The
        // battery's B5 row (drop this arm entirely) was completely green
        // because of it. First means the arm answers for itself.
        if name.starts_with("session-") {
            return Err(format!(
                "a scope named {name:?} would put \"/session-\" into every \
                 process's cgroup path, which origin::classify reads as a \
                 local login session"
            ));
        }
        if !name.starts_with(SCOPE_PREFIX) {
            return Err(format!(
                "a scope name must begin {SCOPE_PREFIX:?}, and {name:?} does not"
            ));
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(format!(
                "a scope name may only contain ASCII letters, digits, '-' and \
                 '_', and {name:?} does not"
            ));
        }
        Ok(ScopeName(name))
    }

    /// The `--unit=` argument.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The last component of the cgroup path processes in this scope will have.
    pub fn cgroup_leaf(&self) -> String {
        format!("{}.scope", self.0)
    }
}

/// The cgroup path a transient user scope will be created under.
///
/// Derived from the daemon's own `/proc/self/cgroup` rather than hardcoded,
/// but **not** by taking the daemon's parent: `systemd-run --user --scope`
/// puts the scope under `…/user@<uid>.service/app.slice/` regardless of where
/// the caller sits. Measured twice, from both placements a daemon can have:
///
/// ```text
/// caller in …/user@1000.service/app.slice/rime-agentd.service  (the shipped unit)
/// caller in /user.slice/user-1000.slice/session-4.scope        (a login session)
///   both ->  …/user-1000.slice/user@1000.service/app.slice/<name>.scope
/// ```
///
/// So there are **two** ways to name that boundary and the daemon needs both,
/// because it has both placements in practice — the shipped user unit sits
/// under `user@<uid>.service`, and a daemon started from a terminal or an ssh
/// session sits in a `session-<n>.scope` that has no such component at all:
///
/// 1. a `user@<uid>.service` component in the path — the boundary is that
///    component plus `app.slice`. This is the observed path, so it is
///    preferred whenever it is there.
/// 2. otherwise a `user-<uid>.slice` component — the user manager for that uid
///    is its `user@<uid>.service` child, whether or not this process happens
///    to be inside it. That is the login-session case, and reading its absence
///    as "there is no user manager" is [`FOUND (seventh)`] — see the card.
///
/// A path with neither is [`Reading::Unavailable`] and not a guess: a system
/// service has no user manager to ask, and a guess here is a budget that
/// silently is not enforced. A `user-<uid>.slice` whose uid is not a number is
/// also unavailable — it is a component that cannot be made sense of, which is
/// not the same as one that is not there.
///
/// If the constructed path does not exist, [`delegated_controllers`] fails to
/// read its `cgroup.controllers` and the session is refused — the unknown
/// branch, which is the correct polarity for a boundary that was inferred.
pub fn scope_parent(proc_cgroup: &str) -> Reading<String> {
    let Some(path) = proc_cgroup
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .map(str::trim)
    else {
        return Reading::Unavailable(format!(
            "/proc/self/cgroup has no unified (\"0::\") line: {proc_cgroup:?}"
        ));
    };
    let mut kept: Vec<&str> = Vec::new();
    // The deepest `user-<uid>.slice` seen so far, and the uid it names. Only
    // consulted after the whole path has been walked, so a `user@` component
    // further down still wins: the measured path beats the constructed one.
    let mut user_slice: Option<(String, &str)> = None;
    for part in path.split('/') {
        kept.push(part);
        if part.starts_with("user@") && part.ends_with(".service") {
            return Reading::Known(format!("{}/app.slice", kept.join("/")));
        }
        if let Some(uid) = part
            .strip_prefix("user-")
            .and_then(|rest| rest.strip_suffix(".slice"))
        {
            if uid.is_empty() || !uid.chars().all(|c| c.is_ascii_digit()) {
                return Reading::Unavailable(format!(
                    "the cgroup path {path:?} has a user slice component \
                     {part:?} whose uid is not a number, so the user manager \
                     it belongs to cannot be named"
                ));
            }
            user_slice = Some((kept.join("/"), uid));
        }
    }
    if let Some((slice, uid)) = user_slice {
        return Reading::Known(format!("{slice}/user@{uid}.service/app.slice"));
    }
    Reading::Unavailable(format!(
        "no user@<uid>.service and no user-<uid>.slice component in the cgroup \
         path {path:?}, so this process is under no systemd user manager at \
         all and a transient user scope has no knowable parent"
    ))
}

/// The controllers delegated at a boundary, from that boundary's
/// `cgroup.controllers` **file contents**.
///
/// Takes the text rather than a path so the decision is pure and a fixture can
/// present a machine this one is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Controllers(Vec<String>);

impl Controllers {
    pub fn parse(text: &str) -> Controllers {
        Controllers(text.split_whitespace().map(str::to_string).collect())
    }

    pub fn has(&self, controller: &str) -> bool {
        self.0.iter().any(|c| c == controller)
    }

    pub fn as_text(&self) -> String {
        self.0.join(" ")
    }
}

/// A budget turned into an argv prefix, plus everything the user has to be
/// told about what did not make it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The argv prefix, empty when there is nothing to enforce.
    pub prefix: Vec<String>,
    /// One entry per resource that was asked for, was **read**, and is **not
    /// delegated on this machine**. These are facts, not unknowns: they are
    /// reported with their reason and left out of `prefix` entirely. There is
    /// never an entry here whose effect was to write a limit of zero.
    pub unenforceable: Vec<(Resource, String)>,
}

impl Plan {
    /// Put the prefix in front of an argv. An empty prefix returns the argv
    /// unchanged, which is the untouched-default path.
    pub fn apply(&self, argv: Vec<String>) -> Vec<String> {
        if self.prefix.is_empty() {
            return argv;
        }
        let mut out = self.prefix.clone();
        out.extend(argv);
        out
    }

    /// The lines the daemon logs. One per unenforceable resource, each naming
    /// the resource and why.
    pub fn notes(&self) -> Vec<String> {
        self.unenforceable
            .iter()
            .map(|(r, why)| {
                format!(
                    "rime-agentd: budget: {} was configured but is not enforceable here: {why}",
                    r.controller()
                )
            })
            .collect()
    }

    /// Build the plan.
    ///
    /// `controllers` is the delegation boundary's reading, and the two arms of
    /// it are the two polarities this module is about:
    ///
    /// * `Unavailable` — nobody knows whether any of this would be enforced,
    ///   so the whole request is refused. Starting the agent anyway would hand
    ///   back a session that looks budgeted and is not.
    /// * `Known` — every named resource is checked against it. Present means
    ///   it goes into the prefix; absent means it goes into `unenforceable`
    ///   with its reason and **never into the prefix as a zero**.
    pub fn build(
        budget: &Budget,
        controllers: &Reading<Controllers>,
        scope: &ScopeName,
    ) -> Result<Plan, String> {
        if budget.is_empty() {
            return Ok(Plan {
                prefix: Vec::new(),
                unenforceable: Vec::new(),
            });
        }

        let have = match controllers {
            Reading::Known(c) => c,
            Reading::Unavailable(why) => {
                return Err(format!(
                    "a resource budget was configured but this machine's \
                     delegated controllers could not be read, so there is no \
                     way to tell whether it would be enforced: {why}"
                ))
            }
        };

        let mut unenforceable = Vec::new();
        for resource in budget.named() {
            if !have.has(resource.controller()) {
                unenforceable.push((
                    resource,
                    format!(
                        "the {} controller is not delegated to this user's \
                         cgroup, which offers {:?}",
                        resource.controller(),
                        have.as_text()
                    ),
                ));
            }
        }
        let blocked = |r: Resource| unenforceable.iter().any(|(x, _)| *x == r);

        let mut prefix = vec![
            SYSTEMD_RUN.to_string(),
            "--user".to_string(),
            "--scope".to_string(),
            "-q".to_string(),
            format!("--unit={}", scope.as_str()),
            // Without this the scope is torn down when the last process in it
            // exits, which for an agent that forks a helper and returns is the
            // wrong moment; `--collect` also removes a failed scope's record so
            // the unit name is free for the next session with this id.
            "--collect".to_string(),
        ];
        if let Some(pct) = budget.cpu_percent.filter(|_| !blocked(Resource::Cpu)) {
            prefix.push(format!("-pCPUQuota={pct}%"));
        }
        if let Some(bytes) = budget.memory_max.filter(|_| !blocked(Resource::Memory)) {
            prefix.push(format!("-pMemoryMax={bytes}"));
        }
        if let Some(tasks) = budget.tasks_max.filter(|_| !blocked(Resource::Pids)) {
            prefix.push(format!("-pTasksMax={tasks}"));
        }
        // Wall clock is not a cgroup controller, so it is enforceable whatever
        // the boundary delegates and is never in `unenforceable`.
        if let Some(secs) = budget.runtime_secs {
            prefix.push(format!("-pRuntimeMaxSec={secs}"));
        }
        prefix.push("--".to_string());

        Ok(Plan {
            prefix,
            unenforceable,
        })
    }
}

// ---------------------------------------------------------------------------
// The impure edge: everything above is a decision about values, everything
// below reads this machine.
// ---------------------------------------------------------------------------

/// Wrap a session's argv in its budget, if it has one.
///
/// This is the whole hook. It is called once, from `session::start`, with the
/// argv that is about to be spawned.
///
/// `runtime_dir` must be the runtime directory the **child** will see — the
/// one `session.rs` pushes into `spec.env_set` as `XDG_RUNTIME_DIR` — not the
/// daemon's own. See [`transport_at`] for why that distinction has teeth.
pub fn wrap(
    argv: Vec<String>,
    session_id: u32,
    disposable: bool,
    runtime_dir: &Path,
    cfg: &Config,
) -> Result<Vec<String>, String> {
    let budget = Budget::parse(&cfg.extra)?;
    // The fast path, and the one that keeps every existing test unchanged: no
    // budget configured means no wrapper, no discovery and no syscall.
    if budget.is_empty() {
        return Ok(argv);
    }

    // A disposable session's PTY child is the disposable ENGINE and the agent
    // runs inside the capsule it creates, so a scope here would budget the
    // container client and not the agent — a limit that reads as enforced and
    // is not. The same reason `build_argv` is not in that path either. Refused
    // rather than silently skipped, because "your budget did nothing" is not
    // something to leave in a log line.
    if disposable {
        return Err(
            "a resource budget cannot be applied to a disposable session: the \
             process Rime starts is the disposable engine, so the scope would \
             budget the container client rather than the agent inside it. Run \
             without --disposable, or remove the `budget` setting."
                .to_string(),
        );
    }

    if !Path::new(SYSTEMD_RUN).exists() {
        return Err(format!(
            "a resource budget was configured but {SYSTEMD_RUN} is not \
             installed, so there is nothing to create the scope with"
        ));
    }
    let transport = transport_at(runtime_dir);
    if let Some(why) = transport.why() {
        return Err(format!(
            "a resource budget was configured but the session could not reach \
             a systemd user manager: {why}"
        ));
    }

    let scope = ScopeName::new(std::process::id(), session_id)?;
    let plan = Plan::build(&budget, &delegated_controllers(), &scope)?;
    for note in plan.notes() {
        eprintln!("{note}");
    }
    // Named because it is the only way to find the session's cgroup from
    // outside: the scope execs in place, so there is no systemd-run process to
    // look for and nothing in `rime agent status` that would otherwise say
    // which cgroup a session's limits are written on.
    eprintln!(
        "rime-agentd: budget: session {session_id} runs in {}",
        scope.cgroup_leaf()
    );
    Ok(plan.apply(argv))
}

/// Whether `systemd-run --user` can reach a user manager through this runtime
/// directory.
///
/// **Measured, and it is not what the obvious reading says.** `systemd-run
/// --user` connects over the "local transport" at
/// `$XDG_RUNTIME_DIR/systemd/private`, and when `XDG_RUNTIME_DIR` is *set* it
/// does **not** fall back to `$DBUS_SESSION_BUS_ADDRESS` if that path is
/// missing. So a correct bus address plus a wrong runtime directory fails,
/// while a correct bus address with `XDG_RUNTIME_DIR` unset succeeds:
///
/// ```text
/// XDG_RUNTIME_DIR=<tmp>                                    -> rc=1
/// XDG_RUNTIME_DIR=<tmp> DBUS_SESSION_BUS_ADDRESS=<real>    -> rc=1
/// DBUS_SESSION_BUS_ADDRESS=<real>   (no XDG_RUNTIME_DIR)   -> rc=0
/// XDG_RUNTIME_DIR=/run/user/1000                           -> rc=0
/// ```
///
/// `session.rs` always sets the child's `XDG_RUNTIME_DIR` from the spec, so the
/// first two lines are the case a test harness lands in. Checking it here turns
/// "the agent exited 1 the instant it started, with one line of systemd-run
/// stderr on its PTY" into a refusal that names the directory.
///
/// A stat that fails for a reason other than "not there" is `Unavailable` too
/// and says which — permission denied is not absence, which this repository has
/// now found in about fifteen places.
pub fn transport_at(runtime_dir: &Path) -> Reading<PathBuf> {
    let path = runtime_dir.join("systemd/private");
    match std::fs::metadata(&path) {
        Ok(_) => Reading::Known(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Reading::Unavailable(format!(
            "there is no user manager socket at {}",
            path.display()
        )),
        Err(e) => Reading::Unavailable(format!("{} could not be checked: {e}", path.display())),
    }
}

/// Read the delegation boundary's `cgroup.controllers`.
fn delegated_controllers() -> Reading<Controllers> {
    let own = match std::fs::read_to_string("/proc/self/cgroup") {
        Ok(t) => t,
        Err(e) => return Reading::Unavailable(format!("/proc/self/cgroup could not be read: {e}")),
    };
    let boundary = match scope_parent(&own) {
        Reading::Known(p) => p,
        Reading::Unavailable(why) => return Reading::Unavailable(why),
    };
    let file = Path::new("/sys/fs/cgroup")
        .join(boundary.trim_start_matches('/'))
        .join("cgroup.controllers");
    match std::fs::read_to_string(&file) {
        Ok(t) => Reading::Known(Controllers::parse(&t)),
        Err(e) => Reading::Unavailable(format!("{} could not be read: {e}", file.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(json: &str) -> Config {
        rime_agent_core::config::from_str(json).expect("config parses")
    }

    fn controllers(text: &str) -> Reading<Controllers> {
        Reading::Known(Controllers::parse(text))
    }

    fn scope() -> ScopeName {
        ScopeName::new(4242, 7).expect("a scope name built from two integers is valid")
    }

    /// The machine this was measured on: cpu, memory and pids delegated, io not.
    const L16: &str = "cpu memory pids";

    // -- parsing ------------------------------------------------------------

    #[test]
    fn no_budget_setting_is_an_empty_budget_and_not_an_error() {
        let cfg = cfg_with("{}");
        let b = Budget::parse(&cfg.extra).expect("no stanza is not an error");
        assert!(b.is_empty());
    }

    #[test]
    fn an_empty_budget_leaves_the_argv_byte_identical() {
        // The property the four existing integration tests depend on: a
        // session nobody budgeted runs exactly the argv it ran before this
        // module existed.
        let argv = vec!["/usr/bin/claude".to_string(), "--continue".to_string()];
        let cfg = cfg_with("{}");
        let got = wrap(argv.clone(), 1, false, Path::new("/nonexistent"), &cfg)
            .expect("an unbudgeted session is never refused");
        assert_eq!(got, argv);
    }

    #[test]
    fn an_empty_budget_asks_the_machine_nothing() {
        // The fast path must not depend on this machine at all: the runtime
        // directory below does not exist, and `wrap` must still succeed. If
        // discovery ever moves above the `is_empty` check this fails.
        let cfg = cfg_with("{}");
        assert!(wrap(vec!["x".into()], 1, false, Path::new("/no/such/dir"), &cfg).is_ok());
    }

    #[test]
    fn a_size_may_be_written_with_a_suffix_or_as_bytes() {
        assert_eq!(parse_size("512M"), Some(512 * 1024 * 1024));
        assert_eq!(parse_size("2G"), Some(2 * 1024 * 1024 * 1024));
        assert_eq!(parse_size("1024"), Some(1024));
        assert_eq!(parse_size("4k"), Some(4096));
        assert_eq!(parse_size("half"), None);
        assert_eq!(parse_size(""), None);

        let cfg = cfg_with(r#"{"budget": {"memory_max": "512M"}}"#);
        let b = Budget::parse(&cfg.extra).expect("parses");
        assert_eq!(b.memory_max, Some(512 * 1024 * 1024));

        let cfg = cfg_with(r#"{"budget": {"memory_max": 536870912}}"#);
        let b = Budget::parse(&cfg.extra).expect("parses");
        assert_eq!(b.memory_max, Some(512 * 1024 * 1024));
    }

    #[test]
    fn zero_is_refused_for_every_field_because_zero_is_not_a_limit() {
        // MemoryMax=0 kills the agent at its first allocation and TasksMax=0
        // stops it forking. The whole module's rule is "never a budget of
        // zero"; this is where that becomes impossible rather than merely
        // avoided.
        for key in [
            "memory_max",
            "cpu_percent",
            "tasks_max",
            "runtime_secs",
            "io_read_bps",
            "io_write_bps",
        ] {
            let cfg = cfg_with(&format!(r#"{{"budget": {{"{key}": 0}}}}"#));
            let why = Budget::parse(&cfg.extra).expect_err("0 must be refused for {key}");
            assert!(why.contains(key), "{key}: {why}");
            assert!(why.contains('0'), "{key}: {why}");
        }
        // And the string spelling of it, which goes down a different arm.
        let cfg = cfg_with(r#"{"budget": {"memory_max": "0M"}}"#);
        assert!(Budget::parse(&cfg.extra).is_err());
    }

    #[test]
    fn a_mistyped_key_is_an_error_and_not_an_unbudgeted_session() {
        // `Config::load` degrades a corrupt file to the defaults on purpose.
        // That rule is wrong here: degrading `memmory_max` to "no budget"
        // gives the user an unconfined agent and a config file that reads as
        // if it says otherwise.
        let cfg = cfg_with(r#"{"budget": {"memmory_max": "512M"}}"#);
        let why = Budget::parse(&cfg.extra).expect_err("a typo must be loud");
        assert!(why.contains("memmory_max"), "{why}");
        assert!(why.contains("memory_max"), "the error must name the real key: {why}");
    }

    #[test]
    fn a_budget_that_is_not_an_object_says_what_it_was() {
        let cfg = cfg_with(r#"{"budget": "512M"}"#);
        let why = Budget::parse(&cfg.extra).expect_err("a string is not a budget");
        assert!(why.contains("a string"), "{why}");
    }

    #[test]
    fn a_negative_or_fractional_value_is_refused() {
        for bad in ["-1", "1.5"] {
            let cfg = cfg_with(&format!(r#"{{"budget": {{"cpu_percent": {bad}}}}}"#));
            assert!(
                Budget::parse(&cfg.extra).is_err(),
                "cpu_percent {bad} must be refused"
            );
        }
    }

    // -- discovery ----------------------------------------------------------

    #[test]
    fn the_boundary_is_the_user_managers_app_slice_whoever_asks() {
        // Measured: a scope asked for from a LOGIN SESSION still lands under
        // user@1000.service/app.slice, so the boundary is not the caller's
        // parent. Both of these cgroups must produce the same answer.
        //
        // They did not, and that was FOUND (seventh): this assertion used to
        // say the login-session path was `Unavailable`, three lines under a
        // comment stating the measurement that says otherwise. The daemon was
        // reading the absence of `user@` from ITS OWN path as the absence of a
        // user manager, and refusing every budgeted session on a machine where
        // `systemd-run --user --scope` works — which is every daemon started
        // from a terminal, from ssh, and from `tests/in-login-session.sh`,
        // the harness this repository's own CI runs `cargo test` under.
        let from_service = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/rime-agentd.service\n";
        let from_login = "0::/user.slice/user-1000.slice/session-4.scope\n";
        let want =
            Reading::Known("/user.slice/user-1000.slice/user@1000.service/app.slice".to_string());
        assert_eq!(scope_parent(from_service), want);
        assert_eq!(scope_parent(from_login), want, "whoever asks");
    }

    #[test]
    fn a_root_daemon_in_a_login_session_finds_root_s_user_manager() {
        // The uid comes from the slice component rather than from a constant,
        // so the root daemon — which this machine also runs — gets user@0 and
        // not user@1000.
        assert_eq!(
            scope_parent("0::/user.slice/user-0.slice/session-9.scope\n"),
            Reading::Known("/user.slice/user-0.slice/user@0.service/app.slice".to_string())
        );
    }

    #[test]
    fn a_deeper_user_manager_component_still_wins_over_the_constructed_one() {
        // The measured path beats the inferred one: the walk must not stop at
        // `user-1000.slice` and construct a boundary when the real component
        // is one level further down.
        assert_eq!(
            scope_parent("0::/user.slice/user-1000.slice/user@1000.service/app.slice/x.service\n"),
            Reading::Known("/user.slice/user-1000.slice/user@1000.service/app.slice".to_string())
        );
    }

    #[test]
    fn a_user_slice_whose_uid_is_not_a_number_is_unavailable_and_not_ignored() {
        // A component that cannot be made sense of is not a component that is
        // not there. Guessing a manager name off it would build a path that
        // either does not exist or belongs to somebody else.
        let got = scope_parent("0::/user.slice/user-foo.slice/session-4.scope\n");
        assert!(matches!(got, Reading::Unavailable(_)), "{got:?}");
        assert!(
            got.why().unwrap().contains("user-foo.slice"),
            "the refusal must name the component it could not read: {got:?}"
        );
    }

    #[test]
    fn a_cgroup_line_with_no_user_manager_is_unavailable_and_says_so() {
        for cg in [
            "0::/system.slice/sshd.service\n",
            "0::/\n",
            "1:name=systemd:/user.slice\n",
            "",
        ] {
            let got = scope_parent(cg);
            assert!(matches!(got, Reading::Unavailable(_)), "{cg:?} -> {got:?}");
        }
    }

    #[test]
    fn the_root_uid_user_manager_is_found_too() {
        // There are two rime-agentd processes on this machine, uid 0 and uid
        // 1000, and the root one is under user@0.service.
        assert_eq!(
            scope_parent("0::/user.slice/user-0.slice/user@0.service/app.slice/rime-agentd.service"),
            Reading::Known("/user.slice/user-0.slice/user@0.service/app.slice".to_string())
        );
    }

    // -- the polarity -------------------------------------------------------

    #[test]
    fn io_is_reported_with_its_reason_and_never_as_a_budget_of_zero() {
        // The rule the whole unit keeps re-learning. `io` is in the ROOT
        // cgroup's controllers and never in its subtree_control, so no io.max
        // file exists anywhere under user.slice. A budget naming it must come
        // back as a reading carrying that fact — not as `IOReadBandwidthMax=0`,
        // which would be a total I/O ban dressed up as a conservative default.
        let cfg = cfg_with(r#"{"budget": {"memory_max": "512M", "io_read_bps": "10M"}}"#);
        let budget = Budget::parse(&cfg.extra).expect("parses");
        let plan = Plan::build(&budget, &controllers(L16), &scope()).expect("builds");

        assert_eq!(plan.unenforceable.len(), 1);
        assert_eq!(plan.unenforceable[0].0, Resource::Io);
        assert!(plan.unenforceable[0].1.contains("io"), "{:?}", plan.unenforceable);
        assert!(
            plan.unenforceable[0].1.contains("cpu memory pids"),
            "the reason must name what the machine DOES offer: {:?}",
            plan.unenforceable
        );

        let joined = plan.prefix.join(" ");
        assert!(
            !joined.contains("IO"),
            "no io property may reach the argv at all: {joined}"
        );
        for arg in &plan.prefix {
            assert!(
                !arg.ends_with("=0") && !arg.ends_with("=0%"),
                "a budget of zero reached the argv: {arg}"
            );
        }
        // And the thing that IS enforceable still is.
        assert!(joined.contains("-pMemoryMax=536870912"), "{joined}");

        // The user is told, in one line per dropped resource.
        let notes = plan.notes();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("io"), "{:?}", notes);
        assert!(notes[0].contains("not enforceable"), "{:?}", notes);
    }

    #[test]
    fn an_unreadable_controllers_file_refuses_rather_than_starting_unbudgeted() {
        // The other polarity, and the one that decides whether this feature is
        // worth having. Absent is a fact you report; unreadable is an unknown,
        // and a session started unbudgeted after an unknown is a session the
        // user believes is budgeted.
        let cfg = cfg_with(r#"{"budget": {"memory_max": "512M"}}"#);
        let budget = Budget::parse(&cfg.extra).expect("parses");
        let unreadable = Reading::Unavailable("/sys/fs/cgroup/…: Permission denied".to_string());
        let why = Plan::build(&budget, &unreadable, &scope())
            .expect_err("an unknown must refuse, not degrade");
        assert!(why.contains("Permission denied"), "{why}");
        assert!(
            why.contains("whether it would be enforced"),
            "the refusal must say what is unknown: {why}"
        );
    }

    #[test]
    fn an_unreadable_controllers_file_with_no_budget_is_not_an_error() {
        // The refusal above must be caused by the BUDGET, not by the reading:
        // a machine whose cgroup files cannot be read still runs unbudgeted
        // agents perfectly well, and this feature must not break it.
        let plan = Plan::build(
            &Budget::default(),
            &Reading::Unavailable("nope".to_string()),
            &scope(),
        )
        .expect("no budget, no opinion");
        assert!(plan.prefix.is_empty());
        assert!(plan.unenforceable.is_empty());
    }

    #[test]
    fn a_machine_that_delegates_nothing_drops_every_resource_and_keeps_the_reasons() {
        let cfg = cfg_with(
            r#"{"budget": {"memory_max": "512M", "cpu_percent": 50, "tasks_max": 64}}"#,
        );
        let budget = Budget::parse(&cfg.extra).expect("parses");
        let plan = Plan::build(&budget, &controllers(""), &scope()).expect("builds");
        assert_eq!(plan.unenforceable.len(), 3);
        let joined = plan.prefix.join(" ");
        assert!(!joined.contains("-p"), "nothing may be enforced: {joined}");
        // But the scope is still created, so the session is still contained in
        // a cgroup of its own and `rime agent kill` still has one thing to aim
        // at. A scope with no properties is honest; a zero would not be.
        assert!(joined.contains("--scope"), "{joined}");
    }

    // -- the prefix ---------------------------------------------------------

    #[test]
    fn a_full_budget_becomes_the_measured_command_line() {
        let cfg = cfg_with(
            r#"{"budget": {"memory_max": "512M", "cpu_percent": 50,
                            "tasks_max": 64, "runtime_secs": 5}}"#,
        );
        let budget = Budget::parse(&cfg.extra).expect("parses");
        let plan = Plan::build(&budget, &controllers(L16), &scope()).expect("builds");
        assert_eq!(
            plan.prefix,
            vec![
                rime_paths::SYSTEMD_RUN,
                "--user",
                "--scope",
                "-q",
                "--unit=rime-agent-4242-7",
                "--collect",
                "-pCPUQuota=50%",
                "-pMemoryMax=536870912",
                "-pTasksMax=64",
                "-pRuntimeMaxSec=5",
                "--",
            ]
        );
        assert!(plan.unenforceable.is_empty());

        let argv = vec!["/usr/bin/claude".to_string(), "--continue".to_string()];
        let wrapped = plan.apply(argv.clone());
        assert_eq!(&wrapped[wrapped.len() - 2..], &argv[..]);
        assert_eq!(wrapped[0], SYSTEMD_RUN);
    }

    #[test]
    fn the_scope_is_a_scope_and_not_a_service() {
        // `--service` would fork the program off the user manager instead of
        // exec'ing in place, so the pid the daemon records would be
        // systemd-run's and every pid-based operation — kill, pause, the
        // parent-chain session lookup — would aim at the wrong process.
        let cfg = cfg_with(r#"{"budget": {"memory_max": "512M"}}"#);
        let budget = Budget::parse(&cfg.extra).expect("parses");
        let plan = Plan::build(&budget, &controllers(L16), &scope()).expect("builds");
        assert!(plan.prefix.iter().any(|a| a == "--scope"), "{:?}", plan.prefix);
        assert!(
            !plan.prefix.iter().any(|a| a == "--service" || a == "-d"),
            "{:?}",
            plan.prefix
        );
        // And the argv must be separated from the properties, or a program
        // named like an option is parsed as one.
        assert_eq!(plan.prefix.last().map(String::as_str), Some("--"));
    }

    #[test]
    fn wall_clock_is_never_defaulted() {
        // A coding agent killed at an arbitrary wall-clock deadline is a
        // regression. RuntimeMaxSec exists only when a human wrote it down.
        let cfg = cfg_with(r#"{"budget": {"memory_max": "512M"}}"#);
        let budget = Budget::parse(&cfg.extra).expect("parses");
        assert_eq!(budget.runtime_secs, None);
        let plan = Plan::build(&budget, &controllers(L16), &scope()).expect("builds");
        assert!(
            !plan.prefix.iter().any(|a| a.contains("RuntimeMaxSec")),
            "{:?}",
            plan.prefix
        );
    }

    #[test]
    fn wall_clock_survives_a_machine_that_delegates_nothing() {
        // RuntimeMaxSec is a timer, not a cgroup controller, so it is
        // enforceable whatever the boundary delegates — and must not be swept
        // up by the unenforceable pass.
        let cfg = cfg_with(r#"{"budget": {"runtime_secs": 5}}"#);
        let budget = Budget::parse(&cfg.extra).expect("parses");
        let plan = Plan::build(&budget, &controllers(""), &scope()).expect("builds");
        assert!(plan.unenforceable.is_empty(), "{:?}", plan.unenforceable);
        assert!(plan.prefix.iter().any(|a| a == "-pRuntimeMaxSec=5"));
    }

    // -- the scope name is a privilege boundary -----------------------------

    #[test]
    fn the_produced_cgroup_path_still_classifies_as_a_scheduled_job() {
        // The check FOUND (sixth) exists for. A transient user scope lands
        // under user@<uid>.service/app.slice/<name>.scope, and origin::classify
        // decides §7 origin by substring over exactly that string. This asserts
        // the real thing: the path a budgeted agent will actually have.
        use rime_agent_core::origin::classify;
        use rime_agent_core::policy::RequestOrigin;

        let name = ScopeName::new(std::process::id(), 3).expect("valid");
        let path = format!(
            "/user.slice/user-1000.slice/user@1000.service/app.slice/{}",
            name.cgroup_leaf()
        );
        for tty in [true, false] {
            assert_eq!(
                classify(&path, tty),
                Some(RequestOrigin::ScheduledJob),
                "a budgeted agent must not gain a local origin: {path}"
            );
        }
    }

    #[test]
    fn a_scope_name_that_would_forge_a_login_session_is_refused() {
        // The one-refactor-away case: name the scope after the session and
        // `/session-` appears in the cgroup path of every process in it, which
        // origin::classify used to read as a human at this machine — the
        // origin §7 reserves root and break-glass for.
        //
        // **`classify` no longer promotes it (P2-016, 2026-09-12).** The
        // user-manager arm now runs first, so every transient scope a user
        // manager can hold is a `scheduled-job` whatever it calls itself, and
        // this name is no longer the difference between local and remote.
        //
        // The name check stays, and the assertion above is now the reason
        // rather than the consequence: two fences, and this is the one that
        // does not depend on a substring rule in another crate staying the way
        // it is today. `classify` was reordered once; the name it is fed
        // should not be the thing that made it safe. If the classifier is ever
        // relaxed again, this refusal is what is still standing.
        use rime_agent_core::origin::classify;
        use rime_agent_core::policy::RequestOrigin;

        let forged = "/user.slice/user-1000.slice/user@1000.service/app.slice/session-99-agent.scope";
        assert_eq!(
            classify(forged, true),
            Some(RequestOrigin::ScheduledJob),
            "the classifier's user-manager arm must run before its session arm"
        );
        assert!(ScopeName::checked("session-99-agent".to_string()).is_err());
        // ...and it is refused for the right reason: it is caught before the
        // charset rule, which would also have let it through.
        let why = ScopeName::checked("session-99-agent".to_string()).unwrap_err();
        assert!(
            why.contains("origin::classify") && why.contains("login session"),
            "refused, but by the wrong rule — this assertion used to accept \
             `why.contains(\"begin\")`, which is the PREFIX rule's word, so \
             deleting the session rule left it green: {why}"
        );
    }

    #[test]
    fn the_scope_prefix_itself_cannot_forge_a_login_session() {
        // What the `session-` arm really guards is a change to SCOPE_PREFIX:
        // the arm is unreachable while the prefix is "rime-agent-", so the
        // constant is the thing a test has to pin. A prefix beginning
        // "session-" would promote every budgeted agent to the origin §7
        // reserves for a human at this machine, and every name `new` produces
        // would pass the charset rule on the way there.
        use rime_agent_core::origin::classify;
        use rime_agent_core::policy::RequestOrigin;

        assert!(
            !SCOPE_PREFIX.starts_with("session-"),
            "SCOPE_PREFIX is {SCOPE_PREFIX:?}"
        );
        for (pid, id) in [(1u32, 1u32), (4242, 7), (u32::MAX, u32::MAX)] {
            let leaf = ScopeName::new(pid, id).expect("valid").cgroup_leaf();
            let path =
                format!("/user.slice/user-1000.slice/user@1000.service/app.slice/{leaf}");
            for tty in [true, false] {
                assert_eq!(
                    classify(&path, tty),
                    Some(RequestOrigin::ScheduledJob),
                    "{path}"
                );
            }
        }
    }

    #[test]
    fn a_scope_name_may_not_carry_a_path_separator_or_anything_exotic() {
        for bad in [
            "rime-agent-1/../session-1",
            "rime-agent-1 --property=User=root",
            "rime-agent-\n1",
            "notrime-1",
        ] {
            assert!(
                ScopeName::checked(bad.to_string()).is_err(),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn the_scope_name_carries_the_daemon_pid_so_two_daemons_cannot_collide() {
        // Session ids are minted per state directory, so a test harness and
        // the shipped daemon both mint session 1 — and `--unit=` is global to
        // the user manager they share. Without the pid the second session dies
        // at exec with "Unit already exists".
        let a = ScopeName::new(100, 1).expect("valid");
        let b = ScopeName::new(200, 1).expect("valid");
        assert_ne!(a, b);
        assert!(a.as_str().contains("100"));
        assert_eq!(a.cgroup_leaf(), "rime-agent-100-1.scope");
    }

    // -- the impure edge ----------------------------------------------------

    #[test]
    fn a_budgeted_disposable_session_is_refused_and_not_silently_unwrapped() {
        let cfg = cfg_with(r#"{"budget": {"memory_max": "512M"}}"#);
        let why = wrap(vec!["x".into()], 1, true, Path::new("/run/user/1000"), &cfg)
            .expect_err("a disposable session cannot be budgeted");
        assert!(why.contains("disposable"), "{why}");
        assert!(
            why.contains("engine") || why.contains("container client"),
            "the refusal must say what would actually have been budgeted: {why}"
        );
    }

    #[test]
    fn an_unbudgeted_disposable_session_is_untouched() {
        // The refusal above must be caused by the budget and not by the word
        // "disposable": disposable sessions have to keep working for everyone
        // who never configured a budget, which is everyone by default.
        let cfg = cfg_with("{}");
        let argv = vec!["engine".to_string()];
        assert_eq!(
            wrap(argv.clone(), 1, true, Path::new("/nope"), &cfg).expect("untouched"),
            argv
        );
    }

    #[test]
    fn a_runtime_directory_with_no_user_manager_refuses_and_names_the_path() {
        // Measured: `systemd-run --user` uses $XDG_RUNTIME_DIR/systemd/private
        // and does NOT fall back to $DBUS_SESSION_BUS_ADDRESS when
        // XDG_RUNTIME_DIR is set and that path is missing. Without this check
        // the agent execs into systemd-run, fails with one line of stderr on
        // its PTY and exits 1 — a failure nobody can trace back to here.
        let dir = std::env::temp_dir().join(format!("rime-budget-rt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let got = transport_at(&dir);
        assert!(matches!(got, Reading::Unavailable(_)), "{got:?}");
        assert!(got.why().expect("a reason").contains("systemd/private"));

        let cfg = cfg_with(r#"{"budget": {"memory_max": "512M"}}"#);
        let why = wrap(vec!["x".into()], 1, false, &dir, &cfg)
            .expect_err("no user manager, no budget, no session");
        assert!(why.contains(&dir.display().to_string()), "{why}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_transport_that_cannot_be_checked_is_unavailable_and_not_absent() {
        // Permission denied is not absence — the fifteenth time this repo has
        // had to say so. A non-traversable parent makes the stat fail with
        // EACCES, and folding that into "no user manager" would be the same
        // bug in a new file.
        let base = std::env::temp_dir().join(format!("rime-budget-eacces-{}", std::process::id()));
        let inner = base.join("run");
        std::fs::create_dir_all(inner.join("systemd")).expect("dirs");
        std::fs::write(inner.join("systemd/private"), b"").expect("marker");
        assert!(matches!(transport_at(&inner), Reading::Known(_)));

        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        let got = transport_at(&inner);
        // Root ignores the mode bits, so this only proves anything when the
        // test is not run as root. Saying so beats a false pass.
        if unsafe { libc::geteuid() } != 0 {
            assert!(matches!(got, Reading::Unavailable(_)), "{got:?}");
            let why = got.why().expect("a reason");
            assert!(
                !why.contains("there is no user manager socket"),
                "EACCES was reported as absence: {why}"
            );
            assert!(why.contains("could not be checked"), "{why}");
        }
        let _ = std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn this_machines_boundary_reads_as_a_real_controller_list() {
        // Not an assertion about what this machine delegates — that is the
        // machine's business and a test asserting `cpu memory pids` would fail
        // on a different one. The claim is narrower and is the one that can
        // break: whatever `delegated_controllers` returns here, it is a
        // reading, and if it is Known it is not empty and not a path.
        match delegated_controllers() {
            Reading::Known(c) => {
                let text = c.as_text();
                assert!(!text.contains('/'), "that is a path, not a controller list: {text}");
                // io is the one this unit measured as absent everywhere. If it
                // is ever present here, the note in the module docs is stale
                // rather than the code being wrong, so this only records it.
                eprintln!("delegated controllers here: {text:?} (io: {})", c.has("io"));
            }
            Reading::Unavailable(why) => {
                // Legitimate in a container with no user manager. Must not be
                // a silent pass, so it says so.
                eprintln!("no delegation boundary on this machine: {why}");
            }
        }
    }
}
