//! What paired computers tell each other on every connection, and the
//! background refresh that connects (`docs/remote-live-contract.md` §2.15).
//!
//! ## On every connection
//!
//! Whichever computer dials (for `ping`, the refresh, or to tell another it
//! was removed) runs [`exchange`] once the session is open: its tombstones
//! for the other's (`peer_tombstones`), then its presence for the other's
//! (`peer_presence`). The side that was dialled answers both from
//! `computers::peer_reply` with [`answer_tombstones`] and [`answer_presence`].
//! Either side checks what it got whole (`rime_remote_core::peer`) and stores
//! nothing from a message that fails. A computer whose `remote_hello` does
//! not list a verb (an older daemon) is not sent it.
//!
//! Tombstones are attributed to the computer whose key completed the
//! handshake, never to a key a message names: an entry naming this
//! computer's own key means that computer removed this one, so that computer
//! is removed here. Entries naming other computers remove them too (a
//! removal reaches every member); entries naming phones are skipped
//! (`DeviceStore::apply_tombstones`).
//!
//! ## Telling a removed computer
//!
//! Removing a computer here asks the refresh thread to dial it at once and
//! pass the list that names it ([`Wake::tell_now`]), so it removes this one
//! too. It still accepts this computer then: the removal is only here. When
//! it is not reachable, the record keeps no `told_ms`, and it is tried again
//! when this daemon starts and whenever the refresh is awake for a paired
//! computer anyway, for [`TELL_FOR_MS`]. There is no timer for it alone:
//! with no paired computer the refresh sleeps until something changes. The
//! other direction cannot help: once removed here, its handshakes are refused
//! before a byte of the session, and a refused handshake proves nothing
//! (a rollback to an older daemon refuses too), so a removal is never
//! inferred from one.
//!
//! ## The refresh
//!
//! [`supervise`] dials each paired computer every [`AC_INTERVAL`] on AC and
//! [`BATTERY_INTERVAL`] on battery, doubling the wait after each failure up
//! to [`MAX_BACKOFF`]. [`plan`] decides, as a pure function:
//! * no paired computer: sleep until a pairing, removal or forget wakes it,
//!   with no timeout and without reading the power state;
//! * nothing due yet: sleep until the first is due, without reading it;
//! * something due in power-saver: dial nothing, read the power state again
//!   after [`SAVER_RECHECK`] (that read is how it notices power-saver ended).
//!
//! Presence is kept in memory ([`Board`]), never in `computers.json`: a
//! refresh writes nothing to disk unless a tombstone changed the store.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rime_remote_core::peer::{self, Presence, Tombstone};
use serde_json::{json, Value};

use crate::state::State;

/// How often a paired computer is dialled on AC.
pub const AC_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// How often on battery.
pub const BATTERY_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// The longest wait after failures.
pub const MAX_BACKOFF: Duration = Duration::from_secs(60 * 60);
/// How long power-saver waits before reading the power state again.
pub const SAVER_RECHECK: Duration = Duration::from_secs(15 * 60);
/// How long after the daemon starts before the first refresh: the network
/// may still be coming up.
pub const START_DELAY: Duration = Duration::from_secs(20);
/// How long after a computer is paired before it is first dialled: the other
/// side stores the pairing a moment after this one.
pub const SETTLE: Duration = Duration::from_secs(5);
/// How long a removed computer that was never reached keeps being told.
pub const TELL_FOR_MS: u64 = 30 * 24 * 3600 * 1000;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The last presence each paired computer gave, and when. Memory only.
#[derive(Default)]
pub struct Board(Mutex<BTreeMap<String, (Presence, u64)>>);

impl Board {
    pub fn set(&self, id: &str, presence: Presence, seen_ms: u64) {
        lock(&self.0).insert(id.to_string(), (presence, seen_ms));
    }

    pub fn get(&self, id: &str) -> Option<(Presence, u64)> {
        lock(&self.0).get(id).cloned()
    }

    pub fn forget(&self, id: &str) {
        lock(&self.0).remove(id);
    }
}

/// How the rest of the daemon wakes the refresh thread.
#[derive(Default)]
pub struct Wake {
    m: Mutex<WakeState>,
    cv: Condvar,
}

#[derive(Default)]
struct WakeState {
    generation: u64,
    tell: BTreeSet<String>,
}

impl Wake {
    /// The paired computers changed: look again.
    pub fn poke(&self) {
        lock(&self.m).generation += 1;
        self.cv.notify_all();
    }

