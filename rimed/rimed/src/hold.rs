//! The mode the user chose, kept across restarts.
//!
//! ## The defect this answers (katana, 2026-10-10)
//!
//! Settings → Gaming → "Gaming" runs `rime mode set gaming`, which moves three
//! live levers (auto-switch off, the `performance` pin, game mode on) and
//! records nothing. rimed builds its start-up state from the profile defaults,
//! so every boot, and every daemon restart, came back in Daily. A desktop game
//! quitting did the same sooner: gamemode.ini's `end=` hook runs
//! `rime game stop`, and so does Gaming Mode's EXIT trap, and either one ended
//! the game mode the user had switched on.
//!
//! ## What is kept, and where
//!
//! One word: the mode's id, in `/var/lib/rimed/mode` (`StateDirectory=rimed`).
//! Not a snapshot of the levers. The shutdown path deliberately leaves game mode
//! and drops to `balanced`, so a snapshot taken there would always say Daily;
//! and the mode is the thing the user picked, so it is the thing to keep.
//! Daily is what rimed does with no file at all, so choosing it removes the
//! file. A file that does not name a mode is ignored (and said so), never
//! guessed at.
//!
//! ## What holding a mode changes
//!
//! * At start-up rimed plans the held mode against its own initial state with
//!   the same pure [`rimed_core::mode::plan`] `rime mode set` uses, and runs the
//!   steps itself, in the planner's order. Nothing asks polkit: the daemon is
//!   acting on a choice an authorised caller already made.
//! * While the held mode turns game mode on, `GameMode.SetActive(false)` is
//!   refused with a message naming the way out (`rime mode set daily`). That is
//!   the one frozen member whose error behaviour changes, and the change is the
//!   point: "until you turn it off" was false the first time a game quit.
//!   `game_exit` itself is untouched, so shutdown and a mode change still leave
//!   game mode normally.
//! * A held session never adopts an owner. Gaming Mode's
//!   `rime game start --owner-pid $$` would otherwise hand the held session to
//!   the owner watch, which releases it when Gaming Mode ends.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use rimed_core::mode::{self, ModeId, ModeState, Step};

use crate::state::Ctx;

/// Where the held mode lives when systemd does not say (`$STATE_DIRECTORY`).
pub const DEFAULT_STATE_DIR: &str = "/var/lib/rimed";
/// The file's name inside the state directory.
pub const MODE_FILE: &str = "mode";

/// Read the held mode. Missing = none; unreadable or not a mode id = none,
/// logged, because a damaged file must not be able to pick a power policy.
pub fn read(path: &Path) -> Option<ModeId> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            eprintln!("rimed: mode: cannot read {} ({e}); starting in daily", path.display());
            return None;
        }
    };
    match text.trim().parse::<ModeId>() {
        Ok(ModeId::Daily) => None,
        Ok(id) => Some(id),
        Err(e) => {
            eprintln!(
                "rimed: mode: {} does not name a mode ({e}); ignoring it and starting in daily",
                path.display()
            );
            None
        }
    }
}

/// Record the held mode. `None` (and Daily, which is the absence of a hold)
/// removes the file. Written to a temporary name and renamed, so a crash
/// mid-write leaves the old choice or the new one, never half of either.
pub fn write(path: &Path, id: Option<ModeId>) -> Result<()> {
    match id {
        None | Some(ModeId::Daily) => match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
        },
        Some(id) => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("creating {}", dir.display()))?;
            }
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, format!("{}\n", id.as_str()))
                .with_context(|| format!("writing {}", tmp.display()))?;
            if let Ok(f) = std::fs::File::open(&tmp) {
                let _ = f.sync_all();
            }
            std::fs::rename(&tmp, path)
                .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))
        }
    }
}

impl Ctx {
    /// The mode held across restarts, if any.
    pub async fn held_mode(&self) -> Option<ModeId> {
        *self.held.lock().await
    }

