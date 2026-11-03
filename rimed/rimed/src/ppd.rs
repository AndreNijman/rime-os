//! power-profiles-daemon's D-Bus API, served by rimed (docs/nixos/desktops.md §7).
//!
//! GNOME's quick settings, KDE's powerdevil, COSMIC's battery applet and `powerprofilesctl` all drive power through
//! `org.freedesktop.UPower.PowerProfiles` (and the older `net.hadess.PowerProfiles`). Rime ships no
//! power-profiles-daemon: a second daemon writing the same knobs as rimed would undo rimed's tiers. rimed answers that
//! API itself, so every desktop's own power control moves Rime's tiers and shows what rimed did.
//!
//! The mapping:
//! - `ActiveProfile` is `Power.Tier` (the ids are the same three strings). A write is exactly `Power.SetTier`: it
//!   leaves auto-switch alone and releases every hold (`ProfileReleased(cookie)` for each), as a user's choice does in
//!   power-profiles-daemon. During game mode a write sets the tier game mode returns to, and `ActiveProfile` stays
//!   `performance` (it is emitted again so a client that optimistically showed its own choice is corrected).
//! - `HoldProfile` / `ReleaseProfile` / `ActiveProfileHolds`: a hold belongs to the caller's unique bus name and goes
//!   when that name leaves the bus. Any `power-saver` hold beats every `performance` hold. When the last hold goes,
//!   the tier returns to the one in force before the first hold (or, if AC changed meanwhile with auto-switch on, to
//!   the tier auto-switch wants now). Game mode is listed as a `performance` hold by `org.rimeos.GameMode`.
//! - `BatteryAware` is `Power.AutoSwitch`.
//! - `Profiles` names `rimed` as the driver of every profile, never `placeholder` (GNOME hides a placeholder).
//! - Actions: rimed has none (`Actions` and `ActionsInfo` are empty, `SetActionEnabled` is NotSupported).
//!
//! Property writes are authorised by polkit like the methods are. zbus hands a property setter no caller, so the two
//! objects get their own `org.freedesktop.DBus.Properties` ([`Props`]) in place of zbus's: its `Set` sees the
//! caller, asks polkit, then writes.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use rimed_core::tier::Tier;
use zbus::export::futures_util::StreamExt;
use zbus::message::Header;
use zbus::names::InterfaceName;
use zbus::object_server::Interface;
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

    /// Move the base tier: during game mode only the tier it returns to changes; game mode keeps its own.
    async fn set_base_tier(self: &Arc<Self>, t: Tier) -> Result<()> {
        {
            let mut g = self.game.lock().await;
            if let Some(s) = g.as_mut() {
                s.prior_tier = t;
                return Ok(());
            }
        }
        self.apply_tier(t).await.map(|_| ())
    }

    /// `HoldProfile`: add the hold and move to the tier the holds now ask for.
    pub async fn ppd_hold(self: &Arc<Self>, profile: Tier, reason: &str, app_id: &str, owner: &str) -> Result<u32> {
        let mut h = self.ppd.lock().await;
        if h.list.is_empty() {
            h.return_tier = Some(self.base_tier().await);
        }
        let cookie = h.add(profile, reason, app_id, owner);
        let target = h.effective().unwrap_or(profile);
        if let Err(e) = self.set_base_tier(target).await {
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
        let (gone, kept): (Vec<Hold>, Vec<Hold>) = std::mem::take(&mut h.list).into_iter().partition(|x| pick(x));
        h.list = kept;
        if gone.is_empty() {
            return Ok(Vec::new());
        }
        let target = match h.effective() {
            Some(t) => Some(t),
            None => h.return_tier.take(),
        };
        if let Some(t) = target {
            self.set_base_tier(t).await?;
        }
        Ok(gone.iter().map(|x| x.cookie).collect())
    }

    /// A user's explicit choice made elsewhere (`Power.SetTier`, `Mode.Set`): every hold goes and nothing else moves.
    pub async fn ppd_forget(&self) -> Vec<u32> {
        let mut h = self.ppd.lock().await;
        h.return_tier = None;
        h.list.drain(..).map(|x| x.cookie).collect()
    }

    /// The `ActiveProfile` write: every hold goes, then the tier is `t` (during game mode: the tier it returns to).
    pub async fn ppd_choose(self: &Arc<Self>, t: Tier) -> Result<Vec<u32>> {
        let mut h = self.ppd.lock().await;
        h.return_tier = None;
        let released: Vec<u32> = h.list.drain(..).map(|x| x.cookie).collect();
        self.set_base_tier(t).await?;
        Ok(released)
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

/// `Profiles`: one entry per tier, highest first (the order `Power.Tiers` has).
pub fn profiles(sys_root: &Path) -> Vec<HashMap<String, OwnedValue>> {
    let cpu = cpu_driver(sys_root);
    let platform = platform_driver(sys_root);
    Tier::ALL
        .iter()
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

/// Ask for both well-known names. Never fatal: a power-profiles-daemon that holds them (it should not be installed;
/// nix/modules/desktops.nix asserts that) costs this API, not rimed.
pub async fn claim_names(conn: &Connection) {
    for name in [NAME, LEGACY_NAME] {
        if let Err(e) = conn.request_name(name).await {
            eprintln!("rimed: power profiles: cannot own {name} ({e}); desktops' power controls will not reach rimed");
        }
    }
}

/// Drop the holds of a client that leaves the bus.
pub async fn watch_holders(ctx: Arc<Ctx>, conn: Connection) {
    let Ok(proxy) = zbus::fdo::DBusProxy::new(&conn).await else {
        eprintln!("rimed: power profiles: cannot watch the bus; holds are kept until released");
        return;
    };
    let Ok(mut changes) = proxy.receive_name_owner_changed().await else {
        eprintln!("rimed: power profiles: cannot watch the bus; holds are kept until released");
        return;
    };
    while let Some(sig) = changes.next().await {
        let Ok(a) = sig.args() else { continue };
        if a.new_owner().is_some() {
            continue;
        }
        let name = a.name().to_string();
        if !name.starts_with(':') {
            continue;
        }
        match ctx.ppd_release(|h| h.owner == name).await {
            Ok(gone) if !gone.is_empty() => {
                emit_released(&conn, &gone).await;
                crate::dbus::emit_state_changed(&conn, &ctx).await;
            }
            Ok(_) => {}
            Err(e) => eprintln!("rimed: power profiles: releasing the holds of {name} failed: {e:#}"),
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
        for (entry, t) in p.iter().zip(Tier::ALL.iter()) {
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