    /// Computer `id` was removed here: dial it now and tell it, whatever the
    /// power state.
    pub fn tell_now(&self, id: &str) {
        let mut s = lock(&self.m);
        s.tell.insert(id.to_string());
        s.generation += 1;
        self.cv.notify_all();
    }

    fn generation(&self) -> u64 {
        lock(&self.m).generation
    }

    /// Sleep until poked after `seen`, or `timeout` (forever when `None`).
    /// The generation now, and what to tell at once.
    fn wait(&self, seen: u64, timeout: Option<Duration>) -> (u64, BTreeSet<String>) {
        let mut s = lock(&self.m);
        let deadline = timeout.map(|t| Instant::now() + t);
        while s.generation == seen && s.tell.is_empty() {
            match deadline {
                None => s = self.cv.wait(s).unwrap_or_else(|p| p.into_inner()),
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        break;
                    }
                    s = self.cv.wait_timeout(s, d - now).unwrap_or_else(|p| p.into_inner()).0;
                }
            }
        }
        (s.generation, std::mem::take(&mut s.tell))
    }
}

/// What the refresh needs to know about power.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Power {
    pub on_ac: Option<bool>,
    /// The battery says it is discharging.
    pub discharging: bool,
    pub mode: Option<String>,
}

impl Power {
    pub fn from_status(s: &crate::machine::Status) -> Power {
        Power {
            on_ac: s.on_ac,
            discharging: s.battery.as_ref().is_some_and(|b| b.state == "discharging"),
            mode: s.power_mode.clone(),
        }
    }

    pub fn saver(&self) -> bool {
        self.mode.as_deref() == Some("power-saver")
    }

    /// On battery: rimed says so, or (no rimed) the battery is discharging.
    /// A desktop with neither is on AC.
    pub fn on_battery(&self) -> bool {
        match self.on_ac {
            Some(ac) => !ac,
            None => self.discharging,
        }
    }
}

/// How often to dial a paired computer.
pub fn interval(p: &Power) -> Duration {
    if p.on_battery() {
        BATTERY_INTERVAL
    } else {
        AC_INTERVAL
    }
}

/// When to dial next after `failures` failures in a row (0 after a
/// success): the interval, doubled per failure, at most [`MAX_BACKOFF`].
pub fn next_due(now_ms: u64, interval: Duration, failures: u32) -> u64 {
    let wait = interval
        .checked_mul(1u32.checked_shl(failures.min(16)).unwrap_or(u32::MAX))
        .unwrap_or(MAX_BACKOFF)
        .min(MAX_BACKOFF);
    now_ms + wait.as_millis() as u64
}

/// What the refresh does next.
#[derive(Debug, PartialEq)]
pub enum Plan {
    /// No paired computer: sleep until something changes, no timeout.
    Idle,
    /// Sleep this long.
    Wait(Duration),
    /// Dial these now; the next ones are due `interval` after.
    Dial { ids: Vec<String>, interval: Duration },
}

/// The refresh's decision. `active` maps each paired computer to when it is
/// due, `untold` each removed one still to be told. `power` is read only
/// when a paired computer is due. A removed computer is dialled only along
/// with a paired one: it never wakes the refresh by itself.
pub fn plan(
    now_ms: u64,
    active: &BTreeMap<String, u64>,
    untold: &BTreeMap<String, u64>,
    power: impl FnOnce() -> Power,
) -> Plan {
    let Some(earliest) = active.values().min().copied() else {
        return Plan::Idle;
    };
    if earliest > now_ms {
        return Plan::Wait(Duration::from_millis(earliest - now_ms));
    }
    let p = power();
    if p.saver() {
        return Plan::Wait(SAVER_RECHECK);
    }
    let ids = active
        .iter()
        .chain(untold.iter())
        .filter(|(_, due)| **due <= now_ms)
        .map(|(id, _)| id.clone())
        .collect();
    Plan::Dial { ids, interval: interval(&p) }
}

/// This computer's presence, as the machine backend reads it.
pub fn own(state: &State) -> Presence {
    let backend = crate::machine::backend();
    let s = backend.status(Instant::now() + crate::machine::BUDGET);
    Presence {
        name: state.machine.clone(),
        battery: s.battery.map(|b| peer::Battery { percent: b.percent.min(100), charging: b.state == "charging" }),
        on_ac: s.on_ac,
        power_mode: s.power_mode.filter(|m| peer::POWER_MODES.contains(&m.as_str())),
        locked: s.locked,
        network: backend.network().filter(|n| peer::NETWORKS.contains(&n.as_str())),
        uptime_s: backend.uptime_s().filter(|u| *u <= peer::MAX_UPTIME_S),
    }
}

