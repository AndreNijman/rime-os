//! Introductions: computers introduced to each other through the phone, and
//! the phone joining a new computer by itself (contract §2.15).
//!
//! ## Who may introduce
//!
//! Only a phone session of scope Full whose last report, sent on a Full
//! session, lists [`CAN_JOIN`]: the app reports it while its switch "Join my
//! other computers automatically" is on. Companion sessions never reach these
//! verbs (they are not on `companion::ALLOWED_VERBS`), peers never reach them
//! (`computers::peer_reply` answers first), and agents and scheduled jobs have
//! no phone session at all.
//!
//! ## Flow A: the phone introduces two computers it holds
//!
//! The phone asks each computer for its card ([`card`]: key, name, addresses,
//! port, relay, and the keys of every computer it knows in any state). For
//! two computers that know nothing of each other it sends each the other's
//! card (`computer_introduce`). Each records the other as an introduced
//! computer ([`DeviceStore::introduce_computer`]), says so in a notification
//! with Remove, and the refresh (`presence.rs`) dials it. The card's
//! addresses must all be dialable (M1a's rule) and only this computer's own
//! relay is ever dialled (`dialer.rs`).
//!
//! ## Flow B: a computer brings its phone to a new member
//!
//! When a phone with a Full events channel reports [`CAN_JOIN`], this
//! computer asks each computer it trusts (paired with a code, or allowed),
//! once per phone per run, for an offer over the peer session
//! ([`PEER_VERB`]), and hands it to the phone as event [`JOIN_EVENT`]. The
//! phone pairs by the existing protocol. The computer that minted the offer
//! records the phone as introduced: its own key opens a companion session
//! until a person there runs `rime remote devices allow`.
//!
//! `peer_offer` is answered only for a peer that is trusted here; at most one
//! offer per peer is outstanding; at most [`MAX_BROUGHT`] phones per peer for
//! the life of its pairing, revoked ones counted; each offer is single use
//! and lives [`PEER_OFFER_TTL_MS`].
//!
//! ## Removals
//!
//! A computer this one removes, by hand or by a removal passed on, is told to
//! every phone with a Full events channel as [`REMOVED_EVENT`], so the phone
//! forgets it too.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Condvar, Mutex};

use rime_remote_core::computer::About;
use rime_remote_core::device::{Device, DeviceStore, Introduced, Introducer, Scope};
use rime_remote_core::pairing::{Offer, PairingError, PairingOffer, PairingRequest, PEER_OFFER_TTL_MS};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::state::State;

/// The phone verbs this module answers. Full scope only.
pub const VERBS: [&str; 2] = ["computer_card", "computer_introduce"];

/// What `remote_hello` lists, so the phone knows to introduce.
pub const FEATURES: [&str; 1] = ["computer_introductions"];

/// The one peer verb: another computer asks for an offer for its phone.
pub const PEER_VERB: &str = "peer_offer";

/// The `can` word a phone reports while its auto-join switch is on.
pub const CAN_JOIN: &str = "join";

/// The event that hands the phone an offer from another computer.
pub const JOIN_EVENT: &str = "join_offer";

/// The event that tells the phone a computer was removed.
pub const REMOVED_EVENT: &str = "device_removed";

/// At most this many phones one peer brings, for the life of its pairing.
pub const MAX_BROUGHT: usize = 4;

/// The key of the "joined" notification, with a `:` so no phone can post one.
pub const JOINED_KEY: &str = "intro:joined";

/// The action on it.
pub const REMOVE_ACTION: &str = "remove";

/// The refusal kind for a request that is not allowed.
pub const REFUSAL: &str = "not_allowed";

/// A computer's card: what a phone carries to another computer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub key: String,
    #[serde(flatten)]
    pub about: About,
}

/// An offer minted for a peer's phone.
struct PeerOffer {
    by: String,
    offer: Offer,
}

/// One `(phone, computer)` pair's last try.
#[derive(Debug, Clone, Copy)]
struct Asked {
    at_ms: u64,
    delivered: bool,
}

/// How long a try that reached nobody waits before the next: the other
/// computer may be asleep, or the phone may have left. A phone reports on
/// every battery step, so without this each report would dial every computer.
pub const RETRY_MS: u64 = 10 * 60 * 1000;

/// How long after an offer reached the phone the next may be sent. The phone
/// says nothing back: it may hold the computer already, or the offer may have
/// run out before the person opened the app, and only another offer helps
/// then. An hour keeps a phone that holds every computer from being a dial
/// on each report.
pub const DELIVERED_RETRY_MS: u64 = 60 * 60 * 1000;

