//! The UI boost (`org.rimeos.Rimed1.Ui.Boost`): a short, capped raise of the
//! CPU's energy/performance preference for the length of one animation.
//!
//! WHY: on a laptop on battery the Daily tier is `balanced` (governor
//! `powersave`, EPP `balance_power`). An animation is a burst of work every
//! 16 ms; to the firmware's utilisation estimate that is low load, so clocks
//! stay low and frames run late. Changing the tier would cost battery all day.
//! Instead the Shell (on the input event, before the first animated frame) and
//! the Hyprland watcher (`rime ui hypr-boost`, on a window or workspace event)
//! ask for a boost of a few hundred milliseconds, and rimed puts the tier's own
//! EPP back the moment the last animation ends.
//!
//! The policy half ([`Engine`]) is pure and takes the time as an argument, so
//! every rule below is tested without sleeping:
//!
//! - **Cap.** One call buys at most [`Limits::per_call`] (600 ms).
//! - **Coalesce, never stack.** A call while boosted moves the end of the ONE
//!   window to `max(current end, now + ask)`; it never adds durations.
//! - **Ceiling + cooldown.** One continuous window ends no later than
//!   [`Limits::continuous`] after it started, and a window that reached the
//!   ceiling is followed by [`Limits::cooldown`] in which nothing starts. A
//!   client that calls in a loop therefore gets at most
//!   continuous / (continuous + cooldown) of the time, not all of it.
//! - **Per-caller rate.** Each bus connection has a token bucket; a caller over
//!   it gets `LimitsExceeded`, not a boost.
//! - **Release.** `Boost(0)` gives this caller's share back; a caller that
//!   disconnects gives it back too. When nobody holds the window it ends now.
//!
//! The hardware half (the `impl Ctx` below) writes only through the
//! [`SysWriter`](rimed_core::SysWriter), writes once per window (an extension
//! writes nothing), and ends a window by applying the tier's OWN EPP action
//! ([`Profile::ui_boost_plan`](rimed_core::Profile::ui_boost_plan)). Any tier
//! change cancels a window outright (see `Ctx::apply_tier`): the tier's writes
//! land last and nothing stale is written after them.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rimed_core::tier::{Action, Tier};

use crate::state::Ctx;

/// The tunable limits. [`Limits::default`] is what the daemon runs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The most one call can buy.
    pub per_call: Duration,
    /// The longest one continuous window can last.
    pub continuous: Duration,
    /// The gap enforced after a window that reached `continuous`.
    pub cooldown: Duration,
    /// Calls a caller may make back to back.
    pub burst: u32,
    /// Calls per second a caller earns back.
    pub rate_per_s: u32,
}

impl Default for Limits {
    /// 600 ms per call: Rime's longest UI motion (a window opening on the
    /// `rimeArrive` spring, Hyprland speed 4.8) is under half a second.
    /// 1.5 s continuous then 1 s cooldown: a looping caller gets at most 60 %
    /// of the time boosted, ordinary use (one animation per gesture) never
    /// reaches the ceiling. 8 calls back to back, 20 a second: a Hyprland
    /// workspace scrub emits a few events per frame at most and the watcher
    /// coalesces them before calling.
    fn default() -> Self {
        Limits {
            per_call: Duration::from_millis(600),
            continuous: Duration::from_millis(1500),
            cooldown: Duration::from_millis(1000),
            burst: 8,
            rate_per_s: 20,
        }
    }
}

/// What a request did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    /// A new window: write the boost now; it ends at `until`.
    Start { id: u64, until: Instant },
    /// The running window now ends later, at `until`. Nothing to write.
    Extend { until: Instant },
    /// The running window already lasts past the ask. Nothing to write.
    Covered { until: Instant },
    /// The last holder gave its share back: restore now.
    Release,
    /// Nothing to do (`Boost(0)` from a caller holding nothing).
    Idle,
    /// Refused: inside the cooldown after a window that reached the ceiling.
    Cooldown,
    /// Refused: this caller is over its call rate.
    RateLimited,
}

/// What the end-of-window timer should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Poll {
    /// That window is gone (released, cancelled or replaced): do nothing.
    Gone,
    /// Not yet: it now ends at this instant.
    Wait(Instant),
    /// Over: restore now.
    Expired,
}

