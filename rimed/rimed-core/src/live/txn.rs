//! The live-update transaction: its states, which moves between them are
//! legal, and what a crash leaves behind.
//!
//! The record is written (tmp + fsync + rename) BEFORE every action it
//! describes, so the file always says the most a crash could have done. On the
//! next run the engine reads it and [`recover`] decides, from the state and
//! whether the machine has booted since, what to do about it. The CLI owns the
//! file and the lock; this module owns the rules.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::plan::Plan;
use super::{Component, Outcome};

pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    /// An update exists and was named.
    Discovered,
    /// Its signature and provenance verified (before the pull).
    Verified,
    /// Downloaded and verified again by digest; not yet queued for boot.
    Staged,
    /// Classified and planned against this machine.
    Planned,
    /// The live layer is being built or merged, or activators are running.
    Activating,
    /// Activators ran; the engine is checking the new code is the running code.
    Verifying,
    /// Everything planned is active. Final for this boot.
    Active,
    /// Nothing could activate live now; the staged deployment carries it.
    Deferred,
    /// An activation failed and the engine is undoing it.
    RollingBack,
    /// Undone: the machine runs what it ran before. Final.
    RolledBack,
    /// Failed, and the undo also failed. Final; `rime live doctor` explains.
    Failed,
    /// The machine booted since, or a newer release replaced it. Final.
    Superseded,
}

impl State {
    pub fn is_final(self) -> bool {
        matches!(self, State::Active | State::Deferred | State::RolledBack | State::Failed | State::Superseded)
    }

    /// Does the machine possibly run a half-applied live layer in this state?
    pub fn live_layer_in_flux(self) -> bool {
        matches!(self, State::Activating | State::Verifying | State::RollingBack)
    }

    pub fn may_move_to(self, next: State) -> bool {
        use State::*;
        if next == Superseded {
            // Anything not already final can be overtaken by a reboot or a
            // newer release; that is reported, not prevented.
            return !self.is_final() || self == Active || self == Deferred;
        }
        matches!(
            (self, next),
            (Discovered, Verified)
                | (Discovered, Failed)
                | (Verified, Staged)
                | (Verified, Failed)
                | (Staged, Planned)
                | (Staged, Failed)
                | (Planned, Activating)
                | (Planned, Deferred)
                | (Planned, Active) // nothing to activate: all unchanged or pending
                | (Activating, Verifying)
                | (Activating, RollingBack)
                | (Verifying, Active)
                | (Verifying, RollingBack)
                | (RollingBack, RolledBack)
                | (RollingBack, Failed)
                // A live layer can be removed on request after it is active
                // (`rime live rollback`).
                | (Active, RollingBack)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub state: State,
    /// Seconds since the epoch.
    pub at: u64,
    pub note: String,
}

/// The persistent record of one transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Txn {
    pub schema: u32,
    pub id: String,
    pub boot_id: String,
    pub state: State,
    /// Image digest the machine was running when it started.
    pub booted_digest: String,
    /// Image digest that was verified and staged (empty before staging).
    pub target_digest: String,
    pub target_release: Option<String>,
    pub plan: Option<Plan>,
    /// The live layer this transaction installed, if any, and the one it
    /// replaced, so a rollback can put the previous one back.
    pub layer: Option<String>,
    pub previous_layer: Option<String>,
    pub outcomes: BTreeMap<Component, Outcome>,
    pub history: Vec<Step>,
    /// The staged deployment is queued for the next boot (not download-only).
    #[serde(default)]
    pub staged_for_boot: bool,
    #[serde(default)]
    pub soft_reboot_capable: bool,
}

impl Txn {
    pub fn new(id: &str, boot_id: &str, booted_digest: &str, now: u64) -> Txn {
        Txn {
            schema: SCHEMA,
            id: id.into(),
            boot_id: boot_id.into(),
            state: State::Discovered,
            booted_digest: booted_digest.into(),
            target_digest: String::new(),
            target_release: None,
            plan: None,
            layer: None,
            previous_layer: None,
            outcomes: BTreeMap::new(),
            history: vec![Step { state: State::Discovered, at: now, note: String::new() }],
            staged_for_boot: false,
            soft_reboot_capable: false,
        }
    }

    /// Move to `next`, or refuse with the illegal move named. A refused move
    /// changes nothing.
    pub fn advance(&mut self, next: State, now: u64, note: impl Into<String>) -> Result<(), String> {
        if !self.state.may_move_to(next) {
            return Err(format!("live update {}: {:?} cannot move to {:?}", self.id, self.state, next));
        }
        self.state = next;
        self.history.push(Step { state: next, at: now, note: note.into() });
        Ok(())
    }

