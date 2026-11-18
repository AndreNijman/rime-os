//! The owner's other computers: pairing one with a code, what a session from
//! one may do, and asking one something.
//!
//! ## Pairing
//!
//! One computer opens "Add a computer" (`rime remote computers add
//! --show-code`): this process mints a six-digit code that lives two minutes
//! and holds it in [`State::showing`] for exactly as long as that request's
//! connection stays open. The person types the code into the other computer
//! (`rime remote computers add <address>`), which dials this one with the
//! computer-pairing hello and runs `rime_remote_core::computer` (Noise XX,
//! then SPAKE2 over the code, then key confirmation). See that module for the
//! cryptography; this one is the bookkeeping around it:
//!
//! * With no screen open, or its code expired, a pairing connection is ended
//!   before this computer shows its key ([`has_code`]).
//! * The code is taken out of the slot the moment the handshake finishes
//!   ([`spend`]), before the key check or the code check, so every attempt
//!   uses it up, right or wrong. The screen is then told how it ended and
//!   closes; a new attempt needs a new screen.
//! * Both ends of the request are a person: the screen and the typed code are
//!   refused to anything but a local terminal or Rime Shell (`control.rs`).
//!
//! ## Membership is not authority
//!
//! A session another computer opens gets [`Scope::Peer`], from which key
//! completed the handshake. `serve.rs` hands every control line of such a
//! session to [`peer_reply`] before anything else, which answers
//! [`PEER_VERBS`] and refuses everything else: nothing from a peer reaches
//! `rime-agentd`, nex, the computer's own verbs, the phone verbs, or the human
//! gate, and a peer may open no channel. Controlling another computer's
//! agents or terminals will be a local grant on the computer being
//! controlled, per pair; it is not in this file and not in this release.
//!
//! ## Asking
//!
//! Each computer dials the other for its own requests (`dialer.rs`); there is
//! no held link yet. `rime remote computers ping` is the one request.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rime_remote_core::computer::{self, About, CodeOffer, PairError, MAX_ADDRESSES};
use rime_remote_core::device::Device;
use rime_remote_core::rendezvous::Path as RemotePath;
use serde_json::json;

use crate::control::Reply;
use crate::state::State;

/// The only verbs a peer session may send. Everything else is refused before
/// it is routed anywhere.
pub const PEER_VERBS: [&str; 2] = ["remote_hello", "peer_hello"];

/// What a peer's `remote_hello` lists.
pub const PEER_FEATURES: [&str; 1] = ["peer_hello"];

/// The `kind` of the refusal a peer gets for anything outside the list.
pub const REFUSAL: &str = "not_allowed";

/// How long `add` waits for the other computer at each step.
const STEP: Duration = Duration::from_secs(15);

/// How long `add` tries to connect to one address.
const CONNECT: Duration = Duration::from_secs(5);

/// The refusal, one line.
pub fn refusal(what: &str) -> Vec<u8> {
    json!({
        "reply": "error",
        "kind": REFUSAL,
        "message": format!("{what} is not allowed between paired computers"),
    })
    .to_string()
    .into_bytes()
}

/// The reply to one control line from a peer session. Never `None`, never
/// forwarded: a line nothing here answers is refused.
pub fn peer_reply(state: &State, line: &[u8]) -> Vec<u8> {
    let cmd = crate::companion::verb_of(line);
    match cmd.as_deref() {
        Some(c) if PEER_VERBS.contains(&c) && c == "remote_hello" => json!({
            "reply": "remote",
            "version": rime_remote_core::REMOTE_PROTOCOL_VERSION,
            "features": PEER_FEATURES,
            "scope": "peer",
        })
        .to_string()
        .into_bytes(),
        Some(c) if PEER_VERBS.contains(&c) && c == "peer_hello" => json!({
            "reply": "peer_hello",
            "name": state.machine,
            "version": rime_remote_core::REMOTE_PROTOCOL_VERSION,
            "build": env!("CARGO_PKG_VERSION"),
        })
        .to_string()
        .into_bytes(),
        other => refusal(other.unwrap_or("that request")),
    }
}

/// How a pairing attempt on a shown code ended, for the screen showing it:
/// the paired computer's id and name, or why not.
pub type Outcome = Result<(String, String), String>;

/// The open "Add a computer" screen: its code, which request it belongs to,
/// and how to tell that request how an attempt ended.
pub struct Showing {
    pub offer: CodeOffer,
    pub id: u64,
    pub tell: mpsc::Sender<Outcome>,
}