/// What this module keeps in memory.
#[derive(Default)]
pub struct Intro {
    /// Offers peers asked for, at most one live per peer.
    offers: Mutex<Vec<PeerOffer>>,
    /// `(phone, computer)` pairs this run: when last tried, and whether an
    /// offer reached the phone.
    asked: Mutex<HashMap<(String, String), Asked>>,
    /// Phones waiting for [`supervise`].
    queue: Mutex<BTreeSet<String>>,
    wake: Condvar,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Intro {
    /// Phone `id` may be brought to computers now (a Full report saying join).
    pub fn wake(&self, phone: &str) {
        lock(&self.queue).insert(phone.to_string());
        self.wake.notify_all();
    }

    fn next(&self) -> BTreeSet<String> {
        let mut q = lock(&self.queue);
        while q.is_empty() {
            q = self.wake.wait(q).unwrap_or_else(|p| p.into_inner());
        }
        std::mem::take(&mut *q)
    }

    /// Whether `(phone, computer)` may be tried at `now_ms`: never once an
    /// offer reached the phone this run, and not again within [`RETRY_MS`] of
    /// a try that did not. Marks the try.
    fn may_try(&self, phone: &str, computer: &str, now_ms: u64) -> bool {
        let mut asked = lock(&self.asked);
        let key = (phone.to_string(), computer.to_string());
        if let Some(a) = asked.get(&key) {
            let wait = if a.delivered { DELIVERED_RETRY_MS } else { RETRY_MS };
            if now_ms.saturating_sub(a.at_ms) < wait {
                return false;
            }
        }
        asked.insert(key, Asked { at_ms: now_ms, delivered: false });
        true
    }

    /// An offer from `computer` reached phone `phone`: not tried again this run.
    fn delivered(&self, phone: &str, computer: &str) {
        if let Some(a) = lock(&self.asked).get_mut(&(phone.to_string(), computer.to_string())) {
            a.delivered = true;
        }
    }
}

fn refusal(message: &str) -> Vec<u8> {
    json!({"reply": "error", "kind": REFUSAL, "message": message}).to_string().into_bytes()
}

fn bad_request(message: &str) -> Vec<u8> {
    json!({"reply": "error", "kind": "bad_request", "message": message}).to_string().into_bytes()
}

/// Whether phone `device_id`, on a session of `scope`, may introduce: Full
/// scope, a record paired here or allowed, and its last report says join.
pub fn may_introduce(state: &State, device_id: &str, scope: Scope) -> Result<(), String> {
    if scope != Scope::Full {
        return Err("only an unlocked phone may introduce computers".into());
    }
    let trusted =
        state.devices().ok().and_then(|s| s.find(device_id).ok().map(|d| d.is_active() && d.trusted())).unwrap_or(false);
    if !trusted {
        return Err("this phone is not allowed on this computer yet".into());
    }
    if !state.phones().get(device_id).is_some_and(|r| r.can_join()) {
        return Err("\"Join my other computers automatically\" is off on this phone".into());
    }
    Ok(())
}

/// `computer_card` and `computer_introduce`, from phone `device_id`.
pub fn control(state: &State, device_id: &str, scope: Scope, line: &[u8]) -> Option<Vec<u8>> {
    let v: Value = serde_json::from_slice(line).ok()?;
    let cmd = v.get("cmd")?.as_str()?;
    if !VERBS.contains(&cmd) {
        return None;
    }
    if let Err(why) = may_introduce(state, device_id, scope) {
        return Some(refusal(&why));
    }
    Some(match cmd {
        "computer_card" => card(state),
        "computer_introduce" => introduce(state, device_id, &v["card"]),
        _ => return None,
    })
}

/// This computer's card, and the keys of every computer it knows: paired,
/// or removed and not forgotten. A forgotten removal is not known: the
/// person here lifted it, so a phone may introduce that computer again.
fn card(state: &State) -> Vec<u8> {
    let known: Vec<String> = state
        .devices()
        .map(|s| {
            s.computers.values().filter(|c| c.is_active() || c.is_tombstone()).map(|c| c.public_key.clone()).collect()
        })
        .unwrap_or_default();
    let card = Card { key: state.identity.public_key(), about: crate::computers::own_about(state) };
    json!({"reply": "computer_card", "card": card, "known": known}).to_string().into_bytes()
}

/// A card a phone brought, checked: a key, and an account of the computer
/// that passes M1a's rule (a printable name, dialable addresses, a relay
/// that is one). Anything else is refused, not trimmed.
pub fn checked_card(v: &Value) -> Result<([u8; 32], About), String> {
    let card: Card = serde_json::from_value(v.clone()).map_err(|e| format!("not a computer's card: {e}"))?;
    let key = rime_remote_core::device::check_key(&card.key).map_err(|e| e.to_string())?;
    let about = card.about.checked().map_err(|e| e.to_string())?;
    Ok((key, about))
}

/// `computer_introduce`: record the computer on the card, introduced by
/// phone `device_id`.
fn introduce(state: &State, device_id: &str, card: &Value) -> Vec<u8> {
    let (key, about) = match checked_card(card) {
        Ok(c) => c,
        Err(why) => return bad_request(&why),
    };
    let mut store = match state.devices() {
        Ok(s) => s,
        Err(e) => return bad_request(&e.to_string()),
    };
    let Ok(phone) = store.find(device_id).map(|d| d.name.clone()) else {
        return refusal("this phone is not paired here");
    };
    let by = Introducer { id: device_id.to_string(), name: phone.clone() };
    let own = state.identity.public_bytes();
    let outcome =
        store.introduce_computer(&key, &own, &about.name, about.lan.clone(), about.relay.clone(), by, rime_remote_core::now_ms());
    let added = match outcome {
        Ok(Introduced::Added(c)) => c,
        Ok(Introduced::AlreadyPaired) => return answer("already_paired", None),
        Ok(Introduced::Removed) => return answer("removed", None),
        Err(e) => return bad_request(&e.to_string()),
    };
    if let Err(e) = state.save_devices(&store) {
        return bad_request(&format!("the computer was not written: {e}"));
    }
    drop(store);
    state.refresh.poke();
    notify_joined(&added, &phone, true);
    answer("added", Some(&added.id))
}

fn answer(outcome: &str, id: Option<&str>) -> Vec<u8> {
    let mut v = json!({"reply": "computer_introduced", "outcome": outcome});
    if let Some(id) = id {
        v["id"] = json!(id);
    }
    v.to_string().into_bytes()
}

/// The notification for a device that joined, with Remove: "<Laptop> joined
/// your devices, added by <Pixel>".
fn notify_joined(joined: &Device, by: &str, computer: bool) {
    let shown = joined_shown(&joined.name, by, computer);
    crate::mirror::mirror().submit(crate::mirror::Job::Post { device: joined.id.clone(), key: JOINED_KEY.into(), shown });
}

/// What the "joined" notification shows. Both names come from elsewhere (a
/// card, a pairing request), so each is made safe for a notification server
/// that draws markup, as a phone's notification is.
fn joined_shown(name: &str, by: &str, computer: bool) -> crate::mirror::Shown {
    let name = crate::mirror::clean(name, crate::mirror::APP_CAP, 0, false);
    let by = crate::mirror::clean(by, crate::mirror::APP_CAP, 0, true);
    crate::mirror::Shown {
        app_name: "Rime Remote".into(),
        summary: if computer { format!("{name} joined your devices") } else { format!("{name} joined this computer") },
        body: if computer {
            format!(
                "Added by {by}. It can see this computer's status until you remove it; run `rime remote computers \
                 allow` to trust it fully."
            )
        } else {
            format!(
                "Added by {by}. It can use phone features only until you run `rime remote devices allow` here."
            )
        },
        reply: None,
        actions: vec![(REMOVE_ACTION.to_string(), "Remove".to_string())],
        sticky: false,
    }
}

/// Remove pressed on a "joined" notification for record `id`: remove the
/// computer, or revoke the phone. The press comes from the notification
/// server's bus owner, the person at this computer (contract §2.7); it can
/// only take something away, never allow it.
pub fn on_action(state: &State, id: &str, key: &str, action: &str) -> bool {
    if key != JOINED_KEY || action != REMOVE_ACTION {
        return false;
    }
    // Only a record that joined: a notification that outlived it (the same
    // key paired with a code since) removes nothing a person made.
    let (computer, phone) = match state.devices() {
        Ok(s) => (
            s.computers.get(id).is_some_and(|c| c.introduced_by.is_some()),
            s.devices.get(id).is_some_and(|d| d.introduced_by.is_some()),
        ),
        Err(_) => (false, false),
    };
    if computer {
        let _ = crate::computers::remove(state, id);
    } else if phone {
        let _ = crate::control::revoke(state, id);
    }
    true
}

/// `peer_offer` from computer `peer`: an offer for its phone, or why not.
pub fn answer_peer_offer(state: &State, peer: &str) -> Vec<u8> {
    answer_peer_offer_at(state, peer, rime_remote_core::now_ms())
}

/// [`answer_peer_offer`] at `now`: the offer lives [`PEER_OFFER_TTL_MS`] from
/// then, and [`complete`] redeems it only before that.
fn answer_peer_offer_at(state: &State, peer: &str, now: u64) -> Vec<u8> {
    // Offers first, then the store, here and in `complete`: one order.
    let mut offers = lock(&state.intro.offers);
    offers.retain(|o| o.offer.is_live(now));
    let store = match state.devices() {
        Ok(s) => s,
        Err(e) => return bad_request(&e.to_string()),
    };
    if let Err(why) = may_ask_offer(&store, peer, &offers) {
        return refusal(&why);
    }
    drop(store);
    let offer = Offer::lasting(now, PEER_OFFER_TTL_MS);
    let payload = PairingOffer {
        v: rime_remote_core::REMOTE_PROTOCOL_VERSION,
        machine: state.machine.clone(),
        key: state.identity.public_key(),
        token: offer.token_base64(),
        lan: state.lan_addresses(),
        relay: state.relay.clone(),
        expires_ms: offer.expires_ms(),
    };
    offers.push(PeerOffer { by: peer.to_string(), offer });
    json!({"reply": "peer_offer", "offer": payload.encode(), "expires_ms": payload.expires_ms}).to_string().into_bytes()
}

/// The `peer_offer` rule: a trusted peer, none of its offers outstanding,
/// fewer than [`MAX_BROUGHT`] phones brought since it was paired.
fn may_ask_offer(store: &DeviceStore, peer: &str, offers: &[PeerOffer]) -> Result<(), String> {
    let Some(c) = store.computers.get(peer).filter(|c| c.is_active()) else {
        return Err("only a paired computer may ask for an offer".into());
    };
    if !c.trusted() {
        return Err(format!(
            "'{}' was introduced to this computer and is not allowed yet, so it may not bring a phone",
            c.name
        ));
    }
    if offers.iter().any(|o| o.by == peer) {
        return Err("an offer for this computer's phone is already waiting".into());
    }
    if store.introduced_by_count(peer, c.paired_ms) >= MAX_BROUGHT {
        return Err(format!("'{}' has brought {MAX_BROUGHT} phones here already", c.name));
    }
    Ok(())
}

/// A pairing on an offer a peer asked for, when one matches the token, or
/// [`PairingError::BadToken`] (the same answer as no offer at all).
pub fn complete(state: &State, request: &PairingRequest, now: u64) -> Result<Device, PairingError> {
    let mut offers = lock(&state.intro.offers);
    offers.retain(|o| o.offer.is_live(now));
    let token = rime_remote_core::b64_decode(&request.token).unwrap_or_default();
    for o in offers.iter_mut() {
        if o.offer.redeem(&token, now).is_err() {
            continue;
        }
        let by = o.by.clone();
        let mut store = state.devices().map_err(|e| PairingError::BadDevice(e.to_string()))?;
        let Some(name) = store.computers.get(&by).filter(|c| c.is_active() && c.trusted()).map(|c| c.name.clone()) else {
            return Err(PairingError::BadToken);
        };
        let introducer = Introducer { id: by, name: name.clone() };
        let device = store.pair_introduced(&request.key, &request.name, introducer, now, request.user_verification)?;
        state
            .save_devices(&store)
            .map_err(|e| PairingError::BadDevice(format!("the device was not written: {e}")))?;
        drop(store);
        offers.retain(|o| o.offer.is_live(now));
        notify_joined(&device, &name, false);
        return Ok(device);
    }
    Err(PairingError::BadToken)
}

/// A phone sent a report on a session of `scope`: when it says join on a
/// Full session, bring it to this computer's other computers.
pub fn reported(state: &State, device_id: &str, scope: Scope) {
    if scope == Scope::Full && state.phones().get(device_id).is_some_and(|r| r.can_join()) {
        state.intro.wake(device_id);
    }
}

/// Flow B for the life of the daemon: each phone woken, brought.
pub fn supervise(state: Arc<State>) {
    loop {
        for phone in state.intro.next() {
            bring(&state, &phone);
        }
    }
}

/// Whether phone `phone` may be brought now: a Full events channel, a record
/// paired here or allowed, and its last report says join.
fn may_bring(state: &State, phone: &str) -> bool {
    state.events.holder(phone).is_some_and(|(_, full)| full)
        && state.devices().is_ok_and(|s| s.find(phone).is_ok_and(|d| d.is_active() && d.trusted()))
        && state.phones().get(phone).is_some_and(|r| r.can_join())
}

/// The computers phone `phone` may be brought to: active and trusted here.
fn bring_targets(state: &State) -> Vec<Device> {
    state
        .devices()
        .map(|s| s.computers.values().filter(|c| c.is_active() && c.trusted()).cloned().collect())
        .unwrap_or_default()
}

/// Ask each trusted computer for an offer for `phone`, and hand each to it:
/// once an offer reached the phone, again after [`DELIVERED_RETRY_MS`]; after
/// a try that reached nobody, again after [`RETRY_MS`].
pub fn bring(state: &State, phone: &str) {
    if !may_bring(state, phone) {
        return;
    }
    for c in bring_targets(state) {
        if !state.intro.may_try(phone, &c.id, rime_remote_core::now_ms()) {
            continue;
        }
        match ask_offer(state, &c) {
            Ok(Some(offer)) => {
                let line = json!({"event": JOIN_EVENT, "offer": offer, "computer": c.name, "by": state.machine});
                if state.events.send(phone, &line) {
                    state.intro.delivered(phone, &c.id);
                } else {
                    eprintln!("rime-remoted: the phone left before it could be told about '{}'", c.name);
                }
            }
            Ok(None) => {}
            Err(e) => eprintln!("rime-remoted: could not ask '{}' to let the phone join: {e}", c.name),
        }
    }
}

/// Dial computer `c` and ask for an offer. `None` when it does not take
/// `peer_offer` (an older daemon) or refused.
fn ask_offer(state: &State, c: &Device) -> Result<Option<String>, String> {
    let mut link = crate::dialer::open(state, c, crate::dialer::Via::Any)?;
    let hello = link.call(br#"{"cmd":"remote_hello"}"#)?;
    let offers = hello["features"].as_array().is_some_and(|f| f.iter().any(|x| x.as_str() == Some(PEER_VERB)));
    if !offers {
        return Ok(None);
    }
    let reply = link.call(json!({"cmd": PEER_VERB}).to_string().as_bytes())?;
    if reply["reply"] != "peer_offer" {
        eprintln!(
            "rime-remoted: '{}' gave no offer for the phone: {}",
            c.name,
            crate::dialer::printable(reply["message"].as_str().unwrap_or_default())
        );
        return Ok(None);
    }
    let offer = reply["offer"].as_str().ok_or("an offer that is not text")?;
    offer_from(offer, c, state.relay.as_deref()).map(Some)
}

/// `offer`, when it is an offer for computer `c`, as the phone is handed it:
/// only an offer for the computer that was dialled, so a peer cannot steer
/// the phone to a key of its choosing; only its dialable addresses; and its
/// relay only when it is the relay recorded for `c` or this computer's own
/// (`own_relay`), so a peer cannot send the phone to a relay of its choosing.
fn offer_from(offer: &str, c: &Device, own_relay: Option<&str>) -> Result<String, String> {
    let mut parsed = PairingOffer::decode(offer).map_err(|e| e.to_string())?;
    if parsed.key != c.public_key {
        return Err("an offer for a different computer".into());
    }
    parsed.lan = crate::computers::dialable_only(parsed.lan);
    let relay_ok = parsed.relay.as_deref().is_some_and(|r| c.relay.as_deref() == Some(r) || own_relay == Some(r));
    if !relay_ok {
        parsed.relay = None;
    }
    Ok(parsed.encode())
}

/// Computer `key` (`name`) was removed here: tell every phone with a Full
/// events channel, so it forgets it too.
pub fn tell_removed(state: &State, key: &str, name: &str) {
    let line = json!({"event": REMOVED_EVENT, "key": key, "name": name});
    for phone in state.events.full_holders() {
        state.events.send(&phone, &line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::tests::a_state;

    fn report(state: &State, phone: &str, can: &[&str]) {
        let v = json!({"cmd": "phone_report", "can": can});
        let r = crate::phone::PhoneReport::parse(&v, 1).expect("report");
        state.phone_report(phone, r);
    }

    fn paired_phone(state: &State, seed: u8) -> String {
        let mut store = state.devices().expect("store");
        let d = store.pair(&rime_remote_core::b64_encode(&[seed; 32]), "pixel", 1, false).expect("pair");
        state.save_devices(&store).expect("save");
        d.id
    }

    fn ask(state: &State, phone: &str, scope: Scope, line: Value) -> Value {
        let r = control(state, phone, scope, line.to_string().as_bytes()).expect("answered");
        serde_json::from_slice(&r).expect("json")
    }

    fn other_card(seed: u8, name: &str) -> Value {
        json!({"key": rime_remote_core::b64_encode(&[seed; 32]), "name": name, "lan": ["192.168.1.50:7717"], "port": 7717})
    }

    #[test]
    fn only_a_full_session_whose_report_says_join_may_introduce() {
        let (state, _dir) = a_state("intro-gate");
        let phone = paired_phone(&state, 1);
        let card = json!({"cmd": "computer_card"});
        // No report yet, then a report without join: refused.
        assert_eq!(ask(&state, &phone, Scope::Full, card.clone())["kind"], REFUSAL);
        report(&state, &phone, &["ring"]);
        assert_eq!(ask(&state, &phone, Scope::Full, card.clone())["kind"], REFUSAL);
        report(&state, &phone, &["join"]);
        assert_eq!(ask(&state, &phone, Scope::Companion, card.clone())["kind"], REFUSAL, "a companion session introduced");
        let v = ask(&state, &phone, Scope::Full, card);
        assert_eq!(v["reply"], "computer_card", "{v}");
        assert_eq!(v["card"]["key"], state.identity.public_key());
        assert_eq!(v["card"]["name"], state.machine);
        // An introduced phone not allowed yet may not introduce either.
        let mut store = state.devices().expect("store");
        let p = store
            .pair_introduced(&rime_remote_core::b64_encode(&[2; 32]), "brought", Introducer { id: "c".into(), name: "c".into() }, 1, false)
            .expect("brought");
        drop(store);
        report(&state, &p.id, &["join"]);
        assert_eq!(ask(&state, &p.id, Scope::Full, json!({"cmd": "computer_card"}))["kind"], REFUSAL);
        // A revoked phone whose session has not been dropped yet: refused.
        let mut store = state.devices().expect("store");
        store.revoke(&phone, 2).expect("revoke");
        state.save_devices(&store).expect("save");
        drop(store);
        assert_eq!(ask(&state, &phone, Scope::Full, json!({"cmd": "computer_card"}))["kind"], REFUSAL, "a revoked phone got a card");
    }

    #[test]
    fn an_offer_is_not_redeemed_for_a_peer_that_is_no_longer_trusted() {
        let (state, _dir) = a_state("intro-offer-untrusted");
        let c = a_peer(&state, 7);
        let v = offer_reply(&state, &c.id);
        assert_eq!(v["reply"], "peer_offer", "{v}");
        {
            let mut store = state.devices().expect("store");
            store.computers.get_mut(&c.id).expect("c").introduced_by = Some(Introducer { id: "p".into(), name: "p".into() });
            state.save_devices(&store).expect("save");
        }
        assert!(matches!(redeem(&state, &v, 3), Err(PairingError::BadToken)));
        assert!(state.devices().expect("store").devices.is_empty(), "a phone was recorded");
    }

    #[test]
    fn an_introduced_computer_is_recorded_once_never_over_a_removal_and_its_card_is_checked() {
        let (state, _dir) = a_state("intro-add");
        let phone = paired_phone(&state, 1);
        report(&state, &phone, &["join"]);
        let v = ask(&state, &phone, Scope::Full, json!({"cmd": "computer_introduce", "card": other_card(7, "laptop")}));
        assert_eq!(v["outcome"], "added", "{v}");
        let id = v["id"].as_str().expect("id").to_string();
        {
            let store = state.devices().expect("store");
            let c = store.find_computer(&id).expect("recorded");
            assert_eq!(c.introduced_by.as_ref().map(|i| i.id.as_str()), Some(phone.as_str()));
            assert!(!c.trusted());
            assert_eq!(c.addresses, ["192.168.1.50:7717"]);
        }
        let again = ask(&state, &phone, Scope::Full, json!({"cmd": "computer_introduce", "card": other_card(7, "renamed")}));
        assert_eq!(again["outcome"], "already_paired");
        assert_eq!(state.devices().expect("store").find_computer(&id).expect("c").name, "laptop");
        // Removed here: an introduction does not bring it back.
        assert!(matches!(crate::computers::remove(&state, &id), crate::control::Reply::ComputerRemoved { .. }));
        let back = ask(&state, &phone, Scope::Full, json!({"cmd": "computer_introduce", "card": other_card(7, "laptop")}));
        assert_eq!(back["outcome"], "removed");
        assert!(state.devices().expect("store").authenticate(&rime_remote_core::b64_encode(&[7; 32])).is_none());
        let known = |state: &State| {
            ask(state, &phone, Scope::Full, json!({"cmd": "computer_card"}))["known"]
                .as_array()
                .expect("known")
                .iter()
                .any(|k| k == &json!(rime_remote_core::b64_encode(&[7; 32])))
        };
        assert!(known(&state), "a removal is known, so the phone does not offer it again");
        // Forgotten here: no longer known, and an introduction adds it again.
        {
            let mut store = state.devices().expect("store");
            store.forget_computer(&id, rime_remote_core::now_ms()).expect("forget");
            state.save_devices(&store).expect("save");
        }
        assert!(!known(&state), "a forgotten removal is still in the card's known keys");
        let forgotten = ask(&state, &phone, Scope::Full, json!({"cmd": "computer_introduce", "card": other_card(7, "laptop")}));
        assert_eq!(forgotten["outcome"], "added", "{forgotten}");
        assert!(known(&state));
        // A loopback address, or this computer's own key: refused.
        let mut bad = other_card(8, "x");
        bad["lan"] = json!(["127.0.0.1:7717"]);
        assert_eq!(ask(&state, &phone, Scope::Full, json!({"cmd": "computer_introduce", "card": bad}))["kind"], "bad_request");
        let own = json!({"key": state.identity.public_key(), "name": "me", "lan": []});
        assert_eq!(ask(&state, &phone, Scope::Full, json!({"cmd": "computer_introduce", "card": own}))["kind"], "bad_request");
    }

    fn a_peer(state: &State, seed: u8) -> Device {
        let mut store = state.devices().expect("store");
        let c = store
            .pair_computer(&[seed; 32], &state.identity.public_bytes(), "desk", vec![], None, 1)
            .expect("pair");
        state.save_devices(&store).expect("save");
        c
    }

    fn offer_reply(state: &State, peer: &str) -> Value {
        serde_json::from_slice(&answer_peer_offer(state, peer)).expect("json")
    }

    fn redeem(state: &State, v: &Value, phone_seed: u8) -> Result<Device, PairingError> {
        let offer = PairingOffer::decode(v["offer"].as_str().expect("offer")).expect("decodes");
        let request = PairingRequest {
            key: rime_remote_core::b64_encode(&[phone_seed; 32]),
            name: format!("phone {phone_seed}"),
            token: offer.token,
            user_verification: false,
        };
        complete(state, &request, rime_remote_core::now_ms())
    }

    #[test]
    fn a_peer_offer_is_single_use_one_at_a_time_and_makes_a_companion_phone() {
        let (state, _dir) = a_state("intro-offer");
        let c = a_peer(&state, 7);
        let v = offer_reply(&state, &c.id);
        assert_eq!(v["reply"], "peer_offer", "{v}");
        assert_eq!(PairingOffer::decode(v["offer"].as_str().expect("offer")).expect("decodes").key, state.identity.public_key());
        // One outstanding.
        assert_eq!(offer_reply(&state, &c.id)["kind"], REFUSAL);
        let d = redeem(&state, &v, 1).expect("paired");
        assert_eq!(d.introduced_by.as_ref().map(|i| i.id.as_str()), Some(c.id.as_str()));
        assert_eq!(
            state.devices().expect("store").authenticate(&rime_remote_core::b64_encode(&[1; 32])).map(|(_, s)| s),
            Some(Scope::Companion)
        );
        // Single use.
        assert!(matches!(redeem(&state, &v, 2), Err(PairingError::BadToken)));
        // The human offer slot is untouched and empty.
        assert!(state.offer.lock().expect("offer").is_none());
    }

    #[test]
    fn a_peer_offer_lasts_two_minutes_from_when_it_was_asked_for_and_not_a_moment_more() {
        let (state, _dir) = a_state("intro-offer-ttl");
        let c = a_peer(&state, 7);
        let ask_at = |now: u64| -> Value { serde_json::from_slice(&answer_peer_offer_at(&state, &c.id, now)).expect("json") };
        let request = |v: &Value, seed: u8| PairingRequest {
            key: rime_remote_core::b64_encode(&[seed; 32]),
            name: format!("phone {seed}"),
            token: PairingOffer::decode(v["offer"].as_str().expect("offer")).expect("decodes").token,
            user_verification: true,
        };
        let t0 = 1_800_000_000_000;
        // Asked for at t0: good until t0 + two minutes, by the reply and by
        // the payload the phone gets. (Literal, not the constant: a change
        // to the constant has to change this test too.)
        let v = ask_at(t0);
        assert_eq!(v["reply"], "peer_offer", "{v}");
        assert_eq!(v["expires_ms"].as_u64(), Some(t0 + 120_000), "{v}");
        let payload = PairingOffer::decode(v["offer"].as_str().expect("offer")).expect("decodes");
        assert_eq!(payload.expires_ms, t0 + 120_000);
        // A wrong token is refused and does not spend it.
        let mut wrong = request(&v, 1);
        wrong.token = rime_remote_core::b64_encode(&[0; 16]);
        assert!(matches!(complete(&state, &wrong, t0 + 1), Err(PairingError::BadToken)));
        // A moment before it runs out it pairs, introduced, keeping what the
        // phone said about its key.
        let d = complete(&state, &request(&v, 1), t0 + 119_999).expect("paired just in time");
        assert_eq!(d.introduced_by.as_ref().map(|i| i.id.as_str()), Some(c.id.as_str()));
        assert!(d.requires_user_verification);
        // The next one, asked for later, is refused at its two minutes, the
        // phone is not recorded, and the peer may ask again at that instant.
        let t1 = t0 + 600_000;
        let v = ask_at(t1);
        assert_eq!(v["expires_ms"].as_u64(), Some(t1 + 120_000), "{v}");
        assert!(matches!(complete(&state, &request(&v, 2), t1 + 120_000), Err(PairingError::BadToken)));
        assert!(state.devices().expect("store").authenticate(&rime_remote_core::b64_encode(&[2; 32])).is_none());
        assert_eq!(ask_at(t1 + 120_000)["reply"], "peer_offer", "an expired offer still counts as outstanding");
    }

    #[test]
    fn a_peer_brings_at_most_four_phones_for_the_life_of_its_pairing_revoked_ones_counted() {
        let (state, _dir) = a_state("intro-cap");
        let c = a_peer(&state, 7);
        for seed in 1..=MAX_BROUGHT as u8 {
            let v = offer_reply(&state, &c.id);
            let d = redeem(&state, &v, seed).expect("paired");
            if seed == 1 {
                let _ = crate::control::revoke(&state, &d.id);
            }
        }
        let v = offer_reply(&state, &c.id);
        assert_eq!(v["kind"], REFUSAL, "{v}");
        assert!(v["message"].as_str().expect("message").contains("4 phones"), "{v}");
    }

    #[test]
    fn an_introduced_computer_not_allowed_may_not_ask_for_an_offer() {
        let (state, _dir) = a_state("intro-unallowed");
        let mut store = state.devices().expect("store");
        let Introduced::Added(c) = store
            .introduce_computer(
                &[7; 32],
                &state.identity.public_bytes(),
                "laptop",
                vec![],
                None,
                Introducer { id: "p".into(), name: "pixel".into() },
                1,
            )
            .expect("introduce")
        else {
            panic!("not added")
        };
        drop(store);
        assert_eq!(offer_reply(&state, &c.id)["kind"], REFUSAL);
        state.devices().expect("store").allow_computer(&c.id, 2).expect("allow");
        assert_eq!(offer_reply(&state, &c.id)["reply"], "peer_offer");
    }

    #[test]
    fn a_report_saying_join_on_a_companion_session_wakes_nothing() {
        let (state, _dir) = a_state("intro-wake");
        let phone = paired_phone(&state, 1);
        report(&state, &phone, &["join"]);
        reported(&state, &phone, Scope::Companion);
        assert!(lock(&state.intro.queue).is_empty(), "a companion report woke the phone");
        reported(&state, &phone, Scope::Full);
        assert!(lock(&state.intro.queue).contains(&phone));
    }

    #[test]
    fn a_join_sent_on_a_companion_session_is_not_kept_for_a_later_full_one() {
        // A companion key is not behind the lock screen. Whatever holds it
        // must not be able to switch "Join my other computers" on for the
        // phone's next full session.
        let (state, _dir) = a_state("intro-companion-join");
        let phone = paired_phone(&state, 1);
        let line = json!({"cmd": "phone_report", "can": ["ring", "join"]}).to_string();
        crate::phone::control(&state, &phone, Scope::Companion, line.as_bytes());
        let kept = state.phones().get(&phone).cloned().expect("kept");
        assert!(!kept.can_join(), "{kept:?}");
        assert!(may_introduce(&state, &phone, Scope::Full).is_err());
        crate::phone::control(&state, &phone, Scope::Full, line.as_bytes());
        assert!(may_introduce(&state, &phone, Scope::Full).is_ok());
    }

    #[test]
    fn a_removed_peer_gets_no_offer() {
        let (state, _dir) = a_state("intro-removed-peer");
        let c = a_peer(&state, 7);
        assert!(matches!(crate::computers::remove(&state, &c.id), crate::control::Reply::ComputerRemoved { .. }));
        assert_eq!(offer_reply(&state, &c.id)["kind"], REFUSAL);
    }

    #[test]
    fn only_trusted_phones_are_brought_and_only_to_trusted_computers() {
        let (state, _dir) = a_state("intro-bring");
        let trusted = a_peer(&state, 7);
        let mut store = state.devices().expect("store");
        let by = Introducer { id: "p".into(), name: "pixel".into() };
        let Introduced::Added(unallowed) = store
            .introduce_computer(&[8; 32], &state.identity.public_bytes(), "lab", vec![], None, by.clone(), 1)
            .expect("introduce")
        else {
            panic!("not added")
        };
        let brought = store.pair_introduced(&rime_remote_core::b64_encode(&[9; 32]), "other", by, 1, false).expect("pair");
        state.save_devices(&store).expect("save");
        drop(store);
        let targets: Vec<String> = bring_targets(&state).into_iter().map(|c| c.id).collect();
        assert_eq!(targets, vec![trusted.id.clone()], "an introduced computer not allowed is asked for nothing");
        assert!(!targets.contains(&unallowed.id));

        // A phone brought here, even holding a full channel, is not brought on.
        report(&state, &brought.id, &["join"]);
        let _held = state.events.open_for(&brought.id, 1, true).expect("open");
        assert!(!may_bring(&state, &brought.id));
        state.devices().expect("store").allow_phone(&brought.id, 2).expect("allow");
        assert!(may_bring(&state, &brought.id));
        // A companion channel is not enough.
        let phone = paired_phone(&state, 1);
        report(&state, &phone, &["join"]);
        let _companion = state.events.open_for(&phone, 2, false).expect("open");
        assert!(!may_bring(&state, &phone));
    }

    #[test]
    fn a_try_that_reached_nobody_is_tried_again_soon_and_a_delivered_one_after_an_hour() {
        let intro = Intro::default();
        assert!(intro.may_try("p", "c", 1_000), "the first try");
        assert!(!intro.may_try("p", "c", 1_000 + RETRY_MS - 1), "not again at once");
        assert!(intro.may_try("p", "d", 1_000), "another computer is its own pair");
        assert!(intro.may_try("p", "c", 1_000 + RETRY_MS), "a try that reached nobody is tried again");
        intro.delivered("p", "c");
        let sent = 1_000 + RETRY_MS;
        assert!(!intro.may_try("p", "c", sent + DELIVERED_RETRY_MS - 1), "an offer that reached the phone is not sent again soon");
        assert!(intro.may_try("p", "c", sent + DELIVERED_RETRY_MS), "nor never: the first may have run out unused");
    }

    #[test]
    fn device_removed_goes_to_full_channels_only() {
        let (state, _dir) = a_state("intro-removed-event");
        let full = paired_phone(&state, 1);
        let companion = paired_phone(&state, 2);
        let (full_end, _) = state.events.open_for(&full, 1, true).expect("open");
        let (companion_end, _) = state.events.open_for(&companion, 2, false).expect("open");
        for end in [&full_end, &companion_end] {
            end.set_read_timeout(Some(std::time::Duration::from_millis(200))).expect("timeout");
        }
        tell_removed(&state, &rime_remote_core::b64_encode(&[7; 32]), "laptop");
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(&full_end), &mut line).expect("full phone told");
        let v: Value = serde_json::from_str(&line).expect("json");
        assert_eq!(v["event"], REMOVED_EVENT, "{v}");
        assert_eq!(v["name"], "laptop", "{v}");
        let mut other = String::new();
        let got = std::io::BufRead::read_line(&mut std::io::BufReader::new(&companion_end), &mut other);
        assert!(got.is_err() || other.is_empty(), "a companion channel was told: {other}");
    }

    #[test]
    fn only_an_offer_for_the_computer_that_was_dialled_is_handed_on() {
        let (state, _dir) = a_state("intro-offer-key");
        let c = a_peer(&state, 7);
        let mut other = PairingOffer::decode(offer_reply(&state, &c.id)["offer"].as_str().expect("offer")).expect("decodes");
        // An offer this daemon minted carries its own key, not the peer's.
        assert!(offer_from(&other.encode(), &c, None).is_err());
        other.key = c.public_key.clone();
        other.lan = vec!["192.168.1.50:7717".into()];
        other.relay = None;
        assert_eq!(offer_from(&other.encode(), &c, None).as_deref(), Ok(other.encode().as_str()));
        assert!(offer_from("not an offer", &c, None).is_err());
        // Addresses nobody should dial are dropped, and a relay is kept only
        // when it is this computer's own or the one recorded for that computer.
        let mut steered = other.clone();
        steered.lan = vec!["127.0.0.1:22".into(), "192.168.1.50:7717".into()];
        steered.relay = Some("wss://relay.elsewhere.example".into());
        let handed = |own: Option<&str>, c: &Device| PairingOffer::decode(&offer_from(&steered.encode(), c, own).expect("ok")).expect("decodes");
        let got = handed(Some("wss://own.example"), &c);
        assert_eq!(got.lan, ["192.168.1.50:7717"]);
        assert_eq!(got.relay, None, "a relay the peer chose reached the phone");
        assert_eq!(got.token, steered.token);
        assert_eq!(handed(Some("wss://relay.elsewhere.example"), &c).relay.as_deref(), Some("wss://relay.elsewhere.example"));
        let mut recorded = c.clone();
        recorded.relay = Some("wss://relay.elsewhere.example".into());
        assert_eq!(handed(None, &recorded).relay.as_deref(), Some("wss://relay.elsewhere.example"));
    }

    #[test]
    fn a_joined_notification_shows_no_markup_from_a_name() {
        let s = joined_shown("<b>x</b>", "<a href='u'>desk</a> & co", false);
        assert!(!s.summary.contains('<') && !s.body.contains('<'), "{s:?}");
        assert!(!s.body.contains("& co"), "{s:?}");
        assert!(s.body.contains("rime remote devices allow"));
        assert_eq!(s.actions, [(REMOVE_ACTION.to_string(), "Remove".to_string())]);
    }

    #[test]
    fn remove_on_a_joined_notification_takes_away_and_nothing_else_does() {
        let (state, _dir) = a_state("intro-action");
        let by = paired_phone(&state, 1);
        report(&state, &by, &["join"]);
        let v = ask(&state, &by, Scope::Full, json!({"cmd": "computer_introduce", "card": other_card(7, "laptop")}));
        let c = v["id"].as_str().expect("id").to_string();
        // Another notification's action, or another action: not ours.
        assert!(!on_action(&state, &c, "ring", REMOVE_ACTION));
        assert!(!on_action(&state, &c, JOINED_KEY, "allow"));
        assert!(state.devices().expect("store").find_computer(&c).expect("c").is_active());
        assert!(on_action(&state, &c, JOINED_KEY, REMOVE_ACTION));
        assert!(!state.devices().expect("store").find_computer(&c).expect("c").is_active());
        let brought = {
            let mut store = state.devices().expect("store");
            let p = store
                .pair_introduced(&rime_remote_core::b64_encode(&[2; 32]), "brought", Introducer { id: "c".into(), name: "c".into() }, 1, false)
                .expect("brought");
            state.save_devices(&store).expect("save");
            p.id
        };
        assert!(on_action(&state, &brought, JOINED_KEY, REMOVE_ACTION));
        assert!(!state.devices().expect("store").find(&brought).expect("p").is_active());
        // A record a person paired here is not one that joined: a stale
        // notification's Remove leaves it.
        let peer = a_peer(&state, 9);
        assert!(on_action(&state, &peer.id, JOINED_KEY, REMOVE_ACTION));
        assert!(on_action(&state, &by, JOINED_KEY, REMOVE_ACTION));
        assert!(state.devices().expect("store").find_computer(&peer.id).expect("c").is_active(), "a code-paired computer was removed");
        assert!(state.devices().expect("store").find(&by).expect("p").is_active(), "a phone paired here was revoked");
    }
}