    /// True when the held mode is one that keeps game mode on.
    pub async fn held_keeps_game(&self) -> bool {
        self.held_mode().await.is_some_and(|m| m.spec().game)
    }

    /// Why `GameMode.SetActive(false)` must be refused right now, or `None`
    /// when it may go ahead: game mode is on AND the held mode keeps it on.
    pub async fn game_stop_refusal(&self) -> Option<String> {
        if !self.game_active().await {
            return None;
        }
        let m = self.held_mode().await.filter(|m| m.spec().game)?;
        Some(format!(
            "'{m}' mode is on and keeps game mode on until it is turned off: \
             choose Everyday in Settings → Gaming, or run `rime mode set daily`"
        ))
    }

    /// Hold `id` (Daily or `None` releases the hold). Persisted BEFORE memory
    /// changes, so a write that fails leaves the daemon saying what the disk
    /// says.
    pub async fn set_held(&self, id: Option<ModeId>) -> Result<()> {
        let id = id.filter(|m| *m != ModeId::Daily);
        if !self.dry_run {
            write(&self.mode_file, id)?;
        }
        *self.held.lock().await = id;
        Ok(())
    }

    /// Put the held mode back: read the file, plan it against the state rimed
    /// is in now, and run the steps in the planner's order. Returns the mode
    /// restored, or `None` when nothing is held.
    ///
    /// A step that fails is logged and the rest still run: half of Gaming (the
    /// pin without game mode, say) is closer to what the user chose than Daily.
    pub async fn restore_held(self: &Arc<Self>) -> Option<ModeId> {
        let id = read(&self.mode_file)?;
        *self.held.lock().await = Some(id);
        let game_active = self.game_active().await;
        let state = {
            let st = self.state.lock().await;
            ModeState {
                tier: st.tier,
                auto_switch: st.auto_switch,
                game_active,
            }
        };
        for step in mode::plan(id.spec(), &state) {
            if let Err(e) = self.apply_step(&step).await {
                eprintln!("rimed: mode: restoring '{id}': {} failed: {e:#}", step.describe());
            }
        }
        eprintln!("rimed: mode: '{id}' restored (held across restarts)");
        Some(id)
    }

    /// One planner step, done by the daemon itself — the same effect as the
    /// D-Bus member [`Step`] names, without the bus or polkit.
    async fn apply_step(self: &Arc<Self>, step: &Step) -> Result<()> {
        match step {
            Step::AutoSwitch(on) => {
                self.state.lock().await.auto_switch = *on;
                if *on {
                    let t = self.auto_target().await;
                    self.apply_tier(t).await?;
                }
                Ok(())
            }
            Step::SetTier(t) => self.apply_tier(*t).await.map(|_| ()),
            Step::GameMode(true) => self.game_enter(&[]).await,
            Step::GameMode(false) => self.game_exit().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("rimed-hold-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_mode_round_trips_and_daily_removes_the_file() {
        let d = scratch("roundtrip");
        let f = d.join("state/mode");
        assert_eq!(read(&f), None, "no file is no hold");
        write(&f, Some(ModeId::Gaming)).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "gaming\n");
        assert_eq!(read(&f), Some(ModeId::Gaming));
        write(&f, Some(ModeId::Daily)).unwrap();
        assert!(!f.exists(), "daily is the absence of a hold, not a file saying so");
        write(&f, None).unwrap(); // removing nothing is not an error
        assert!(!d.join("state/mode.tmp").exists());
    }

    #[test]
    fn a_damaged_file_holds_nothing() {
        let d = scratch("damaged");
        let f = d.join("mode");
        for junk in ["", "gamingg", "../etc/passwd", "performance"] {
            std::fs::write(&f, junk).unwrap();
            assert_eq!(read(&f), None, "{junk:?} must not pick a policy");
        }
        std::fs::write(&f, "daily\n").unwrap();
        assert_eq!(read(&f), None, "a file saying daily holds nothing");
        std::fs::write(&f, "  creator \n").unwrap();
        assert_eq!(read(&f), Some(ModeId::Creator));
    }
}
