//! Agents across computers (M4, contract §2.15): one computer lists, reads
//! and attaches to the agent sessions of another of the owner's computers.
//!
//! ## The grant, and where it lives
//!
//! What another computer may do with THIS computer's agent sessions is a
//! grant a person here set (`computer_grant`, behind the human gate), kept in
//! this computer's own record of that computer (`Share::agents`): off (the
//! default), `view` or `control`. Nothing another computer sends changes it,
//! and it is never sent anywhere except as a hint to the computer it is about
//! (its own `remote_hello` answer here), which that computer shows as a claim
//! and never treats as a permission.
//!
//! ## The peer side
//!
//! A peer session's verbs here ([`VERBS`], and the channel [`ATTACH_VERB`])
//! are checked against that grant, read from the store at the moment of the
//! request ([`allowed`]): `view` lists the sessions and reads the tail of one;
//! `control` also attaches to one and resizes it. Without the grant the
//! answer is the same `not_allowed` as any verb outside the peer list, so a
//! computer with no grant learns nothing more than one asking for nex.
//!
//! Nothing a peer sends is passed on. Each request to `rime-agentd` is built
//! here from fields this module checked (a session id as `u32`, a size in a
//! fixed range, a byte count under the cap) and sent on a connection that
//! declared origin `claude-remote-control` with actor `computer:<id>`, so
//! `rime-agentd` files the caller as a remote viewer: its §1.7 size rule
//! applies (a remote viewer never shrinks the local one), and a managed
//! session's PTY stays closed to it. The verbs that answer privilege requests
//! (`decide`, `requests`), start or kill sessions, type text with `input`,
//! nex, input or lock control are not built here at all.
//!
//! ## The controlling side
//!
//! `computer_agents`, `computer_agent_peek`, `computer_attach` and
//! `computer_agent_resize` dial the other computer and ask. An attach holds
//! the control connection the way "Add a computer" does: after the
//! `computer_attached` reply it carries raw terminal bytes both ways, with
//! OSC 52 (the clipboard escape) taken out of what the other computer sends,
//! so a session there can neither write nor read this computer's clipboard
//! through the terminal it is shown in.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rime_agent_core::protocol::SessionInfo;
use rime_remote_core::device::{AgentGrant, Device};
use rime_remote_core::wire::Frame;
use serde_json::{json, Value};

use crate::control::Reply;
use crate::state::State;

/// The feature a computer that answers these verbs lists in `remote_hello`,
/// to a peer it has granted anything to.
pub const FEATURE: &str = "peer_agents";
/// `view`: the sessions.
pub const LIST_VERB: &str = "peer_agents";
/// `view`: the tail of one session.
pub const PEEK_VERB: &str = "peer_agent_peek";
/// `control`: tell one session the other computer's window changed size.
pub const RESIZE_VERB: &str = "peer_agent_resize";
/// `control`: the channel that attaches to one session.
pub const ATTACH_VERB: &str = "peer_agent_attach";
/// The reply that says the attach channel is now the session's terminal.
pub const ATTACHED: &str = "peer_agent_attached";

/// The peer control verbs this module answers, each only as far as the grant goes.
pub const VERBS: [&str; 3] = [LIST_VERB, PEEK_VERB, RESIZE_VERB];

/// How many attachments one computer may hold here at once.
pub const MAX_ATTACHED: usize = 4;
/// How many attaches one computer may start in [`ATTACH_WINDOW`]: each one
/// posts a notification here.
pub const MAX_ATTACHES: usize = 6;
const ATTACH_WINDOW: Duration = Duration::from_secs(60);
/// How many list, peek and resize requests one computer may make in
/// [`ASK_WINDOW`]: each one opens a connection to `rime-agentd`.
pub const MAX_ASKS: usize = 30;
const ASK_WINDOW: Duration = Duration::from_secs(10);

/// The most a `peer_agents` answer carries, well under one frame.
pub const MAX_LIST_BYTES: usize = 60_000;
/// The most sessions it carries.
pub const MAX_SESSIONS: usize = 200;
/// The largest tail a peek asks for: `rime-agentd`'s own cap.
pub const MAX_PEEK: usize = rime_agent_core::session::PEEK_MAX;
/// A terminal size a peer may give: anything outside is refused.
pub const MIN_COLS: u16 = 10;
pub const MAX_COLS: u16 = 1000;
pub const MIN_ROWS: u16 = 3;
pub const MAX_ROWS: u16 = 500;

/// The channel number the controlling side opens an attach on.
const CH: u32 = 1;
/// How long the controlling side waits for any frame of a held attach: the
/// other computer pings far more often than this.
const HELD_READ: Duration = Duration::from_secs(120);

/// What one computer's requests here have used, and what was heard about
/// this computer's grant on others. Memory only.
#[derive(Default)]
pub struct Agents {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Live attachments per computer.
    attached: HashMap<String, usize>,
    /// When each recent attach started, per computer.
    attaches: HashMap<String, VecDeque<Instant>>,
    /// When each recent list/peek/resize came, per computer.
    asks: HashMap<String, VecDeque<Instant>>,
    /// What each other computer last said it lets this one do, and when.
    hints: HashMap<String, (AgentGrant, u64)>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Whether one more event fits in `window`, recording it if so.
fn take(q: &mut VecDeque<Instant>, max: usize, window: Duration, now: Instant) -> bool {
    while q.front().is_some_and(|t| now.duration_since(*t) >= window) {
        q.pop_front();
    }
    if q.len() >= max {
        return false;
    }
    q.push_back(now);
    true
}

/// One live attachment's place, held for as long as the channel is open.
pub struct Slot {
    agents: Arc<State>,
    computer: String,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut g = lock(&self.agents.agents.inner);
        if let Some(n) = g.attached.get_mut(&self.computer) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                g.attached.remove(&self.computer);
            }
        }
    }
}

impl Agents {
    /// A list, peek or resize from `computer`, if its rate allows.
    fn ask(&self, computer: &str, now: Instant) -> bool {
        take(lock(&self.inner).asks.entry(computer.to_string()).or_default(), MAX_ASKS, ASK_WINDOW, now)
    }

