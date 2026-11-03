//! rimed as the ONE writer of the CPU's power policy: the power-profiles
//! facade.
//!
//! WHY: until this module, Rime ran power-profiles-daemon (PPD) next to rimed.
//! Both write the same files (`scaling_governor`,
//! `energy_performance_preference`, `cpufreq/boost`, `platform_profile`), and
//! the last writer wins: PPD at boot, rimed on its next AC transition. Measured
//! read-only on an MSI laptop on AC: rimed reported tier `performance` while
//! sysfs held `powersave` / `balance_performance`, PPD's `balanced`. A UI boost
//! that restores "the tier's exact values" means nothing while a second daemon
//! rewrites them.
//!
//! The image masks `power-profiles-daemon.service` and rimed owns PPD's two bus
//! names, `org.freedesktop.UPower.PowerProfiles` and `net.hadess.PowerProfiles`.
//! PPD starts its drivers only after it has acquired its name, and quits when
//! it cannot, so a PPD started by hand anyway writes nothing. Everything that
//! spoke PPD's API (`powerprofilesctl`, GNOME/KDE applets, apps taking a
//! profile hold) now reads and holds rimed's tiers, which map one to one:
//! `performance`, `balanced`, `power-saver`.
//!
//! What the facade does NOT do:
//! - **Writing `ActiveProfile` is refused.** zbus property setters see no
//!   message header, so the caller cannot be checked with polkit, and an
//!   unchecked tier change from any local user would undo the policy
//!   `org.rimeos.Rimed1.Power.SetTier` enforces. The error names `rime tier`.
//! - **No PPD "actions"** (`Actions`/`ActionsInfo` are empty). PPD 0.30's
//!   `trickle_charge` touches only peripheral batteries, and its amdgpu
//!   `panel_power`/`dpm` actions are off by default; the CPU-side parity
//!   (`cpufreq/boost`, the min-frequency floor) moved into rimed's tiers
//!   (`cpu_boost`, `cpu_min_freq`).
//!
//! Holds work as in PPD: `HoldProfile` takes `performance` or `power-saver`,
//! `power-saver` wins over `performance` when both are held, and when the last
//! hold goes (released, or its caller left the bus) the tier and the
//! auto-switch setting from before the first hold come back. Holds need the
//! same polkit action as `SetTier`, and are refused during game mode, which
//! owns the tier while it runs. A user choosing a tier drops every hold.

use std::collections::HashMap;
use std::sync::Arc;

use rimed_core::tier::Tier;
use zbus::fdo::RequestNameFlags;
use zbus::message::Header;
use zbus::{interface, Connection, SignalContext};
use zvariant::{OwnedValue, Value};

use crate::polkit::{authorize, ACTION_POWER};
use crate::state::Ctx;

/// PPD's current name and object path.
pub const UPOWER_NAME: &str = "org.freedesktop.UPower.PowerProfiles";
pub const UPOWER_PATH: &str = "/org/freedesktop/UPower/PowerProfiles";
/// PPD's legacy name and object path, still used by older clients.
pub const HADESS_NAME: &str = "net.hadess.PowerProfiles";
pub const HADESS_PATH: &str = "/net/hadess/PowerProfiles";

/// One profile hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
    pub cookie: u32,
    pub profile: Tier,
    pub reason: String,
    pub app_id: String,
    pub sender: String,
}

/// The holds, pure. `saved` is what comes back when the last hold goes.
#[derive(Debug, Default)]
pub struct Holds {
    next: u32,
    list: Vec<Hold>,
    saved: Option<(Tier, bool)>,
}

/// What releasing holds asks the daemon to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Released {
    pub cookies: Vec<u32>,
    /// The tier to apply now (`None`: holds remain and the effective profile
    /// did not change, or nothing was released).
    pub tier: Option<Tier>,
    /// The auto-switch setting to put back (the last hold went).
    pub auto_switch: Option<bool>,
}

