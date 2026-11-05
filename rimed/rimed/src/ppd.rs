//! power-profiles-daemon's D-Bus API, served by rimed (docs/nixos/desktops.md §7): rimed is the ONE writer of the
//! CPU's power policy.
//!
//! WHY: Rime used to run power-profiles-daemon (PPD) next to rimed. Both write the same files (`scaling_governor`,
//! `energy_performance_preference`, `cpufreq/boost`, `platform_profile`), and the last writer wins: PPD at boot, rimed
//! on its next AC transition. Measured read-only on an MSI laptop on AC: rimed reported tier `performance` while sysfs
//! held `powersave` / `balance_performance`, PPD's `balanced`. A UI boost that restores "the tier's exact values" means
//! nothing while a second daemon rewrites them.
//!
//! So no power-profiles-daemon runs: the bootc image masks `power-profiles-daemon.service` (Containerfile.base) and
//! NixOS ships none (nix/modules/desktops.nix asserts it). rimed owns PPD's two bus names,
//! `org.freedesktop.UPower.PowerProfiles` and `net.hadess.PowerProfiles` (`RIMED_POWER_PROFILES=0` opts out). PPD
//! starts its drivers only after it has its name and quits when it cannot, so a PPD started by hand anyway writes
//! nothing. GNOME's quick settings, KDE's powerdevil, COSMIC's battery applet, `powerprofilesctl` and apps taking a
//! hold all drive rimed's tiers, whose ids are PPD's: `performance`, `balanced`, `power-saver`.
//!
//! The mapping:
//! - `ActiveProfile` is `Power.Tier` (a hold wins). A write is exactly `Power.SetTier`: it leaves auto-switch alone,
//!   releases every hold (`ProfileReleased(cookie)` for each) as a user's choice does in power-profiles-daemon, and
//!   with auto-switch on it is the user's tier for the power source the machine is on (choice.rs). During game mode a
//!   write sets the tier game mode returns to, and `ActiveProfile` stays the game's tier (it is emitted again so a
//!   client that optimistically showed its own choice is corrected).
//! - `HoldProfile` / `ReleaseProfile` / `ActiveProfileHolds`: a hold belongs to the caller's unique bus name and goes
//!   when that name leaves the bus. Any `power-saver` hold beats every `performance` hold, game mode's included: game
//!   mode is listed as a `performance` hold by `org.rimeos.GameMode`, and a `power-saver` hold during it runs the
//!   machine at `power-saver` while every other game mode lever stays (and `Mode.Active` stays `gaming`). When the
//!   last hold goes, the tier returns to the one in force before the first hold (or, if AC changed meanwhile with
//!   auto-switch on, to the tier auto-switch wants now). A release that leaves the holds asking for the same tier
//!   writes nothing. Holds never record a choice.
//! - `BatteryAware` is `Power.AutoSwitch`.
//! - `Profiles` names `rimed` as the driver of every profile, never `placeholder` (GNOME hides a placeholder).
//! - Actions: rimed has none (`Actions` and `ActionsInfo` are empty, `SetActionEnabled` is NotSupported). PPD 0.30's
//!   `trickle_charge` touches only peripheral batteries and its amdgpu `panel_power`/`dpm` actions are off by default;
//!   the CPU-side parity (`cpufreq/boost`, the min-frequency floor) is in rimed's tiers (`cpu_boost`, `cpu_min_freq`).
//!
//! Property writes are authorised by polkit like the methods are, with PPD's own action ids (bootc: the policy of the
//! power-profiles-daemon package; NixOS: `org.rimeos.rimed.power-profiles.policy`). zbus hands a property setter no
//! caller, so the two objects get their own `org.freedesktop.DBus.Properties` ([`Props`]) in place of zbus's: its
//! `Set` sees the caller, asks polkit, then writes.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use rimed_core::tier::Tier;
use zbus::message::Header;
use zbus::names::InterfaceName;
use zbus::object_server::Interface;
use zbus::fdo::RequestNameFlags;
use zbus::{interface, Connection, ObjectServer, SignalContext};
use zvariant::{OwnedValue, Value};

use crate::polkit::authorize;
use crate::state::Ctx;

pub const NAME: &str = "org.freedesktop.UPower.PowerProfiles";
pub const PATH: &str = "/org/freedesktop/UPower/PowerProfiles";
pub const LEGACY_NAME: &str = "net.hadess.PowerProfiles";
pub const LEGACY_PATH: &str = "/net/hadess/PowerProfiles";

pub const ACTION_SWITCH: &str = "org.freedesktop.UPower.PowerProfiles.switch-profile";
pub const ACTION_HOLD: &str = "org.freedesktop.UPower.PowerProfiles.hold-profile";
pub const ACTION_BATTERY_AWARE: &str = "org.freedesktop.UPower.PowerProfiles.configure-battery-aware";
/// Shipped in the policy for completeness (power-profiles-daemon has it); rimed has no action to configure.
#[cfg_attr(not(test), allow(dead_code))]
pub const ACTION_CONFIGURE: &str = "org.freedesktop.UPower.PowerProfiles.configure-action";

