//! The desktop half of push: watching the daemon, and posting envelopes.
//!
//! ## Why this watcher exists on the desktop at all
//!
//! There is one already, on the phone: `AlertWatcher` in `android/core` folds
//! successive polls into alerts. It cannot be the only one, and the reason is
//! the whole point of P1-058 — it runs inside the app, and the app is not
//! running. A phone in a pocket with the screen off has no poll loop, and an
//! agent that needs somebody at two in the morning reaches nobody.
//!
//! So the rules are implemented a second time, here, where the process is
//! always alive. That is a duplication and it is a real cost; it is paid
//! deliberately, and the two are kept honest by the same device `wire.rs` uses
//! against the hand-rolled Kotlin Noise — committed vectors that both suites
//! read. The rules themselves are stated in one place, in
//! `rime_remote_core::push`, and this module is the observation.
//!
//! ## The rules, which are `AlertWatcher`'s rules
//!
//! * **The first observation raises nothing.** A daemon restarting to six
//!   sessions that have been waiting since yesterday must not push six
//!   notifications for things that did not just happen.
//! * **Only the transition edge.** `Session::set_state` has no same-state
//!   early return and four independent paths publish `waiting_for_user` for
//!   one turn — the notification hook, the stop hook, the PTY BEL/OSC scanner
//!   and the ten-second idle rule — so keying on "is waiting" would push four
//!   times. Keying on "became waiting" pushes once.
//! * **A pruned session is forgotten.** Ids are reused after a prune, and a
//!   new session landing on a recycled id must not be diffed against a
//!   stranger's state.
//!
//! ## Why `worktrees` is on a slower timer than `list`
//!
//! Answering `worktrees` runs git in every remembered project and does a
//! `merge-tree --write-tree` per worktree. The Android client deliberately
//! keeps it off its four-second loop for that reason, and the consequence
//! recorded against P1-058 was that a TEST_FAILED alert could only fire across
//! two manual refreshes of the Projects screen.
//!
//! Here it is on its own timer, so that window becomes [`WORKTREE_INTERVAL`]
//! rather than "whenever somebody opens a screen". That is a real improvement
//! and it is still not instant; it is stated as a number rather than as
//! "supported".
//!
//! ## What this module will not do
//!
//! Read a session's `detail`, `cwd`, `project`, `worktree`, `args` or
//! telemetry. It has the whole `SessionInfo` in its hand — it must, to see the
//! state — and it puts none of it in an envelope, because
//! `rime_remote_core::push::Body` has nowhere to put it. That is checked by a
//! test that drives a session carrying a password through this watcher and
//! asserts the password is in none of the bytes that leave the machine.

use std::collections::{BTreeMap, BTreeSet};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use rime_agent_core::protocol::{Response, SessionInfo};
use rime_agent_core::request::{Decision, PrivilegeRequest};
use rime_agent_core::worktree::{TestState, WorktreeStatus};
use rime_remote_core::push::{
    is_deployment_verb, Adapter, Body, Delivery, Endpoint, Envelope, Kind, MAX_FAILURES, NO_SESSION,
};
use rime_remote_core::tls::Trust;

use crate::state::State;

/// How often the session list is polled.
///
/// Four seconds, which is what the Android client's own loop uses when it is
/// in the foreground. A notification that arrives four seconds late is not
/// late; one that arrives four minutes late is a different feature.
pub const LIST_INTERVAL: Duration = Duration::from_secs(4);

/// How often worktree test state is polled. See the module note.
pub const WORKTREE_INTERVAL: Duration = Duration::from_secs(60);

/// How long one delivery may take, end to end.
pub const DELIVERY_TIMEOUT: Duration = Duration::from_secs(20);

/// One thing worth waking a phone for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Raised {
    pub kind: Kind,
    /// The session, or [`NO_SESSION`].
    pub session: i32,
    /// That session's start time in unix seconds, or 0 when there is none.
    pub started: u64,
    pub adapter: Adapter,
}

impl Raised {
    /// The body this raises, once a sequence number is drawn for a device.
    pub fn body(&self, seq: u64) -> Body {
        Body {
            kind: self.kind,
            adapter: self.adapter,
            session: self.session,
            started: self.started,
            seq,
        }
    }
}

/// What the machine looked like on one poll.
#[derive(Debug, Default)]
pub struct Observation {
    pub sessions: Vec<SessionInfo>,
    /// `None` when worktrees were not polled this round, which is not the same
    /// as "there are none" — an empty vector would retire every remembered
    /// test state and re-raise it on the next real poll.
    pub worktrees: Option<Vec<WorktreeStatus>>,
    pub requests: Vec<PrivilegeRequest>,
    /// The approvals nexd is waiting on, by id. `None` when nexd was not
    /// asked or did not answer, which is not the same as "nothing is
    /// waiting", for the reason given on [`Observation::worktrees`].
    pub nex: Option<BTreeSet<String>>,
}

/// Turns successive observations into things worth pushing.
///
/// Not thread-safe and does not need to be: one loop drives it.
#[derive(Debug, Default)]
pub struct Watcher {
    /// Whether anything has been observed yet.
    seen: bool,
    /// Whether worktrees have been observed yet. Separate from [`Watcher::seen`]
    /// because they are on their own timer: the first worktree poll happens
    /// after several session polls and must still raise nothing.
    seen_worktrees: bool,
    /// Session id -> the state it was last in.
    last_state: BTreeMap<u32, String>,
    /// Worktree path -> the test state word it was last in.
    last_tests: BTreeMap<String, String>,
    /// Privilege requests already announced as needing a decision.
    announced: BTreeSet<u32>,
    /// Deployment requests already announced as having finished.
    executed: BTreeSet<u32>,
    /// Whether nexd has answered yet. Separate from [`Watcher::seen`]: nexd
    /// may start long after this service, and the approvals waiting when it
    /// is first heard from did not just happen.
    seen_nex: bool,
    /// nex approvals known to be waiting.
    nex_waiting: BTreeSet<String>,
}

impl Watcher {
    pub fn new() -> Watcher {
        Watcher::default()
    }

