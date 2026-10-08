//! The decision layer: what the program is willing to do, why, and the words
//! it says before it does anything irreversible.
//!
//! Everything here is platform-neutral and pure, which is the point. The
//! Windows API layer next door cannot be unit-tested anywhere this repository
//! builds; the rules that decide whether a partition may be erased can be, and
//! are, on every CI run. A rule that only executes on a machine nobody in this
//! program owns is a rule nobody has ever checked.

use std::fmt;

/// The GPT type GUIDs this program knows by name.
pub const LINUX_FILESYSTEM: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
pub const WINDOWS_BASIC_DATA: &str = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7";
pub const EFI_SYSTEM: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
pub const MICROSOFT_RESERVED: &str = "e3c9e316-0b5c-4db8-817d-f92df00215ae";
pub const WINDOWS_RECOVERY: &str = "de94bba4-06d1-4d40-a16a-bfd50179d6ac";

/// 16 decimal GB, borrowed from `installer/rime-install` and applied to a
/// different thing.
///
/// That installer checks `$DISK` — in BOTH of its modes, whole-disk and
/// partition (`installer/rime-install:849-855`), so in partition mode it never
/// sizes the target partition at all. A 40 GB disk with a 2 GB free partition
/// passes its check and still cannot hold Rime. This program applies the same
/// number to the partition, which is the thing that has to hold the operating
/// system, and is therefore **stricter than the Linux installer**, not equal
/// to it. Worth knowing before anyone "aligns" the two.
pub const MINIMUM_ROOT_BYTES: u64 = 16 * 1000 * 1000 * 1000;

pub fn type_name(guid: &str) -> &'static str {
    match guid {
        LINUX_FILESYSTEM => "Linux filesystem",
        WINDOWS_BASIC_DATA => "Windows basic data",
        EFI_SYSTEM => "EFI system partition",
        MICROSOFT_RESERVED => "Microsoft reserved",
        WINDOWS_RECOVERY => "Windows recovery",
        _ => "unrecognised type",
    }
}

/// How a disk is named to a person. Never contains a device index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskIdentity {
    pub model: String,
    pub serial: String,
    pub bus: String,
    pub length: u64,
    pub sector_size: u32,
    pub gpt_disk_guid: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionFacts {
    pub id: String,
    pub type_guid: String,
    pub name: String,
    pub offset: u64,
    pub length: u64,
    pub attributes: u64,
}

/// Something Windows is doing with the bytes under consideration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub what: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Every rule that can be decided without reading the content passed.
    /// Content still has to be scanned; this is not permission.
    ContentCheckAllowed,
    Refused(Refusal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub rule: &'static str,
    pub detail: String,
    /// What the user could legitimately do about it, when there is something.
    /// Never an offer to do it for them.
    pub remedy: Option<String>,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "REFUSED ({}): {}", self.rule, self.detail)?;
        if let Some(r) = &self.remedy {
            write!(f, "\n  What you can do: {r}")?;
        }
        Ok(())
    }
}

fn refuse(rule: &'static str, detail: String, remedy: Option<String>) -> Verdict {
    Verdict::Refused(Refusal { rule, detail, remedy })
}

