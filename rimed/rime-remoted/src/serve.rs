//! One device connection, from the first handshake byte to the last frame.
//!
//! Two shapes arrive on the same listener and are told apart by the first
//! thing the device says: a **pairing** connection, which uses `Noise_NK` and
//! the one-time token, and a **session** connection, which uses `Noise_IK`
//! and an already-paired key. The device announces which in one plaintext
//! byte before the handshake — the only plaintext this protocol has after the
//! length prefix, and it says nothing an observer could not infer from the
//! message sizes anyway.
//!
//! ## What a connection may do, in order
//!
//! 1. Handshake. A session handshake yields the device's static key, which is
//!    looked up in the store. An unknown or revoked key ends the connection
//!    here, before a single frame is read. Which of the device's keys it was
//!    decides the session's scope: a companion key gets only
//!    `companion::ALLOWED_VERBS` and `companion::ALLOWED_CHANNELS`.
//! 2. Frames. Control frames are round-tripped through `rime-agentd` on a
//!    connection that declared its origin first; `Open` starts a PTY channel;
//!    `Data` goes to the PTY it names.
//!
//! There is no step where the device says who it is. It cannot: the identity
//! is what the handshake proved, and every proxied request carries it as the
//! `actor` the daemon records.
//!
//! ## Three threads per connection, and why the frame loop is never the one that waits
//!
//! The **frame loop** reads, decrypts and routes. It answers a `Ping`, times a
//! `Pong`, and writes a `Data` frame to the PTY it names — and it does nothing
//! that can wait on `rime-agentd`. The **worker** takes every `Control` and
//! every `Open`, strictly in the order they arrived, and writes each reply
//! through the same [`Sealer`]. The **pinger** measures the connection and,
//! since `liveness`, ends it when the device stops answering.
//!
//! It used to be one loop doing all of it, and a control request that waited
//! — `CONTROL_TIMEOUT` is five minutes, for the requests a human answers —
//! stopped every terminal on the connection for as long as it waited: no
//! keystroke reached a PTY, no pong was read, and a phone that wanted a
//! terminal to keep moving had to open a second connection per terminal and
//! pay a full handshake each time. The worker is what makes one connection
//! enough (`mux_attach`).
//!
//! **One worker, not one thread per request.** The phone pairs a reply with
//! its request by ORDER alone — there is no request id on the wire — and its
//! queue holds `Open`s as well as `Control`s, because an `Open` is answered
//! on channel zero too (`android/.../link/Mux.kt`, "The ordering rule"). A
//! pool would answer a fast `list` before a slow `approve` sent ahead of it
//! and the phone would show one reply under the other request. So a slow
//! request still delays the replies behind it, which the protocol requires;
//! what it no longer delays is everything that is not a reply.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rime_remote_core::device::Scope;
use rime_remote_core::noise::{Channel, Handshake};
use rime_remote_core::pairing::{PairingRequest, PairingError};
use rime_remote_core::wire::Frame;

use crate::state::{Live, State};

/// The device's first byte: which handshake follows.
///
/// A byte rather than a negotiation. Both values lead to a Noise handshake
/// that either completes or does not, so an observer flipping it produces a
/// failed connection rather than a downgrade — there is no weaker option to
/// be steered towards.
pub const HELLO_PAIR: u8 = b'P';
pub const HELLO_SESSION: u8 = b'S';
/// Another Rime computer pairing with a code (`computers.rs`).
pub const HELLO_COMPUTER: u8 = b'C';

/// How often the desktop measures an open connection.
///
/// Fifteen seconds is a compromise between a page that is out of date and a
/// radio that is kept awake. It is also the keepalive: a `Ping` is the only
/// traffic an idle session has, and an idle session with no traffic is one a
/// NAT eventually forgets.
pub const PING_INTERVAL: Duration = Duration::from_secs(15);

/// How long an unauthenticated peer may take to finish its handshake.
///
/// Applied by `main` on every accepted socket and cleared in [`session`] only
/// after the device has been identified and authorised, so a peer that
/// connects and says nothing is dropped rather than holding a thread. Long
/// enough for a phone waking its radio on a bad connection; short enough that
/// filling the thread pool takes deliberate effort rather than patience.
pub const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// What this build answers `remote_hello` with.
///
/// A phone uses a feature only when its name is here, so each entry is a
/// promise about behaviour this file keeps, not a version number: `nodelay`
/// is `main::prepare_accepted` and `net::write_message`'s single write;
/// `control_worker` is [`Worker`]; `mux_attach` is the consequence — a
/// terminal on the control connection keeps moving while a request waits, so
/// the phone may stop dialling a connection per terminal; `liveness` is
/// [`Outstanding::silent`]. A name the code did not honour would be a phone
/// relying on something that is not there, which is worse than one that
/// never heard of it.
pub const FEATURES: [&str; 4] = ["nodelay", "control_worker", "mux_attach", "liveness"];

/// The verbs this file answers itself, before a line could reach `rime-agentd`.
///
/// `push::VERBS` are the only others. Both lists are consulted by
/// [`answered_here`] and nothing else is intercepted.
pub const VERBS: [&str; 1] = ["remote_hello"];

/// How many pings in a row may go unanswered before the connection is closed.
///
/// Three, at the shipped fifteen-second interval: forty-five seconds without
/// a pong. The phone reconnects on its own, so closing a connection that is
/// merely slow costs a handshake; keeping one that is dead costs everything
/// sent into it, and before this nothing ever closed one — a phone that
/// walked out of Wi-Fi coverage left a connection here that swallowed its
/// terminal output until TCP gave up, which on Linux is a quarter of an hour.
pub const LIVENESS_MISSES: usize = 3;

/// The least silence that closes a connection, however short the interval.
///
/// Only matters below the shipped interval. The suites run pings every
/// 100 ms so they can watch a measurement happen, and a test that pauses for
/// half a second to poll the status socket would otherwise have its session
/// closed underneath it — which is a fact about the test's clock, not about
/// the link.
pub const LIVENESS_FLOOR: Duration = Duration::from_secs(2);

/// How many requests may wait for the worker before the frame loop waits too.
///
/// Bounded, so a device that floods control frames meets the same
/// back-pressure it met when the frame loop did the work itself — the loop
/// stops reading and TCP pushes back — rather than growing this process's
/// memory. A phone never has more than a handful outstanding.
const WORK_QUEUE: usize = 64;

/// Why a device connection ended.
#[derive(Debug)]
pub enum ServeError {
    Io(std::io::Error),
    Handshake(String),
    /// The handshake completed and the key is not one this machine accepts.
    ///
    /// Deliberately the same message for "never paired" and "revoked": a
    /// device that can tell the difference has an oracle for which keys this
    /// machine has ever seen.
    NotPaired,
    Protocol(String),
    /// The device stopped answering pings and the connection was closed.
    ///
    /// Carries how long it had been silent, which is the one number that says
    /// whether this was a phone in a lift or a phone in a drawer.
    Silent(Duration),
}

impl std::fmt::Display for ServeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServeError::Io(e) => write!(f, "{e}"),
            ServeError::Handshake(w) => write!(f, "the handshake did not complete: {w}"),
            ServeError::NotPaired => write!(f, "that device is not paired with this machine"),
            ServeError::Protocol(w) => write!(f, "{w}"),
            ServeError::Silent(quiet) => write!(
                f,
                "it answered none of the last {LIVENESS_MISSES} pings ({} s without a sign of \
                 life), so the connection was closed and the device will have to reconnect",
                quiet.as_secs()
            ),
        }
    }
}

impl From<std::io::Error> for ServeError {
    fn from(e: std::io::Error) -> ServeError {
        ServeError::Io(e)
    }
}

/// Handle one accepted TCP connection.
pub fn connection(mut socket: TcpStream, state: Arc<State>, agentd: std::path::PathBuf) {
    // Flattened here, at the boundary, and nowhere else. The listener is
    // dual-stack, so every IPv4 peer arrives as `::ffff:a.b.c.d`; see
    // `state::canonical`.
    let peer = crate::state::canonical(socket.peer_addr().ok());
    let mut hello = [0u8; 1];
    if socket.read_exact(&mut hello).is_err() {
        return;
    }
    let outcome = match hello[0] {
        HELLO_PAIR => pair(&mut socket, &state),
        HELLO_SESSION => session(socket.try_clone().ok(), socket, &state, &agentd),
        HELLO_COMPUTER => crate::computers::accept(&mut socket, &state, peer),
        other => Err(ServeError::Protocol(format!(
            "a connection opened with {other:#04x}, which is neither a pairing nor a session"
        ))),
    };
    if let Err(e) = outcome {
        // One line, on stderr, where journald keeps it. Never the key, never
        // the token, never a frame's contents: this log is read by whoever
        // can read the unit's journal, which on a shared machine is more
        // people than can read the device store.
        eprintln!(
            "rime-remoted: connection from {} ended: {e}",
            peer.map(|p| p.ip().to_string())
                .unwrap_or_else(|| "an unknown address".into())
        );
    }
}

/// A pairing connection: `Noise_NK`, then the token.
fn pair(socket: &mut TcpStream, state: &State) -> Result<(), ServeError> {
    let secret = crate::secret_of(state);
    let mut hs = Handshake::pairing_responder(&secret, rime_remote_core::REMOTE_PROTOCOL_VERSION)
        .map_err(|e| ServeError::Handshake(e.to_string()))?;
    let m1 = crate::net::read_message(socket)?;
    let payload = hs
        .read(&m1)
        .map_err(|e| ServeError::Handshake(e.to_string()))?;
    let request: PairingRequest = serde_json::from_slice(&payload)
        .map_err(|e| ServeError::Protocol(format!("the pairing request is malformed: {e}")))?;

    let now = rime_remote_core::now_ms();
    let result = complete_pairing(state, &request, now);

    // The answer goes back inside the finished handshake either way, so a
    // refusal is as confidential as an acceptance. A plaintext "no" would
    // tell anybody watching that this machine is not currently pairing.
    let answer = match &result {
        Ok(device) => serde_json::json!({"ok": true, "device": device.id, "machine": state.machine}),
        Err(e) => serde_json::json!({"ok": false, "error": e.to_string()}),
    };
    let m2 = hs
        .write(answer.to_string().as_bytes())
        .map_err(|e| ServeError::Handshake(e.to_string()))?;
    crate::net::write_message(socket, &m2)?;
    result.map(|_| ()).map_err(|e| ServeError::Protocol(e.to_string()))
}

/// The store side of pairing, so the socket side above stays readable.
fn complete_pairing(
    state: &State,
    request: &PairingRequest,
    now: u64,
) -> Result<rime_remote_core::device::Device, PairingError> {
    {
        let mut offer = state.offer.lock().expect("offer lock");
        if let Some(offer) = offer.as_mut() {
            let mut store = state
                .devices()
                .map_err(|e| PairingError::BadDevice(e.to_string()))?;
            match rime_remote_core::pairing::complete(offer, request, &mut store, now) {
                // Not the QR on screen: perhaps an offer a peer asked for.
                Err(PairingError::BadToken) => {}
                Err(e) => return Err(e),
                Ok(device) => {
                    state
                        .save_devices(&store)
                        .map_err(|e| PairingError::BadDevice(format!("the device was not written: {e}")))?;
                    return Ok(device);
                }
            }
        }
    }
    // An offer another computer asked for its phone (intro.rs): the phone is
    // recorded as introduced, never with the full scope. No offer open
    // anywhere is the same error a wrong token gets, so a device cannot use
    // the difference to find out whether the owner is looking at a QR code.
    crate::intro::complete(state, request, now)
}