    /// Fold one observation into the alerts it raises.
    pub fn observe(&mut self, o: &Observation) -> Vec<Raised> {
        let first = !self.seen;
        self.seen = true;
        let mut out = Vec::new();

        for s in &o.sessions {
            let state = state_word(s);
            let was = self.last_state.insert(s.id, state.clone());
            if first || was.as_deref() == Some(state.as_str()) {
                continue;
            }
            let Some(kind) = kind_of(&state) else { continue };
            out.push(Raised {
                kind,
                session: session_id(s.id),
                started: s.started,
                adapter: Adapter::of(&s.agent),
            });
        }
        // A session the daemon has forgotten must not keep its old state here:
        // `Remove` and `Prune` make the id reusable, and a new session landing
        // on a recycled id would be compared against a stranger's state.
        let live: BTreeSet<u32> = o.sessions.iter().map(|s| s.id).collect();
        self.last_state.retain(|id, _| live.contains(id));

        if let Some(worktrees) = &o.worktrees {
            let first_worktrees = !self.seen_worktrees;
            self.seen_worktrees = true;
            for w in worktrees {
                let word = test_word(&w.tests);
                let was = self.last_tests.insert(w.path.clone(), word.to_string());
                if first_worktrees || was.as_deref() == Some(word) {
                    continue;
                }
                if !matches!(w.tests, TestState::Failed { .. }) {
                    continue;
                }
                let session = w.sessions.first().copied();
                let info = session.and_then(|id| o.sessions.iter().find(|s| s.id == id));
                out.push(Raised {
                    kind: Kind::TestFailed,
                    session: session.map(session_id).unwrap_or(NO_SESSION),
                    started: info.map(|s| s.started).unwrap_or(0),
                    adapter: info.map(|s| Adapter::of(&s.agent)).unwrap_or(Adapter::Unknown),
                });
            }
            let paths: BTreeSet<&str> = worktrees.iter().map(|w| w.path.as_str()).collect();
            self.last_tests.retain(|p, _| paths.contains(p.as_str()));
        }

        for r in &o.requests {
            let info = r.session.and_then(|id| o.sessions.iter().find(|s| s.id == id));
            let started = info.map(|s| s.started).unwrap_or(0);
            let adapter = r
                .agent
                .as_deref()
                .map(Adapter::of)
                .unwrap_or(Adapter::Unknown);
            let session = r.session.map(session_id).unwrap_or(NO_SESSION);

            if matches!(r.decision, Decision::Pending) {
                if self.announced.insert(r.id) && !first {
                    out.push(Raised { kind: Kind::Approval, session, started, adapter });
                }
            } else {
                // Decided elsewhere. Forgotten so that a request re-opened by a
                // daemon restart announces itself again.
                self.announced.remove(&r.id);
            }

            // A deployment that has finished. `executed_ms` is set by the
            // daemon when the operation actually ran, and `exit_code` says how
            // it went — so this is the completion of a deployment observed on
            // the wire that already exists, and not an event invented for the
            // criterion.
            if r.executed_ms.is_some() && is_deployment_verb(verb_word(r)) && self.executed.insert(r.id)
            {
                if first {
                    continue;
                }
                let kind = if r.exit_code == Some(0) {
                    Kind::Deployed
                } else {
                    Kind::DeployFailed
                };
                out.push(Raised { kind, session, started, adapter });
            }
        }
        let known: BTreeSet<u32> = o.requests.iter().map(|r| r.id).collect();
        self.announced.retain(|id| known.contains(id));
        self.executed.retain(|id| known.contains(id));

        if let Some(waiting) = &o.nex {
            let first_nex = !self.seen_nex;
            self.seen_nex = true;
            let new = waiting.iter().any(|id| !self.nex_waiting.contains(id));
            // Kept as exactly what nexd says is waiting: an approval answered
            // or cancelled leaves, so if nexd ever reuses an id, it is news.
            self.nex_waiting = waiting.clone();
            // One alert however many arrived since the last poll. The phone
            // shows one notification per machine for nex anyway, and its nex
            // screen lists every one of them; two pushes would only buzz twice.
            if new && !first_nex {
                out.push(Raised {
                    kind: Kind::NexApproval,
                    session: NO_SESSION,
                    started: 0,
                    adapter: Adapter::Unknown,
                });
            }
        }

        out
    }
}

/// A session id as an envelope carries it.
///
/// `SessionInfo.id` is a `u32` and [`Body::session`] is an `i32`, because the
/// body also has to be able to say "no session". The cast is saturating rather
/// than wrapping: a daemon that had really issued two billion session ids
/// would otherwise start producing negative ones, and one of them would
/// eventually be [`NO_SESSION`] — a notification that opened the Agent Center
/// instead of the session it names.
fn session_id(id: u32) -> i32 {
    i32::try_from(id).unwrap_or(i32::MAX)
}

/// The state word, as the wire spells it.
fn state_word(s: &SessionInfo) -> String {
    serde_json::to_value(s.state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The verb word, as the wire spells it.
///
/// Read through serde rather than matched on the enum, so that a verb added to
/// `rime-agent-core` needs no line here and, more importantly, so that
/// `is_deployment_verb` is checked against the SAME spelling the Android
/// client sees. The two would otherwise be free to disagree about
/// `pkg_rebuild` versus `PkgRebuild`.
fn verb_word(r: &PrivilegeRequest) -> &'static str {
    // `Verb` is `#[serde(tag = "verb")]` flattened into the request, so the
    // word is the request's own `verb` key.
    match serde_json::to_value(r) {
        Ok(v) => match v.get("verb").and_then(|v| v.as_str()) {
            Some("update") => "update",
            Some("rollback") => "rollback",
            Some("pin") => "pin",
            Some("pkg_rebuild") => "pkg_rebuild",
            Some("pkg_rollback") => "pkg_rollback",
            _ => "",
        },
        Err(_) => "",
    }
}

/// The test-state word, for edge detection.
fn test_word(t: &TestState) -> &'static str {
    match t {
        TestState::Unobserved => "unobserved",
        TestState::Running { .. } => "running",
        TestState::Passed { .. } => "passed",
        TestState::Failed { .. } => "failed",
    }
}

/// Which kind a session state is news of, or `None` when it is not news.
///
/// `starting` and `working` are not news. `complete` and `exited` both mean
/// the session ended, and are one kind rather than two, because the difference
/// between them is not something a person acts on differently.
fn kind_of(state: &str) -> Option<Kind> {
    match state {
        "waiting_for_user" => Some(Kind::Waiting),
        "permission_request" => Some(Kind::Permission),
        "failed" => Some(Kind::Failed),
        "complete" | "exited" => Some(Kind::Finished),
        _ => None,
    }
}

/// Ask the daemon one question and parse the answer.
///
/// Through [`crate::proxy::Agentd`], which declares this connection's origin
/// before it sends anything — so the watcher is filed as
/// `claude-remote-control` exactly like a phone's own request, and cannot
/// accidentally be a more privileged caller than the thing it serves.
pub fn ask(agentd: &std::path::Path, request: &str) -> Option<Response> {
    let reply = crate::proxy::Agentd::open(agentd, "rime-remote-push")
        .and_then(|mut a| a.round_trip(request.as_bytes()))
        .ok()?;
    serde_json::from_slice(&reply).ok()
}