    /// How many attachments `computer` holds here now.
    #[cfg(test)]
    pub fn attached(&self, computer: &str) -> usize {
        lock(&self.inner).attached.get(computer).copied().unwrap_or(0)
    }

    /// What `computer` last said it lets this one do, and when.
    pub fn hint(&self, computer: &str) -> Option<(AgentGrant, u64)> {
        lock(&self.inner).hints.get(computer).copied()
    }

    /// Everything kept about `computer`: it was removed.
    pub fn forget(&self, computer: &str) {
        let mut g = lock(&self.inner);
        g.attaches.remove(computer);
        g.asks.remove(computer);
        g.hints.remove(computer);
    }
}

/// A place for one more attachment from `computer`, or why not.
fn slot(state: &Arc<State>, computer: &str, now: Instant) -> Result<Slot, &'static str> {
    let mut g = lock(&state.agents.inner);
    if g.attached.get(computer).copied().unwrap_or(0) >= MAX_ATTACHED {
        return Err("busy");
    }
    if !take(g.attaches.entry(computer.to_string()).or_default(), MAX_ATTACHES, ATTACH_WINDOW, now) {
        return Err("busy");
    }
    *g.attached.entry(computer.to_string()).or_default() += 1;
    Ok(Slot {
        agents: Arc::clone(state),
        computer: computer.to_string(),
    })
}

/// What a verb needs: `None` for a verb this module does not answer.
pub fn needs(verb: &str) -> Option<AgentGrant> {
    match verb {
        LIST_VERB | PEEK_VERB => Some(AgentGrant::View),
        RESIZE_VERB | ATTACH_VERB => Some(AgentGrant::Control),
        _ => None,
    }
}

/// The grant a person here gave computer `device_id`, as it counts now: off
/// for an unknown id, a removed computer, or one introduced and not allowed.
pub fn grant_of(state: &State, device_id: &str) -> Option<AgentGrant> {
    let store = state.devices().ok()?;
    store.computers.get(device_id).and_then(Device::agents_grant)
}

/// Whether computer `device_id` may send `verb` now. Read from the store on
/// every request, so a grant lowered a moment ago already counts.
pub fn allowed(state: &State, device_id: &str, verb: &str) -> bool {
    match needs(verb) {
        Some(need) => grant_of(state, device_id).is_some_and(|g| g >= need),
        None => false,
    }
}

/// What a peer's `remote_hello` adds for computer `device_id`: the feature
/// and the hint, only when it has a grant.
pub fn hello_fields(state: &State, device_id: &str) -> Option<&'static str> {
    grant_of(state, device_id).map(AgentGrant::word)
}

fn refused(kind: &str, message: &str) -> Vec<u8> {
    json!({"reply": "error", "kind": kind, "message": message}).to_string().into_bytes()
}

/// `rime-agentd`'s refusal as a peer sees it: the kind when it is one of
/// `rime-agentd`'s own words, never the text.
fn agentd_refusal(v: &Value) -> Vec<u8> {
    const KINDS: [&str; 4] = ["no_such_session", "session_exited", "permission_denied", "bad_request"];
    let kind = v["kind"].as_str().filter(|k| KINDS.contains(k)).unwrap_or("refused");
    refused(kind, "the agent runtime refused it")
}

fn unavailable() -> Vec<u8> {
    refused("unavailable", "the agent runtime did not answer")
}

/// The actor `rime-agentd` records for a request from computer `device_id`.
pub fn actor(device_id: &str) -> String {
    format!("computer:{device_id}")
}

fn round_trip(agentd: &Path, device_id: &str, line: &Value) -> Option<Value> {
    let mut a = crate::proxy::Agentd::open(agentd, &actor(device_id)).ok()?;
    let reply = a.round_trip(line.to_string().as_bytes()).ok()?;
    serde_json::from_slice(&reply).ok()
}

/// A session id a peer gave: a whole number that fits `u32`.
fn session_id(v: &Value) -> Option<u32> {
    v["id"].as_u64().and_then(|n| u32::try_from(n).ok())
}

/// A terminal size a peer gave, inside the fixed range.
fn size(v: &Value) -> Option<(u16, u16)> {
    let cols = v["cols"].as_u64().and_then(|n| u16::try_from(n).ok())?;
    let rows = v["rows"].as_u64().and_then(|n| u16::try_from(n).ok())?;
    ((MIN_COLS..=MAX_COLS).contains(&cols) && (MIN_ROWS..=MAX_ROWS).contains(&rows)).then_some((cols, rows))
}

/// One control line from peer computer `device_id` naming one of [`VERBS`].
/// The grant first, then the rate, then the fields; only then `rime-agentd`.
pub fn answer(state: &State, agentd: &Path, device_id: &str, verb: &str, line: &[u8]) -> Vec<u8> {
    if !allowed(state, device_id, verb) {
        return crate::computers::refusal(verb);
    }
    if !state.agents.ask(device_id, Instant::now()) {
        return refused("busy", "too many requests; ask again in a few seconds");
    }
    let v: Value = serde_json::from_slice(line).unwrap_or(Value::Null);
    match verb {
        LIST_VERB => list(agentd, device_id),
        PEEK_VERB => peek(agentd, device_id, &v),
        RESIZE_VERB => resize(agentd, device_id, &v),
        _ => crate::computers::refusal(verb),
    }
}

/// `peer_agents`: the sessions, as `rime-agentd` lists them to a phone,
/// trimmed to fit one frame.
fn list(agentd: &Path, device_id: &str) -> Vec<u8> {
    let Some(v) = round_trip(agentd, device_id, &json!({"cmd": "list"})) else {
        return unavailable();
    };
    if v["reply"] != "sessions" {
        return agentd_refusal(&v);
    }
    let sessions: Vec<SessionInfo> = v["sessions"]
        .as_array()
        .map(|a| a.iter().filter_map(|s| serde_json::from_value(s.clone()).ok()).collect())
        .unwrap_or_default();
    let (kept, truncated) = fit(sessions, MAX_SESSIONS, MAX_LIST_BYTES);
    json!({"reply": LIST_VERB, "sessions": kept, "truncated": truncated}).to_string().into_bytes()
}

