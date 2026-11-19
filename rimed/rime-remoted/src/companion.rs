//! The companion link: a second key a phone keeps outside its lock screen, and
//! the one list of what a session on that key may do.
//!
//! ## Why there is a second key at all
//!
//! A phone holds its device key behind its lock screen, so after the phone
//! restarts nothing can open a session until somebody unlocks it, and its
//! battery report, notifications and ring stop until then. The companion key
//! is a second X25519 key the phone keeps without that gate. A session on the
//! full key mints it (`companion_register`); the device store keeps it on the
//! same record; a handshake that proves it yields [`Scope::Companion`].
//!
//! ## Default deny, in one place
//!
//! A companion session may use [`ALLOWED_VERBS`] on the control channel and
//! open [`ALLOWED_CHANNELS`], and nothing else: not the computer's own verbs
//! (lock, media, volume), not nex, not this module's own verbs, and never
//! anything that would reach `rime-agentd`. A line or channel outside the
//! lists is answered with [`REFUSAL`] in the worker, before any routing, so a
//! verb added to some module later is refused to a companion session until
//! somebody adds it here on purpose. `serve.rs` adds a second lock: a
//! companion line that nothing in this process answers is refused rather than
//! forwarded, and a companion `Open` that is not one of the phone channels is
//! refused rather than handed to nex or `rime-agentd`.
//!
//! The scope is decided by WHICH key completed the handshake
//! ([`rime_remote_core::device::DeviceStore::authenticate`]). Nothing the
//! phone sends can change it.

use rime_remote_core::device::{Scope, StoreError};
use serde_json::{json, Value};

use crate::state::State;

/// The verbs this module answers. Full scope only.
pub const VERBS: [&str; 2] = ["companion_register", "companion_forget"];

/// What `remote_hello` adds: this build takes `companion_register`, and the
/// hello says the session's scope.
pub const FEATURES: [&str; 1] = ["companion_link"];

/// The `kind` of the error a companion session gets for anything outside the
/// lists: `{"reply":"error","kind":"not_allowed","message":…}`.
pub const REFUSAL: &str = "not_allowed";

/// The phone-to-computer clipboard verb a companion session may use.
///
/// Empty on purpose. The only such verb is `clipboard_write`, and it is
/// `rime-agentd`'s (contract §1.8): allowing it would mean forwarding a
/// companion session's line to `rime-agentd`, which this link never does. If
/// `rime-remoted` ever answers a clipboard verb itself, it goes here.
pub const CLIPBOARD_VERBS: [&str; 0] = [];

/// Every control verb a companion session may send. The device features and
/// nothing else: its hello, its report and acks, its notifications, and what
/// it is playing.
///
/// Not `push_register` / `push_unregister`: a push carries what
/// `rime-agentd` says about agent sessions (their kind, id and start time),
/// so a lifted companion key that could register an endpoint could read
/// those and silence the owner's phone. Registration stays with the full key.
///
/// `messages_reply` and `messages_changed` are here: reading the phone's SMS
/// threads works while the app is locked, because the person asking is at
/// this computer and the phone's own switch decides (contract §2.11). Not
/// `messages_sent`: sending needs the app unlocked, and the computer never
/// writes `messages_send` to a companion session's channel.
///
/// `call_state`, `call_done` and `call_history_reply` are here (contract
/// §2.12): a call rings on a locked phone, and its lock screen lets anyone
/// holding it answer or decline, so the person at this computer may too; the
/// history read is gated here like the SMS read. A dial is the exception: the
/// computer never writes a `dial` action to a companion session's channel,
/// and the phone refuses one on that lane, so `call_done` for a dial never
/// comes from a companion session.
///
/// Not `photos_reply` / `photos_fetch_reply`, nor the `photos_upload`
/// channel (contract §2.13): a photo is the person's own picture, so photos
/// are listed and saved from an unlocked phone only. The computer writes no
/// photos request to a companion session's channel and takes no answer or
/// upload from one.
pub const ALLOWED_VERBS: [&str; 13] = [
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
    "call_state",
    "call_done",
    "call_history_reply",
];

/// Every channel a companion session may open: the events channel the
/// computer writes rings and offers to, and the channel an offer is taken on.
pub const ALLOWED_CHANNELS: [&str; 2] = [crate::phone::CHANNEL, crate::phone::TAKE];

/// The `cmd` of a line, when it is a JSON object with a string `cmd`.
///
/// The same reading every module's own dispatch uses, so the verb checked
/// here is the verb that would be routed.
pub fn verb_of(line: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(line)
        .ok()
        .and_then(|v| v.get("cmd").and_then(|c| c.as_str()).map(str::to_string))
}

/// Whether a session of this scope may send this control verb.
///
/// `None` is a line with no readable verb: a full session forwards it to
/// `rime-agentd`, which answers `bad_request`; a companion session is refused.
pub fn allowed(scope: Scope, cmd: Option<&str>) -> bool {
    match scope {
        Scope::Full => true,
        Scope::Companion => cmd.is_some_and(|c| ALLOWED_VERBS.contains(&c) || CLIPBOARD_VERBS.contains(&c)),
        // Another computer never reaches this list: `serve.rs` answers it from
        // `computers::PEER_VERBS` before anything else. False here is the
        // second lock, so a path that forgot that branch still refuses.
        Scope::Peer => false,
    }
}

