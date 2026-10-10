//! The frozen `org.rimeos.Rimed1` D-Bus surface. Seven interfaces share one
//! object path (`/org/rimeos/Rimed1`); all state lives in [`Ctx`]. The seventh,
//! `org.rimeos.Rimed1.Mode` (2026-10-10), is additive: it changes no frozen
//! member's signature.
//!
//! Tier IDs are the frozen strings from `rimed_core::Tier` and must never
//! change — the rime-shell `PowerProfileService` and the `rime` CLI both depend
//! on them verbatim.

use std::collections::HashMap;
use std::sync::Arc;

use rimed_core::mode::ModeId;
use rimed_core::tier::Tier;
use zbus::message::Header;
use zbus::{interface, Connection, SignalContext};
use zvariant::{OwnedValue, Value};

use crate::metrics::Reading;
use crate::polkit::{authorize, ACTION_BATTERY, ACTION_POWER};
use crate::state::Ctx;

/// Well-known bus name.
pub const BUS_NAME: &str = "org.rimeos.Rimed1";
/// Shared object path.
pub const OBJECT_PATH: &str = "/org/rimeos/Rimed1";

fn to_fdo(e: anyhow::Error) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(format!("{e:#}"))
}

// ── Power ──────────────────────────────────────────────────────────────────

/// `org.rimeos.Rimed1.Power` — the tier engine surface the shell consumes.
pub struct PowerIface {
    pub ctx: Arc<Ctx>,
}

#[interface(name = "org.rimeos.Rimed1.Power")]
impl PowerIface {
    /// Current tier ID.
    #[zbus(property)]
    async fn tier(&self) -> String {
        self.ctx.state.lock().await.tier.as_str().to_string()
    }

    /// All tier IDs, highest to lowest (frozen order).
    #[zbus(property)]
    async fn tiers(&self) -> Vec<String> {
        Tier::all_ids()
    }

    /// Whether AC power is currently online.
    #[zbus(property)]
    async fn on_ac_power(&self) -> bool {
        self.ctx.state.lock().await.on_ac
    }

    /// Whether AC/battery auto-switching is enabled.
    #[zbus(property)]
    async fn auto_switch(&self) -> bool {
        self.ctx.state.lock().await.auto_switch
    }

    /// Switch to `tier`. Rejected (InvalidArgs) for an unknown ID.
    async fn set_tier(
        &self,
        tier: String,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, ACTION_POWER).await?;
        let t: Tier = tier
            .parse()
            .map_err(|e: rimed_core::UnknownTier| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        self.ctx.apply_tier(t).await.map_err(to_fdo)?;
        PowerIface::tier_changed_signal(&ctxt, t.as_str()).await?;
        self.tier_changed(&ctxt).await?;
        Ok(())
    }

    /// Enable/disable auto-switching. Enabling immediately reconciles to the
    /// AC/battery default.
    async fn set_auto_switch(
        &self,
        enabled: bool,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, ACTION_POWER).await?;
        {
            let mut st = self.ctx.state.lock().await;
            st.auto_switch = enabled;
        }
        self.auto_switch_changed(&ctxt).await?;
        if enabled {
            let target = self.ctx.auto_target().await;
            self.ctx.apply_tier(target).await.map_err(to_fdo)?;
            PowerIface::tier_changed_signal(&ctxt, target.as_str()).await?;
            self.tier_changed(&ctxt).await?;
        }
        Ok(())
    }

    /// Emitted whenever the active tier changes (D-Bus name `TierChanged`).
    #[zbus(signal, name = "TierChanged")]
    async fn tier_changed_signal(ctxt: &SignalContext<'_>, tier: &str) -> zbus::Result<()>;
}

// ── Battery ──────────────────────────────────────────────────────────────────

/// `org.rimeos.Rimed1.Battery` — charge thresholds, travel mode, calibration.
///
/// Every reading comes from the batteries discovered at start-up, never from a
/// hard-coded `BAT0`/`BAT1`. On a machine with no battery the properties answer
/// their neutral values (`Capacity = 0`, `Status = "Unknown"`,
/// `Supported = false`) and the mutating methods fail with a clear message
/// instead of pretending to have written something.
pub struct BatteryIface {
    pub ctx: Arc<Ctx>,
}

#[interface(name = "org.rimeos.Rimed1.Battery")]
impl BatteryIface {
    #[zbus(property)]
    async fn charge_start(&self) -> u8 {
        self.ctx.state.lock().await.charge_start
    }