static NEXT_SCREEN: AtomicU64 = AtomicU64::new(1);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Open a screen: a fresh code in the slot, replacing any other screen's
/// (whose request then hears that it was replaced).
pub fn open_screen(slot: &Mutex<Option<Showing>>, now_ms: u64) -> (u64, CodeOffer, mpsc::Receiver<Outcome>) {
    let (tell, hear) = mpsc::channel();
    let id = NEXT_SCREEN.fetch_add(1, Ordering::Relaxed);
    let offer = CodeOffer::new(now_ms);
    *lock(slot) = Some(Showing {
        offer: offer.clone(),
        id,
        tell,
    });
    (id, offer, hear)
}

/// Whether a code is open and live right now.
pub fn has_code(slot: &Mutex<Option<Showing>>, now_ms: u64) -> bool {
    lock(slot).as_ref().is_some_and(|s| s.offer.is_live(now_ms))
}

/// Take the code out of the slot: `None` when there is none, or it expired
/// (and is then dropped too). Whoever takes it owns telling the screen.
pub fn spend(slot: &Mutex<Option<Showing>>, now_ms: u64) -> Option<(String, mpsc::Sender<Outcome>)> {
    let showing = lock(slot).take()?;
    if !showing.offer.is_live(now_ms) {
        return None;
    }
    Some((showing.offer.code().to_string(), showing.tell))
}

/// Close screen `id` if its code is still in the slot. `true` when it was.
pub fn close_screen(slot: &Mutex<Option<Showing>>, id: u64) -> bool {
    let mut s = lock(slot);
    if s.as_ref().is_some_and(|s| s.id == id) {
        *s = None;
        true
    } else {
        false
    }
}

fn still_open(slot: &Mutex<Option<Showing>>, id: u64) -> bool {
    lock(slot).as_ref().is_some_and(|s| s.id == id)
}

/// What this computer tells another about itself.
fn own_about(state: &State) -> About {
    let mut lan = state.lan_addresses();
    lan.truncate(MAX_ADDRESSES);
    About {
        name: state.machine.clone(),
        lan,
        relay: state.relay.clone(),
        port: Some(state.port),
    }
}

/// Where a computer that just paired can be dialled: where its connection
/// came from (when it came over the LAN, with the port it says it listens
/// on), then what it listed.
fn addresses_for(observed: Option<String>, about: &About) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for a in observed.into_iter().chain(about.lan.iter().cloned()) {
        if !v.contains(&a) {
            v.push(a);
        }
    }
    v.truncate(MAX_ADDRESSES);
    v
}

fn store_computer(state: &State, key: &[u8; 32], about: &About, addresses: Vec<String>) -> Result<Device, String> {
    let mut store = state.devices().map_err(|e| e.to_string())?;
    let own = state.identity.public_bytes();
    let c = store
        .pair_computer(key, &own, &about.name, addresses, about.relay.clone(), rime_remote_core::now_ms())
        .map_err(|e| e.to_string())?;
    state
        .save_devices(&store)
        .map_err(|e| format!("the computer was not written: {e}"))?;
    Ok(c)
}

/// What the showing computer says about an attempt that failed, in its own
/// words (the dialler's are `PairError`'s).
fn screen_words(e: &PairError) -> String {
    match e {
        PairError::WrongCode => "the code typed on the other computer did not match, so this code is used up; \
                                 show a new one to try again"
            .into(),
        PairError::OwnKey => "the other computer has this computer's own Rime Remote key (a copied home \
                              folder), so it was refused; this code is used up"
            .into(),
        other => format!("pairing did not complete ({other}); this code is used up"),
    }
}