/// The application id game mode's hold is listed under.
pub const GAME_APP_ID: &str = "org.rimeos.GameMode";
/// The power-profiles-daemon API version this implements (0.30: `BatteryAware`, `ActionsInfo`).
pub const VERSION: &str = "0.30";

// ── holds ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Hold {
    pub cookie: u32,
    pub profile: Tier,
    pub reason: String,
    pub app_id: String,
    /// The holder's unique bus name; the hold goes when it leaves the bus.
    pub owner: String,
}

#[derive(Debug, Default)]
pub struct Holds {
    next: u32,
    pub list: Vec<Hold>,
    /// The tier before the first hold, where the machine returns when the last one goes.
    pub return_tier: Option<Tier>,
}

impl Holds {
    /// The tier the holds ask for: any `power-saver` hold wins over every `performance` hold.
    pub fn effective(&self) -> Option<Tier> {
        if self.list.iter().any(|h| h.profile == Tier::PowerSaver) {
            Some(Tier::PowerSaver)
        } else if self.list.is_empty() {
            None
        } else {
            Some(Tier::Performance)
        }
    }

    /// Take out the holds `pick` selects. Answers the released cookies and the tier to move to: what the remaining
    /// holds ask for when that changed, or the tier before the first hold when the last one went. `None` when nothing
    /// moves (nothing released, or the holds still ask for the same tier).
    pub fn release(&mut self, pick: impl Fn(&Hold) -> bool) -> (Vec<u32>, Option<Tier>) {
        let before = self.effective();
        let (gone, kept): (Vec<Hold>, Vec<Hold>) = std::mem::take(&mut self.list).into_iter().partition(|x| pick(x));
        self.list = kept;
        if gone.is_empty() {
            return (Vec::new(), None);
        }
        let target = match self.effective() {
            Some(t) if Some(t) == before => None,
            Some(t) => Some(t),
            None => self.return_tier.take(),
        };
        (gone.iter().map(|x| x.cookie).collect(), target)
    }

    /// A user's choice: every hold goes and there is nothing to return to (the choice stands).
    pub fn clear(&mut self) -> Vec<u32> {
        self.return_tier = None;
        self.list.drain(..).map(|x| x.cookie).collect()
    }

    /// Add a hold. `base` is the tier in force now without holds; the first hold keeps it as the tier to return to.
    pub fn take(&mut self, profile: Tier, reason: &str, app_id: &str, owner: &str, base: Tier) -> u32 {
        if self.list.is_empty() {
            self.return_tier = Some(base);
        }
        self.add(profile, reason, app_id, owner)
    }

    fn add(&mut self, profile: Tier, reason: &str, app_id: &str, owner: &str) -> u32 {
        loop {
            self.next = self.next.wrapping_add(1);
            if self.next != 0 && !self.list.iter().any(|h| h.cookie == self.next) {
                break;
            }
        }
        self.list.push(Hold {
            cookie: self.next,
            profile,
            reason: reason.to_string(),
            app_id: app_id.to_string(),
            owner: owner.to_string(),
        });
        self.next
    }
}

impl Ctx {
    /// The tier the machine is on when nothing overrides it: during game mode, the tier game mode returns to.
    async fn base_tier(&self) -> Tier {
        if let Some(s) = self.game.lock().await.as_ref() {
            return s.prior_tier;
        }
        self.state.lock().await.tier
    }

    /// Move the base tier to `t`; `held` is what the holds ask for now. Outside game mode `t` is the tier. During game
    /// mode `t` is only the tier game mode returns to, and the machine runs [`live_in_game`]: the game's tier, unless a
    /// `power-saver` hold beats it. Writes nothing when the machine is already on the tier it should be on.
    async fn set_base_tier(self: &Arc<Self>, t: Tier, held: Option<Tier>) -> Result<()> {
        let live = {
            let mut g = self.game.lock().await;
            match g.as_mut() {
                Some(s) => {
                    s.prior_tier = t;
                    live_in_game(s.tier, held)
                }
                None => t,
            }
        };
        if self.state.lock().await.tier == live {
            return Ok(());
        }
        self.apply_tier(live).await.map(|_| ())
    }

    /// `HoldProfile`: add the hold and move to the tier the holds now ask for.
    pub async fn ppd_hold(self: &Arc<Self>, profile: Tier, reason: &str, app_id: &str, owner: &str) -> Result<u32> {
        let mut h = self.ppd.lock().await;
        let base = self.base_tier().await;
        let cookie = h.take(profile, reason, app_id, owner, base);
        let held = h.effective();
        if let Err(e) = self.set_base_tier(held.unwrap_or(profile), held).await {
            h.list.retain(|x| x.cookie != cookie);
            if h.list.is_empty() {
                h.return_tier = None;
            }
            return Err(e);
        }
        Ok(cookie)
    }

