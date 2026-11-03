//! The live session table.
//!
//! One [`Session`] per PTY, each with its own reader thread. A `Mutex` per
//! session rather than one lock over the table, so a busy agent producing
//! megabytes of output never blocks `rime agent list`.
//!
//! Locking rule, and the reason this stays deadlock-free: the registry lock is
//! held long enough to clone an `Arc` out of the map, and — once per session
//! start — long enough to reserve an id against the store, which is a directory
//! listing and one exclusive create. Session locks are taken *after* the
//! registry lock is released, never the other way round, and no code path holds
//! two session locks at once.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::RawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rime_agent_core::paths;
use rime_agent_core::protocol::{AgentState, SessionInfo};
use rime_agent_core::destination::Allowlist;
use rime_agent_core::graph;
use rime_agent_core::hook::{HookEvent, ToolTransition};
use rime_agent_core::sandbox::SandboxSpec;
use rime_agent_core::session::{
    self as logic, OutputScanner, Scrollback, SizeClaims, Viewer, SCROLLBACK_BYTES,
};
use rime_agent_core::term::WinSize;

use crate::pty;

/// Cap on a session's on-disk transcript.
///
/// An agent that loops printing output must not fill the user's home. When the
/// cap is passed the log stops growing and the fact is recorded once; the
/// in-memory scrollback keeps working, so `attach` is unaffected.
pub const LOG_LIMIT_BYTES: u64 = 32 * 1024 * 1024;

/// How long a write to an attached client may block before that client is
/// dropped.
///
/// Without this the session stalls indefinitely behind a terminal that has
/// stopped reading — a suspended client, a stalled SSH connection, a terminal
/// paused with ctrl-S. A Unix socket buffers about 176 KiB (measured on this
/// kernel) and then blocks, so a busy agent reaches that in well under a
/// second and the *agent* stops running, not just the display.
///
/// Two seconds is far longer than any healthy client needs and short enough
/// that a wedged one cannot hold up the work. The session keeps running; only
/// the unresponsive viewer is disconnected, and it can reattach.
const ATTACH_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// What a session is confined by, kept for the §6.2 policy point.
#[derive(Debug, Clone)]
pub struct Confinement {
    pub spec: SandboxSpec,
    pub allowlist: Allowlist,
}

/// One client mirroring a session's output, and what kind of client it is.
///
/// The kind is decided once, from the connection's origin, when it attaches
/// (§1.7). Carried per attacher rather than as a count beside the list, because
/// an attacher can leave two ways — its own detach, and [`Session::broadcast`]
/// dropping a connection whose write failed — and a counter kept in step by
/// hand in both places is a counter that drifts.
#[derive(Debug)]
pub struct Attacher {
    pub stream: UnixStream,
    pub viewer: Viewer,
}

/// One live session.
#[derive(Debug)]
pub struct Session {
    pub info: SessionInfo,
    /// Master side of the PTY.
    pub master: RawFd,
    pub pid: libc::pid_t,
    pub pgid: libc::pid_t,
    pub scrollback: Scrollback,
    pub scanner: OutputScanner,
    /// Connections currently mirroring this session's output.
    pub attachers: Vec<Attacher>,
    /// The sizes this session's terminal has been asked to be, by the desktop
    /// and by a phone (§1.7). See [`SizeClaims`] for the rule; the methods
    /// below are where it is applied to the PTY.
    pub sizes: SizeClaims,
    /// When a published event last said a tool started, and nothing has said
    /// it finished. `None` for every session no hook speaks for, which is what
    /// leaves the idle rule deciding on its own exactly as it did before.
    tool_started: Option<u64>,
    /// The confinement this session was actually started with, and the
    /// allowlist that was in force when it was.
    ///
    /// Kept so the §6.2 policy point can be answered from the evidence rather
    /// than from a spec rebuilt at decision time out of whatever the config
    /// says now — a session started an hour ago against a smaller allowlist
    /// must be judged against the one it is running under. `None` for a session
    /// the runtime adopted rather than built.
    pub confinement: Option<Box<Confinement>>,
    log: Option<File>,
    log_bytes: u64,
    log_capped: bool,
    /// True once the reader thread has been asked to stop.
    pub closing: bool,
    /// Unix milliseconds at which the session was first asked to stop.
    ///
    /// `SIGTERM` is a request, and a process may decline it. That is fine for
    /// `rime agent kill`, where the user can ask again — and not fine for
    /// §3.4's automatic expiry, where the session declining to die is a
    /// session keeping root past its window. This is what lets a caller
    /// escalate after a bounded grace: see [`force_kill`] and the daemon's
    /// expiry thread.
    pub closing_since_ms: Option<u64>,
    /// True once `SIGKILL` has been sent, so it is sent once.
    pub killed: bool,
}

impl Session {
    /// Append output: scrollback, transcript, and every attached client.
    pub fn absorb(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        self.scrollback.push(data);
        self.write_log(data);
        self.broadcast(data);
        self.info.last_activity = now_secs();
    }

    fn write_log(&mut self, data: &[u8]) {
        if self.log_capped {
            return;
        }
        let Some(log) = self.log.as_mut() else {
            return;
        };
        if self.log_bytes + data.len() as u64 > LOG_LIMIT_BYTES {
            let _ = log.write_all(
                b"\r\n[rime-agentd: transcript truncated, session log limit reached]\r\n",
            );
            let _ = log.flush();
            self.log_capped = true;
            return;
        }
        if log.write_all(data).is_ok() {
            self.log_bytes += data.len() as u64;
        }
    }

    /// Mirror bytes to attached clients, dropping any that have gone away.
    fn broadcast(&mut self, data: &[u8]) {
        if self.attachers.is_empty() {
            return;
        }
        let before = self.attachers.len();
        self.attachers.retain_mut(|a| a.stream.write_all(data).is_ok());
        if self.attachers.len() != before {
            self.info.attached = self.attachers.len() as u32;
            // A phone whose connection died mid-write has stopped looking as
            // surely as one that detached, and the desktop gets its size back
            // the same way. Without this, a phone that walked out of Wi-Fi
            // range would leave the terminal at its size until the next
            // attach.
            self.settle_size();
        }
    }

