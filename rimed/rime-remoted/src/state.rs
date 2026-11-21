//! What the service holds while it runs.
//!
//! One `State` per process, shared by the control socket, the LAN listener
//! and every device connection. Everything mutable is behind its own mutex,
//! and the locks are held for as long as one map operation takes: a device
//! connection that held the store lock while it proxied a terminal would
//! stall `rime remote revoke`, which is the one command that must never wait.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rime_remote_core::device::{DeviceStore, StoreError};
use rime_remote_core::identity::Identity;
use rime_remote_core::pairing::Offer;
use rime_remote_core::push::PushStore;
use rime_remote_core::rendezvous::{Connection, Path as RemotePath};

use crate::relay::Sources;

/// A live device connection, as far as the rest of the service needs to know.
///
/// Kept so that revoking a device can end the connection it is already
/// holding. Without it, revocation would mean "stop letting it back in",
/// which is not what the owner of a lost phone is asking for.
pub struct Live {
    /// The device this connection authenticated as.
    pub device_id: String,
    /// Which of that device's connections this is.
    ///
    /// Read once when the connection is registered, not on the way out: after
    /// a `shutdown(2)` there is no address to read, so a version that asked
    /// the socket at removal time would match nothing and the list would grow
    /// one entry per connection for the daemon's life.
    pub peer: Option<SocketAddr>,
    /// A handle that closes the socket when asked. `shutdown(2)` rather than
    /// a flag the connection thread checks, because the thread is blocked in
    /// a read and would not check anything until the phone sent something.
    pub socket: std::net::TcpStream,
    /// What the device calls itself, so a listing does not have to go back to
    /// the store for a name it already had.
    pub device_name: String,
    /// Which path this connection arrived on.
    pub path: RemotePath,
    /// Unix milliseconds at which it authenticated.
    pub since_ms: u64,
    /// The most recent measured round trip, written by the frame loop's
    /// `Pong` arm and read by anybody asking for status.
    ///
    /// Shared rather than copied: the measurement happens on the connection's
    /// own thread and the reader is the control socket's, and a value that
    /// had to be pushed somewhere on every ping would be a lock held across
    /// a write.
    pub rtt_ms: Arc<Mutex<Option<u64>>>,
    /// Which of the device's keys opened it: what this connection may do,
    /// and which connections a forgotten or replaced companion key ends.
    pub scope: rime_remote_core::device::Scope,
}

