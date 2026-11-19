//! The clipboard shared between the owner's paired computers, text only
//! (contract §2.15). Files are files.rs.
//!
//! ## The clipboard
//!
//! This process never reads the clipboard. Rime Shell watches it and hands
//! each text copy to `rime remote clipboard` (§2.14); [`local_copy`] decides
//! whether that copy also goes to other computers. It goes to each computer
//! whose send switch is on here ([`Device::sends_clipboard`]), and is put on
//! that computer's clipboard only when its receive switch for this one is on
//! there ([`Device::takes_clipboard`]). Both switches are off until a person
//! turns them on, and neither counts for a computer that is removed, or
//! introduced by a phone and not allowed yet: a clipboard is not low-risk.
//!
//! Never shared: a copy the Shell marked sensitive (a password manager's),
//! anything over [`MAX_CLIP_BYTES`], anything while this computer's screen is
//! locked or its lock state cannot be read, and a copy that came from a phone
//! or another computer.
//!
//! ### Echo
//!
//! Only a copy made at this computer travels, and only directly to the
//! computers it is shared with: a copy that arrived from another computer is
//! put on the clipboard here and goes nowhere else, neither to other
//! computers nor to this computer's phones; a copy a phone wrote here goes to
//! no other computer. Both are recognised when the Shell hands them back (it
//! sees every clipboard change, including the ones made here for a phone or a
//! computer):
//!
//! * a copy from another computer by its hash, remembered as applied BEFORE
//!   the clipboard is written, for [`ECHO_FOR`];
//! * a copy from a phone by its text, from `state.clip_echo`.
//!
//! And on the receiving side, a copy whose hash this computer itself sent in
//! the last [`ECHO_FOR`] is dropped, so A→B→A ends at A even if B were an
//! older or misbehaving build. A copy carries the sending computer's key as
//! `origin`, which must be the key that opened the session (a copy is never
//! relayed), and a `seq` the receiver has not seen from that computer, seeded
//! from the clock so a restarted sender does not repeat an earlier one.
//!
//! ### Delivery and power
//!
//! Each copy is pushed when it is made, over the dialer (the LAN, then the
//! relay), on a thread per computer; nothing runs between copies and nothing
//! is queued for a computer that cannot be reached. While a push to a
//! computer is under way the next copy waits behind it and a newer one
//! replaces it: the latest copy is the one that matters. Power saver changes
//! none of this: a push only ever follows a person's copy.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rime_remote_core::device::Device;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::state::State;

/// The peer verb that carries one clipboard copy.
pub const CLIP_VERB: &str = "peer_clipboard";

/// The longest copy shared, in bytes: what `rime-agentd` puts on a clipboard
/// at most (`clipboard_write`), the same cap a phone's copy has here.
pub const MAX_CLIP_BYTES: usize = 8192;

/// How long a copy sent or applied here is remembered for the echo rule.
pub const ECHO_FOR: Duration = crate::clipboard::ECHO_FOR;

/// At most this many copies a [`RATE_WINDOW`], sent from here in all, and
/// taken from any one computer. The Shell hands over at most two a second.
pub const CLIP_RATE: usize = 30;
pub const RATE_WINDOW: Duration = Duration::from_secs(60);

/// How many recent `seq`s are kept per computer to refuse a repeat.
const SEEN_SEQS: usize = 128;

/// How many recent hashes are kept for the echo rule, whatever their age.
const RECENT: usize = 64;

/// Sharing's memory: process lifetime only, nothing written.
#[derive(Default)]
pub struct Sharing {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// The last `seq` given out; 0 until the first copy.
    seq: u64,
    /// Hashes sent or applied here, oldest first.
    recent: VecDeque<Recent>,
    /// Per sending computer: the `seq`s taken from it, oldest first.
    seen: HashMap<String, VecDeque<u64>>,
    /// Per sending computer: when its copies arrived, oldest first.
    arrivals: HashMap<String, VecDeque<Instant>>,
    /// When this computer sent its copies, oldest first.
    sent: VecDeque<Instant>,
    /// A computer with a push under way, and the copy waiting behind it.
    outgoing: HashMap<String, Option<Clip>>,
}