/// A computer-pairing connection on the LAN listener (the computer hello).
/// `peer` is where it came from.
///
/// **Over the LAN only**, refused before a byte of the handshake is read or
/// written. Relay splices arrive on the same listener, and XX message 2
/// carries this computer's static key, from which the relay rendezvous id is
/// derived: a host on the LAN that dialled once while a code was shown could
/// otherwise hand that id to anyone, who could then spend the code from
/// anywhere on the internet. A connection whose address is unknown is refused
/// too, since it cannot be shown to be on the LAN.
pub fn accept(socket: &mut TcpStream, state: &State, peer: Option<SocketAddr>) -> Result<(), crate::serve::ServeError> {
    let observed_ip = match (peer, state.path_of(peer)) {
        (Some(p), RemotePath::Lan) => p.ip(),
        _ => {
            return Err(crate::serve::ServeError::Protocol(
                "a computer pairing arrived over the relay or from an unknown address; computers pair over \
                 the local network only"
                    .into(),
            ))
        }
    };
    let secret = crate::secret_of(state);
    let about = own_about(state);
    let mut teller: Option<mpsc::Sender<Outcome>> = None;
    let mut stored: Option<Device> = None;
    let result = computer::show(
        socket,
        &secret,
        rime_remote_core::REMOTE_PROTOCOL_VERSION,
        || has_code(&state.showing, rime_remote_core::now_ms()),
        || {
            spend(&state.showing, rime_remote_core::now_ms()).map(|(code, tell)| {
                teller = Some(tell);
                code
            })
        },
        &about,
        |key, theirs| {
            let observed = theirs.port.map(|port| crate::state::advertise(observed_ip, port));
            stored = Some(store_computer(state, key, theirs, addresses_for(observed, theirs))?);
            Ok(())
        },
    );
    if let Some(tell) = teller {
        let _ = tell.send(match (&result, &stored) {
            (Ok(_), Some(c)) => Ok((c.id.clone(), c.name.clone())),
            (Ok(_), None) => Err("pairing completed but nothing was stored".into()),
            (Err(e), _) => Err(screen_words(e)),
        });
    }
    result
        .map(|_| ())
        .map_err(|e| crate::serve::ServeError::Protocol(format!("computer pairing: {e}")))
}

/// Hold an "Add a computer" screen open on a control connection: answer with
/// the code, then wait until a pairing uses it, it expires, another screen
/// replaces it, or the person closes the screen (the client hangs up), and
/// answer once more unless they hung up.
pub fn hold_screen(stream: &UnixStream, state: &State) -> std::io::Result<()> {
    let mut writer = stream.try_clone()?;
    let (id, offer, hear) = open_screen(&state.showing, rime_remote_core::now_ms());
    crate::control::reply(
        &mut writer,
        &Reply::ComputerCode {
            code: offer.code().to_string(),
            expires_ms: offer.expires_ms(),
            machine: state.machine.clone(),
        },
    )?;
    stream.set_read_timeout(Some(Duration::from_millis(150)))?;
    let finish = |writer: &mut UnixStream, reply: Reply| crate::control::reply(writer, &reply);
    loop {
        match hear.recv_timeout(Duration::from_millis(50)) {
            Ok(Ok((id, name))) => return finish(&mut writer, Reply::ComputerPaired { id, name }),
            Ok(Err(why)) => return finish(&mut writer, Reply::Error { message: why }),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return finish(
                    &mut writer,
                    Reply::Error {
                        message: "another \"Add a computer\" screen opened, so this code was withdrawn".into(),
                    },
                )
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let mut b = [0u8; 1];
        match (&*stream).read(&mut b) {
            // The screen closed: the code goes with it.
            Ok(_) => {
                close_screen(&state.showing, id);
                return Ok(());
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) => {
                close_screen(&state.showing, id);
                return Err(e);
            }
        }
        // Expiry, only while the code is still in the slot: once a pairing
        // took it, that pairing tells this screen how it ended.
        if !offer.is_live(rime_remote_core::now_ms()) && still_open(&state.showing, id) && close_screen(&state.showing, id) {
            return finish(
                &mut writer,
                Reply::Error {
                    message: "the code expired after two minutes without being used; show a new one".into(),
                },
            );
        }
    }
}

/// A typed address to the addresses to try, and the spelling to store: with
/// the default port when none was given.
fn typed_address(address: &str) -> Result<(String, Vec<SocketAddr>), String> {
    let address = address.trim();
    if address.is_empty() || address.len() > 255 || address.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("give the other computer's address or host name, as `rime remote status` shows it there".into());
    }
    if let Ok(ip) = address.parse::<std::net::IpAddr>() {
        let a = SocketAddr::new(ip, crate::DEFAULT_PORT);
        return Ok((a.to_string(), vec![a]));
    }
    let spelled = match address.to_socket_addrs() {
        Ok(_) => address.to_string(),
        Err(_) => format!("{address}:{}", crate::DEFAULT_PORT),
    };
    let found: Vec<SocketAddr> = spelled
        .to_socket_addrs()
        .map_err(|e| format!("{address}: {e}"))?
        .collect();
    if found.is_empty() {
        return Err(format!("{address} did not resolve to an address"));
    }
    Ok((spelled, found))
}

/// Connect to the first of `addrs` that answers.
pub fn connect_any(addrs: &[SocketAddr], timeout: Duration) -> Result<TcpStream, String> {
    let mut why = Vec::new();
    for a in addrs {
        match TcpStream::connect_timeout(a, timeout) {
            Ok(s) => return Ok(s),
            Err(e) => why.push(format!("{a}: {e}")),
        }
    }
    Err(why.join("; "))
}