/// Decide whether a partition may even be *considered*, before a single byte
/// of its content is read.
///
/// The order matters and is not arbitrary. Ownership is checked before type
/// and before size, because a partition Windows is using is the one whose
/// erasure ends the session, and a refusal a user reads should name the worst
/// reason first rather than the first alphabetically.
pub fn assess(p: &PartitionFacts, claims: &[Claim]) -> Verdict {
    if !claims.is_empty() {
        return refuse(
            "in use by Windows",
            format!(
                "Windows currently has this partition: {}",
                claims.iter().map(|c| c.what.as_str()).collect::<Vec<_>>().join("; ")
            ),
            Some(
                "choose a partition Windows is not using. This program will not \
                 dismount, offline or force a volume closed."
                    .to_string(),
            ),
        );
    }

    match p.type_guid.as_str() {
        LINUX_FILESYSTEM => {}
        EFI_SYSTEM | MICROSOFT_RESERVED | WINDOWS_RECOVERY => {
            return refuse(
                "protected partition type",
                format!(
                    "this is a {} ({}). Erasing it breaks the machine's ability to start.",
                    type_name(&p.type_guid),
                    p.type_guid
                ),
                None,
            );
        }
        WINDOWS_BASIC_DATA => {
            // The honest one. This is exactly what Disk Management produces
            // when a user shrinks C: and creates a new volume, so it is the
            // partition most users will actually offer — and this program
            // refuses it on purpose rather than quietly widening the rule.
            //
            // A basic-data partition is a partition Windows believes belongs
            // to Windows: it will assign it a drive letter, mount it, and
            // write to it the moment it recognises a filesystem there. Zero
            // bytes today is not the same claim as "Windows has released it".
            return refuse(
                "Windows-owned partition type",
                format!(
                    "this is a {} ({}). Windows treats basic-data partitions as its own \
                     and will mount and write to one as soon as it recognises a filesystem, \
                     including between this check and the install.",
                    type_name(&p.type_guid),
                    p.type_guid
                ),
                Some(format!(
                    "in an elevated diskpart, select this partition and run BOTH of \
                     `set id={LINUX_FILESYSTEM}` (hands the partition to Linux) and \
                     `gpt attributes=0x0000000000000000` (clears the no-drive-letter \
                     flag Windows puts on partitions it creates), then run this program \
                     again. Both commands are needed: `set id=` changes only the type \
                     GUID and leaves the attribute bit in place, which this program \
                     refuses separately -- measured in the lab, not assumed. That is a \
                     deliberate, reversible act by you; this program will not retype a \
                     partition on your behalf."
                )),
            );
        }
        other => {
            return refuse(
                "unrecognised partition type",
                format!(
                    "type {other} is not one this program knows. Unknown state is refused, \
                     not guessed at."
                ),
                None,
            );
        }
    }

    if p.attributes != 0 {
        return refuse(
            "partition attributes set",
            format!(
                "GPT attributes {:#018x} are set on this partition. Required-partition, \
                 no-drive-letter, hidden and read-only flags all mean somebody marked \
                 these bytes as special.",
                p.attributes
            ),
            None,
        );
    }

    if p.length < MINIMUM_ROOT_BYTES {
        return refuse(
            "too small",
            format!(
                "{} is below the {} Rime needs. The Linux installer refuses the same \
                 number.",
                human(p.length),
                human(MINIMUM_ROOT_BYTES)
            ),
            None,
        );
    }

    Verdict::ContentCheckAllowed
}

/// Which filesystem, volume or container signature, if any, sits at the
/// start, at 64 MiB, or at the end of a partition. `first` is its first MiB,
/// `at64` the 64 KiB from 64 MiB on (empty when the partition is smaller),
/// `last` its last MiB.
///
/// This is how "empty" is decided for a partition the user offers, and it is
/// deliberately not "every byte is zero". A partition made by shrinking C:
/// and creating a new one sits on clusters NTFS used a minute ago, so an
/// all-zero rule refuses nearly every real partition a person would offer,
/// and teaches them to wipe things. What must never be overwritten is a
/// filesystem, an encrypted volume or a member of something larger, and each
/// of those announces itself where blkid looks for it: these are its
/// offsets for every type it knows that can live on a Windows machine.
pub fn signature(first: &[u8], at64: &[u8], last: &[u8]) -> Option<&'static str> {
    let at = |b: &[u8], o: usize, m: &[u8]| b.len() >= o + m.len() && &b[o..o + m.len()] == m;
    let checks: [(&[u8], usize, &[u8], &'static str); 17] = [
        (first, 3, b"NTFS    ", "NTFS"),
        (first, 3, b"-FVE-FS-", "BitLocker"),
        (first, 3, b"EXFAT   ", "exFAT"),
        (first, 82, b"FAT32   ", "FAT32"),
        (first, 54, b"FAT1", "FAT"),
        (first, 3, b"ReFS", "ReFS"),
        (first, 0, b"LUKS\xba\xbe", "LUKS"),
        (first, 0, &[0xfc, 0x4e, 0x2b, 0xa9], "Linux RAID"),
        (first, 1080, &[0x53, 0xef], "ext2/3/4"),
        (first, 0, b"XFSB", "XFS"),
        (first, 4086, b"SWAPSPACE2", "Linux swap"),
        (first, 536, b"LVM2 001", "LVM"),
        (first, 32769, b"CD001", "ISO 9660"),
        (first, 512, b"EFI PART", "a nested GPT"),
        (first, 1024, b"\x10\x20\xf5\xf2", "F2FS"),
        (at64, 64, b"_BHRfS_M", "btrfs"),
        (first, 4096, &[0xfc, 0x4e, 0x2b, 0xa9], "Linux RAID"),
    ];
    for (b, o, m, name) in checks {
        if at(b, o, m) {
            return Some(name);
        }
    }
    // MD RAID 0.90/1.0 keep their superblock near the END.
    // Superblocks are 4 KiB aligned there, so only those offsets are read: a
    // match anywhere in a megabyte of old data would refuse partitions at
    // random.
    if last.chunks(4096).any(|c| c.starts_with(&[0xfc, 0x4e, 0x2b, 0xa9])) {
        return Some("Linux RAID");
    }
    // An MBR boot signature with a partition table behind it.
    if at(first, 510, &[0x55, 0xaa]) && first.len() >= 512 && first[446..510].iter().any(|v| *v != 0) {
        return Some("a nested partition table");
    }
    None
}