pub struct State {
    /// This machine's long-term keypair.
    pub identity: Identity,
    /// What the QR code calls this machine.
    pub machine: String,
    /// The port the LAN listener is on.
    pub port: u16,
    /// The address the LAN listener is bound to, which decides which address
    /// families a device may be told about.
    pub bound: std::net::IpAddr,
    /// The relay base URL, when the owner configured one.
    ///
    /// Kept as the owner wrote it, because that is what `rime remote status`
    /// shows and what a QR code carries. The parsed form is
    /// [`State::relay_endpoint`].
    pub relay: Option<String>,
    /// Which loopback source ports are relay splices right now.
    ///
    /// A relayed session arrives on this daemon's own listener from
    /// `127.0.0.1`, so the source port is the only thing that distinguishes
    /// it from a LAN session, and the splice arms it before the first byte.
    pub relay_sources: Sources,
    /// Where the device store lives.
    pub store_path: PathBuf,
    /// Where the push registrations live: a **different** file, because they
    /// carry a secret and `devices.json` is asserted not to.
    pub push_path: PathBuf,
    /// Where each phone's last `phone_report` lives, beside the device store.
    pub phones_path: PathBuf,
    /// The open `device_events` channels and the rings waiting on an ack.
    pub events: crate::phone::Events,
    /// What somebody here sent a phone, until it takes it (contract §2.6).
    pub outbox: crate::phone::Outbox,
    /// The messages requests waiting on phones (contract §2.11). Memory only.
    pub messages: crate::messages::Messages,
    /// Each phone's current call and the calls requests waiting on phones
    /// (contract §2.12). Memory only.
    pub calls: crate::calls::Calls,
    /// The photos requests waiting on phones and the thumbnails written for
    /// the Shell (contract §2.13).
    pub photos: Arc<crate::photos::Photos>,
    /// How often an open connection is measured.
    pub ping_interval: std::time::Duration,
    /// The presence each paired computer last gave (§2.15). Memory only.
    pub presence: crate::presence::Board,
    /// Wakes the refresh thread when the paired computers change.
    pub refresh: crate::presence::Wake,
    /// Introductions: peer offers and the phones waiting to be brought.
    pub intro: crate::intro::Intro,
    devices: Mutex<DeviceStore>,
    /// The pairing offer, when one is open. In memory only: a token that
    /// survived a restart would be one the owner did not ask to survive.
    pub offer: Mutex<Option<Offer>>,
    /// The code an "Add a computer" screen is showing, while it is open. In
    /// memory only, like the offer; see `computers.rs`.
    pub showing: Mutex<Option<crate::computers::Showing>>,
    live: Mutex<Vec<Live>>,
    push: Mutex<PushStore>,
    phones: Mutex<crate::phone::PhoneStore>,
    /// The text each phone last put on this computer's clipboard, so it is
    /// not sent straight back (§2.14).
    pub clip_echo: crate::clipboard::Echo,
    /// What sharing with other computers remembers: recent copies for the
    /// echo rule, seqs, rates, pushes under way (§2.15).
    pub sharing: crate::share::Sharing,
    /// Files arriving from other computers: slots, quota, and what each
    /// saved file's notification shows (§2.15).
    pub files: Arc<crate::files::Files>,
    /// Links other computers offered, waiting for Open (§2.15, handoff.rs).
    pub handoff: crate::handoff::Handoff,
    /// Agents across computers (M4, agents.rs): attachments, rates, and what
    /// other computers said they let this one do. Memory only.
    pub agents: crate::agents::Agents,
    /// Which Do Not Disturb was last handed the Shell, per phone (§2.9).
    focus_seen: crate::phone::FocusSeen,
    /// Where a phone's Do Not Disturb is written for the Shell; see
    /// [`State::set_focus_file`].
    focus_file: Mutex<Option<PathBuf>>,
    /// nexd's socket, when `--nexd` moved it; see [`State::nexd`].
    nexd: Mutex<Option<PathBuf>>,
}

impl State {
    pub fn new(
        identity: Identity,
        machine: String,
        port: u16,
        // The address the LAN listener is actually bound to. Passed in rather
        // than assumed: it is what decides which addresses may be advertised.
        // See `State::lan_addresses`.
        bound: std::net::IpAddr,
        relay: Option<String>,
        store_path: PathBuf,
        ping_interval: std::time::Duration,
    ) -> Result<Arc<State>, StoreError> {
        let devices = DeviceStore::load(&store_path)?;
        // Beside the device store, and derived from it rather than passed in,
        // so the two cannot end up in different directories when a test or a
        // second daemon relocates one of them.
        let push_path = store_path
            .parent()
            .map(|d| d.join("push.json"))
            .unwrap_or_else(|| PathBuf::from("push.json"));
        // A registration store that will not parse is NOT fatal, unlike a
        // device store that will not parse: the worst case is that push
        // notifications stop until a phone re-registers, and refusing to start
        // the whole service over it would take LAN remote control down too.
        let push = PushStore::load(&push_path).unwrap_or_else(|e| {
            eprintln!(
                "rime-remoted: {} is unreadable ({e}); push registrations start empty and a \
                 phone will re-register on its next connection",
                push_path.display()
            );
            PushStore::default()
        });
        let phones_path = store_path
            .parent()
            .map(|d| d.join("phones.json"))
            .unwrap_or_else(|| PathBuf::from("phones.json"));
        // Unreadable is not fatal either: the phones report again on their
        // next connection, and until then the Shell shows no battery.
        let phones = crate::phone::PhoneStore::load(&phones_path).unwrap_or_else(|e| {
            eprintln!(
                "rime-remoted: {} is unreadable ({e}); phone reports start empty",
                phones_path.display()
            );
            crate::phone::PhoneStore::default()
        });
        Ok(Arc::new(State {
            identity,
            machine,
            port,
            bound,
            relay,
            store_path,
            push_path,
            phones_path,
            events: crate::phone::Events::default(),
            outbox: crate::phone::Outbox::default(),
            messages: crate::messages::Messages::default(),
            calls: crate::calls::Calls::default(),
            photos: Arc::new(crate::photos::Photos::default()),
            ping_interval,
            presence: crate::presence::Board::default(),
            refresh: crate::presence::Wake::default(),
            intro: crate::intro::Intro::default(),
            relay_sources: Sources::default(),
            devices: Mutex::new(devices),
            offer: Mutex::new(None),
            showing: Mutex::new(None),
            live: Mutex::new(Vec::new()),
            push: Mutex::new(push),
            phones: Mutex::new(phones),
            clip_echo: crate::clipboard::Echo::default(),
            sharing: crate::share::Sharing::default(),
            files: Arc::default(),
            handoff: crate::handoff::Handoff::default(),
            agents: crate::agents::Agents::default(),
            focus_seen: crate::phone::FocusSeen::default(),
            focus_file: Mutex::new(None),
            nexd: Mutex::new(None),
        }))
    }