    /// Register a local client and hand it the scrollback to repaint with.
    pub fn attach(&mut self, stream: UnixStream, replay: usize) -> Result<()> {
        self.attach_as(stream, replay, Viewer::Local)
    }

    /// Register a client of either kind and hand it the scrollback.
    pub fn attach_as(&mut self, mut stream: UnixStream, replay: usize, viewer: Viewer) -> Result<()> {
        // A client that stops reading must not be able to stall the session.
        // See ATTACH_WRITE_TIMEOUT.
        stream.set_write_timeout(Some(ATTACH_WRITE_TIMEOUT)).ok();
        if replay > 0 && !self.scrollback.is_empty() {
            let tail = self.scrollback.tail(replay);
            stream
                .write_all(&tail)
                .context("sending scrollback to the attaching client")?;
        }
        self.attachers.push(Attacher { stream, viewer });
        self.info.attached = self.attachers.len() as u32;
        Ok(())
    }

    /// How many phones are looking at this session right now.
    pub fn remote_viewers(&self) -> usize {
        self.attachers
            .iter()
            .filter(|a| a.viewer == Viewer::Remote)
            .count()
    }

    /// The size the PTY is at, as far as this daemon has set it.
    pub fn current_size(&self) -> WinSize {
        WinSize {
            cols: self.info.cols,
            rows: self.info.rows,
        }
    }

    /// Resize the PTY and record the size, when there is a PTY to resize.
    ///
    /// A master of `-1` is a session the reaper has already closed, and there
    /// is nothing to resize: not an error, and not worth one.
    fn apply_size(&mut self, size: WinSize) -> Result<()> {
        if self.master < 0 {
            return Ok(());
        }
        pty::resize(self.master, size)?;
        self.info.cols = size.cols;
        self.info.rows = size.rows;
        Ok(())
    }

    /// A client is attaching at `size`: remember it for its kind, and take it.
    pub fn attach_size(&mut self, viewer: Viewer, size: WinSize) -> Result<()> {
        let size = self.sizes.attached(viewer, size);
        self.apply_size(size)
    }

    /// A client sent a `resize`. `Ok(false)` is a phone's resize that arrived
    /// with no phone attached, remembered and deliberately not applied — see
    /// [`SizeClaims::resized`].
    pub fn resize_from(&mut self, viewer: Viewer, size: WinSize) -> Result<bool> {
        let remote = self.remote_viewers();
        match self.sizes.resized(viewer, size, remote) {
            Some(size) => self.apply_size(size).map(|()| true),
            None => Ok(false),
        }
    }

    /// Give the terminal back to the desktop if no phone is looking any more.
    ///
    /// Best effort: the ioctl on a live master does not fail, and a session
    /// whose master is gone has nothing to give back.
    pub fn settle_size(&mut self) {
        let current = self.current_size();
        if let Some(size) = self.sizes.settle(self.remote_viewers(), current) {
            let _ = self.apply_size(size);
        }
    }

    /// A viewer typed `bytes` into its attach: the latest typist's size.
    pub fn typed(&mut self, viewer: Viewer, bytes: &[u8]) {
        let current = self.current_size();
        if let Some(size) = self.sizes.typed(viewer, bytes, self.remote_viewers(), current) {
            let _ = self.apply_size(size);
        }
    }

    /// Record a state transition, refusing to leave a terminal state.
    pub fn set_state(&mut self, state: AgentState, detail: Option<String>) {
        if self.info.state.is_terminal() && !state.is_terminal() {
            return;
        }
        self.info.state = state;
        if detail.is_some() {
            self.info.detail = detail;
        }
        self.info.last_activity = now_secs();
    }

    /// Record what a published lifecycle event says about the session graph
    /// (§P1-020).
    ///
    /// Three of the events matter and the rest are none of this method's
    /// business. `subagent_start` and `subagent_stop` open and close a node;
    /// `stop` — the end of a turn — closes whatever is still open, because
    /// nothing Claude delegated outlives the reply it was delegated for and a
    /// `SubagentStop` is a hook that can fail to arrive. See
    /// [`rime_agent_core::graph`] for why the record holds no state field to
    /// go stale in the first place.
    pub fn apply_graph_event(
        &mut self,
        event: HookEvent,
        agent_id: Option<&str>,
        agent_type: Option<&str>,
    ) {
        let at = now_secs();
        match event {
            HookEvent::SubagentStart => {
                graph::subagent_started(&mut self.info.children, agent_id, agent_type, at)
            }
            HookEvent::SubagentStop => {
                graph::subagent_stopped(&mut self.info.children, agent_id, agent_type, at)
            }
            // Two sweeps, two words, because they are two different facts.
            // The turn ending is provisional — a subagent can outlive it, and
            // `subagent_stopped` will overwrite the entry if the real report
            // turns up. The session ending is not: the agent is on its way
            // out, and nothing it delegated survives the process.
            HookEvent::Stop => {
                graph::close_open(&mut self.info.children, graph::ChildEnd::ParentStop, at);
            }
            HookEvent::SessionEnd => {
                graph::close_open(&mut self.info.children, graph::ChildEnd::ParentExit, at);
            }
            _ => {}
        }
    }

    /// Record that the process ended.
    pub fn set_exited(&mut self, code: Option<i32>, signal: Option<i32>) {
        self.info.exit_code = code;
        self.info.exit_signal = signal;
        self.info.state = rime_agent_core::session::exit_state(code, signal);
        self.info.last_activity = now_secs();
        // Nothing this session started is still running, whatever the last
        // hook said. This is the sweep that keeps the graph from claiming a
        // subagent is working under an agent that is gone.
        graph::close_open(
            &mut self.info.children,
            graph::ChildEnd::ParentExit,
            self.info.last_activity,
        );
        self.attachers.clear();
        self.info.attached = 0;
        if let Some(log) = self.log.as_mut() {
            let _ = log.flush();
        }
    }