impl Holds {
    pub fn list(&self) -> &[Hold] {
        &self.list
    }

    /// `power-saver` beats `performance`, as in PPD.
    pub fn effective(&self) -> Option<Tier> {
        if self.list.is_empty() {
            None
        } else if self.list.iter().any(|h| h.profile == Tier::PowerSaver) {
            Some(Tier::PowerSaver)
        } else {
            Some(Tier::Performance)
        }
    }

    /// Take a hold. `current`/`auto` are the tier and auto-switch setting
    /// right now; the first hold remembers them. Answers the cookie and the
    /// tier to apply.
    pub fn take(
        &mut self,
        profile: Tier,
        reason: &str,
        app_id: &str,
        sender: &str,
        current: Tier,
        auto: bool,
    ) -> (u32, Tier) {
        if self.list.is_empty() {
            self.saved = Some((current, auto));
        }
        self.next = self.next.wrapping_add(1).max(1);
        let cookie = self.next;
        self.list.push(Hold {
            cookie,
            profile,
            reason: reason.to_string(),
            app_id: app_id.to_string(),
            sender: sender.to_string(),
        });
        (cookie, self.effective().unwrap_or(profile))
    }

    /// Release one hold by cookie. `None` for an unknown cookie.
    pub fn release(&mut self, cookie: u32) -> Option<Released> {
        let before = self.effective();
        let n = self.list.len();
        self.list.retain(|h| h.cookie != cookie);
        if self.list.len() == n {
            return None;
        }
        Some(self.after(before, vec![cookie]))
    }

    /// `sender` left the bus: release its holds.
    pub fn release_sender(&mut self, sender: &str) -> Option<Released> {
        let before = self.effective();
        let cookies: Vec<u32> = self
            .list
            .iter()
            .filter(|h| h.sender == sender)
            .map(|h| h.cookie)
            .collect();
        if cookies.is_empty() {
            return None;
        }
        self.list.retain(|h| h.sender != sender);
        Some(self.after(before, cookies))
    }

    /// The user chose a tier: every hold goes and that choice stands. Only the
    /// auto-switch setting the first hold turned off comes back.
    pub fn clear(&mut self) -> Released {
        let cookies = self.list.drain(..).map(|h| h.cookie).collect();
        Released {
            cookies,
            tier: None,
            auto_switch: self.saved.take().map(|(_, a)| a),
        }
    }

    fn after(&mut self, before: Option<Tier>, cookies: Vec<u32>) -> Released {
        if self.list.is_empty() {
            let (tier, auto) = self.saved.take().unzip();
            return Released {
                cookies,
                tier,
                auto_switch: auto,
            };
        }
        let now = self.effective();
        Released {
            cookies,
            tier: if now != before { now } else { None },
            auto_switch: None,
        }
    }
}

impl Ctx {
    /// Take a hold for `sender` and apply the tier it asks for.
    pub async fn ppd_hold(
        self: &Arc<Self>,
        profile: Tier,
        reason: &str,
        app_id: &str,
        sender: &str,
    ) -> anyhow::Result<(u32, Tier)> {
        if self.game.lock().await.is_some() {
            anyhow::bail!("game mode owns the power profile while it runs");
        }
        let mut holds = self.ppd_holds.lock().await;
        let (current, auto) = {
            let st = self.state.lock().await;
            (st.tier, st.auto_switch)
        };
        let (cookie, tier) = holds.take(profile, reason, app_id, sender, current, auto);
        // While a hold stands, the AC/battery switch must not move the tier.
        self.state.lock().await.auto_switch = false;
        if let Err(e) = self.apply_tier(tier).await {
            holds.release(cookie);
            self.state.lock().await.auto_switch = auto;
            return Err(e);
        }
        Ok((cookie, tier))
    }