/// `rime remote computers add <address>`: type the code another computer is
/// showing, and pair with it.
pub fn add(state: &State, address: &str, code: &str) -> Reply {
    match add_inner(state, address, code) {
        Ok(c) => Reply::ComputerPaired { id: c.id, name: c.name },
        Err(message) => Reply::Error { message },
    }
}

fn add_inner(state: &State, address: &str, code: &str) -> Result<Device, String> {
    let code = computer::normalize_code(code).map_err(|e| e.to_string())?;
    let (spelled, addrs) = typed_address(address)?;
    let mut stream = connect_any(&addrs, CONNECT).map_err(|e| format!("could not reach {address}: {e}"))?;
    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(STEP)).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(STEP)).map_err(|e| e.to_string())?;
    stream
        .write_all(&[crate::serve::HELLO_COMPUTER])
        .map_err(|e| format!("could not reach {address}: {e}"))?;
    let own = state.identity.public_bytes();
    let paired = computer::dial(
        &mut stream,
        &crate::secret_of(state),
        &code,
        &own_about(state),
        rime_remote_core::REMOTE_PROTOCOL_VERSION,
        |theirs| {
            // What `pair_computer` would refuse, asked before the other side
            // stores anything, on a scratch copy of the store.
            let mut probe = state.devices().map_err(|e| PairError::Refused(e.to_string()))?.clone();
            probe
                .pair_computer(theirs, &own, "probe", Vec::new(), None, 0)
                .map(|_| ())
                .map_err(|e| PairError::Refused(format!("this computer will not store it: {e}")))
        },
    )
    .map_err(|e| e.to_string())?;
    let mut addresses = vec![spelled];
    addresses.extend(addresses_for(None, &paired.about));
    addresses.dedup();
    addresses.truncate(MAX_ADDRESSES);
    store_computer(state, &paired.key, &paired.about, addresses)
}

/// `rime remote computers list`.
pub fn list(state: &State) -> Reply {
    match state.devices() {
        Ok(store) => Reply::Computers {
            computers: store.list_computers().into_iter().cloned().collect(),
        },
        Err(e) => Reply::Error { message: e.to_string() },
    }
}

/// `rime remote computers remove <computer>`: a tombstone, and its live
/// connections end now.
pub fn remove(state: &State, computer: &str) -> Reply {
    let mut store = match state.devices() {
        Ok(s) => s,
        Err(e) => return Reply::Error { message: e.to_string() },
    };
    let c = match store.revoke_computer(computer, rime_remote_core::now_ms()) {
        Ok(c) => c,
        Err(e) => return Reply::Error { message: e.to_string() },
    };
    if let Err(e) = state.save_devices(&store) {
        return Reply::Error {
            message: format!("the computer was not written: {e}"),
        };
    }
    drop(store);
    state.drop_connections_for(&c.id);
    Reply::ComputerRemoved { id: c.id, name: c.name }
}