    /// Whether computer `id` is paired here and not removed.
    pub fn computer_active(&self, id: &str) -> bool {
        self.devices()
            .map(|s| s.computers.get(id).is_some_and(|c| c.is_active()))
            .unwrap_or(false)
    }

    /// Where nexd listens: `--nexd`, else the path nexd itself uses.
    pub fn nexd(&self) -> PathBuf {
        self.nexd
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .unwrap_or_else(crate::nex::default_socket)
    }

    /// Where to write a phone's Do Not Disturb for the Shell. Only `main.rs`
    /// sets it, from the runtime directory, and unset means nothing is
    /// written: the unit tests build a `State` inside the developer's own
    /// session, where `$XDG_RUNTIME_DIR` is the one the real Shell watches,
    /// and a default taken from it would turn on that desktop's Do Not
    /// Disturb from a test.
    pub fn set_focus_file(&self, path: PathBuf) {
        *self.focus_file.lock().unwrap_or_else(|p| p.into_inner()) = Some(path);
    }

    /// Point the bridge at another nexd: `--nexd` at start, or a test.
    pub fn set_nexd(&self, path: PathBuf) {
        *self.nexd.lock().unwrap_or_else(|p| p.into_inner()) = Some(path);
    }

    /// The device store, locked.
    pub fn devices(&self) -> Result<std::sync::MutexGuard<'_, DeviceStore>, StoreError> {
        self.devices
            .lock()
            .map_err(|_| StoreError::Corrupt("the device store lock was poisoned".into()))
    }

    /// Persist a store the caller has changed.
    pub fn save_devices(&self, store: &DeviceStore) -> Result<(), StoreError> {
        store.save(&self.store_path)
    }

    /// How long the open pairing offer has left, or `None`.
    pub fn offer_ms_left(&self, now_ms: u64) -> Option<u64> {
        let offer = self.offer.lock().ok()?;
        let offer = offer.as_ref()?;
        offer
            .is_live(now_ms)
            .then(|| offer.expires_ms().saturating_sub(now_ms))
    }

    /// Remember a live connection.
    pub fn register(&self, live: Live) {
        if let Ok(mut v) = self.live.lock() {
            v.push(live);
        }
    }

    /// Forget one connection, leaving the device's others alone.
    ///
    /// The first version of this ended its condition with `|| peer.is_none()`
    /// and was called with `None`, so the retain predicate was true for every
    /// entry and nothing was ever removed. A leak rather than a hole, and the
    /// kind that is invisible until a daemon has been up for a week.
    pub fn unregister(&self, device_id: &str, peer: Option<SocketAddr>) {
        if let Ok(mut v) = self.live.lock() {
            v.retain(|l| !(l.device_id == device_id && l.peer == peer));
        }
    }




    /// Record a phone's report. The id is the handshake's.
    ///
    /// A changed Do Not Disturb from a phone that said `focus` is written for
    /// the Shell too, under the same lock, so two reports racing from two
    /// connections land in `focus.json` in the order they were stored.
    pub fn phone_report(&self, device_id: &str, report: crate::phone::PhoneReport) {
        let mut store = self.phones.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(dnd) = self.focus_seen.change(device_id, &report) {
            let path = self.focus_file.lock().unwrap_or_else(|p| p.into_inner()).clone();
            if let Some(path) = path {
                if let Err(e) = crate::phone::write_focus(&path, dnd, device_id, report.reported_ms) {
                    eprintln!("rime-remoted: could not write {}: {e}", path.display());
                }
            }
        }
        // Calls switched off on the phone: what it said about a call goes,
        // and a ringing notification closes, at once.
        let calls_off = !report.can_calls();
        // Photos switched off: the thumbnails written for it go too.
        let photos_off = !report.can_photos();
        store.phones.insert(device_id.to_string(), report);
        if let Err(e) = store.save(&self.phones_path) {
            eprintln!("rime-remoted: could not write {}: {e}", self.phones_path.display());
        }
        drop(store);
        if calls_off {
            self.calls.off(device_id, crate::mirror::mirror());
        }
        if photos_off {
            self.photos.off(device_id);
        }
    }

    /// Every phone's last report.
    pub fn phones(&self) -> std::collections::BTreeMap<String, crate::phone::PhoneReport> {
        self.phones.lock().unwrap_or_else(|e| e.into_inner()).phones.clone()
    }

    /// Forget what a phone reported, close its events channel and drop what was
    /// waiting for it: revoke.
    pub fn phone_forget(&self, device_id: &str) {
        self.events.forget(device_id);
        self.outbox.forget(device_id);
        self.messages.forget(device_id);
        self.calls.forget(device_id);
        self.photos.forget(device_id);
        crate::mirror::mirror().forget(device_id);
        crate::media::media().forget(device_id);
        self.focus_seen.forget(device_id);
        self.clip_echo.forget(device_id);
        let mut store = self.phones.lock().unwrap_or_else(|e| e.into_inner());
        if store.phones.remove(device_id).is_some() {
            if let Err(e) = store.save(&self.phones_path) {
                eprintln!("rime-remoted: could not write {}: {e}", self.phones_path.display());
            }
        }
    }

    /// The push registrations, locked.
    pub fn push_store(&self) -> std::sync::MutexGuard<'_, PushStore> {
        // A poisoned lock here means a thread panicked holding it. Recovering
        // the guard is right rather than propagating: the alternative is that
        // one panic in a delivery turns push off for the life of the daemon,
        // and the data behind the lock is a map that is written whole.
        self.push.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether anything is registered at all.
    ///
    /// Checked before the watcher does any work, so a machine nobody has
    /// registered a phone with costs one map lookup per poll.
    pub fn push_is_empty(&self) -> bool {
        self.push_store().registrations.is_empty()
    }

    /// Record a device's endpoint and key. The device id comes from the
    /// handshake and never from the request body.
    pub fn push_register(
        &self,
        device_id: &str,
        endpoint: &str,
        key: &str,
        now_ms: u64,
    ) -> Result<(), rime_remote_core::push::PushError> {
        let mut store = self.push_store();
        store.register(device_id, endpoint, key, now_ms)?;
        self.persist_push(&store);
        Ok(())
    }

    /// Forget a device's registration. Called on revoke as well as on
    /// unregister — a revoked phone must stop being notified, and a
    /// registration that outlived the pairing would keep waking a device the
    /// owner threw off this machine.
    pub fn push_forget(&self, device_id: &str) -> bool {
        let mut store = self.push_store();
        let gone = store.forget(device_id);
        if gone {
            self.persist_push(&store);
        }
        gone
    }

    /// Draw the next sequence number for a device, persisting it.
    ///
    /// Persisted on every draw rather than at shutdown, because the phone's
    /// replay guard is "a sequence greater than the last I saw" and a daemon
    /// that was killed would otherwise rewind and have every envelope it sent
    /// afterwards correctly ignored.
    pub fn push_next_seq(&self, device_id: &str) -> Option<u64> {
        let mut store = self.push_store();
        let seq = store.next_seq(device_id)?;
        self.persist_push(&store);
        Some(seq)
    }

    /// Record a delivery that worked.
    pub fn push_delivered(&self, device_id: &str) {
        let mut store = self.push_store();
        if let Some(r) = store.registrations.get_mut(device_id) {
            if r.failures == 0 {
                return;
            }
            r.failures = 0;
            self.persist_push(&store);
        }
    }

    /// Record a delivery that did not, and answer how many in a row that is.
    pub fn push_failed(&self, device_id: &str) -> u32 {
        let mut store = self.push_store();
        let Some(r) = store.registrations.get_mut(device_id) else {
            return 0;
        };
        r.failures = r.failures.saturating_add(1);
        let n = r.failures;
        self.persist_push(&store);
        n
    }

    fn persist_push(&self, store: &PushStore) {
        if let Err(e) = store.save(&self.push_path) {
            eprintln!(
                "rime-remoted: could not write {}: {e}",
                self.push_path.display()
            );
        }
    }

    /// End every connection a device is holding.
    ///
    /// `shutdown` on the socket, which unblocks the reading thread with an
    /// end of file. A flag the thread checked would do nothing until the
    /// phone next sent a byte, and a phone in someone else's pocket may not
    /// send one for hours.
    pub fn drop_connections_for(&self, device_id: &str) {
        if let Ok(mut v) = self.live.lock() {
            v.retain(|l| {
                if l.device_id == device_id {
                    let _ = l.socket.shutdown(std::net::Shutdown::Both);
                    false
                } else {
                    true
                }
            });
        }
    }

    /// End a device's companion-scope connections and leave its full ones.
    ///
    /// For `companion_forget` and a replaced companion key: the key that
    /// opened them no longer opens anything, so neither should the sessions
    /// it already opened.
    pub fn drop_companion_connections_for(&self, device_id: &str) {
        if let Ok(mut v) = self.live.lock() {
            v.retain(|l| {
                if l.device_id == device_id && l.scope == rime_remote_core::device::Scope::Companion {
                    let _ = l.socket.shutdown(std::net::Shutdown::Both);
                    false
                } else {
                    true
                }
            });
        }
    }

    /// Every connection open right now, with its path and its round trip.
    ///
    /// P1-052's last criterion. Live only: nothing here is read back from the
    /// device store, because a quality measured during a session that has
    /// ended is not a fact about now and a page that showed one would be
    /// telling the owner about a connection that does not exist.
    pub fn connections(&self) -> Vec<Connection> {
        let Ok(live) = self.live.lock() else {
            return Vec::new();
        };
        live.iter()
            .map(|l| Connection {
                device_id: l.device_id.clone(),
                device_name: l.device_name.clone(),
                path: l.path.as_str().to_string(),
                since_ms: l.since_ms,
                rtt_ms: l.rtt_ms.lock().ok().and_then(|v| *v),
            })
            .collect()
    }

    /// How a connection from this address reached the machine.
    pub fn path_of(&self, peer: Option<SocketAddr>) -> RemotePath {
        match peer {
            Some(addr) if arrived_by_relay(addr, |p| self.relay_sources.holds(p)) => {
                RemotePath::Relay
            }
            _ => RemotePath::Lan,
        }
    }

    /// The addresses a device on this network could dial.
    ///
    /// Every non-loopback address this machine has, with the listener's port.
    /// Loopback is excluded because it is never reachable from a phone and
    /// putting it in a QR code costs a device one failed connection attempt
    /// before it moves on.
    ///
    /// **Through `SocketAddr`, not `format!("{ip}:{port}")`**, and that is the
    /// whole content of this function. `format!` on an `IpAddr::V6` produces
    /// `fd00:…:3d31:47717` — an address with nine colon-separated groups,
    /// which is not an IPv6 address at all and not a `host:port` either. The
    /// Android client's `splitHostPort` reads more than one colon and no
    /// brackets as an unbracketed literal, hands the WHOLE string to
    /// `InetSocketAddress` as a host, and falls back to port 7717; the lookup
    /// fails and the address is skipped.
    ///
    /// So every pairing code this machine has ever produced has advertised an
    /// IPv6 address no phone could dial. It has never been noticed because the
    /// IPv4 address is tried first and answers — which is exactly how it would
    /// stay hidden until somebody paired on a network that had only the one.
    /// MEASURED on a Pixel 7a against this daemon, not reasoned about: see
    /// `android/app/src/androidTest/.../LanEndToEndTest.kt`, which dials every
    /// address in the offer it just scanned and names the ones that refuse.
    ///
    /// `SocketAddr`'s own `Display` brackets a V6 and leaves a V4 alone, which
    /// is the `host:port` grammar both ends already agree on.
    ///
    /// ## And only the families the listener actually accepts
    ///
    /// The bracketing above was half the defect. The other half is that
    /// `main.rs` binds `0.0.0.0` — IPv4 only — while this function offered
    /// every address the machine has, IPv6 included. So a pairing code
    /// advertised a port on an address where nothing was listening: a phone
    /// that reached it got `ECONNREFUSED`, and on a dual-stack network it was
    /// invisible because the IPv4 address is tried first and answers.
    ///
    /// Measured from a Pixel 7a, twice: before the bracketing fix the IPv6
    /// entry did not even parse, and after it the phone connected to the
    /// address and was refused. Filtering here rather than hardcoding "v4
    /// only" means the day the listener binds `::` as well, the advertisement
    /// follows without anybody remembering to change it.
    pub fn lan_addresses(&self) -> Vec<String> {
        crate::net::local_addresses()
            .into_iter()
            .filter(|ip| reachable_on(self.bound, *ip))
            .map(|ip| advertise(ip, self.port))
            .collect()
    }
}