    pub fn parse(text: &str) -> Result<Txn, String> {
        let t: Txn = serde_json::from_str(text).map_err(|e| format!("live transaction record: {e}"))?;
        if t.schema != SCHEMA {
            return Err(format!("live transaction record has schema {}, this engine reads {SCHEMA}", t.schema));
        }
        Ok(t)
    }
}

/// What to do about a record found at start-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovery {
    /// Final, or never touched the live system: nothing to do.
    Nothing,
    /// The machine rebooted: the live layer (in /run) is gone with it, and the
    /// booted deployment is whatever the boot chose. Mark it superseded.
    Supersede,
    /// Same boot, interrupted while the live layer was changing: put the
    /// previous layer back (or none) and re-run the activators that the
    /// interrupted transaction ran, so running code matches the files.
    RollBack,
    /// Same boot, interrupted before activation: the staged deployment is
    /// safe as it is (it is either queued or download-only); just close the
    /// record as failed so the next run starts clean.
    Abandon,
}

pub fn recover(t: &Txn, boot_id: &str) -> Recovery {
    if t.state.is_final() {
        if t.boot_id != boot_id && matches!(t.state, State::Active | State::Deferred) {
            return Recovery::Supersede;
        }
        return Recovery::Nothing;
    }
    if t.boot_id != boot_id {
        return Recovery::Supersede;
    }
    if t.state.live_layer_in_flux() {
        Recovery::RollBack
    } else {
        Recovery::Abandon
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(t: &mut Txn, states: &[State]) {
        for (i, s) in states.iter().enumerate() {
            t.advance(*s, i as u64 + 1, "").unwrap();
        }
    }

    #[test]
    fn happy_path_and_history() {
        let mut t = Txn::new("1", "boot-a", "sha256:aa", 0);
        walk(&mut t, &[State::Verified, State::Staged, State::Planned, State::Activating, State::Verifying, State::Active]);
        assert_eq!(t.state, State::Active);
        assert_eq!(t.history.len(), 7);
        assert_eq!(recover(&t, "boot-a"), Recovery::Nothing);
        assert_eq!(recover(&t, "boot-b"), Recovery::Supersede);
    }

    #[test]
    fn illegal_moves_are_refused_and_change_nothing() {
        let mut t = Txn::new("1", "b", "d", 0);
        // Activation without verification and staging.
        assert!(t.advance(State::Activating, 1, "").is_err());
        assert!(t.advance(State::Staged, 1, "").is_err());
        assert_eq!(t.state, State::Discovered);
        assert_eq!(t.history.len(), 1);
        walk(&mut t, &[State::Verified, State::Staged, State::Planned, State::Activating]);
        // A failed activation cannot claim success or skip the undo.
        assert!(t.advance(State::Active, 9, "").is_err());
        assert!(t.advance(State::RolledBack, 9, "").is_err());
        walk(&mut t, &[State::RollingBack, State::RolledBack]);
        assert!(t.advance(State::Activating, 9, "").is_err());
        assert!(t.advance(State::Superseded, 9, "").is_err());
    }

    #[test]
    fn crash_recovery_by_state_and_boot() {
        let mut t = Txn::new("1", "b", "d", 0);
        walk(&mut t, &[State::Verified, State::Staged]);
        assert_eq!(recover(&t, "b"), Recovery::Abandon);
        walk(&mut t, &[State::Planned, State::Activating]);
        assert_eq!(recover(&t, "b"), Recovery::RollBack);
        assert_eq!(recover(&t, "other"), Recovery::Supersede);
        walk(&mut t, &[State::Verifying]);
        assert_eq!(recover(&t, "b"), Recovery::RollBack);
        t.advance(State::RollingBack, 9, "").unwrap();
        assert_eq!(recover(&t, "b"), Recovery::RollBack);
    }

    #[test]
    fn record_round_trips_and_rejects_other_schemas() {
        let mut t = Txn::new("1", "b", "d", 0);
        t.advance(State::Verified, 1, "signature ok").unwrap();
        t.outcomes.insert(Component::Shell, Outcome::Unchanged);
        let s = serde_json::to_string(&t).unwrap();
        let back = Txn::parse(&s).unwrap();
        assert_eq!(back.state, State::Verified);
        assert_eq!(back.outcomes[&Component::Shell], Outcome::Unchanged);
        let bad = s.replacen("\"schema\":1", "\"schema\":2", 1);
        assert!(Txn::parse(&bad).is_err());
        assert!(Txn::parse("{").is_err());
    }
}