/// Record tombstones computer `sender` passed on, save when the store
/// changed, and end what the removed computers held. `keep` is a session
/// that is answering right now, ended after its reply instead (`serve.rs`).
fn record(state: &State, sender: &str, list: &[Tombstone], keep: Option<&str>) {
    let (applied, removed) = {
        let Ok(mut store) = state.devices() else { return };
        let applied = store.apply_tombstones(sender, &state.identity.public_bytes(), list, rime_remote_core::now_ms());
        if applied.changed {
            if let Err(e) = state.save_devices(&store) {
                eprintln!("rime-remoted: a removal another computer passed on was not written: {e}");
            }
        }
        let removed: Vec<(String, String)> = applied
            .removed
            .iter()
            .filter_map(|id| store.computers.get(id).map(|c| (c.public_key.clone(), c.name.clone())))
            .collect();
        (applied, removed)
    };
    // The phones forget them too (intro.rs).
    for (key, name) in &removed {
        crate::intro::tell_removed(state, key, name);
    }
    for id in &applied.removed {
        state.presence.forget(id);
        crate::handoff::forget(state, id);
        if Some(id.as_str()) != keep {
            state.drop_connections_for(id);
        }
    }
}

fn bad_request(message: &str) -> Vec<u8> {
    json!({"reply": "error", "kind": "bad_request", "message": message}).to_string().into_bytes()
}

/// `peer_tombstones` from computer `sender`: record its list, answer with
/// this computer's (after recording).
pub fn answer_tombstones(state: &State, sender: &str, line: &[u8]) -> Vec<u8> {
    let v: Value = serde_json::from_slice(line).unwrap_or(Value::Null);
    let list = match peer::checked_tombstones(&v["tombstones"], rime_remote_core::now_ms()) {
        Ok(l) => l,
        Err(why) => return bad_request(&format!("refused, nothing was recorded: {why}")),
    };
    record(state, sender, &list, Some(sender));
    let mine = state.devices().map(|s| s.computer_tombstones()).unwrap_or_default();
    json!({"reply": "peer_tombstones", "tombstones": mine}).to_string().into_bytes()
}

/// `peer_presence` from computer `sender`: keep its presence, answer with
/// this computer's.
pub fn answer_presence(state: &State, sender: &str, line: &[u8]) -> Vec<u8> {
    let v: Value = serde_json::from_slice(line).unwrap_or(Value::Null);
    let theirs = match Presence::checked(&v["presence"]) {
        Ok(p) => p,
        Err(why) => return bad_request(&format!("refused, nothing was kept: {why}")),
    };
    state.presence.set(sender, theirs, rime_remote_core::now_ms());
    json!({"reply": "peer_presence", "presence": own(state)}).to_string().into_bytes()
}