/// A session connection: `Noise_IK`, then frames.
///
/// Takes the `Arc` rather than a reference because the connection's worker
/// outlives nothing but still has to own what it uses: a borrowed `State`
/// would tie the worker to this thread's stack, and a scoped thread would
/// hold the connection open until a five-minute request finished.
fn session(
    reader_socket: Option<TcpStream>,
    mut socket: TcpStream,
    state: &Arc<State>,
    agentd: &std::path::Path,
) -> Result<(), ServeError> {
    let secret = crate::secret_of(state);
    let mut hs = Handshake::session_responder(&secret, rime_remote_core::REMOTE_PROTOCOL_VERSION)
        .map_err(|e| ServeError::Handshake(e.to_string()))?;
    let m1 = crate::net::read_message(&mut socket)?;
    hs.read(&m1)
        .map_err(|e| ServeError::Handshake(e.to_string()))?;
    let device_key = hs
        .remote_static()
        .ok_or_else(|| ServeError::Handshake("the device sent no static key".into()))?;
    // This machine's own key, presented by somebody else: a copied home.
    // Refused before the store is asked, whatever it holds, and with the same
    // words as any key this machine does not accept.
    if refuses_own_key(state, &device_key) {
        return Err(ServeError::NotPaired);
    }

    // Identified, then authorised, in that order and both before a frame is
    // read. The lookup is on the full key: an id is a prefix and prefixes are
    // how two keys become one device.
    let key_text = rime_remote_core::b64_encode(&device_key);
    let (device_id, device_name, scope) = {
        let store = state
            .devices()
            .map_err(|e| ServeError::Protocol(e.to_string()))?;
        let Some((d, scope)) = store.authenticate(&key_text) else {
            // The handshake is deliberately NOT completed for an unpaired
            // key. Completing it and then refusing would confirm to the
            // caller that this machine's key is what they think it is, which
            // is exactly what a scanner sweeping for Rime machines wants.
            return Err(ServeError::NotPaired);
        };
        (d.id.clone(), d.name.clone(), scope)
    };

    let m2 = hs
        .write(state.machine.as_bytes())
        .map_err(|e| ServeError::Handshake(e.to_string()))?;
    crate::net::write_message(&mut socket, &m2)?;
    let channel = hs
        .into_transport()
        .map_err(|e| ServeError::Handshake(e.to_string()))?;

    // Authenticated, so the handshake deadline comes off: a PTY may sit idle
    // for hours and a read deadline on it would end the terminal. Cleared
    // here and nowhere earlier — everything above this line runs against an
    // unauthenticated peer.
    socket.set_read_timeout(None).ok();
    socket.set_write_timeout(None).ok();

    // The peer address is what identifies THIS connection among a device's
    // several, so it is read once here: after the socket is shut down there
    // is nothing to read it from, and `unregister` would then match nothing.
    // It is also what says which path this session arrived on, so it is read
    // before the store is told.
    let peer = crate::state::canonical(socket.peer_addr().ok());
    let path = state.path_of(peer);
    let now = rime_remote_core::now_ms();
    {
        let mut store = state
            .devices()
            .map_err(|e| ServeError::Protocol(e.to_string()))?;
        // Not the literal "lan" it used to be: a relayed session reaches this
        // listener too, and a device list that called every session local
        // would be telling the owner nobody else was on the path.
        store.seen(&device_id, now, path.as_str());
        // Not for another computer: its refresh connects every few minutes,
        // and writing the store each time would be a disk write per dial.
        // The time is kept in memory and written with the next change.
        if scope != Scope::Peer {
            let _ = state.save_devices(&store);
        }
    }
    // Shared with the frame loop, which is the only writer, and with anybody
    // asking for status, which is the only reader.
    let rtt_ms: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));
    // A connection revoke cannot find is one revoke cannot end, so a socket
    // that cannot be cloned for the list is not served at all.
    let live = Live {
        device_id: device_id.clone(),
        peer,
        socket: socket.try_clone()?,
        device_name: device_name.clone(),
        path,
        since_ms: now,
        rtt_ms: Arc::clone(&rtt_ms),
        scope,
    };
    if !admit(state, live, &key_text) {
        let _ = socket.shutdown(std::net::Shutdown::Both);
        return Err(ServeError::NotPaired);
    }

    let result = frames(
        reader_socket.unwrap_or(socket.try_clone()?),
        socket,
        channel,
        state,
        agentd,
        &device_id,
        scope,
        rtt_ms,
    );
    state.unregister(&device_id, peer);
    result
}

/// Whether a key presented to the session responder is this machine's own.
fn refuses_own_key(state: &State, key: &[u8; 32]) -> bool {
    *key == state.identity.public_bytes()
}

/// Whether a session of this scope may open a channel with this request. A
/// full session may open any (what is behind it decides); a companion session
/// only a request the allow list names AND the phone routers' own predicates
/// claim, so nothing it opens can fall through to nex or `rime-agentd`; a
/// peer session (another computer) a file it sends or an agent attach and
/// nothing else, which the channel opener routes on the scope: the attach
/// only as far as the grant a person here gave that computer (`agents.rs`
/// checks it before anything else), never the peer's request passed on.
fn may_open(scope: Scope, request: &[u8]) -> bool {
    match scope {
        Scope::Full => true,
        Scope::Peer => crate::companion::verb_of(request)
            .is_some_and(|v| crate::computers::PEER_CHANNELS.contains(&v.as_str())),
        Scope::Companion => {
            crate::companion::channel_allowed(scope, request)
                && (crate::phone::is_channel(request) || crate::phone::is_take(request))
        }
    }
}

/// Put this connection on the live list, then ask the store again whether
/// `key_text` still opens it, with the same device and scope.
///
/// A revoke, `companion_forget` or replaced companion key that lands between
/// the handshake's lookup and the list ends the connections it finds on the
/// list, so it would miss this one. Asked again AFTER it is listed, one of
/// the two always sees the other: the change is in the store before this
/// look, or this connection is on the list before the change ends what is
/// there. `false`, with the connection taken off the list again, when it no
/// longer opens.
fn admit(state: &State, live: Live, key_text: &str) -> bool {
    let (device_id, peer, scope) = (live.device_id.clone(), live.peer, live.scope);
    state.register(live);
    let still = state.devices().is_ok_and(|store| {
        store
            .authenticate(key_text)
            .is_some_and(|(d, s)| d.id == device_id && s == scope)
    });
    if !still {
        state.unregister(&device_id, peer);
    }
    still
}

/// The frame loop.
///
/// One writer, several readers: PTY channels each get a thread that reads the
/// daemon's Unix socket and pushes `Data` frames out. Everything that leaves
/// this connection goes through one [`Sealer`], because the Noise channel's
/// nonce is a counter and two threads sealing concurrently would produce two
/// messages claiming the same one.
///
/// What this function itself does is read. Control and `Open` go to the
/// [`Worker`]; `Data` and `Close` go straight to the channel they name unless
/// that channel is still behind the worker (see [`Channels`]); `Ping` is
/// answered here and `Pong` is timed here, which is what keeps both working
/// while a request is in flight.
// The scope travels as its own argument, next to the device id it belongs
// to, so a reader can follow it from the handshake to the worker.
#[allow(clippy::too_many_arguments)]
fn frames(
    mut inbound: TcpStream,
    outbound: TcpStream,
    channel: Channel,
    state: &Arc<State>,
    agentd: &std::path::Path,
    device_id: &str,
    scope: Scope,
    rtt_ms: Arc<Mutex<Option<u64>>>,
) -> Result<(), ServeError> {
    // The write half gets a deadline the read half never has. A read that
    // waits is an idle terminal and is fine for hours; a WRITE that makes no
    // progress for this long is a peer whose TCP window has been shut the
    // whole time — gone, or frozen. Without it a writer blocked on such a
    // socket holds the sealer, the pinger queues behind it, and the liveness
    // rule below never gets to run. SO_SNDTIMEO bounds each `write(2)` rather
    // than the whole frame, so a slow link that is moving never trips it.
    outbound
        .set_write_timeout(Some(write_deadline(state.ping_interval)))
        .ok();
    // Held here so this function can end the connection for every thread at
    // once when it returns, whatever the reason.
    let hangup = outbound.try_clone()?;
    let sealer = Arc::new(Mutex::new(Sealer {
        channel,
        socket: outbound,
    }));
    let channels = Arc::new(Mutex::new(Channels::default()));
    let ended = Arc::new(AtomicBool::new(false));

    // The desktop measures its own connections. Answering a device's pings,
    // which is all this used to do, tells the desktop nothing: a round trip
    // is only known to whoever sent the first half of it.
    let outstanding = Arc::new(Mutex::new(Outstanding::new()));
    let silenced: Arc<Mutex<Option<Duration>>> = Arc::new(Mutex::new(None));
    {
        let sealer = Arc::clone(&sealer);
        let outstanding = Arc::clone(&outstanding);
        let silenced = Arc::clone(&silenced);
        let ended = Arc::clone(&ended);
        let hangup = hangup.try_clone()?;
        let interval = state.ping_interval;
        std::thread::spawn(move || {
            let mut token: u64 = 0;
            loop {
                if ended.load(Ordering::Relaxed) {
                    return;
                }
                {
                    let Ok(mut o) = outstanding.lock() else { return };
                    // Checked BEFORE the next ping goes out, so the newest of
                    // the three unanswered ones has had a whole interval to
                    // come back. `shutdown` rather than a flag: the frame loop
                    // is parked in a read and would not look at a flag until
                    // the device sent something, which is the one thing a
                    // silent device does not do.
                    if let Some(quiet) = o.silent() {
                        drop(o);
                        if let Ok(mut s) = silenced.lock() {
                            *s = Some(quiet);
                        }
                        let _ = hangup.shutdown(std::net::Shutdown::Both);
                        return;
                    }
                    token += 1;
                    o.sent(token);
                }
                // The thread also ends when the write fails, which is what a
                // closed connection looks like from here.
                if send(&sealer, Frame::Ping { token }).is_err() {
                    return;
                }
                std::thread::sleep(interval);
            }
        });
    }

    static CONNECTIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let connection = CONNECTIONS.fetch_add(1, Ordering::Relaxed) + 1;
    let (work, queue) = std::sync::mpsc::sync_channel::<Work>(WORK_QUEUE);
    {
        let worker = Worker {
            state: Arc::clone(state),
            agentd: agentd.to_path_buf(),
            device_id: device_id.to_string(),
            scope,
            connection,
            sealer: Arc::clone(&sealer),
            channels: Arc::clone(&channels),
        };
        std::thread::spawn(move || worker.run(queue));
    }

    let outcome = read_frames(
        &mut inbound,
        &sealer,
        &channels,
        &work,
        &outstanding,
        &rtt_ms,
    );

    // The end, in an order that leaves nothing half-done. The worker is told
    // first — under the channel lock, so an `Open` it is finishing right now
    // either lands before the sweep below or sees the flag and detaches —
    // then its queue is closed, so it drains what the device sent and stops.
    // Every PTY is detached (the sessions go on running in the daemon), and
    // the socket is shut so every writer still holding the sealer fails at
    // once instead of at its next ping.
    ended.store(true, Ordering::Relaxed);
    let detached: Vec<Arc<DaemonChannel>> = {
        let mut c = lock(&channels);
        c.ended = true;
        c.open.drain().map(|(_, p)| p).collect()
    };
    drop(work);
    drop(detached);
    let _ = hangup.shutdown(std::net::Shutdown::Both);
    // What the phone was playing was true while it was here to say so.
    crate::media::media().ended(device_id, connection);
    // A messages request dies with the connection it was asked on, and what
    // it had gathered goes with it (contract §2.11).
    state.messages.ended(device_id, connection);
    // So does a calls request, and what the connection said about a call:
    // a ringing notification closes rather than ring on for a phone gone.
    state.calls.ended(device_id, connection, crate::mirror::mirror());
    // A photos listing or save asked on it fails, and the thumbnails its
    // listing wrote are removed (contract §2.13).
    state.photos.ended(device_id, connection);
    state.events.ended(device_id, connection);

    if let Some(quiet) = silenced.lock().ok().and_then(|s| *s) {
        return Err(ServeError::Silent(quiet));
    }
    outcome
}

/// How long one write to the device may make no progress.
///
/// The same window the pings get, so the two ways this side notices a dead
/// device agree about how long "dead" takes.
fn write_deadline(interval: Duration) -> Duration {
    (interval * LIVENESS_MISSES as u32).max(LIVENESS_FLOOR)
}