#[derive(Debug)]
struct Window {
    id: u64,
    start: Instant,
    until: Instant,
    holders: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

/// The pure policy. Holds no hardware state.
#[derive(Debug)]
pub struct Engine {
    limits: Limits,
    window: Option<Window>,
    cooldown_until: Option<Instant>,
    buckets: HashMap<String, Bucket>,
    next_id: u64,
}

/// Callers tracked for rate limiting before idle ones are dropped. The caller
/// policy admits only the active session's own processes, so this is a bound on
/// memory, not a limit anyone reaches.
const MAX_BUCKETS: usize = 256;

impl Engine {
    pub fn new(limits: Limits) -> Engine {
        Engine {
            limits,
            window: None,
            cooldown_until: None,
            buckets: HashMap::new(),
            next_id: 1,
        }
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// The end of the running window, if one is running.
    #[cfg(test)]
    pub fn until(&self) -> Option<Instant> {
        self.window.as_ref().map(|w| w.until)
    }

    /// One `Boost(ms)` from `sender` at `now`.
    pub fn request(&mut self, now: Instant, sender: &str, ms: u32) -> Grant {
        if !self.take_token(now, sender) {
            return Grant::RateLimited;
        }
        // A window whose end passed while its timer had not yet run is over.
        if self.window.as_ref().is_some_and(|w| w.until <= now) {
            self.end_window();
        }
        if ms == 0 {
            let emptied = match &mut self.window {
                Some(w) => w.holders.remove(sender) && w.holders.is_empty(),
                None => false,
            };
            if emptied {
                self.window = None;
                return Grant::Release;
            }
            return Grant::Idle;
        }
        let ask = Duration::from_millis(u64::from(ms)).min(self.limits.per_call);
        match &mut self.window {
            None => {
                if self.cooldown_until.is_some_and(|c| now < c) {
                    return Grant::Cooldown;
                }
                self.cooldown_until = None;
                let id = self.next_id;
                self.next_id += 1;
                let until = now + ask.min(self.limits.continuous);
                self.window = Some(Window {
                    id,
                    start: now,
                    until,
                    holders: BTreeSet::from([sender.to_string()]),
                });
                Grant::Start { id, until }
            }
            Some(w) => {
                w.holders.insert(sender.to_string());
                let ceiling = w.start + self.limits.continuous;
                let want = (now + ask).min(ceiling);
                if want > w.until {
                    w.until = want;
                    Grant::Extend { until: want }
                } else {
                    Grant::Covered { until: w.until }
                }
            }
        }
    }

    /// The timer for window `id` woke at `now`.
    pub fn poll(&mut self, id: u64, now: Instant) -> Poll {
        match &self.window {
            Some(w) if w.id == id => {
                if now < w.until {
                    Poll::Wait(w.until)
                } else {
                    self.end_window();
                    Poll::Expired
                }
            }
            _ => Poll::Gone,
        }
    }

    /// `sender` left the bus. `true` when that ended the window (restore now).
    pub fn disconnect(&mut self, sender: &str) -> bool {
        self.buckets.remove(sender);
        let emptied = match &mut self.window {
            Some(w) => w.holders.remove(sender) && w.holders.is_empty(),
            None => false,
        };
        if emptied {
            self.window = None;
        }
        emptied
    }

    /// The tier changed underneath: the window is gone and its restore is the
    /// tier's own plan, already written. No cooldown: nothing was overused.
    pub fn cancel(&mut self) {
        self.window = None;
    }

    fn end_window(&mut self) {
        if let Some(w) = self.window.take() {
            if w.until.duration_since(w.start) >= self.limits.continuous {
                self.cooldown_until = Some(w.until + self.limits.cooldown);
            }
        }
    }

    fn take_token(&mut self, now: Instant, sender: &str) -> bool {
        let (burst, rate) = (
            f64::from(self.limits.burst),
            f64::from(self.limits.rate_per_s),
        );
        if self.buckets.len() >= MAX_BUCKETS && !self.buckets.contains_key(sender) {
            // Forget callers whose buckets have refilled: they are
            // indistinguishable from callers never seen.
            self.buckets.retain(|_, b| {
                b.tokens + now.saturating_duration_since(b.at).as_secs_f64() * rate < burst
            });
        }
        let b = self.buckets.entry(sender.to_string()).or_insert(Bucket {
            tokens: burst,
            at: now,
        });
        let refill = now.saturating_duration_since(b.at).as_secs_f64() * rate;
        b.tokens = (b.tokens + refill).min(burst);
        b.at = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// The engine plus what is on the hardware right now.
#[derive(Debug)]
pub struct BoostCell {
    pub engine: Engine,
    /// The tier's own actions that end the boost on the hardware. `Some`
    /// exactly while a boost is written.
    restore: Option<(Tier, Vec<Action>)>,
}

impl BoostCell {
    pub fn new(limits: Limits) -> BoostCell {
        BoostCell {
            engine: Engine::new(limits),
            restore: None,
        }
    }

    /// Whether a boost is on the hardware.
    pub fn active(&self) -> bool {
        self.restore.is_some()
    }

    /// The tier is being rewritten: forget the window without writing its
    /// restore, because the tier's own plan is about to land.
    pub fn cancel(&mut self) {
        self.engine.cancel();
        self.restore = None;
    }
}

/// Why a boost was refused outright (the D-Bus layer maps these to errors).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoostRefusal {
    RateLimited,
}

/// `tokio`'s clock, as a `std` instant. Under `tokio::time::pause` this is the
/// paused clock, which is what lets the timer tests run without sleeping.
fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

impl Ctx {
    /// `Ui.Boost(ms)` from `sender`, already admitted by the caller policy.
    /// Answers the milliseconds the boost now lasts (0 = nothing boosted).
    pub async fn ui_boost(self: &Arc<Self>, sender: &str, ms: u32) -> Result<u32, BoostRefusal> {
        let at = now();
        let mut cell = self.ui_boost.lock().await;
        let tier = self.state.lock().await.tier;
        let plan = self.profile().ui_boost_plan(tier);
        if ms > 0 && plan.is_none() && !cell.active() {
            // Nothing to raise in this tier (performance, power-saver, a tier
            // whose EPP is already high). Still counts against the caller's
            // rate, so a refused loop is not free either.
            return match cell.engine.request(at, sender, 0) {
                Grant::RateLimited => Err(BoostRefusal::RateLimited),
                _ => Ok(0),
            };
        }
        match cell.engine.request(at, sender, ms) {
            Grant::Start { id, until } => {
                let Some(plan) = plan else {
                    cell.engine.cancel();
                    return Ok(0);
                };
                let mut landed = false;
                for a in &plan.boost {
                    match self.writer.apply(a) {
                        Ok(o) => landed |= o.landed(),
                        Err(e) => eprintln!("rimed: ui boost: {e:#}"),
                    }
                }
                if !landed {
                    // The driver refused (EPP pinned, attribute absent): no
                    // boost is on the hardware, so there is nothing to time.
                    cell.engine.cancel();
                    return Ok(0);
                }
                cell.restore = Some((tier, plan.restore));
                self.arm_boost_timer(id, until);
                Ok(ms_left(at, until))
            }
            Grant::Extend { until } | Grant::Covered { until } => Ok(ms_left(at, until)),
            Grant::Release => {
                self.restore_boost(&mut cell);
                Ok(0)
            }
            Grant::Idle | Grant::Cooldown => Ok(0),
            Grant::RateLimited => Err(BoostRefusal::RateLimited),
        }
    }

    /// A bus peer went away: drop its share of the window.
    pub async fn ui_boost_peer_gone(self: &Arc<Self>, sender: &str) {
        let mut cell = self.ui_boost.lock().await;
        if cell.engine.disconnect(sender) {
            self.restore_boost(&mut cell);
        }
    }

    /// Whether a boost is on the hardware right now.
    pub async fn ui_boost_active(&self) -> bool {
        self.ui_boost.lock().await.active()
    }

    /// Write the tier's own actions back. Called with the cell locked, so no
    /// tier change can interleave.
    fn restore_boost(&self, cell: &mut BoostCell) {
        if let Some((tier, actions)) = cell.restore.take() {
            for a in &actions {
                if let Err(e) = self.writer.apply(a) {
                    eprintln!("rimed: ui boost: restoring tier {tier}: {e:#}");
                }
            }
        }
    }

    fn arm_boost_timer(self: &Arc<Self>, id: u64, until: Instant) {
        let ctx = Arc::clone(self);
        tokio::spawn(async move {
            let mut until = until;
            loop {
                tokio::time::sleep_until(tokio::time::Instant::from_std(until)).await;
                let mut cell = ctx.ui_boost.lock().await;
                match cell.engine.poll(id, now()) {
                    Poll::Gone => return,
                    Poll::Wait(later) => until = later,
                    Poll::Expired => {
                        ctx.restore_boost(&mut cell);
                        return;
                    }
                }
            }
        });
    }
}

fn ms_left(now: Instant, until: Instant) -> u32 {
    u32::try_from(until.saturating_duration_since(now).as_millis()).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn engine() -> (Engine, Instant) {
        (Engine::new(Limits::default()), Instant::now())
    }

    #[test]
    fn one_call_is_capped_at_600_ms() {
        let (mut e, t) = engine();
        assert_eq!(
            e.request(t, ":1.1", 5_000),
            Grant::Start {
                id: 1,
                until: t + ms(600)
            }
        );
    }

    #[test]
    fn calls_coalesce_into_one_window_and_never_stack() {
        let (mut e, t) = engine();
        assert!(matches!(e.request(t, ":1.1", 400), Grant::Start { .. }));
        // Inside the window, a shorter ask is already covered.
        assert_eq!(
            e.request(t + ms(100), ":1.1", 200),
            Grant::Covered { until: t + ms(400) }
        );
        // A longer one moves the END, it does not add 400 + 600.
        assert_eq!(
            e.request(t + ms(100), ":1.2", 600),
            Grant::Extend { until: t + ms(700) }
        );
        assert_eq!(e.until(), Some(t + ms(700)));
        // Ten more callers at once still end at now + 600, not later.
        for i in 0..10 {
            e.request(t + ms(150), &format!(":1.{}", 10 + i), 600);
        }
        assert_eq!(e.until(), Some(t + ms(750)));
    }

    #[test]
    fn a_window_ends_at_the_ceiling_and_a_cooldown_follows() {
        let (mut e, t) = engine();
        e.request(t, ":1.1", 600);
        let mut at = t;
        // Keep asking every 100 ms (inside the per-caller rate).
        for _ in 0..14 {
            at += ms(100);
            e.request(at, ":1.1", 600);
        }
        assert_eq!(e.until(), Some(t + ms(1_500)), "held at the 1.5 s ceiling");
        // The window that hit the ceiling ended at t+1500: cooldown until t+2500.
        assert_eq!(e.request(t + ms(1_500), ":1.1", 600), Grant::Cooldown);
        assert_eq!(e.until(), None);
        assert_eq!(e.request(t + ms(2_400), ":1.2", 300), Grant::Cooldown);
        assert!(matches!(
            e.request(t + ms(2_500), ":1.2", 300),
            Grant::Start { .. }
        ));
    }

    #[test]
    fn the_ceiling_bounds_the_end_even_mid_window() {
        let (mut e, t) = engine();
        e.request(t, ":1.1", 600);
        e.request(t + ms(500), ":1.1", 600);
        e.request(t + ms(1_000), ":1.1", 600);
        assert_eq!(e.until(), Some(t + ms(1_500)));
        assert_eq!(
            e.request(t + ms(1_400), ":1.1", 600),
            Grant::Covered {
                until: t + ms(1_500)
            }
        );
    }

    #[test]
    fn a_short_window_earns_no_cooldown() {
        let (mut e, t) = engine();
        let Grant::Start { id, .. } = e.request(t, ":1.1", 300) else {
            panic!()
        };
        assert_eq!(e.poll(id, t + ms(300)), Poll::Expired);
        assert!(matches!(
            e.request(t + ms(301), ":1.1", 300),
            Grant::Start { .. }
        ));
    }

    #[test]
    fn the_timer_waits_for_an_extended_end_and_ignores_a_replaced_window() {
        let (mut e, t) = engine();
        let Grant::Start { id, .. } = e.request(t, ":1.1", 300) else {
            panic!()
        };
        e.request(t + ms(200), ":1.1", 500);
        assert_eq!(e.poll(id, t + ms(300)), Poll::Wait(t + ms(700)));
        assert_eq!(e.poll(id, t + ms(700)), Poll::Expired);
        assert_eq!(e.poll(id, t + ms(701)), Poll::Gone, "fires once");
        let Grant::Start { id: id2, .. } = e.request(t + ms(800), ":1.1", 300) else {
            panic!()
        };
        assert_ne!(id, id2);
        assert_eq!(
            e.poll(id, t + ms(900)),
            Poll::Gone,
            "a stale timer never ends a new window"
        );
    }

    #[test]
    fn a_caller_over_its_rate_is_refused() {
        let (mut e, t) = engine();
        for _ in 0..8 {
            assert_ne!(e.request(t, ":1.1", 100), Grant::RateLimited);
        }
        assert_eq!(e.request(t, ":1.1", 100), Grant::RateLimited);
        // Another caller is unaffected; the first earns calls back over time.
        assert_ne!(e.request(t, ":1.2", 100), Grant::RateLimited);
        assert_ne!(e.request(t + ms(50), ":1.1", 100), Grant::RateLimited);
    }

    #[test]
    fn zero_gives_the_share_back_and_the_last_share_releases() {
        let (mut e, t) = engine();
        e.request(t, ":1.1", 500);
        e.request(t, ":1.2", 500);
        assert_eq!(
            e.request(t + ms(10), ":1.1", 0),
            Grant::Idle,
            ":1.2 still holds"
        );
        assert_eq!(e.request(t + ms(20), ":1.2", 0), Grant::Release);
        assert_eq!(e.until(), None);
        assert_eq!(e.request(t + ms(30), ":1.3", 0), Grant::Idle);
    }

    #[test]
    fn a_disconnect_releases_only_when_nobody_else_holds() {
        let (mut e, t) = engine();
        e.request(t, ":1.1", 500);
        e.request(t, ":1.2", 500);
        assert!(!e.disconnect(":1.1"));
        assert!(e.until().is_some());
        assert!(e.disconnect(":1.2"));
        assert_eq!(e.until(), None);
        assert!(!e.disconnect(":1.9"), "a stranger leaving changes nothing");
    }

    #[test]
    fn cancel_drops_the_window_without_a_cooldown() {
        let (mut e, t) = engine();
        let Grant::Start { id, .. } = e.request(t, ":1.1", 600) else {
            panic!()
        };
        e.request(t + ms(500), ":1.1", 600);
        e.request(t + ms(1_000), ":1.1", 600);
        e.cancel();
        assert_eq!(e.poll(id, t + ms(1_500)), Poll::Gone);
        assert!(matches!(
            e.request(t + ms(1_100), ":1.1", 300),
            Grant::Start { .. }
        ));
    }
}

/// The daemon half: what reaches the writer, on tokio's paused clock.
#[cfg(test)]
pub(crate) mod ctx_tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use rimed_core::gpu::MockNvidiaSmi;
    use rimed_core::syswriter::MockWriter;
    use rimed_core::tier::{Action, Tier};
    use rimed_core::{ProfileSet, Selection};

    use crate::state::{Ctx, State};

    pub(crate) const PROFILE: &str = r#"
        id = "test-boost"
        kind = "device"
        [defaults]
        ac = "balanced"
        battery = "balanced"
        [tiers.performance]
        governor = "performance"
        epp = "performance"
        [tiers.balanced]
        governor = "powersave"
        epp = "balance_power"
        [tiers.power-saver]
        governor = "powersave"
        epp = "power"
        [ui_boost]
        enabled = true
        epp = "balance_performance"
        tiers = ["balanced"]
    "#;

    fn scratch(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "rimed-boost-ctx-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&root).ok();
        root
    }

    pub(crate) fn build(tag: &str, profile: &str) -> (Arc<Ctx>, Arc<MockWriter>) {
        let root = scratch(tag);
        std::fs::create_dir_all(root.join("profiles")).unwrap();
        std::fs::write(root.join("profiles/test-boost.toml"), profile).unwrap();
        let set = ProfileSet::load(Some(&root.join("profiles"))).unwrap();
        let selection = Selection {
            generic: "test-boost".into(),
            class: None,
            device: Some("test-boost".into()),
            active: "test-boost".into(),
        };
        let fingerprint = rimed_core::Fingerprint::detect_from(&root, &root);
        let writer = Arc::new(MockWriter::new());
        let ctx = Ctx::new(
            set,
            selection,
            fingerprint,
            writer.clone(),
            false,
            State {
                tier: Tier::Balanced,
                auto_switch: true,
                on_ac: false,
                travel_mode: false,
                charge_start: 0,
                charge_stop: 100,
            },
            &root,
            root.join("no-irqs"),
            root.join("proc"),
            Arc::new(MockNvidiaSmi::default()),
            Path::new(&root).join("state/mode"),
        );
        (ctx, writer)
    }

    fn epp(e: &str) -> Action {
        Action::Epp(e.into())
    }

    /// Let spawned timers run up to the paused clock's `now`.
    async fn settle() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_boost_writes_once_and_restores_the_tiers_exact_epp() {
        let (ctx, w) = build("restore", PROFILE);
        assert_eq!(ctx.ui_boost(":1.1", 400).await, Ok(400));
        // Coalesced calls write nothing more.
        assert_eq!(ctx.ui_boost(":1.2", 200).await, Ok(400));
        assert_eq!(w.recorded(), vec![epp("balance_performance")]);
        assert!(ctx.ui_boost_active().await);

        tokio::time::advance(std::time::Duration::from_millis(399)).await;
        settle().await;
        assert_eq!(w.recorded().len(), 1, "not before the window ends");

        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        settle().await;
        assert_eq!(
            w.recorded(),
            vec![epp("balance_performance"), epp("balance_power")]
        );
        assert!(!ctx.ui_boost_active().await);
    }

    #[tokio::test(start_paused = true)]
    async fn an_extended_window_restores_at_its_new_end_only() {
        let (ctx, w) = build("extend", PROFILE);
        ctx.ui_boost(":1.1", 300).await.unwrap();
        tokio::time::advance(std::time::Duration::from_millis(200)).await;
        assert_eq!(ctx.ui_boost(":1.1", 600).await, Ok(600));
        tokio::time::advance(std::time::Duration::from_millis(150)).await;
        settle().await;
        assert_eq!(w.recorded().len(), 1, "the first timer saw the extension");
        tokio::time::advance(std::time::Duration::from_millis(450)).await;
        settle().await;
        assert_eq!(w.recorded().last(), Some(&epp("balance_power")));
        assert_eq!(w.recorded().len(), 2, "restored exactly once");
    }

    #[tokio::test(start_paused = true)]
    async fn the_last_holder_leaving_restores_now() {
        let (ctx, w) = build("disconnect", PROFILE);
        ctx.ui_boost(":1.1", 600).await.unwrap();
        ctx.ui_boost(":1.2", 600).await.unwrap();
        ctx.ui_boost_peer_gone(":1.1").await;
        assert_eq!(w.recorded().len(), 1, ":1.2 still holds the window");
        ctx.ui_boost_peer_gone(":1.2").await;
        assert_eq!(
            w.recorded(),
            vec![epp("balance_performance"), epp("balance_power")]
        );
        // The window's own timer later finds nothing to do.
        tokio::time::advance(std::time::Duration::from_millis(700)).await;
        settle().await;
        assert_eq!(w.recorded().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn boost_zero_releases_this_callers_share() {
        let (ctx, w) = build("zero", PROFILE);
        ctx.ui_boost(":1.1", 600).await.unwrap();
        assert_eq!(ctx.ui_boost(":1.1", 0).await, Ok(0));
        assert_eq!(
            w.recorded(),
            vec![epp("balance_performance"), epp("balance_power")]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_tier_change_cancels_the_boost_and_the_tier_writes_last() {
        let (ctx, w) = build("tier", PROFILE);
        ctx.ui_boost(":1.1", 600).await.unwrap();
        ctx.apply_tier(Tier::PowerSaver).await.unwrap();
        let after_tier = w.recorded();
        assert_eq!(after_tier.last(), Some(&epp("power")));
        tokio::time::advance(std::time::Duration::from_millis(700)).await;
        settle().await;
        assert_eq!(
            w.recorded(),
            after_tier,
            "no stale restore after the tier's writes"
        );
        assert!(!ctx.ui_boost_active().await);
    }

    #[tokio::test(start_paused = true)]
    async fn a_tier_that_needs_no_boost_writes_nothing() {
        let (ctx, w) = build("perf", PROFILE);
        ctx.apply_tier(Tier::Performance).await.unwrap();
        w.clear();
        assert_eq!(ctx.ui_boost(":1.1", 600).await, Ok(0));
        ctx.apply_tier(Tier::PowerSaver).await.unwrap();
        w.clear();
        assert_eq!(ctx.ui_boost(":1.1", 600).await, Ok(0));
        assert!(w.recorded().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_looping_caller_is_rate_limited() {
        let (ctx, _w) = build("rate", PROFILE);
        for _ in 0..8 {
            assert!(ctx.ui_boost(":1.1", 100).await.is_ok());
        }
        assert_eq!(
            ctx.ui_boost(":1.1", 100).await,
            Err(super::BoostRefusal::RateLimited)
        );
    }
}