    #[zbus(property)]
    async fn charge_end(&self) -> u8 {
        self.ctx.state.lock().await.charge_stop
    }

    #[zbus(property)]
    async fn travel_mode(&self) -> bool {
        self.ctx.state.lock().await.travel_mode
    }

    /// Whether this machine has any battery charge-threshold control at all.
    /// False on a desktop, and false on the many laptops whose driver exposes
    /// no threshold attribute.
    #[zbus(property)]
    async fn supported(&self) -> bool {
        self.ctx.charge_thresholds_supported()
    }

    /// The batteries discovered on this machine (`BAT0`, `BAT1`, `CMB0`, …).
    /// Empty on a desktop.
    #[zbus(property)]
    async fn batteries(&self) -> Vec<String> {
        self.ctx.batteries.names()
    }

    #[zbus(property)]
    async fn capacity(&self) -> u8 {
        read_battery_field(&self.ctx, "capacity")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    #[zbus(property)]
    async fn status(&self) -> String {
        read_battery_field(&self.ctx, "status").unwrap_or_else(|| "Unknown".to_string())
    }

    /// Set charge start/stop thresholds (percent).
    async fn set_charge_thresholds(
        &self,
        start: u8,
        end: u8,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, ACTION_BATTERY).await?;
        if start > end || end > 100 {
            return Err(zbus::fdo::Error::InvalidArgs(format!(
                "invalid thresholds start={start} end={end}"
            )));
        }
        self.ctx
            .set_charge_thresholds(start, end)
            .await
            .map_err(to_fdo)?;
        self.charge_start_changed(&ctxt).await?;
        self.charge_end_changed(&ctxt).await?;
        Ok(())
    }

    /// Toggle travel mode: on = tighten to a storage window (55/60); off =
    /// restore the profile's charge defaults.
    async fn set_travel_mode(
        &self,
        enabled: bool,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, ACTION_BATTERY).await?;
        if enabled {
            self.ctx.set_charge_thresholds(55, 60).await.map_err(to_fdo)?;
        } else {
            self.ctx.apply_charge_defaults().await.map_err(to_fdo)?;
        }
        {
            let mut st = self.ctx.state.lock().await;
            st.travel_mode = enabled;
        }
        self.travel_mode_changed(&ctxt).await?;
        self.charge_start_changed(&ctxt).await?;
        self.charge_end_changed(&ctxt).await?;
        Ok(())
    }

    /// Begin a battery calibration cycle: raise the charge ceiling to 100 so a
    /// full charge/discharge pass can recalibrate the gauge. (A fuller
    /// automated routine lands with the battery work in a later milestone.)
    async fn calibrate(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, ACTION_BATTERY).await?;
        self.ctx.set_charge_thresholds(0, 100).await.map_err(to_fdo)?;
        eprintln!("rimed: calibration window opened (0/100); restore thresholds when the cycle completes");
        Ok(())
    }
}

/// Read a field from the machine's primary battery. `None` when there is no
/// battery, or when this driver does not publish that field.
fn read_battery_field(ctx: &Arc<Ctx>, field: &str) -> Option<String> {
    ctx.batteries.primary()?.read(field)
}

// ── Mode (2026-10-10) ────────────────────────────────────────────────────────

/// `org.rimeos.Rimed1.Mode` — the mode kept across restarts.
///
/// `rime mode set` calls `Hold` FIRST and then moves the levers over the
/// frozen members exactly as before. First, because holding a mode that keeps
/// game mode on makes `GameMode.SetActive(false)` refuse — so leaving one has
/// to release the hold before its plan turns game mode off. `Hold` changes no
/// lever itself; rimed applies the held mode only when it starts.
pub struct ModeIface {
    pub ctx: Arc<Ctx>,
}

#[interface(name = "org.rimeos.Rimed1.Mode")]
impl ModeIface {
    /// The held mode id, or "" when nothing is held (Daily).
    #[zbus(property)]
    async fn held(&self) -> String {
        self.ctx
            .held_mode()
            .await
            .map(|m| m.as_str().to_string())
            .unwrap_or_default()
    }