/// Read, decrypt and route until the connection ends.
fn read_frames(
    inbound: &mut TcpStream,
    sealer: &Arc<Mutex<Sealer>>,
    channels: &Arc<Mutex<Channels>>,
    work: &SyncSender<Work>,
    outstanding: &Arc<Mutex<Outstanding>>,
    rtt_ms: &Arc<Mutex<Option<u64>>>,
) -> Result<(), ServeError> {
    let to_worker = |w: Work| {
        work.send(w).map_err(|_| {
            ServeError::Protocol("this connection's control worker stopped".into())
        })
    };
    loop {
        let message = match crate::net::read_message(inbound) {
            Ok(m) => m,
            // An ordinary close, a revoked device's socket shut down under
            // it, or the pinger giving up on a silent one. All end the loop
            // and none is an error worth a log line here; the pinger's case
            // is reported by the caller, which knows it happened.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(ServeError::Io(e)),
        };
        let plaintext = {
            let mut s = sealer.lock().expect("sealer lock");
            s.channel
                .open(&message)
                .map_err(|e| ServeError::Protocol(e.to_string()))?
        };
        let frame = Frame::decode(&plaintext).map_err(|e| ServeError::Protocol(e.to_string()))?;
        match frame {
            Frame::Ping { token } => send(sealer, Frame::Pong { token })?,
            // The other half of the measurement. A token that was never sent
            // is ignored rather than timed: a device that echoed a number of
            // its own choosing could otherwise report any quality it liked,
            // including a good one for a connection that is unusable. It
            // does not count as a sign of life either, for the same reason.
            Frame::Pong { token } => {
                if let Ok(mut o) = outstanding.lock() {
                    if let Some(elapsed) = o.answered(token) {
                        if let Ok(mut v) = rtt_ms.lock() {
                            *v = Some(elapsed.as_millis() as u64);
                        }
                    }
                }
            }
            Frame::Control(line) => to_worker(Work::Control(line))?,
            Frame::Open { channel, request } => {
                lock(channels).queue(channel);
                to_worker(Work::Open { channel, request })?;
            }
            Frame::Data { channel, bytes } => {
                let route = lock(channels).route(channel);
                match route {
                    Route::Direct(pty) => {
                        let _ = pty.write(&bytes);
                    }
                    Route::Worker => to_worker(Work::Data { channel, bytes })?,
                    Route::Nowhere => {}
                }
            }
            Frame::Close { channel, .. } => {
                let detached = {
                    let mut c = lock(channels);
                    if c.is_behind(channel) {
                        c.queue(channel);
                        None
                    } else {
                        Some(c.open.remove(&channel))
                    }
                };
                // Dropped outside the lock: dropping the last handle shuts
                // the daemon connection down, which is a syscall the worker
                // has no reason to wait behind.
                if detached.is_none() {
                    to_worker(Work::Close { channel })?;
                }
            }
        }
    }
}

/// What the frame loop hands the worker, in the order the device sent it.
enum Work {
    /// One line of `rime-agentd`'s protocol, or of this service's own.
    Control(Vec<u8>),
    /// A channel to open. Answered on channel zero, so it queues with control.
    Open { channel: u32, request: Vec<u8> },
    /// Bytes for a channel whose `Open` the worker has not finished yet.
    Data { channel: u32, bytes: Vec<u8> },
    /// A close for a channel whose `Open` the worker has not finished yet.
    Close { channel: u32 },
}

/// The channels one connection has open, shared by the frame loop and the
/// worker.
///
/// ## `behind`, and the reordering it exists to prevent
///
/// With `Open` off the frame loop, a device's `Data` can arrive for a channel
/// the worker has not opened yet. Dropping it would lose bytes that the old
/// inline loop delivered; writing it anywhere but through the worker would let
/// it overtake the `Open`. So a channel with anything still queued in the
/// worker — its `Open`, or a `Data`/`Close` that followed it — takes its next
/// frames through the worker too, in order, and goes back to the direct path
/// only once the worker has caught up. The count is changed under this lock
/// by both sides, so the frame loop can never see a channel as neither behind
/// nor open while its `Open` is being finished.
///
/// A real phone waits for the `attached` reply before it types, so in
/// practice this is empty; it is here so that a client that does not wait is
/// still served exactly as it was before.
#[derive(Default)]
struct Channels {
    /// Channels carrying a PTY or an upload, by the device's channel number.
    open: HashMap<u32, Arc<DaemonChannel>>,
    /// How many frames for a channel are waiting in the worker's queue.
    behind: HashMap<u32, usize>,
    /// Set once the connection has ended: a channel opened after this is
    /// detached rather than published into a map nobody will ever sweep.
    ended: bool,
}

/// Where a `Data` frame goes.
enum Route {
    Direct(Arc<DaemonChannel>),
    Worker,
    /// No such channel: closed, refused, or never opened. Dropped, as before.
    Nowhere,
}

impl Channels {
    fn queue(&mut self, channel: u32) {
        *self.behind.entry(channel).or_default() += 1;
    }

    fn is_behind(&self, channel: u32) -> bool {
        self.behind.contains_key(&channel)
    }

    /// The worker has dealt with one frame for this channel.
    fn done(&mut self, channel: u32) {
        if let Some(n) = self.behind.get_mut(&channel) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.behind.remove(&channel);
            }
        }
    }

    fn route(&mut self, channel: u32) -> Route {
        if self.is_behind(channel) {
            self.queue(channel);
            Route::Worker
        } else if let Some(pty) = self.open.get(&channel) {
            Route::Direct(Arc::clone(pty))
        } else {
            Route::Nowhere
        }
    }
}

/// A lock that survives a panic on another thread.
///
/// The data behind it is two maps and a flag, written whole, so a guard
/// recovered from a poisoned lock is as good as any; refusing it would turn
/// one panicking pump into a connection that can open no more channels.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The per-connection thread that answers control requests and opens
/// channels, one at a time and in arrival order.
struct Worker {
    state: Arc<State>,
    agentd: PathBuf,
    device_id: String,
    /// Which key opened this connection. A companion session's lines and
    /// channels are checked against `companion`'s lists here, before any
    /// routing.
    scope: Scope,
    /// This connection's number, unique in this process: what a phone's
    /// media player is tied to (`media.rs`).
    connection: u64,
    sealer: Arc<Mutex<Sealer>>,
    channels: Arc<Mutex<Channels>>,
}

impl Worker {
    /// Until the frame loop drops its end of the queue and the queue is empty.
    ///
    /// ## When the connection ends with a request in flight
    ///
    /// The request in flight finishes — `rime-agentd` is not interrupted, as
    /// it was not when the frame loop itself waited — and its reply is sent
    /// into a closed socket and lost. Requests queued behind it still RUN, in
    /// order, with nobody to read their replies: each was received whole
    /// before the connection ended, and the old loop likewise ran everything
    /// it had read. `Open`s are the exception. An attach for a connection that
    /// no longer exists would resize the session to the phone's terminal and
    /// detach again at once, which changes the desktop's terminal for nobody,
    /// so a queued `Open` is skipped once the connection has ended.
    fn run(self, queue: Receiver<Work>) {
        for work in queue {
            match work {
                Work::Control(line) => {
                    // A phone setting this computer's clipboard (§1.8) is
                    // remembered BEFORE it is forwarded: the Shell sees the
                    // new clipboard as soon as rime-agentd writes it, and its
                    // `rime remote clipboard` must find the text already
                    // known, or it goes straight back to this phone (§2.14).
                    // Only a full session may write it at all (a companion
                    // one is refused below), so only a full one is remembered.
                    if self.scope == Scope::Full {
                        if let Some(text) = crate::clipboard::phone_wrote(&line) {
                            self.state.clip_echo.wrote(&self.device_id, text);
                        }
                    }
                    let reply = self.control(&line);
                    let _ = send(&self.sealer, Frame::Control(reply));
                    // A computer this line's tombstones made this one remove
                    // (it removed this computer first) gets its answer, then
                    // the session ends: the removal holds from the next byte.
                    if self.scope == Scope::Peer && !self.state.computer_active(&self.device_id) {
                        self.state.drop_connections_for(&self.device_id);
                    }
                }
                Work::Open { channel, request } => self.open(channel, &request),
                Work::Data { channel, bytes } => {
                    let pty = lock(&self.channels).open.get(&channel).cloned();
                    if let Some(pty) = pty {
                        let _ = pty.write(&bytes);
                    }
                    lock(&self.channels).done(channel);
                }
                Work::Close { channel } => {
                    let detached = {
                        let mut c = lock(&self.channels);
                        c.done(channel);
                        c.open.remove(&channel)
                    };
                    drop(detached);
                }
            }
        }
    }

    /// One control line's reply.
    ///
    /// The scope first, before anything is routed: a companion session's verb
    /// that is not on the list is refused here and goes nowhere. And a
    /// companion line that passed the list but that nothing in this process
    /// answered is refused too, rather than forwarded: `control` (the road to
    /// `rime-agentd`) is reachable from a full session only.
    fn control(&self, line: &[u8]) -> Vec<u8> {
        reply_to(&self.state, &self.agentd, &self.device_id, self.scope, self.connection, line)
    }

