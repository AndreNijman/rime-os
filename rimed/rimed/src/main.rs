//! `rimed` — the Rime OS power daemon.
//!
//! Detects the machine, selects a layered profile, exposes the
//! `org.rimeos.Rimed1` D-Bus surface, auto-switches tiers on AC/battery
//! transitions, and serves Prometheus metrics. Never writes hardware when
//! `RIMED_DRY_RUN=1` (or `--dry-run`).
//!
//! Nothing here assumes a particular machine: every capability (batteries,
//! charge thresholds, cpufreq knobs, the ACPI platform profile, fans, GPUs) is
//! probed at start-up, and an absent one is reported rather than fatal.

mod dbus;
mod fan;
mod game;
mod hold;
mod metrics;
mod polkit;
mod state;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use rimed_core::gpu::{NvidiaSmi, RealNvidiaSmi};
use rimed_core::syswriter::{RealWriter, SysWriter};
use rimed_core::{select, Fingerprint, ProfileSet};

use crate::dbus::{
    BatteryIface, FanIface, GameModeIface, MetricsIface, ModeIface, PowerIface, ProfileIface,
    BUS_NAME, OBJECT_PATH,
};
use crate::state::{read_ac_online, Ctx, State};

#[tokio::main]
async fn main() -> Result<()> {
    let dry_run = rimed_core::dry_run_from_env() || std::env::args().any(|a| a == "--dry-run");

    // Detect + select (read-only).
    let fingerprint = Fingerprint::detect();
    let profiles = ProfileSet::load(Some(Path::new(rimed_core::PROFILE_DIR)))
        .context("loading system profiles")?;
    let selection = select(&fingerprint, &profiles);

    eprintln!(
        "rimed: {} / {} — profile active={} class={} device={} (dry_run={})",
        fingerprint.sys_vendor,
        fingerprint.product_version,
        selection.active,
        selection.class_or_empty(),
        selection.device_or_empty(),
        dry_run
    );

    // Writer: real sysfs, gated by dry-run.
    let writer: Arc<dyn SysWriter> = Arc::new(RealWriter::for_daemon(dry_run));

    // Initial state.
    let on_ac = read_ac_online(Path::new("/sys"));
    let profile = profiles
        .get(&selection.active)
        .or_else(|| profiles.get(&selection.generic))
        .context("profile set has no generic layer")?;
    let (charge_start, charge_stop) = profile.charge_window().unwrap_or((0, 100));
    let initial_tier = if on_ac {
        profile.defaults.ac
    } else {
        profile.defaults.battery
    };
    let initial = State {
        tier: initial_tier,
        auto_switch: true,
        on_ac,
        travel_mode: false,
        charge_start,
        charge_stop,
    };

    let nvidia: Arc<dyn NvidiaSmi> = Arc::new(RealNvidiaSmi);
    let ctx = Ctx::new(
        profiles,
        selection,
        fingerprint,
        writer,
        dry_run,
        initial,
        Path::new("/sys"),
        Path::new(rimed_core::irq::PROC_IRQ),
        Path::new("/proc"),
        nvidia,
        mode_file(),
    );

    eprintln!("rimed: batteries: {}", ctx.batteries.summary());
    if ctx.fan.supported() {
        eprintln!("rimed: fan control: {}", ctx.fan.backends().join("; "));
    } else {
        eprintln!("rimed: fan control: no controllable fan found (reporting unsupported)");
    }

    // Bring hardware to the initial state (charge thresholds + tier + fan).
    ctx.apply_charge_defaults().await.ok();
    ctx.apply_tier(initial_tier).await.ok();
    ctx.fan.apply_default().await;

    // The mode the user chose, put back. Every start, not only boot: a daemon
    // restart (an update, `Restart=on-failure`) is "anything happens" too.
    ctx.restore_held().await;

    // Build the D-Bus service: seven interfaces on one path.
    let conn = zbus::connection::Builder::system()
        .context("connecting to the system bus")?
        .name(BUS_NAME)
        .context("claiming bus name")?
        .serve_at(OBJECT_PATH, PowerIface { ctx: ctx.clone() })?
        .serve_at(OBJECT_PATH, BatteryIface { ctx: ctx.clone() })?
        .serve_at(OBJECT_PATH, ProfileIface { ctx: ctx.clone() })?
        .serve_at(OBJECT_PATH, MetricsIface { ctx: ctx.clone() })?
        .serve_at(OBJECT_PATH, FanIface { ctx: ctx.clone() })?
        .serve_at(OBJECT_PATH, GameModeIface { ctx: ctx.clone() })?
        .serve_at(OBJECT_PATH, ModeIface { ctx: ctx.clone() })?
        .build()
        .await
        .context("building the D-Bus service")?;

    eprintln!("rimed: serving {BUS_NAME} at {OBJECT_PATH}");

    // Metrics endpoint.
    tokio::spawn(metrics::serve(ctx.clone()));

    // AC/battery poll loop.
    tokio::spawn(ac_event_loop(ctx.clone(), conn.clone()));

    // The watch that lets a destroyed Gaming Mode session release the machine.
    tokio::spawn(game_owner_watch(ctx.clone(), conn.clone()));

    // Run until told to stop, then unwind in the reverse order of set-up.
    wait_for_shutdown().await;
    eprintln!("rimed: shutting down");
    // 1. Leave game mode: releases the GPU clock locks, the IRQ affinities and
    //    the cpuset, and restores the tier the session interrupted.
    ctx.game_exit().await.ok();
    // 2. Hand the fans back to the firmware. This must happen before the
    //    process can exit for any *graceful* reason; a crash is covered by
    //    `ExecStopPost=/usr/bin/rime fan restore --local` in rimed.service.
    ctx.fan.restore().await;
    // 3. Leave the machine on the middle tier rather than on whatever the last
    //    request happened to be — a stopped daemon should not leave a laptop
    //    pinned to `performance` with nothing left to walk it back down.
    ctx.apply_tier(rimed_core::Tier::Balanced).await.ok();
    Ok(())
}