/// As many sessions as fit `max` and `bytes` once serialised, and whether any were left out.
pub fn fit(mut sessions: Vec<SessionInfo>, max: usize, bytes: usize) -> (Vec<SessionInfo>, bool) {
    let mut truncated = sessions.len() > max;
    sessions.truncate(max);
    while !sessions.is_empty() && serde_json::to_vec(&sessions).map_or(usize::MAX, |b| b.len()) > bytes {
        sessions.pop();
        truncated = true;
    }
    (sessions, truncated)
}

/// `peer_agent_peek {id, bytes?}`: the tail of one session (§1.6).
fn peek(agentd: &Path, device_id: &str, v: &Value) -> Vec<u8> {
    let Some(id) = session_id(v) else {
        return refused("bad_request", "id is a session number");
    };
    let bytes = match &v["bytes"] {
        Value::Null => MAX_PEEK,
        b => match b.as_u64() {
            Some(n) if n >= 1 && n <= MAX_PEEK as u64 => n as usize,
            _ => return refused("bad_request", "bytes is 1 to 8192"),
        },
    };
    let Some(r) = round_trip(agentd, device_id, &json!({"cmd": "peek", "id": id, "bytes": bytes})) else {
        return unavailable();
    };
    if r["reply"] != "peek" {
        return agentd_refusal(&r);
    }
    match checked_peek(&r) {
        Some((data, cols, rows, state)) => json!({
            "reply": PEEK_VERB, "id": id, "data": data, "cols": cols, "rows": rows, "state": state,
        })
        .to_string()
        .into_bytes(),
        None => unavailable(),
    }
}

/// A peek answer's fields, checked: base64 of at most [`MAX_PEEK`] bytes, a
/// size, and a state word. Used on both sides.
pub fn checked_peek(r: &Value) -> Option<(String, u16, u16, String)> {
    let data = r["data"].as_str()?;
    let longest = MAX_PEEK.div_ceil(3) * 4;
    if data.len() > longest || !data.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=') {
        return None;
    }
    let cols = r["cols"].as_u64().and_then(|n| u16::try_from(n).ok())?;
    let rows = r["rows"].as_u64().and_then(|n| u16::try_from(n).ok())?;
    let state = r["state"].as_str().unwrap_or("");
    let state = if !state.is_empty() && state.len() <= 32 && state.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
        state.to_string()
    } else {
        "unknown".to_string()
    };
    Some((data.to_string(), cols, rows, state))
}

/// `peer_agent_resize {id, cols, rows}`: the other computer's window for
/// this session changed. `rime-agentd` files it as a remote size, so it
/// never shrinks the terminal here.
fn resize(agentd: &Path, device_id: &str, v: &Value) -> Vec<u8> {
    let (Some(id), Some((cols, rows))) = (session_id(v), size(v)) else {
        return refused("bad_request", "id is a session number; cols and rows a terminal size");
    };
    let Some(r) = round_trip(agentd, device_id, &json!({"cmd": "resize", "id": id, "cols": cols, "rows": rows})) else {
        return unavailable();
    };
    if r["reply"] != "ok" {
        return agentd_refusal(&r);
    }
    json!({"reply": RESIZE_VERB, "id": id}).to_string().into_bytes()
}

/// What a session is called on a notification: its name, its title, or its
/// agent and project, then its number; plain text.
fn label_of(info: &Value, id: u32) -> String {
    let pick = |k: &str| info[k].as_str().map(|s| crate::mirror::clean(s, crate::mirror::TITLE_CAP, 0, false)).filter(|s| !s.is_empty());
    if let Some(l) = pick("name").or_else(|| pick("title")) {
        return l;
    }
    match (pick("agent"), pick("project_name")) {
        (Some(a), Some(p)) => format!("{a} · {p}"),
        (Some(a), None) => format!("{a} session {id}"),
        _ => format!("session {id}"),
    }
}

/// An agent attach opened for a peer: the stream onto `rime-agentd`, the
/// reply line for the peer, bytes that came after `rime-agentd`'s reply, and
/// the slot that counts it.
pub type Opened = (UnixStream, Vec<u8>, Vec<u8>, Slot);

/// The `peer_agent_attach {id, cols, rows}` channel from computer
/// `device_id`: the grant, the fields, a slot, the session; then the attach,
/// which `rime-agentd` takes over. `Err` is the refusal line.
pub fn open_attach(
    state: &Arc<State>,
    agentd: &Path,
    device_id: &str,
    request: &[u8],
) -> Result<Opened, Vec<u8>> {
    if !allowed(state, device_id, ATTACH_VERB) {
        return Err(crate::computers::refusal(ATTACH_VERB));
    }
    let v: Value = serde_json::from_slice(request).unwrap_or(Value::Null);
    let (Some(id), Some((cols, rows))) = (session_id(&v), size(&v)) else {
        return Err(refused("bad_request", "id is a session number; cols and rows a terminal size"));
    };
    let slot = slot(state, device_id, Instant::now())
        .map_err(|_| refused("busy", "this computer already holds as many attachments here as it may"))?;
    let info = round_trip(agentd, device_id, &json!({"cmd": "info", "id": id})).ok_or_else(unavailable)?;
    if info["reply"] != "session" {
        return Err(agentd_refusal(&info));
    }
    let label = label_of(&info, id);
    let opened = crate::proxy::Agentd::open(agentd, &actor(device_id))
        .and_then(|a| a.take_over(json!({"cmd": "attach", "id": id, "cols": cols, "rows": rows}).to_string().as_bytes()));
    let (stream, reply, buffered) = opened.map_err(|_| unavailable())?;
    let r: Value = serde_json::from_slice(&reply).unwrap_or(Value::Null);
    if r["reply"] != "attached" {
        return Err(agentd_refusal(&r));
    }
    notify_attached(state, device_id, &label);
    let ours = json!({"reply": ATTACHED, "id": id, "label": label}).to_string().into_bytes();
    Ok((stream, ours, buffered, slot))
}

