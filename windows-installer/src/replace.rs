//! Replacing an existing partition: the decision, and the take-over.
//!
//! Pure on purpose, like `plan`: the Windows calls are passed in (the volume
//! facts as data, the lock, dismount, letter removal and the read-back as a
//! `Takeover`), so every row of the decision table below runs in `cargo test`
//! on Linux, where CI runs it. ARCHITECTURE.md, "Exclusivity", has the
//! measurements this rests on.
//!
//! ```text
//!  partition                                   candidates()          install()
//!  ------------------------------------------  --------------------  ----------------------------------
//!  ESP / Microsoft reserved / recovery type    not offered           -
//!  a volume with a filesystem Windows knows    not offered           -
//!  basic data / Linux, no volume, no signature usable                re-probe, write
//!  basic data, ONE RAW volume exactly over it  usable ("replaced")   lock, dismount, re-probe UNDER the
//!    (no filesystem), lettered or not                                lock, remove letter, write, unlock
//!    ... lock fails                            -                     refused "something is using it"
//!    ... signature appears before the lock     -                     refused "holds X data", unlocked,
//!                                                                    letter kept, nothing written
//!  any other volume (several, spanned, an      refused "Windows is   refused
//!    unreadable filesystem, a non-basic type)    using it"
//!  a filesystem/BitLocker/RAID/... signature   refused "holds X"     refused
//!  required-partition attribute (bit 0)        refused               refused
//!  other type, too small, unreadable           refused               refused
//! ```

use crate::plan::{self, PartitionFacts};
use std::io;

/// A Windows volume that overlaps a partition, as far as these rules need to
/// know it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeClaim {
    /// `\\?\Volume{...}\`, the name the lock is taken on.
    pub name: String,
    /// Drive letters and folder mount points, as Windows lists them (`G:\`).
    pub mount_points: Vec<String>,
    /// The filesystem Windows reports; empty when it reported none.
    pub filesystem: String,
    /// Windows said it recognises NO filesystem here (GetVolumeInformationW
    /// failed with ERROR_UNRECOGNIZED_VOLUME, or named the filesystem RAW).
    /// Any other failure to answer is not this, and is refused.
    pub unrecognised: bool,
    /// One extent, on this disk, at exactly the partition's offset and
    /// length: a basic volume and nothing larger (a spanned or striped
    /// volume that merely includes the partition is refused).
    pub exactly_this_partition: bool,
    /// Windows' own description, for refusals.
    pub what: String,
}