    /// Apply what a release asks for. Answers the tier applied, if any.
    pub async fn ppd_settle(self: &Arc<Self>, r: &Released) -> Option<Tier> {
        if let Some(a) = r.auto_switch {
            self.state.lock().await.auto_switch = a;
        }
        let t = r.tier?;
        match self.apply_tier(t).await {
            Ok(_) => Some(t),
            Err(e) => {
                eprintln!("rimed: power-profiles: restoring tier {t}: {e:#}");
                None
            }
        }
    }
}

fn profile_name(t: Tier) -> &'static str {
    t.as_str()
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

/// The `Profiles` property: lowest first, as PPD lists them.
fn profiles(ctx: &Ctx) -> Vec<HashMap<String, OwnedValue>> {
    let platform = ctx.sys_root.join("firmware/acpi/platform_profile").exists();
    [Tier::PowerSaver, Tier::Balanced, Tier::Performance]
        .into_iter()
        .map(|t| {
            let mut pairs = vec![
                ("Profile", Value::from(profile_name(t))),
                ("CpuDriver", Value::from("rimed")),
                ("Driver", Value::from("rimed")),
            ];
            if platform {
                pairs.push(("PlatformDriver", Value::from("rimed")));
            }
            dict(&pairs)
        })
        .collect()
}

async fn holds_property(ctx: &Ctx) -> Vec<HashMap<String, OwnedValue>> {
    ctx.ppd_holds
        .lock()
        .await
        .list()
        .iter()
        .map(|h| {
            dict(&[
                ("Profile", Value::from(profile_name(h.profile))),
                ("ApplicationId", Value::from(h.app_id.as_str())),
                ("Reason", Value::from(h.reason.as_str())),
            ])
        })
        .collect()
}

fn refused_set() -> zbus::fdo::Error {
    zbus::fdo::Error::AccessDenied(
        "rimed owns the power profile; switch it with `rime tier <name>` \
         (org.rimeos.Rimed1.Power.SetTier)"
            .into(),
    )
}