/// "<Computer> attached to <session>", once per attach.
fn notify_attached(state: &State, device_id: &str, label: &str) {
    let name = state
        .devices()
        .ok()
        .and_then(|s| s.computers.get(device_id).map(|c| c.name.clone()))
        .unwrap_or_default();
    let name = crate::mirror::clean(&name, crate::mirror::APP_CAP, 0, false);
    let name = if name.is_empty() { "A paired computer".to_string() } else { name };
    crate::mirror::mirror().submit(crate::mirror::Job::Notice(crate::mirror::Shown {
        app_name: "Rime Remote".into(),
        summary: format!("{name} attached to {label}"),
        body: "It can see and type into this session. Lower its access with `rime remote computers grant`.".into(),
        reply: None,
        actions: vec![],
        sticky: false,
        expires_ms: None,
    }));
}

// ---------------------------------------------------------------------------
// The controlling side.
// ---------------------------------------------------------------------------

/// Record what computer `id` said, in a `remote_hello` answer, it lets this
/// one do. Anything but the two words is off.
pub fn heard_hello(state: &State, id: &str, hello: &Value) {
    let offers = hello["features"].as_array().is_some_and(|f| f.iter().any(|x| x.as_str() == Some(FEATURE)));
    let grant = match hello["agents"].as_str() {
        Some("view") if offers => Some(AgentGrant::View),
        Some("control") if offers => Some(AgentGrant::Control),
        _ => None,
    };
    let mut g = lock(&state.agents.inner);
    match grant {
        Some(grant) => {
            g.hints.insert(id.to_string(), (grant, rime_remote_core::now_ms()));
        }
        None => {
            g.hints.remove(id);
        }
    }
}

/// A paired computer by id or name, or why not.
fn active(state: &State, computer: &str) -> Result<Device, String> {
    let store = state.devices().map_err(|e| e.to_string())?;
    let c = store.find_computer(computer).map_err(|e| e.to_string())?;
    if !c.is_active() {
        return Err(format!("'{}' was removed from this computer", crate::dialer::printable(&c.name)));
    }
    Ok(c.clone())
}

/// Dial `c`, hear its hello (and so its hint), and ask `line`.
fn ask_once(state: &State, c: &Device, line: &Value) -> Result<(crate::dialer::Link, Value), String> {
    let mut link = crate::dialer::open(state, c, crate::dialer::Via::Any)?;
    let hello = link.call(br#"{"cmd":"remote_hello"}"#)?;
    heard_hello(state, &c.id, &hello);
    let answer = link.call(line.to_string().as_bytes())?;
    Ok((link, answer))
}

/// The other computer's refusal, for the person here.
fn refusal_text(name: &str, verb: &str, v: &Value) -> String {
    let name = crate::dialer::printable(name);
    match v["kind"].as_str() {
        Some(crate::computers::REFUSAL) => match needs(verb) {
            Some(AgentGrant::Control) => format!(
                "'{name}' does not let this computer control its agent sessions. A person there can allow it \
                 with `rime remote computers grant <this computer> agents control`"
            ),
            _ => format!(
                "'{name}' does not let this computer see its agent sessions. A person there can allow it with \
                 `rime remote computers grant <this computer> agents view`"
            ),
        },
        Some("busy") => format!("'{name}' is busy with this computer's requests; try again in a moment"),
        Some("no_such_session") => format!("'{name}' has no such session"),
        Some("session_exited") => format!("that session on '{name}' has already exited"),
        Some("permission_denied") => format!("'{name}' does not let a remote viewer into that session"),
        Some("unavailable") => format!("the agent runtime on '{name}' did not answer"),
        Some("bad_request") => format!("'{name}' did not accept the request"),
        _ => format!("'{name}' refused it"),
    }
}

/// `computer_grant {computer, agents}`: set what another computer may do
/// with this computer's agent sessions. Lowering it ends that computer's
/// live connections here, so a live attachment ends now.
pub fn grant(state: &State, computer: &str, word: &str) -> Reply {
    let grant = match AgentGrant::parse(word) {
        Ok(g) => g,
        Err(message) => return Reply::Error { message },
    };
    let mut store = match state.devices() {
        Ok(s) => s,
        Err(e) => return Reply::Error { message: e.to_string() },
    };
    let (c, lowered) = match store.set_computer_agents(computer, grant) {
        Ok(r) => r,
        Err(e) => return Reply::Error { message: e.to_string() },
    };
    if let Err(e) = state.save_devices(&store) {
        return Reply::Error {
            message: format!("the computer was not written: {e}"),
        };
    }
    drop(store);
    if lowered {
        state.drop_connections_for(&c.id);
    }
    Reply::ComputerGranted {
        agents: AgentGrant::word_of(c.agents_grant()).to_string(),
        dropped: lowered,
        id: c.id,
        name: c.name,
    }
}

/// `computer_agents {computer}`: the other computer's sessions.
pub fn agents_of(state: &State, computer: &str) -> Reply {
    let c = match active(state, computer) {
        Ok(c) => c,
        Err(message) => return Reply::Error { message },
    };
    let answer = match ask_once(state, &c, &json!({"cmd": LIST_VERB})) {
        Ok((_, a)) => a,
        Err(message) => return Reply::Error { message },
    };
    if answer["reply"] != LIST_VERB {
        return Reply::Error {
            message: refusal_text(&c.name, LIST_VERB, &answer),
        };
    }
    let (sessions, unreadable) = received(answer["sessions"].as_array().cloned().unwrap_or_default());
    Reply::ComputerAgents {
        id: c.id,
        name: c.name,
        sessions,
        truncated: answer["truncated"].as_bool().unwrap_or(false),
        unreadable,
    }
}

/// The sessions another computer listed, as this computer keeps them: typed,
/// so a hostile answer brings no field and no value the type does not have;
/// every text field made [`plain`]; at most [`MAX_SESSIONS`]. A session that
/// does not parse is left out and counted.
fn received(given: Vec<Value>) -> (Vec<SessionInfo>, usize) {
    let mut sessions: Vec<SessionInfo> = Vec::new();
    let mut unreadable = 0usize;
    for s in given.into_iter().take(MAX_SESSIONS) {
        match serde_json::from_value::<SessionInfo>(s) {
            Ok(mut s) => {
                plain_session(&mut s);
                sessions.push(s)
            }
            Err(_) => unreadable += 1,
        }
    }
    (sessions, unreadable)
}

/// The longest text field kept from another computer's session list.
pub const FIELD_CAP: usize = 1024;

/// One text field another computer sent, made safe to print or show: no
/// control characters (so no escape sequences), no bidirectional overrides,
/// at most [`FIELD_CAP`] characters. Nothing else changes, so a path stays
/// the path it was.
pub fn plain(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control() && !crate::mirror::is_bidi_control(*c) && *c != '\u{2028}' && *c != '\u{2029}')
        .take(FIELD_CAP)
        .collect()
}

