//! The local control socket: what `rime remote` says to this service.
//!
//! Newline-delimited JSON on a Unix socket in `$XDG_RUNTIME_DIR`, exactly
//! like `rime-agentd`'s, because a second framing convention on a second
//! socket in the same runtime directory would be two things to learn for no
//! gain.
//!
//! ## The one rule this socket enforces by itself
//!
//! **Only a human at this machine may open a pairing offer.** P1-051's fourth
//! criterion is "pairing cannot be completed silently by an agent", and this
//! is the half of it that a policy can enforce; the other half is that an
//! offer expires on its own and is single-use, which lives in
//! `rime_remote_core::pairing`.
//!
//! The check is the same one `rime-agentd`'s `privilege::decide` uses for
//! approving a root operation: [`rime_agent_core::origin::observe_pid`] on
//! the peer's own pid, and a refusal unless the answer is one of §7's local
//! origins. An agent inside a managed session is under `user@N.service` and
//! classifies as `scheduled-job`; a person in a terminal is under
//! `session-N.scope` and classifies as `local-terminal`. Neither can move
//! itself between the two.
//!
//! `send` is gated the same way: the phone takes what is sent without asking,
//! so an agent that could send would choose what a person pastes on their
//! phone (contract §2.6).
//!
//! `messages` and `messages_send` (contract §2.11) are gated the same way,
//! reading included: a phone's SMS carry one-time codes, so an agent that
//! could read them could sign in as the owner. Rime Shell classifies as
//! `rime-shell` and still may.
//!
//! `focus` (contract §2.9) is not gated that way either, for the same reason
//! `ring` is not: it changes how a phone of the owner's interrupts them, which
//! the owner can change back on the phone, and the Shell sends it every time
//! the person toggles Do Not Disturb, from a process the origin rule cannot
//! always classify. It is still this account's alone, and a paired phone has
//! no verb that sends it.
//!
//! Listing and revoking are *not* gated that way. Revoking a device is the
//! safe direction — an agent that revokes the owner's phone is a nuisance,
//! not an escalation, and the owner is the only one who can pair it back.
//! Refusing a revoke from a non-local caller would mean a script cannot
//! respond to a lost phone, which is the case where speed matters most.

use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;

use rime_agent_core::policy::RequestOrigin;
use rime_remote_core::pairing::{Offer, PairingOffer};
use serde::{Deserialize, Serialize};

use crate::state::State;

/// What `rime remote` can ask for.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Is the service up, what is its identity, where can it be reached.
    Status,
    /// Open a pairing offer and return the text that goes in the QR code.
    Pair,
    /// Every device, paired and revoked.
    Devices,
    /// Take a device's access away.
    Revoke { device: String },
    /// Make a paired phone ring, by id or name.
    Ring { device: String },
    /// Set Do Not Disturb on every connected phone that said it follows this
    /// machine's (`can` has "focus").
    Focus { dnd: bool },
    /// Send a phone a file (`kind` "file": the open file rides the request
    /// line as a descriptor, `name` is what the phone is offered) or text
    /// for its clipboard (`kind` "text").
    Send {
        device: String,
        kind: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        text: Option<String>,
    },
    /// A phone's SMS threads, or one thread's messages (contract §2.11).
    /// `device` may be left out when exactly one phone has Messages on.
    Messages {
        #[serde(default)]
        device: Option<String>,
        #[serde(default)]
        thread: Option<u64>,
        #[serde(default)]
        before: Option<u64>,
        #[serde(default)]
        limit: Option<u32>,
    },
    /// Send one SMS from a phone: to a number, or into a thread.
    MessagesSend {
        #[serde(default)]
        device: Option<String>,
        #[serde(default)]
        to: Option<String>,
        #[serde(default)]
        thread: Option<u64>,
        text: String,
    },
    /// A phone's current call and its call history (contract §2.12).
    /// `device` may be left out when exactly one phone has calls on.
    Calls {
        #[serde(default)]
        device: Option<String>,
        #[serde(default)]
        before: Option<u64>,
        #[serde(default)]
        limit: Option<u32>,
    },
    /// Answer, decline or hang up the phone's call, or dial `number`.
    Call {
        #[serde(default)]
        device: Option<String>,
        action: String,
        #[serde(default)]
        number: Option<String>,
    },
}

/// What it answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    Status {
        version: u32,
        /// This machine's static public key, base64url. Public by design.
        key: String,
        machine: String,
        /// LAN addresses a device can dial, `host:port`.
        lan: Vec<String>,
        /// The relay this machine would fall back to, when one is configured.
        relay: Option<String>,
        /// Where a device would meet this machine on that relay.
        rendezvous: String,
        /// Devices that are paired and not revoked.
        paired: usize,
        /// Whether a pairing offer is open right now, and for how much longer.
        offer_ms_left: Option<u64>,
        /// Every connection open right now, with the path it came in on and
        /// the last round trip measured on it.
        ///
        /// P1-052's last criterion, on the desktop side. Live rather than
        /// remembered: a quality from a session that has ended is not a fact
        /// about now.
        #[serde(default)]
        connections: Vec<rime_remote_core::rendezvous::Connection>,
    },
    /// A pairing offer. `qr` is the whole payload; `expires_ms` is when it
    /// stops being accepted.
    Offer { qr: String, expires_ms: u64 },
    Devices {
        devices: Vec<rime_remote_core::device::Device>,
        /// Each phone's last `phone_report`, by device id. Absent from an
        /// older daemon, so a newer `rime` reads none.
        #[serde(default)]
        phones: std::collections::BTreeMap<String, crate::phone::PhoneReport>,
        /// The ids of the paired devices that have a companion key: they keep
        /// working after the phone restarts. The key itself is never sent;
        /// `devices` carries none.
        #[serde(default)]
        companions: Vec<String>,
    },
    /// A `focus` line went to this many phones' open events channels. Zero
    /// is an answer, not an error: no phone connected, or none said "focus".
    Focused { reached: usize },
    /// A ring went out. `path` is `link` when the app said it is ringing, or
    /// `push` when the push service took the request (the phone rings if it
    /// is on and online, which nobody here can see).
    Rang { device: String, path: String },
    /// Something was sent. `path`: `link` when the app said it is taking it,
    /// `push` when the push service took the request to tell the phone, or
    /// `waiting` when it waits for the phone to connect. `push_error` says
    /// why push failed, when it was tried and did.
    Sent {
        device: String,
        path: String,
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        push_error: Option<String>,
    },
    /// What a phone answered to `messages`: `threads`, or `thread` and its
    /// `messages`, newest first. `changed_ms` is when the phone last said
    /// something changed, while this daemon has been up.
    Messages {
        device: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        threads: Option<Vec<crate::messages::Thread>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        messages: Option<Vec<crate::messages::Message>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        changed_ms: Option<u64>,
    },
    /// The phone's radio took the message.
    MessageSent { device: String },
    /// A phone's call now (`current`, absent when idle) and its history,
    /// newest first. `history_error` says why there is no history when the
    /// phone gave none; the current call is still true then.
    Calls {
        device: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current: Option<crate::calls::Call>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        calls: Option<Vec<crate::calls::Entry>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        history_error: Option<String>,
    },
    /// The phone did what `call` asked.
    CallDone { device: String, action: String },
    Ok,
    Error { message: String },
}