/// One full observation of the daemon.
pub fn observe(agentd: &std::path::Path, nexd: &std::path::Path, with_worktrees: bool) -> Observation {
    let sessions = match ask(agentd, r#"{"cmd":"list"}"#) {
        Some(Response::Sessions { sessions }) => sessions,
        _ => Vec::new(),
    };
    let requests = match ask(agentd, r#"{"cmd":"requests"}"#) {
        Some(Response::Requests { requests }) => requests,
        _ => Vec::new(),
    };
    let worktrees = if with_worktrees {
        match ask(agentd, r#"{"cmd":"worktrees"}"#) {
            Some(Response::Worktrees { worktrees }) => Some(worktrees),
            // A failure is NOT an empty list. Returning one would retire every
            // remembered test state and re-raise every failure on the next
            // successful poll.
            _ => None,
        }
    } else {
        None
    };
    // Asked even when nexd is not running: the failed connect is the whole
    // cost, and it is what lets nexd start later and be heard from.
    let nex = crate::nex::pending_approvals(nexd);
    Observation { sessions, worktrees, requests, nex }
}

/// Post one envelope, and say what the server said.
///
/// Hand-written HTTP over the TLS this crate already speaks to the relay, for
/// the reason `relay.rs` gives for hand-writing RFC 6455: the workspace has no
/// HTTP client, and one request with a fixed body and `Connection: close` is a
/// smaller thing to get right than a dependency is to audit.
pub fn deliver(endpoint: &Endpoint, envelope: &Envelope, trust: &Trust) -> Result<Delivery, String> {
    let socket = TcpStream::connect((endpoint.host.as_str(), endpoint.port))
        .map_err(|e| format!("could not reach {}: {e}", endpoint.authority()))?;
    // A push server that accepts a connection and then says nothing must not
    // hold this thread: the watcher is the only thing delivering to every
    // registered phone, and one wedged endpoint would stop the others.
    socket.set_read_timeout(Some(DELIVERY_TIMEOUT)).ok();
    socket.set_write_timeout(Some(DELIVERY_TIMEOUT)).ok();
    // Nagle off, like every other socket this process opens. A delivery is a
    // TLS handshake and one small request — exactly the write-write-read
    // pattern a delayed ACK stalls — and a notification that a session is
    // waiting on its owner is worth the 40 ms.
    socket.set_nodelay(true).ok();
    let (mut reader, mut writer) = trust
        .connect(
            socket.try_clone().map_err(|e| e.to_string())?,
            &endpoint.host,
        )
        .map_err(|e| format!("TLS to {} failed: {e}", endpoint.authority()))?;
    rime_remote_core::push::exchange(&mut reader, &mut writer, endpoint, envelope)
        .map_err(|e| e.to_string())
}

/// Send one raised alert to every registered device.
///
/// Returns how many were delivered, so the caller can say nothing happened
/// when nothing did.
pub fn fan_out(state: &State, raised: &[Raised], trust: Option<&Trust>) -> usize {
    if raised.is_empty() {
        return 0;
    }
    let Some(trust) = trust else { return 0 };
    let mut sent = 0;
    // The store is read, acted on, and written once per round rather than held
    // across the network: a delivery can take twenty seconds, and holding the
    // lock over it would block `push_register` on a phone that is trying to
    // fix exactly the endpoint that is timing out.
    let devices: Vec<(String, String, String)> = {
        let store = state.push_store();
        store
            .registrations
            .values()
            .map(|r| (r.device_id.clone(), r.endpoint.clone(), r.key.clone()))
            .collect()
    };
    for (device_id, endpoint, key) in devices {
        let Ok(parsed) = Endpoint::parse(&endpoint) else {
            // Stored endpoints are validated on the way in, so this is a store
            // somebody edited. Retired rather than retried.
            state.push_forget(&device_id);
            continue;
        };
        let Some(raw) = rime_remote_core::b64_decode(&key) else {
            state.push_forget(&device_id);
            continue;
        };
        for r in raised {
            let Some(seq) = state.push_next_seq(&device_id) else {
                break;
            };
            let Ok(envelope) = Envelope::seal(&raw, &r.body(seq)) else {
                break;
            };
            match deliver(&parsed, &envelope, trust) {
                Ok(Delivery::Accepted) => {
                    state.push_delivered(&device_id);
                    sent += 1;
                }
                Ok(Delivery::Gone) => {
                    // RFC 8030's way of saying the subscription is gone.
                    // Retrying it is a request that will never succeed.
                    state.push_forget(&device_id);
                    break;
                }
                Ok(Delivery::Retry(_)) | Err(_) => {
                    if state.push_failed(&device_id) >= MAX_FAILURES {
                        state.push_forget(&device_id);
                    }
                    break;
                }
            }
        }
    }
    sent
}

/// Deliver one alert to ONE device: a ring, which is about one phone and is
/// never fanned out.
///
/// Same bookkeeping as [`fan_out`] (sequence drawn and persisted, failures
/// counted, a gone subscription retired), but the outcome is returned in words
/// for the person who asked, since somebody is waiting on this one.
pub fn push_one(state: &State, device_id: &str, raised: Raised, trust: &Trust) -> Result<(), String> {
    let (endpoint, key) = {
        let store = state.push_store();
        let r = store
            .registrations
            .get(device_id)
            .ok_or("it has no push registration")?;
        (r.endpoint.clone(), r.key.clone())
    };
    let parsed = Endpoint::parse(&endpoint).map_err(|e| e.to_string())?;
    let raw = rime_remote_core::b64_decode(&key).ok_or("its push key is unreadable")?;
    let seq = state
        .push_next_seq(device_id)
        .ok_or("it has no push registration")?;
    let envelope = Envelope::seal(&raw, &raised.body(seq)).map_err(|e| e.to_string())?;
    let failed = |why: String| {
        if state.push_failed(device_id) >= MAX_FAILURES {
            state.push_forget(device_id);
        }
        Err(why)
    };
    match deliver(&parsed, &envelope, trust) {
        Ok(Delivery::Accepted) => {
            state.push_delivered(device_id);
            Ok(())
        }
        Ok(Delivery::Gone) => {
            state.push_forget(device_id);
            Err("its push service no longer knows it; open Rime Remote on it to register again".into())
        }
        Ok(Delivery::Retry(code)) => failed(format!("its push service answered {code}")),
        Err(e) => failed(e),
    }
}

/// The loop. Never returns.
pub fn supervise(state: Arc<State>, agentd: std::path::PathBuf) {
    // One trust store for the life of the daemon, like the relay's. Reading
    // `/etc/pki` on every delivery would be a syscall storm for a value that
    // does not change, and a machine with no roots should refuse once rather
    // than once a minute.
    let trust = match Trust::system() {
        Ok(t) => Some(t),
        Err(e) => {
            eprintln!(
                "rime-remoted: push notifications are off — no usable certificate store ({e}). \
                 A phone with the app open still gets alerts from its own poll."
            );
            None
        }
    };
    let mut watcher = Watcher::new();
    let mut last_worktrees = std::time::Instant::now() - WORKTREE_INTERVAL;
    loop {
        match poll_plan(state.push_is_empty(), last_worktrees.elapsed() >= WORKTREE_INTERVAL) {
            None => {
                // Nothing registered, so the watcher is discarded rather than
                // advanced against nobody. Its state would be a backlog by the
                // time a phone arrived, and `Watcher::new()` is the rule that
                // the first observation raises nothing — which is exactly what
                // a phone registering should see.
                watcher = Watcher::new();
            }
            Some(with_worktrees) => {
                if with_worktrees {
                    last_worktrees = std::time::Instant::now();
                }
                let raised = watcher.observe(&observe(&agentd, &state.nexd(), with_worktrees));
                fan_out(&state, &raised, trust.as_ref());
            }
        }
        std::thread::sleep(LIST_INTERVAL);
    }
}