    /// Seconds since the last output or published event.
    pub fn idle_secs(&self) -> u64 {
        now_secs().saturating_sub(self.info.last_activity)
    }

    /// Record what a published lifecycle event says about a tool call.
    ///
    /// Set by a `pre_tool_use`, cleared by the `post_tool_use` that answers it
    /// and by anything that ends the turn. See
    /// [`rime_agent_core::hook::HookEvent::tool_transition`] for why a `stop`
    /// clears it too: it is the recovery path for a `post_tool_use` that never
    /// arrived.
    pub fn apply_tool_transition(&mut self, t: ToolTransition) {
        match t {
            ToolTransition::Started => self.tool_started = Some(now_secs()),
            ToolTransition::Finished => self.tool_started = None,
            ToolTransition::Unchanged => {}
        }
    }

    /// How long ago a published event said a tool started, if one did.
    ///
    /// This is the argument [`rime_agent_core::session::next_state`] takes,
    /// and `None` — an unintegrated agent, or a hook that never fired — is
    /// what makes the idle rule the fallback rather than a second opinion.
    pub fn tool_in_flight(&self) -> logic::ToolInFlight {
        self.tool_started
            .map(|at| now_secs().saturating_sub(at))
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A session handle shared between the control threads and its reader thread.
pub type Handle = Arc<Mutex<Session>>;

/// The table of live sessions.
#[derive(Debug)]
pub struct Registry {
    /// Root of the on-disk session store — `sessions/` and `logs/` hang off
    /// this. A field rather than a call to [`paths::state_dir`] at every use,
    /// so a test can drive a real store without writing into the user's.
    store: PathBuf,
    /// Lowest id this process will hand out next. Only a floor: the store
    /// decides what is actually free. Keeping it stops an id being reused
    /// within one run after `rime agent rm` deleted the record at it.
    next_id: u32,
    sessions: HashMap<u32, Handle>,
}

impl Default for Registry {
    fn default() -> Registry {
        Registry::new()
    }
}

impl Registry {
    pub fn new() -> Registry {
        Registry::with_store(paths::state_dir())
    }

    /// A registry over an explicit store root.
    pub fn with_store(store: PathBuf) -> Registry {
        Registry {
            store,
            next_id: 1,
            sessions: HashMap::new(),
        }
    }

    /// Take the next session id, reserving it on disk before returning it.
    ///
    /// Ids have to be unique against what is *on disk*, not against what this
    /// process happens to remember. Sessions outlive the daemon — their records
    /// and transcripts are the whole point of `$XDG_STATE_HOME` — so a daemon
    /// that starts counting from 1 again writes session 1's record and PTY log
    /// straight over the previous session 1's. Restart the daemon, run one
    /// agent, and yesterday's transcript is gone.
    ///
    /// The starting point is the highest id any record *or* transcript still
    /// uses, plus one — deliberately not the number of records. `rime agent
    /// prune` and `rime agent rm` leave gaps, and counting records would step
    /// back into the range below a gap and destroy exactly the history the user
    /// chose to keep. Transcripts count as well as records, because a `.log`
    /// whose `.json` has gone is still a session's output.
    ///
    /// Nothing here reads a record's *contents*: the id is in the filename. A
    /// truncated or malformed record therefore cannot make allocation reuse an
    /// id, so the fail-open/fail-closed question does not arise for it — the
    /// file is still counted, and `reconcile_stale_records` deals with the
    /// unreadable content separately.
    ///
    /// Where it does arise is the directory scan and the reservation, and they
    /// are answered differently on purpose. The scan is a hint and fails open:
    /// if `sessions/` cannot be listed, allocation walks up from the floor and
    /// the exclusive create still decides. The reservation fails closed: an
    /// error that is not "that id is taken" means the store is unwritable, and
    /// starting a session whose record cannot exist is how history gets
    /// overwritten in the first place. The user gets the error instead.
    pub fn allocate(&mut self) -> Result<Reservation> {
        let reservation = reserve_id(&self.store, self.next_id)?;
        self.next_id = reservation.id.saturating_add(1);
        Ok(reservation)
    }

    /// Add a session, opening its transcript.
    pub fn insert(
        &mut self,
        info: SessionInfo,
        master: RawFd,
        pid: libc::pid_t,
        pgid: libc::pid_t,
    ) -> Handle {
        let id = info.id;
        let log = open_log(&self.store, id);
        let handle = Arc::new(Mutex::new(Session {
            info,
            master,
            pid,
            pgid,
            scrollback: Scrollback::new(SCROLLBACK_BYTES),
            scanner: OutputScanner::new(),
            attachers: Vec::new(),
            sizes: SizeClaims::default(),
            tool_started: None,
            confinement: None,
            log,
            log_bytes: 0,
            log_capped: false,
            closing: false,
            closing_since_ms: None,
            killed: false,
        }));
        self.sessions.insert(id, Arc::clone(&handle));
        handle
    }

    pub fn get(&self, id: u32) -> Option<Handle> {
        self.sessions.get(&id).map(Arc::clone)
    }

    /// Every session, ordered by id.
    pub fn list(&self) -> Vec<Handle> {
        let mut ids: Vec<&u32> = self.sessions.keys().collect();
        ids.sort();
        ids.into_iter()
            .filter_map(|id| self.sessions.get(id).map(Arc::clone))
            .collect()
    }

    /// Drop a session from the table.
    pub fn remove(&mut self, id: u32) -> Option<Handle> {
        self.sessions.remove(&id)
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

/// A session id that is reserved on disk but not yet owned by a session.
///
/// The reservation *is* the record file, created empty and exclusively. That
/// makes taking an id atomic against the filesystem rather than against one
/// process's counter, which matters because two daemons sharing a store is not
/// hypothetical: they are kept apart by the control socket, and the socket
/// lives in `$XDG_RUNTIME_DIR` while the store lives in `$XDG_STATE_HOME`.
/// Point one daemon at a different runtime directory — which is exactly how the
/// runtime is tested — and both write to the same `sessions/`.
///
/// Dropping a reservation without [`Reservation::commit`] gives the id back, so
/// a run that failed between allocation and spawn leaves no stub behind.
#[derive(Debug)]
pub struct Reservation {
    id: u32,
    store: PathBuf,
    committed: bool,
}

impl Reservation {
    pub fn id(&self) -> u32 {
        self.id
    }

    /// The session exists and owns its record now; stop guarding the id.
    pub fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.committed {
            forget_record_in(&self.store, self.id);
        }
    }
}

/// Reserve the lowest free session id at or above `floor`.
fn reserve_id(store: &Path, floor: u32) -> Result<Reservation> {
    let dir = paths::sessions_dir_in(store);
    paths::ensure_private_dir(&dir)
        .with_context(|| format!("preparing the session store {}", dir.display()))?;

    let mut id = floor.max(highest_used_id(store).saturating_add(1)).max(1);
    loop {
        // A transcript with no record beside it still belongs to a session the
        // user can read back, so an id carrying one is not free either. Checked
        // here as well as in the scan so it holds while walking upwards.
        if !paths::session_log_in(store, id).exists() {
            let path = paths::session_record_in(store, id);
            match OpenOptions::new().create_new(true).write(true).open(&path) {
                Ok(_) => {
                    // Same reasoning as the transcript: a record describes the
                    // user's work and nobody else needs to read it.
                    use std::os::unix::fs::PermissionsExt;
                    let _ =
                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
                    return Ok(Reservation {
                        id,
                        store: store.to_path_buf(),
                        committed: false,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("reserving session {id} at {}", path.display())
                    })
                }
            }
        }
        id = id
            .checked_add(1)
            .context("every session id is taken; run `rime agent prune`")?;
    }
}

/// The largest id the store still has a record or a transcript for.
///
/// Filenames only. Reading a record to learn its id would make a corrupt file
/// able to lower the mark, which is the one thing this must never do.
fn highest_used_id(store: &Path) -> u32 {
    let mut high = 0;
    for (dir, ext) in [
        (paths::sessions_dir_in(store), "json"),
        (paths::logs_dir_in(store), "log"),
    ] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some(ext) {
                continue;
            }
            let id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse::<u32>().ok());
            if let Some(id) = id {
                high = high.max(id);
            }
        }
    }
    high
}