impl Reply {
    fn error(message: impl Into<String>) -> Reply {
        Reply::Error {
            message: message.into(),
        }
    }
}

/// Serve the control socket forever.
pub fn serve(listener: UnixListener, state: Arc<State>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let state = Arc::clone(&state);
        // One thread per connection, like `rime-agentd`. These connections are
        // short and rare — a person typing `rime remote devices` — so a
        // thread each costs nothing and keeps a slow client from blocking the
        // next one.
        std::thread::spawn(move || {
            let _ = handle(stream, &state);
        });
    }
}

/// The longest request line read. A `send` of the most text, every byte
/// escaped, is well under it.
const MAX_REQUEST: usize = 1024 * 1024;

fn handle(stream: UnixStream, state: &State) -> std::io::Result<()> {
    // Peer credentials once, from the accepted socket, before a request is
    // parsed. The kernel filled them in at `connect(2)`; anything in a request
    // line is whatever the client chose to send.
    let peer = crate::peer::credentials(&stream);
    let mut writer = stream.try_clone()?;
    // Read with recvmsg, never through a BufReader: a descriptor passed with a
    // `send` arrives beside the bytes, and a plain read(2) would close it.
    let mut pending: Vec<u8> = Vec::new();
    let mut fds: Vec<OwnedFd> = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        let line = loop {
            if let Some(i) = pending.iter().position(|b| *b == b'\n') {
                break pending.drain(..=i).collect::<Vec<u8>>();
            }
            if pending.len() > MAX_REQUEST {
                return Ok(());
            }
            let n = match rime_remote_core::fdpass::recv(&stream, &mut buf, &mut fds) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                    let _ = reply(&mut writer, &Reply::error(e.to_string()));
                    return Ok(());
                }
                Err(e) => return Err(e),
            };
            if n == 0 {
                return Ok(());
            }
            pending.extend_from_slice(&buf[..n]);
        };
        let text = String::from_utf8_lossy(&line);
        let line = text.trim();
        if line.is_empty() {
            continue;
        }
        // The descriptors that came with this line go to this request and no
        // other; a request that takes none drops them, which closes them.
        let carried = std::mem::take(&mut fds);
        let answer = match serde_json::from_str::<Request>(line) {
            Ok(req) => dispatch(req, carried, peer, state),
            Err(e) => Reply::error(format!("unparseable request: {e}")),
        };
        reply(&mut writer, &answer)?;
    }
}