/// Whether a connection from `peer` arrived through a relay splice.
///
/// Relay only when BOTH halves say so: the address is loopback *and* its port
/// is one a splice has armed. The kernel reuses ephemeral ports, and a check on
/// the port alone would eventually label a LAN session — or a local one
/// somebody made by hand — as relayed, which is a lie in the direction that
/// matters, because `Path::disclosure` tells the owner a third party was
/// involved.
///
/// ## `to_canonical`, which is the whole reason this is a function of its own
///
/// On a dual-stack listener every IPv4 peer arrives as `::ffff:a.b.c.d`, and
/// `Ipv6Addr::is_loopback` is **false** for `::ffff:127.0.0.1` — it answers
/// only for `::1`. So the moment the listener moved from `0.0.0.0` to `::`,
/// every relayed session on this machine would have been recorded as a LAN
/// one, and the owner would have been told nobody else was on the path when
/// somebody was. That is the regression `main.rs` named and declined to take
/// blind; this is the measurement it asked for, and
/// `a_v4_mapped_relay_splice_is_a_relay_session` is where it fails without
/// this line.
///
/// `armed` is passed as a closure rather than the `Sources` itself so the rule
/// can be asserted without a `State`, which needs an identity and a device
/// store on disk to exist.
pub fn arrived_by_relay(peer: SocketAddr, armed: impl Fn(u16) -> bool) -> bool {
    peer.ip().to_canonical().is_loopback() && armed(peer.port())
}