    /// Hold `mode` across restarts; "daily" or "" releases the hold. polkit
    /// `manage-power`, the action behind every lever a mode moves.
    async fn hold(
        &self,
        mode: String,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, ACTION_POWER).await?;
        let id = if mode.trim().is_empty() {
            None
        } else {
            Some(
                mode.parse::<ModeId>()
                    .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?,
            )
        };
        self.ctx.set_held(id).await.map_err(to_fdo)?;
        self.held_changed(&ctxt).await?;
        Ok(())
    }
}

// ── Profile ──────────────────────────────────────────────────────────────────

/// `org.rimeos.Rimed1.Profile` — the resolved layered selection (read-only).
pub struct ProfileIface {
    pub ctx: Arc<Ctx>,
}

#[interface(name = "org.rimeos.Rimed1.Profile")]
impl ProfileIface {
    #[zbus(property)]
    async fn active(&self) -> String {
        self.ctx.selection.active.clone()
    }

    #[zbus(property)]
    async fn class(&self) -> String {
        self.ctx.selection.class_or_empty().to_string()
    }

    #[zbus(property)]
    async fn device(&self) -> String {
        self.ctx.selection.device_or_empty().to_string()
    }
}

// ── Metrics ──────────────────────────────────────────────────────────────────

/// `org.rimeos.Rimed1.Metrics` — the a{sv} telemetry snapshot.
pub struct MetricsIface {
    pub ctx: Arc<Ctx>,
}

#[interface(name = "org.rimeos.Rimed1.Metrics")]
impl MetricsIface {
    #[zbus(property)]
    async fn snapshot(&self) -> HashMap<String, OwnedValue> {
        Reading::gather(&self.ctx).await.to_snapshot()
    }
}

// ── Fan (M6) ─────────────────────────────────────────────────────────────────

/// `org.rimeos.Rimed1.Fan` — real fan control since M6.
///
/// `Mode` and `SetMode` keep their frozen M3 signatures. `Mode` still answers
/// `auto` on a machine with no controllable fan (the shell reads it
/// unconditionally); the new `Supported` property is where "this machine has no
/// fan knob" is expressed.
pub struct FanIface {
    pub ctx: Arc<Ctx>,
}

#[interface(name = "org.rimeos.Rimed1.Fan")]
impl FanIface {
    /// Current mode keyword: `auto`, `max`, `manual` or `curve`.
    #[zbus(property)]
    async fn mode(&self) -> String {
        self.ctx.fan.mode().await.as_str().to_string()
    }

    /// Whether this machine exposes a fan knob rimed can turn.
    #[zbus(property)]
    async fn supported(&self) -> bool {
        self.ctx.fan.supported()
    }

    /// The mode keywords this hardware accepts.
    #[zbus(property)]
    async fn modes(&self) -> Vec<String> {
        self.ctx.fan.modes()
    }

    /// The duty cycle rimed last commanded (0 unless in manual/curve mode).
    #[zbus(property)]
    async fn pwm(&self) -> u8 {
        match self.ctx.fan.mode().await {
            rimed_core::fan::FanMode::Manual(p) => p,
            _ => 0,
        }
    }

    /// Per-fan readings: `id`(s), `chip`(s), `rpm`(u, where the backend reports
    /// RPM), `percent`(y, where it reports a percentage instead — msi-ec),
    /// `pwm`(y) and `controllable`(b).
    #[zbus(property)]
    async fn fans(&self) -> Vec<HashMap<String, OwnedValue>> {
        self.ctx
            .fan
            .readings()
            .into_iter()
            .map(|r| {
                let mut m: HashMap<String, OwnedValue> = HashMap::new();
                insert_value(&mut m, "id", Value::from(r.id));
                insert_value(&mut m, "chip", Value::from(r.chip));
                if let Some(rpm) = r.rpm {
                    insert_value(&mut m, "rpm", Value::from(rpm));
                }
                if let Some(pct) = r.percent {
                    insert_value(&mut m, "percent", Value::from(pct));
                }
                if let Some(pwm) = r.pwm {
                    insert_value(&mut m, "pwm", Value::from(pwm));
                }
                insert_value(&mut m, "controllable", Value::from(r.controllable));
                m
            })
            .collect()
    }