/// Whether to ask the daemon anything this round, and whether to include
/// worktrees.
///
/// `None` means ask it nothing, and that is the branch worth writing down.
/// Every Rime machine runs `rime-remoted`, and most of them will never have a
/// phone registered; a watcher that polled anyway would put `list` and
/// `requests` on a four-second timer on all of them, and — the expensive part —
/// would run `worktrees` every minute, which means git in every remembered
/// project with a `merge-tree --write-tree` per worktree. That is real work on
/// a machine that has asked for none of it, forever, to compute alerts with
/// nowhere to send them.
///
/// The cost of skipping is bounded and is stated where the caller acts on it:
/// the watcher is reset, so the first round after a phone registers raises
/// nothing — which is the rule anyway, for the same reason a restart does not
/// fire six notifications for sessions that have been waiting since yesterday.
pub fn poll_plan(push_empty: bool, worktrees_due: bool) -> Option<bool> {
    if push_empty {
        None
    } else {
        Some(worktrees_due)
    }
}

/// The two verbs this service answers **itself**, before the line reaches
/// `rime-agentd`.
///
/// ## Why they are intercepted here and are not daemon verbs
///
/// `wire.rs` states the invariant this bends: a `Frame::Control` carries one
/// line of `rime-agentd`'s own protocol, verbatim, and that is what makes a
/// new daemon verb reach a phone for free. Push registration cannot be a
/// daemon verb, because `rime-agentd` has no concept of a transport — it binds
/// a Unix socket in a 0700 directory and has never heard of a phone, a relay
/// or an endpoint. A verb about *how this connection is reached* belongs to
/// the process that owns the connection.
///
/// The invariant is bent rather than broken: these two names are reserved
/// here, everything else is forwarded untouched, and a daemon that grows a
/// verb by either name would be a collision this file would have to notice.
/// The alternative — a new `Frame` tag — would have been a wire change and a
/// version bump for two request/response messages, which is the more expensive
/// end of the same trade.
///
/// ## Graceful degradation is the point of the reply shape
///
/// A phone talking to an older `rime-remoted` has its `push_register`
/// forwarded to `rime-agentd`, which does not know the verb and answers
/// `bad_request`. That is the correct outcome and the Android client is
/// written to read it as "this machine cannot wake me; poll while you are
/// open" rather than as a failure. So the replies here are in the daemon's own
/// `Response` vocabulary and nothing new has to be parsed.
///
/// Returns `None` for a line this module does not own.
pub fn control(state: &State, device_id: &str, line: &[u8]) -> Option<Vec<u8>> {
    let v: serde_json::Value = serde_json::from_slice(line).ok()?;
    let cmd = v.get("cmd")?.as_str()?;
    // Checked against the list rather than only by the match below, so that
    // "which verbs does this module take" has one answer a test can read.
    if !VERBS.contains(&cmd) {
        return None;
    }
    match cmd {
        "push_register" => {
            let endpoint = v.get("endpoint").and_then(|e| e.as_str()).unwrap_or_default();
            let key = v.get("key").and_then(|k| k.as_str()).unwrap_or_default();
            // The device id comes from the HANDSHAKE and never from the body.
            // A paired phone may register its own endpoint and nobody else's;
            // a body field naming a device would be a way to redirect another
            // phone's notifications to an endpoint of the caller's choosing.
            Some(
                match state.push_register(device_id, endpoint, key, rime_remote_core::now_ms()) {
                    Ok(()) => ok(),
                    Err(e) => error("bad_request", &e.to_string()),
                },
            )
        }
        "push_unregister" => {
            state.push_forget(device_id);
            // `ok` whether or not anything was there. Unregistering twice is
            // not an error: the caller's intent is the same both times.
            Some(ok())
        }
        _ => None,
    }
}

/// The verbs [`control`] owns, so a test can assert nothing else is taken.
pub const VERBS: [&str; 2] = ["push_register", "push_unregister"];

fn ok() -> Vec<u8> {
    br#"{"reply":"ok"}"#.to_vec()
}