/// A peer address with any IPv4-mapped form flattened back to plain IPv4.
///
/// Applied ONCE, where `serve.rs` reads `peer_addr()`, so that everything
/// downstream — the relay/LAN decision, the address in `rime remote status`,
/// and the key `unregister` matches a closing connection on — sees the same
/// value. Canonicalising in each of those instead would leave the next one
/// added to be forgotten, and the failure of forgetting is silent.
pub fn canonical(peer: Option<SocketAddr>) -> Option<SocketAddr> {
    peer.map(|a| SocketAddr::new(a.ip().to_canonical(), a.port()))
}

/// Whether a peer dialling `candidate` could reach a listener bound to `bound`.
///
/// A socket bound to `0.0.0.0` accepts IPv4 and nothing else; one bound to
/// `::` accepts IPv6 and, on Linux with the default `net.ipv6.bindv6only=0`,
/// IPv4-mapped connections too. A socket bound to one specific address accepts
/// only that address.
pub fn reachable_on(bound: std::net::IpAddr, candidate: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match bound {
        IpAddr::V4(b) if b.is_unspecified() => candidate.is_ipv4(),
        IpAddr::V6(b) if b.is_unspecified() => true,
        other => other == candidate,
    }
}

/// One address as a device should read it: `host:port`, IPv6 bracketed.
///
/// A named function rather than a closure so the rule can be asserted without
/// a running daemon and without a machine that happens to have an IPv6
/// address — which is what let the unbracketed version ship.
pub fn advertise(ip: std::net::IpAddr, port: u16) -> String {
    SocketAddr::new(ip, port).to_string()
}