fn open_log(store: &Path, id: u32) -> Option<File> {
    let path = paths::session_log_in(store, id);
    let dir = path.parent()?;
    paths::ensure_private_dir(dir).ok()?;
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)
        .ok()?;
    // A transcript is a record of the user's work; nobody else on the machine
    // needs to read it.
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    Some(file)
}

/// Persist a session record so `rime agent list` can describe it after the
/// daemon has restarted.
pub fn write_record(info: &SessionInfo) {
    write_record_in(&paths::state_dir(), info)
}

fn write_record_in(store: &Path, info: &SessionInfo) {
    let path = paths::session_record_in(store, info.id);
    let Some(dir) = path.parent() else { return };
    if paths::ensure_private_dir(dir).is_err() {
        return;
    }
    let Ok(text) = serde_json::to_string_pretty(info) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Delete a session's record and transcript.
pub fn forget_record(id: u32) {
    forget_record_in(&paths::state_dir(), id)
}

fn forget_record_in(store: &Path, id: u32) {
    let _ = std::fs::remove_file(paths::session_record_in(store, id));
    let _ = std::fs::remove_file(paths::session_log_in(store, id));
}

/// Read the tail of a session's transcript from disk.
pub fn read_log(id: u32, bytes: usize) -> Result<String> {
    use std::io::{Read, Seek, SeekFrom};

    let path = paths::session_log(id);
    let mut file = File::open(&path).with_context(|| format!("no transcript for session {id}"))?;
    let len = file.metadata()?.len();
    let want = bytes as u64;
    if len > want {
        file.seek(SeekFrom::Start(len - want))?;
    }
    let mut buf = Vec::with_capacity(want.min(len) as usize);
    file.read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Mark records left behind by a previous daemon as exited.
///
/// Sessions do not survive the daemon — every PTY it owned closed with it — so
/// a record still claiming `working` at startup is stale, and leaving it would
/// show the user a session they can neither attach to nor kill.
pub fn reconcile_stale_records() {
    reconcile_stale_records_in(&paths::state_dir())
}

fn reconcile_stale_records_in(store: &Path) {
    let dir = paths::sessions_dir_in(store);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        // An empty record is a reservation, not a corrupt session: another
        // daemon may be between taking that id and writing the session out.
        // Deleting it would free the id back to the daemon starting here, and
        // then both would write a record and truncate a transcript at it —
        // the exact collision the exclusive create exists to stop. A retired
        // id costs nothing; a shared one costs the user's transcript.
        if std::fs::metadata(&path).is_ok_and(|m| m.len() == 0) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(mut info) = serde_json::from_str::<SessionInfo>(&text) else {
            // Unparseable record: remove it rather than leave something no
            // version of the CLI can read.
            let _ = std::fs::remove_file(&path);
            continue;
        };
        if info.state.is_terminal() {
            continue;
        }
        info.state = AgentState::Exited;
        info.attached = 0;
        if info.exit_code.is_none() && info.exit_signal.is_none() {
            info.exit_code = Some(-1);
        }
        // A record left behind by a daemon that died holds whatever the graph
        // looked like at the last write, which for a session that was working
        // is a set of open subagents. The parent is demonstrably gone, so they
        // are closed here for the same reason `set_exited` closes them: the
        // one thing the graph must never do is report a dead agent as alive.
        graph::close_open(&mut info.children, graph::ChildEnd::ParentExit, now_secs());
        write_record_in(store, &info);
    }
}

/// Every persisted record, for listing sessions the current daemon does not own.
pub fn historical_records() -> Vec<SessionInfo> {
    let dir = paths::sessions_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(info) = serde_json::from_str::<SessionInfo>(&text) {
                out.push(info);
            }
        }
    }
    out.sort_by_key(|i| i.id);
    out
}