/// `$STATE_DIRECTORY/mode` (rimed.service sets `StateDirectory=rimed`), else
/// `/var/lib/rimed/mode`.
fn mode_file() -> std::path::PathBuf {
    let dir = std::env::var_os("STATE_DIRECTORY")
        .filter(|d| !d.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| hold::DEFAULT_STATE_DIR.into());
    dir.join(hold::MODE_FILE)
}

/// Poll AC state; on a transition, update state, emit the property change, and
/// (when auto-switch is on) reconcile the tier.
async fn ac_event_loop(ctx: Arc<Ctx>, conn: zbus::Connection) {
    let mut ticker = tokio::time::interval(Duration::from_secs(2));
    loop {
        ticker.tick().await;
        let now = read_ac_online(Path::new("/sys"));
        let (changed, auto) = {
            let mut st = ctx.state.lock().await;
            let changed = st.on_ac != now;
            st.on_ac = now;
            (changed, st.auto_switch)
        };
        if !changed {
            continue;
        }
        if let Err(e) = dbus::emit_ac_changed(&conn).await {
            eprintln!("rimed: emit OnAcPower failed: {e}");
        }
        if auto {
            let target = ctx.auto_target().await;
            if let Err(e) = ctx.apply_tier(target).await {
                eprintln!("rimed: auto-switch apply failed: {e:#}");
                continue;
            }
            if let Err(e) = dbus::emit_tier_changed(&conn, target).await {
                eprintln!("rimed: emit TierChanged failed: {e}");
            }
            eprintln!("rimed: AC {} -> tier {}", if now { "on" } else { "off" }, target);
        }
    }
}

/// Release game mode when the process that asked for it has died.
///
/// THE DEFECT, measured on katana 2026-09-19 (evidence §3.4): Gaming Mode's
/// session script releases game mode from an EXIT trap that runs
/// `rime game stop`. That is polkit action `org.rimeos.rimed.manage-power`,
/// `allow_active=yes` and `auth_admin` otherwise, so the moment logind stops
/// calling the session active — a `systemctl restart greetd`, a VT switch away,
/// any logind-driven teardown — the trap's call is REFUSED and does nothing.
/// The machine was left with a p-core cpuset, steered IRQs, the `performance`
/// tier and `scx_lavd` loaded, with nothing able to undo them.
///
/// The daemon is root and asks polkit nothing about itself, so it is the one
/// party that can still act after the session is gone. Two seconds is the same
/// cadence [`ac_event_loop`] already runs at; a loop this cheap (one `read` of
/// one small file, and only while a session with an owner is live) does not
/// deserve an event mechanism, and a poll cannot miss an edge the way a dropped
/// signal subscription can.
///
/// An UNREADABLE `/proc` is not a release. Releasing on it would let an I/O
/// error change the machine's power state; it is logged once per run of
/// failures and the session is left alone.
async fn game_owner_watch(ctx: Arc<Ctx>, conn: zbus::Connection) {
    let mut ticker = tokio::time::interval(Duration::from_secs(2));
    let mut complained = false;
    loop {
        ticker.tick().await;
        match ctx.game_release_if_owner_gone().await {
            Ok(None) => complained = false,
            Ok(Some(_why)) => {
                complained = false;
                // Tell the bus, or rime-shell keeps drawing a session that is
                // over and `rime game status` disagrees with the hardware.
                if let Err(e) = dbus::emit_game_mode_changed(&conn, false).await {
                    eprintln!("rimed: game: emitting the owner-driven release failed: {e}");
                }
            }
            Err(e) => {
                if !complained {
                    eprintln!(
                        "rimed: game: the session owner could not be read ({e:#}); \
                         leaving the session alone"
                    );
                    complained = true;
                }
            }
        }
    }
}

/// Resolve on SIGINT or SIGTERM.
async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