    fn open(&self, channel: u32, request: &[u8]) {
        {
            let mut c = lock(&self.channels);
            if c.ended {
                c.done(channel);
                return;
            }
        }
        // A companion session opens the phone channels and nothing else. The
        // second half of the check is the router's own predicates, so a
        // request the list let through can still never fall to nex or to
        // `rime-agentd` below. Refused the way a refused takeover is: the
        // reply, then the close.
        if !may_open(self.scope, request) {
            let what = crate::companion::verb_of(request);
            let _ = send(
                &self.sealer,
                Frame::Control(crate::companion::refusal(what.as_deref().unwrap_or("that channel"))),
            );
            let _ = send(
                &self.sealer,
                Frame::Close {
                    channel,
                    reason: String::new(),
                },
            );
            lock(&self.channels).done(channel);
            return;
        }
        // A nex channel goes to nexd and is read-only; everything else is a
        // rime-agentd takeover. nex's refusal travels as a reply then a
        // close, exactly like a takeover the daemon refused.
        // `keep`: whether the channel goes in the map. A one-shot nex answer
        // ends by itself once its bytes are out, so it is never kept.
        let opened = if self.scope == Scope::Peer
            && crate::companion::verb_of(request).as_deref() == Some(crate::agents::ATTACH_VERB)
        {
            // An agent session's terminal, for a computer a person here gave
            // `control`: agents.rs checks the grant, the fields and the cap,
            // then builds its own `attach`. The slot rides the channel, so
            // the attachment counts until it is detached.
            crate::agents::open_attach(&self.state, &self.agentd, &self.device_id, request)
                .map(|(stream, reply, buffered, slot)| {
                    let ch = DaemonChannel {
                        stream,
                        read_only: false,
                        _slot: Some(slot),
                    };
                    (ch, reply, buffered, true)
                })
                .map_err(crate::proxy::ProxyError::NotTakenOver)
        } else if self.scope == Scope::Peer {
            // Another computer's file, and only that: `may_open` let nothing
            // else through, and nothing a peer opens falls to the routes
            // below. Its bytes are checked and saved by files.rs, which
            // builds its own `save_file`; the channel closes after the outcome.
            crate::files::open_incoming(&self.state, &self.agentd, &self.device_id, request)
                .map(|(stream, reply)| {
                    let ch = DaemonChannel {
                        stream,
                        read_only: false,
                        _slot: None,
                    };
                    (ch, reply, Vec::new(), true)
                })
                .map_err(crate::proxy::ProxyError::NotTakenOver)
        } else if crate::phone::is_channel(request) {
            // The one channel this machine writes to unprompted: its other end
            // stays in `state.events`, and a ring is a line written there.
            self.state
                .events
                .open_for(&self.device_id, self.connection, self.scope == Scope::Full)
                .map(|(stream, _)| {
                    // What was sent while the phone was away, written now:
                    // the lines wait in the socketpair and the pump, which
                    // starts after the reply, forwards them behind it.
                    for line in self.state.outbox.pending(&self.device_id, &self.state.machine) {
                        self.state.events.send(&self.device_id, &line);
                    }
                    // A phone that already said join is brought now that it
                    // can be told (intro.rs); lines wait behind the reply.
                    crate::intro::reported(&self.state, &self.device_id, self.scope);
                    let ch = DaemonChannel {
                        stream,
                        read_only: true,
                        _slot: None,
                    };
                    (ch, crate::phone::opened_reply(), Vec::new(), true)
                })
                .map_err(|e| {
                    crate::proxy::ProxyError::NotTakenOver(
                        serde_json::json!({"reply": "error", "code": "unavailable", "message": e.to_string()})
                            .to_string()
                            .into_bytes(),
                    )
                })
        } else if crate::phone::is_take(request) {
            // What somebody here sent this phone, by id, under this
            // connection's device id; the channel closes after the last byte.
            crate::phone::open_take(&self.state.outbox, &self.device_id, request)
                .map(|(stream, reply)| {
                    let ch = DaemonChannel {
                        stream,
                        read_only: true,
                        _slot: None,
                    };
                    (ch, reply, Vec::new(), true)
                })
                .map_err(crate::proxy::ProxyError::NotTakenOver)
        } else if crate::photos::is_upload(request) {
            // A photo somebody here asked to save, uploaded into the file
            // they opened; the channel closes after `uploaded`. Writable:
            // the phone's Data is the photo.
            self.state
                .photos
                .open_upload(&self.device_id, self.scope == Scope::Full, request)
                .map(|(stream, reply)| {
                    let ch = DaemonChannel {
                        stream,
                        read_only: false,
                        _slot: None,
                    };
                    (ch, reply, Vec::new(), true)
                })
                .map_err(crate::proxy::ProxyError::NotTakenOver)
        } else if crate::nex::is_channel(request) {
            crate::nex::open_channel(&self.state.nexd(), &self.device_id, request)
                .map(|o| {
                    let ch = DaemonChannel {
                        stream: o.stream,
                        read_only: true,
                        _slot: None,
                    };
                    (ch, o.reply, o.buffered, o.feed)
                })
                .map_err(|e| crate::proxy::ProxyError::NotTakenOver(e.reply()))
        } else {
            DaemonChannel::open(&self.agentd, &self.device_id, request)
                .map(|(ch, reply, buffered)| (ch, reply, buffered, true))
        };
        match opened {
            Ok((pty, reply, buffered, keep)) => {
                // The reply, then the bytes that arrived with it, THEN the
                // pump. The pump used to start inside `open`, before the
                // reply had been sent, so the first burst of scrollback could
                // reach the phone ahead of the `attached` that tells it the
                // channel exists — and the phone drops `Data` for a channel it
                // does not know yet. A terminal that opened blank, sometimes.
                let delivered = send(&self.sealer, Frame::Control(reply)).is_ok()
                    && Frame::data_frames(channel, &buffered)
                        .into_iter()
                        .all(|f| send(&self.sealer, f).is_ok());
                let pty = Arc::new(pty);
                let pumping = match delivered.then(|| pty.pump(channel, &self.sealer)) {
                    Some(Ok(())) => true,
                    Some(Err(e)) => {
                        let _ = send(
                            &self.sealer,
                            Frame::Close {
                                channel,
                                reason: e.to_string(),
                            },
                        );
                        false
                    }
                    None => false,
                };
                // Published and marked done in one critical section, so the
                // frame loop's next `Data` for this channel finds it open.
                // A channel with the same number already open is replaced and
                // detached; the old loop kept both and wrote to the first.
                let mut c = lock(&self.channels);
                if pumping && keep && !c.ended {
                    c.open.insert(channel, pty);
                }
                c.done(channel);
            }
            // A refused takeover comes back as the daemon's own reply where
            // possible, so the device renders "no session 4" or the size cap
            // rather than a channel that closed for no stated reason.
            Err(crate::proxy::ProxyError::NotTakenOver(reply)) => {
                let _ = send(&self.sealer, Frame::Control(reply));
                let _ = send(
                    &self.sealer,
                    Frame::Close {
                        channel,
                        reason: String::new(),
                    },
                );
                lock(&self.channels).done(channel);
            }
            Err(e) => {
                let _ = send(
                    &self.sealer,
                    Frame::Close {
                        channel,
                        reason: e.to_string(),
                    },
                );
                lock(&self.channels).done(channel);
            }
        }
    }
}

/// The reply to one control line from a session of this scope; see
/// [`Worker::control`].
fn reply_to(state: &State, agentd: &Path, device_id: &str, scope: Scope, connection: u64, line: &[u8]) -> Vec<u8> {
    // Another computer first, and only: its own short list, answered here,
    // never routed and never forwarded. Membership is not authority.
    if scope == Scope::Peer {
        return crate::computers::peer_reply(state, agentd, device_id, line);
    }
    let cmd = crate::companion::verb_of(line);
    if !crate::companion::allowed(scope, cmd.as_deref()) {
        return crate::companion::refusal(cmd.as_deref().unwrap_or("that request"));
    }
    match answered_here(state, device_id, scope, connection, line) {
        Some(reply) => reply,
        None if scope == Scope::Full => control(agentd, device_id, line),
        None => crate::companion::refusal(cmd.as_deref().unwrap_or("that request")),
    }
}

/// A control line this service answers itself, or `None` to forward it.
///
/// `remote_hello`, the companion key, and push registration. Each is about the CONNECTION — what
/// this end of it can do, how the phone is reached — which `rime-agentd` has
/// no concept of; see `push::control` for why this is the seam and not a new
/// frame tag. Everything else is forwarded verbatim.
fn answered_here(state: &State, device_id: &str, scope: Scope, connection: u64, line: &[u8]) -> Option<Vec<u8>> {
    let cmd = serde_json::from_slice::<serde_json::Value>(line)
        .ok()
        .and_then(|v| v.get("cmd").and_then(|c| c.as_str()).map(str::to_string));
    match cmd.as_deref() {
        // Through the list, so "which verbs does this file take" has one
        // answer a test can read — the same rule `push::control` keeps.
        Some(c) if VERBS.contains(&c) => Some(remote_hello(state, scope)),
        // The companion key: minted and forgotten by a full session only.
        Some(c) if crate::companion::VERBS.contains(&c) => crate::companion::control(state, device_id, scope, line),
        // The bridge to nex: the line goes to nexd, never to rime-agentd, and
        // only after this connection's device has been declared as its origin.
        Some(c) if crate::nex::VERBS.contains(&c) => Some(crate::nex::control(&state.nexd(), device_id, line)),
        // The computer itself: status, lock, media and volume.
        Some(c) if crate::machine::VERBS.contains(&c) => Some(crate::machine::control(crate::machine::backend(), line)),
        // The phone's own report and its ring acks.
        Some(c) if crate::phone::VERBS.contains(&c) => {
            let reply = crate::phone::control(state, device_id, scope, line);
            crate::intro::reported(state, device_id, scope);
            reply
        }
        // Introductions: a Full phone whose report says join (intro.rs).
        Some(c) if crate::intro::VERBS.contains(&c) => crate::intro::control(state, device_id, scope, line),
        // The phone's notifications, shown on this desktop. The name on them
        // is the device store's, never one the phone puts in the body.
        Some(c) if crate::mirror::VERBS.contains(&c) => {
            let name = state.devices().ok().and_then(|s| s.find(device_id).ok().map(|d| d.name.clone()));
            Some(crate::mirror::control(crate::mirror::mirror(), device_id, name.as_deref().unwrap_or(""), line))
        }
        // A phone's SMS threads, answering a request somebody here made.
        Some(c) if crate::messages::VERBS.contains(&c) => state.messages.control(device_id, connection, line),
        // A phone's recent photos, answering a listing or a save somebody
        // here asked for.
        Some(c) if crate::photos::VERBS.contains(&c) => state.photos.control(device_id, connection, line),
        // A phone's calls: its state, shown here while it rings, and the
        // answers to what somebody here asked of it.
        Some(c) if crate::calls::VERBS.contains(&c) => {
            let name = state.devices().ok().and_then(|s| s.find(device_id).ok().map(|d| d.name.clone()));
            let report = state.phones().get(device_id).cloned();
            state.calls.control(
                device_id,
                name.as_deref().unwrap_or(""),
                connection,
                report.as_ref(),
                crate::mirror::mirror(),
                line,
            )
        }
        // What the phone is playing, shown here as a media player.
        Some(c) if crate::media::VERBS.contains(&c) => {
            let name = state.devices().ok().and_then(|s| s.find(device_id).ok().map(|d| d.name.clone()));
            Some(crate::media::control(crate::media::media(), device_id, connection, name.as_deref().unwrap_or(""), line))
        }
        _ => crate::push::control(state, device_id, line),
    }
}

/// What this end of the connection can do, and where else it can be reached.
///
/// Answered here and never forwarded, so it describes THIS process. An older
/// `rime-remoted` forwards the line, `rime-agentd` answers `bad_request`, and
/// the phone reads that as "no features" — which is exactly right for a
/// daemon that has none of them.
///
/// `lan` is the pairing code's list, computed the same way at the moment of
/// asking: the port actually bound and only the address families the listener
/// accepts. A phone that paired on another network, or before a DHCP lease
/// changed, refreshes its stored list from this instead of dialling
/// addresses that stopped being this machine's.
///
/// `scope` is which of the device's keys opened this session, so a phone on
/// its companion key knows what it may ask for.
fn remote_hello(state: &State, scope: Scope) -> Vec<u8> {
    // `nex` only while nexd answers: a phone that shows nex screens for a
    // machine without it would show nothing but errors. Asked each time, so
    // starting nexd after the phone connected is seen on its next hello.
    let mut features: Vec<&str> = FEATURES.to_vec();
    features.extend(crate::machine::FEATURES);
    features.extend(crate::phone::FEATURES);
    features.extend(crate::mirror::FEATURES);
    features.extend(crate::media::FEATURES);
    features.extend(crate::messages::FEATURES);
    features.extend(crate::calls::FEATURES);
    features.extend(crate::photos::FEATURES);
    features.extend(crate::companion::FEATURES);
    features.extend(crate::intro::FEATURES);
    if crate::nex::available(&state.nexd()) {
        features.push("nex");
    }
    serde_json::json!({
        "reply": "remote",
        "version": rime_remote_core::REMOTE_PROTOCOL_VERSION,
        "features": features,
        "lan": state.lan_addresses(),
        "scope": scope.as_str(),
    })
    .to_string()
    .into_bytes()
}

/// Pings that have gone out and not come back, and when the device was last
/// heard from.
///
/// Bounded on purpose. A device that answers nothing would otherwise make
/// this grow by one entry every interval for as long as the connection is
/// open, which is a slow leak driven by the far end — exactly the shape of
/// thing a remote peer should not be able to do. And since `liveness` it
/// cannot grow for long anyway: [`LIVENESS_MISSES`] unanswered ends the
/// connection.
struct Outstanding {
    sent: VecDeque<(u64, Instant)>,
    /// The connection's start, then the moment of every valid pong.
    heard: Instant,
}

// The liveness rule counts entries in `sent`, so the bound has to be able to
// hold that many. A compile-time check rather than a comment, because the two
// numbers live in different places and drift apart one edit at a time.
const _: () = assert!(Outstanding::DEPTH >= LIVENESS_MISSES);

impl Outstanding {
    /// How many unanswered pings are remembered.
    ///
    /// Four intervals' worth. Past that a connection is not slow, it is gone,
    /// and the answer to a ping from a minute ago is not a measurement of
    /// anything current.
    const DEPTH: usize = 4;

    fn new() -> Outstanding {
        Outstanding {
            sent: VecDeque::new(),
            heard: Instant::now(),
        }
    }

    fn sent(&mut self, token: u64) {
        self.sent.push_back((token, Instant::now()));
        while self.sent.len() > Self::DEPTH {
            self.sent.pop_front();
        }
    }

    /// The round trip for a token this connection actually sent, or `None`.
    ///
    /// Consumed, so a device replaying one pong cannot pin a good reading in
    /// place while the connection degrades. Every OLDER ping goes with it: an
    /// answer to the newest proves the link is carrying answers now, and an
    /// earlier ping that got lost on the way is not evidence against a
    /// connection that has since answered.
    fn answered(&mut self, token: u64) -> Option<Duration> {
        let i = self.sent.iter().position(|(t, _)| *t == token)?;
        let (_, at) = self.sent[i];
        self.sent.drain(..=i);
        self.heard = Instant::now();
        Some(at.elapsed())
    }