/// Ask a session's process group to stop, and close its terminal.
///
/// `SIGHUP` then `SIGTERM`: a request, which a process may decline. Records
/// when it was first asked, so a caller that cannot accept a decline can
/// escalate — see [`force_kill`].
pub fn terminate(session: &mut Session) {
    if session.info.is_live() {
        let _ = pty::signal_group(session.pgid, libc::SIGHUP);
        let _ = pty::signal_group(session.pgid, libc::SIGTERM);
        if session.closing_since_ms.is_none() {
            session.closing_since_ms = Some(rime_agent_core::request::now_ms());
        }
    }
    session.closing = true;
}

/// `SIGKILL` a session's process group, once.
///
/// The escalation [`terminate`] leaves room for. Returns whether the signal
/// was actually sent, so the caller records the escalation exactly once and
/// only when there was something to escalate against.
///
/// This exists for §3.4. A break-glass session that outlived its window has
/// `no_new_privs` cleared and cannot have it put back, so "the grant expired"
/// is only true if the process is gone — and a process that ignores `SIGTERM`
/// would otherwise keep `sudo` for as long as it liked while the audit trail
/// said its window was over.
pub fn force_kill(session: &mut Session) -> bool {
    if session.killed || !session.info.is_live() {
        return false;
    }
    session.killed = true;
    pty::signal_group(session.pgid, libc::SIGKILL).is_ok()
}

/// How long a session gets to exit on its own before [`force_kill`].
///
/// Long enough for an agent to flush a transcript and drop a lock; short
/// enough that a break-glass window is not meaningfully extended by declining
/// to die.
pub const TERMINATE_GRACE_MS: u64 = 10_000;

/// The grace has to leave time to flush a transcript and drop a lock, and it
/// has to be a small enough fraction of the shortest useful break-glass window
/// that declining to die does not meaningfully extend it. Both are facts about
/// constants, so they are checked when the crate compiles.
const _: () = assert!(TERMINATE_GRACE_MS >= 1_000);
const _: () = assert!(TERMINATE_GRACE_MS * 60 < rime_agent_core::grant::MAX_BREAK_GLASS_MS);

#[cfg(test)]
mod tests {
    use super::*;