/// Whether a session of this scope may open a channel with this request.
pub fn channel_allowed(scope: Scope, request: &[u8]) -> bool {
    match scope {
        Scope::Full => true,
        Scope::Companion => verb_of(request).is_some_and(|c| ALLOWED_CHANNELS.contains(&c.as_str())),
        Scope::Peer => false,
    }
}

/// The refusal, one line.
pub fn refusal(what: &str) -> Vec<u8> {
    json!({
        "reply": "error",
        "kind": REFUSAL,
        "message": format!(
            "{what} is not allowed on a companion link; unlock the phone to use it"
        ),
    })
    .to_string()
    .into_bytes()
}

/// Answer `companion_register` / `companion_forget`, or `None` for any other
/// line.
///
/// The device is the handshake's, never one named in the body. A companion
/// session is refused here as well as by [`allowed`]: a key may only be minted
/// by the key the phone keeps behind its lock.
pub fn control(state: &State, device_id: &str, scope: Scope, line: &[u8]) -> Option<Vec<u8>> {
    let v: Value = serde_json::from_slice(line).ok()?;
    let cmd = v.get("cmd")?.as_str()?;
    if !VERBS.contains(&cmd) {
        return None;
    }
    if scope != Scope::Full {
        return Some(refusal(cmd));
    }
    Some(match cmd {
        "companion_register" => {
            let Some(key) = v.get("public_key").and_then(|k| k.as_str()) else {
                return Some(error("bad_request", "public_key must be a string"));
            };
            register(state, device_id, key)
        }
        _ => forget(state, device_id),
    })
}

fn register(state: &State, device_id: &str, key: &str) -> Vec<u8> {
    let replaced = {
        let mut store = match state.devices() {
            Ok(s) => s,
            Err(e) => return error("internal", &e.to_string()),
        };
        // The whole record, so a failed write also takes back the key it
        // retired.
        let before = store.devices.get(device_id).cloned();
        let after = match store.set_companion(device_id, key) {
            Ok(d) => d.companion_key,
            Err(StoreError::BadKey(why)) => return error("bad_request", &why),
            Err(e @ StoreError::KeyInUse) => return error("key_in_use", &e.to_string()),
            Err(e) => return error("internal", &e.to_string()),
        };
        if let Err(e) = state.save_devices(&store) {
            // Not kept in memory either: a key that works until the next
            // restart and then silently stops is worse than a refusal now.
            if let Some(d) = before.clone() {
                store.devices.insert(device_id.to_string(), d);
            }
            return error("internal", &format!("the companion key was not written: {e}"));
        }
        let before = before.and_then(|d| d.companion_key);
        before.is_some() && before != after
    };
    // A replaced key's sessions end now, not at their next reconnect.
    if replaced {
        state.drop_companion_connections_for(device_id);
    }
    ok()
}

fn forget(state: &State, device_id: &str) -> Vec<u8> {
    {
        let mut store = match state.devices() {
            Ok(s) => s,
            Err(e) => return error("internal", &e.to_string()),
        };
        let before = store.devices.get(device_id).cloned();
        if store.forget_companion(device_id) {
            if let Err(e) = state.save_devices(&store) {
                if let Some(d) = before {
                    store.devices.insert(device_id.to_string(), d);
                }
                return error("internal", &format!("the companion key was not removed: {e}"));
            }
        }
    }
    state.drop_companion_connections_for(device_id);
    ok()
}

fn ok() -> Vec<u8> {
    json!({"reply": "ok"}).to_string().into_bytes()
}

