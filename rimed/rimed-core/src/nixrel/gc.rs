//! Which system generations `rime gc` keeps (docs/nixos/updates-and-release.md §11.3).
//!
//! The caller lists the system profile's generations, resolves the booted and current ones, reads the pins and the
//! health record, then deletes exactly [`GcPlan::delete`] BY NUMBER (`nix-env -p … --delete-generations <n> …`),
//! never with `old` or `-d`. The paths of an interrupted download are protected separately (temporary roots for the
//! length of the `nix-store --gc` run), since they belong to no generation yet.

use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcInputs {
    /// Every generation number in the system profile.
    pub generations: BTreeSet<u32>,
    /// The profile's current generation (what boots next).
    pub current: u32,
    /// The generation `/run/booted-system` resolves to; `None` = it matches none in the profile.
    pub booted: Option<u32>,
    /// Pinned generations (`/var/lib/rime/nix/pins.json`).
    pub pinned: BTreeSet<u32>,
    /// The generation the last update started from (`LastUpdate.from_digest` resolved to a number), kept until the
    /// health record shows the machine ran the new one.
    pub from: Option<u32>,
    /// The health record shows the machine booted into (boot-class) or ran (switch-class) the current generation.
    pub health_confirmed: bool,
    /// How many of the newest generations to keep in any case (at least 2: current and previous).
    pub keep_newest: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcPlan {
    pub keep: BTreeSet<u32>,
    pub delete: Vec<u32>,
    /// Set when the plan refuses to delete anything, with the reason.
    pub refused: Option<String>,
}

pub fn gc_plan(i: &GcInputs) -> GcPlan {
    let refuse = |why: &str| GcPlan { keep: i.generations.clone(), delete: Vec::new(), refused: Some(why.to_string()) };
    if !i.generations.contains(&i.current) {
        return refuse("the profile's current generation is not in its own list");
    }
    let Some(booted) = i.booted else {
        // The booted generation must never be deleted; if it cannot be named, nothing is.
        return refuse("the running system's generation could not be identified");
    };
    let mut keep: BTreeSet<u32> = BTreeSet::new();
    keep.insert(i.current);
    keep.insert(booted);
    // The rollback target: the newest generation older than the current one, and the newest older than the booted
    // one (what `rime rollback` picks, §11.1).
    for anchor in [i.current, booted] {
        if let Some(prev) = i.generations.range(..anchor).next_back() {
            keep.insert(*prev);
        }
    }
    keep.extend(i.pinned.iter().filter(|g| i.generations.contains(g)));
    keep.extend(i.generations.iter().rev().take(i.keep_newest.max(2)));
    if !i.health_confirmed {
        if let Some(from) = i.from.filter(|g| i.generations.contains(g)) {
            keep.insert(from);
        }
    }
    let delete = i.generations.iter().copied().filter(|g| !keep.contains(g)).collect();
    GcPlan { keep, delete, refused: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> GcInputs {
        GcInputs {
            generations: (1..=9).collect(),
            current: 9,
            booted: Some(9),
            pinned: BTreeSet::new(),
            from: Some(8),
            health_confirmed: true,
            keep_newest: 2,
        }
    }

    #[test]
    fn keeps_current_previous_and_deletes_the_rest_by_number() {
        let p = gc_plan(&base());
        assert_eq!(p.keep, [8, 9].into());
        assert_eq!(p.delete, vec![1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn never_deletes_the_booted_generation_or_its_rollback_target() {
        let mut i = base();
        i.booted = Some(4); // a pending update (current 9) on a machine still running generation 4
        let p = gc_plan(&i);
        assert!(p.keep.contains(&4) && p.keep.contains(&3) && p.keep.contains(&9) && p.keep.contains(&8));
        assert!(!p.delete.contains(&4));
        i.booted = None;
        let p = gc_plan(&i);
        assert!(p.delete.is_empty() && p.refused.is_some());
    }

    #[test]
    fn keeps_pins_and_the_from_generation_until_health_confirms() {
        let mut i = base();
        i.pinned = [2].into();
        i.from = Some(5);
        i.health_confirmed = false;
        let p = gc_plan(&i);
        assert!(p.keep.contains(&2) && p.keep.contains(&5));
        i.health_confirmed = true;
        assert!(gc_plan(&i).delete.contains(&5));
    }

    #[test]
    fn keep_newest_and_never_keeps_nothing() {
        let mut i = base();
        i.keep_newest = 4;
        assert_eq!(gc_plan(&i).keep, [6, 7, 8, 9].into());
        i.keep_newest = 0;
        i.generations = [1].into();
        i.current = 1;
        i.booted = Some(1);
        let p = gc_plan(&i);
        assert_eq!(p.keep, [1].into());
        assert!(p.delete.is_empty());
    }
}