/// `rime remote computers ping <computer>`: dial it, say `peer_hello`, and
/// report what came back and how long it took.
pub fn ping(state: &State, computer: &str, via: Option<&str>) -> Reply {
    let via = match via {
        None | Some("any") => crate::dialer::Via::Any,
        Some("lan") => crate::dialer::Via::Lan,
        Some("relay") => crate::dialer::Via::Relay,
        Some(other) => {
            return Reply::Error {
                message: format!("--via is lan, relay or any, not {other:?}"),
            }
        }
    };
    let c = match state.devices().map(|s| s.find_computer(computer).cloned()) {
        Ok(Ok(c)) if c.is_active() => c,
        Ok(Ok(c)) => {
            return Reply::Error {
                message: format!("'{}' was removed from this computer", c.name),
            }
        }
        Ok(Err(e)) | Err(e) => return Reply::Error { message: e.to_string() },
    };
    let started = Instant::now();
    let mut link = match crate::dialer::open(state, &c, via) {
        Ok(l) => l,
        Err(message) => return Reply::Error { message },
    };
    let answer = match link.call(br#"{"cmd":"peer_hello"}"#) {
        Ok(v) => v,
        Err(message) => return Reply::Error { message },
    };
    let rtt_ms = started.elapsed().as_millis() as u64;
    if answer.get("reply").and_then(|r| r.as_str()) != Some("peer_hello") {
        return Reply::Error {
            message: format!(
                "'{}' answered, but not to peer_hello: {}",
                c.name,
                crate::dialer::printable(&answer.to_string())
            ),
        };
    }
    if let Ok(mut store) = state.devices() {
        store.seen(&c.id, rime_remote_core::now_ms(), link.path);
        if let Some(a) = &link.address {
            store.computer_reached_at(&c.id, a);
        }
        let _ = state.save_devices(&store);
    }
    Reply::ComputerPong {
        id: c.id,
        name: c.name,
        machine: crate::dialer::printable(answer.get("name").and_then(|n| n.as_str()).unwrap_or("")),
        version: answer.get("version").and_then(|n| n.as_u64()).unwrap_or(0) as u32,
        build: crate::dialer::printable(answer.get("build").and_then(|n| n.as_str()).unwrap_or("")),
        path: link.path.to_string(),
        rtt_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_is_used_once_lives_two_minutes_and_a_new_screen_replaces_the_old() {
        let slot = Mutex::new(None);
        assert!(!has_code(&slot, 0));
        assert!(spend(&slot, 0).is_none(), "a code with no screen open");
        let (id, offer, hear) = open_screen(&slot, 1_000);
        assert!(has_code(&slot, 1_000));
        let (code, tell) = spend(&slot, 2_000).expect("spent");
        assert_eq!(code, offer.code());
        assert!(spend(&slot, 2_000).is_none(), "a code was used twice");
        assert!(!has_code(&slot, 2_000));
        tell.send(Ok(("a".into(), "b".into()))).expect("told");
        assert!(hear.try_recv().is_ok());
        assert!(!close_screen(&slot, id), "a spent code was still open");

        let (_id, offer, _hear) = open_screen(&slot, 10_000);
        assert!(has_code(&slot, offer.expires_ms() - 1));
        assert!(!has_code(&slot, offer.expires_ms()), "an expired code is open");
        assert!(spend(&slot, offer.expires_ms()).is_none(), "an expired code was spent");
        assert!(!has_code(&slot, 10_000), "an expired spend left the code behind");

        let (first, _o, first_hears) = open_screen(&slot, 20_000);
        let (second, _o, _h) = open_screen(&slot, 20_000);
        assert_ne!(first, second);
        assert!(
            matches!(first_hears.try_recv(), Err(mpsc::TryRecvError::Disconnected)),
            "the replaced screen was not told"
        );
        assert!(!close_screen(&slot, first), "closing a replaced screen closed the new one");
        assert!(close_screen(&slot, second));
    }

    #[test]
    fn a_peer_gets_its_two_verbs_and_a_refusal_for_everything_else() {
        let (state, _dir) = crate::serve::tests::a_state("peer-reply");
        let hello: serde_json::Value =
            serde_json::from_slice(&peer_reply(&state, br#"{"cmd":"remote_hello"}"#)).expect("json");
        assert_eq!(hello["scope"], "peer");
        assert_eq!(hello["features"], json!(PEER_FEATURES));
        let ph: serde_json::Value = serde_json::from_slice(&peer_reply(&state, br#"{"cmd":"peer_hello"}"#)).expect("json");
        assert_eq!(ph["reply"], "peer_hello");
        assert_eq!(ph["name"], json!(state.machine));
        for line in [&br#"{"cmd":"list"}"#[..], b"garbage", br#"{"cmd":"pair"}"#, br#"{"nocmd":1}"#] {
            let r: serde_json::Value = serde_json::from_slice(&peer_reply(&state, line)).expect("json");
            assert_eq!(r["kind"], REFUSAL, "{}", String::from_utf8_lossy(line));
        }
    }

    #[test]
    fn a_typed_address_gets_the_default_port_and_garbage_is_refused() {
        let (s, a) = typed_address("192.168.1.9").expect("ip");
        assert_eq!(s, format!("192.168.1.9:{}", crate::DEFAULT_PORT));
        assert_eq!(a.len(), 1);
        assert_eq!(typed_address("127.0.0.1:9").expect("ip:port").0, "127.0.0.1:9");
        assert_eq!(typed_address("::1").expect("v6").0, format!("[::1]:{}", crate::DEFAULT_PORT));
        for bad in ["", "a b", "x\ny"] {
            assert!(typed_address(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn where_a_new_computer_is_dialled_starts_where_it_came_from() {
        let about = About {
            name: "desk".into(),
            lan: vec!["10.0.0.2:7717".into(), "10.0.0.3:7717".into()],
            relay: None,
            port: Some(7717),
        };
        assert_eq!(
            addresses_for(Some("10.0.0.3:7717".into()), &about),
            ["10.0.0.3:7717", "10.0.0.2:7717"]
        );
        assert_eq!(addresses_for(None, &about), ["10.0.0.2:7717", "10.0.0.3:7717"]);
    }
}
