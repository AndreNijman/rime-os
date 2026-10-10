//! Daemon runtime state and the logic that turns tier changes into writer
//! actions.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Result};
use rimed_core::battery::BatteryInventory;
use rimed_core::gpu::NvidiaSmi;
use rimed_core::mode::ModeId;
use rimed_core::profile::Profile;
use rimed_core::syswriter::SysWriter;
use rimed_core::tier::Tier;
use rimed_core::{ProfileSet, Selection};
use tokio::sync::Mutex;

use crate::fan::FanController;
use crate::game::GameSession;

/// Mutable daemon state (behind an async mutex).
#[derive(Debug, Clone)]
pub struct State {
    pub tier: Tier,
    pub auto_switch: bool,
    pub on_ac: bool,
    pub travel_mode: bool,
    pub charge_start: u8,
    pub charge_stop: u8,
}

/// Everything the interfaces and loops share. Immutable config lives inline;
/// mutable bits live behind `state`.
pub struct Ctx {
    pub set: ProfileSet,
    pub selection: Selection,
    pub fingerprint: rimed_core::Fingerprint,
    pub writer: Arc<dyn SysWriter>,
    pub dry_run: bool,
    /// The batteries this machine actually has, discovered at start-up, with
    /// their charge-threshold capability probed. Empty on a desktop.
    pub batteries: BatteryInventory,
    /// The sysfs root everything reads from (parameterised for fixtures).
    pub sys_root: PathBuf,
    /// The procfs IRQ root game mode enumerates interrupts from.
    ///
    /// Parameterised for the same reason `sys_root` is, and the hazard is the
    /// same one: this was `rimed_core::irq::PROC_IRQ`, a hardcoded `/proc/irq`,
    /// in a daemon where every other read is rooted. A test that wanted to
    /// prove what game mode reports about IRQ steering had to either read the
    /// host's real interrupts — non-deterministic, and one careless writer away
    /// from steering the developer's machine — or not exist. It did not exist,
    /// which is how `rime game status` shipped reporting a plan as a
    /// measurement.
    pub proc_irq_root: PathBuf,
    /// The procfs root the session-owner watch reads `<pid>/stat` from.
    ///
    /// Rooted for exactly the reason `proc_irq_root` is, and the stake is
    /// higher: this is the reading that makes the daemon UNDO game mode. A
    /// hardcoded `/proc` would mean the only way to test "a torn-down session
    /// releases the machine" is to tear down a real session on real hardware,
    /// which is how that defect reached katana in the first place.
    pub proc_root: PathBuf,
    /// M6: fan discovery, mode state and the restore path.
    pub fan: Arc<FanController>,
    /// M6: read-side access to `nvidia-smi`.
    pub nvidia: Arc<dyn NvidiaSmi>,
    /// M6: the active game session, if any.
    pub game: Mutex<Option<GameSession>>,
    pub state: Mutex<State>,
    /// The mode the user chose, kept across restarts (see `hold.rs`). In
    /// memory as well as on disk, so the guards read no file per call.
    pub held: Mutex<Option<ModeId>>,
    /// Where the held mode is written (`$STATE_DIRECTORY/mode`); a fixture
    /// path in tests, so no suite can write the host's choice.
    pub mode_file: PathBuf,
}