    /// Release the holds `pick` selects and move to what the rest ask for, or back to the tier before the first hold.
    /// Returns the released cookies.
    pub async fn ppd_release(self: &Arc<Self>, pick: impl Fn(&Hold) -> bool) -> Result<Vec<u32>> {
        let mut h = self.ppd.lock().await;
        let (gone, target) = h.release(pick);
        if let Some(t) = target {
            self.set_base_tier(t, h.effective()).await?;
        }
        Ok(gone)
    }

    /// A user's explicit choice made elsewhere (`Power.SetTier`): every hold goes and nothing else moves.
    pub async fn ppd_forget(&self) -> Vec<u32> {
        self.ppd.lock().await.clear()
    }

    /// The `ActiveProfile` write: every hold goes, then the tier is `t` (during game mode: the tier it returns to).
    pub async fn ppd_choose(self: &Arc<Self>, t: Tier) -> Result<Vec<u32>> {
        let mut h = self.ppd.lock().await;
        let released = h.clear();
        self.set_base_tier(t, None).await?;
        Ok(released)
    }

    /// The tier game mode runs at right now when nothing else beats it, if game mode is on, and whether a
    /// `power-saver` hold beats it (`Mode.Active` stays `gaming` then).
    pub async fn game_tier_beaten(&self) -> Option<(Tier, bool)> {
        let game = self.game.lock().await.as_ref().map(|s| s.tier)?;
        let beaten = live_in_game(game, self.ppd.lock().await.effective()) != game;
        Some((game, beaten))
    }

    /// Auto-switch wants `t` (AC changed, or auto-switch was turned on). While holds exist they keep the tier and `t`
    /// becomes the tier to return to; returns true when absorbed that way.
    pub async fn ppd_absorb(&self, t: Tier) -> bool {
        let mut h = self.ppd.lock().await;
        if h.list.is_empty() {
            return false;
        }
        h.return_tier = Some(t);
        true
    }

    /// `ActiveProfileHolds`: game mode's own hold first, then the clients'.
    pub async fn ppd_holds_view(&self) -> Vec<HashMap<String, OwnedValue>> {
        let mut out = Vec::new();
        if self.game_active().await {
            out.push(hold_entry(GAME_APP_ID, Tier::Performance, "Gaming Mode"));
        }
        for h in &self.ppd.lock().await.list {
            out.push(hold_entry(&h.app_id, h.profile, &h.reason));
        }
        out
    }

    /// Turn auto-switch on or off (`Power.SetAutoSwitch`, `BatteryAware`). Turning it on moves to the tier it wants
    /// now, unless holds keep the tier (then that tier is where they return).
    pub async fn set_auto_switch(self: &Arc<Self>, on: bool) -> Result<()> {
        self.state.lock().await.auto_switch = on;
        if on {
            let t = self.auto_target().await;
            if !self.ppd_absorb(t).await {
                self.apply_tier(t).await?;
            }
        }
        Ok(())
    }
}

/// The tier the machine runs during game mode, whose own tier is `game`, while the holds ask for `held`: any
/// `power-saver` hold beats game mode's tier as it beats every `performance` hold (§7); a `performance` hold changes
/// nothing.
pub fn live_in_game(game: Tier, held: Option<Tier>) -> Tier {
    if held == Some(Tier::PowerSaver) {
        Tier::PowerSaver
    } else {
        game
    }
}