    /// Switch fan mode. Accepts `auto`, `max`, `manual`, `manual:<0-255>` and
    /// `curve`. polkit `manage-power`.
    async fn set_mode(
        &self,
        mode: String,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, ACTION_POWER).await?;
        // A bare `manual` resolves to the profile's floor, not to full speed.
        let default_pwm = self.ctx.fan.default_manual_pwm();
        let m = rimed_core::fan::FanMode::parse(&mode, default_pwm)
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        self.ctx.fan.set_mode(m).await.map_err(to_fdo)?;
        self.mode_changed(&ctxt).await?;
        self.pwm_changed(&ctxt).await?;
        Ok(())
    }

    /// Manual mode at an explicit duty cycle (0-255, floored by the profile's
    /// `min_pwm`). polkit `manage-power`.
    async fn set_pwm(
        &self,
        pwm: u8,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, ACTION_POWER).await?;
        self.ctx
            .fan
            .set_mode(rimed_core::fan::FanMode::Manual(pwm))
            .await
            .map_err(to_fdo)?;
        self.mode_changed(&ctxt).await?;
        self.pwm_changed(&ctxt).await?;
        Ok(())
    }

    /// Hand the fans back to firmware control immediately. polkit
    /// `manage-power`.
    async fn restore_firmware(
        &self,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, ACTION_POWER).await?;
        self.ctx.fan.restore().await;
        self.mode_changed(&ctxt).await?;
        self.pwm_changed(&ctxt).await?;
        Ok(())
    }
}

// ── GameMode (M6) ────────────────────────────────────────────────────────────

/// `org.rimeos.Rimed1.GameMode` — real orchestration since M6: top tier, fan
/// mode, NVIDIA clock locks, P-core cpuset and IRQ steering, all reversed on
/// exit.
pub struct GameModeIface {
    pub ctx: Arc<Ctx>,
}

#[interface(name = "org.rimeos.Rimed1.GameMode")]
impl GameModeIface {
    /// Whether a session is running.
    #[zbus(property)]
    async fn active(&self) -> bool {
        self.ctx.game_active().await
    }

    /// Whether the active profile permits game mode.
    #[zbus(property)]
    async fn supported(&self) -> bool {
        self.ctx.game_supported()
    }

    /// Session detail: `active`(b), `cpus`(s), `core_source`(s),
    /// `irqs_steered`(u), `irqs_attempted`(u), `irqs_refused`(u),
    /// `gpus_locked`(au), `pids`(au), `prior_tier`(s), `tier`(s), `cgroup`(s),
    /// `notes`(as) — plus `pcores`/`ecores` when idle.
    ///
    /// `irqs_steered` counts the affinity writes the KERNEL ACCEPTED. It
    /// previously carried the number the plan intended, which made it a lie on
    /// any machine that refuses affinity writes; `irqs_attempted` now carries
    /// the plan's number and `irqs_refused` the difference, so a partial result
    /// (normal — kernel-managed interrupts are unmovable) reads as a partial
    /// result rather than as a smaller success.
    #[zbus(property)]
    async fn status(&self) -> HashMap<String, OwnedValue> {
        self.ctx.game_status().await
    }

    /// Enter or leave game mode. polkit `manage-power`.
    async fn set_active(
        &self,
        active: bool,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, ACTION_POWER).await?;
        // A mode the user holds game mode on in is ended by choosing another
        // mode, not by whatever calls `rime game stop` next: gamemode.ini's
        // `end=` hook when a desktop game quits, Gaming Mode's EXIT trap. The
        // one change to this frozen member's error behaviour (see hold.rs).
        if !active {
            if let Some(why) = self.ctx.game_stop_refusal().await {
                return Err(zbus::fdo::Error::Failed(why));
            }
        }
        self.transition(active, &[], &ctxt, conn).await
    }

    /// Enter game mode and pin `pid` (and thus its children, which inherit the
    /// cgroup) to the game's cpuset. polkit `manage-power`.
    async fn start_for_pid(
        &self,
        pid: u32,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, ACTION_POWER).await?;
        self.transition(true, &[pid], &ctxt, conn).await
    }

    /// Enter game mode and record `owner_pid` as the process whose death ends
    /// the session. polkit `manage-power` — *entering* stays exactly as
    /// restricted as it was.
    ///
    /// The session's own release path cannot be relied on: it runs
    /// `rime game stop`, and the instant logind deactivates the session (a
    /// greetd restart, a VT switch, any logind-driven teardown) polkit refuses
    /// it, leaving the machine on a p-core cpuset with steered IRQs, the
    /// `performance` tier and `scx_lavd` and nothing able to undo them —
    /// measured on katana 2026-09-19, evidence §3.4. This is the answer to
    /// that, and it is deliberately NOT a looser polkit rule: a release that
    /// `allow_inactive=yes` would authorise is a release any unprivileged local
    /// caller can perform.
    ///
    /// `owner_pid` is WATCHED, not pinned. Use `StartForPid`/`AttachPid` to put
    /// a process in the game cpuset.
    async fn start_owned_by(
        &self,
        owner_pid: u32,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, ACTION_POWER).await?;
        self.ctx
            .game_enter_owned(&[], Some(owner_pid))
            .await
            .map_err(to_fdo)?;
        self.announce(true, &ctxt, conn).await
    }

    /// Attach one more PID to a running session. polkit `manage-power`.
    async fn attach_pid(
        &self,
        pid: u32,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, ACTION_POWER).await?;
        self.ctx.game_attach(pid).await.map_err(to_fdo)
    }

    /// Emitted on every entry/exit (D-Bus name `ActiveChanged`).
    #[zbus(signal, name = "ActiveChanged")]
    async fn active_changed_signal(ctxt: &SignalContext<'_>, active: bool) -> zbus::Result<()>;
}

