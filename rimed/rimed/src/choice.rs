//! The tier the user picked, per power source, kept across AC events and
//! restarts.
//!
//! THE DEFECT (2026-11-03): picking Power Saver on the Dashboard ran `SetTier`,
//! rimed wrote the tier, and nothing remembered it. The next AC transition
//! re-derived the tier from the profile's defaults (battery → `balanced`), and
//! so did every rimed start — every boot, and every live update, which
//! restarts rimed. The choice quietly fell back to Balanced, the Shell's next
//! read showed Balanced, and to the user the switch "did nothing".
//!
//! Now a `SetTier` made while auto-switch is ON (the Dashboard, `rime tier`)
//! is the user's tier **for the power source the machine is on**, and
//! auto-switch follows it: pick Power Saver on battery, plug in (the AC
//! choice, or the profile's AC default, applies), unplug, and Power Saver comes
//! back. A `SetTier` made with auto-switch OFF is a mode pinning a tier
//! (`rime mode`, which turns auto-switch off first and keeps its own hold in
//! `hold.rs`), so it is not recorded: a mode must not rewrite Daily's choice.
//!
//! File: `$STATE_DIRECTORY/tier`, two lines, `ac=<tier>` and `battery=<tier>`,
//! either absent. Written to a temporary name and renamed. An unreadable or
//! damaged file is ignored (logged): the profile's defaults apply.

use std::path::Path;

use anyhow::{Context, Result};
use rimed_core::tier::Tier;

/// The file's name inside the state directory (next to `mode`).
pub const TIER_FILE: &str = "tier";

/// The user's tier per power source. `None` = the profile's default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Choice {
    pub ac: Option<Tier>,
    pub battery: Option<Tier>,
}

impl Choice {
    /// The choice for one power source.
    pub fn for_source(&self, on_ac: bool) -> Option<Tier> {
        if on_ac {
            self.ac
        } else {
            self.battery
        }
    }

    /// Record `tier` for one power source.
    pub fn set(&mut self, on_ac: bool, tier: Tier) {
        if on_ac {
            self.ac = Some(tier);
        } else {
            self.battery = Some(tier);
        }
    }

    fn render(&self) -> String {
        let mut s = String::new();
        if let Some(t) = self.ac {
            s.push_str(&format!("ac={}\n", t.as_str()));
        }
        if let Some(t) = self.battery {
            s.push_str(&format!("battery={}\n", t.as_str()));
        }
        s
    }

    fn parse(text: &str) -> std::result::Result<Choice, String> {
        let mut c = Choice::default();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let (k, v) = line
                .split_once('=')
                .ok_or_else(|| format!("not key=value: {line:?}"))?;
            let t: Tier = v.trim().parse().map_err(|e| format!("{e}"))?;
            match k.trim() {
                "ac" => c.ac = Some(t),
                "battery" => c.battery = Some(t),
                other => return Err(format!("unknown key {other:?}")),
            }
        }
        Ok(c)
    }
}

/// Read the recorded choice. Missing = none. Damaged = none, logged: a broken
/// file must not be able to pick a power policy.
pub fn read(path: &Path) -> Choice {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Choice::default(),
        Err(e) => {
            eprintln!(
                "rimed: tier choice: cannot read {} ({e}); using the profile's defaults",
                path.display()
            );
            return Choice::default();
        }
    };
    Choice::parse(&text).unwrap_or_else(|e| {
        eprintln!(
            "rimed: tier choice: {} is damaged ({e}); using the profile's defaults",
            path.display()
        );
        Choice::default()
    })
}

/// Record the choice (temporary file + rename).
pub fn write(path: &Path, c: &Choice) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, c.render()).with_context(|| format!("writing {}", tmp.display()))?;
    if let Ok(f) = std::fs::File::open(&tmp) {
        let _ = f.sync_all();
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("rimed-choice-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_choice_round_trips_per_source() {
        let p = scratch("roundtrip").join(TIER_FILE);
        assert_eq!(read(&p), Choice::default(), "missing file = no choice");
        let mut c = Choice::default();
        c.set(false, Tier::PowerSaver);
        write(&p, &c).unwrap();
        assert_eq!(read(&p).for_source(false), Some(Tier::PowerSaver));
        assert_eq!(read(&p).for_source(true), None, "AC keeps its default");
        c.set(true, Tier::Balanced);
        write(&p, &c).unwrap();
        let back = read(&p);
        assert_eq!(back.ac, Some(Tier::Balanced));
        assert_eq!(back.battery, Some(Tier::PowerSaver));
    }

    #[test]
    fn a_damaged_file_picks_nothing() {
        let p = scratch("damaged").join(TIER_FILE);
        for bad in ["battery=ultra\n", "garbage\n", "wifi=balanced\n"] {
            std::fs::write(&p, bad).unwrap();
            assert_eq!(read(&p), Choice::default(), "{bad:?}");
        }
    }

    // ── the daemon side ─────────────────────────────────────────────────────
    use crate::boost::ctx_tests::{build, PROFILE};

    #[tokio::test]
    async fn a_pick_with_auto_switch_on_is_kept_for_that_source_only() {
        let (ctx, _w) = build("choice-pick", PROFILE);
        assert_eq!(ctx.auto_target().await, Tier::Balanced, "profile default");
        ctx.apply_tier(Tier::PowerSaver).await.unwrap();
        ctx.record_choice(Tier::PowerSaver).await;
        assert_eq!(ctx.auto_target().await, Tier::PowerSaver, "battery pick");
        assert_eq!(
            read(&ctx.choice_file).battery,
            Some(Tier::PowerSaver),
            "on disk"
        );
        // On AC nothing was picked: the profile's AC default.
        ctx.state.lock().await.on_ac = true;
        assert_eq!(ctx.auto_target().await, Tier::Balanced);
        ctx.state.lock().await.on_ac = false;
        assert_eq!(ctx.auto_target().await, Tier::PowerSaver);
    }

    #[tokio::test]
    async fn a_mode_pinning_a_tier_with_auto_switch_off_records_nothing() {
        let (ctx, _w) = build("choice-pinned", PROFILE);
        ctx.state.lock().await.auto_switch = false;
        ctx.record_choice(Tier::Performance).await;
        assert_eq!(*ctx.choice.lock().await, Choice::default());
        assert!(!ctx.choice_file.exists());
        ctx.state.lock().await.auto_switch = true;
        assert_eq!(
            ctx.auto_target().await,
            Tier::Balanced,
            "Daily's default is untouched"
        );
    }
}