fn error(kind: &str, message: &str) -> Vec<u8> {
    json!({"reply": "error", "kind": kind, "message": message})
        .to_string()
        .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn a_state(tag: &str) -> (Arc<State>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "rime-companion-{}-{tag}-{}",
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
            crate::serve::PING_INTERVAL,
        )
        .expect("state");
        state.set_nexd(dir.join("no-nexd.sock"));
        (state, dir)
    }

    fn key(seed: u8) -> String {
        rime_remote_core::b64_encode(&[seed; 32])
    }

    fn reply(r: &[u8]) -> Value {
        serde_json::from_slice(r).expect("json")
    }

    fn pair(state: &State, seed: u8, name: &str) -> String {
        let mut s = state.devices().expect("store");
        let id = s.pair(&key(seed), name, 1, false).expect("pair").id;
        state.save_devices(&s).expect("save");
        id
    }

    #[test]
    fn a_full_session_registers_and_forgets_and_the_store_on_disk_follows() {
        let (state, dir) = a_state("register");
        let id = pair(&state, 1, "phone");
        let line = format!(r#"{{"cmd":"companion_register","public_key":"{}"}}"#, key(2));
        let r = reply(&control(&state, &id, Scope::Full, line.as_bytes()).expect("answered"));
        assert_eq!(r["reply"], "ok", "{r}");
        let on_disk = rime_remote_core::device::DeviceStore::load(&dir.join("devices.json")).expect("load");
        assert_eq!(on_disk.authenticate(&key(2)).map(|(d, s)| (d.id.clone(), s)), Some((id.clone(), Scope::Companion)));

        let r = reply(&control(&state, &id, Scope::Full, br#"{"cmd":"companion_forget"}"#).expect("answered"));
        assert_eq!(r["reply"], "ok", "{r}");
        let on_disk = rime_remote_core::device::DeviceStore::load(&dir.join("devices.json")).expect("load");
        assert!(on_disk.authenticate(&key(2)).is_none());
        // Forgetting nothing is still ok.
        let r = reply(&control(&state, &id, Scope::Full, br#"{"cmd":"companion_forget"}"#).expect("answered"));
        assert_eq!(r["reply"], "ok", "{r}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_companion_session_can_neither_register_nor_forget() {
        let (state, dir) = a_state("from-companion");
        let id = pair(&state, 3, "phone");
        state.devices().expect("store").set_companion(&id, &key(4)).expect("set");
        for line in [
            format!(r#"{{"cmd":"companion_register","public_key":"{}"}}"#, key(5)),
            r#"{"cmd":"companion_forget"}"#.to_string(),
        ] {
            let r = reply(&control(&state, &id, Scope::Companion, line.as_bytes()).expect("answered"));
            assert_eq!(r["kind"], REFUSAL, "{line}: {r}");
        }
        let s = state.devices().expect("store");
        assert_eq!(s.devices[&id].companion_key, Some(key(4)), "a companion session changed the key");
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_bad_or_known_key_is_refused_and_nothing_is_stored() {
        let (state, dir) = a_state("refusals");
        let id = pair(&state, 6, "phone");
        let other = pair(&state, 7, "other");
        state.devices().expect("store").set_companion(&other, &key(8)).expect("set");
        let ask = |body: &str| reply(&control(&state, &id, Scope::Full, body.as_bytes()).expect("answered"));
        let r = ask(r#"{"cmd":"companion_register"}"#);
        assert_eq!(r["kind"], "bad_request", "{r}");
        let r = ask(r#"{"cmd":"companion_register","public_key":"short"}"#);
        assert_eq!(r["kind"], "bad_request", "{r}");
        for (k, why) in [(key(6), "its own full key"), (key(7), "another device's key"), (key(8), "another device's companion")] {
            let r = ask(&format!(r#"{{"cmd":"companion_register","public_key":"{k}"}}"#));
            assert_eq!(r["kind"], "key_in_use", "{why}: {r}");
            assert!(!r["message"].as_str().unwrap_or_default().contains("other"), "{why}: the refusal names a device: {r}");
        }
        assert_eq!(state.devices().expect("store").devices[&id].companion_key, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_this_modules_verbs_are_answered_here() {
        let (state, dir) = a_state("verbs");
        for line in [&br#"{"cmd":"companion"}"#[..], br#"{"cmd":"list"}"#, b"companion_register", br#"{"cmd":1}"#] {
            assert_eq!(control(&state, "x", Scope::Full, line), None, "{}", String::from_utf8_lossy(line));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_companion_session_may_open_the_phone_channels_and_nothing_else() {
        for ok in [&br#"{"cmd":"device_events"}"#[..], br#"{"cmd":"take_offer","id":3}"#] {
            assert!(channel_allowed(Scope::Companion, ok), "{}", String::from_utf8_lossy(ok));
            assert!(crate::phone::is_channel(ok) || crate::phone::is_take(ok), "the list and the router disagree");
        }
        for no in [
            &br#"{"cmd":"attach","id":1,"cols":80,"rows":24}"#[..],
            br#"{"cmd":"receive","id":1,"name":"a","len":1}"#,
            br#"{"cmd":"save_file","name":"a","len":1}"#,
            br#"{"cmd":"nex_events"}"#,
            br#"{"cmd":"nex"}"#,
            br#"{"cmd":"device_events_and_more"}"#,
            br#"{"cmd":["device_events"]}"#,
            br#"{"device_events":true}"#,
            b"device_events",
            b"",
        ] {
            assert!(!channel_allowed(Scope::Companion, no), "{}", String::from_utf8_lossy(no));
            assert!(channel_allowed(Scope::Full, no), "a full session lost a channel");
        }
        assert_eq!(ALLOWED_CHANNELS, ["device_events", "take_offer"]);
    }

    #[test]
    fn the_refusal_is_one_line_with_the_documented_kind() {
        let r = refusal("machine_lock");
        assert!(!r.contains(&b'\n'));
        let v = reply(&r);
        assert_eq!(v["reply"], "error");
        assert_eq!(v["kind"], "not_allowed");
        assert!(v["message"].as_str().unwrap_or_default().contains("machine_lock"), "{v}");
    }
}