fn reply(writer: &mut UnixStream, reply: &Reply) -> std::io::Result<()> {
    let mut out = serde_json::to_string(reply).unwrap_or_else(|e| {
        format!(r#"{{"reply":"error","message":"could not serialise a reply: {e}"}}"#)
    });
    out.push('\n');
    writer.write_all(out.as_bytes())?;
    writer.flush()
}

fn dispatch(req: Request, fds: Vec<OwnedFd>, peer: Option<crate::peer::Peer>, state: &State) -> Reply {
    // WHO is asking, before WHAT is asked.
    if let Err(why) = authorized(&req, peer) {
        return Reply::error(why);
    }
    match req {
        Request::Status => status(state),
        Request::Pair => pair(state),
        Request::Devices => match state.devices() {
            Ok(store) => devices_reply(&store, state.phones()),
            Err(e) => Reply::error(e.to_string()),
        },
        Request::Revoke { device } => revoke(state, &device),
        Request::Ring { device } => ring(state, &device),
        Request::Focus { dnd } => focus(state, dnd),
        Request::Send {
            device,
            kind,
            name,
            text,
        } => send(state, &device, &kind, name, text, fds),
        Request::Messages {
            device,
            thread,
            before,
            limit,
        } => messages(state, device.as_deref(), thread, before, limit),
        Request::MessagesSend {
            device,
            to,
            thread,
            text,
        } => messages_send(state, device.as_deref(), to, thread, &text),
        Request::Calls { device, before, limit } => calls(state, device.as_deref(), before, limit),
        Request::Call { device, action, number } => call(state, device.as_deref(), &action, number.as_deref()),
    }
}

/// The device list as `rime remote devices` gets it: whether a device has a
/// companion key, and never the key.
fn devices_reply(
    store: &rime_remote_core::device::DeviceStore,
    phones: std::collections::BTreeMap<String, crate::phone::PhoneReport>,
) -> Reply {
    let list = store.list();
    let companions = list
        .iter()
        .filter(|d| d.is_active() && d.companion_key.is_some())
        .map(|d| d.id.clone())
        .collect();
    let devices = list
        .into_iter()
        .map(|d| rime_remote_core::device::Device {
            companion_key: None,
            retired_companion_keys: Vec::new(),
            ..d.clone()
        })
        .collect();
    Reply::Devices {
        devices,
        phones,
        companions,
    }
}

/// Who may ask this.
///
/// Two halves, and until P2-016 only one verb was asked about either. `Pair`
/// went through [`local_caller`]; `Status`, `Devices` and `Revoke` went through
/// nothing at all, on the argument that the socket lives in a `0700` directory
/// inside `$XDG_RUNTIME_DIR` and another account cannot reach it.
///
/// That argument is true and it is the same argument `local_caller`'s own
/// comment dismisses, one line above, with "checked anyway: it costs one
/// syscall". The asymmetry was measured rather than reasoned about: a second
/// account on the L16, with the directory and the socket mode widened by hand
/// so that the filesystem was not the thing answering, got the owner's machine
/// key and LAN addresses out of `Status`, the owner's paired-device list out of
/// `Devices`, and reached the owner's device STORE through `Revoke` — which
/// answered "no paired device 'somebody-elses-phone'", meaning it had looked,
/// and would have revoked one whose name matched. Only `Pair` said no.
///
/// P2-016's fourth criterion is that a remote agent's permissions are bound to
/// the owning user. A verb that hands another account the owner's device list,
/// or takes a device's access away on the owner's behalf, is not bound to
/// anybody. So the account half now applies to every request, and the
/// human-at-the-keyboard half stays where it was — `rime remote devices` from
/// a script is a reasonable thing to do; pairing a phone from one is not.
///
/// There is no `sudo` path to keep open here, which is what makes this
/// different from `rime-agentd` (see `rime-agentd/src/peer.rs::is_own_user`):
/// `rime remote` finds this socket through `$XDG_RUNTIME_DIR`, so under sudo it
/// looks in root's and finds no daemon at all.
fn authorized(req: &Request, peer: Option<crate::peer::Peer>) -> Result<(), String> {
    authorized_by(req, peer, rime_agent_core::origin::observe_pid)
}

/// [`authorized`] with the origin observation passed in, so a test can be a
/// scheduled job without being one.
fn authorized_by(
    req: &Request,
    peer: Option<crate::peer::Peer>,
    observe: impl Fn(i32) -> Result<RequestOrigin, String>,
) -> Result<(), String> {
    match req {
        Request::Pair => local_caller(peer),
        Request::Send { .. } => sending_caller(peer),
        Request::Messages { .. } | Request::MessagesSend { .. } => messages_caller(peer, observe),
        Request::Calls { .. } | Request::Call { .. } => calls_caller(peer, observe),
        _ => own_user(peer),
    }
}

/// Refuse `calls` or `call` from anything but a human at this machine: a
/// call history names who the person talks to, and answering or dialling
/// acts as them on their phone.
fn calls_caller(
    peer: Option<crate::peer::Peer>,
    observe: impl Fn(i32) -> Result<RequestOrigin, String>,
) -> Result<(), String> {
    let Some(peer) = peer else {
        return Err(
            "the kernel would not report the peer credentials of this connection, so there is no \
             way to tell whether a human is at this machine — and a phone's calls are reserved \
             for one"
                .to_string(),
        );
    };
    own_user(Some(peer))?;
    may_use_calls(observe(peer.pid)?)
}

/// The calls rule over a value, for the same reason [`may_pair`] is one.
pub fn may_use_calls(origin: RequestOrigin) -> Result<(), String> {
    if MAY_PAIR.contains(&origin) {
        return Ok(());
    }
    Err(format!(
        "a {origin} request cannot see, answer or place a phone's calls: only a person at this \
         machine may. Run `rime remote calls` from a terminal, or use Rime Shell"
    ))
}

/// Refuse a `messages` or `messages_send` from anything but a human at this
/// machine: the same two origins as pairing and sending.
fn messages_caller(
    peer: Option<crate::peer::Peer>,
    observe: impl Fn(i32) -> Result<RequestOrigin, String>,
) -> Result<(), String> {
    let Some(peer) = peer else {
        return Err(
            "the kernel would not report the peer credentials of this connection, so there is no \
             way to tell whether a human is at this machine — and a phone's messages are reserved \
             for one"
                .to_string(),
        );
    };
    own_user(Some(peer))?;
    may_use_messages(observe(peer.pid)?)
}

/// The messages rule over a value, for the same reason [`may_pair`] is one.
pub fn may_use_messages(origin: RequestOrigin) -> Result<(), String> {
    if MAY_PAIR.contains(&origin) {
        return Ok(());
    }
    Err(format!(
        "a {origin} request cannot read or send a phone's messages: they carry sign-in codes, so \
         only a person at this machine may. Run `rime remote messages` from a terminal, or use \
         Rime Shell"
    ))
}

/// The account half: is this connection this user's?
fn own_user(peer: Option<crate::peer::Peer>) -> Result<(), String> {
    let Some(peer) = peer else {
        return Err(
            "the kernel would not report the peer credentials of this connection, so there is no \
             way to tell which account it belongs to"
                .to_string(),
        );
    };
    if !crate::peer::is_own_user(&peer) {
        return Err("that connection does not belong to this user".to_string());
    }
    Ok(())
}

/// Refuse anything but a human at this machine.
///
/// The message names what was observed, because "permission denied" from a
/// service the user just started is a mystery and "you are a scheduled-job"
/// is a fact they can act on.
fn local_caller(peer: Option<crate::peer::Peer>) -> Result<(), String> {
    let Some(peer) = peer else {
        return Err(
            "the kernel would not report the peer credentials of this connection, so there is no \
             way to tell whether a human is at this machine — and pairing a device is reserved \
             for one"
                .to_string(),
        );
    };
    own_user(Some(peer))?;
    may_pair(rime_agent_core::origin::observe_pid(peer.pid)?)
}

/// Refuse a `send` from anything but a human at this machine: the same two
/// origins as pairing, with a refusal in `send`'s words.
fn sending_caller(peer: Option<crate::peer::Peer>) -> Result<(), String> {
    let Some(peer) = peer else {
        return Err(
            "the kernel would not report the peer credentials of this connection, so there is no \
             way to tell whether a human is at this machine — and sending to a phone is reserved \
             for one"
                .to_string(),
        );
    };
    own_user(Some(peer))?;
    may_send(rime_agent_core::origin::observe_pid(peer.pid)?)
}

/// `send`'s rule over a value, for the same reason [`may_pair`] is one.
pub fn may_send(origin: RequestOrigin) -> Result<(), String> {
    if MAY_PAIR.contains(&origin) {
        return Ok(());
    }
    Err(format!(
        "a {origin} request cannot send to a phone: the phone takes what is sent without asking, so \
         only a person at this machine may send. Run `rime remote send` from a terminal, or use \
         Rime Settings"
    ))
}

/// The rule, over a value.
///
/// Split out from [`local_caller`] because of what a mutation found: with the
/// check replaced by `if true`, every test in this file still passed. They all
/// run as whatever `cargo test` is, which on a developer's machine is a login
/// session and therefore local, so nothing ever reached the refusal. A test
/// that cannot fail is not a test, and the way to make this one able to fail
/// is to ask the rule about all seven origins rather than about this process.
pub fn may_pair(origin: RequestOrigin) -> Result<(), String> {
    if MAY_PAIR.contains(&origin) {
        return Ok(());
    }
    Err(format!(
        "a {origin} request cannot pair a device: pairing hands a phone standing access to this \
         machine's agents, so §7 reserves it for a human at the keyboard. Run `rime remote pair` \
         from a terminal or from Rime Settings"
    ))
}

fn status(state: &State) -> Reply {
    let paired = state
        .devices()
        .map(|s| s.list().iter().filter(|d| d.is_active()).count())
        .unwrap_or(0);
    let now = rime_remote_core::now_ms();
    Reply::Status {
        version: rime_remote_core::REMOTE_PROTOCOL_VERSION,
        key: state.identity.public_key(),
        machine: state.machine.clone(),
        lan: state.lan_addresses(),
        relay: state.relay.clone(),
        rendezvous: rime_remote_core::rendezvous::rendezvous_id(&state.identity.public_bytes()),
        paired,
        offer_ms_left: state.offer_ms_left(now),
        connections: state.connections(),
    }
}

fn pair(state: &State) -> Reply {
    let now = rime_remote_core::now_ms();
    let offer = Offer::new(now);
    let payload = PairingOffer {
        v: rime_remote_core::REMOTE_PROTOCOL_VERSION,
        machine: state.machine.clone(),
        key: state.identity.public_key(),
        token: offer.token_base64(),
        lan: state.lan_addresses(),
        relay: state.relay.clone(),
        expires_ms: offer.expires_ms(),
    };
    let expires_ms = offer.expires_ms();
    // Replacing any offer already open, deliberately. Two live tokens would
    // mean a QR shown five minutes ago still pairs while the owner is looking
    // at a fresh one, and the owner would have no way to tell.
    *state.offer.lock().expect("offer lock") = Some(offer);
    Reply::Offer {
        qr: payload.encode(),
        expires_ms,
    }
}

fn revoke(state: &State, device: &str) -> Reply {
    let mut store = match state.devices() {
        Ok(s) => s,
        Err(e) => return Reply::error(e.to_string()),
    };
    let now = rime_remote_core::now_ms();
    let revoked = match store.revoke(device, now) {
        Ok(d) => d,
        Err(e) => return Reply::error(e.to_string()),
    };
    if let Err(e) = state.save_devices(&store) {
        return Reply::error(format!("the device was not written: {e}"));
    }
    // A revoked device with a live connection is still holding one. The store
    // is what stops it reconnecting; this is what ends the connection it
    // already has, and without it "revoke" would mean "revoke, eventually".
    state.drop_connections_for(&revoked.id);
    // And it must stop being *notified*. A push registration that outlived the
    // pairing would go on waking a phone the owner threw off this machine —
    // the one direction of "revoke, eventually" that survives losing the
    // connection, because push needs no connection by construction.
    state.push_forget(&revoked.id);
    // And what it reported, with its events channel: a revoked phone's
    // battery has no business staying in the Shell.
    state.phone_forget(&revoked.id);
    Reply::Ok
}

/// Tell every connected phone that follows this machine's Do Not Disturb what
/// it is now. A state, not a toggle: a phone already there changes nothing,
/// and the report it sends back matches what the Shell already shows, so
/// nothing comes round again.
///
/// Only phones with an events channel open: a phone that is not connected
/// takes the computer's state from nothing, and pushing it to wake one would
/// be a notification for a setting. Revoked devices have no channel.
fn focus(state: &State, dnd: bool) -> Reply {
    let line = crate::phone::focus_line(dnd);
    let reached = state
        .phones()
        .iter()
        .filter(|(_, report)| report.can_focus())
        .filter(|(id, _)| state.events.send(id, &line))
        .count();
    Reply::Focused { reached }
}

fn ring(state: &State, device: &str) -> Reply {
    let (id, name) = match state.devices() {
        Ok(store) => match store.find(device) {
            Ok(d) if d.is_active() => (d.id.clone(), d.name.clone()),
            Ok(d) => return Reply::error(format!("{} was revoked; it cannot be rung", d.name)),
            Err(e) => return Reply::error(e.to_string()),
        },
        Err(e) => return Reply::error(e.to_string()),
    };
    let report = state.phones().remove(&id);
    let has_push = state.push_store().registrations.contains_key(&id);
    let now_s = rime_remote_core::now_ms() / 1000;
    let got = crate::phone::ring(
        &state.events,
        &id,
        &state.machine,
        crate::phone::Reach {
            report: report.as_ref(),
            has_push,
        },
        now_s,
        crate::phone::ACK_WAIT,
        |started| {
            let trust = rime_remote_core::tls::Trust::system().map_err(|e| e.to_string())?;
            let raised = crate::push::Raised {
                kind: rime_remote_core::push::Kind::Ring,
                session: rime_remote_core::push::NO_SESSION,
                started,
                adapter: rime_remote_core::push::Adapter::Unknown,
            };
            crate::push::push_one(state, &id, raised, &trust)
        },
    );
    match got {
        Ok(rang) => Reply::Rang {
            device: name,
            path: match rang {
                crate::phone::Rang::Link => "link",
                crate::phone::Rang::Push => "push",
            }
            .into(),
        },
        Err(why) => Reply::error(why.sentence(&name)),
    }
}

fn send(
    state: &State,
    device: &str,
    kind: &str,
    name: Option<String>,
    text: Option<String>,
    mut fds: Vec<OwnedFd>,
) -> Reply {
    let (id, device_name) = match state.devices() {
        Ok(store) => match store.find(device) {
            Ok(d) if d.is_active() => (d.id.clone(), d.name.clone()),
            Ok(d) => return Reply::error(format!("{} was revoked; nothing can be sent to it", d.name)),
            Err(e) => return Reply::error(e.to_string()),
        },
        Err(e) => return Reply::error(e.to_string()),
    };
    let body = match kind {
        "file" => {
            if fds.len() != 1 {
                return Reply::error(format!(
                    "a file is sent as one open descriptor passed with the request (`rime remote send <device> <file>`); \
                     this request carried {}",
                    fds.len()
                ));
            }
            let file = std::fs::File::from(fds.remove(0));
            crate::phone::Outgoing::file(file, name.as_deref().unwrap_or(""))
        }
        "text" if fds.is_empty() => crate::phone::Outgoing::text(text.unwrap_or_default()),
        "text" => Err("text is sent in the request, with no descriptor".to_string()),
        other => Err(format!("kind must be \"file\" or \"text\", not {other:?}")),
    };
    let body = match body {
        Ok(b) => b,
        Err(why) => return Reply::error(why),
    };
    let report = state.phones().remove(&id);
    let has_push = state.push_store().registrations.contains_key(&id);
    let now_s = rime_remote_core::now_ms() / 1000;
    let got = crate::phone::send(
        &state.events,
        &state.outbox,
        &id,
        &state.machine,
        crate::phone::Reach {
            report: report.as_ref(),
            has_push,
        },
        body,
        now_s,
        crate::phone::ACK_WAIT,
        |started| {
            let trust = rime_remote_core::tls::Trust::system().map_err(|e| e.to_string())?;
            let raised = crate::push::Raised {
                kind: rime_remote_core::push::Kind::Sent,
                session: rime_remote_core::push::NO_SESSION,
                started,
                adapter: rime_remote_core::push::Adapter::Unknown,
            };
            crate::push::push_one(state, &id, raised, &trust)
        },
    );
    match got {
        Ok((path, offer)) => {
            let (path, push_error) = match path {
                crate::phone::SentPath::Link => ("link", None),
                crate::phone::SentPath::Push => ("push", None),
                crate::phone::SentPath::Waiting { push } => ("waiting", push),
            };
            Reply::Sent {
                device: device_name,
                path: path.into(),
                id: offer,
                push_error,
            }
        }
        Err(why) => Reply::error(why.sentence(&device_name)),
    }
}

/// The phone a messages request goes to: the one named, or the only phone
/// that has Messages on. Its id and name.
fn messages_phone(state: &State, device: Option<&str>) -> Result<(String, String), String> {
    let store = state.devices().map_err(|e| e.to_string())?;
    let phones = state.phones();
    let has = |id: &str| phones.get(id).is_some_and(crate::phone::PhoneReport::can_messages);
    let (id, name) = match device {
        Some(d) => match store.find(d) {
            Ok(d) if d.is_active() => (d.id.clone(), d.name.clone()),
            Ok(d) => return Err(format!("{} was revoked", d.name)),
            Err(e) => return Err(e.to_string()),
        },
        None => {
            let with: Vec<_> = store.list().into_iter().filter(|d| d.is_active() && has(&d.id)).collect();
            match with.as_slice() {
                [d] => (d.id.clone(), d.name.clone()),
                [] => {
                    return Err("no paired phone has turned on Messages on your computer. On the phone: \
                                Rime Remote → Messages"
                        .into())
                }
                many => {
                    let names: Vec<&str> = many.iter().map(|d| d.name.as_str()).collect();
                    return Err(format!(
                        "more than one phone has Messages on ({}); name one with --device",
                        names.join(", ")
                    ));
                }
            }
        }
    };
    // Asked before anything is written: a phone that has not turned Messages
    // on gets no request at all.
    if !has(&id) {
        return Err(format!(
            "{name} has not turned on Messages on your computer. On the phone: Rime Remote → Messages"
        ));
    }
    Ok((id, name))
}

fn messages(state: &State, device: Option<&str>, thread: Option<u64>, before: Option<u64>, limit: Option<u32>) -> Reply {
    let limit = limit.map_or(crate::messages::DEFAULT_LIMIT, |l| l as usize);
    if limit == 0 || limit > crate::messages::MAX_LIMIT {
        return Reply::error(format!("limit must be from 1 to {}", crate::messages::MAX_LIMIT));
    }
    let (id, name) = match messages_phone(state, device) {
        Ok(p) => p,
        Err(why) => return Reply::error(why),
    };
    let want = match thread {
        Some(t) => crate::messages::Want::Thread { id: t, before, limit },
        None => crate::messages::Want::Threads { before, limit },
    };
    let changed_ms = state.messages.changed_ms(&id);
    match state.messages.ask(&state.events, &id, want, crate::messages::READ_WAIT) {
        Ok(crate::messages::Answer::Threads(threads)) => Reply::Messages {
            device: name,
            threads: Some(threads),
            thread: None,
            messages: None,
            changed_ms,
        },
        Ok(crate::messages::Answer::Messages { thread, messages }) => Reply::Messages {
            device: name,
            threads: None,
            thread: Some(thread),
            messages: Some(messages),
            changed_ms,
        },
        Ok(crate::messages::Answer::Sent) => Reply::error("the phone answered a read as a send"),
        Err(why) => Reply::error(why.sentence(&name)),
    }
}

fn messages_send(state: &State, device: Option<&str>, to: Option<String>, thread: Option<u64>, text: &str) -> Reply {
    let to = match (to, thread) {
        (Some(to), None) => match crate::messages::number(&to) {
            Ok(n) => Some(n),
            Err(why) => return Reply::error(why),
        },
        (None, Some(_)) => None,
        _ => return Reply::error("a message goes to one number (--to) or into one conversation (--thread)"),
    };
    let text = match crate::messages::text(text) {
        Ok(t) => t,
        Err(why) => return Reply::error(why),
    };
    let (id, name) = match messages_phone(state, device) {
        Ok(p) => p,
        Err(why) => return Reply::error(why),
    };
    let want = crate::messages::Want::Send { to, thread, text };
    match state.messages.ask(&state.events, &id, want, crate::messages::SEND_WAIT) {
        Ok(crate::messages::Answer::Sent) => Reply::MessageSent { device: name },
        Ok(_) => Reply::error("the phone answered a send as a read"),
        Err(why) => Reply::error(why.sentence(&name)),
    }
}

/// The phone a calls request goes to: the one named, or the only phone that
/// has calls on. Its id, name and last report.
fn calls_phone(state: &State, device: Option<&str>) -> Result<(String, String, crate::phone::PhoneReport), String> {
    let store = state.devices().map_err(|e| e.to_string())?;
    let phones = state.phones();
    let has = |id: &str| phones.get(id).is_some_and(crate::phone::PhoneReport::can_calls);
    let (id, name) = match device {
        Some(d) => match store.find(d) {
            Ok(d) if d.is_active() => (d.id.clone(), d.name.clone()),
            Ok(d) => return Err(format!("{} was revoked", d.name)),
            Err(e) => return Err(e.to_string()),
        },
        None => {
            let with: Vec<_> = store.list().into_iter().filter(|d| d.is_active() && has(&d.id)).collect();
            match with.as_slice() {
                [d] => (d.id.clone(), d.name.clone()),
                [] => {
                    return Err("no paired phone has turned on Calls on your computer. On the phone: \
                                Rime Remote → Calls on your computer"
                        .into())
                }
                many => {
                    let names: Vec<&str> = many.iter().map(|d| d.name.as_str()).collect();
                    return Err(format!(
                        "more than one phone has calls on ({}); name one with --device",
                        names.join(", ")
                    ));
                }
            }
        }
    };
    // Asked before anything is written: a phone that has not turned calls on
    // gets no request at all.
    match phones.get(&id) {
        Some(r) if r.can_calls() => Ok((id, name, r.clone())),
        _ => Err(format!(
            "{name} has not turned on Calls on your computer. On the phone: Rime Remote → Calls on your computer"
        )),
    }
}

fn calls(state: &State, device: Option<&str>, before: Option<u64>, limit: Option<u32>) -> Reply {
    let limit = limit.map_or(crate::calls::DEFAULT_LIMIT, |l| l as usize);
    if limit == 0 || limit > crate::calls::MAX_LIMIT {
        return Reply::error(format!("limit must be from 1 to {}", crate::calls::MAX_LIMIT));
    }
    let (id, name, report) = match calls_phone(state, device) {
        Ok(p) => p,
        Err(why) => return Reply::error(why),
    };
    let current = state.calls.current(&id);
    let (calls, history_error) =
        match state.calls.history(&state.events, &id, &report, before, limit, crate::calls::HISTORY_WAIT) {
            Ok(calls) => (Some(calls), None),
            Err(why) => (None, Some(why.sentence(&name))),
        };
    Reply::Calls { device: name, current, calls, history_error }
}

fn call(state: &State, device: Option<&str>, action: &str, number: Option<&str>) -> Reply {
    if !crate::calls::ACTIONS.contains(&action) {
        return Reply::error("the action must be answer, decline, hangup or dial");
    }
    let number = match (action, number) {
        ("dial", Some(n)) => match crate::messages::number(n) {
            Ok(n) => Some(n),
            Err(why) => return Reply::error(why),
        },
        ("dial", None) => return Reply::error("dial needs a number"),
        (_, None) => None,
        (_, Some(_)) => return Reply::error("only dial takes a number"),
    };
    let (id, name, report) = match calls_phone(state, device) {
        Ok(p) => p,
        Err(why) => return Reply::error(why),
    };
    match state.calls.act(&state.events, &id, &report, action, number.as_deref(), crate::calls::ACTION_WAIT) {
        Ok(()) => Reply::CallDone { device: name, action: action.to_string() },
        Err(why) => Reply::error(why.sentence(&name)),
    }
}

/// The origins this socket accepts a pairing request from.
///
/// Written out and matched against, rather than left implicit in
/// `is_local()`. The two mean the same thing today and they are different
/// claims: `is_local` is §7's answer to "is a human present", and this is
/// this socket's answer to "may this caller hand a phone standing access to
/// the machine". A test pins them together, so a future §7 origin that is
/// local for one purpose and not the other has to be decided rather than
/// inherited.
const MAY_PAIR: [RequestOrigin; 2] = [RequestOrigin::LocalTerminal, RequestOrigin::RimeShell];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_origin_that_may_pair_is_local_and_no_other_is() {
        // The two lists have to agree, and the reason to assert it here is
        // that `MAY_PAIR` is what a reader checks against §7 while
        // `is_local()` is what the code runs.
        for o in RequestOrigin::ALL {
            assert_eq!(
                MAY_PAIR.contains(o),
                o.is_local(),
                "{o} disagrees between MAY_PAIR and is_local"
            );
        }
        assert_eq!(MAY_PAIR.len(), 2);
    }

    #[test]
    fn a_connection_with_no_peer_credentials_may_not_pair() {
        let why = local_caller(None).expect_err("an unidentifiable peer paired a device");
        assert!(why.contains("a human is at this machine"), "{why}");
    }

    #[test]
    fn exactly_the_two_local_origins_may_pair_and_the_other_five_are_refused_by_name() {
        // Over §7's whole vocabulary, not over whatever this process happens
        // to be. The version of this file that asked only about the running
        // process passed with the check replaced by `if true`.
        let mut allowed = 0;
        for o in RequestOrigin::ALL {
            match may_pair(*o) {
                Ok(()) => {
                    assert!(o.is_local(), "{o} was allowed to pair a device");
                    allowed += 1;
                }
                Err(why) => {
                    assert!(!o.is_local(), "{o} was refused");
                    assert!(why.contains(o.as_str()), "{why} does not name {o}");
                    // And it says what to do instead, because the person
                    // reading it is looking at a command that just failed.
                    assert!(why.contains("rime remote pair"), "{why}");
                }
            }
        }
        assert_eq!(allowed, 2, "the wrong number of origins may pair");
    }

    #[test]
    fn exactly_the_two_local_origins_may_send_and_the_refusal_says_send() {
        // `send` is gated like pairing because the phone takes what arrives
        // without asking; an agent (a scheduled-job) must be refused, by name,
        // in words about sending rather than pairing.
        let mut allowed = 0;
        for o in RequestOrigin::ALL {
            match may_send(*o) {
                Ok(()) => {
                    assert!(o.is_local(), "{o} was allowed to send to a phone");
                    allowed += 1;
                }
                Err(why) => {
                    assert!(!o.is_local(), "{o} was refused");
                    assert!(why.contains(o.as_str()), "{why} does not name {o}");
                    assert!(why.contains("rime remote send"), "{why}");
                    assert!(!why.contains("pair"), "{why}");
                }
            }
        }
        assert_eq!(allowed, 2);
        assert!(may_send(RequestOrigin::ScheduledJob).is_err());
    }

    #[test]
    fn another_accounts_connection_may_not_pair_however_local_it_looks() {
        // P2-016 criterion 4: a remote agent's permissions are bound to the
        // OWNING user. Pairing is where that binding is made, and until now
        // nothing asserted the uid half of it — every test in this file runs
        // as one uid, so `local_caller`'s `is_own_user` branch was never
        // reached and the check could have been deleted without a red run.
        // The same file already records that lesson once, for `may_pair`.
        //
        // The pid is THIS process's on purpose. It classifies as whatever the
        // test runner is, and under tests/in-login-session.sh that is a local
        // origin — the one `may_pair` allows. So with the uid check removed
        // this peer WOULD pair, which is what makes the assertion able to
        // fail rather than passing on the origin rule's coat-tails.
        //
        // uid 0 rather than "some uid that is not mine": root always resolves
        // to a real account, on every machine, and it is the account that
        // must not quietly inherit another's paired devices. `me().uid + 1`
        // is not an account on a one-account machine, and this program has
        // already shipped one test that passed for that reason instead of the
        // one it was testing.
        for (uid, what) in [
            (0, "root"),
            (unsafe { libc::getuid() }.wrapping_add(1), "a neighbouring uid"),
        ] {
            let foreign = crate::peer::Peer {
                pid: std::process::id() as libc::pid_t,
                uid,
                gid: unsafe { libc::getgid() },
            };
            let Err(why) = local_caller(Some(foreign)) else {
                panic!("{what} paired a device");
            };
            assert!(
                why.contains("does not belong to this user"),
                "{what}: {why}"
            );
            // Refused for THAT reason and not by the origin rule. The two are
            // different failures with different fixes, and a test that
            // accepted either would go green on a CI runner — where every
            // caller is a scheduled-job — over a uid check that had been
            // removed.
            assert!(!why.contains("rime remote pair"), "{what}: {why}");
        }
    }

    #[test]
    fn every_control_verb_is_refused_to_another_account_and_not_only_pairing() {
        // The asymmetry P2-016 found, over the whole vocabulary rather than
        // over one verb. `Devices` discloses the owner's paired devices and
        // `Revoke` takes a device's access away; before this they reached the
        // owner's store with no question asked about who was on the other end
        // of the socket, and only `Pair` said no. Measured with a real second
        // account, not inferred.
        let foreign = crate::peer::Peer {
            pid: std::process::id() as libc::pid_t,
            uid: 0,
            gid: unsafe { libc::getgid() },
        };
        let every = [
            Request::Status,
            Request::Pair,
            Request::Devices,
            Request::Revoke {
                device: "somebody-elses-phone".into(),
            },
            Request::Ring {
                device: "somebody-elses-phone".into(),
            },
            Request::Focus { dnd: true },
            Request::Send {
                device: "somebody-elses-phone".into(),
                kind: "text".into(),
                name: None,
                text: Some("x".into()),
            },
            Request::Messages {
                device: None,
                thread: None,
                before: None,
                limit: None,
            },
            Request::MessagesSend {
                device: None,
                to: Some("5556".into()),
                thread: None,
                text: "x".into(),
            },
            Request::Calls { device: None, before: None, limit: None },
            Request::Call { device: None, action: "dial".into(), number: Some("5556".into()) },
        ];
        for req in &every {
            let Err(why) = authorized(req, Some(foreign)) else {
                panic!("{req:?} was allowed from another account");
            };
            assert!(
                why.contains("does not belong to this user"),
                "{req:?}: {why}"
            );
        }

        // And the account half must not have swallowed the human half: this
        // process's own peer still has to satisfy `may_pair` for `Pair`, and
        // must not for the other three. Without this, `authorized` collapsing
        // to `own_user` for everything would pass the loop above and quietly
        // let a scheduled job pair a phone.
        let me = crate::peer::Peer {
            pid: std::process::id() as libc::pid_t,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
        };
        for req in &every {
            let verdict = authorized(req, Some(me));
            match req {
                Request::Pair => assert_eq!(
                    verdict.is_ok(),
                    local_caller(Some(me)).is_ok(),
                    "Pair stopped asking the origin rule"
                ),
                // An origin that cannot be classified (a build sandbox's
                // cgroup) refuses, as `sending_caller` does.
                Request::Send { .. } => assert_eq!(
                    verdict.is_ok(),
                    rime_agent_core::origin::observe_pid(me.pid).is_ok_and(|o| may_send(o).is_ok()),
                    "Send stopped asking the origin rule"
                ),
                Request::Messages { .. } | Request::MessagesSend { .. } => assert_eq!(
                    verdict.is_ok(),
                    rime_agent_core::origin::observe_pid(me.pid).is_ok_and(|o| may_use_messages(o).is_ok()),
                    "{req:?} stopped asking the origin rule"
                ),
                Request::Calls { .. } | Request::Call { .. } => assert_eq!(
                    verdict.is_ok(),
                    rime_agent_core::origin::observe_pid(me.pid).is_ok_and(|o| may_use_calls(o).is_ok()),
                    "{req:?} stopped asking the origin rule"
                ),
                other => assert!(verdict.is_ok(), "{other:?} refused its own user: {verdict:?}"),
            }
        }
    }

    #[test]
    fn a_scheduled_job_may_neither_read_nor_send_a_phones_messages() {
        // The origin is injected, so this fails on any machine — a developer's
        // login session included — if the gate is removed.
        let me = crate::peer::Peer {
            pid: std::process::id() as libc::pid_t,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
        };
        let read = Request::Messages { device: None, thread: None, before: None, limit: None };
        let send = Request::MessagesSend { device: None, to: Some("5556".into()), thread: None, text: "x".into() };
        for o in RequestOrigin::ALL {
            for req in [&read, &send] {
                let verdict = authorized_by(req, Some(me), |_| Ok(*o));
                assert_eq!(verdict.is_ok(), o.is_local(), "{o} {req:?}: {verdict:?}");
                if let Err(why) = verdict {
                    assert!(why.contains(o.as_str()) && why.contains("rime remote messages"), "{why}");
                }
            }
        }
        // An origin that cannot be observed is refused too.
        assert!(authorized_by(&send, Some(me), |_| Err("no cgroup".into())).is_err());
        assert!(authorized_by(&read, None, |_| Ok(RequestOrigin::LocalTerminal)).is_err());
        // And `status` is still a script's to ask.
        assert!(authorized_by(&Request::Status, Some(me), |_| Ok(RequestOrigin::ScheduledJob)).is_ok());
    }

    #[test]
    fn a_scheduled_job_may_neither_see_nor_handle_a_phones_calls() {
        // Injected origins, as for messages: this fails on any machine if the
        // gate is removed, a developer's login session included.
        let me = crate::peer::Peer {
            pid: std::process::id() as libc::pid_t,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
        };
        let list = Request::Calls { device: None, before: None, limit: None };
        let every: Vec<Request> = std::iter::once(list)
            .chain(["answer", "decline", "hangup"].into_iter().map(|a| Request::Call {
                device: None,
                action: a.into(),
                number: None,
            }))
            .chain(std::iter::once(Request::Call { device: None, action: "dial".into(), number: Some("5556".into()) }))
            .collect();
        for o in RequestOrigin::ALL {
            for req in &every {
                let verdict = authorized_by(req, Some(me), |_| Ok(*o));
                assert_eq!(verdict.is_ok(), o.is_local(), "{o} {req:?}: {verdict:?}");
                if let Err(why) = verdict {
                    assert!(why.contains(o.as_str()) && why.contains("rime remote calls"), "{why}");
                }
            }
        }
        assert!(authorized_by(&every[4], Some(me), |_| Err("no cgroup".into())).is_err());
        assert!(authorized_by(&every[0], None, |_| Ok(RequestOrigin::LocalTerminal)).is_err());
    }

    #[test]
    fn the_refusal_names_the_origin_it_refused() {
        // A message the user can act on. Built from the real classification of
        // this process rather than from a fixture, so it stays true.
        let me = crate::peer::Peer {
            pid: std::process::id() as libc::pid_t,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
        };
        let observed = rime_agent_core::origin::observe_pid(me.pid);
        match (observed, local_caller(Some(me))) {
            (Ok(o), Ok(())) => assert!(o.is_local(), "{o} was allowed to pair"),
            (Ok(o), Err(why)) => {
                assert!(!o.is_local(), "{o} was refused");
                assert!(why.contains(o.as_str()), "{why} does not name {o}");
            }
            (Err(_), Err(_)) => {}
            (Err(why), Ok(())) => panic!("unclassifiable ({why}) and still allowed to pair"),
        }
    }

    #[test]
    fn every_request_and_reply_survives_the_line_framing() {
        // One JSON object per line. A serialised value containing a raw
        // newline would desynchronise the socket.
        let requests = vec![
            Request::Status,
            Request::Pair,
            Request::Devices,
            Request::Revoke {
                device: "pixel-8".into(),
            },
            Request::Ring {
                device: "pixel-8".into(),
            },
            Request::Focus { dnd: false },
            Request::Send {
                device: "pixel-8".into(),
                kind: "text".into(),
                name: None,
                text: Some("line one\nline two".into()),
            },
            Request::Messages {
                device: Some("pixel-8".into()),
                thread: Some(3),
                before: Some(1),
                limit: Some(20),
            },
            Request::MessagesSend {
                device: None,
                to: None,
                thread: Some(3),
                text: "line one\nline two".into(),
            },
        ];
        // The wire shapes `rime remote calls` depends on.
        assert_eq!(
            serde_json::to_string(&Request::Call { device: None, action: "dial".into(), number: Some("5556".into()) })
                .unwrap(),
            r#"{"cmd":"call","device":null,"action":"dial","number":"5556"}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Calls { device: Some("p".into()), before: None, limit: Some(5) }).unwrap(),
            r#"{"cmd":"calls","device":"p","before":null,"limit":5}"#
        );
        assert_eq!(
            serde_json::to_string(&Reply::CallDone { device: "Pixel".into(), action: "answer".into() }).unwrap(),
            r#"{"reply":"call_done","device":"Pixel","action":"answer"}"#
        );
        // The wire shapes `rime remote messages` and a Shell page depend on.
        assert_eq!(
            serde_json::to_string(&Request::MessagesSend { device: None, to: Some("5556".into()), thread: None, text: "x".into() })
                .unwrap(),
            r#"{"cmd":"messages_send","device":null,"to":"5556","thread":null,"text":"x"}"#
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"messages"}"#).map(|r| matches!(r, Request::Messages { .. })).ok(),
            Some(true)
        );
        // The wire shape `rime remote focus` and the Shell's call depend on.
        assert_eq!(serde_json::to_string(&Request::Focus { dnd: true }).unwrap(), r#"{"cmd":"focus","dnd":true}"#);
        assert_eq!(serde_json::to_string(&Reply::Focused { reached: 2 }).unwrap(), r#"{"reply":"focused","reached":2}"#);
        for r in &requests {
            let text = serde_json::to_string(r).expect("serialise");
            assert!(!text.contains('\n'), "{text}");
            serde_json::from_str::<Request>(&text).expect("round trip");
        }
        let replies = vec![
            Reply::Ok,
            Reply::error("something went wrong"),
            Reply::Offer {
                qr: "apex-remote:abc".into(),  // rime-rename: keep (SCHEME)
                expires_ms: 1,
            },
            Reply::Devices { devices: vec![], phones: Default::default(), companions: vec![] },
            Reply::Rang { device: "Pixel".into(), path: "link".into() },
            Reply::Focused { reached: 0 },
            Reply::MessageSent { device: "Pixel".into() },
            Reply::Messages {
                device: "Pixel".into(),
                threads: None,
                thread: Some(3),
                messages: Some(vec![crate::messages::Message {
                    id: 1,
                    date_ms: 2,
                    from_me: false,
                    body: "a\nb".into(),
                    status: "received".into(),
                }]),
                changed_ms: None,
            },
            Reply::Sent { device: "Pixel".into(), path: "waiting".into(), id: 3, push_error: Some("503".into()) },
            Reply::Status {
                version: 1,
                key: "k".into(),
                machine: "l16".into(),
                lan: vec!["10.0.0.1:7717".into()],
                relay: None,
                connections: Vec::new(),
                rendezvous: "r".into(),
                paired: 0,
                offer_ms_left: Some(1000),
            },
        ];
        for r in &replies {
            let text = serde_json::to_string(r).expect("serialise");
            assert!(!text.contains('\n'), "{text}");
            serde_json::from_str::<Reply>(&text).expect("round trip");
        }
    }

    #[test]
    fn the_device_list_says_which_have_a_companion_key_and_never_carries_one() {
        let mut store = rime_remote_core::device::DeviceStore::default();
        let k = |n: u8| rime_remote_core::b64_encode(&[n; 32]);
        let a = store.pair(&k(1), "a", 1, false).expect("pair");
        let b = store.pair(&k(2), "b", 2, false).expect("pair");
        let gone = store.pair(&k(3), "gone", 3, false).expect("pair");
        store.set_companion(&a.id, &k(10)).expect("set");
        store.set_companion(&a.id, &k(11)).expect("replace: k(10) is retired");
        store.set_companion(&gone.id, &k(13)).expect("set");
        store.revoke(&gone.id, 4).expect("revoke");
        let reply = devices_reply(&store, Default::default());
        let text = serde_json::to_string(&reply).expect("serialise");
        assert!(!text.contains(&k(10)) && !text.contains(&k(11)) && !text.contains(&k(13)), "{text}");
        assert!(!text.contains("companion_key"), "{text}");
        let Reply::Devices { companions, devices, .. } = reply else { panic!("{text}") };
        assert_eq!(companions, vec![a.id.clone()], "a revoked device's key is no companion");
        assert_eq!(devices.len(), 3);
        assert!(!companions.contains(&b.id));
    }
}