impl GameModeIface {
    /// Shared enter/exit body: flip the session, then tell the bus about both
    /// the game state *and* the tier the session moved.
    async fn transition(
        &self,
        active: bool,
        pids: &[u32],
        ctxt: &SignalContext<'_>,
        conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        if active {
            self.ctx.game_enter(pids).await.map_err(to_fdo)?;
        } else {
            self.ctx.game_exit().await.map_err(to_fdo)?;
        }
        self.announce(active, ctxt, conn).await
    }

    /// Tell the bus what just happened. Split out of [`Self::transition`] so
    /// the owner watch in `main.rs` emits *exactly* the same set — a release
    /// nobody is told about leaves rime-shell showing a game session that is
    /// over, which is a second bug wearing the first one's clothes.
    async fn announce(
        &self,
        active: bool,
        ctxt: &SignalContext<'_>,
        conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        let tier = self.ctx.state.lock().await.tier;
        GameModeIface::active_changed_signal(ctxt, active).await?;
        self.active_changed(ctxt).await?;
        self.status_changed(ctxt).await?;
        // The tier moved underneath the Power interface; keep its consumers
        // (rime-shell) in step.
        if let Err(e) = emit_tier_changed(conn, tier).await {
            eprintln!("rimed: game: emitting TierChanged failed: {e}");
        }
        Ok(())
    }
}

/// Emit the game-mode change signals from OUTSIDE a D-Bus method — the owner
/// watch's path. Same set as [`GameModeIface::announce`], reached through the
/// object server the way `emit_tier_changed` reaches the Power interface.
pub async fn emit_game_mode_changed(conn: &Connection, active: bool) -> zbus::Result<()> {
    let iref = conn
        .object_server()
        .interface::<_, GameModeIface>(OBJECT_PATH)
        .await?;
    let ctxt = iref.signal_context().clone();
    // Bound and dropped explicitly: the interface deref borrows `iref`, and
    // leaving it as the block's tail expression outlives the binding.
    let iface = iref.get().await;
    let out = iface.announce(active, &ctxt, conn).await;
    drop(iface);
    out.map_err(|e| zbus::Error::Failure(e.to_string()))
}

fn insert_value(m: &mut HashMap<String, OwnedValue>, key: &str, v: Value<'_>) {
    if let Ok(owned) = v.try_to_owned() {
        m.insert(key.to_string(), owned);
    }
}

// ── emission helpers (used by the AC/battery event loop) ─────────────────────

/// Emit `TierChanged` + the `Tier` property change from outside a D-Bus method
/// (i.e. from the auto-switch loop).
pub async fn emit_tier_changed(conn: &Connection, tier: Tier) -> zbus::Result<()> {
    let iref = conn
        .object_server()
        .interface::<_, PowerIface>(OBJECT_PATH)
        .await?;
    let ctxt = iref.signal_context().clone();
    PowerIface::tier_changed_signal(&ctxt, tier.as_str()).await?;
    iref.get().await.tier_changed(&ctxt).await?;
    Ok(())
}

/// Emit the `OnAcPower` property change from the event loop.
pub async fn emit_ac_changed(conn: &Connection) -> zbus::Result<()> {
    let iref = conn
        .object_server()
        .interface::<_, PowerIface>(OBJECT_PATH)
        .await?;
    let ctxt = iref.signal_context().clone();
    iref.get().await.on_ac_power_changed(&ctxt).await?;
    Ok(())
}