/// One facade interface. PPD serves the same members under two interface
/// names, so the impl is generated twice.
macro_rules! facade {
    ($ty:ident, $iface:tt) => {
        pub struct $ty {
            pub ctx: Arc<Ctx>,
        }

        #[interface(name = $iface)]
        impl $ty {
            #[zbus(property)]
            async fn active_profile(&self) -> String {
                profile_name(self.ctx.state.lock().await.tier).to_string()
            }

            #[zbus(property)]
            async fn set_active_profile(&self, _profile: String) -> zbus::Result<()> {
                Err(zbus::Error::FDO(Box::new(refused_set())))
            }

            #[zbus(property)]
            async fn performance_inhibited(&self) -> String {
                String::new()
            }

            #[zbus(property)]
            async fn performance_degraded(&self) -> String {
                String::new()
            }

            #[zbus(property)]
            async fn profiles(&self) -> Vec<HashMap<String, OwnedValue>> {
                profiles(&self.ctx)
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
                holds_property(&self.ctx).await
            }

            #[zbus(property)]
            async fn version(&self) -> String {
                concat!("rimed ", env!("CARGO_PKG_VERSION")).to_string()
            }

            /// Battery awareness is rimed's auto-switch (`rime tier auto`).
            #[zbus(property)]
            async fn battery_aware(&self) -> bool {
                true
            }

            async fn hold_profile(
                &self,
                profile: String,
                reason: String,
                application_id: String,
                #[zbus(connection)] conn: &Connection,
                #[zbus(header)] hdr: Header<'_>,
            ) -> zbus::fdo::Result<u32> {
                authorize(conn, &hdr, ACTION_POWER).await?;
                let tier = match profile.as_str() {
                    "performance" => Tier::Performance,
                    "power-saver" => Tier::PowerSaver,
                    _ => {
                        return Err(zbus::fdo::Error::InvalidArgs(format!(
                            "only performance and power-saver can be held, not {profile:?}"
                        )))
                    }
                };
                let sender = hdr.sender().map(|s| s.to_string()).unwrap_or_default();
                let (cookie, applied) = self
                    .ctx
                    .ppd_hold(tier, &reason, &application_id, &sender)
                    .await
                    .map_err(|e| zbus::fdo::Error::Failed(format!("{e:#}")))?;
                announce(conn, applied).await;
                Ok(cookie)
            }

            async fn release_profile(
                &self,
                cookie: u32,
                #[zbus(connection)] conn: &Connection,
            ) -> zbus::fdo::Result<()> {
                let r = self.ctx.ppd_holds.lock().await.release(cookie);
                let Some(r) = r else {
                    return Err(zbus::fdo::Error::InvalidArgs(format!(
                        "no hold with cookie {cookie}"
                    )));
                };
                settle(&self.ctx, conn, &r).await;
                Ok(())
            }

            async fn set_action_enabled(
                &self,
                action: String,
                _enabled: bool,
            ) -> zbus::fdo::Result<()> {
                Err(zbus::fdo::Error::InvalidArgs(format!(
                    "no action {action:?}"
                )))
            }

            #[zbus(signal)]
            async fn profile_released(ctxt: &SignalContext<'_>, cookie: u32) -> zbus::Result<()>;
        }
    };
}

facade!(UPowerFacade, "org.freedesktop.UPower.PowerProfiles");
facade!(HadessFacade, "net.hadess.PowerProfiles");

/// Apply a release and tell everyone: `ProfileReleased` per cookie, the
/// property changes, and the tier change on rimed's own interface.
pub async fn settle(ctx: &Arc<Ctx>, conn: &Connection, r: &Released) {
    let tier = ctx.ppd_settle(r).await;
    for &c in &r.cookies {
        emit_released(conn, c).await;
    }
    match tier {
        Some(t) => announce(conn, t).await,
        None => emit_changed(conn).await,
    }
}

/// A hold or release moved the tier: rimed's `TierChanged` plus the facade.
async fn announce(conn: &Connection, tier: Tier) {
    if let Err(e) = crate::dbus::emit_tier_changed(conn, tier).await {
        eprintln!("rimed: power-profiles: emitting TierChanged failed: {e}");
    }
}

async fn emit_released(conn: &Connection, cookie: u32) {
    let srv = conn.object_server();
    if let Ok(i) = srv.interface::<_, UPowerFacade>(UPOWER_PATH).await {
        let _ = UPowerFacade::profile_released(i.signal_context(), cookie).await;
    }
    if let Ok(i) = srv.interface::<_, HadessFacade>(HADESS_PATH).await {
        let _ = HadessFacade::profile_released(i.signal_context(), cookie).await;
    }
}

/// `PropertiesChanged` for `ActiveProfile` and `ActiveProfileHolds` on both
/// facade objects. Errors are ignored: a daemon serving no facade (a test
/// connection) has nothing to tell.
pub async fn emit_changed(conn: &Connection) {
    let srv = conn.object_server();
    if let Ok(i) = srv.interface::<_, UPowerFacade>(UPOWER_PATH).await {
        let ctxt = i.signal_context().clone();
        let g = i.get().await;
        let _ = g.active_profile_changed(&ctxt).await;
        let _ = g.active_profile_holds_changed(&ctxt).await;
    }
    if let Ok(i) = srv.interface::<_, HadessFacade>(HADESS_PATH).await {
        let ctxt = i.signal_context().clone();
        let g = i.get().await;
        let _ = g.active_profile_changed(&ctxt).await;
        let _ = g.active_profile_holds_changed(&ctxt).await;
    }
}

/// Serve the facade objects and claim both names. A name that cannot be had
/// is logged, not fatal: rimed's own interface is what the Shell needs.
///
/// `ReplaceExisting`: a PPD that was somehow started first and allows
/// replacement (PPD does) loses its name and exits; `DoNotQueue`: rimed never
/// waits in line behind another owner.
pub async fn serve(ctx: &Arc<Ctx>, conn: &Connection) {
    let srv = conn.object_server();
    let served = srv
        .at(UPOWER_PATH, UPowerFacade { ctx: ctx.clone() })
        .await
        .and(srv.at(HADESS_PATH, HadessFacade { ctx: ctx.clone() }).await);
    if let Err(e) = served {
        eprintln!("rimed: power-profiles: serving the facade failed: {e}");
        return;
    }
    for name in [UPOWER_NAME, HADESS_NAME] {
        let flags = RequestNameFlags::ReplaceExisting | RequestNameFlags::DoNotQueue;
        match conn.request_name_with_flags(name, flags).await {
            Ok(r) => eprintln!("rimed: power-profiles: {name}: {r:?}"),
            Err(e) => eprintln!("rimed: power-profiles: could not own {name}: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_saver_wins_over_performance() {
        let mut h = Holds::default();
        let (_, t) = h.take(
            Tier::Performance,
            "build",
            "a",
            ":1.1",
            Tier::Balanced,
            true,
        );
        assert_eq!(t, Tier::Performance);
        let (_, t) = h.take(
            Tier::PowerSaver,
            "low",
            "b",
            ":1.2",
            Tier::Performance,
            false,
        );
        assert_eq!(t, Tier::PowerSaver);
        let (_, t) = h.take(
            Tier::Performance,
            "again",
            "c",
            ":1.3",
            Tier::PowerSaver,
            false,
        );
        assert_eq!(t, Tier::PowerSaver, "a later performance hold does not win");
    }

    #[test]
    fn the_last_release_puts_back_what_the_first_hold_found() {
        let mut h = Holds::default();
        let (c1, _) = h.take(Tier::Performance, "", "a", ":1.1", Tier::Balanced, true);
        let (c2, _) = h.take(Tier::PowerSaver, "", "b", ":1.2", Tier::Performance, false);
        let r = h.release(c2).unwrap();
        assert_eq!(
            r.tier,
            Some(Tier::Performance),
            "back to the remaining hold"
        );
        assert_eq!(r.auto_switch, None);
        let r = h.release(c1).unwrap();
        assert_eq!(r.tier, Some(Tier::Balanced));
        assert_eq!(r.auto_switch, Some(true));
        assert!(h.release(c1).is_none(), "a cookie releases once");
    }

    #[test]
    fn a_release_that_changes_nothing_applies_nothing() {
        let mut h = Holds::default();
        let (c1, _) = h.take(Tier::Performance, "", "a", ":1.1", Tier::Balanced, true);
        h.take(Tier::Performance, "", "b", ":1.2", Tier::Performance, false);
        assert_eq!(h.release(c1).unwrap().tier, None);
    }

    #[test]
    fn a_caller_leaving_the_bus_releases_its_holds() {
        let mut h = Holds::default();
        h.take(Tier::PowerSaver, "", "a", ":1.1", Tier::Balanced, true);
        h.take(Tier::PowerSaver, "", "a", ":1.1", Tier::PowerSaver, false);
        assert!(h.release_sender(":1.9").is_none());
        let r = h.release_sender(":1.1").unwrap();
        assert_eq!(r.cookies.len(), 2);
        assert_eq!(r.tier, Some(Tier::Balanced));
        assert!(h.list().is_empty());
    }

    #[test]
    fn a_user_choice_drops_every_hold_and_keeps_the_choice() {
        let mut h = Holds::default();
        h.take(Tier::Performance, "", "a", ":1.1", Tier::Balanced, true);
        let r = h.clear();
        assert_eq!(r.cookies.len(), 1);
        assert_eq!(r.tier, None, "the user's tier stands");
        assert_eq!(r.auto_switch, Some(true));
        assert_eq!(h.clear().auto_switch, None, "nothing to restore twice");
    }
}