/// One copy on its way out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clip {
    pub text: String,
    pub hash: String,
    pub seq: u64,
}

struct Recent {
    hash: String,
    at: Instant,
    how: How,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum How {
    Sent,
    Applied,
}

impl Sharing {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A removed computer: what was kept about it goes.
    pub fn forget(&self, id: &str) {
        let mut g = self.lock();
        g.seen.remove(id);
        g.arrivals.remove(id);
    }
}

impl Inner {
    fn was(&mut self, hash: &str, how: How, now: Instant) -> bool {
        self.recent.retain(|r| now.saturating_duration_since(r.at) < ECHO_FOR);
        self.recent.iter().any(|r| r.how == how && r.hash == hash)
    }

    fn remember(&mut self, hash: &str, how: How, now: Instant) {
        self.recent.push_back(Recent { hash: hash.to_string(), at: now, how });
        while self.recent.len() > RECENT {
            self.recent.pop_front();
        }
    }

    /// The next `seq`: above every earlier one, and above the clock in
    /// milliseconds, so a restarted process starts past what it sent before.
    fn next_seq(&mut self, now_ms: u64) -> u64 {
        self.seq = self.seq.max(now_ms) + 1;
        self.seq
    }

    /// Whether `seq` is new from `sender`; it is recorded either way.
    fn fresh(&mut self, sender: &str, seq: u64) -> bool {
        let seen = self.seen.entry(sender.to_string()).or_default();
        if seen.contains(&seq) {
            return false;
        }
        seen.push_back(seq);
        while seen.len() > SEEN_SEQS {
            seen.pop_front();
        }
        true
    }