/// Where the control socket lives.
pub fn control_socket_in(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("rime-remoted").join("control.sock")
}

/// Where the control socket lives for this user.
pub fn control_socket() -> PathBuf {
    control_socket_in(&rime_agent_core::paths::runtime_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_v4_mapped_relay_splice_is_a_relay_session() {
        // The regression that kept this daemon on `0.0.0.0`, written down as
        // the test `main.rs` said it needed.
        //
        // A relay splice connects to the listener over loopback, and on a
        // dual-stack listener that arrives as `::ffff:127.0.0.1`. Without
        // `to_canonical` this is not loopback — `Ipv6Addr::is_loopback`
        // answers only for `::1` — so the session is recorded as LAN and the
        // owner is told nobody else was on the path.
        let spliced: SocketAddr = "[::ffff:127.0.0.1]:41000".parse().expect("mapped loopback");
        assert!(
            arrived_by_relay(spliced, |p| p == 41000),
            "a splice arriving IPv4-mapped is still a splice"
        );
        // The v4 form of the same thing, which is what a listener on 0.0.0.0
        // sees and what has always worked.
        let plain: SocketAddr = "127.0.0.1:41000".parse().expect("loopback");
        assert!(arrived_by_relay(plain, |p| p == 41000));
        // And the real IPv6 loopback, which a v6 splice would use.
        let six: SocketAddr = "[::1]:41000".parse().expect("v6 loopback");
        assert!(arrived_by_relay(six, |p| p == 41000));

        // Both halves still have to say so. These are the lies in the other
        // direction and they matter more: a LAN session labelled `relay`
        // tells the owner a third party carried their terminal when none did.
        assert!(
            !arrived_by_relay(spliced, |_| false),
            "a loopback connection on a port no splice armed is not a relay"
        );
        let lan: SocketAddr = "[::ffff:192.168.1.98]:41000".parse().expect("mapped lan");
        assert!(
            !arrived_by_relay(lan, |p| p == 41000),
            "a phone on the LAN is not a relay however its address is written"
        );
    }

    #[test]
    fn the_peer_address_is_flattened_once_at_the_boundary() {
        // What `serve.rs` stores, and therefore what `rime remote status`
        // prints and what `unregister` matches on. A device list showing
        // `::ffff:192.168.1.98` for a phone that is plainly on 192.168.1.98
        // is the same address written in a way its owner cannot recognise.
        let mapped: SocketAddr = "[::ffff:192.168.1.98]:55000".parse().expect("mapped");
        assert_eq!(
            canonical(Some(mapped)),
            Some("192.168.1.98:55000".parse().expect("v4")),
        );
        // A genuine IPv6 peer is left exactly as it is: it is not a v4 address
        // written differently, and rewriting it would lose it.
        let six: SocketAddr = "[fd00::1]:55000".parse().expect("v6");
        assert_eq!(canonical(Some(six)), Some(six));
        assert_eq!(canonical(None), None);
    }

    #[test]
    fn nothing_is_advertised_on_a_family_the_listener_does_not_accept() {
        let v4: std::net::IpAddr = "192.168.1.232".parse().expect("v4");
        let v6: std::net::IpAddr = "fd00::1".parse().expect("v6");
        let any4: std::net::IpAddr = "0.0.0.0".parse().expect("any4");
        let any6: std::net::IpAddr = "::".parse().expect("any6");

        // The defect, stated as a rule: a listener on 0.0.0.0 has nothing on
        // any IPv6 address, so a pairing code must not name one.
        assert!(reachable_on(any4, v4));
        assert!(!reachable_on(any4, v6), "0.0.0.0 does not accept IPv6");

        // A dual-stack listener reaches both, which is what makes this a
        // filter rather than a hardcoded "IPv4 only".
        assert!(reachable_on(any6, v4));
        assert!(reachable_on(any6, v6));

        // And a listener pinned to one address advertises that one only.
        assert!(reachable_on(v4, v4));
        assert!(!reachable_on(v4, "192.168.1.9".parse().expect("v4")));
    }

    #[test]
    fn an_advertised_ipv6_address_is_bracketed_and_an_ipv4_one_is_not() {
        // The defect: `format!("{ip}:{port}")` on a V6 produces nine
        // colon-separated groups, which is neither an address nor a
        // `host:port`. A phone reads the whole thing as a host, fails to
        // resolve it, and skips the address — silently, because the V4
        // address in the same offer answers first.
        let v6: std::net::IpAddr = "fd00:12b:83b0:c4de:a38b:4ef2:b234:3d31".parse().expect("v6");
        assert_eq!(
            advertise(v6, 47717),
            "[fd00:12b:83b0:c4de:a38b:4ef2:b234:3d31]:47717"
        );
        // And the V4 spelling is untouched: bracketing one would break every
        // client that already parses it.
        let v4: std::net::IpAddr = "192.168.1.232".parse().expect("v4");
        assert_eq!(advertise(v4, 47717), "192.168.1.232:47717");

        // The property the phone's parser actually depends on: outside the
        // brackets there is exactly one colon, so "the last colon separates
        // the port" is true for both families.
        for text in [advertise(v6, 7717), advertise(v4, 7717)] {
            let tail = text.rsplit(']').next().expect("a tail");
            assert_eq!(
                tail.matches(':').count(),
                1,
                "{text} has no single port separator"
            );
        }
    }

    #[test]
    fn a_connection_is_registered_and_then_forgotten() {
        // The leak this had: `unregister` ended its condition with
        // `|| peer.is_none()` and was called with `None`, so the predicate
        // was true for every entry and the list only ever grew.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let addr = listener.local_addr().expect("addr");
        // The ACCEPTED sockets, which is what the daemon holds: their
        // `peer_addr` is the client's ephemeral port and differs per
        // connection. The client-side sockets all report the listener's one
        // address, which is what made the first version of this test assert
        // two equal values.
        let _c1 = std::net::TcpStream::connect(addr).expect("connect");
        let _c2 = std::net::TcpStream::connect(addr).expect("connect");
        let (s1, _) = listener.accept().expect("accept");
        let (s2, _) = listener.accept().expect("accept");
        let p1 = s1.peer_addr().ok();
        let p2 = s2.peer_addr().ok();
        assert_ne!(p1, p2, "two connections share an address");

        let sample = |peer, socket| Live {
            device_id: "d".into(),
            peer,
            socket,
            device_name: "a phone".into(),
            path: RemotePath::Lan,
            since_ms: 0,
            rtt_ms: Arc::new(Mutex::new(None)),
            scope: rime_remote_core::device::Scope::Full,
        };
        let live = Mutex::new(vec![sample(p1, s1), sample(p2, s2)]);
        // Exercised through the same predicate the daemon uses, over a
        // standalone list so no daemon has to be started.
        let remove = |v: &mut Vec<Live>, id: &str, peer: Option<SocketAddr>| {
            v.retain(|l| !(l.device_id == id && l.peer == peer));
        };
        let mut v = live.into_inner().expect("lock");
        remove(&mut v, "d", p1);
        assert_eq!(v.len(), 1, "the wrong number of connections survived");
        assert_eq!(v[0].peer, p2, "the wrong connection was removed");
        remove(&mut v, "d", p2);
        assert!(v.is_empty());
    }

    #[test]
    fn the_control_socket_is_beside_the_agent_runtimes_and_not_in_it() {
        // A separate directory, because the two daemons have separate
        // lifetimes: `rime agent enable` starts one and does not start the
        // other, and a socket inside the other's directory would make the
        // second depend on the first having run.
        let dir = Path::new("/run/user/1000");
        let mine = control_socket_in(dir);
        assert!(mine.ends_with("rime-remoted/control.sock"), "{mine:?}");
        assert_ne!(mine, rime_agent_core::paths::control_socket());
    }

    #[test]
    fn forgetting_a_companion_key_ends_its_sessions_and_leaves_the_full_one() {
        use rime_remote_core::device::Scope;
        use std::io::Read;
        let dir = std::env::temp_dir().join(format!(
            "rime-state-companion-{}-{}",
            std::process::id(),
            rime_remote_core::now_ms()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let state = State::new(
            rime_remote_core::identity::Identity::generate().expect("identity"),
            "m".into(),
            47717,
            std::net::IpAddr::from([0u8; 16]),
            None,
            dir.join("devices.json"),
            crate::serve::PING_INTERVAL,
        )
        .expect("state");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let mut far = Vec::new();
        for (id, scope) in [("d", Scope::Full), ("d", Scope::Companion), ("e", Scope::Companion)] {
            let c = std::net::TcpStream::connect(addr).expect("connect");
            let (s, _) = listener.accept().expect("accept");
            state.register(Live {
                device_id: id.into(),
                peer: s.peer_addr().ok(),
                socket: s,
                device_name: "a phone".into(),
                path: RemotePath::Lan,
                since_ms: 0,
                rtt_ms: Arc::new(Mutex::new(None)),
                scope,
            });
            c.set_read_timeout(Some(std::time::Duration::from_millis(300))).ok();
            far.push(c);
        }
        state.drop_companion_connections_for("d");
        let left: Vec<String> = state.connections().into_iter().map(|c| c.device_id).collect();
        assert_eq!(left, ["d", "e"], "the wrong connections survived");
        let mut b = [0u8; 1];
        assert!(matches!(far[1].read(&mut b), Ok(0)), "the companion connection is still open");
        assert!(far[0].read(&mut b).is_err(), "the full connection was closed");
        let _ = std::fs::remove_dir_all(dir);
    }
}