    /// A process that declines SIGTERM, which is the case the escalation
    /// exists for.
    ///
    /// `sh` with SIGTERM and SIGHUP trapped, spawned on a real PTY through the
    /// same `pty::spawn` a session uses — so the process group, the signal
    /// delivery and the reaping are the shipped ones and not a mock.
    fn a_session_that_refuses_to_die() -> Option<(Session, pty::Spawned)> {
        // `ready` is written once the traps are installed, so the test asks it
        // to stop only after it can decline. Without that the signals race
        // `sh` reaching its own first command, and the fixture dies to SIGHUP
        // while appearing to prove the opposite.
        let script = "trap '' TERM HUP INT QUIT; echo ready; while true; do sleep 0.05; done";
        let argv = vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
        let spawned = pty::spawn(
            &argv,
            std::path::Path::new("/tmp"),
            &[],
            false,
            true,
            rime_agent_core::term::WinSize { cols: 80, rows: 24 },
        )
        .ok()?;
        let mut s = session(1);
        s.info.pid = spawned.pid;
        s.pid = spawned.pid;
        s.pgid = spawned.pgid;
        s.master = spawned.master;
        // Wait for `ready` on the PTY rather than sleeping a guessed amount.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut seen = Vec::new();
        while std::time::Instant::now() < deadline && !seen.windows(5).any(|w| w == b"ready") {
            let mut buf = [0u8; 256];
            match pty::read_nonblocking(spawned.master, &mut buf) {
                Ok(Some(n)) if n > 0 => seen.extend_from_slice(&buf[..n]),
                _ => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        if !seen.windows(5).any(|w| w == b"ready") {
            return None;
        }
        Some((s, spawned))
    }

    #[test]
    fn a_session_that_ignores_sigterm_is_killed_rather_than_asked_again() {
        // §3.4's automatic expiry, at the only place it can actually be
        // enforced. A break-glass session has `no_new_privs` cleared and
        // nothing can put it back, so "the window is over" is true only when
        // the process is gone — and SIGTERM is a request a process may
        // decline. Without the escalation the expiry would be a polite note
        // while the session kept root.
        let Some((mut s, spawned)) = a_session_that_refuses_to_die() else {
            eprintln!("SKIP: no PTY available in this environment");
            return;
        };

        // Asked politely, and it declines. Recorded so a caller can escalate.
        terminate(&mut s);
        assert!(s.closing);
        let asked = s.closing_since_ms.expect("the ask was timed");
        assert!(asked > 0);
        // Still there a moment later, which is the whole premise.
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(
            pty::try_wait(spawned.pid),
            pty::Wait::Running,
            "the fixture died to SIGTERM, so it is not testing the escalation"
        );

        // Escalated, once.
        assert!(force_kill(&mut s), "the kill was not sent");
        assert!(!force_kill(&mut s), "the kill was sent twice");

        // And it is gone. SIGKILL cannot be declined; this asserts the signal
        // reached the right process GROUP, which is the part that could be
        // wrong.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut outcome = pty::Wait::Running;
        while std::time::Instant::now() < deadline {
            outcome = pty::try_wait(spawned.pid);
            if outcome != pty::Wait::Running {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            outcome,
            pty::Wait::Signalled(libc::SIGKILL),
            "the session survived its own expiry"
        );
        // Asked again after it is gone changes nothing.
        s.info.exit_signal = Some(libc::SIGKILL);
        assert!(!force_kill(&mut s));
    }


    /// A store of this test's own.
    ///
    /// Every registry test here writes real files. Before the store became a
    /// field these ran against `$XDG_STATE_HOME`, so `cargo test` truncated the
    /// transcripts of the developer's own sessions 1, 2 and 3. The name carries
    /// a counter as well as the pid because the suite runs in parallel.
    struct Store(PathBuf);

    impl Store {
        fn new() -> Store {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "rime-agentd-registry-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::remove_dir_all(&dir).ok();
            Store(dir)
        }

        fn path(&self) -> PathBuf {
            self.0.clone()
        }

        fn registry(&self) -> Registry {
            Registry::with_store(self.path())
        }
    }

    impl Drop for Store {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn info(id: u32) -> SessionInfo {
        SessionInfo {
            id,
            agent: "generic".into(),
            program: "sh".into(),
            args: vec![],
            cwd: "/tmp".into(),
            project: None,
            project_name: None,
            name: None,
            title: None,
            worktree: None,
            state: AgentState::Starting,
            detail: None,
            paused: false,
            policy: rime_agent_core::AgentPolicy::default(),
            allowlist: None,
            request_origin: Some(rime_agent_core::policy::RequestOrigin::LocalTerminal),
            origin_source: Some(rime_agent_core::origin::OriginSource::Observed),
            actor: None,
            grant: None,
            grant_expires_ms: None,
            native_observed: None,
            telemetry: None,
        children: Vec::new(),
            pid: 0,
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
        }
    }

    fn session(id: u32) -> Session {
        Session {
            info: info(id),
            master: -1,
            pid: 0,
            pgid: 0,
            scrollback: Scrollback::new(1024),
            scanner: OutputScanner::new(),
            attachers: Vec::new(),
            sizes: SizeClaims::default(),
            tool_started: None,
            confinement: None,
            log: None,
            log_bytes: 0,
            log_capped: false,
            closing: false,
            closing_since_ms: None,
            killed: false,
        }
    }

    #[test]
    fn ids_are_allocated_in_order_and_never_reused() {
        let store = Store::new();
        let mut r = store.registry();
        let a = r.allocate().unwrap();
        let b = r.allocate().unwrap();
        assert_eq!((a.id(), b.id()), (1, 2));
        r.insert(info(a.id()), -1, 0, 0);
        r.remove(a.id());
        // Removing a session must not let the next one take its id: a stale
        // `rime agent attach 1` would otherwise reach a different session.
        assert_eq!(r.allocate().unwrap().id(), 3);
    }

    #[test]
    fn a_restart_neither_reuses_an_id_nor_overwrites_the_record_at_it() {
        // The defect this exists for: `Registry::new` started counting at 1
        // with no reference to the store, so the first session after a daemon
        // restart wrote its record and transcript over session 1's. A restart
        // is all it took to lose yesterday's work.
        let store = Store::new();

        let mut first = store.registry();
        let reserved = first.allocate().unwrap();
        let id = reserved.id();
        assert_eq!(id, 1);
        let mut original = info(id);
        original.program = "the first session".into();
        write_record_in(&store.path(), &original);
        paths::ensure_private_dir(&paths::logs_dir_in(&store.path())).unwrap();
        std::fs::write(paths::session_log_in(&store.path(), id), b"the first transcript").unwrap();
        reserved.commit();
        drop(first);

        // The daemon restarts. Nothing is in memory; the store is all there is.
        let mut second = store.registry();
        let next = second.allocate().unwrap();
        assert_ne!(next.id(), id, "a restart handed out an id that is already in use");
        let mut replacement = info(next.id());
        replacement.program = "the second session".into();
        write_record_in(&store.path(), &replacement);
        next.commit();

        let kept = std::fs::read_to_string(paths::session_record_in(&store.path(), id)).unwrap();
        let kept: SessionInfo = serde_json::from_str(&kept).unwrap();
        assert_eq!(kept.program, "the first session", "the earlier record was overwritten");
        assert_eq!(
            std::fs::read(paths::session_log_in(&store.path(), id)).unwrap(),
            b"the first transcript",
            "the earlier transcript was overwritten"
        );
    }

    #[test]
    fn a_gap_left_by_prune_is_not_filled_again() {
        // Counting records instead of taking the highest would allocate 3 here
        // and destroy session 3. Pruning is normal use, so gaps are normal.
        let store = Store::new();
        {
            let mut r = store.registry();
            for _ in 0..3 {
                r.allocate().unwrap().commit();
            }
        }
        forget_record_in(&store.path(), 2);

        let mut after = store.registry();
        assert_eq!(after.allocate().unwrap().id(), 4);
    }

    #[test]
    fn a_transcript_without_a_record_still_holds_its_id() {
        // A record can be lost — a failed write, a half-finished prune — while
        // the transcript survives. The transcript is the part the user reads
        // back, so it must not be truncated by the next session.
        let store = Store::new();
        let logs = paths::logs_dir_in(&store.path());
        paths::ensure_private_dir(&logs).unwrap();
        std::fs::write(logs.join("7.log"), b"output nobody has a record for").unwrap();

        let mut r = store.registry();
        assert_eq!(r.allocate().unwrap().id(), 8);
    }

    #[test]
    fn a_reservation_dropped_without_a_session_gives_the_id_back() {
        // A run that fails between allocation and spawn must not retire an id
        // or leave an empty record for `rime agent list` to trip over.
        let store = Store::new();
        let mut r = store.registry();
        let abandoned = r.allocate().unwrap().id();
        assert!(!paths::session_record_in(&store.path(), abandoned).exists());

        // Within the run the floor still moves on, so a stale id is never
        // reissued to a client that already saw it fail.
        assert_eq!(r.allocate().unwrap().id(), abandoned + 1);
    }

    #[test]
    fn a_reservation_survives_another_daemon_starting_up() {
        // Startup reconciliation deletes records it cannot parse, and a
        // reservation is an empty file that cannot be parsed. Deleting one
        // would hand the id straight back while the daemon that took it is
        // still starting its session, and both would then write over it.
        let store = Store::new();
        let mut holder = store.registry();
        let held = holder.allocate().unwrap();

        // Live record from a daemon that went away, plus a genuinely corrupt
        // one, so the reconciliation this guards is still doing its own job.
        let mut running = info(50);
        running.state = AgentState::Working;
        write_record_in(&store.path(), &running);
        let corrupt = paths::session_record_in(&store.path(), 51);
        std::fs::write(&corrupt, "{not json").unwrap();

        reconcile_stale_records_in(&store.path());

        assert!(
            paths::session_record_in(&store.path(), held.id()).exists(),
            "a reservation in flight was deleted"
        );
        assert!(!corrupt.exists(), "a corrupt record was left in place");
        let text =
            std::fs::read_to_string(paths::session_record_in(&store.path(), 50)).unwrap();
        let reconciled: SessionInfo = serde_json::from_str(&text).unwrap();
        assert_eq!(reconciled.state, AgentState::Exited);

        // And the id is still not free to the daemon that just reconciled.
        let mut other = store.registry();
        assert_ne!(other.allocate().unwrap().id(), held.id());
    }

    #[test]
    fn two_registries_sharing_a_store_never_hand_out_the_same_id() {
        // Two daemons over one store is not hypothetical: they are kept apart
        // by a socket in $XDG_RUNTIME_DIR while the store is in
        // $XDG_STATE_HOME, so pointing one at another runtime directory is
        // enough. An in-memory counter cannot see the other process at all.
        let store = Store::new();
        let per_thread = 25;
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let dir = store.path();
                std::thread::spawn(move || {
                    let mut r = Registry::with_store(dir);
                    (0..per_thread)
                        .map(|_| {
                            let res = r.allocate().unwrap();
                            let id = res.id();
                            res.commit();
                            id
                        })
                        .collect::<Vec<u32>>()
                })
            })
            .collect();

        let mut ids: Vec<u32> = threads
            .into_iter()
            .flat_map(|t| t.join().expect("thread panicked"))
            .collect();
        let handed_out = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), handed_out, "two registries took the same id");
    }