impl VolumeClaim {
    /// A filesystem Windows mounts and protects. Not offered at all.
    pub fn recognised_filesystem(&self) -> bool {
        !self.filesystem.is_empty() && !self.filesystem.eq_ignore_ascii_case("RAW")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Not listed: a partition Windows needs, or a filesystem it mounted.
    NotOffered,
    Refused(String),
    /// May be replaced. `release` is the RAW volume Windows keeps over it,
    /// which `take_over` must lock before anything is written.
    Usable { release: Option<VolumeClaim> },
}

fn windows_is_using_it(vols: &[VolumeClaim]) -> String {
    format!(
        "Windows is using it ({}). If it is empty, delete it in Disk Management so it becomes unallocated space, then refresh",
        vols.iter().map(|c| c.what.as_str()).collect::<Vec<_>>().join("; ")
    )
}

/// Whether an existing partition may be replaced by Rime's two. `vols` is
/// every Windows volume that overlaps it; `probe` reads the three places
/// `plan::signature` looks.
pub fn verdict(
    p: &PartitionFacts,
    vols: &[VolumeClaim],
    need: u64,
    probe: impl FnOnce() -> io::Result<Option<&'static str>>,
) -> Verdict {
    if matches!(p.type_guid.as_str(), plan::EFI_SYSTEM | plan::MICROSOFT_RESERVED | plan::WINDOWS_RECOVERY) {
        return Verdict::NotOffered;
    }
    // Windows mounts a filesystem it recognises and writes to it; listing C:
    // or a USB stick as "refused" only invites the question.
    if vols.iter().any(VolumeClaim::recognised_filesystem) {
        return Verdict::NotOffered;
    }
    // Windows keeps a volume object over EVERY basic-data partition, lettered
    // or not; over an empty one it is RAW. That one, and only that shape of
    // it, is released by take_over() under an exclusive lock. Anything else
    // a volume covers is Windows' business.
    let release = match vols {
        [] => None,
        [v] if p.type_guid == plan::WINDOWS_BASIC_DATA && v.unrecognised && v.exactly_this_partition => Some(v.clone()),
        _ => return Verdict::Refused(windows_is_using_it(vols)),
    };
    match p.type_guid.as_str() {
        plan::WINDOWS_BASIC_DATA | plan::LINUX_FILESYSTEM => {}
        other => return Verdict::Refused(format!("its type ({}) is not one Rime replaces", plan::type_name(other))),
    }
    // Bit 0 is "required partition": the platform said it needs this.
    if p.attributes & 1 != 0 {
        return Verdict::Refused("it is marked as required by the platform".to_string());
    }
    if p.length < need {
        return Verdict::Refused(format!("{} is too small; Rime needs {}", plan::human(p.length), plan::human(need)));
    }
    match probe() {
        Ok(None) => Verdict::Usable { release },
        Ok(Some(fs)) => Verdict::Refused(holds(fs)),
        Err(e) => Verdict::Refused(format!("it could not be read to check it is empty ({e})")),
    }
}

fn holds(fs: &str) -> String {
    format!("it holds {fs} data. Rime never overwrites a filesystem; if you no longer need it, delete it in Disk Management, then refresh")
}

/// The Windows calls `take_over` makes, in the order it makes them.
pub trait Takeover {
    /// Proof the lock is held; dropping it releases the lock.
    type Held;
    /// Open the volume and take Windows' exclusive volume lock. Fails when anything
    /// else has the volume open: that is the exclusivity being asked for.
    fn lock(&mut self, volume: &str) -> io::Result<Self::Held>;
    /// Dismount the volume, through the locked handle.
    fn dismount(&mut self, held: &mut Self::Held) -> io::Result<()>;
    /// The signature probe again, from the disk, now.
    fn reprobe(&mut self) -> io::Result<Option<&'static str>>;
    /// Remove one drive letter or folder mount point.
    fn remove_mount_point(&mut self, mount_point: &str) -> io::Result<()>;
}

/// Take the RAW volume over an empty partition away from Windows, and prove
/// it is still empty while nothing else can open it. On success the caller
/// writes while it holds the returned lock. On failure nothing has been
/// written and the lock is already released; the drive letter is removed
/// only once the read-back under the lock found nothing.
pub fn take_over<T: Takeover>(v: &VolumeClaim, t: &mut T) -> Result<T::Held, String> {
    let mut held = t.lock(&v.name).map_err(|e| {
        format!(
            "something is using it (Windows would not give this program the partition's volume {} to itself: {e}). \
             Close whatever has it open, or restart Windows, then try again; nothing was changed",
            v.name
        )
    })?;
    t.dismount(&mut held).map_err(|e| format!("Windows would not dismount the volume {} ({e}); nothing was changed", v.name))?;
    match t.reprobe() {
        Ok(None) => {}
        Ok(Some(fs)) => return Err(format!("{}; it changed after it was checked, and nothing was written", holds(fs))),
        Err(e) => return Err(format!("it could not be read again to check it is still empty ({e}); nothing was changed")),
    }
    for m in &v.mount_points {
        t.remove_mount_point(m)
            .map_err(|e| format!("its drive letter or mount point {m} could not be removed ({e}); nothing was written"))?;
    }
    Ok(held)
}

/// The volumes still overlapping something about to be written, less the
/// one this program holds locked. Only that exact volume is excused: any
/// other overlap still refuses the write.
pub fn foreign<'a>(vols: &'a [VolumeClaim], ours: Option<&str>) -> Vec<&'a VolumeClaim> {
    vols.iter().filter(|v| Some(v.name.as_str()) != ours).collect()
}