/// The two enumerations must agree. One side is Windows'
/// `IOCTL_DISK_GET_DRIVE_LAYOUT_EX`; the other is this crate reading the GPT
/// bytes off the same handle. They are produced by entirely different code
/// and a disagreement means one of them is describing a disk that is not
/// there — which is the moment to stop, not to pick a winner.
pub fn cross_check(
    from_gpt: &[PartitionFacts],
    from_windows: &[PartitionFacts],
) -> Result<(), String> {
    let key = |p: &PartitionFacts| {
        (p.id.clone(), p.type_guid.clone(), p.offset, p.length, p.attributes)
    };
    let mut a: Vec<_> = from_gpt.iter().map(key).collect();
    let mut b: Vec<_> = from_windows.iter().map(key).collect();
    a.sort();
    b.sort();
    if a == b {
        return Ok(());
    }
    let only_gpt: Vec<_> = a.iter().filter(|k| !b.contains(k)).collect();
    let only_win: Vec<_> = b.iter().filter(|k| !a.contains(k)).collect();
    Err(format!(
        "the partition table read from the disk and the one Windows reports disagree.\n  \
         only in the on-disk GPT: {only_gpt:?}\n  only in Windows' view: {only_win:?}"
    ))
}

/// Decimal units, because that is what is printed on the drive.
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "kB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} bytes") } else { format!("{v:.1} {}", UNITS[u]) }
}

/// What to do when there is nowhere to put Rime. Windows' own Disk
/// Management shrinks a volume safely (it moves files, it knows NTFS); this
/// program never touches NTFS, so the shrink is the user's, with Windows'
/// tool, and the installer takes it from there.
pub const NO_SPACE_HELP: &str = "\
There is no free space for Rime OS yet. To make some:
  1. Open Disk Management (right-click Start, then Disk Management).
  2. Right-click your C: drive and choose Shrink Volume.
  3. Shrink it by at least 30000 MB (more if you can) and click Shrink.
  4. Leave the new space Unallocated. Do not create a volume in it.
  5. Come back here and refresh.";

/// The confirmation for installing into a chosen space. Names the disk by
/// model, serial and size, never by number (see `confirmation_text`).
pub fn install_confirmation(disk: &DiskIdentity, what: &str, bytes: u64) -> String {
    format!(
        "INSTALL RIME OS -- read this before continuing.\n\n\
         Rime will be installed into:\n\
         \x20   {what}\n\
         \x20   size             {size}\n\
         \x20   disk             {model}\n\
         \x20   serial number    {serial}\n\
         \x20   disk GUID        {dguid}\n\n\
         This creates two partitions there: Rime's own boot partition and Rime's \
         system partition. Anything in that space is overwritten. Windows, its \
         partitions and its boot files are not changed.\n\n\
         Then the computer starts Rime's installer ONCE, at the next restart, \
         where you create your account. If you restart and do nothing, nothing \
         else changes, and Windows starts as before the time after.\n\n\
         If the disk above is not the one you meant, stop. Disk numbers change \
         between restarts; the serial number does not.",
        size = human(bytes),
        model = if disk.model.is_empty() { "(the disk reports no model)" } else { &disk.model },
        serial = if disk.serial.is_empty() { "(the disk reports no serial number)" } else { &disk.serial },
        dguid = disk.gpt_disk_guid,
    )
}