    #[test]
    fn sessions_are_listed_in_id_order() {
        let store = Store::new();
        let mut r = store.registry();
        for id in [3u32, 1, 2] {
            r.insert(info(id), -1, 0, 0);
        }
        let ids: Vec<u32> = r
            .list()
            .into_iter()
            .map(|h| h.lock().unwrap().info.id)
            .collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn absorbing_output_fills_the_scrollback() {
        let mut s = session(1);
        s.absorb(b"hello ");
        s.absorb(b"world");
        assert_eq!(s.scrollback.tail(64), b"hello world".to_vec());
        assert!(s.info.last_activity > 0);
    }

    #[test]
    fn a_terminal_state_is_not_reopened_by_a_late_event() {
        let mut s = session(1);
        s.set_exited(Some(0), None);
        assert_eq!(s.info.state, AgentState::Complete);
        s.set_state(AgentState::Working, None);
        assert_eq!(
            s.info.state,
            AgentState::Complete,
            "a late event reopened a finished session"
        );
    }

    #[test]
    fn a_terminal_state_may_be_replaced_by_another_terminal_state() {
        let mut s = session(1);
        s.set_state(AgentState::Complete, None);
        s.set_state(AgentState::Failed, None);
        assert_eq!(s.info.state, AgentState::Failed);
    }

    #[test]
    fn exiting_clears_attachments() {
        let mut s = session(1);
        let (a, _b) = UnixStream::pair().unwrap();
        s.attach(a, 0).unwrap();
        assert_eq!(s.info.attached, 1);
        s.set_exited(Some(0), None);
        assert_eq!(s.info.attached, 0);
        assert!(s.attachers.is_empty());
    }

    #[test]
    fn attaching_replays_the_scrollback() {
        use std::io::Read;
        let mut s = session(1);
        s.absorb(b"earlier output");
        let (client, mut peer) = UnixStream::pair().unwrap();
        s.attach(client, 1024).unwrap();

        let mut buf = [0u8; 64];
        let n = peer.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"earlier output");
    }