/// The candidate line for a usable partition. Says plainly what happens to it.
pub fn usable_text(p: &PartitionFacts, release: Option<&VolumeClaim>, disk: &str) -> String {
    let name = if p.name.is_empty() { "(unnamed)".to_string() } else { format!("{:?}", p.name) };
    let letters = release.map(|v| v.mount_points.join(", ")).unwrap_or_default();
    format!(
        "Empty partition {name} on {disk}. It holds no filesystem; Rime deletes it and puts its own two partitions in exactly its space{}",
        if letters.is_empty() { String::new() } else { format!(" (its drive letter {letters} goes away)") }
    )
}

/// What the confirmation says about the partition being replaced.
pub fn replaces_text(p: &PartitionFacts, release: Option<&VolumeClaim>) -> String {
    let name = if p.name.is_empty() { "(unnamed)".to_string() } else { format!("{:?}", p.name) };
    let letters = release.map(|v| v.mount_points.join(", ")).unwrap_or_default();
    format!(
        "the empty partition {name}, partition GUID {}, {}, bytes {} .. {} of the disk{}",
        p.id,
        plan::human(p.length),
        p.offset,
        p.offset + p.length,
        if letters.is_empty() { ", no drive letter".to_string() } else { format!(", drive letter {letters}") }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const NEED: u64 = 20_000_000_000;
    const BIG: u64 = 36_507_222_016;
    const VOL: &str = "\\\\?\\Volume{49588608-0000-0000-0000-000000000000}\\";

    fn part(ty: &str, length: u64, attributes: u64) -> PartitionFacts {
        PartitionFacts {
            id: "49588608-1111-2222-3333-444455556666".into(),
            type_guid: ty.into(),
            name: "Basic data partition".into(),
            offset: 30_063_722_496,
            length,
            attributes,
        }
    }
    fn raw(letters: &[&str]) -> VolumeClaim {
        VolumeClaim {
            name: VOL.into(),
            mount_points: letters.iter().map(|s| s.to_string()).collect(),
            filesystem: String::new(),
            unrecognised: true,
            exactly_this_partition: true,
            what: "unrecognised filesystem".into(),
        }
    }
    fn ntfs() -> VolumeClaim {
        VolumeClaim { filesystem: "NTFS".into(), unrecognised: false, ..raw(&["E:\\"]) }
    }
    fn empty() -> io::Result<Option<&'static str>> {
        Ok(None)
    }
    fn release_of(v: Verdict) -> Option<VolumeClaim> {
        match v {
            Verdict::Usable { release } => release,
            other => panic!("expected usable, got {other:?}"),
        }
    }
    fn refusal(v: Verdict) -> String {
        match v {
            Verdict::Refused(r) => r,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// Records every call, and answers them as told.
    #[derive(Default)]
    struct Fake {
        calls: RefCell<Vec<String>>,
        lock_fails: bool,
        /// What the disk holds by the time it is read under the lock.
        under_lock: Option<&'static str>,
    }
    impl Takeover for Fake {
        type Held = ();
        fn lock(&mut self, v: &str) -> io::Result<()> {
            self.calls.borrow_mut().push(format!("lock {v}"));
            if self.lock_fails { Err(io::Error::from_raw_os_error(5)) } else { Ok(()) }
        }
        fn dismount(&mut self, _: &mut ()) -> io::Result<()> {
            self.calls.borrow_mut().push("dismount".into());
            Ok(())
        }
        fn reprobe(&mut self) -> io::Result<Option<&'static str>> {
            self.calls.borrow_mut().push("reprobe".into());
            Ok(self.under_lock)
        }
        fn remove_mount_point(&mut self, m: &str) -> io::Result<()> {
            self.calls.borrow_mut().push(format!("unmount {m}"));
            Ok(())
        }
    }
    fn calls(f: &Fake) -> Vec<String> {
        f.calls.borrow().clone()
    }

    #[test]
    fn raw_without_a_letter_is_allowed_and_names_the_volume_to_lock() {
        let r = release_of(verdict(&part(plan::WINDOWS_BASIC_DATA, BIG, 0), &[raw(&[])], NEED, empty));
        assert_eq!(r.as_ref().map(|v| v.name.as_str()), Some(VOL), "the RAW volume must be handed to take_over");
        let mut f = Fake::default();
        assert!(take_over(r.as_ref().unwrap(), &mut f).is_ok());
        assert_eq!(calls(&f), [format!("lock {VOL}"), "dismount".into(), "reprobe".into()]);
    }

    #[test]
    fn raw_with_a_letter_is_allowed_through_the_lock_and_loses_its_letter_last() {
        let r = release_of(verdict(&part(plan::WINDOWS_BASIC_DATA, BIG, 0), &[raw(&["G:\\"])], NEED, empty)).unwrap();
        let mut f = Fake::default();
        assert!(take_over(&r, &mut f).is_ok());
        assert_eq!(calls(&f), [format!("lock {VOL}"), "dismount".into(), "reprobe".into(), "unmount G:\\".into()]);
    }

    #[test]
    fn a_failed_lock_refuses_by_name_and_touches_nothing_else() {
        let mut f = Fake { lock_fails: true, ..Fake::default() };
        let e = take_over(&raw(&["G:\\"]), &mut f).unwrap_err();
        assert!(e.contains("something is using it"), "{e}");
        assert_eq!(calls(&f), [format!("lock {VOL}")], "no dismount, read or letter removal after a failed lock");
    }

    #[test]
    fn a_signature_that_appears_after_the_survey_is_caught_under_the_lock() {
        // The survey read the partition empty; by the time of the lock someone
        // had formatted it.
        let r = release_of(verdict(&part(plan::WINDOWS_BASIC_DATA, BIG, 0), &[raw(&["G:\\"])], NEED, empty)).unwrap();
        let mut f = Fake { under_lock: Some("NTFS"), ..Fake::default() };
        let e = take_over(&r, &mut f).unwrap_err();
        assert!(e.contains("holds NTFS data") && e.contains("nothing was written"), "{e}");
        assert!(!calls(&f).iter().any(|c| c.starts_with("unmount")), "the letter must stay: {:?}", calls(&f));
        // ...and the re-probe really ran after the lock, not before it.
        let c = calls(&f);
        assert!(c.iter().position(|x| x == "reprobe") > c.iter().position(|x| x.starts_with("lock")), "{c:?}");
    }

    #[test]
    fn a_recognised_filesystem_is_never_offered_and_its_signature_is_refused() {
        assert_eq!(verdict(&part(plan::WINDOWS_BASIC_DATA, BIG, 0), &[ntfs()], NEED, empty), Verdict::NotOffered);
        let fat = VolumeClaim { filesystem: "FAT32".into(), unrecognised: false, ..raw(&[]) };
        assert_eq!(verdict(&part(plan::WINDOWS_BASIC_DATA, BIG, 0), &[fat], NEED, empty), Verdict::NotOffered);
        // RAW to Windows, but the bytes say NTFS (or BitLocker): refused.
        for fs in ["NTFS", "BitLocker", "exFAT", "ReFS", "FAT32"] {
            let r = refusal(verdict(&part(plan::WINDOWS_BASIC_DATA, BIG, 0), &[raw(&[])], NEED, || Ok(Some(fs))));
            assert!(r.contains(&format!("holds {fs} data")) && r.contains("Disk Management"), "{r}");
        }
    }

    #[test]
    fn required_attribute_type_size_and_read_errors_are_refused() {
        let r = refusal(verdict(&part(plan::WINDOWS_BASIC_DATA, BIG, 1), &[raw(&[])], NEED, empty));
        assert!(r.contains("required"), "{r}");
        let r = refusal(verdict(&part(plan::WINDOWS_BASIC_DATA, NEED - 1, 0), &[raw(&[])], NEED, empty));
        assert!(r.contains("too small"), "{r}");
        let r = refusal(verdict(&part("00000000-1111-2222-3333-444444444444", BIG, 0), &[], NEED, empty));
        assert!(r.contains("not one Rime replaces"), "{r}");
        let r = refusal(verdict(&part(plan::LINUX_FILESYSTEM, BIG, 0), &[], NEED, || Err(io::Error::other("bad sector"))));
        assert!(r.contains("could not be read"), "{r}");
        // Other attributes (hidden, no drive letter) are Windows' bookkeeping.
        assert!(matches!(verdict(&part(plan::WINDOWS_BASIC_DATA, BIG, 0x8000_0000_0000_0000), &[raw(&[])], NEED, empty), Verdict::Usable { .. }));
    }

    #[test]
    fn partitions_windows_needs_are_not_offered() {
        for t in [plan::EFI_SYSTEM, plan::MICROSOFT_RESERVED, plan::WINDOWS_RECOVERY] {
            assert_eq!(verdict(&part(t, BIG, 0), &[], NEED, empty), Verdict::NotOffered, "{t}");
            assert_eq!(verdict(&part(t, BIG, 0), &[raw(&[])], NEED, empty), Verdict::NotOffered, "{t}");
        }
    }

    #[test]
    fn only_one_raw_volume_exactly_over_a_basic_data_partition_is_released() {
        let p = part(plan::WINDOWS_BASIC_DATA, BIG, 0);
        // Windows could not say what the filesystem is, for a reason other
        // than "none": not RAW, so not ours to lock.
        let unknown = VolumeClaim { unrecognised: false, ..raw(&[]) };
        // A spanned/striped volume that includes the partition.
        let spanned = VolumeClaim { exactly_this_partition: false, ..raw(&[]) };
        let two = [raw(&[]), VolumeClaim { name: "\\\\?\\Volume{other}\\".into(), ..raw(&[]) }];
        for vols in [vec![unknown], vec![spanned], two.to_vec()] {
            let r = refusal(verdict(&p, &vols, NEED, empty));
            assert!(r.starts_with("Windows is using it"), "{r}");
        }
        // A volume over a Linux-typed partition is not the shape Windows makes.
        let r = refusal(verdict(&part(plan::LINUX_FILESYSTEM, BIG, 0), &[raw(&[])], NEED, empty));
        assert!(r.starts_with("Windows is using it"), "{r}");
        // No volume at all: usable, nothing to lock (the pre-existing path).
        assert_eq!(verdict(&part(plan::LINUX_FILESYSTEM, BIG, 0), &[], NEED, empty), Verdict::Usable { release: None });
    }

    #[test]
    fn the_write_time_check_excuses_only_the_volume_this_program_locked() {
        let other = VolumeClaim { name: "\\\\?\\Volume{other}\\".into(), ..raw(&[]) };
        let vols = [raw(&[]), other.clone()];
        assert_eq!(foreign(&vols, Some(VOL)), vec![&other]);
        assert_eq!(foreign(&vols, None).len(), 2);
        assert!(foreign(&[raw(&[])], Some(VOL)).is_empty());
    }

    #[test]
    fn the_words_say_the_partition_is_replaced_and_name_it() {
        let p = part(plan::WINDOWS_BASIC_DATA, BIG, 0);
        let line = usable_text(&p, Some(&raw(&["G:\\"])), "RIME-LAB-SYSTEM (serial SYS1)");
        assert!(line.starts_with("Empty partition \"Basic data partition\""), "{line}");
        assert!(line.contains("Rime deletes it") && line.contains("G:\\"), "{line}");
        let c = replaces_text(&p, Some(&raw(&[])));
        for must in ["empty partition", "49588608-1111-2222-3333-444455556666", "36.5 GB", "30063722496", "no drive letter"] {
            assert!(c.contains(must), "{must} missing: {c}");
        }
        assert!(line.is_ascii() && c.is_ascii());
        let disk = plan::DiskIdentity {
            model: "RIME-LAB-SYSTEM".into(),
            serial: "SYS0000000001".into(),
            bus: "SATA".into(),
            length: 68_719_476_736,
            sector_size: 512,
            gpt_disk_guid: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(),
        };
        let conf = plan::install_confirmation(&disk, &line, p.length, Some(&c));
        assert!(conf.contains("This REPLACES the empty partition") && conf.contains(&p.id), "{conf}");
        let free = plan::install_confirmation(&disk, "Unallocated space", p.length, None);
        assert!(!free.contains("REPLACES"), "{free}");
    }
}