/// Every text field of a session another computer listed, through [`plain`]:
/// the CLI prints them and the Shell shows them.
fn plain_session(s: &mut SessionInfo) {
    let one = |v: &mut String| *v = plain(v);
    let maybe = |v: &mut Option<String>| {
        if let Some(t) = v {
            *t = plain(t);
        }
    };
    one(&mut s.agent);
    one(&mut s.program);
    one(&mut s.cwd);
    s.args.iter_mut().for_each(one);
    s.args.truncate(64);
    maybe(&mut s.project);
    maybe(&mut s.project_name);
    maybe(&mut s.name);
    maybe(&mut s.title);
    maybe(&mut s.worktree);
    maybe(&mut s.detail);
    maybe(&mut s.actor);
    maybe(&mut s.checkpoint);
    maybe(&mut s.capsule);
    maybe(&mut s.native_observed);
    if let Some(list) = &mut s.allowlist {
        list.iter_mut().for_each(one);
        list.truncate(64);
    }
    if let Some(t) = &mut s.telemetry {
        maybe(&mut t.model);
    }
    s.children.truncate(64);
    for c in &mut s.children {
        one(&mut c.id);
        one(&mut c.label);
    }
}

/// `computer_agent_peek {computer, id, bytes?}`: the tail of one session there.
pub fn peek_of(state: &State, computer: &str, id: u32, bytes: Option<u32>) -> Reply {
    let c = match active(state, computer) {
        Ok(c) => c,
        Err(message) => return Reply::Error { message },
    };
    let mut line = json!({"cmd": PEEK_VERB, "id": id});
    if let Some(b) = bytes {
        line["bytes"] = json!(b);
    }
    let answer = match ask_once(state, &c, &line) {
        Ok((_, a)) => a,
        Err(message) => return Reply::Error { message },
    };
    if answer["reply"] != PEEK_VERB {
        return Reply::Error {
            message: refusal_text(&c.name, PEEK_VERB, &answer),
        };
    }
    match checked_peek(&answer) {
        Some((data, cols, rows, state_word)) => Reply::ComputerAgentPeek {
            id: c.id,
            name: c.name,
            session: id,
            data,
            cols,
            rows,
            state: state_word,
        },
        None => Reply::Error {
            message: format!("'{}' sent a tail that was refused", crate::dialer::printable(&c.name)),
        },
    }
}

/// `computer_agent_resize {computer, id, cols, rows}`: this terminal's
/// window changed while attached there.
pub fn resize_of(state: &State, computer: &str, id: u32, cols: u16, rows: u16) -> Reply {
    let c = match active(state, computer) {
        Ok(c) => c,
        Err(message) => return Reply::Error { message },
    };
    match ask_once(state, &c, &json!({"cmd": RESIZE_VERB, "id": id, "cols": cols, "rows": rows})) {
        Ok((_, a)) if a["reply"] == RESIZE_VERB => Reply::Ok,
        Ok((_, a)) => Reply::Error {
            message: refusal_text(&c.name, RESIZE_VERB, &a),
        },
        Err(message) => Reply::Error { message },
    }
}

/// `computer_attach {computer, id, cols, rows}` on control connection
/// `client`, which this holds: `computer_attached` (or the refusal), then
/// the session's terminal both ways until either end closes. `early` is what
/// the client sent after the request line, which belongs to the terminal.
pub fn attach_out(
    state: &State,
    client: &UnixStream,
    computer: &str,
    id: u32,
    cols: u16,
    rows: u16,
    early: Vec<u8>,
) -> std::io::Result<()> {
    let mut w = client.try_clone()?;
    let c = match active(state, computer) {
        Ok(c) => c,
        Err(why) => return crate::control::reply(&mut w, &Reply::Error { message: why }),
    };
    let mut link = match crate::dialer::open(state, &c, crate::dialer::Via::Any) {
        Ok(l) => l,
        Err(why) => return crate::control::reply(&mut w, &Reply::Error { message: why }),
    };
    match link.call(br#"{"cmd":"remote_hello"}"#) {
        Ok(hello) => heard_hello(state, &c.id, &hello),
        Err(why) => return crate::control::reply(&mut w, &Reply::Error { message: why }),
    }
    let request = json!({"cmd": ATTACH_VERB, "id": id, "cols": cols, "rows": rows}).to_string();
    let answer = match link.open_channel(CH, request.as_bytes()) {
        Ok(a) => a,
        Err(why) => return crate::control::reply(&mut w, &Reply::Error { message: why }),
    };
    if answer["reply"] != ATTACHED {
        let message = refusal_text(&c.name, ATTACH_VERB, &answer);
        return crate::control::reply(&mut w, &Reply::Error { message });
    }
    let label = crate::mirror::clean(answer["label"].as_str().unwrap_or(""), crate::mirror::TITLE_CAP, 0, false);
    crate::control::reply(
        &mut w,
        &Reply::ComputerAttached {
            id: c.id.clone(),
            name: c.name.clone(),
            session: id,
            label,
        },
    )?;
    let (mut reader, writer, shut) = link.into_duplex(HELD_READ).map_err(std::io::Error::other)?;
    // The other computer → the client, OSC 52 taken out.
    let mut out = client.try_clone()?;
    let hang_up = client.try_clone()?;
    let pump = std::thread::spawn(move || {
        let mut filter = Osc52::default();
        loop {
            match reader.next() {
                Ok(Frame::Data { channel: CH, bytes }) => {
                    let shown = filter.feed(&bytes);
                    if !shown.is_empty() && out.write_all(&shown).is_err() {
                        break;
                    }
                }
                Ok(Frame::Close { channel: CH, .. }) | Err(_) => break,
                Ok(_) => continue,
            }
        }
        let _ = hang_up.shutdown(std::net::Shutdown::Both);
    });
    // The client → the other computer.
    let send = |bytes: &[u8]| Frame::data_frames(CH, bytes).into_iter().all(|f| writer.send(f).is_ok());
    let mut ok = early.is_empty() || send(&early);
    let mut input = client.try_clone()?;
    let mut buf = [0u8; 16 * 1024];
    while ok {
        match input.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => ok = send(&buf[..n]),
        }
    }
    let _ = writer.send(Frame::Close {
        channel: CH,
        reason: String::new(),
    });
    let _ = shut.shutdown(std::net::Shutdown::Both);
    let _ = pump.join();
    Ok(())
}