fn dict(pairs: &[(&str, Value<'_>)]) -> HashMap<String, OwnedValue> {
    let mut m = HashMap::new();
    for (k, v) in pairs {
        if let Ok(o) = v.try_to_owned() {
            m.insert((*k).to_string(), o);
        }
    }
    m
}

fn hold_entry(app_id: &str, profile: Tier, reason: &str) -> HashMap<String, OwnedValue> {
    dict(&[
        ("ApplicationId", Value::from(app_id)),
        ("Profile", Value::from(profile.as_str())),
        ("Reason", Value::from(reason)),
    ])
}

/// The cpufreq driver as power-profiles-daemon names it.
pub fn cpu_driver(sys_root: &Path) -> String {
    let raw = std::fs::read_to_string(sys_root.join("devices/system/cpu/cpu0/cpufreq/scaling_driver")).unwrap_or_default();
    let d = raw.trim();
    if d.starts_with("intel_") {
        "intel_pstate".into()
    } else if d.starts_with("amd-pstate") || d.starts_with("amd_pstate") {
        "amd_pstate".into()
    } else {
        "rimed".into()
    }
}

/// The platform driver as power-profiles-daemon names it.
pub fn platform_driver(sys_root: &Path) -> String {
    if sys_root.join("firmware/acpi/platform_profile").exists() {
        "platform_profile".into()
    } else {
        "rimed".into()
    }
}

/// `Profiles`: one entry per tier, lowest first, as power-profiles-daemon lists them. Clients turn that round
/// themselves (`powerprofilesctl list`, GNOME's menu) to show `performance` on top.
pub fn profiles(sys_root: &Path) -> Vec<HashMap<String, OwnedValue>> {
    let cpu = cpu_driver(sys_root);
    let platform = platform_driver(sys_root);
    Tier::ALL
        .iter()
        .rev()
        .map(|t| {
            dict(&[
                ("Profile", Value::from(t.as_str())),
                ("Driver", Value::from("rimed")),
                ("PlatformDriver", Value::from(platform.as_str())),
                ("CpuDriver", Value::from(cpu.as_str())),
            ])
        })
        .collect()
}

/// `PerformanceDegraded`: why `performance` is not delivering, as power-profiles-daemon reports it. Only in
/// `performance`. `lap-detected`: thinkpad_acpi's lap sensor. `high-operating-temperature`: intel_pstate's turbo
/// switched off underneath rimed (rimed never turns turbo off in `performance`, so the firmware did).
pub fn degraded(sys_root: &Path, tier: Tier) -> String {
    if tier != Tier::Performance {
        return String::new();
    }
    let one = |p: &str| std::fs::read_to_string(sys_root.join(p)).map(|s| s.trim() == "1").unwrap_or(false);
    let mut why = Vec::new();
    if one("bus/platform/devices/thinkpad_acpi/dytc_lapmode") {
        why.push("lap-detected");
    }
    if one("devices/system/cpu/intel_pstate/no_turbo") {
        why.push("high-operating-temperature");
    }
    why.join(",")
}

impl Ctx {
    pub async fn ppd_degraded(&self) -> String {
        let tier = self.state.lock().await.tier;
        degraded(&self.sys_root, tier)
    }
}

// ── the two interfaces ───────────────────────────────────────────────────────

fn written_through_properties() -> zbus::Error {
    zbus::Error::FDO(Box::new(zbus::fdo::Error::Failed("written through org.freedesktop.DBus.Properties.Set".into())))
}

/// `org.freedesktop.UPower.PowerProfiles` at [`PATH`].
pub struct PowerProfiles {
    pub ctx: Arc<Ctx>,
}

#[interface(name = "org.freedesktop.UPower.PowerProfiles")]
impl PowerProfiles {
    #[zbus(property)]
    async fn active_profile(&self) -> String {
        self.ctx.state.lock().await.tier.as_str().to_string()
    }
    /// Declared so introspection says readwrite; [`Props::set`] does the write.
    #[zbus(property)]
    async fn set_active_profile(&self, _profile: String) -> zbus::Result<()> {
        Err(written_through_properties())
    }
    #[zbus(property)]
    async fn performance_inhibited(&self) -> String {
        String::new()
    }
    #[zbus(property)]
    async fn performance_degraded(&self) -> String {
        self.ctx.ppd_degraded().await
    }
    #[zbus(property)]
    async fn profiles(&self) -> Vec<HashMap<String, OwnedValue>> {
        profiles(&self.ctx.sys_root)
    }
    #[zbus(property)]
    async fn actions(&self) -> Vec<String> {
        Vec::new()
    }
    #[zbus(property)]
    async fn actions_info(&self) -> Vec<HashMap<String, OwnedValue>> {
        Vec::new()
    }
    #[zbus(property)]
    async fn active_profile_holds(&self) -> Vec<HashMap<String, OwnedValue>> {
        self.ctx.ppd_holds_view().await
    }
    #[zbus(property)]
    async fn version(&self) -> String {
        VERSION.to_string()
    }
    #[zbus(property)]
    async fn battery_aware(&self) -> bool {
        self.ctx.state.lock().await.auto_switch
    }
    #[zbus(property)]
    async fn set_battery_aware(&self, _on: bool) -> zbus::Result<()> {
        Err(written_through_properties())
    }

    async fn hold_profile(
        &self,
        profile: String,
        reason: String,
        application_id: String,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<u32> {
        hold(&self.ctx, conn, &hdr, &profile, &reason, &application_id).await
    }

    async fn release_profile(&self, cookie: u32, #[zbus(connection)] conn: &Connection) -> zbus::fdo::Result<()> {
        release(&self.ctx, conn, cookie).await
    }

    async fn set_action_enabled(&self, action: String, _enabled: bool) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported(format!("rimed has no action '{action}'")))
    }

    #[zbus(signal)]
    async fn profile_released(ctxt: &SignalContext<'_>, cookie: u32) -> zbus::Result<()>;
}

/// `net.hadess.PowerProfiles` at [`LEGACY_PATH`]: the same, without what came after the rename.
pub struct LegacyPowerProfiles {
    pub ctx: Arc<Ctx>,
}

#[interface(name = "net.hadess.PowerProfiles")]
impl LegacyPowerProfiles {
    #[zbus(property)]
    async fn active_profile(&self) -> String {
        self.ctx.state.lock().await.tier.as_str().to_string()
    }
    #[zbus(property)]
    async fn set_active_profile(&self, _profile: String) -> zbus::Result<()> {
        Err(written_through_properties())
    }
    #[zbus(property)]
    async fn performance_inhibited(&self) -> String {
        String::new()
    }
    #[zbus(property)]
    async fn performance_degraded(&self) -> String {
        self.ctx.ppd_degraded().await
    }
    #[zbus(property)]
    async fn profiles(&self) -> Vec<HashMap<String, OwnedValue>> {
        profiles(&self.ctx.sys_root)
    }
    #[zbus(property)]
    async fn actions(&self) -> Vec<String> {
        Vec::new()
    }
    #[zbus(property)]
    async fn active_profile_holds(&self) -> Vec<HashMap<String, OwnedValue>> {
        self.ctx.ppd_holds_view().await
    }
    #[zbus(property)]
    async fn version(&self) -> String {
        VERSION.to_string()
    }

    async fn hold_profile(
        &self,
        profile: String,
        reason: String,
        application_id: String,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<u32> {
        hold(&self.ctx, conn, &hdr, &profile, &reason, &application_id).await
    }

    async fn release_profile(&self, cookie: u32, #[zbus(connection)] conn: &Connection) -> zbus::fdo::Result<()> {
        release(&self.ctx, conn, cookie).await
    }

    #[zbus(signal)]
    async fn profile_released(ctxt: &SignalContext<'_>, cookie: u32) -> zbus::Result<()>;
}

fn to_fdo(e: anyhow::Error) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(format!("{e:#}"))
}

fn parse_profile(s: &str) -> zbus::fdo::Result<Tier> {
    s.parse::<Tier>().map_err(|_| zbus::fdo::Error::InvalidArgs(format!("Invalid profile name '{s}'")))
}

async fn hold(
    ctx: &Arc<Ctx>,
    conn: &Connection,
    hdr: &Header<'_>,
    profile: &str,
    reason: &str,
    app_id: &str,
) -> zbus::fdo::Result<u32> {
    authorize(conn, hdr, ACTION_HOLD).await?;
    let t = parse_profile(profile)?;
    if t == Tier::Balanced {
        return Err(zbus::fdo::Error::InvalidArgs(
            "Only profiles 'performance' and 'power-saver' can be a hold profile".into(),
        ));
    }
    let owner = hdr
        .sender()
        .map(|s| s.to_string())
        .ok_or_else(|| zbus::fdo::Error::AccessDenied("no sender on message".into()))?;
    let cookie = ctx.ppd_hold(t, reason, app_id, &owner).await.map_err(to_fdo)?;
    crate::dbus::emit_state_changed(conn, ctx).await;
    Ok(cookie)
}

async fn release(ctx: &Arc<Ctx>, conn: &Connection, cookie: u32) -> zbus::fdo::Result<()> {
    let gone = ctx.ppd_release(|h| h.cookie == cookie).await.map_err(to_fdo)?;
    if gone.is_empty() {
        return Err(zbus::fdo::Error::InvalidArgs(format!("No hold with cookie {cookie}")));
    }
    emit_released(conn, &gone).await;
    crate::dbus::emit_state_changed(conn, ctx).await;
    Ok(())
}

/// The `ActiveProfile` write, after polkit.
async fn write_profile(ctx: &Arc<Ctx>, conn: &Connection, profile: &str) -> zbus::fdo::Result<()> {
    let t = parse_profile(profile)?;
    let gone = ctx.ppd_choose(t).await.map_err(to_fdo)?;
    // As `Power.SetTier`: with auto-switch on, the user's tier for this power source, kept across AC changes and
    // restarts (choice.rs). Game mode turns auto-switch off, so a write during it records nothing.
    ctx.record_choice(t).await;
    emit_released(conn, &gone).await;
    crate::dbus::emit_state_changed(conn, ctx).await;
    Ok(())
}

/// `ProfileReleased(cookie)` on both objects, one per cookie.
pub async fn emit_released(conn: &Connection, cookies: &[u32]) {
    if cookies.is_empty() {
        return;
    }
    let server = conn.object_server();
    if let Ok(iref) = server.interface::<_, PowerProfiles>(PATH).await {
        for c in cookies {
            let _ = PowerProfiles::profile_released(iref.signal_context(), *c).await;
        }
    }
    if let Ok(iref) = server.interface::<_, LegacyPowerProfiles>(LEGACY_PATH).await {
        for c in cookies {
            let _ = LegacyPowerProfiles::profile_released(iref.signal_context(), *c).await;
        }
    }
}

/// `PropertiesChanged` for every property that can change, on both objects. Called from every path that moves the
/// tier, auto-switch, game mode or the holds (dbus.rs `emit_state_changed`).
pub async fn emit_changed(conn: &Connection) {
    let server = conn.object_server();
    if let Ok(iref) = server.interface::<_, PowerProfiles>(PATH).await {
        let ctxt = iref.signal_context().clone();
        let i = iref.get().await;
        let _ = i.active_profile_changed(&ctxt).await;
        let _ = i.active_profile_holds_changed(&ctxt).await;
        let _ = i.battery_aware_changed(&ctxt).await;
        let _ = i.performance_degraded_changed(&ctxt).await;
    }
    if let Ok(iref) = server.interface::<_, LegacyPowerProfiles>(LEGACY_PATH).await {
        let ctxt = iref.signal_context().clone();
        let i = iref.get().await;
        let _ = i.active_profile_changed(&ctxt).await;
        let _ = i.active_profile_holds_changed(&ctxt).await;
        let _ = i.performance_degraded_changed(&ctxt).await;
    }
}

/// `PerformanceDegraded` alone (the AC loop's tick notices it change).
pub async fn emit_degraded_changed(conn: &Connection) {
    let server = conn.object_server();
    if let Ok(iref) = server.interface::<_, PowerProfiles>(PATH).await {
        let _ = iref.get().await.performance_degraded_changed(iref.signal_context()).await;
    }
    if let Ok(iref) = server.interface::<_, LegacyPowerProfiles>(LEGACY_PATH).await {
        let _ = iref.get().await.performance_degraded_changed(iref.signal_context()).await;
    }
}

// ── org.freedesktop.DBus.Properties, with the caller ─────────────────────────

/// The `org.freedesktop.DBus.Properties` of [`PATH`] (`legacy` false) or [`LEGACY_PATH`] (true), in place of zbus's,
/// so a property write is authorised by polkit against the caller.
pub struct Props {
    pub ctx: Arc<Ctx>,
    pub legacy: bool,
}

impl Props {
    fn ours(&self) -> &'static str {
        if self.legacy {
            LEGACY_NAME
        } else {
            NAME
        }
    }

    async fn all(&self, server: &ObjectServer, iface: &str) -> zbus::fdo::Result<HashMap<String, OwnedValue>> {
        if iface == self.ours() {
            if self.legacy {
                server.interface::<_, LegacyPowerProfiles>(LEGACY_PATH).await?.get().await.get_all().await
            } else {
                server.interface::<_, PowerProfiles>(PATH).await?.get().await.get_all().await
            }
        } else if iface.starts_with("org.freedesktop.DBus.") {
            Ok(HashMap::new())
        } else {
            Err(zbus::fdo::Error::UnknownInterface(format!("Unknown interface '{iface}'")))
        }
    }
}

#[interface(name = "org.freedesktop.DBus.Properties")]
impl Props {
    async fn get(
        &self,
        interface_name: InterfaceName<'_>,
        property_name: &str,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> zbus::fdo::Result<OwnedValue> {
        let mut all = self.all(server, interface_name.as_str()).await?;
        all.remove(property_name)
            .ok_or_else(|| zbus::fdo::Error::UnknownProperty(format!("Unknown property '{property_name}'")))
    }

    async fn get_all(
        &self,
        interface_name: InterfaceName<'_>,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> zbus::fdo::Result<HashMap<String, OwnedValue>> {
        self.all(server, interface_name.as_str()).await
    }

    async fn set(
        &self,
        interface_name: InterfaceName<'_>,
        property_name: &str,
        value: Value<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        let all = self.all(server, interface_name.as_str()).await?;
        match property_name {
            "ActiveProfile" if interface_name.as_str() == self.ours() => {
                authorize(conn, &hdr, ACTION_SWITCH).await?;
                let s = String::try_from(value)
                    .map_err(|_| zbus::fdo::Error::InvalidArgs("ActiveProfile is a string".into()))?;
                write_profile(&self.ctx, conn, &s).await
            }
            "BatteryAware" if !self.legacy && interface_name.as_str() == self.ours() => {
                authorize(conn, &hdr, ACTION_BATTERY_AWARE).await?;
                let on = bool::try_from(value)
                    .map_err(|_| zbus::fdo::Error::InvalidArgs("BatteryAware is a boolean".into()))?;
                self.ctx.set_auto_switch(on).await.map_err(to_fdo)?;
                crate::dbus::emit_state_changed(conn, &self.ctx).await;
                Ok(())
            }
            p if all.contains_key(p) => Err(zbus::fdo::Error::PropertyReadOnly(format!("Property '{p}' is read-only"))),
            p => Err(zbus::fdo::Error::UnknownProperty(format!("Unknown property '{p}'"))),
        }
    }

    #[zbus(signal)]
    async fn properties_changed(
        ctxt: &SignalContext<'_>,
        interface_name: InterfaceName<'_>,
        changed_properties: HashMap<&str, Value<'_>>,
        invalidated_properties: &[&str],
    ) -> zbus::Result<()>;
}

/// Put [`Props`] in place of zbus's `Properties` on both objects. After the objects exist (the node must keep an
/// interface while the old `Properties` goes, or zbus removes the node).
pub async fn install_props(conn: &Connection, ctx: &Arc<Ctx>) -> zbus::Result<()> {
    let server = conn.object_server();
    for (path, legacy) in [(PATH, false), (LEGACY_PATH, true)] {
        server.remove::<zbus::fdo::Properties, _>(path).await?;
        server.at(path, Props { ctx: ctx.clone(), legacy }).await?;
    }
    Ok(())
}

/// Ask for both well-known names. Never fatal: a name rimed cannot have costs this API, not rimed.
///
/// `ReplaceExisting`: a power-profiles-daemon that was somehow started first and allows replacement (it does) loses
/// its name and exits; `DoNotQueue`: rimed never waits in line behind another owner.
pub async fn claim_names(conn: &Connection) {
    for name in [NAME, LEGACY_NAME] {
        let flags = RequestNameFlags::ReplaceExisting | RequestNameFlags::DoNotQueue;
        match conn.request_name_with_flags(name, flags).await {
            Ok(r) => eprintln!("rimed: power profiles: {name}: {r:?}"),
            Err(e) => {
                eprintln!("rimed: power profiles: cannot own {name} ({e}); desktops' power controls will not reach rimed")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every action rimed asks polkit about is in the policy it ships, and the policy names no action rimed does not
    /// know (a renamed action authorises nothing: pkcheck answers "no such action" for ever).
    #[test]
    fn the_shipped_policies_name_exactly_the_actions_rimed_asks_for() {
        let ppd = include_str!("../../../files/system/polkit-1/actions/org.rimeos.rimed.power-profiles.policy");
        let ids: Vec<&str> =
            ppd.split("<action id=\"").skip(1).map(|r| r.split('"').next().expect("closed")).collect();
        assert_eq!(ids, [ACTION_SWITCH, ACTION_HOLD, ACTION_BATTERY_AWARE, ACTION_CONFIGURE]);
        let rimed = include_str!("../../../files/system/polkit-1/actions/org.rimeos.rimed.policy");
        for id in [crate::updates::ACTION_CHECK, crate::updates::ACTION_APPLY] {
            assert!(rimed.contains(&format!("<action id=\"{id}\">")), "{id}");
        }
        // Applying an update installs a system: never without an administrator, whoever asks.
        let apply = rimed.split("<action id=\"org.rimeos.rimed.apply-update\">").nth(1).expect("apply action");
        let apply = apply.split("</action>").next().expect("closed");
        for allow in ["allow_any", "allow_inactive", "allow_active"] {
            assert!(apply.contains(&format!("<{allow}>auth_admin_keep</{allow}>")), "{allow}: {apply}");
        }
    }

    fn hold(h: &mut Holds, t: Tier) -> u32 {
        h.add(t, "r", "app", ":1.1")
    }

    #[test]
    fn power_saver_beats_performance() {
        let mut h = Holds::default();
        assert_eq!(h.effective(), None);
        hold(&mut h, Tier::Performance);
        assert_eq!(h.effective(), Some(Tier::Performance));
        let c = hold(&mut h, Tier::PowerSaver);
        hold(&mut h, Tier::Performance);
        assert_eq!(h.effective(), Some(Tier::PowerSaver));
        h.list.retain(|x| x.cookie != c);
        assert_eq!(h.effective(), Some(Tier::Performance));
    }

    // rime-os #139's model intents, on this model.

    #[test]
    fn the_last_release_puts_back_what_the_first_hold_found() {
        let mut h = Holds::default();
        let c1 = h.take(Tier::Performance, "", "a", ":1.1", Tier::Balanced);
        let c2 = h.take(Tier::PowerSaver, "", "b", ":1.2", Tier::Performance);
        assert_eq!(h.release(|x| x.cookie == c2), (vec![c2], Some(Tier::Performance)), "back to the remaining hold");
        assert_eq!(h.release(|x| x.cookie == c1), (vec![c1], Some(Tier::Balanced)));
        assert_eq!(h.release(|x| x.cookie == c1), (vec![], None), "a cookie releases once");
    }

    #[test]
    fn a_release_that_changes_nothing_applies_nothing() {
        let mut h = Holds::default();
        let c1 = h.take(Tier::Performance, "", "a", ":1.1", Tier::Balanced);
        h.take(Tier::Performance, "", "b", ":1.2", Tier::Performance);
        assert_eq!(h.release(|x| x.cookie == c1), (vec![c1], None));
        let ps = h.take(Tier::PowerSaver, "", "c", ":1.3", Tier::Performance);
        h.take(Tier::PowerSaver, "", "d", ":1.4", Tier::PowerSaver);
        assert_eq!(h.release(|x| x.cookie == ps), (vec![ps], None), "another power-saver hold still stands");
    }

    #[test]
    fn a_caller_leaving_the_bus_releases_its_holds() {
        let mut h = Holds::default();
        h.take(Tier::PowerSaver, "", "a", ":1.1", Tier::Balanced);
        h.take(Tier::PowerSaver, "", "a", ":1.1", Tier::PowerSaver);
        assert_eq!(h.release(|x| x.owner == ":1.9"), (vec![], None));
        let (gone, t) = h.release(|x| x.owner == ":1.1");
        assert_eq!(gone.len(), 2);
        assert_eq!(t, Some(Tier::Balanced));
        assert!(h.list.is_empty());
    }

    #[test]
    fn a_user_choice_drops_every_hold_and_keeps_the_choice() {
        let mut h = Holds::default();
        h.take(Tier::Performance, "", "a", ":1.1", Tier::Balanced);
        assert_eq!(h.clear().len(), 1);
        assert_eq!(h.return_tier, None, "the user's tier stands");
        assert!(h.clear().is_empty(), "nothing to release twice");
        // The next hold returns to what the user chose, not to the tier before the cleared holds.
        let c = h.take(Tier::PowerSaver, "", "b", ":1.2", Tier::Performance);
        assert_eq!(h.release(|x| x.cookie == c), (vec![c], Some(Tier::Performance)));
    }

    #[test]
    fn a_power_saver_hold_beats_game_mode_and_a_performance_hold_does_not() {
        assert_eq!(live_in_game(Tier::Performance, None), Tier::Performance);
        assert_eq!(live_in_game(Tier::Performance, Some(Tier::Performance)), Tier::Performance);
        assert_eq!(live_in_game(Tier::Performance, Some(Tier::PowerSaver)), Tier::PowerSaver);
        assert_eq!(live_in_game(Tier::Balanced, Some(Tier::Performance)), Tier::Balanced);
    }

    #[test]
    fn cookies_are_unique_and_never_zero() {
        let mut h = Holds { next: u32::MAX - 1, ..Default::default() };
        let a = hold(&mut h, Tier::Performance);
        let b = hold(&mut h, Tier::Performance);
        assert_eq!((a, b), (u32::MAX, 1));
    }

    #[test]
    fn drivers_and_degradation_come_from_sysfs() {
        let root = std::env::temp_dir().join(format!("rimed-ppd-sys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(cpu_driver(&root), "rimed");
        assert_eq!(platform_driver(&root), "rimed");
        std::fs::create_dir_all(root.join("devices/system/cpu/cpu0/cpufreq")).unwrap();
        std::fs::write(root.join("devices/system/cpu/cpu0/cpufreq/scaling_driver"), "amd-pstate-epp\n").unwrap();
        std::fs::create_dir_all(root.join("firmware/acpi")).unwrap();
        std::fs::write(root.join("firmware/acpi/platform_profile"), "balanced\n").unwrap();
        assert_eq!(cpu_driver(&root), "amd_pstate");
        assert_eq!(platform_driver(&root), "platform_profile");
        let p = profiles(&root);
        assert_eq!(p.len(), 3);
        for (entry, t) in p.iter().zip(Tier::ALL.iter().rev()) {
            assert_eq!(String::try_from(entry["Profile"].try_clone().unwrap()).unwrap(), t.as_str());
            assert_eq!(String::try_from(entry["Driver"].try_clone().unwrap()).unwrap(), "rimed");
        }
        std::fs::create_dir_all(root.join("bus/platform/devices/thinkpad_acpi")).unwrap();
        std::fs::write(root.join("bus/platform/devices/thinkpad_acpi/dytc_lapmode"), "1\n").unwrap();
        assert_eq!(degraded(&root, Tier::Performance), "lap-detected");
        assert_eq!(degraded(&root, Tier::Balanced), "");
        std::fs::create_dir_all(root.join("devices/system/cpu/intel_pstate")).unwrap();
        std::fs::write(root.join("devices/system/cpu/intel_pstate/no_turbo"), "1\n").unwrap();
        assert_eq!(degraded(&root, Tier::Performance), "lap-detected,high-operating-temperature");
        let _ = std::fs::remove_dir_all(&root);
    }
}