/// Both exchanges on an open link to computer `c`, as the side that dialled.
pub fn exchange(state: &State, link: &mut crate::dialer::Link, c: &rime_remote_core::device::Device) -> Result<(), String> {
    let hello = link.call(br#"{"cmd":"remote_hello"}"#)?;
    let offers = |verb: &str| {
        hello["features"].as_array().is_some_and(|f| f.iter().any(|x| x.as_str() == Some(verb)))
    };
    if offers("peer_tombstones") {
        let mine = list_for(&*state.devices().map_err(|e| e.to_string())?, c);
        let names_them = mine.iter().any(|t| t.key == c.public_key);
        let reply = link.call(json!({"cmd": "peer_tombstones", "tombstones": mine}).to_string().as_bytes())?;
        if reply["reply"] != "peer_tombstones" {
            return Err(format!("'{}' did not take the removals: {}", c.name, crate::dialer::printable(&reply.to_string())));
        }
        take_their_removals(state, c, &reply["tombstones"])?;
        if names_them {
            let mut store = state.devices().map_err(|e| e.to_string())?;
            store.computer_told(&c.id, rime_remote_core::now_ms());
            let _ = state.save_devices(&store);
        }
    }
    if !state.computer_active(&c.id) {
        return Ok(());
    }
    if offers("peer_presence") {
        let reply = link.call(json!({"cmd": "peer_presence", "presence": own(state)}).to_string().as_bytes())?;
        if reply["reply"] != "peer_presence" {
            return Err(format!("'{}' did not give its presence: {}", c.name, crate::dialer::printable(&reply.to_string())));
        }
        let theirs = Presence::checked(&reply["presence"])
            .map_err(|e| format!("'{}' gave presence that was refused: {e}", crate::dialer::printable(&c.name)))?;
        state.presence.set(&c.id, theirs, rime_remote_core::now_ms());
    }
    Ok(())
}

/// The list sent to computer `c`. A paired computer gets every removal this
/// one passes on; a removed one is told only that it was removed: the keys
/// and names of the computers still paired here are none of its business, and
/// its own entry can never be cut off the end of a full list.
fn list_for(store: &rime_remote_core::device::DeviceStore, c: &rime_remote_core::device::Device) -> Vec<Tombstone> {
    if c.is_active() {
        return store.computer_tombstones();
    }
    store
        .computers
        .get(&c.id)
        .filter(|r| r.is_tombstone() && r.public_key == c.public_key)
        .map(|r| Tombstone { key: r.public_key.clone(), removed_ms: r.revoked_ms.unwrap_or(0), name: r.name.clone() })
        .into_iter()
        .collect()
}

/// The removals computer `c` answered with, recorded only while it is still
/// paired here. A computer this one removed is dialled only to be told: what
/// it answers is never read, or a removed (perhaps stolen) machine could
/// remove every computer still paired here.
fn take_their_removals(state: &State, c: &rime_remote_core::device::Device, list: &Value) -> Result<(), String> {
    if !c.is_active() || !state.computer_active(&c.id) {
        return Ok(());
    }
    let theirs = peer::checked_tombstones(list, rime_remote_core::now_ms())
        .map_err(|e| format!("'{}' passed on removals that were refused: {e}", crate::dialer::printable(&c.name)))?;
    record(state, &c.id, &theirs, None);
    Ok(())
}

/// Dial computer `id` and exchange: the refresh's one step, and the tell.
/// Writes nothing to disk unless the exchange changed the store or the
/// address that answered is not the first one kept.
fn dial(state: &State, id: &str) -> Result<(), String> {
    let c = state
        .devices()
        .map_err(|e| e.to_string())?
        .computers
        .get(id)
        .cloned()
        .ok_or_else(|| format!("no computer {id}"))?;
    let mut link = crate::dialer::open(state, &c, crate::dialer::Via::Any)?;
    if c.is_active() {
        let mut store = state.devices().map_err(|e| e.to_string())?;
        store.seen(&c.id, rime_remote_core::now_ms(), link.path);
        if let Some(a) = link.address.as_deref().filter(|a| c.addresses.first().map(String::as_str) != Some(a)) {
            store.computer_reached_at(&c.id, a);
            let _ = state.save_devices(&store);
        }
    }
    exchange(state, &mut link, &c)
}

/// Paired computers, and removed ones still to be told (reachable, removed
/// less than [`TELL_FOR_MS`] ago).
fn targets(state: &State, now_ms: u64) -> (BTreeSet<String>, BTreeSet<String>) {
    let Ok(store) = state.devices() else { return Default::default() };
    let active = store.computers.values().filter(|c| c.is_active()).map(|c| c.id.clone()).collect();
    let untold = store
        .computers
        .values()
        .filter(|c| c.is_tombstone() && c.told_ms.is_none() && !c.learned)
        .filter(|c| c.revoked_ms.is_some_and(|r| r.saturating_add(TELL_FOR_MS) > now_ms))
        .filter(|c| !c.addresses.is_empty() || state.relay.is_some())
        .map(|c| c.id.clone())
        .collect();
    (active, untold)
}

struct Entry {
    due_ms: u64,
    failures: u32,
}

/// The refresh thread, for the life of the daemon.
pub fn supervise(state: Arc<State>) {
    let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
    let mut generation = state.refresh.generation();
    let started = rime_remote_core::now_ms();
    // At start, every removed computer not yet told gets one try.
    let mut tell: BTreeSet<String> = targets(&state, started).1;
    loop {
        for id in std::mem::take(&mut tell) {
            if let Err(e) = dial(&state, &id) {
                eprintln!("rime-remoted: could not tell a removed computer ({id}) yet: {e}");
            }
        }
        let now = rime_remote_core::now_ms();
        let (active, untold) = targets(&state, now);
        entries.retain(|id, _| active.contains(id) || untold.contains(id));
        let first_due = (started + START_DELAY.as_millis() as u64).max(now + SETTLE.as_millis() as u64);
        for id in &active {
            entries.entry(id.clone()).or_insert(Entry { due_ms: first_due, failures: 0 });
        }
        for id in &untold {
            entries.entry(id.clone()).or_insert(Entry { due_ms: now + AC_INTERVAL.as_millis() as u64, failures: 0 });
        }
        let due = |set: &BTreeSet<String>| -> BTreeMap<String, u64> {
            set.iter().filter_map(|id| Some((id.clone(), entries.get(id)?.due_ms))).collect()
        };
        let decision = plan(now, &due(&active), &due(&untold), || {
            Power::from_status(&crate::machine::backend().power(Instant::now() + crate::machine::BUDGET))
        });
        match decision {
            Plan::Idle => (generation, tell) = state.refresh.wait(generation, None),
            Plan::Wait(d) => (generation, tell) = state.refresh.wait(generation, Some(d)),
            Plan::Dial { ids, interval } => {
                for id in ids {
                    let ok = dial(&state, &id);
                    let Some(e) = entries.get_mut(&id) else { continue };
                    match ok {
                        Ok(()) => e.failures = 0,
                        Err(why) => {
                            e.failures = e.failures.saturating_add(1);
                            if e.failures == 1 {
                                eprintln!("rime-remoted: a paired computer ({id}) did not answer; trying less often: {why}");
                            }
                        }
                    }
                    e.due_ms = next_due(rime_remote_core::now_ms(), interval, e.failures);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn due(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    fn ac() -> Power {
        Power { on_ac: Some(true), discharging: false, mode: Some("balanced".into()) }
    }

    #[test]
    fn with_no_paired_computer_the_refresh_sleeps_without_a_timeout_and_reads_nothing() {
        let read = Cell::new(false);
        let p = || {
            read.set(true);
            ac()
        };
        assert_eq!(plan(1_000, &due(&[]), &due(&[]), p), Plan::Idle);
        // A removed computer still to be told does not wake it either.
        assert_eq!(plan(1_000, &due(&[]), &due(&[("gone", 0)]), || { read.set(true); ac() }), Plan::Idle);
        assert!(!read.get(), "the power state was read with nothing to dial");
        // Nothing due yet: sleep until the first, still without reading it.
        assert_eq!(
            plan(1_000, &due(&[("a", 5_000), ("b", 3_000)]), &due(&[]), || { read.set(true); ac() }),
            Plan::Wait(Duration::from_millis(2_000))
        );
        assert!(!read.get());
    }

    #[test]
    fn power_saver_dials_nothing_and_looks_again_later() {
        let saver = Power { on_ac: Some(false), discharging: true, mode: Some("power-saver".into()) };
        assert_eq!(plan(10, &due(&[("a", 0)]), &due(&[("gone", 0)]), || saver.clone()), Plan::Wait(SAVER_RECHECK));
        let saver_on_ac = Power { on_ac: Some(true), ..saver };
        assert_eq!(plan(10, &due(&[("a", 0)]), &due(&[]), || saver_on_ac), Plan::Wait(SAVER_RECHECK));
    }

    #[test]
    fn what_is_due_is_dialled_every_five_minutes_on_ac_and_fifteen_on_battery() {
        let r = plan(100, &due(&[("a", 100), ("b", 500)]), &due(&[("gone", 50), ("later", 900)]), ac);
        assert_eq!(r, Plan::Dial { ids: vec!["a".into(), "gone".into()], interval: AC_INTERVAL });
        let battery = Power { on_ac: Some(false), discharging: true, mode: Some("balanced".into()) };
        assert!(matches!(plan(100, &due(&[("a", 0)]), &due(&[]), || battery), Plan::Dial { interval, .. } if interval == BATTERY_INTERVAL));
        // No rimed: a discharging battery is battery, and no battery is AC.
        assert_eq!(interval(&Power { on_ac: None, discharging: true, mode: None }), BATTERY_INTERVAL);
        assert_eq!(interval(&Power { on_ac: None, discharging: false, mode: None }), AC_INTERVAL);
        assert!(!Power { on_ac: None, discharging: false, mode: None }.saver());
    }

    #[test]
    fn failures_double_the_wait_up_to_an_hour() {
        let m = |d: Duration| d.as_millis() as u64;
        assert_eq!(next_due(0, AC_INTERVAL, 0), m(AC_INTERVAL));
        assert_eq!(next_due(0, AC_INTERVAL, 1), m(AC_INTERVAL * 2));
        assert_eq!(next_due(0, AC_INTERVAL, 3), m(AC_INTERVAL * 8));
        assert_eq!(next_due(0, AC_INTERVAL, 4), m(MAX_BACKOFF), "80 minutes is capped");
        assert_eq!(next_due(0, BATTERY_INTERVAL, 2), m(MAX_BACKOFF));
        assert_eq!(next_due(7, AC_INTERVAL, u32::MAX), 7 + m(MAX_BACKOFF));
    }

    #[test]
    fn a_wait_ends_on_a_poke_or_a_tell_and_otherwise_at_its_timeout() {
        let w = Arc::new(Wake::default());
        let g = w.generation();
        let t = Instant::now();
        assert_eq!(w.wait(g, Some(Duration::from_millis(50))).0, g);
        assert!(t.elapsed() >= Duration::from_millis(50));
        let w2 = Arc::clone(&w);
        let h = std::thread::spawn(move || w2.wait(g, None));
        std::thread::sleep(Duration::from_millis(50));
        w.tell_now("b");
        let (g2, tell) = h.join().expect("woke");
        assert!(g2 > g);
        assert_eq!(tell.into_iter().collect::<Vec<_>>(), ["b"]);
        let w2 = Arc::clone(&w);
        let h = std::thread::spawn(move || w2.wait(g2, None));
        std::thread::sleep(Duration::from_millis(50));
        w.poke();
        assert!(h.join().expect("woke").0 > g2);
    }

    #[test]
    fn this_computers_presence_passes_the_other_sides_check() {
        let (state, dir) = crate::serve::tests::a_state("own-presence");
        let mine = own(&state);
        let v = serde_json::to_value(&mine).expect("json");
        assert_eq!(Presence::checked(&v).expect("checked"), mine);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_removed_computer_dialled_to_be_told_removes_nothing_here_and_learned_ones_are_never_dialled() {
        let (state, dir) = crate::serve::tests::a_state("peer-told");
        let own = state.identity.public_bytes();
        let (b, c) = {
            let mut store = state.devices().expect("store");
            let b = store.pair_computer(&[7u8; 32], &own, "b", vec![], None, 1).expect("b");
            let c = store.pair_computer(&[8u8; 32], &own, "c", vec!["192.0.2.8:1".into()], None, 1).expect("c");
            (b, c)
        };
        let now = rime_remote_core::now_ms();
        // c's answer names b and a stranger.
        let answer = json!([
            {"key": rime_remote_core::b64_encode(&[7u8; 32]), "removed_ms": now, "name": "b"},
            {"key": rime_remote_core::b64_encode(&[9u8; 32]), "removed_ms": now, "name": "s"},
        ]);
        // c is removed here, so it is dialled only to be told.
        let removed = {
            let mut store = state.devices().expect("store");
            let removed = store.revoke_computer(&c.id, now).expect("removed");
            state.save_devices(&store).expect("saved");
            removed
        };
        take_their_removals(&state, &removed, &answer).expect("told");
        assert!(state.computer_active(&b.id), "a removed computer removed one still paired here");
        assert_eq!(state.devices().expect("store").computers.len(), 2, "a removed computer's list was recorded");
        assert_eq!(targets(&state, now).1, BTreeSet::from([c.id.clone()]));

        // From b, still paired, the same kind of list counts: the stranger is
        // learned and never dialled.
        let answer = json!([{"key": rime_remote_core::b64_encode(&[9u8; 32]), "removed_ms": now, "name": "s"}]);
        take_their_removals(&state, &b, &answer).expect("taken");
        assert_eq!(state.devices().expect("store").computers.len(), 3);
        assert_eq!(targets(&state, now).1, BTreeSet::from([c.id.clone()]), "a learned removal is dialled");
        // Forgotten here, then removed again by a newer list: still never
        // dialled, though the new removal waits to be told.
        let stranger = rime_remote_core::device::Device::id_for(&rime_remote_core::b64_encode(&[9u8; 32]));
        state.devices().expect("store").forget_computer(&stranger, now + 1).expect("forgotten");
        let again = json!([{"key": rime_remote_core::b64_encode(&[9u8; 32]), "removed_ms": now + 2, "name": "s"}]);
        take_their_removals(&state, &b, &again).expect("taken");
        {
            // Even one with an address to dial (none is ever learned with one).
            let mut store = state.devices().expect("store");
            let s = store.computers.get_mut(&stranger).expect("learned");
            assert!(s.is_tombstone() && s.told_ms.is_none());
            s.addresses = vec!["192.0.2.9:1".into()];
        }
        assert_eq!(targets(&state, now + 2).1, BTreeSet::from([c.id.clone()]), "a learned removal is dialled after a forget");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_removed_computer_is_told_only_its_own_removal_however_long_the_list() {
        let own = [99u8; 32];
        let mut store = rime_remote_core::device::DeviceStore::default();
        // c removed first, then more computers removed here than a list holds.
        let c = store.pair_computer(&[1u8; 32], &own, "c", vec![], None, 1).expect("c");
        store.revoke_computer(&c.id, 10).expect("removed");
        for i in 0..(peer::MAX_TOMBSTONES + 3) {
            let mut k = [0x40u8; 32];
            k[0] = (i >> 8) as u8;
            k[1] = i as u8;
            let o = store.pair_computer(&k, &own, "o", vec![], None, 1).expect("o");
            store.revoke_computer(&o.id, 1_000 + i as u64).expect("removed");
        }
        let removed = store.computers[&c.id].clone();
        let told = list_for(&store, &removed);
        assert_eq!(told.len(), 1, "a removed computer was sent other computers' removals");
        assert_eq!((told[0].key.as_str(), told[0].removed_ms), (c.public_key.as_str(), 10));
        // A paired computer gets the full list, capped.
        let p = store.pair_computer(&[2u8; 32], &own, "p", vec![], None, 1).expect("p");
        assert_eq!(list_for(&store, &p).len(), peer::MAX_TOMBSTONES);
        // A removal forgotten here tells nothing.
        store.forget_computer(&c.id, 20).expect("forgotten");
        assert!(list_for(&store, &store.computers[&c.id].clone()).is_empty());
    }

    #[test]
    fn garbage_from_a_peer_is_refused_and_nothing_is_kept() {
        let (state, dir) = crate::serve::tests::a_state("peer-garbage");
        let own_key = state.identity.public_key();
        let sender = {
            let mut store = state.devices().expect("store");
            let c = store
                .pair_computer(&[7u8; 32], &state.identity.public_bytes(), "desk", vec![], None, 1)
                .expect("paired");
            c.id
        };
        for line in [
            json!({"cmd": "peer_presence", "presence": {"name": "desk", "battery": {"percent": 900, "charging": true}}}),
            json!({"cmd": "peer_presence", "presence": {"name": "desk", "network": "x".repeat(10_000)}}),
            json!({"cmd": "peer_presence"}),
        ] {
            let r: Value = serde_json::from_slice(&answer_presence(&state, &sender, line.to_string().as_bytes())).expect("json");
            assert_eq!(r["kind"], "bad_request", "{line}: {r}");
            assert!(state.presence.get(&sender).is_none(), "garbage presence was kept: {line}");
        }
        let good = json!({"cmd": "peer_presence", "presence": {"name": "desk", "uptime_s": 5}});
        let r: Value = serde_json::from_slice(&answer_presence(&state, &sender, good.to_string().as_bytes())).expect("json");
        assert_eq!(r["reply"], "peer_presence", "{r}");
        assert_eq!(state.presence.get(&sender).map(|(p, _)| p.uptime_s), Some(Some(5)));

        // A tombstone list with one bad entry records none of it, including
        // the good entry before it and this computer's own key.
        let bad = json!({"cmd": "peer_tombstones", "tombstones": [
            {"key": own_key, "removed_ms": 5, "name": "me"},
            {"key": rime_remote_core::b64_encode(&[9u8; 32]), "removed_ms": 5, "name": "x"},
            {"key": rime_remote_core::b64_encode(&[8u8; 32]), "removed_ms": u64::MAX, "name": "future"},
        ]});
        let r: Value = serde_json::from_slice(&answer_tombstones(&state, &sender, bad.to_string().as_bytes())).expect("json");
        assert_eq!(r["kind"], "bad_request", "{r}");
        let store = state.devices().expect("store");
        assert_eq!(store.computers.len(), 1, "a refused list was recorded");
        assert!(store.computers[&sender].is_active(), "a refused list removed its sender");
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }
}