    /// How long the device has been silent, once that is long enough to
    /// close the connection; `None` while it is not.
    ///
    /// Both halves: [`LIVENESS_MISSES`] pings in a row with no answer, and at
    /// least [`LIVENESS_FLOOR`] since the last one that was answered.
    fn silent(&self) -> Option<Duration> {
        let quiet = self.heard.elapsed();
        (self.sent.len() >= LIVENESS_MISSES && quiet >= LIVENESS_FLOOR).then_some(quiet)
    }
}

/// The one place a frame leaves this connection.
///
/// A failed write ends the connection for everybody. The nonce has already
/// moved on and the far end may hold part of the message, so nothing sealed
/// after it could be opened there; before this, a pump whose write failed
/// simply stopped, and the next frame any other thread sent was one the phone
/// could not decrypt.
fn send(sealer: &Arc<Mutex<Sealer>>, frame: Frame) -> Result<(), ServeError> {
    let bytes = frame
        .encode()
        .map_err(|e| ServeError::Protocol(e.to_string()))?;
    let mut s = sealer.lock().expect("sealer lock");
    let sealed = s
        .channel
        .seal(&bytes)
        .map_err(|e| ServeError::Protocol(e.to_string()))?;
    if let Err(e) = crate::net::write_message(&mut s.socket, &sealed) {
        let _ = s.socket.shutdown(std::net::Shutdown::Both);
        return Err(ServeError::Io(e));
    }
    Ok(())
}

/// The encrypting half of the connection, and the socket it writes to.
///
/// One mutex over both, on purpose. Sealing increments a nonce counter and
/// writing has to happen in the same order the sealing did; two locks would
/// let a thread seal message 5, lose the socket lock, and have message 6 go
/// out first — which the far end would refuse, correctly, and the connection
/// would die for a reason nothing explains.
struct Sealer {
    channel: Channel,
    socket: TcpStream,
}

/// Round-trip one control frame through the daemon.
///
/// Failures come back as a `Response::Error` the device can render, in the
/// daemon's own vocabulary, rather than as a dropped connection: a phone that
/// loses its terminal because the runtime was restarted is a worse experience
/// than one that is told the runtime was restarted.
fn control(agentd: &Path, device_id: &str, line: &[u8]) -> Vec<u8> {
    // A takeover verb on the control channel would leave the daemon connection
    // half out of control mode with this side still expecting reply lines.
    // `attach` wedges with the device's terminal bytes going nowhere;
    // `receive` wedges harder, because the daemon is then blocked reading an
    // upload that will never arrive and this loop is blocked reading a reply
    // that will never come. Refused with the thing that does work, rather than
    // by wedging.
    if crate::proxy::takes_over_the_channel(line) {
        return serde_json::json!({
            "reply": "error",
            "kind": "bad_request",
            "message": "attach, receive and save_file do not travel on the control channel; open a channel for them",
        })
        .to_string()
        .into_bytes();
    }
    match crate::proxy::Agentd::open(agentd, device_id).and_then(|mut a| a.round_trip(line)) {
        Ok(reply) => reply,
        Err(e) => serde_json::json!({
            "reply": "error",
            "kind": "internal",
            "message": e.to_string(),
        })
        .to_string()
        .into_bytes(),
    }
}

/// A connection to the daemon that the device has open as a channel.
///
/// Two verbs produce one. `attach` makes it the session's PTY, in both
/// directions and for as long as the terminal lives. `receive` makes it a sink
/// for one file: the device's `Frame::Data` becomes the upload, and the single
/// line the daemon writes back when it is done — the `injected` reply, or the
/// refusal — comes back as `Frame::Data` too, because the pump below does not
/// know or need to know which of the two it is carrying.
///
/// Shared (`Arc`) between the channel map and whichever thread is writing to
/// it at the moment, so a write never happens under the map's lock; the last
/// handle to go detaches.
struct DaemonChannel {
    stream: std::os::unix::net::UnixStream,
    /// A nex channel: what the device sends on it is dropped, never written
    /// to nexd. The feed is for reading; every write goes through `nex` on
    /// the control channel, where it is one request with one reply.
    read_only: bool,
    /// Another computer's agent attachment: its place under the cap, given
    /// back when the channel is detached (`agents.rs`).
    _slot: Option<crate::agents::Slot>,
}

impl DaemonChannel {
    fn open(
        agentd: &Path,
        device_id: &str,
        request: &[u8],
    ) -> Result<(DaemonChannel, Vec<u8>, Vec<u8>), crate::proxy::ProxyError> {
        let (stream, reply, buffered) =
            crate::proxy::Agentd::open(agentd, device_id)?.take_over(request)?;
        // The daemon stops treating a connection as control only when it says
        // so, and there are exactly two words for it: `attached` for a PTY,
        // `receiving` for an upload. Anything else — no such session, one that
        // has exited, an upload past the size cap, a malformed request —
        // leaves the connection in control mode, and a pump thread reading
        // THAT would deliver reply lines to the device as terminal output and
        // leave a channel open onto nothing. The reply goes back either way;
        // what changes is whether a channel is opened.
        let taken = serde_json::from_slice::<serde_json::Value>(&reply)
            .map(|v| {
                crate::proxy::TAKEOVER_REPLIES
                    .iter()
                    .any(|word| v["reply"] == *word)
            })
            .unwrap_or(false);
        if !taken {
            return Err(crate::proxy::ProxyError::NotTakenOver(reply));
        }
        Ok((
            DaemonChannel {
                stream,
                read_only: false,
                _slot: None,
            },
            reply,
            buffered,
        ))
    }

    /// Start copying the daemon's side of the channel to the device.
    ///
    /// Separate from [`DaemonChannel::open`] so the caller can put the reply
    /// and the buffered bytes on the wire first; see [`Worker::open`].
    fn pump(&self, channel: u32, sealer: &Arc<Mutex<Sealer>>) -> std::io::Result<()> {
        let mut read_half = self.stream.try_clone()?;
        let sealer = Arc::clone(sealer);
        // Detached: it ends by itself when the daemon connection does, which
        // is when the channel is closed or the daemon hangs up.
        std::thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match read_half.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        let _ = send(
                            &sealer,
                            Frame::Close {
                                channel,
                                reason: String::new(),
                            },
                        );
                        return;
                    }
                    Ok(n) => {
                        for f in Frame::data_frames(channel, &buf[..n]) {
                            if send(&sealer, f).is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        });
        Ok(())
    }

    fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        if self.read_only {
            return Ok(());
        }
        (&self.stream).write_all(bytes)?;
        (&self.stream).flush()
    }
}