/// Takes OSC 52 (set or query the clipboard) out of a terminal byte stream,
/// across chunk boundaries, and passes everything else through unchanged.
#[derive(Default)]
pub struct Osc52 {
    state: Osc,
    /// Bytes held back while it is not yet known whether they start OSC 52.
    held: Vec<u8>,
    /// How much of a dropped sequence has gone by.
    dropped: usize,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Osc {
    #[default]
    Text,
    Esc,
    Number,
    Drop,
    DropEsc,
}

/// An OSC 52 that never ends stops being dropped after this much.
const MAX_DROPPED: usize = 1 << 20;

impl Osc52 {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        for &b in bytes {
            match self.state {
                Osc::Text => {
                    if b == 0x1b {
                        self.held.push(b);
                        self.state = Osc::Esc;
                    } else {
                        out.push(b);
                    }
                }
                Osc::Esc => {
                    if b == b']' {
                        self.held.push(b);
                        self.state = Osc::Number;
                    } else if b == 0x1b {
                        out.append(&mut self.held);
                        self.held.push(b);
                    } else {
                        out.append(&mut self.held);
                        out.push(b);
                        self.state = Osc::Text;
                    }
                }
                Osc::Number => {
                    if b.is_ascii_digit() && self.held.len() < 10 {
                        self.held.push(b);
                    } else if b == b';' && &self.held[2..] == b"52" {
                        self.held.clear();
                        self.dropped = 0;
                        self.state = Osc::Drop;
                    } else {
                        out.append(&mut self.held);
                        if b == 0x1b {
                            self.held.push(b);
                            self.state = Osc::Esc;
                        } else {
                            out.push(b);
                            self.state = Osc::Text;
                        }
                    }
                }
                Osc::Drop | Osc::DropEsc => {
                    self.dropped += 1;
                    let was_esc = self.state == Osc::DropEsc;
                    self.state = match b {
                        0x07 | 0x18 | 0x1a | 0x9c => Osc::Text,
                        b'\\' if was_esc => Osc::Text,
                        0x1b => Osc::DropEsc,
                        _ => Osc::Drop,
                    };
                    if self.state != Osc::Text && self.dropped > MAX_DROPPED {
                        self.state = Osc::Text;
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rime_remote_core::device::Introducer;

    fn state_with(tag: &str) -> (Arc<State>, Device) {
        let (state, _dir) = crate::serve::tests::a_state(tag);
        let own = state.identity.public_bytes();
        let c = state
            .devices()
            .expect("store")
            .pair_computer(&[7u8; 32], &own, "desk", vec![], None, 1)
            .expect("paired");
        (state, c)
    }

    fn set(state: &State, id: &str, g: Option<AgentGrant>) {
        state.devices().expect("store").set_computer_agents(id, g).expect("set");
    }

    #[test]
    fn each_verb_needs_its_grant_and_nothing_else_is_answered() {
        let (state, c) = state_with("agents-gates");
        let all = [LIST_VERB, PEEK_VERB, RESIZE_VERB, ATTACH_VERB];
        // Off: nothing.
        for v in all {
            assert!(!allowed(&state, &c.id, v), "{v} with no grant");
        }
        set(&state, &c.id, Some(AgentGrant::View));
        for (v, ok) in [(LIST_VERB, true), (PEEK_VERB, true), (RESIZE_VERB, false), (ATTACH_VERB, false)] {
            assert_eq!(allowed(&state, &c.id, v), ok, "{v} with view");
        }
        set(&state, &c.id, Some(AgentGrant::Control));
        for v in all {
            assert!(allowed(&state, &c.id, v), "{v} with control");
        }
        // Verbs this module does not answer, whatever the grant.
        for v in ["decide", "requests", "input", "run", "kill", "attach", "list", "peek", "nex", "", "peer_agent"] {
            assert!(!allowed(&state, &c.id, v), "{v}");
        }
        // Another id, a phone's id, a name: never.
        assert!(!allowed(&state, "nobody", LIST_VERB));
        assert!(!allowed(&state, "desk", LIST_VERB), "a name resolved to a grant");
        // Removed: nothing, at once.
        state.devices().expect("store").revoke_computer(&c.id, 5).expect("removed");
        for v in all {
            assert!(!allowed(&state, &c.id, v), "{v} after removal");
        }
    }

    #[test]
    fn an_introduced_computer_has_no_grant_until_allowed_and_granted() {
        let (state, _) = state_with("agents-intro");
        let own = state.identity.public_bytes();
        let new = match state
            .devices()
            .expect("store")
            .introduce_computer(&[9u8; 32], &own, "new", vec![], None, Introducer { id: "p".into(), name: "pixel".into() }, 2)
            .expect("introduced")
        {
            rime_remote_core::device::Introduced::Added(d) => *d,
            _ => panic!("not added"),
        };
        assert!(state.devices().expect("store").set_computer_agents(&new.id, Some(AgentGrant::Control)).is_err());
        // Even written into the record by hand, it does not count.
        state.devices().expect("store").computers.get_mut(&new.id).expect("rec").share.agents = Some(AgentGrant::Control);
        assert!(!allowed(&state, &new.id, LIST_VERB));
        assert_eq!(hello_fields(&state, &new.id), None);
        state.devices().expect("store").allow_computer(&new.id, 3).expect("allowed");
        assert!(allowed(&state, &new.id, ATTACH_VERB), "allowed + granted");
    }

    #[test]
    fn a_peer_without_the_grant_is_refused_before_rime_agentd_is_reached() {
        let (state, c) = state_with("agents-before");
        // No socket at that path: reaching rime-agentd would say "unavailable".
        let agentd = Path::new("/nonexistent/agentd.sock");
        for verb in VERBS {
            let line = json!({"cmd": verb, "id": 1, "cols": 80, "rows": 24}).to_string();
            let v: Value = serde_json::from_slice(&answer(&state, agentd, &c.id, verb, line.as_bytes())).expect("json");
            assert_eq!(v["kind"], crate::computers::REFUSAL, "{verb}: {v}");
        }
        set(&state, &c.id, Some(AgentGrant::View));
        let v: Value = serde_json::from_slice(&answer(&state, agentd, &c.id, RESIZE_VERB, br#"{"cmd":"peer_agent_resize","id":1,"cols":80,"rows":24}"#)).expect("json");
        assert_eq!(v["kind"], crate::computers::REFUSAL, "resize with view: {v}");
        // The positive control: with view, a list reaches for rime-agentd.
        let v: Value = serde_json::from_slice(&answer(&state, agentd, &c.id, LIST_VERB, br#"{"cmd":"peer_agents"}"#)).expect("json");
        assert_eq!(v["kind"], "unavailable", "{v}");
        let refused = open_attach(&state, agentd, &c.id, br#"{"cmd":"peer_agent_attach","id":1,"cols":80,"rows":24}"#)
            .err()
            .expect("refused");
        let v: Value = serde_json::from_slice(&refused).expect("json");
        assert_eq!(v["kind"], crate::computers::REFUSAL, "attach with view: {v}");
    }

    #[test]
    fn hostile_ids_sizes_and_counts_are_refused_before_rime_agentd() {
        let (state, c) = state_with("agents-hostile");
        set(&state, &c.id, Some(AgentGrant::Control));
        let agentd = Path::new("/nonexistent/agentd.sock");
        for line in [
            json!({"cmd": PEEK_VERB}),
            json!({"cmd": PEEK_VERB, "id": -1}),
            json!({"cmd": PEEK_VERB, "id": 4_294_967_296u64}),
            json!({"cmd": PEEK_VERB, "id": "1"}),
            json!({"cmd": PEEK_VERB, "id": 1.5}),
            json!({"cmd": PEEK_VERB, "id": 1, "bytes": 0}),
            json!({"cmd": PEEK_VERB, "id": 1, "bytes": 8193}),
            json!({"cmd": PEEK_VERB, "id": 1, "bytes": "all"}),
            json!({"cmd": RESIZE_VERB, "id": 1, "cols": 0, "rows": 24}),
            json!({"cmd": RESIZE_VERB, "id": 1, "cols": 80, "rows": 65_536}),
            json!({"cmd": RESIZE_VERB, "id": 1, "cols": 1001, "rows": 24}),
            json!({"cmd": RESIZE_VERB, "id": 1, "cols": 80}),
        ] {
            let verb = line["cmd"].as_str().expect("verb");
            let v: Value = serde_json::from_slice(&answer(&state, agentd, &c.id, verb, line.to_string().as_bytes())).expect("json");
            assert_eq!(v["kind"], "bad_request", "{line}: {v}");
        }
        for bad in [
            &br#"{"cmd":"peer_agent_attach","id":1,"cols":5,"rows":24}"#[..],
            br#"{"cmd":"peer_agent_attach","id":1,"cols":80,"rows":2}"#,
            br#"{"cmd":"peer_agent_attach","id":-3,"cols":80,"rows":24}"#,
            br#"{"cmd":"peer_agent_attach"}"#,
            b"\xff\xfe garbage",
        ] {
            let v: Value = serde_json::from_slice(&open_attach(&state, agentd, &c.id, bad).err().expect("refused")).expect("json");
            assert_eq!(v["kind"], "bad_request", "{v}");
        }
        assert_eq!(state.agents.attached(&c.id), 0, "a refused attach kept a slot");
    }

    #[test]
    fn requests_and_attaches_are_capped_per_computer() {
        let (state, c) = state_with("agents-caps");
        let now = Instant::now();
        let mut slots = Vec::new();
        for _ in 0..MAX_ATTACHED {
            slots.push(slot(&state, &c.id, now).expect("slot"));
        }
        assert!(slot(&state, &c.id, now).is_err(), "a fifth attachment");
        assert!(slot(&state, "other", now).is_ok(), "another computer's cap was shared");
        drop(slots.pop());
        assert_eq!(state.agents.attached(&c.id), MAX_ATTACHED - 1);
        // Starts in the window are capped even when attachments end: four
        // so far, two more, then none until the window passes.
        drop(slots);
        assert_eq!(state.agents.attached(&c.id), 0);
        for _ in MAX_ATTACHED..MAX_ATTACHES {
            drop(slot(&state, &c.id, now).expect("one more"));
        }
        assert!(slot(&state, &c.id, now).is_err(), "the start rate was not kept");
        assert!(slot(&state, &c.id, now + ATTACH_WINDOW).is_ok());
        for _ in 0..MAX_ASKS {
            assert!(state.agents.ask(&c.id, now));
        }
        assert!(!state.agents.ask(&c.id, now));
        assert!(state.agents.ask(&c.id, now + ASK_WINDOW));
    }

    #[test]
    fn the_hint_is_a_closed_set_kept_apart_from_the_grant() {
        let (state, c) = state_with("agents-hint");
        let hello = |agents: Value, features: Value| json!({"reply": "remote", "features": features, "agents": agents});
        heard_hello(&state, &c.id, &hello(json!("control"), json!([FEATURE])));
        assert_eq!(state.agents.hint(&c.id).map(|h| h.0), Some(AgentGrant::Control));
        // What another computer says never changes what this one granted it.
        assert_eq!(grant_of(&state, &c.id), None);
        for (agents, features) in [
            (json!("root"), json!([FEATURE])),
            (json!("control"), json!([])),
            (json!(["control"]), json!([FEATURE])),
            (Value::Null, json!([FEATURE])),
        ] {
            heard_hello(&state, &c.id, &hello(json!("view"), json!([FEATURE])));
            heard_hello(&state, &c.id, &hello(agents.clone(), features));
            assert_eq!(state.agents.hint(&c.id), None, "{agents}");
        }
        heard_hello(&state, &c.id, &hello(json!("view"), json!([FEATURE])));
        state.agents.forget(&c.id);
        assert_eq!(state.agents.hint(&c.id), None);
    }

    #[test]
    fn a_list_is_trimmed_to_one_frame() {
        use rime_agent_core::protocol::AgentState;
        let one = SessionInfo {
            id: 1,
            agent: "generic".into(),
            program: "/bin/cat".into(),
            args: vec!["x".repeat(2000)],
            cwd: "/tmp".into(),
            project: None,
            project_name: None,
            name: None,
            title: None,
            worktree: None,
            state: AgentState::Working,
            detail: None,
            paused: false,
            policy: Default::default(),
            allowlist: None,
            request_origin: None,
            origin_source: None,
            actor: None,
            grant: None,
            grant_expires_ms: None,
            native_observed: None,
            telemetry: None,
            children: Vec::new(),
            pid: 1,
            started: 1,
            last_activity: 1,
            exit_code: None,
            exit_signal: None,
            attached: 0,
            checkpoint: None,
            cols: 80,
            rows: 24,
            injected: 0,
            capsule: None,
        };
        let (kept, truncated) = fit(vec![one.clone(); 100], MAX_SESSIONS, MAX_LIST_BYTES);
        assert!(truncated);
        assert!(serde_json::to_vec(&kept).expect("json").len() <= MAX_LIST_BYTES);
        assert!(!kept.is_empty());
        let (kept, truncated) = fit(vec![one.clone(); 3], MAX_SESSIONS, MAX_LIST_BYTES);
        assert_eq!((kept.len(), truncated), (3, false));

        // What another computer listed loses every escape and override
        // before the CLI prints it or the Shell shows it.
        let mut hostile = one;
        hostile.name = Some("\u{1b}]52;c;aGk=\u{7}evil\u{202e}txt".into());
        hostile.cwd = "/tmp/\u{1b}[2J\u{9b}31m".into();
        hostile.args = vec!["a\u{0}b".into(); 500];
        hostile.title = Some("t".repeat(5000));
        let mut given = vec![serde_json::to_value(&hostile).expect("json"); MAX_SESSIONS + 5];
        given[0] = json!({"id": "not a number"});
        let (kept, unreadable) = received(given);
        assert_eq!((kept.len(), unreadable), (MAX_SESSIONS - 1, 1));
        let hostile = kept.into_iter().next().expect("one");
        assert_eq!(hostile.name.as_deref(), Some("]52;c;aGk=eviltxt"));
        assert_eq!(hostile.cwd, "/tmp/[2J31m");
        assert_eq!((hostile.args.len(), hostile.args[0].as_str()), (64, "ab"));
        assert_eq!(hostile.title.map(|t| t.chars().count()), Some(FIELD_CAP));
    }

    #[test]
    fn a_peek_answer_is_checked() {
        let good = json!({"data": "aGVsbG8=", "cols": 80, "rows": 24, "state": "running"});
        assert_eq!(checked_peek(&good).map(|p| p.3), Some("running".into()));
        let odd_state = json!({"data": "", "cols": 80, "rows": 24, "state": "\u{1b}[31mx"});
        assert_eq!(checked_peek(&odd_state).map(|p| p.3), Some("unknown".into()));
        for bad in [
            json!({"data": "a\u{1b}b", "cols": 80, "rows": 24}),
            json!({"data": "A".repeat(10_925), "cols": 80, "rows": 24}),
            json!({"data": "aGk=", "cols": 70_000, "rows": 24}),
            json!({"cols": 80, "rows": 24}),
        ] {
            assert!(checked_peek(&bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn osc_52_is_taken_out_across_chunks_and_everything_else_passes() {
        let mut f = Osc52::default();
        let text = b"a\x1b[31mred\x1b[0m \x1b]0;title\x07 \x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\ \x1b]520;x\x07".to_vec();
        assert_eq!(f.feed(&text), text, "a sequence that is not OSC 52 was changed");
        let set = b"x\x1b]52;c;aGVsbG8=\x07y";
        assert_eq!(f.feed(set), b"xy");
        let query = b"x\x1b]52;c;?\x1b\\y";
        assert_eq!(f.feed(query), b"xy");
        // Byte by byte, as the network may cut it.
        let mut out = Vec::new();
        for b in b"1\x1b]52;p;Zm9v\x1b\\2\x1b\x1b]52;c;?\x072" {
            out.extend(f.feed(&[*b]));
        }
        assert_eq!(out, b"12\x1b2");
        assert!(!out.windows(3).any(|w| w == b"]52"), "{out:?}");
        // CAN ends it; an unterminated one stops being dropped at the cap.
        assert_eq!(f.feed(b"\x1b]52;c;abc\x18ok"), b"ok");
        let mut f = Osc52::default();
        let mut long = b"\x1b]52;c;".to_vec();
        long.extend(std::iter::repeat_n(b'A', MAX_DROPPED + 10));
        let out = f.feed(&long);
        assert!(out.len() < 20, "{}", out.len());
        assert_eq!(f.feed(b"after"), b"after");
    }
}