    #[test]
    fn attaching_with_zero_replay_sends_nothing_first() {
        use std::io::Read;
        let mut s = session(1);
        s.absorb(b"earlier output");
        let (client, mut peer) = UnixStream::pair().unwrap();
        s.attach(client, 0).unwrap();

        peer.set_read_timeout(Some(std::time::Duration::from_millis(50)))
            .unwrap();
        let mut buf = [0u8; 64];
        assert!(peer.read(&mut buf).is_err(), "replay was sent anyway");

        // ...but live output still arrives.
        s.absorb(b"live");
        let n = peer.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"live");
    }

    #[test]
    fn output_reaches_every_attached_client() {
        use std::io::Read;
        let mut s = session(1);
        let (c1, mut p1) = UnixStream::pair().unwrap();
        let (c2, mut p2) = UnixStream::pair().unwrap();
        s.attach(c1, 0).unwrap();
        s.attach(c2, 0).unwrap();
        assert_eq!(s.info.attached, 2);

        s.absorb(b"shared");
        let mut buf = [0u8; 32];
        let n1 = p1.read(&mut buf).unwrap();
        assert_eq!(&buf[..n1], b"shared");
        let n2 = p2.read(&mut buf).unwrap();
        assert_eq!(&buf[..n2], b"shared");
    }

    #[test]
    fn a_disconnected_client_is_dropped_without_killing_the_session() {
        let mut s = session(1);
        let (c1, p1) = UnixStream::pair().unwrap();
        let (c2, mut p2) = UnixStream::pair().unwrap();
        s.attach(c1, 0).unwrap();
        s.attach(c2, 0).unwrap();
        drop(p1);

        // Keep the live client drained, so the only thing under test is the
        // dead one. Writing without a reader would fill the socket buffer and
        // stall regardless of which client was at fault.
        let drain = std::thread::spawn(move || {
            use std::io::Read;
            let mut sink = [0u8; 8192];
            let mut total = 0usize;
            while let Ok(n) = p2.read(&mut sink) {
                if n == 0 {
                    break;
                }
                total += n;
            }
            total
        });

        for _ in 0..64 {
            s.absorb(&[b'x'; 4096]);
        }
        assert_eq!(s.info.attached, 1, "the dead client was not dropped");

        s.attachers.clear();
        assert!(drain.join().unwrap() > 0, "the live client received nothing");
    }

    #[test]
    fn a_client_that_stops_reading_is_dropped_instead_of_stalling_the_session() {
        // The failure this guards against is not cosmetic: without a write
        // timeout the session's reader thread blocks inside write_all once the
        // socket buffer fills (~176 KiB on this kernel), which stops the agent
        // itself, not just its display.
        let mut s = session(1);
        let (client, _peer) = UnixStream::pair().unwrap();
        s.attach(client, 0).unwrap();

        let started = std::time::Instant::now();
        // Comfortably past the socket buffer, with the peer never reading.
        for _ in 0..128 {
            s.absorb(&[b'x'; 8192]);
        }
        let elapsed = started.elapsed();

        assert_eq!(s.info.attached, 0, "the stalled client was not dropped");
        assert!(
            elapsed < std::time::Duration::from_secs(20),
            "the session was blocked for {elapsed:?} by an unresponsive client"
        );
    }

    #[test]
    fn idle_seconds_grow_from_the_last_activity() {
        let mut s = session(1);
        s.info.last_activity = now_secs() - 30;
        assert!(s.idle_secs() >= 30);
        s.absorb(b"x");
        assert!(s.idle_secs() < 2);
    }

    // ── §1.7, against a real terminal ───────────────────────────────────────
    //
    // The rule is `SizeClaims`', and tested there over every case. These are
    // the two places the daemon applies it that a pure test cannot reach: a
    // phone whose connection DIED (dropped by `broadcast`, never detached),
    // and the latest typist, both read back off the PTY with TIOCGWINSZ.

    #[test]
    fn a_phone_whose_connection_died_gives_the_desktop_its_size_back() {
        use rime_agent_core::term::window_size;
        let Some((master, slave)) = pty::bare_pair() else {
            eprintln!("SKIP: no /dev/ptmx on this machine");
            return;
        };
        let desk = WinSize { cols: 180, rows: 50 };
        let phone = WinSize { cols: 46, rows: 30 };
        let mut s = session(1);
        s.master = master;

        let (local, _local_peer) = UnixStream::pair().unwrap();
        let (remote, remote_peer) = UnixStream::pair().unwrap();
        s.attach_size(Viewer::Local, desk).unwrap();
        s.attach_as(local, 0, Viewer::Local).unwrap();
        s.attach_size(Viewer::Remote, phone).unwrap();
        s.attach_as(remote, 0, Viewer::Remote).unwrap();
        assert_eq!(window_size(master), phone);
        assert_eq!(s.remote_viewers(), 1);

        // The phone walks out of Wi-Fi range: nothing detaches, the next
        // write to it fails, and `broadcast` drops it.
        // Shut down, not only dropped: a test in this binary that forks (pty's
        // held spawns park a child before execve) holds a copy of every fd,
        // so a drop alone may leave the socket open and the write succeeds
        // (hosted run 41549507599, "the dead connection was not dropped").
        remote_peer.shutdown(std::net::Shutdown::Both).unwrap();
        drop(remote_peer);
        s.absorb(b"output nobody on the phone will see\r\n");
        assert_eq!(s.remote_viewers(), 0, "the dead connection was not dropped");
        assert_eq!(s.info.attached, 1);
        assert_eq!(window_size(master), desk, "the desktop kept the phone's size");

        pty::close(slave);
        pty::close(master);
    }

    #[test]
    fn typing_on_the_desktop_takes_the_terminal_back_from_an_attached_phone() {
        use rime_agent_core::term::window_size;
        let Some((master, slave)) = pty::bare_pair() else {
            eprintln!("SKIP: no /dev/ptmx on this machine");
            return;
        };
        let desk = WinSize { cols: 180, rows: 50 };
        let phone = WinSize { cols: 46, rows: 30 };
        let mut s = session(2);
        s.master = master;
        let (local, _lp) = UnixStream::pair().unwrap();
        let (remote, _rp) = UnixStream::pair().unwrap();
        s.attach_size(Viewer::Local, desk).unwrap();
        s.attach_as(local, 0, Viewer::Local).unwrap();
        s.attach_size(Viewer::Remote, phone).unwrap();
        s.attach_as(remote, 0, Viewer::Remote).unwrap();

        // The desktop terminal answering a query is not the desktop typing.
        s.typed(Viewer::Local, b"\x1b[24;1R");
        assert_eq!(window_size(master), phone);
        // A key is.
        s.typed(Viewer::Local, b"y");
        assert_eq!(window_size(master), desk);
        assert_eq!((s.info.cols, s.info.rows), (desk.cols, desk.rows));
        // And the phone typing gives it back to the phone.
        s.typed(Viewer::Remote, b"\r");
        assert_eq!(window_size(master), phone);

        pty::close(slave);
        pty::close(master);
    }

    #[test]
    fn a_session_with_no_terminal_left_resizes_nothing_and_does_not_fail() {
        // The reaper sets `master` to -1 as the child goes; a phone detaching
        // in that moment must not surface an EBADF.
        let mut s = session(3);
        assert_eq!(s.master, -1);
        s.attach_size(Viewer::Remote, WinSize { cols: 40, rows: 20 })
            .expect("nothing to resize is not an error");
        s.settle_size();
    }
}