impl Ctx {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        set: ProfileSet,
        selection: Selection,
        fingerprint: rimed_core::Fingerprint,
        writer: Arc<dyn SysWriter>,
        dry_run: bool,
        initial: State,
        sys_root: impl Into<PathBuf>,
        proc_irq_root: impl Into<PathBuf>,
        proc_root: impl Into<PathBuf>,
        nvidia: Arc<dyn NvidiaSmi>,
        mode_file: impl Into<PathBuf>,
    ) -> Arc<Ctx> {
        let sys_root = sys_root.into();
        let fan_cfg = set
            .get(&selection.active)
            .map(|p| p.fan_config())
            .unwrap_or_default();
        let fan = FanController::new(sys_root.clone(), fan_cfg, writer.clone());
        let batteries = BatteryInventory::discover(&sys_root);
        Arc::new(Ctx {
            set,
            selection,
            fingerprint,
            writer,
            dry_run,
            batteries,
            sys_root,
            proc_irq_root: proc_irq_root.into(),
            proc_root: proc_root.into(),
            fan,
            nvidia,
            game: Mutex::new(None),
            state: Mutex::new(initial),
            held: Mutex::new(None),
            mode_file: mode_file.into(),
        })
    }

    /// The active profile.
    ///
    /// Selection can only ever name a profile the set contains — `ProfileSet`
    /// guarantees the generic layer exists even when an on-disk override
    /// directory supplies nothing usable — but fall back rather than panic if
    /// that invariant is ever broken: a mistuned daemon beats a dead one.
    pub fn profile(&self) -> &Profile {
        self.set
            .get(&self.selection.active)
            .or_else(|| self.set.get(&self.selection.generic))
            .expect("profile set always retains a generic layer")
    }

    /// Apply a tier: update state and push the plan through the writer.
    /// Returns the previous tier.
    pub async fn apply_tier(self: &Arc<Self>, tier: Tier) -> Result<Option<Tier>> {
        let prev = {
            let mut st = self.state.lock().await;
            let prev = st.tier;
            st.tier = tier;
            Some(prev)
        };

        for action in &self.profile().plan_transition(prev, tier) {
            self.writer.apply(action)?;
        }

        Ok(prev)
    }

    /// Compute the tier the auto-switch policy wants for the current AC state.
    pub async fn auto_target(&self) -> Tier {
        let on_ac = self.state.lock().await.on_ac;
        let d = &self.profile().defaults;
        if on_ac {
            d.ac
        } else {
            d.battery
        }
    }

    /// True when at least one discovered battery accepts a charge threshold.
    pub fn charge_thresholds_supported(&self) -> bool {
        self.batteries.supports_thresholds()
    }

    /// Apply the battery charge thresholds the active profile declares (if
    /// any), recording them in state.
    ///
    /// A machine with no battery, or with batteries whose driver exposes no
    /// threshold attribute, is a **silent** skip: the profile expressed an
    /// intent the hardware cannot honour, which is not an error at start-up.
    /// An *explicit* request goes through [`Ctx::set_charge_thresholds`], which
    /// does say so.
    pub async fn apply_charge_defaults(self: &Arc<Self>) -> Result<()> {
        let Some((start, stop)) = self.profile().charge_window() else {
            return Ok(());
        };
        let plan = self.batteries.plan_thresholds(start, stop);
        if plan.is_empty() {
            return Ok(());
        }
        {
            let mut st = self.state.lock().await;
            st.charge_start = start;
            st.charge_stop = stop;
        }
        for action in &plan {
            self.writer.apply(action)?;
        }
        Ok(())
    }

    /// Set explicit charge thresholds (from the D-Bus method), on every battery
    /// that supports them. Errors — rather than silently succeeding — when the
    /// machine has no threshold control at all, so a caller that asked for
    /// something specific is told it did not happen.
    pub async fn set_charge_thresholds(self: &Arc<Self>, start: u8, stop: u8) -> Result<()> {
        let plan = self.batteries.plan_thresholds(start, stop);
        if plan.is_empty() {
            bail!(
                "this machine exposes no battery charge-threshold control ({})",
                self.batteries.summary()
            );
        }
        for action in &plan {
            self.writer.apply(action)?;
        }
        let mut st = self.state.lock().await;
        st.charge_start = start;
        st.charge_stop = stop;
        Ok(())
    }
}

/// Whether the machine is running on wall power. Read-only.
///
/// The ladder matters because the power-supply class is not uniform:
///
/// 1. Any `Mains` supply reporting `online = 1` — the normal laptop answer.
/// 2. A `Mains` supply exists and none is online -> on battery.
/// 3. No `Mains` at all but a battery is present (USB-PD-only tablets, some
///    ARM laptops, and any machine whose AC driver did not bind): believe the
///    battery's own `status`, which reads `Discharging` off the wall.
/// 4. Nothing readable at all (a desktop, a VM, a container): assume AC. A
///    desktop must never be treated as if it were running on a battery.
pub fn read_ac_online(sys_root: &Path) -> bool {
    let dir = sys_root.join("class/power_supply");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return true; // assume AC if we cannot tell (desktop-safe default)
    };
    let mut saw_mains = false;
    let mut battery_discharging: Option<bool> = None;
    for e in entries.flatten() {
        let p = e.path();
        let ty = std::fs::read_to_string(p.join("type"))
            .unwrap_or_default()
            .trim()
            .to_string();
        match ty.as_str() {
            "Mains" => {
                saw_mains = true;
                if std::fs::read_to_string(p.join("online"))
                    .map(|s| s.trim() == "1")
                    .unwrap_or(false)
                {
                    return true;
                }
            }
            "Battery" => {
                if let Ok(s) = std::fs::read_to_string(p.join("status")) {
                    let s = s.trim().to_string();
                    // Any pack that is charging means wall power is present.
                    if s == "Charging" {
                        battery_discharging = Some(false);
                    } else if s == "Discharging" && battery_discharging.is_none() {
                        battery_discharging = Some(true);
                    }
                }
            }
            _ => {}
        }
    }
    if saw_mains {
        return false; // a Mains supply exists and none of them is online
    }
    // No AC line to consult: let the battery speak, else assume wall power.
    !battery_discharging.unwrap_or(false)
}