impl Drop for DaemonChannel {
    fn drop(&mut self) {
        // Closing the daemon connection detaches; the session goes on running
        // in the daemon, which is the whole point of a viewport.
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn the_two_hello_bytes_are_distinct_and_printable() {
        // Printable so a packet capture or a hexdump during a support call
        // reads as `P` or `S` rather than as a control byte someone has to
        // look up.
        assert_ne!(HELLO_PAIR, HELLO_SESSION);
        assert!(HELLO_PAIR.is_ascii_graphic() && HELLO_SESSION.is_ascii_graphic());
        assert!(HELLO_COMPUTER.is_ascii_graphic() && ![HELLO_PAIR, HELLO_SESSION].contains(&HELLO_COMPUTER));
    }

    /// Dial `state`'s `connection` with the computer-pairing hello and XX
    /// message 1, from a loopback socket that is marked as a relay splice
    /// when `relay`, and say whether message 2 (this computer's static key)
    /// came back.
    fn computer_hello_answered(state: &Arc<State>, dir: &Path, relay: bool) -> bool {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let mut client = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        let source = client.local_addr().expect("local").port();
        if relay {
            state.relay_sources.arm(source);
        }
        let (st, agentd) = (state.clone(), dir.join("no-agentd.sock"));
        let worker = std::thread::spawn(move || connection(server, st, agentd));
        client.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
        let mut hs = Handshake::computer_pairing_initiator(&[7u8; 32], rime_remote_core::REMOTE_PROTOCOL_VERSION)
            .expect("initiator");
        let m1 = hs.write(b"").expect("m1");
        client.write_all(&[HELLO_COMPUTER]).expect("hello");
        crate::net::write_message(&mut client, &m1).expect("m1 sent");
        let answered = crate::net::read_message(&mut client).is_ok_and(|m2| hs.read(&m2).is_ok());
        drop(client);
        worker.join().expect("connection thread");
        if relay {
            state.relay_sources.disarm(source);
        }
        answered
    }

    #[test]
    fn a_computer_pairing_over_the_relay_is_refused_before_this_computer_shows_its_key() {
        let (state, dir) = a_state("computer-relay");
        let (_id, _offer, _hear) = crate::computers::open_screen(&state.showing, rime_remote_core::now_ms());
        assert!(
            !computer_hello_answered(&state, &dir, true),
            "a pairing over the relay got message 2, and with it this computer's key"
        );
        assert!(
            crate::computers::has_code(&state.showing, rime_remote_core::now_ms()),
            "a refused relay pairing spent the code"
        );
        // The control arm: the same dial over the LAN is answered, so the
        // refusal above is the path and not a broken dial.
        assert!(computer_hello_answered(&state, &dir, false), "a LAN pairing got no message 2");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_unpaired_and_a_revoked_device_are_refused_with_the_same_words() {
        // A device that could tell them apart would have an oracle for which
        // keys this machine has ever seen.
        let unpaired = ServeError::NotPaired.to_string();
        assert!(!unpaired.contains("revoked"), "{unpaired}");
        assert!(!unpaired.contains("never"), "{unpaired}");
    }

    #[test]
    fn an_attach_on_the_control_channel_is_refused_rather_than_wedging_the_proxy() {
        // It would leave the daemon connection half in PTY mode while this
        // side still read reply lines, and the terminal would go nowhere.
        // Refused without a daemon being involved at all, which is why the
        // socket path here is nonsense and the test still passes.
        let reply = control(
            std::path::Path::new("/nonexistent/agentd.sock"),
            "pixel-8",
            br#"{"cmd":"attach","id":1,"cols":80,"rows":24}"#,
        );
        let v: serde_json::Value = serde_json::from_slice(&reply).expect("a JSON reply");
        assert_eq!(v["kind"], "bad_request", "{v}");
        assert!(
            v["message"].as_str().unwrap_or_default().contains("open a channel"),
            "{v}"
        );
    }

    #[test]
    fn a_control_failure_comes_back_as_a_response_the_device_can_render() {
        // Not a dropped connection. A phone that loses its terminal because
        // the runtime restarted is worse off than one that is told so.
        let reply = control(
            std::path::Path::new("/nonexistent/agentd.sock"),
            "pixel-8",
            br#"{"cmd":"list"}"#,
        );
        let v: serde_json::Value = serde_json::from_slice(&reply).expect("a JSON reply");
        assert_eq!(v["reply"], "error");
        assert!(
            v["message"].as_str().unwrap_or_default().contains("rime agent enable"),
            "{v}"
        );
        // And it is one line, so it cannot desynchronise the frame it rides in.
        assert!(!reply.contains(&b'\n'));
    }

    pub(crate) fn a_state(tag: &str) -> (Arc<State>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "rime-serve-state-{}-{tag}-{}",
            std::process::id(),
            rime_remote_core::now_ms()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let identity = rime_remote_core::identity::Identity::generate().expect("identity");
        let state = State::new(
            identity,
            "test-machine".into(),
            47717,
            std::net::IpAddr::from([0u8; 16]),
            None,
            dir.join("devices.json"),
            PING_INTERVAL,
        )
        .expect("state");
        // Never the machine's own nexd: what a test sees must not depend on
        // whether one happens to be running.
        state.set_nexd(dir.join("no-nexd.sock"));
        (state, dir)
    }

    #[test]
    fn remote_hello_is_answered_here_with_what_this_build_does() {
        let (state, dir) = a_state("hello");
        let reply = answered_here(&state, "pixel-8", Scope::Full, 0, br#"{"cmd":"remote_hello"}"#)
            .expect("remote_hello reached the daemon");
        let v: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(v["reply"], "remote", "{v}");
        assert_eq!(v["version"], rime_remote_core::REMOTE_PROTOCOL_VERSION, "{v}");
        // Exactly the contract's names, in its order: a phone keys behaviour
        // off these strings, so a typo is a feature it silently never uses.
        assert_eq!(
            v["features"],
            serde_json::json!(["nodelay", "control_worker", "mux_attach", "liveness",
                               "machine_status", "machine_lock", "media",
                               "device_events", "phone_report", "take_offer", "phone_focus", "phone_clipboard",
                               "phone_notifications", "phone_notification_reply",
                               "phone_media", "phone_media_control", "phone_messages", "phone_calls",
                               "phone_photos", "companion_link", "computer_introductions"]),
            "{v}"
        );
        assert_eq!(v["scope"], "full", "{v}");
        // The pairing code's list, with the port this listener has — and
        // `host:port` a phone can split, so an IPv6 entry is bracketed.
        let lan: Vec<String> = serde_json::from_value(v["lan"].clone()).expect("strings");
        assert_eq!(lan, state.lan_addresses());
        for a in &lan {
            let addr: std::net::SocketAddr = a.parse().unwrap_or_else(|e| panic!("{a}: {e}"));
            assert_eq!(addr.port(), 47717, "{a}");
        }
        // One line, so it cannot desynchronise the frame it rides in.
        assert!(!reply.contains(&b'\n'));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_the_reserved_verbs_are_answered_here_and_everything_else_is_forwarded() {
        // `wire.rs` says a control frame carries the daemon's protocol
        // verbatim. A verb this service swallowed would be one the phone
        // silently lost, so the set it takes is pinned: this file's own and
        // push registration's, and nothing that merely resembles them.
        let (state, dir) = a_state("verbs");
        for cmd in VERBS
            .iter()
            .chain(crate::push::VERBS.iter())
            .chain(crate::nex::VERBS.iter())
            .chain(crate::machine::VERBS.iter())
            .chain(crate::phone::VERBS.iter())
            .chain(crate::mirror::VERBS.iter())
            .chain(crate::media::VERBS.iter())
            .chain(crate::messages::VERBS.iter())
            .chain(crate::calls::VERBS.iter())
            .chain(crate::photos::VERBS.iter())
            .chain(crate::companion::VERBS.iter())
        {
            let line = format!(r#"{{"cmd":"{cmd}"}}"#);
            assert!(
                answered_here(&state, "pixel-8", Scope::Full, 0, line.as_bytes()).is_some(),
                "{cmd} was forwarded"
            );
        }
        for line in [
            br#"{"cmd":"hello"}"#.to_vec(),
            br#"{"cmd":"list"}"#.to_vec(),
            br#"{"cmd":"remote"}"#.to_vec(),
            br#"{"cmd":"remote_hello_again"}"#.to_vec(),
            // A paired phone may not ring anything: ring is the control
            // socket's, for somebody at this machine.
            br#"{"cmd":"ring"}"#.to_vec(),
            br#"{"cmd":"device_events"}"#.to_vec(),
            // Nor send anything to itself or another phone: `send` is the
            // control socket's too, and `take_offer` is a channel, not a line.
            br#"{"cmd":"send"}"#.to_vec(),
            br#"{"cmd":"take_offer"}"#.to_vec(),
            br#"{"remote_hello":true}"#.to_vec(),
            b"remote_hello".to_vec(),
        ] {
            assert_eq!(
                answered_here(&state, "pixel-8", Scope::Full, 0, &line),
                None,
                "{} never reached the daemon",
                String::from_utf8_lossy(&line)
            );
        }
        assert_eq!(VERBS, ["remote_hello"]);
        assert_eq!(crate::nex::VERBS, ["nex", "nex_events"]);
        assert_eq!(crate::machine::VERBS, ["machine_status", "machine_lock", "media", "media_control", "volume"]);
        assert_eq!(crate::phone::VERBS, ["phone_report", "ring_ack", "offer_ack", "offer_done"]);
        assert_eq!(crate::mirror::VERBS, ["phone_notification", "phone_notification_removed"]);
        assert_eq!(crate::media::VERBS, ["phone_media"]);
        assert_eq!(crate::messages::VERBS, ["messages_reply", "messages_changed", "messages_sent"]);
        assert_eq!(crate::calls::VERBS, ["call_state", "call_history_reply", "call_done"]);
        assert_eq!(crate::photos::VERBS, ["photos_reply", "photos_fetch_reply"]);
        assert_eq!(crate::companion::VERBS, ["companion_register", "companion_forget"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn nex_is_a_feature_only_while_nexd_answers() {
        let (state, dir) = a_state("nexfeature");
        let hello = |s: &State| -> serde_json::Value { serde_json::from_slice(&remote_hello(s, Scope::Full)).expect("json") };
        assert!(!hello(&state)["features"].as_array().unwrap().iter().any(|f| f == "nex"));
        let sock = dir.join("nexd.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
        state.set_nexd(sock);
        assert_eq!(hello(&state)["features"].as_array().unwrap().last().unwrap(), "nex");
        drop(listener);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_nex_channel_drops_what_the_device_writes() {
        let (a, mut b) = std::os::unix::net::UnixStream::pair().expect("pair");
        let ch = DaemonChannel {
            stream: a,
            read_only: true,
            _slot: None,
        };
        ch.write(b"{\"method\":\"system.shutdown\"}\n").expect("dropped, not failed");
        drop(ch); // shuts the stream down: b now reads end-of-file
        let mut got = Vec::new();
        b.read_to_end(&mut got).expect("read");
        assert!(got.is_empty(), "{}", String::from_utf8_lossy(&got));
    }

    #[test]
    fn three_unanswered_pings_are_silence_and_any_answer_ends_it() {
        let long_ago = |o: &mut Outstanding| {
            o.heard = Instant::now()
                .checked_sub(Duration::from_secs(60))
                .expect("a clock a minute old");
        };
        let mut o = Outstanding::new();
        long_ago(&mut o);
        o.sent(1);
        o.sent(2);
        assert_eq!(o.silent(), None, "two misses closed the connection");
        o.sent(3);
        assert!(o.silent().is_some(), "three misses did not");

        // One answer — even to the newest ping only — is a live link, and
        // the older misses go with it rather than lingering towards the next
        // three.
        assert!(o.answered(3).is_some());
        assert_eq!(o.silent(), None);
        assert_eq!(o.answered(2), None, "an older token survived a newer answer");
        o.sent(4);
        o.sent(5);
        assert_eq!(o.silent(), None);

        // A token this end never sent is not a sign of life.
        let mut o = Outstanding::new();
        long_ago(&mut o);
        for t in 1..=3 {
            o.sent(t);
        }
        assert_eq!(o.answered(0xdead_beef), None);
        assert!(o.silent().is_some(), "an invented pong kept a silent device alive");

        // And the floor: at a test's 100 ms interval three misses come in a
        // fraction of a second, which is not a dead link.
        let mut o = Outstanding::new();
        for t in 1..=3 {
            o.sent(t);
        }
        assert_eq!(o.silent(), None, "closed inside the floor");
    }

    #[test]
    fn a_write_that_makes_no_progress_is_given_the_same_window_as_the_pings() {
        assert_eq!(write_deadline(PING_INTERVAL), Duration::from_secs(45));
        assert_eq!(write_deadline(Duration::from_millis(100)), LIVENESS_FLOOR);
    }

    #[test]
    fn a_channel_behind_the_worker_takes_its_frames_through_the_worker_until_it_catches_up() {
        // The reordering this prevents: a `Data` written straight to a PTY
        // while an earlier one for the same channel was still queued behind
        // its `Open`.
        let mut c = Channels::default();
        assert!(matches!(c.route(7), Route::Nowhere), "a channel nobody opened");
        c.queue(7); // the Open
        assert!(matches!(c.route(7), Route::Worker));
        assert!(matches!(c.route(7), Route::Worker));
        // The worker finishes the Open and publishes the channel...
        let (ours, _theirs) = std::os::unix::net::UnixStream::pair().expect("pair");
        c.open.insert(7, Arc::new(DaemonChannel { stream: ours, read_only: false, _slot: None }));
        c.done(7);
        // ...but two Data frames are still queued behind it, so a third must
        // queue too rather than overtake them.
        assert!(matches!(c.route(7), Route::Worker));
        c.done(7);
        c.done(7);
        c.done(7);
        assert!(matches!(c.route(7), Route::Direct(_)), "caught up and still queued");
        // Another channel is not held up by this one.
        c.queue(8);
        assert!(matches!(c.route(7), Route::Direct(_)));
        // And `done` past zero cannot wrap into a channel stuck behind for ever.
        c.done(9);
        assert!(!c.is_behind(9));
    }

    /// Every verb this process answers itself, from every module's own list.
    /// Every control verb this process answers itself, from each module's
    /// own list. A NEW MODULE that answers verbs in `answered_here` must add
    /// its `VERBS` in three places: here, in the chain in
    /// `only_the_reserved_verbs_are_answered_here_and_everything_else_is_forwarded`,
    /// and as an allowed or refused entry in
    /// `every_verb_this_process_answers_has_a_companion_decision_written_down`
    /// (and in `companion::ALLOWED_VERBS` if a companion session may use it).
    /// A module that adds a CHANNEL adds its request name to the candidates in
    /// `every_channel_a_router_claims_is_decided_and_only_the_phones_open`.
    fn every_reserved_verb() -> Vec<&'static str> {
        VERBS
            .iter()
            .chain(crate::push::VERBS.iter())
            .chain(crate::nex::VERBS.iter())
            .chain(crate::machine::VERBS.iter())
            .chain(crate::phone::VERBS.iter())
            .chain(crate::mirror::VERBS.iter())
            .chain(crate::media::VERBS.iter())
            .chain(crate::messages::VERBS.iter())
            .chain(crate::calls::VERBS.iter())
            .chain(crate::photos::VERBS.iter())
            .chain(crate::companion::VERBS.iter())
            .chain(crate::intro::VERBS.iter())
            .copied()
            .collect()
    }

    /// `rime-agentd` verbs a phone sends, and a few lines that are not verbs
    /// at all. None of them may pass on a companion session.
    const AGENTD_AND_GARBAGE: [&str; 24] = [
        r#"{"cmd":"hello"}"#,
        r#"{"cmd":"list"}"#,
        r#"{"cmd":"info","id":1}"#,
        r#"{"cmd":"input","id":1,"text":"rm -rf ~"}"#,
        r#"{"cmd":"inject","id":1,"source":"x"}"#,
        r#"{"cmd":"run","agent":"claude"}"#,
        r#"{"cmd":"signal","id":1,"signal":"KILL"}"#,
        r#"{"cmd":"peek","id":1}"#,
        r#"{"cmd":"clipboard"}"#,
        r#"{"cmd":"clipboard_write","text":"x"}"#,
        r#"{"cmd":"privilege_request","verb":"install","args":["x"],"reason":"y"}"#,
        r#"{"cmd":"decide","id":1,"decision":"allow"}"#,
        r#"{"cmd":"requests"}"#,
        r#"{"cmd":"declare_origin","origin":"local-terminal","actor":"x"}"#,
        r#"{"cmd":"attach","id":1,"cols":80,"rows":24}"#,
        r#"{"cmd":"receive","id":1,"name":"a","len":1}"#,
        r#"{"cmd":"save_file","name":"a","len":1}"#,
        r#"{"cmd":"device_events"}"#,
        r#"{"cmd":"phone_report_and_more"}"#,
        r#"{"cmd":1}"#,
        r#"{"cmd":null}"#,
        r#"{"remote_hello":true}"#,
        "remote_hello",
        "",
    ];

    #[test]
    fn every_verb_this_process_answers_has_a_companion_decision_written_down() {
        // The allow list, and everything refused, spelled out here rather than
        // read back from `companion`: a verb added to any module's list is not
        // in either set below, and this test fails until somebody decides.
        let allowed = [
            "remote_hello",
            "phone_report",
            "ring_ack",
            "offer_ack",
            "offer_done",
            "phone_notification",
            "phone_notification_removed",
            "phone_media",
            "messages_reply",
            "messages_changed",
            // A call rings on a locked phone, and its own lock screen lets the
            // person answer or decline it; a dial is refused by the computer
            // and the phone on that lane (calls.rs, contract §2.12).
            "call_state",
            "call_done",
            "call_history_reply",
        ];
        let refused = [
            "messages_sent",
            "push_register",
            "push_unregister",
            "nex",
            "nex_events",
            "machine_status",
            "machine_lock",
            "media",
            "media_control",
            "volume",
            "companion_register",
            "companion_forget",
            // Photos are listed and saved from an unlocked phone only: the
            // computer asks nothing of a companion session and takes no
            // answer from one (photos.rs, contract §2.13).
            "photos_reply",
            "photos_fetch_reply",
            // Introducing computers is a Full, unlocked phone's (intro.rs).
            "computer_card",
            "computer_introduce",
        ];
        let all = every_reserved_verb();
        for v in &all {
            let a = allowed.contains(v);
            let r = refused.contains(v);
            assert!(a ^ r, "{v} needs exactly one companion decision in this test");
            assert_eq!(crate::companion::allowed(Scope::Companion, Some(v)), a, "{v}");
            assert!(crate::companion::allowed(Scope::Full, Some(v)), "a full session lost {v}");
        }
        for v in allowed.iter().chain(refused.iter()) {
            assert!(all.contains(v), "{v} is decided here but nothing answers it");
        }
        assert_eq!(crate::companion::ALLOWED_VERBS, allowed);
        assert!(crate::companion::CLIPBOARD_VERBS.is_empty());
        for line in AGENTD_AND_GARBAGE {
            let cmd = crate::companion::verb_of(line.as_bytes());
            assert!(!crate::companion::allowed(Scope::Companion, cmd.as_deref()), "{line}");
        }
        assert!(!crate::companion::allowed(Scope::Companion, None));
        assert!(crate::companion::allowed(Scope::Full, None), "a full session's garbage still goes to the daemon");
    }

    /// A stand-in `rime-agentd` socket that counts the connections it gets and
    /// answers each like the daemon: `ok` to the declaration, then one reply.
    fn counting_agentd(dir: &Path) -> (PathBuf, Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::BufRead;
        let sock = dir.join("agentd.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&count);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                seen.fetch_add(1, Ordering::SeqCst);
                let mut w = stream.try_clone().expect("clone");
                let mut r = std::io::BufReader::new(stream);
                let mut line = String::new();
                let _ = r.read_line(&mut line);
                let _ = writeln!(w, r#"{{"reply":"ok"}}"#);
                line.clear();
                let _ = r.read_line(&mut line);
                let _ = writeln!(w, r#"{{"reply":"sessions","sessions":[]}}"#);
            }
        });
        (sock, count)
    }

    /// A stand-in `rime-agentd` that keeps every line of every connection
    /// and answers `ok` to the declaration, then `sessions` to a list and an
    /// error to anything else.
    fn recording_agentd(dir: &Path) -> (PathBuf, Arc<Mutex<Vec<Vec<String>>>>) {
        use std::io::BufRead;
        let sock = dir.join("recording-agentd.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
        let seen: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
        let kept = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let n = {
                    let mut k = kept.lock().expect("lock");
                    k.push(Vec::new());
                    k.len() - 1
                };
                let mut w = stream.try_clone().expect("clone");
                let mut r = std::io::BufReader::new(stream);
                let mut line = String::new();
                while r.read_line(&mut line).is_ok_and(|n| n > 0) {
                    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap_or_default();
                    kept.lock().expect("lock")[n].push(line.trim().to_string());
                    let answer = match v["cmd"].as_str() {
                        Some("declare_origin") => r#"{"reply":"ok"}"#,
                        Some("list") => r#"{"reply":"sessions","sessions":[]}"#,
                        _ => r#"{"reply":"error","kind":"no_such_session","message":"no session 9 \u001b]52;c;?\u0007"}"#,
                    };
                    let _ = writeln!(w, "{answer}");
                    line.clear();
                }
            }
        });
        (sock, seen)
    }

    #[test]
    fn a_granted_peer_reaches_rime_agentd_only_with_requests_built_here_as_computer_origin() {
        let (state, dir) = a_state("peer-agents");
        let (agentd, seen) = recording_agentd(&dir);
        let peer = {
            let mut store = state.devices().expect("store");
            let c = store.pair_computer(&[62; 32], &state.identity.public_bytes(), "desk", vec![], None, 1).expect("pair");
            state.save_devices(&store).expect("save");
            c
        };
        let ask = |line: &str| -> serde_json::Value {
            serde_json::from_slice(&reply_to(&state, &agentd, &peer.id, Scope::Peer, 0, line.as_bytes())).expect("json")
        };
        let grant = |g| {
            let mut store = state.devices().expect("store");
            store.set_computer_agents(&peer.id, g).expect("grant");
            state.save_devices(&store).expect("save");
        };
        // No grant: refused like any verb off the list, no feature, no hint.
        let hello = ask(r#"{"cmd":"remote_hello"}"#);
        assert!(hello.get("agents").is_none(), "{hello}");
        assert!(!hello["features"].as_array().expect("features").iter().any(|f| f == crate::agents::FEATURE));
        for line in [r#"{"cmd":"peer_agents"}"#, r#"{"cmd":"peer_agent_peek","id":1}"#] {
            assert_eq!(ask(line)["kind"], crate::computers::REFUSAL, "{line}");
        }
        assert!(seen.lock().expect("lock").is_empty(), "an ungranted peer reached rime-agentd");
        // View: the hint, and a list that reaches rime-agentd as the
        // computer, with a line built here (the peer's extra fields are gone).
        grant(Some(rime_remote_core::device::AgentGrant::View));
        let hello = ask(r#"{"cmd":"remote_hello"}"#);
        assert_eq!(hello["agents"], "view", "{hello}");
        assert!(hello["features"].as_array().expect("features").iter().any(|f| f == crate::agents::FEATURE));
        let v = ask(r#"{"cmd":"peer_agents","all":true,"cmd2":"decide"}"#);
        assert_eq!((v["reply"].as_str(), v["sessions"].clone()), (Some("peer_agents"), serde_json::json!([])), "{v}");
        let v = ask(r#"{"cmd":"peer_agent_peek","id":9,"bytes":10,"decision":"approve"}"#);
        // rime-agentd's text is not passed on; its kind is.
        assert_eq!((v["kind"].as_str(), v["message"].as_str()), (Some("no_such_session"), Some("the agent runtime refused it")), "{v}");
        assert_eq!(ask(r#"{"cmd":"peer_agent_resize","id":9,"cols":80,"rows":24}"#)["kind"], crate::computers::REFUSAL);
        // Control: a resize too.
        grant(Some(rime_remote_core::device::AgentGrant::Control));
        assert_eq!(ask(r#"{"cmd":"peer_agent_resize","id":9,"cols":80,"rows":24}"#)["kind"], "no_such_session");
        // What reached rime-agentd, exactly.
        let lines = seen.lock().expect("lock").clone();
        assert_eq!(lines.len(), 3, "{lines:?}");
        for conn in &lines {
            let first: serde_json::Value = serde_json::from_str(&conn[0]).expect("json");
            assert_eq!(first["cmd"], "declare_origin");
            assert_eq!(first["origin"], "claude-remote-control");
            assert_eq!(first["actor"], format!("computer:{}", peer.id));
            assert_eq!(conn.len(), 2, "{conn:?}");
        }
        let built: Vec<serde_json::Value> = lines.iter().map(|c| serde_json::from_str(&c[1]).expect("json")).collect();
        assert_eq!(built[0], serde_json::json!({"cmd": "list"}));
        assert_eq!(built[1], serde_json::json!({"cmd": "peek", "id": 9, "bytes": 10}));
        assert_eq!(built[2], serde_json::json!({"cmd": "resize", "id": 9, "cols": 80, "rows": 24}));
        // Approvals are never reachable: those verbs are refused at any grant.
        for line in [r#"{"cmd":"decide","id":1,"decision":"approve"}"#, r#"{"cmd":"requests"}"#, r#"{"cmd":"input","id":1,"data":"x"}"#] {
            assert_eq!(ask(line)["kind"], crate::computers::REFUSAL, "{line}");
        }
        assert_eq!(seen.lock().expect("lock").len(), 3, "an approval verb reached rime-agentd");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn every_channel_a_router_claims_is_decided_and_only_the_phones_open() {
        // Candidates: every name a module answers or routes, `rime-agentd`'s
        // channels, near misses and garbage. The decision is then read off
        // the routers' OWN predicates, not a list kept beside them.
        let mut names: Vec<&str> = every_reserved_verb();
        names.extend(crate::companion::ALLOWED_CHANNELS);
        names.extend([crate::phone::CHANNEL, crate::phone::TAKE, crate::photos::UPLOAD]);
        names.extend(["attach", "receive", "save_file", "device_events_and_more", "take_offer2", "DEVICE_EVENTS"]);
        let mut requests: Vec<Vec<u8>> =
            names.iter().map(|n| format!(r#"{{"cmd":"{n}","id":1,"cols":80,"rows":24}}"#).into_bytes()).collect();
        requests.extend(AGENTD_AND_GARBAGE.iter().map(|l| l.as_bytes().to_vec()));
        let mut opened = Vec::new();
        for r in &requests {
            let shown = String::from_utf8_lossy(r);
            let phone = crate::phone::is_channel(r) || crate::phone::is_take(r);
            let nex = crate::nex::is_channel(r);
            let upload = crate::photos::is_upload(r);
            assert!([phone, nex, upload].iter().filter(|c| **c).count() <= 1, "two routers claim {shown}");
            if upload {
                assert!(!may_open(Scope::Companion, r), "a photo upload opens on a companion link: {shown}");
            }
            assert_eq!(may_open(Scope::Companion, r), phone, "{shown}");
            assert_eq!(crate::companion::channel_allowed(Scope::Companion, r), phone, "{shown}");
            if nex {
                assert!(!may_open(Scope::Companion, r), "a nex channel opens on a companion link: {shown}");
            }
            assert!(may_open(Scope::Full, r), "a full session lost {shown}");
            if phone {
                opened.push(crate::companion::verb_of(r).expect("a verb"));
            }
        }
        opened.sort();
        opened.dedup();
        assert_eq!(opened, ["device_events", "take_offer"], "the phone routers claim a channel nobody decided");
        for v in crate::nex::VERBS {
            let r = format!(r#"{{"cmd":"{v}"}}"#);
            assert!(crate::nex::is_channel(r.as_bytes()) && !may_open(Scope::Companion, r.as_bytes()), "{v}");
        }
    }

    #[test]
    fn a_connection_whose_key_stopped_opening_it_before_it_was_listed_is_not_admitted() {
        let (state, _dir) = a_state("admit");
        let key = |n: u8| rime_remote_core::b64_encode(&[n; 32]);
        let id = {
            let mut store = state.devices().expect("store");
            let d = store.pair(&key(1), "phone", 1, false).expect("pair");
            store.set_companion(&d.id, &key(2)).expect("companion");
            d.id
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let mut held = Vec::new();
        let mut live = |scope: Scope| {
            held.push(TcpStream::connect(addr).expect("connect"));
            let (socket, peer) = listener.accept().expect("accept");
            Live {
                device_id: id.clone(),
                peer: crate::state::canonical(Some(peer)),
                socket,
                device_name: "phone".into(),
                path: rime_remote_core::rendezvous::Path::Lan,
                since_ms: 0,
                rtt_ms: Arc::new(Mutex::new(None)),
                scope,
            }
        };
        let listed = |state: &State| state.connections().len();

        // Still opens: listed.
        assert!(admit(&state, live(Scope::Full), &key(1)));
        assert!(admit(&state, live(Scope::Companion), &key(2)));
        assert_eq!(listed(&state), 2);
        // A key presented with the wrong scope (the store says the other one).
        assert!(!admit(&state, live(Scope::Companion), &key(1)));
        assert_eq!(listed(&state), 2, "a refused connection stayed on the list");

        // The companion key is replaced after the handshake looked it up and
        // before the connection was listed: refused, and not listed.
        state.devices().expect("store").set_companion(&id, &key(3)).expect("replace");
        assert!(!admit(&state, live(Scope::Companion), &key(2)), "a replaced key was admitted");
        // Forgotten in that window.
        state.devices().expect("store").forget_companion(&id);
        assert!(!admit(&state, live(Scope::Companion), &key(3)), "a forgotten key was admitted");
        // Revoked in that window: neither key.
        state.devices().expect("store").revoke(&id, 2).expect("revoke");
        assert!(!admit(&state, live(Scope::Full), &key(1)), "a revoked device was admitted");
        assert_eq!(listed(&state), 2, "a refused connection stayed on the list");
    }

    #[test]
    fn a_companion_line_is_refused_before_routing_and_never_reaches_rime_agentd() {
        let (state, dir) = a_state("companion-gate");
        let (agentd, count) = counting_agentd(&dir);
        let refused = every_reserved_verb()
            .into_iter()
            .filter(|v| !crate::companion::ALLOWED_VERBS.contains(v))
            .map(|v| format!(r#"{{"cmd":"{v}"}}"#))
            .chain(AGENTD_AND_GARBAGE.iter().map(|l| l.to_string()))
            .collect::<Vec<_>>();
        for line in &refused {
            let r = reply_to(&state, &agentd, "pixel-8", Scope::Companion, 0, line.as_bytes());
            let v: serde_json::Value = serde_json::from_slice(&r).expect("json");
            assert_eq!(v["kind"], "not_allowed", "{line}: {v}");
        }
        assert_eq!(count.load(Ordering::SeqCst), 0, "a companion line reached rime-agentd");

        // A companion session's hello says so.
        let r = reply_to(&state, &agentd, "pixel-8", Scope::Companion, 0, br#"{"cmd":"remote_hello"}"#);
        let v: serde_json::Value = serde_json::from_slice(&r).expect("json");
        assert_eq!(v["reply"], "remote", "{v}");
        assert_eq!(v["scope"], "companion", "{v}");

        // The control arm: the same daemon, a full session, is reached, so
        // the zero above is a measurement and not a socket nobody could open.
        let r = reply_to(&state, &agentd, "pixel-8", Scope::Full, 0, br#"{"cmd":"list"}"#);
        let v: serde_json::Value = serde_json::from_slice(&r).expect("json");
        assert_eq!(v["reply"], "sessions", "{v}");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Every control verb `rime remote` sends this daemon's own socket: none
    /// of them may be reached from a peer session either.
    const CONTROL_VERBS: [&str; 13] = [
        "pair",
        "revoke",
        "device_allow",
        "computer_allow",
        "devices",
        "status",
        "computer_show",
        "computer_add",
        "computer_remove",
        "computer_forget",
        "computer_ping",
        "computers",
        "send",
    ];

    #[test]
    fn a_peer_line_is_refused_before_routing_and_never_reaches_rime_agentd() {
        let (state, dir) = a_state("peer-gate");
        let (agentd, count) = counting_agentd(&dir);
        let refused = every_reserved_verb()
            .into_iter()
            .chain(CONTROL_VERBS)
            // A file is a channel, never a control line.
            .chain([crate::files::FILE_VERB, crate::agents::ATTACH_VERB])
            // The agent verbs, from a computer nobody here granted anything.
            .chain(crate::agents::VERBS)
            .filter(|v| !crate::computers::PEER_VERBS.contains(v))
            .map(|v| format!(r#"{{"cmd":"{v}"}}"#))
            .chain(AGENTD_AND_GARBAGE.iter().map(|l| l.to_string()))
            .collect::<Vec<_>>();
        assert!(refused.len() >= 60, "the candidate list shrank: {}", refused.len());
        for line in &refused {
            let r = reply_to(&state, &agentd, "desk", Scope::Peer, 0, line.as_bytes());
            let v: serde_json::Value = serde_json::from_slice(&r).expect("json");
            assert_eq!(v["kind"], crate::computers::REFUSAL, "{line}: {v}");
            assert!(
                v["message"].as_str().unwrap_or_default().contains("between paired computers"),
                "{line} was not answered by the peer list: {v}"
            );
        }
        assert_eq!(count.load(Ordering::SeqCst), 0, "a peer line reached rime-agentd");
        // The two verbs it may send are answered here, in the peer's words.
        let r = reply_to(&state, &agentd, "desk", Scope::Peer, 0, br#"{"cmd":"remote_hello"}"#);
        let v: serde_json::Value = serde_json::from_slice(&r).expect("json");
        assert_eq!((v["reply"].as_str(), v["scope"].as_str()), (Some("remote"), Some("peer")), "{v}");
        assert!(v.get("lan").is_none(), "a peer's hello carries the phone hello's addresses: {v}");
        let r = reply_to(&state, &agentd, "desk", Scope::Peer, 0, br#"{"cmd":"peer_hello"}"#);
        let v: serde_json::Value = serde_json::from_slice(&r).expect("json");
        assert_eq!(v["reply"], "peer_hello", "{v}");
        // The list is exactly these: a verb added to it is a verb the
        // refusals above stop trying, so it is named here and nowhere else.
        assert_eq!(
            crate::computers::PEER_VERBS,
            [
                "remote_hello",
                "peer_hello",
                "peer_tombstones",
                "peer_presence",
                "peer_offer",
                "peer_clipboard",
                "peer_handoff"
            ]
        );
        // And the agent verbs, which answer only as far as a grant goes.
        assert_eq!(crate::agents::VERBS, ["peer_agents", "peer_agent_peek", "peer_agent_resize"]);
        assert_eq!(crate::computers::PEER_CHANNELS, ["peer_file", "peer_agent_attach"]);
        // A clipboard copy from a computer that is not paired here, and from
        // one that is but whose copies are not taken, never reaches
        // rime-agentd; nor does one while the screen is locked or its lock
        // unknown (the unit-test machine stub never says unlocked).
        let clip = |text: &str, seq: u64| {
            serde_json::json!({"cmd": "peer_clipboard", "text": text, "hash": crate::share::hash_hex(text),
                               "origin": rime_remote_core::b64_encode(&[61; 32]), "seq": seq})
            .to_string()
        };
        let v: serde_json::Value =
            serde_json::from_slice(&reply_to(&state, &agentd, "desk", Scope::Peer, 0, clip("x", 1).as_bytes())).expect("json");
        assert_eq!(v["kind"], "not_shared", "{v}");
        let peer = {
            let mut store = state.devices().expect("store");
            let c = store.pair_computer(&[61; 32], &state.identity.public_bytes(), "desk", vec![], None, 1).expect("pair");
            state.save_devices(&store).expect("save");
            c
        };
        let v: serde_json::Value =
            serde_json::from_slice(&reply_to(&state, &agentd, &peer.id, Scope::Peer, 0, clip("x", 2).as_bytes())).expect("json");
        assert_eq!(v["kind"], "not_shared", "{v}");
        {
            let mut store = state.devices().expect("store");
            store.set_computer_clipboard(&peer.id, false, true).expect("on");
            state.save_devices(&store).expect("save");
        }
        let v: serde_json::Value =
            serde_json::from_slice(&reply_to(&state, &agentd, &peer.id, Scope::Peer, 0, clip("x", 3).as_bytes())).expect("json");
        assert_eq!((v["applied"].clone(), v["why"].clone()), (serde_json::json!(false), serde_json::json!("locked")), "{v}");
        for (line, kind) in [
            (&br#"{"cmd":"peer_tombstones","tombstones":[]}"#[..], "peer_tombstones"),
            (br#"{"cmd":"peer_presence","presence":{"name":"desk"}}"#, "peer_presence"),
        ] {
            let v: serde_json::Value = serde_json::from_slice(&reply_to(&state, &agentd, "desk", Scope::Peer, 0, line)).expect("json");
            assert_eq!(v["reply"], kind, "{v}");
        }
        assert_eq!(count.load(Ordering::SeqCst), 0);
        // The control arm: a full session on the same daemon is reached.
        let r = reply_to(&state, &agentd, "pixel-8", Scope::Full, 0, br#"{"cmd":"list"}"#);
        let v: serde_json::Value = serde_json::from_slice(&r).expect("json");
        assert_eq!(v["reply"], "sessions", "{v}");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_peer_opens_only_a_file_and_the_companion_list_refuses_it_everything() {
        let mut names: Vec<&str> = every_reserved_verb();
        names.extend(crate::companion::ALLOWED_CHANNELS);
        names.extend([crate::phone::CHANNEL, crate::phone::TAKE, crate::photos::UPLOAD, "attach", "receive", "save_file"]);
        names.extend([crate::files::FILE_VERB, "peer_file2", "PEER_FILE", "peer_clipboard", "peer_handoff"]);
        names.extend([crate::agents::ATTACH_VERB, "peer_agent_attach2", "peer_agents", "peer_agent_peek"]);
        let mut requests: Vec<Vec<u8>> =
            names.iter().map(|n| format!(r#"{{"cmd":"{n}","id":1,"cols":80,"rows":24}}"#).into_bytes()).collect();
        requests.extend(AGENTD_AND_GARBAGE.iter().map(|l| l.as_bytes().to_vec()));
        let mut opened = Vec::new();
        for r in &requests {
            let shown = String::from_utf8_lossy(r);
            if may_open(Scope::Peer, r) {
                opened.push(crate::companion::verb_of(r).expect("a verb"));
            }
            // The companion list never lets a peer through: a peer's one
            // channel is decided by `may_open` and routed to files.rs alone.
            assert!(!crate::companion::channel_allowed(Scope::Peer, r), "{shown}");
            assert!(!crate::companion::allowed(Scope::Peer, crate::companion::verb_of(r).as_deref()), "{shown}");
        }
        opened.sort();
        opened.dedup();
        // The attach channel opens only as far as the grant goes, which
        // agents.rs checks before anything else (its own tests).
        assert_eq!(opened, [crate::agents::ATTACH_VERB, crate::files::FILE_VERB], "a peer opens a channel nobody decided");
        assert!(!crate::companion::allowed(Scope::Peer, None));
        // A file is a channel on a peer link only: a phone's session and a
        // companion link never reach files.rs.
        let file = br#"{"cmd":"peer_file","name":"a.txt","len":1,"sha256":"00"}"#;
        assert!(!may_open(Scope::Companion, file));
        assert!(!crate::phone::is_channel(file) && !crate::phone::is_take(file) && !crate::nex::is_channel(file));
    }

    #[test]
    fn a_session_presenting_this_machines_own_key_is_refused_whatever_the_store_says() {
        let (state, dir) = a_state("own-key");
        // The worst case: the store accepts the key (a copied home's
        // devices.json, or a phone paired with it). Refused anyway.
        state
            .devices()
            .expect("store")
            .pair(&state.identity.public_key(), "copied", 1, false)
            .expect("pair");
        assert!(refuses_own_key(&state, &state.identity.public_bytes()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let served = Arc::clone(&state);
        let agentd = dir.join("agentd.sock");
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().expect("accept");
            socket.set_read_timeout(Some(Duration::from_secs(5))).ok();
            connection(socket, served, agentd);
        });
        let mut c = TcpStream::connect(addr).expect("connect");
        c.set_read_timeout(Some(Duration::from_secs(5))).ok();
        c.write_all(&[HELLO_SESSION]).expect("hello");
        let secret = state.identity.secret_bytes_for_handshake();
        let mut hs = Handshake::session_initiator(&secret, &state.identity.public_bytes(), rime_remote_core::REMOTE_PROTOCOL_VERSION)
            .expect("hs");
        crate::net::write_message(&mut c, &hs.write(b"").expect("m1")).expect("send");
        let mut len = [0u8; 4];
        assert!(c.read_exact(&mut len).is_err(), "a copied home got an answer");
        server.join().expect("server");
        assert!(state.connections().is_empty(), "a copied home was listed");
        let _ = std::fs::remove_dir_all(dir);
    }

}