/// The words shown on the screen where the user commits.
///
/// Every fact in here survives a reboot and a controller reorder: model,
/// serial, partition GUID, byte offset, byte length. The device index the
/// program used to reach the disk appears nowhere, and `contains_no_index`
/// below is the test that keeps it that way.
pub fn confirmation_text(
    disk: &DiskIdentity,
    p: &PartitionFacts,
    filesystem_label: Option<&str>,
    scanned_bytes: u64,
) -> String {
    let label = match filesystem_label {
        Some(l) if !l.is_empty() => format!("filesystem label {l:?}"),
        _ => "no filesystem label (none was found, and every byte was checked)".to_string(),
    };
    let name = if p.name.is_empty() { "(unnamed)".to_string() } else { format!("{:?}", p.name) };
    format!(
        "ERASE AND INSTALL -- read this before continuing.\n\
         \n\
         Rime will be written to ONE partition:\n\
         \n\
         \x20   partition name   {name}\n\
         \x20   size             {size}\n\
         \x20   type             {tname}\n\
         \x20   partition GUID   {pid}\n\
         \x20   {label}\n\
         \x20   on disk          {model}\n\
         \x20   serial number    {serial}\n\
         \x20   connected by     {bus}\n\
         \x20   disk GUID        {dguid}\n\
         \x20   byte range       {start} .. {end} on that disk\n\
         \n\
         Every one of the {scanned} bytes in that range was read and every one was \
         zero.\n\
         \n\
         Everything in that range will be destroyed. Nothing else on this machine \
         is touched: no other partition is written, the partition table is not \
         rewritten, and Windows remains the disk's default operating system.\n\
         \n\
         If the disk above is not the one you meant, stop. Disk numbers change \
         between restarts; the serial number does not.",
        name = name,
        size = human(p.length),
        tname = type_name(&p.type_guid),
        pid = p.id,
        label = label,
        model = if disk.model.is_empty() { "(the disk reports no model)" } else { &disk.model },
        serial =
            if disk.serial.is_empty() { "(the disk reports no serial number)" } else { &disk.serial },
        bus = disk.bus,
        dguid = disk.gpt_disk_guid,
        start = p.offset,
        end = p.offset + p.length,
        scanned = scanned_bytes,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(type_guid: &str, length: u64, attributes: u64) -> PartitionFacts {
        PartitionFacts {
            id: "11111111-2222-3333-4444-555555555555".into(),
            type_guid: type_guid.into(),
            name: "RIME-TARGET".into(),
            offset: 1_048_576,
            length,
            attributes,
        }
    }
    fn disk() -> DiskIdentity {
        DiskIdentity {
            model: "RIME-FIXTURE-A".into(),
            serial: "FIXA00000001".into(),
            bus: "SATA".into(),
            length: 40_000_000_000,
            sector_size: 512,
            gpt_disk_guid: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(),
        }
    }
    const BIG: u64 = 20 * 1000 * 1000 * 1000;

    #[test]
    fn a_partition_windows_is_using_is_refused_before_anything_else() {
        // Also wrong type, also too small: ownership must still be the reason
        // reported, because it is the one that ends someone's session.
        let p = part(EFI_SYSTEM, 1024, 0);
        let claims = vec![Claim { what: "mounted at C:\\".into() }];
        match assess(&p, &claims) {
            Verdict::Refused(r) => {
                assert_eq!(r.rule, "in use by Windows");
                assert!(r.detail.contains("C:\\"), "the refusal must name the mount: {r}");
            }
            other => panic!("expected refusal, got {other:?}"),
        }
    }

    #[test]
    fn every_windows_owned_type_is_refused_even_when_empty_and_large() {
        for t in [EFI_SYSTEM, MICROSOFT_RESERVED, WINDOWS_RECOVERY, WINDOWS_BASIC_DATA] {
            match assess(&part(t, BIG, 0), &[]) {
                Verdict::Refused(_) => {}
                other => panic!("{t} must be refused, got {other:?}"),
            }
        }
    }

    #[test]
    fn the_basic_data_refusal_tells_the_user_what_they_may_do_about_it() {
        // This is the partition a user actually creates by shrinking C:. If
        // the refusal is a dead end the tool is useless to them, and if it
        // offers to retype the partition itself the tool is dangerous.
        match assess(&part(WINDOWS_BASIC_DATA, BIG, 0), &[]) {
            Verdict::Refused(r) => {
                let remedy = r.remedy.expect("basic data must carry a remedy");
                assert!(remedy.contains("diskpart"), "{remedy}");
                assert!(remedy.contains(LINUX_FILESYSTEM), "{remedy}");
                // BOTH diskpart commands. `set id=` alone leaves GPT attribute
                // bit 63 in place on every partition Windows made, and this
                // program refuses that separately -- so a one-command remedy
                // walks the user out of one refusal and straight into another.
                // Measured in the guest: lab/jobs/diskpart-remedy.
                assert!(
                    remedy.contains("gpt attributes=0x0000000000000000"),
                    "the remedy must clear the attributes too, or it is a dead end: {remedy}"
                );
                assert!(
                    remedy.contains("will not retype"),
                    "the remedy must be the user's act, not ours: {remedy}"
                );
            }
            other => panic!("expected refusal, got {other:?}"),
        }
    }

    #[test]
    fn attributes_and_size_are_refused_with_their_own_rules() {
        match assess(&part(LINUX_FILESYSTEM, BIG, 1), &[]) {
            Verdict::Refused(r) => assert_eq!(r.rule, "partition attributes set"),
            other => panic!("{other:?}"),
        }
        match assess(&part(LINUX_FILESYSTEM, MINIMUM_ROOT_BYTES - 1, 0), &[]) {
            Verdict::Refused(r) => assert_eq!(r.rule, "too small"),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            assess(&part(LINUX_FILESYSTEM, MINIMUM_ROOT_BYTES, 0), &[]),
            Verdict::ContentCheckAllowed
        );
    }

    #[test]
    fn allowed_is_not_permission_to_write() {
        // Guards the vocabulary. The success arm is called
        // ContentCheckAllowed and not `Eligible` or `Ok` because everything it
        // has established is that the content may now be READ.
        let v = assess(&part(LINUX_FILESYSTEM, BIG, 0), &[]);
        assert_eq!(v, Verdict::ContentCheckAllowed);
    }

    #[test]
    fn cross_check_catches_a_partition_only_one_side_can_see() {
        let a = part(LINUX_FILESYSTEM, BIG, 0);
        let mut b = a.clone();
        assert!(cross_check(std::slice::from_ref(&a), std::slice::from_ref(&b)).is_ok());
        b.offset += 512;
        let e = cross_check(std::slice::from_ref(&a), std::slice::from_ref(&b)).unwrap_err();
        assert!(e.contains("disagree"), "{e}");
        assert!(cross_check(std::slice::from_ref(&a), &[]).is_err());
        assert!(cross_check(&[], std::slice::from_ref(&a)).is_err());
    }

    #[test]
    fn cross_check_does_not_care_about_ordering() {
        let mut p2 = part(LINUX_FILESYSTEM, BIG, 0);
        p2.id = "99999999-8888-7777-6666-555555555555".into();
        p2.offset = 99 * 1024 * 1024;
        let a = vec![part(LINUX_FILESYSTEM, BIG, 0), p2.clone()];
        let b = vec![p2, part(LINUX_FILESYSTEM, BIG, 0)];
        assert!(cross_check(&a, &b).is_ok());
    }

    #[test]
    fn the_confirmation_names_the_disk_by_serial_and_never_by_index() {
        let text = confirmation_text(&disk(), &part(LINUX_FILESYSTEM, BIG, 0), None, BIG);
        for must in ["FIXA00000001", "RIME-FIXTURE-A", "11111111-2222-3333-4444-555555555555"] {
            assert!(text.contains(must), "the confirmation must contain {must}:\n{text}");
        }
        // The load-bearing negative. Index-based identification is how people
        // erase the wrong drive.
        for must_not in ["PhysicalDrive", "Disk 0", "Disk 1", "drive 0", "drive 1"] {
            assert!(
                !text.contains(must_not),
                "the confirmation must never identify a target by index, found {must_not:?}:\n{text}"
            );
        }
        assert!(text.contains("Disk numbers change"), "it must say why:\n{text}");
    }

    #[test]
    fn the_confirmation_says_no_label_rather_than_leaving_the_line_out() {
        let with = confirmation_text(&disk(), &part(LINUX_FILESYSTEM, BIG, 0), Some("DATA"), BIG);
        assert!(with.contains("filesystem label \"DATA\""), "{with}");
        let without = confirmation_text(&disk(), &part(LINUX_FILESYSTEM, BIG, 0), None, BIG);
        assert!(without.contains("no filesystem label"), "{without}");
        assert!(
            without.contains("every byte was checked"),
            "an absent label must be stated as checked, not merely omitted:\n{without}"
        );
    }

    #[test]
    fn everything_this_program_prints_is_ascii() {
        // A fresh Windows Server console is not UTF-8. The first guest run
        // rendered every em dash in this program's output as "???", which
        // turned readable sentences into noise and broke the harness's own
        // assertions at the same time. ASCII is not a style preference here;
        // it is the character set the target machine can display.
        let mut texts = vec![confirmation_text(
            &disk(),
            &part(LINUX_FILESYSTEM, BIG, 0),
            Some("DATA"),
            BIG,
        )];
        for t in [LINUX_FILESYSTEM, WINDOWS_BASIC_DATA, EFI_SYSTEM, MICROSOFT_RESERVED,
                  WINDOWS_RECOVERY, "00000000-0000-0000-0000-000000000000"] {
            if let Verdict::Refused(r) = assess(&part(t, 1024, 1), &[]) {
                texts.push(r.to_string());
            }
            texts.push(type_name(t).to_string());
        }
        if let Verdict::Refused(r) = assess(&part(LINUX_FILESYSTEM, BIG, 0),
                                            &[Claim { what: "mounted at C:\\".into() }]) {
            texts.push(r.to_string());
        }
        texts.push(cross_check(&[part(LINUX_FILESYSTEM, BIG, 0)], &[]).unwrap_err());
        for t in texts {
            if let Some(c) = t.chars().find(|c| !c.is_ascii()) {
                panic!("non-ASCII {c:?} in text shown to a Windows console:\n{t}");
            }
        }
    }

    #[test]
    fn signatures_are_found_where_blkid_looks_and_old_noise_is_not_one() {
        let mib = || vec![0u8; 1 << 20];
        assert_eq!(signature(&mib(), &[0; 65536], &mib()), None);
        let mut f = mib();
        f[3..11].copy_from_slice(b"NTFS    ");
        assert_eq!(signature(&f, &[], &mib()), Some("NTFS"));
        let mut f = mib();
        f[3..11].copy_from_slice(b"-FVE-FS-");
        assert_eq!(signature(&f, &[], &mib()), Some("BitLocker"));
        let mut f = mib();
        f[1080..1082].copy_from_slice(&[0x53, 0xef]);
        assert_eq!(signature(&f, &[], &mib()), Some("ext2/3/4"));
        let mut b = vec![0u8; 65536];
        b[64..72].copy_from_slice(b"_BHRfS_M");
        assert_eq!(signature(&mib(), &b, &mib()), Some("btrfs"));
        let mut l = mib();
        l[(1 << 20) - 65536..][..4].copy_from_slice(&[0xfc, 0x4e, 0x2b, 0xa9]);
        assert_eq!(signature(&mib(), &[], &l), Some("Linux RAID"));
        // The same bytes at an unaligned offset are noise, not a superblock.
        let mut l = mib();
        l[1234..1238].copy_from_slice(&[0xfc, 0x4e, 0x2b, 0xa9]);
        assert_eq!(signature(&mib(), &[], &l), None);
        let mut f = mib();
        f[0..6].copy_from_slice(b"LUKS\xba\xbe");
        assert_eq!(signature(&f, &[], &mib()), Some("LUKS"));
    }

    #[test]
    fn human_is_decimal_because_that_is_what_the_label_says() {
        assert_eq!(human(0), "0 bytes");
        assert_eq!(human(999), "999 bytes");
        assert_eq!(human(1_000), "1.0 kB");
        assert_eq!(human(500_107_862_016), "500.1 GB");
        assert_eq!(human(MINIMUM_ROOT_BYTES), "16.0 GB");
    }
}