fn error(kind: &str, message: &str) -> Vec<u8> {
    serde_json::json!({ "reply": "error", "kind": kind, "message": message })
        .to_string()
        .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rime_agent_core::protocol::SessionInfo;

    fn session(id: u32, state: &str, agent: &str) -> SessionInfo {
        let mut v = serde_json::to_value(sample()).expect("sample");
        v["id"] = serde_json::json!(id);
        v["state"] = serde_json::json!(state);
        v["agent"] = serde_json::json!(agent);
        v["started"] = serde_json::json!(1_726_000_000u64);
        serde_json::from_value(v).expect("session")
    }

    /// A real `SessionInfo`, deserialised from the daemon's own wire shape.
    ///
    /// Deliberately parsed rather than built with a struct literal: a required
    /// field added to `SessionInfo` then fails HERE, loudly, instead of the
    /// literal being updated with a plausible value and the watcher never
    /// being exercised against the real record.
    fn sample() -> SessionInfo {
        serde_json::from_str(
            r#"{"id":1,"agent":"claude","program":"claude","args":[],"cwd":"/home/a",
                "project":null,"project_name":null,"worktree":null,"state":"working",
                "detail":null,"paused":false,"pid":1234,"started":1,"last_activity":1,
                "exit_code":null,"exit_signal":null,"attached":0,"checkpoint":null,
                "cols":80,"rows":24}"#,
        )
        .expect("a sample session")
    }

    fn obs(sessions: Vec<SessionInfo>) -> Observation {
        Observation { sessions, worktrees: None, requests: Vec::new(), nex: None }
    }

    #[test]
    fn the_first_observation_raises_nothing() {
        // Six sessions that have been waiting since yesterday are not six
        // things that just happened. An empty map is also the state after a
        // daemon restart, which is exactly when the burst would be worst.
        let mut w = Watcher::new();
        let waiting: Vec<SessionInfo> = (1..=6)
            .map(|i| session(i, "waiting_for_user", "claude"))
            .collect();
        assert!(w.observe(&obs(waiting.clone())).is_empty());
        // And the second observation of the same thing still raises nothing,
        // because nothing changed.
        assert!(w.observe(&obs(waiting)).is_empty());
    }

    #[test]
    fn one_turn_raises_one_alert_however_many_times_the_daemon_republishes_it() {
        // Criterion 4's real duplicate. Four paths inside Rime publish
        // `waiting_for_user` for one turn and `Session::set_state` has no
        // same-state early return, so a watcher keying on "is waiting" would
        // push four times seconds apart.
        let mut w = Watcher::new();
        w.observe(&obs(vec![session(1, "working", "claude")]));
        let first = w.observe(&obs(vec![session(1, "waiting_for_user", "claude")]));
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].kind, Kind::Waiting);
        assert_eq!(first[0].session, 1);
        assert_eq!(first[0].adapter, Adapter::Claude);
        for _ in 0..3 {
            assert!(
                w.observe(&obs(vec![session(1, "waiting_for_user", "claude")])).is_empty(),
                "a republished state raised a second alert"
            );
        }
        // Answered, then waiting again: that IS news, and must raise again.
        w.observe(&obs(vec![session(1, "working", "claude")]));
        assert_eq!(w.observe(&obs(vec![session(1, "waiting_for_user", "claude")])).len(), 1);
    }

    #[test]
    fn a_recycled_session_id_is_not_diffed_against_a_stranger() {
        // `Request::Remove` and `Prune` make an id reusable, so session 1 today
        // and session 1 tomorrow are different agents doing different work.
        //
        // The failure this catches is SILENT and is the worse direction: with
        // the pruned entry left in the map, a new session that arrives already
        // waiting is compared against the OLD session's `waiting_for_user`,
        // reads as "no change", and the new agent's request for input is never
        // delivered. Forgetting the id makes it a first sighting of a session
        // that wants somebody, which is news.
        let mut w = Watcher::new();
        w.observe(&obs(vec![session(1, "working", "claude")]));
        assert_eq!(
            w.observe(&obs(vec![session(1, "waiting_for_user", "claude")])).len(),
            1
        );
        // Pruned.
        assert!(w.observe(&obs(vec![])).is_empty());

        let out = w.observe(&obs(vec![session(1, "waiting_for_user", "codex")]));
        assert_eq!(
            out.len(),
            1,
            "a recycled id was diffed against the previous session's state, and the new \
             agent's request for input was swallowed"
        );
        assert_eq!(
            out[0].adapter,
            Adapter::Codex,
            "the alert belongs to the session that is there now"
        );
    }

    #[test]
    fn a_session_first_seen_in_a_state_nobody_needs_to_act_on_raises_nothing() {
        // The other side of the same rule. A first sighting raises only when
        // the state is news; `working` never is, so a session that appears
        // between two polls and is busy does not notify anybody.
        let mut w = Watcher::new();
        w.observe(&obs(vec![]));
        assert!(w.observe(&obs(vec![session(4, "working", "claude")])).is_empty());
        assert!(w.observe(&obs(vec![
            session(4, "working", "claude"),
            session(5, "starting", "codex"),
        ]))
        .is_empty());
    }

    #[test]
    fn starting_and_working_are_not_news_and_every_other_state_is() {
        let mut w = Watcher::new();
        w.observe(&obs(vec![session(1, "starting", "claude")]));
        assert!(w.observe(&obs(vec![session(1, "working", "claude")])).is_empty());
        for (state, kind) in [
            ("waiting_for_user", Kind::Waiting),
            ("permission_request", Kind::Permission),
            ("failed", Kind::Failed),
            ("complete", Kind::Finished),
        ] {
            let mut w = Watcher::new();
            w.observe(&obs(vec![session(1, "working", "claude")]));
            let out = w.observe(&obs(vec![session(1, state, "claude")]));
            assert_eq!(out.len(), 1, "{state} raised {} alerts", out.len());
            assert_eq!(out[0].kind, kind, "{state}");
        }
        // `exited` is the same news as `complete`: the session ended.
        let mut w = Watcher::new();
        w.observe(&obs(vec![session(1, "working", "claude")]));
        assert_eq!(
            w.observe(&obs(vec![session(1, "exited", "claude")]))[0].kind,
            Kind::Finished
        );
    }

    #[test]
    fn nothing_from_a_session_record_reaches_the_bytes_that_leave_this_machine() {
        // CRITERION 2, driven through the whole path rather than asserted
        // against a hand-built body — against one of those the assertion would
        // be vacuous, since `Body` has no field to put a secret in, which is
        // the property being checked.
        //
        // The session carries a password in `detail`, a path in `cwd`, a
        // project, a worktree and an argument, all of which `hook::detail_for`
        // really does copy verbatim.
        const SECRETS: [&str; 6] = [
            "hunter2-the-password",
            "/home/andre/clients/acme-secret",
            "acme-secret",
            "wt-merger-due-diligence",
            "--prompt=rewrite the payroll importer",
            "grep -r AWS_SECRET_ACCESS_KEY",
        ];
        let mut v = serde_json::to_value(session(1, "waiting_for_user", "claude")).expect("json");
        v["detail"] = serde_json::json!(SECRETS[0]);
        v["cwd"] = serde_json::json!(SECRETS[1]);
        v["project"] = serde_json::json!(SECRETS[2]);
        v["project_name"] = serde_json::json!(SECRETS[2]);
        v["worktree"] = serde_json::json!(SECRETS[3]);
        v["args"] = serde_json::json!([SECRETS[4], SECRETS[5]]);
        let loaded: SessionInfo = serde_json::from_value(v).expect("session");
        // The record really does carry them, or the assertion below proves
        // nothing.
        assert_eq!(loaded.detail.as_deref(), Some(SECRETS[0]));
        assert_eq!(loaded.cwd, SECRETS[1]);

        let mut w = Watcher::new();
        w.observe(&obs(vec![session(1, "working", "claude")]));
        let raised = w.observe(&obs(vec![loaded]));
        assert_eq!(raised.len(), 1);

        let key = [3u8; 32];
        let envelope = Envelope::seal(&key, &raised[0].body(1)).expect("seal");
        let endpoint = Endpoint::parse("https://ntfy.sh/upAbCdEf").expect("endpoint");
        // Everything that would cross the push infrastructure: the request
        // line, the headers and the body.
        let wire = endpoint.request(&envelope);
        for secret in SECRETS {
            assert!(
                !window(&wire, secret.as_bytes()),
                "{secret:?} reached the bytes that leave this machine"
            );
            // And not in the plaintext either, which is the stronger claim:
            // the body is 22 fixed bytes, so there is nowhere to put it even
            // before encryption.
            assert!(!window(&raised[0].body(1).pack(), secret.as_bytes()));
        }
        assert_eq!(envelope.as_bytes().len(), rime_remote_core::push::ENVELOPE_LEN);
    }

    fn window(haystack: &[u8], needle: &[u8]) -> bool {
        !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
    }

    fn worktree(path: &str, tests: TestState, sessions: Vec<u32>) -> WorktreeStatus {
        let mut v = serde_json::to_value(sample_worktree()).expect("json");
        v["path"] = serde_json::json!(path);
        v["tests"] = serde_json::to_value(&tests).expect("tests");
        v["sessions"] = serde_json::json!(sessions);
        serde_json::from_value(v).expect("worktree")
    }

    fn sample_worktree() -> WorktreeStatus {
        serde_json::from_str(
            r#"{"name":"wt","slug":"p","path":"/w","branch":null,"head":null,
                "is_agent":true,"base":null,"dirty":false,
                "diff":{"files":0,"insertions":0,"deletions":0},
                "ahead":null,"behind":null,"upstream":null,"unpushed":null,
                "conflicts":{"state":"clean"},"tests":{"state":"unobserved"},
                "sessions":[],"ready":{"ready_to_propose":true,"blockers":[]}}"#,
        )
        .expect("a sample worktree")
    }

    fn failed() -> TestState {
        TestState::Failed {
            command: "cargo test".into(),
            finished: 10,
            head: None,
        }
    }

    fn passed() -> TestState {
        TestState::Passed {
            command: "cargo test".into(),
            finished: 10,
            head: None,
        }
    }

    #[test]
    fn a_test_failure_raises_once_on_the_edge_and_carries_its_session() {
        let mut w = Watcher::new();
        // The session list is polled several times before worktrees are, and
        // the first worktree poll must still raise nothing.
        w.observe(&obs(vec![session(7, "working", "claude")]));
        w.observe(&obs(vec![session(7, "working", "claude")]));
        let mut o = obs(vec![session(7, "working", "claude")]);
        o.worktrees = Some(vec![worktree("/w/a", failed(), vec![7])]);
        assert!(
            w.observe(&o).is_empty(),
            "the first worktree poll raised a failure that had been failing all along"
        );

        let mut o = obs(vec![session(7, "working", "claude")]);
        o.worktrees = Some(vec![worktree("/w/a", passed(), vec![7])]);
        assert!(w.observe(&o).is_empty());

        let mut o = obs(vec![session(7, "working", "claude")]);
        o.worktrees = Some(vec![worktree("/w/a", failed(), vec![7])]);
        let out = w.observe(&o);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, Kind::TestFailed);
        assert_eq!(out[0].session, 7, "the alert must open the session that did it");
        assert_eq!(out[0].adapter, Adapter::Claude);

        // Still failing is not news again.
        let mut o = obs(vec![session(7, "working", "claude")]);
        o.worktrees = Some(vec![worktree("/w/a", failed(), vec![7])]);
        assert!(w.observe(&o).is_empty());
    }

    #[test]
    fn a_worktree_poll_that_did_not_happen_is_not_an_empty_list() {
        // The distinction that breaks this if it is lost: `None` means "not
        // asked this round", `Some(vec![])` means "there are none". Treating
        // the first as the second retires every remembered test state, and the
        // next real poll re-raises every failure as though it were new.
        let mut w = Watcher::new();
        let mut o = obs(vec![]);
        o.worktrees = Some(vec![worktree("/w/a", passed(), vec![])]);
        w.observe(&o);
        // Three rounds with no worktree poll.
        for _ in 0..3 {
            assert!(w.observe(&obs(vec![])).is_empty());
        }
        let mut o = obs(vec![]);
        o.worktrees = Some(vec![worktree("/w/a", failed(), vec![])]);
        let out = w.observe(&o);
        assert_eq!(out.len(), 1, "the edge was lost across rounds that did not poll");
        assert_eq!(out[0].session, NO_SESSION);
    }

    fn request(id: u32, verb: &str, decision: &str) -> PrivilegeRequest {
        let mut body = serde_json::json!({
            "id": id,
            "verb": verb,
            "reason": "because",
            "session": null,
            "agent": "claude",
            "project": null,
            "request_origin": null,
            "origin_source": null,
            "actor": null,
            "decision": decision,
            "created_ms": 1,
            "decided_ms": null,
            "executed_ms": null,
            "exit_code": null,
            "system_grant": null,
        });
        // `Verb` is an internally tagged enum flattened into the request, so
        // the two verbs that carry arguments need them or it does not parse.
        if matches!(verb, "install" | "remove") {
            body["packages"] = serde_json::json!(["hello"]);
        }
        serde_json::from_value(body).expect("request")
    }

    fn executed(mut r: PrivilegeRequest, code: i32) -> PrivilegeRequest {
        r.executed_ms = Some(2);
        r.exit_code = Some(code);
        r
    }

    #[test]
    fn a_pending_privilege_request_is_announced_once() {
        let mut w = Watcher::new();
        w.observe(&obs(vec![]));
        let mut o = obs(vec![]);
        o.requests = vec![request(5, "install", "pending")];
        let out = w.observe(&o);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, Kind::Approval);
        // Still pending is not news.
        let mut o = obs(vec![]);
        o.requests = vec![request(5, "install", "pending")];
        assert!(w.observe(&o).is_empty());
    }

    #[test]
    fn a_deployment_raises_when_it_finishes_and_says_whether_it_worked() {
        // P1-058's deployment criterion. The event is real and on the wire
        // already: `executed_ms` is set when the operation ran and `exit_code`
        // says how it went.
        for (code, kind) in [(0, Kind::Deployed), (1, Kind::DeployFailed)] {
            let mut w = Watcher::new();
            w.observe(&obs(vec![]));
            let mut o = obs(vec![]);
            o.requests = vec![request(9, "update", "allow_once")];
            let out = w.observe(&o);
            assert!(out.is_empty(), "an approved-but-unrun deployment raised {out:?}");

            let mut o = obs(vec![]);
            o.requests = vec![executed(request(9, "update", "allow_once"), code)];
            let out = w.observe(&o);
            assert_eq!(out.len(), 1, "exit {code} raised {} alerts", out.len());
            assert_eq!(out[0].kind, kind, "exit {code}");

            // And not again on the next poll.
            let mut o = obs(vec![]);
            o.requests = vec![executed(request(9, "update", "allow_once"), code)];
            assert!(w.observe(&o).is_empty());
        }
    }

    #[test]
    fn only_the_deployment_verbs_raise_a_deployment() {
        for verb in ["update", "rollback", "pin", "pkg_rebuild", "pkg_rollback"] {
            let mut w = Watcher::new();
            w.observe(&obs(vec![]));
            let mut o = obs(vec![]);
            o.requests = vec![executed(request(1, verb, "allow_once"), 0)];
            let out = w.observe(&o);
            assert!(
                out.iter().any(|r| r.kind == Kind::Deployed),
                "{verb} raised no deployment"
            );
        }
        // A package operation's notification is its approval; a second one on
        // completion would double every install.
        for verb in ["install", "remove", "pkg_upgrade"] {
            let mut w = Watcher::new();
            w.observe(&obs(vec![]));
            let mut o = obs(vec![]);
            o.requests = vec![executed(request(1, verb, "allow_once"), 0)];
            let out = w.observe(&o);
            assert!(
                !out.iter().any(|r| r.kind.is_deployment()),
                "{verb} raised a deployment"
            );
        }
    }

    #[test]
    fn a_deployment_that_finished_before_this_daemon_started_is_not_news() {
        // The same rule as the first poll, and it matters more here: a machine
        // that was updated last week has a finished `update` request sitting in
        // the daemon's list, and restarting rime-remoted must not announce it.
        let mut w = Watcher::new();
        let mut o = obs(vec![]);
        o.requests = vec![executed(request(9, "update", "allow_once"), 0)];
        assert!(w.observe(&o).is_empty());
        let mut o = obs(vec![]);
        o.requests = vec![executed(request(9, "update", "allow_once"), 0)];
        assert!(w.observe(&o).is_empty());
    }

    #[test]
    fn every_criterion_one_category_has_a_kind_that_can_be_raised() {
        // P1-058's first criterion, as a list rather than as prose. Each entry
        // names the category and the observation that produces it, so a
        // category that lost its source fails here.
        let mut w = Watcher::new();
        w.observe(&obs(vec![session(1, "working", "claude")]));

        let mut raised: BTreeSet<u8> = BTreeSet::new();
        for state in ["waiting_for_user", "permission_request", "failed", "complete"] {
            let mut w = Watcher::new();
            w.observe(&obs(vec![session(1, "working", "claude")]));
            for r in w.observe(&obs(vec![session(1, state, "claude")])) {
                raised.insert(r.kind.code());
            }
        }
        {
            let mut w = Watcher::new();
            let mut o = obs(vec![]);
            o.worktrees = Some(vec![worktree("/w/a", passed(), vec![])]);
            w.observe(&o);
            let mut o = obs(vec![]);
            o.worktrees = Some(vec![worktree("/w/a", failed(), vec![])]);
            for r in w.observe(&o) {
                raised.insert(r.kind.code());
            }
        }
        {
            let mut w = Watcher::new();
            w.observe(&obs(vec![]));
            let mut o = obs(vec![]);
            o.requests = vec![request(5, "install", "pending")];
            for r in w.observe(&o) {
                raised.insert(r.kind.code());
            }
            let mut o = obs(vec![]);
            o.requests = vec![executed(request(6, "update", "allow_once"), 0)];
            for r in w.observe(&o) {
                raised.insert(r.kind.code());
            }
            let mut o = obs(vec![]);
            o.requests = vec![executed(request(7, "update", "allow_once"), 3)];
            for r in w.observe(&o) {
                raised.insert(r.kind.code());
            }
        }
        {
            let mut w = Watcher::new();
            w.observe(&nex(&[]));
            for r in w.observe(&nex(&["ap-1"])) {
                raised.insert(r.kind.code());
            }
        }
        // Ring and Sent are the kinds the watcher never raises: somebody at this
        // machine asks for it (`rime remote ring`), it goes to one device
        // through `push_one`, and `phone.rs` tests that path.
        let want: BTreeSet<u8> = Kind::ALL
            .iter()
            .filter(|k| !matches!(**k, Kind::Ring | Kind::Sent))
            .map(|k| k.code())
            .collect();
        assert_eq!(
            raised, want,
            "a kind in the vocabulary has no observation that raises it"
        );
    }


    /// A `State` with its stores in a directory of this test's own.
    fn state(tag: &str) -> (Arc<State>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "rime-push-state-{}-{tag}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let store = dir.join("devices.json");
        let identity = rime_remote_core::identity::Identity::generate().expect("identity");
        let s = State::new(
            identity,
            "test-machine".into(),
            7717,
            std::net::IpAddr::from([127, 0, 0, 1]),
            None,
            store,
            Duration::from_secs(15),
        )
        .expect("state");
        (s, dir)
    }

    const A_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    #[test]
    fn registering_binds_the_endpoint_to_the_handshake_and_not_to_the_body() {
        // The attack this forecloses: a paired phone naming ANOTHER device in
        // the request and redirecting that device's notifications — which are
        // its owner's, and whose envelopes it could then at least count — to
        // an endpoint of its choosing.
        let (st, dir) = state("bind");
        let line = br#"{"cmd":"push_register","endpoint":"https://ntfy.sh/upA","key":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","device":"someone-else"}"#;
        assert_eq!(control(&st, "dev-aaaa", line), Some(br#"{"reply":"ok"}"#.to_vec()));
        let store = st.push_store();
        assert_eq!(store.registrations.len(), 1);
        assert!(store.registrations.contains_key("dev-aaaa"));
        assert!(!store.registrations.contains_key("someone-else"));
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_registration_survives_a_restart_and_a_revoke_takes_it_away() {
        let (st, dir) = state("persist");
        st.push_register("dev-aaaa", "https://ntfy.sh/upA", A_KEY, 1)
            .expect("register");
        // The sequence has to survive too: the phone's replay guard is "a
        // sequence greater than the last I saw", so a daemon that rewound
        // would have every envelope it sent afterwards correctly ignored.
        assert_eq!(st.push_next_seq("dev-aaaa"), Some(1));
        assert_eq!(st.push_next_seq("dev-aaaa"), Some(2));

        let back = rime_remote_core::push::PushStore::load(&st.push_path).expect("load");
        assert_eq!(back.registrations["dev-aaaa"].seq, 2);
        assert_eq!(back.registrations["dev-aaaa"].endpoint, "https://ntfy.sh/upA");

        assert!(st.push_forget("dev-aaaa"));
        let back = rime_remote_core::push::PushStore::load(&st.push_path).expect("load");
        assert!(
            back.registrations.is_empty(),
            "a forgotten registration was still on disk, so a revoked phone keeps being woken"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_unusable_registration_is_refused_in_the_daemons_own_words() {
        let (st, dir) = state("refuse");
        for line in [
            br#"{"cmd":"push_register","endpoint":"http://ntfy.sh/upA","key":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#.to_vec(),
            br#"{"cmd":"push_register","endpoint":"https://ntfy.sh/upA","key":"not base64!!"}"#.to_vec(),
            br#"{"cmd":"push_register"}"#.to_vec(),
        ] {
            let reply = control(&st, "dev-aaaa", &line).expect("owned");
            let v: serde_json::Value = serde_json::from_slice(&reply).expect("json");
            assert_eq!(v["reply"], "error", "{}", String::from_utf8_lossy(&reply));
            // The daemon's own vocabulary, so the phone needs no new parser.
            assert_eq!(v["kind"], "bad_request");
            assert!(v["message"].as_str().is_some_and(|m| !m.is_empty()));
        }
        assert!(st.push_is_empty(), "a refusal registered something");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unregistering_is_idempotent_and_everything_else_is_forwarded() {
        let (st, dir) = state("forward");
        assert_eq!(
            control(&st, "dev-aaaa", br#"{"cmd":"push_unregister"}"#),
            Some(br#"{"reply":"ok"}"#.to_vec()),
            "unregistering something that was never there is not an error"
        );
        // Everything else must reach `rime-agentd` untouched. `wire.rs` says a
        // control frame carries the daemon's protocol verbatim, and a verb
        // this module swallowed would be a verb the phone silently lost.
        for line in [
            br#"{"cmd":"list"}"#.to_vec(),
            br#"{"cmd":"requests"}"#.to_vec(),
            br#"{"cmd":"input","id":1,"data":"hi"}"#.to_vec(),
            br#"{"cmd":"push"}"#.to_vec(),
            br#"{"cmd":"push_registers"}"#.to_vec(),
            // Answered by this service too, but one level up: `serve::VERBS`
            // is checked before this function is asked, and push must not
            // claim a verb that is not about push. The union of the two
            // lists is pinned in `serve`'s own test.
            br#"{"cmd":"remote_hello"}"#.to_vec(),
            b"not json".to_vec(),
            br#"{"no":"cmd"}"#.to_vec(),
        ] {
            assert_eq!(
                control(&st, "dev-aaaa", &line),
                None,
                "{} was taken by push and never reached the daemon",
                String::from_utf8_lossy(&line)
            );
        }
        assert_eq!(VERBS.len(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_delivery_failure_retires_a_registration_only_after_it_persists() {
        let (st, dir) = state("failures");
        st.push_register("dev-aaaa", "https://ntfy.sh/upA", A_KEY, 1)
            .expect("register");
        for i in 1..MAX_FAILURES {
            assert_eq!(st.push_failed("dev-aaaa"), i);
        }
        // One short of the limit the registration is still there: a push
        // server that was down for an afternoon must not cost the pairing.
        assert!(!st.push_is_empty());
        st.push_delivered("dev-aaaa");
        assert_eq!(
            st.push_failed("dev-aaaa"),
            1,
            "a success did not clear the run of failures"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_machine_with_no_phone_registered_asks_the_daemon_nothing() {
        // The cost of this feature on a machine that is not using it. Every
        // Rime machine runs rime-remoted; most will never have a phone. A
        // `worktrees` poll runs git in every remembered project with a
        // `merge-tree --write-tree` per worktree, and putting that on a
        // sixty-second timer for everybody would be a regression nobody asked
        // for and nobody would attribute to notifications.
        assert_eq!(poll_plan(true, false), None);
        assert_eq!(
            poll_plan(true, true),
            None,
            "the expensive poll ran on a machine with nothing registered"
        );
        // And with a phone registered it polls, worktrees only when due.
        assert_eq!(poll_plan(false, false), Some(false));
        assert_eq!(poll_plan(false, true), Some(true));
    }

    #[test]
    fn a_watcher_discarded_while_nobody_was_registered_raises_no_backlog() {
        // What the `None` branch above costs, asserted rather than asserted
        // about. A phone registering must not be met with every transition
        // that happened while it was not there — six sessions that have been
        // waiting since yesterday are not six things that just happened.
        let mut w = Watcher::new();
        w.observe(&obs(vec![session(1, "working", "claude")]));
        assert_eq!(w.observe(&obs(vec![session(1, "waiting_for_user", "claude")])).len(), 1);
        // Nobody registered: the loop replaces it.
        w = Watcher::new();
        assert!(
            w.observe(&obs(vec![session(1, "waiting_for_user", "claude")])).is_empty(),
            "a phone registering was handed a backlog"
        );
    }

    #[test]
    fn a_session_id_too_large_for_the_envelope_never_becomes_no_session() {
        assert_eq!(session_id(0), 0);
        assert_eq!(session_id(7), 7);
        assert_eq!(session_id(u32::MAX), i32::MAX);
        assert_ne!(session_id(u32::MAX), NO_SESSION);
        // Every u32 that wraps to a negative i32 must saturate instead, or one
        // of them is NO_SESSION and its notification opens the wrong screen.
        for id in [i32::MAX as u32 + 1, u32::MAX - 1, 0x8000_0000] {
            assert!(session_id(id) >= 0, "{id} became a negative session");
        }
    }

    fn nex(ids: &[&str]) -> Observation {
        Observation {
            nex: Some(ids.iter().map(|s| s.to_string()).collect()),
            ..Observation::default()
        }
    }

    #[test]
    fn a_new_nex_approval_raises_one_alert_that_names_nothing() {
        let mut w = Watcher::new();
        // What was waiting when nexd was first heard from did not just happen.
        assert!(w.observe(&nex(&["ap-old"])).is_empty());
        assert!(w.observe(&nex(&["ap-old"])).is_empty(), "nothing changed");
        let out = w.observe(&nex(&["ap-old", "ap-1", "ap-2"]));
        assert_eq!(out.len(), 1, "two approvals in one poll are one buzz: {out:?}");
        assert_eq!(out[0].kind, Kind::NexApproval);
        // No session number, no start time, no adapter: the body says only
        // "nex needs a decision", and the phone reads the rest over its link.
        assert_eq!(out[0].session, NO_SESSION);
        assert_eq!(out[0].started, 0);
        assert_eq!(out[0].adapter, Adapter::Unknown);
        assert!(w.observe(&nex(&["ap-1", "ap-2"])).is_empty(), "an answer is not news");
        assert_eq!(w.observe(&nex(&["ap-2", "ap-3"])).len(), 1, "a later one is");
    }

    #[test]
    fn nexd_not_answering_forgets_nothing_and_its_first_answer_raises_nothing() {
        let mut w = Watcher::new();
        // nexd not running yet: sessions are watched as before.
        assert!(w.observe(&Observation::default()).is_empty());
        // nexd starts with an approval already waiting: not news.
        assert!(w.observe(&nex(&["ap-1"])).is_empty());
        // One poll where nexd did not answer must not empty the set, or the
        // next answer would announce ap-1 again.
        assert!(w.observe(&Observation::default()).is_empty());
        assert!(
            w.observe(&nex(&["ap-1"])).is_empty(),
            "an approval was announced again after nexd missed one poll"
        );
    }

    #[test]
    fn a_nex_alert_seals_into_the_same_fifty_one_bytes_as_every_other() {
        let mut w = Watcher::new();
        w.observe(&nex(&[]));
        let out = w.observe(&nex(&["ap-secret-run-rm-rf"]));
        let key = [7u8; 32];
        let sealed = Envelope::seal(&key, &out[0].body(1)).expect("seal");
        assert_eq!(sealed.as_bytes().len(), rime_remote_core::push::ENVELOPE_LEN);
        let opened = Envelope::open(&key, sealed.as_bytes()).expect("open");
        assert_eq!(opened.kind, Kind::NexApproval);
        assert_eq!(opened.session, NO_SESSION);
    }
}