    fn under_rate(times: &mut VecDeque<Instant>, now: Instant) -> bool {
        while times.front().is_some_and(|t| now.saturating_duration_since(*t) >= RATE_WINDOW) {
            times.pop_front();
        }
        if times.len() >= CLIP_RATE {
            return false;
        }
        times.push_back(now);
        true
    }
}

/// The hash a copy carries: SHA-256 of its UTF-8, lowercase hex.
pub fn hash_hex(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether the screen here is locked. Unknown counts as locked: nothing is
/// shared, or put on this clipboard, when it cannot be told.
fn screen_locked() -> bool {
    counts_as_locked(crate::machine::backend().locked(Instant::now() + crate::machine::BUDGET))
}

fn counts_as_locked(locked: Option<bool>) -> bool {
    locked != Some(false)
}

/// What became of a copy the Shell handed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Local {
    /// It is a copy another computer sent here: it goes nowhere, phones
    /// included.
    FromAComputer,
    /// Pushed to this many computers (0 for none).
    Shared(usize),
}

/// A text copy made here (`rime remote clipboard`): push it to every
/// computer it is shared with, and say whether it came from one.
pub fn local_copy(state: &Arc<State>, text: &str, sensitive: bool) -> Local {
    local_copy_with(state, text, sensitive, Instant::now(), screen_locked, |id, clip| queue(state, id, clip))
}

fn local_copy_with(
    state: &State,
    text: &str,
    sensitive: bool,
    now: Instant,
    locked: impl FnOnce() -> bool,
    mut send: impl FnMut(&str, Clip),
) -> Local {
    let hash = hash_hex(text);
    if state.sharing.lock().was(&hash, How::Applied, now) {
        return Local::FromAComputer;
    }
    if sensitive || text.is_empty() || text.len() > MAX_CLIP_BYTES || state.clip_echo.came_from_a_phone(text) {
        return Local::Shared(0);
    }
    let targets: Vec<String> = match state.devices() {
        Ok(store) => store.computers.values().filter(|c| c.sends_clipboard()).map(|c| c.id.clone()).collect(),
        Err(_) => return Local::Shared(0),
    };
    if targets.is_empty() || locked() {
        return Local::Shared(0);
    }
    let clip = {
        let mut g = state.sharing.lock();
        if !Inner::under_rate(&mut g.sent, now) {
            return Local::Shared(0);
        }
        g.remember(&hash, How::Sent, now);
        Clip {
            text: text.to_string(),
            hash,
            seq: g.next_seq(rime_remote_core::now_ms()),
        }
    };
    for id in &targets {
        send(id, clip.clone());
    }
    Local::Shared(targets.len())
}

/// Push `clip` to computer `id` on its own thread, or leave it waiting
/// behind the push already under way (replacing any copy waiting there).
fn queue(state: &Arc<State>, id: &str, clip: Clip) {
    {
        let mut g = state.sharing.lock();
        if let Some(waiting) = g.outgoing.get_mut(id) {
            *waiting = Some(clip);
            return;
        }
        g.outgoing.insert(id.to_string(), None);
    }
    let state = Arc::clone(state);
    let id = id.to_string();
    std::thread::spawn(move || {
        let mut clip = clip;
        loop {
            if let Err(e) = push(&state, &id, &clip) {
                eprintln!("rime-remoted: a clipboard copy did not reach computer {id}: {e}");
            }
            let mut g = state.sharing.lock();
            match g.outgoing.get_mut(&id).and_then(Option::take) {
                Some(next) => clip = next,
                None => {
                    g.outgoing.remove(&id);
                    return;
                }
            }
        }
    });
}

/// One copy to one computer: dial, check it takes copies, hand it over.
fn push(state: &State, id: &str, clip: &Clip) -> Result<(), String> {
    // Asked again now: the switch may have gone off, or the computer been
    // removed, while this copy waited.
    let c = match state.devices() {
        Ok(store) => store.computers.get(id).filter(|c| c.sends_clipboard()).cloned(),
        Err(e) => return Err(e.to_string()),
    };
    let Some(c) = c else { return Ok(()) };
    let mut link = crate::dialer::open(state, &c, crate::dialer::Via::Any)?;
    let hello = link.call(br#"{"cmd":"remote_hello"}"#)?;
    if !offers(&hello, CLIP_VERB) {
        return Err(format!("'{}' runs a build that does not take clipboard copies", crate::dialer::printable(&c.name)));
    }
    let line = json!({
        "cmd": CLIP_VERB,
        "text": clip.text,
        "hash": clip.hash,
        "origin": state.identity.public_key(),
        "seq": clip.seq,
    });
    let reply = link.call(line.to_string().as_bytes())?;
    if reply["reply"] != CLIP_VERB {
        return Err(crate::dialer::printable(&reply.to_string()));
    }
    Ok(())
}

/// Whether a `remote_hello` reply lists `verb` among its features.
pub fn offers(hello: &Value, verb: &str) -> bool {
    hello["features"].as_array().is_some_and(|f| f.iter().any(|x| x.as_str() == Some(verb)))
}

fn refused(kind: &str, message: &str) -> Vec<u8> {
    json!({"reply": "error", "kind": kind, "message": message}).to_string().into_bytes()
}

fn clip_reply(applied: bool, why: Option<&str>) -> Vec<u8> {
    let mut v = json!({"reply": CLIP_VERB, "applied": applied});
    if let Some(why) = why {
        v["why"] = json!(why);
    }
    v.to_string().into_bytes()
}

/// The answer to a `peer_clipboard` from paired computer `sender`.
pub fn answer_clip(state: &State, agentd: &Path, sender: &str, line: &[u8]) -> Vec<u8> {
    answer_clip_with(state, sender, line, Instant::now(), screen_locked, |text| write_clipboard(agentd, sender, text))
}

fn answer_clip_with(
    state: &State,
    sender: &str,
    line: &[u8],
    now: Instant,
    locked: impl FnOnce() -> bool,
    write: impl FnOnce(&str) -> Result<(), String>,
) -> Vec<u8> {
    let c: Option<Device> = state.devices().ok().and_then(|s| s.computers.get(sender).cloned());
    let Some(c) = c.filter(|c| c.takes_clipboard()) else {
        return refused("not_shared", "this computer does not take clipboard copies from that one");
    };
    // Counted before the line is read, so a flood of bad lines is a flood.
    if !Inner::under_rate(state.sharing.lock().arrivals.entry(sender.to_string()).or_default(), now) {
        return refused("rate_limited", "too many clipboard copies; this one was dropped");
    }
    let Ok(v) = serde_json::from_slice::<Value>(line) else {
        return refused("bad_request", "not a JSON object");
    };
    let (Some(text), Some(hash), Some(origin), Some(seq)) =
        (v["text"].as_str(), v["hash"].as_str(), v["origin"].as_str(), v["seq"].as_u64())
    else {
        return refused("bad_request", "peer_clipboard needs text, hash, origin and seq");
    };
    let same_key = match (
        rime_remote_core::device::check_key(origin),
        rime_remote_core::device::check_key(&c.public_key),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if !same_key {
        return refused("bad_request", "origin is not the computer that sent it; copies are never passed on");
    }
    if text.is_empty() || text.len() > MAX_CLIP_BYTES {
        return refused("too_large", &format!("a copy is 1 to {MAX_CLIP_BYTES} bytes"));
    }
    if hash != hash_hex(text) {
        return refused("bad_request", "the hash does not match the text");
    }
    {
        let mut g = state.sharing.lock();
        if !g.fresh(sender, seq) {
            return clip_reply(false, Some("duplicate"));
        }
        if g.was(hash, How::Sent, now) {
            return clip_reply(false, Some("echo"));
        }
    }
    if locked() {
        return clip_reply(false, Some("locked"));
    }
    // Before the write: the Shell sees the new clipboard the moment it is
    // written and hands it back, and must find it known by then.
    state.sharing.lock().remember(hash, How::Applied, now);
    match write(text) {
        Ok(()) => clip_reply(true, None),
        Err(why) => {
            // `rime-agentd`'s own words stay here: they may name a path.
            eprintln!("rime-remoted: a copy from {} was not put on the clipboard: {why}", crate::dialer::printable(&c.name));
            refused("unavailable", "this computer could not put the copy on its clipboard")
        }
    }
}

/// Put `text` on this computer's clipboard through `rime-agentd`: a request
/// built here from the checked text, never the peer's line.
fn write_clipboard(agentd: &Path, sender: &str, text: &str) -> Result<(), String> {
    let mut a = crate::proxy::Agentd::open(agentd, &format!("computer:{sender}")).map_err(|e| e.to_string())?;
    let request = json!({"cmd": "clipboard_write", "text": text}).to_string();
    let reply = a.round_trip(request.as_bytes()).map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_slice(&reply).map_err(|e| format!("an unreadable answer: {e}"))?;
    if v["reply"] == "clipboard_written" {
        Ok(())
    } else {
        Err(v["message"].as_str().unwrap_or("the clipboard was not written").to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::tests::a_state;

    /// A computer paired here with a code, with both clipboard switches on.
    fn sharing_peer(state: &State, seed: u8) -> Device {
        let mut store = state.devices().expect("store");
        let c = store
            .pair_computer(&[seed; 32], &state.identity.public_bytes(), &format!("desk {seed}"), vec![], None, 1)
            .expect("pair");
        let c = store.set_computer_clipboard(&c.id, true, true).expect("switch");
        state.save_devices(&store).expect("save");
        c
    }

    fn key_of(seed: u8) -> String {
        rime_remote_core::b64_encode(&[seed; 32])
    }

    fn clip_line(seed: u8, text: &str, seq: u64) -> Vec<u8> {
        json!({"cmd": CLIP_VERB, "text": text, "hash": hash_hex(text), "origin": key_of(seed), "seq": seq})
            .to_string()
            .into_bytes()
    }

    /// Answer one line with the screen unlocked, and say what was written.
    fn take(state: &State, sender: &str, line: &[u8], now: Instant) -> (Value, Option<String>) {
        let mut wrote = None;
        let reply = answer_clip_with(state, sender, line, now, || false, |t| {
            wrote = Some(t.to_string());
            Ok(())
        });
        (serde_json::from_slice(&reply).expect("json"), wrote)
    }

    /// Hand a copy to `local_copy_with` with the screen unlocked; the ids it
    /// would push to.
    fn copy(state: &State, text: &str, sensitive: bool, now: Instant) -> (Local, Vec<String>) {
        let mut to = Vec::new();
        let got = local_copy_with(state, text, sensitive, now, || false, |id, _| to.push(id.to_string()));
        (got, to)
    }

    #[test]
    fn a_copy_goes_to_the_computers_whose_send_switch_is_on_and_no_other() {
        let (state, _dir) = a_state("share-targets");
        let on = sharing_peer(&state, 11);
        let off = sharing_peer(&state, 12);
        {
            let mut store = state.devices().expect("store");
            store.set_computer_clipboard(&off.id, false, true).expect("off");
            state.save_devices(&store).expect("save");
        }
        let (got, to) = copy(&state, "hello", false, Instant::now());
        assert_eq!(got, Local::Shared(1));
        assert_eq!(to, vec![on.id.clone()]);
    }

    #[test]
    fn an_introduced_computer_not_allowed_yet_neither_gets_nor_gives_a_copy() {
        let (state, _dir) = a_state("share-introduced");
        let by = sharing_peer(&state, 21);
        let introduced = {
            let mut store = state.devices().expect("store");
            let added = store
                .introduce_computer(
                    &[22; 32],
                    &state.identity.public_bytes(),
                    "lab",
                    vec![],
                    None,
                    rime_remote_core::device::Introducer { id: "phone".into(), name: "Pixel".into() },
                    5,
                )
                .expect("introduce");
            let rime_remote_core::device::Introduced::Added(c) = added else { panic!("not added") };
            // A switch cannot be turned on for it...
            assert!(store.set_computer_clipboard(&c.id, true, true).is_err());
            // ...and one set in the file by hand does not count either.
            store.computers.get_mut(&c.id).expect("there").share.clipboard_send = true;
            store.computers.get_mut(&c.id).expect("there").share.clipboard_receive = true;
            state.save_devices(&store).expect("save");
            *c
        };
        let (_, to) = copy(&state, "for the trusted only", false, Instant::now());
        assert_eq!(to, vec![by.id.clone()], "an introduced computer was sent a copy");
        let (v, wrote) = take(&state, &introduced.id, &clip_line(22, "from the lab", 1), Instant::now());
        assert_eq!(v["kind"], "not_shared", "{v}");
        assert_eq!(wrote, None);
        // Allowed by a person here, it shares like any other.
        let mut store = state.devices().expect("store");
        store.allow_computer(&introduced.id, 6).expect("allow");
        state.save_devices(&store).expect("save");
        drop(store);
        let (v, wrote) = take(&state, &introduced.id, &clip_line(22, "from the lab", 2), Instant::now());
        assert_eq!(v["applied"], true, "{v}");
        assert_eq!(wrote.as_deref(), Some("from the lab"));
    }

    #[test]
    fn a_sensitive_copy_an_oversized_one_and_one_while_locked_go_nowhere() {
        let (state, _dir) = a_state("share-never");
        let _peer = sharing_peer(&state, 31);
        let now = Instant::now();
        assert_eq!(copy(&state, "hunter2", true, now), (Local::Shared(0), vec![]));
        let most = "a".repeat(MAX_CLIP_BYTES);
        assert_eq!(copy(&state, &most, false, now).0, Local::Shared(1), "exactly the cap is shared");
        assert_eq!(copy(&state, &format!("{most}a"), false, now), (Local::Shared(0), vec![]));
        assert_eq!(copy(&state, "", false, now), (Local::Shared(0), vec![]));
        let mut to = Vec::new();
        let got = local_copy_with(&state, "while locked", false, now, || true, |id, _| to.push(id.to_string()));
        assert_eq!((got, to), (Local::Shared(0), vec![]));
    }

    #[test]
    fn receiving_while_locked_drops_the_copy_and_unknown_counts_as_locked() {
        assert!(counts_as_locked(None), "an unknown lock state counts as locked");
        assert!(counts_as_locked(Some(true)));
        assert!(!counts_as_locked(Some(false)));
        let (state, _dir) = a_state("share-locked");
        let peer = sharing_peer(&state, 32);
        let mut wrote = false;
        let reply = answer_clip_with(&state, &peer.id, &clip_line(32, "x", 1), Instant::now(), || true, |_| {
            wrote = true;
            Ok(())
        });
        let v: Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!((v["applied"].clone(), v["why"].clone()), (json!(false), json!("locked")), "{v}");
        assert!(!wrote);
    }

    #[test]
    fn a_copy_from_a_phone_goes_to_no_computer() {
        let (state, _dir) = a_state("share-phone");
        let _peer = sharing_peer(&state, 33);
        state.clip_echo.wrote("phone", "from the phone".into());
        assert_eq!(copy(&state, "from the phone", false, Instant::now()), (Local::Shared(0), vec![]));
        assert_eq!(copy(&state, "typed here", false, Instant::now()).0, Local::Shared(1));
    }

    #[test]
    fn a_bad_line_is_refused_and_writes_nothing() {
        let (state, _dir) = a_state("share-bad");
        let peer = sharing_peer(&state, 41);
        let now = Instant::now();
        let text = "the text";
        for (line, kind) in [
            (b"not json".to_vec(), "bad_request"),
            (json!({"cmd": CLIP_VERB, "text": text}).to_string().into_bytes(), "bad_request"),
            // Somebody else's key as the origin: a copy is never passed on.
            (
                json!({"cmd": CLIP_VERB, "text": text, "hash": hash_hex(text), "origin": key_of(99), "seq": 1}).to_string().into_bytes(),
                "bad_request",
            ),
            (
                json!({"cmd": CLIP_VERB, "text": text, "hash": hash_hex("other"), "origin": key_of(41), "seq": 1}).to_string().into_bytes(),
                "bad_request",
            ),
            (clip_line(41, "", 1), "too_large"),
            (clip_line(41, &"a".repeat(MAX_CLIP_BYTES + 1), 1), "too_large"),
            (
                json!({"cmd": CLIP_VERB, "text": text, "hash": hash_hex(text), "origin": key_of(41), "seq": -1}).to_string().into_bytes(),
                "bad_request",
            ),
        ] {
            let (v, wrote) = take(&state, &peer.id, &line, now);
            assert_eq!(v["kind"], kind, "{} -> {v}", String::from_utf8_lossy(&line));
            assert_eq!(wrote, None);
        }
    }

    #[test]
    fn a_repeated_seq_is_taken_once_and_a_restarted_sender_starts_past_it() {
        let (state, _dir) = a_state("share-seq");
        let peer = sharing_peer(&state, 42);
        let now = Instant::now();
        assert_eq!(take(&state, &peer.id, &clip_line(42, "one", 7), now).0["applied"], true);
        let (v, wrote) = take(&state, &peer.id, &clip_line(42, "two", 7), now);
        assert_eq!(v["why"], "duplicate", "{v}");
        assert_eq!(wrote, None);
        // A sender's seq is above the clock: one that restarts gives out
        // numbers past every one it gave before.
        let mut before = Inner::default();
        let a = before.next_seq(1_000);
        let b = before.next_seq(1_000);
        assert!(b > a);
        let mut restarted = Inner::default();
        assert!(restarted.next_seq(1_001) > b - 1, "a restarted sender repeated a seq");
    }

    #[test]
    fn a_flood_is_cut_off_at_the_rate() {
        let (state, _dir) = a_state("share-rate");
        let peer = sharing_peer(&state, 43);
        let now = Instant::now();
        for i in 0..CLIP_RATE as u64 {
            let (v, _) = take(&state, &peer.id, &clip_line(43, &format!("copy {i}"), i + 1), now);
            assert_eq!(v["reply"], CLIP_VERB, "{v}");
        }
        let (v, wrote) = take(&state, &peer.id, &clip_line(43, "one too many", 999), now);
        assert_eq!(v["kind"], "rate_limited", "{v}");
        assert_eq!(wrote, None);
        // A window later it is taken again.
        let (v, _) = take(&state, &peer.id, &clip_line(43, "later", 1000), now + RATE_WINDOW);
        assert_eq!(v["applied"], true, "{v}");
    }

    /// Two computers' worth of memory in one process: what A sends B, B puts
    /// on its clipboard; B's Shell hands it back and B sends it nowhere.
    #[test]
    fn two_computers_settle_with_the_copy_applied_once_and_no_echo() {
        let (a, _da) = a_state("share-echo-a");
        let (b, _db) = a_state("share-echo-b");
        let b_on_a = pair_both(&a, &b);
        let now = Instant::now();
        let mut out = Vec::new();
        assert_eq!(
            local_copy_with(&a, "made on A", false, now, || false, |id, clip| out.push((id.to_string(), clip))),
            Local::Shared(1)
        );
        assert_eq!(out[0].0, b_on_a);
        let a_on_b = computer_id(&a);
        let (v, wrote) = take(&b, &a_on_b, &line_of(&a, &out[0].1), now);
        assert_eq!(v["applied"], true, "{v}");
        assert_eq!(wrote.as_deref(), Some("made on A"));
        // B's Shell sees the new clipboard and hands it over.
        assert_eq!(copy(&b, "made on A", false, now), (Local::FromAComputer, vec![]), "B sent A's copy on");
        // And had B sent it back anyway, A drops its own copy.
        let back = json!({"cmd": CLIP_VERB, "text": "made on A", "hash": hash_hex("made on A"),
                          "origin": b.identity.public_key(), "seq": 1});
        let (v, wrote) = take(&a, &b_on_a, back.to_string().as_bytes(), now);
        assert_eq!(v["why"], "echo", "{v}");
        assert_eq!(wrote, None);
    }

    /// Three computers, all sharing both ways: one copy on A is applied on B
    /// and C once each, and neither sends anything on.
    #[test]
    fn three_computers_settle_with_each_copy_applied_once() {
        let (a, _da) = a_state("share-three-a");
        let (b, _db) = a_state("share-three-b");
        let (c, _dc) = a_state("share-three-c");
        for (x, y) in [(&a, &b), (&a, &c), (&b, &c)] {
            pair_both(x, y);
        }
        let all = [&a, &b, &c];
        let now = Instant::now();
        // Every computer's outgoing pushes, delivered until none are left.
        let mut wire: Vec<(usize, String, Clip)> = Vec::new();
        let mut applied = [0usize; 3];
        let push = |from: usize, text: &str, wire: &mut Vec<(usize, String, Clip)>| {
            local_copy_with(all[from], text, false, now, || false, |id, clip| wire.push((from, id.to_string(), clip)))
        };
        push(0, "one copy", &mut wire);
        let mut steps = 0;
        while let Some((from, to_id, clip)) = wire.pop() {
            steps += 1;
            assert!(steps < 20, "the copy is still going round");
            let to = all.iter().position(|s| computer_id(s) == to_id).expect("to");
            let (v, wrote) = take(all[to], &computer_id(all[from]), &line_of(all[from], &clip), now);
            assert_ne!(v["reply"], "error", "{v}");
            if let Some(text) = wrote {
                applied[to] += 1;
                // The receiving Shell hands the new clipboard over, and it
                // goes nowhere: settling does not lean on the other end
                // dropping its own copy.
                let handed = push(to, &text, &mut wire);
                assert!(matches!(handed, Local::FromAComputer), "computer {to} passed a copy on");
            }
        }
        assert_eq!(applied, [0, 1, 1]);
        assert_eq!(steps, 2, "one push to each other computer, and none after");
    }

    /// Pair `x` and `y` both ways with both clipboard switches on; `y`'s id
    /// on `x`.
    fn pair_both(x: &State, y: &State) -> String {
        let mut sx = x.devices().expect("store");
        let y_on_x = sx
            .pair_computer(&y.identity.public_bytes(), &x.identity.public_bytes(), "y", vec![], None, 1)
            .expect("pair");
        sx.set_computer_clipboard(&y_on_x.id, true, true).expect("on");
        x.save_devices(&sx).expect("save");
        let mut sy = y.devices().expect("store");
        let x_on_y = sy
            .pair_computer(&x.identity.public_bytes(), &y.identity.public_bytes(), "x", vec![], None, 1)
            .expect("pair");
        sy.set_computer_clipboard(&x_on_y.id, true, true).expect("on");
        y.save_devices(&sy).expect("save");
        y_on_x.id
    }

    /// A computer's id, as any other computer stores it.
    fn computer_id(s: &State) -> String {
        Device::id_for(&s.identity.public_key())
    }

    fn line_of(from: &State, clip: &Clip) -> Vec<u8> {
        json!({"cmd": CLIP_VERB, "text": clip.text, "hash": clip.hash, "origin": from.identity.public_key(), "seq": clip.seq})
            .to_string()
            .into_bytes()
    }
}
