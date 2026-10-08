//! The whole Windows-side install, as a plan and then as writes.
//!
//! ═══ WHAT "INSTALL FROM WINDOWS" MEANS HERE ═══
//!
//! Windows cannot run `bootc install`, so the Windows program does not install
//! Rime. It prepares the disk so that Rime's own Linux installer can, and then
//! gets out of the way (ARCHITECTURE.md, "Route 2"):
//!
//! 1. Rime's OWN EFI System Partition is created in the free space the user
//!    chose, and the installer's live environment (shim, GRUB, kernel,
//!    initramfs, squashfs) is written into it as a FAT32 filesystem. The ESP
//!    is the one partition the Linux installer never formats, which is why the
//!    live files live there and not in the root partition: dracut keeps the
//!    live medium mounted for the whole session, so a medium inside the
//!    partition being formatted could not be formatted at all.
//! 2. An empty Linux-typed partition takes the rest of the space.
//! 3. A one-shot firmware entry starts the live environment once (BootNext).
//!
//! Windows' own ESP is never opened for writing; Rime boots from its own.
//!
//! ═══ THE ORDER OF WRITES, AND WHY ═══
//!
//! The payload goes into space no partition describes YET, and the partition
//! table is written LAST. Until that one write, nothing Windows or the
//! firmware can see has changed: an interrupted run leaves bytes in
//! unallocated space and nothing else. The GPT write itself puts the primary
//! header last (gptwrite::writes), so that is the single commit point, and
//! the firmware entry comes after the table is verified.
//!
//! Everything here works on any `Read + Write + Seek`, so the same code that
//! writes a Windows disk also writes a disk image on Linux, where the result
//! can be checked with sfdisk, fsck.fat and a real UEFI boot in QEMU.

use crate::gptwrite::{self, GptSnapshot, NewPartition};
use crate::payload::{self, FatFile, FatLayout, IsoFile, Sha256};
use crate::plan::{EFI_SYSTEM, LINUX_FILESYSTEM};
use std::io::{self, Read, Seek, SeekFrom, Write};

pub const MIB: u64 = 1024 * 1024;
const ALIGN_LBA: u64 = 2048;
/// The smallest root partition offered. The Linux installer refuses below
/// 10 GB for btrfs; 16 GB leaves room for the first update (a core update is
/// about 5 GB of new objects next to the old ones).
pub const MIN_ROOT_BYTES: u64 = 16 * 1000 * 1000 * 1000;
/// Free space left on Rime's ESP after the live files. Rime's own boot files
/// need about 30 MB; the rest is room for the systemd-boot layout Rime is
/// moving to, which needs about 350 MiB (docs/boot-v2.md).
const ESP_HEADROOM: u64 = 512 * MIB;
pub const FAT_LABEL: &str = "RIME-EFI";

/// The files inside the installer ISO that make up the live environment, and
/// where each goes on Rime's ESP. Paths on the ESP are 8.3 because the FAT
/// writer writes no long names; the directory is `rimeinst` on purpose, so
/// nothing a later bootloader writes (`EFI/fedora`, `EFI/BOOT`) can collide
/// with it and the Linux installer can delete exactly it after a success.
pub const ISO_FILES: [(&str, &str); 6] = [
    ("EFI/BOOT/BOOTX64.EFI", "EFI/rimeinst/shimx64.efi"),
    ("EFI/BOOT/grubx64.efi", "EFI/rimeinst/grubx64.efi"),
    ("EFI/BOOT/mmx64.efi", "EFI/rimeinst/mmx64.efi"),
    ("images/pxeboot/vmlinuz", "rimeinst/vmlinuz"),
    ("images/pxeboot/initrd.img", "rimeinst/initrd.img"),
    ("LiveOS/squashfs.img", "rimeinst/LiveOS/squashfs.img"),
];
pub const GRUB_CFG: &str = "EFI/rimeinst/grub.cfg";
pub const HANDOFF: &str = "rimeinst/handoff.cfg";

/// Where Rime goes: free space between partitions, or an existing partition
/// that has been checked empty, replaced by Rime's two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Space {
    Free { first_lba: u64, last_lba: u64 },
    Replace { partition_guid: String },
}

#[derive(Debug, Clone)]
pub enum Source {
    Iso { offset: u64, size: u64, sha256: Option<[u8; 32]> },
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone)]
pub struct StagedFile {
    pub dest: String,
    pub source: Source,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub before: GptSnapshot,
    pub after: GptSnapshot,
    pub esp: NewPartition,
    pub root: NewPartition,
    pub esp_slot: u32,
    pub root_slot: u32,
    pub fat: FatLayout,
    pub files: Vec<StagedFile>,
    /// Byte ranges (disk offsets) zeroed so no stale signature from whatever
    /// used to live in the free space makes the new root look like a
    /// filesystem it is not. The Linux installer refuses a "Linux partition"
    /// that blkid says holds btrfs, and it would be right to.
    pub wipes: Vec<(u64, u64)>,
    pub grub_cfg: String,
    pub handoff: String,
}

impl Plan {
    pub fn esp_offset(&self) -> u64 { self.esp.first_lba * gptwrite::SECTOR }
    pub fn esp_bytes(&self) -> u64 { (self.esp.last_lba - self.esp.first_lba + 1) * gptwrite::SECTOR }
    pub fn root_offset(&self) -> u64 { self.root.first_lba * gptwrite::SECTOR }
    pub fn root_bytes(&self) -> u64 { (self.root.last_lba - self.root.first_lba + 1) * gptwrite::SECTOR }
    /// Every byte range this plan may write, for a writer that refuses
    /// anything outside them: both GPT areas, the ESP, and the root's wipes.
    pub fn write_windows(&self) -> Vec<(u64, u64)> {
        let last = self.before.disk_bytes() / gptwrite::SECTOR - 1;
        let mut w = vec![
            (0, 34 * gptwrite::SECTOR),
            ((last - 32) * gptwrite::SECTOR, 33 * gptwrite::SECTOR),
            (self.esp_offset(), self.esp_bytes()),
        ];
        w.extend(self.wipes.iter().copied());
        w
    }
}

/// What the plan needs to know about the payload: each live file's place in
/// the ISO, plus (when the caller has them) their expected SHA-256s.
#[derive(Debug, Clone)]
pub struct Payload {
    pub files: Vec<(String, IsoFile)>,
}

pub fn locate_payload<R: Read + Seek>(iso: &mut R) -> io::Result<Payload> {
    let mut files = Vec::new();
    for (src, dest) in ISO_FILES {
        let f = payload::iso_find(iso, src)?;
        if f.size == 0 {
            return Err(refuse(&format!("{src} is empty in the installer image")));
        }
        files.push((dest.to_string(), f));
    }
    Ok(Payload { files })
}

pub fn payload_bytes(p: &Payload) -> u64 {
    p.files.iter().map(|(_, f)| f.size).sum()
}

/// The ESP is sized to hold the live files plus headroom, rounded up to
/// 256 MiB, and never below 1 GiB.
pub fn esp_size_bytes(payload: u64) -> u64 {
    let want = payload + ESP_HEADROOM + 16 * MIB;
    let step = 256 * MIB;
    want.div_ceil(step).max(4) * step
}

fn refuse(m: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, m.to_string())
}

/// FAT volume ids are shown by GRUB and blkid as XXXX-XXXX.
pub fn fat_uuid(id: u32) -> String {
    format!("{:04X}-{:04X}", id >> 16, id & 0xffff)
}

pub struct Inputs<'a> {
    pub before: &'a GptSnapshot,
    pub space: Space,
    pub payload: &'a Payload,
    pub esp_guid: String,
    pub root_guid: String,
    pub fat_volume_id: u32,
    pub boot_number: u16,
    pub windows_bitlocker: bool,
    pub created: String,
    pub app_version: &'a str,
}

/// Decide everything before anything is written. Pure: the same inputs give
/// the same plan, and nothing here touches a disk.
pub fn plan(i: &Inputs) -> io::Result<Plan> {
    let (start, end, base) = match &i.space {
        Space::Free { first_lba, last_lba } => {
            let fits = i.before.free_regions(ALIGN_LBA, 1).iter().any(|(a, b)| {
                a <= first_lba && last_lba <= b
            });
            if !fits {
                return Err(refuse("the chosen space is no longer free"));
            }
            (*first_lba, *last_lba, i.before.clone())
        }
        Space::Replace { partition_guid } => {
            let removed = gptwrite::with_removed(i.before, partition_guid)?;
            let layout = parsed(i.before)?;
            let p = layout
                .partitions
                .iter()
                .find(|p| p.id == *partition_guid)
                .ok_or_else(|| refuse("the chosen partition is gone"))?;
            let first = p.offset / gptwrite::SECTOR;
            let last = first + p.length / gptwrite::SECTOR - 1;
            (first, last, removed)
        }
    };
    let start = start.div_ceil(ALIGN_LBA) * ALIGN_LBA;
    if end <= start {
        return Err(refuse("the chosen space is too small"));
    }
    let payload_size = payload_bytes(i.payload);
    let esp_lba = esp_size_bytes(payload_size) / gptwrite::SECTOR;
    let esp = NewPartition {
        type_guid: EFI_SYSTEM.to_string(),
        unique_guid: i.esp_guid.clone(),
        first_lba: start,
        last_lba: start + esp_lba - 1,
        name: "Rime OS boot".to_string(),
        attributes: 0,
    };
    let root_first = start + esp_lba;
    // The root ends on a 1 MiB boundary too: whatever comes after it, the
    // backup GPT included, is never shared with a partial MiB.
    let root_last = ((end + 1) / ALIGN_LBA) * ALIGN_LBA - 1;
    if root_last <= root_first || (root_last - root_first + 1) * gptwrite::SECTOR < MIN_ROOT_BYTES {
        return Err(refuse(&format!(
            "the chosen space holds {} after Rime's boot partition; Rime needs at least {}",
            crate::plan::human((end.saturating_sub(root_first) + 1) * gptwrite::SECTOR),
            crate::plan::human(MIN_ROOT_BYTES)
        )));
    }
    let root = NewPartition {
        type_guid: LINUX_FILESYSTEM.to_string(),
        unique_guid: i.root_guid.clone(),
        first_lba: root_first,
        last_lba: root_last,
        name: "Rime OS".to_string(),
        attributes: 0,
    };
    let after = gptwrite::with_added(&base, &[esp.clone(), root.clone()])?;
    let esp_slot = gptwrite::slot_of(&after, &esp.unique_guid).ok_or_else(|| refuse("ESP slot"))?;
    let root_slot = gptwrite::slot_of(&after, &root.unique_guid).ok_or_else(|| refuse("root slot"))?;
    // Exactly the intended entries changed, and nothing else in either table.
    let changed = gptwrite::changed_slots(i.before, &after)?;
    let expect_max = if matches!(i.space, Space::Replace { .. }) { 3 } else { 2 };
    if changed.len() > expect_max || changed.is_empty() {
        return Err(refuse("the new partition table changes more than Rime's own entries"));
    }

    let disk_guid = i.before.disk_guid();
    let grub_cfg = grub_cfg(&i.esp_guid, i.fat_volume_id);
    let handoff = handoff(i, &disk_guid);
    let mut files: Vec<StagedFile> = i
        .payload
        .files
        .iter()
        .map(|(dest, f)| StagedFile {
            dest: dest.clone(),
            source: Source::Iso { offset: f.offset, size: f.size, sha256: None },
        })
        .collect();
    files.push(StagedFile { dest: GRUB_CFG.to_string(), source: Source::Bytes(grub_cfg.clone().into_bytes()) });
    files.push(StagedFile { dest: HANDOFF.to_string(), source: Source::Bytes(handoff.clone().into_bytes()) });
    let fat_files: Vec<FatFile> = files
        .iter()
        .map(|f| FatFile {
            path: f.dest.clone(),
            size: match &f.source {
                Source::Iso { size, .. } => *size,
                Source::Bytes(b) => b.len() as u64,
            },
        })
        .collect();
    let fat = payload::plan_fat32(esp_lba * gptwrite::SECTOR, FAT_LABEL, i.fat_volume_id, &fat_files)?;

    let ro = root_first * gptwrite::SECTOR;
    let rb = (root_last - root_first + 1) * gptwrite::SECTOR;
    let wipes = vec![(ro, MIB), (ro + 64 * MIB, MIB), (ro + rb - MIB, MIB)];
    Ok(Plan { before: i.before.clone(), after, esp, root, esp_slot, root_slot, fat, files, wipes, grub_cfg, handoff })
}

fn parsed(s: &GptSnapshot) -> io::Result<crate::Layout> {
    s.layout()
}

/// The live environment's boot menu. It is the ISO's own entries, pointed at
/// Rime's ESP instead of a CD label: `root=live:PARTUUID=` is one of the
/// forms dracut's dmsquash-live accepts (parse-dmsquash-live.sh), and the
/// PARTUUID is the one this program generated, so no other disk can answer
/// to it. "Back to Windows" is GRUB's `exit`: the firmware carries on down
/// its boot order, which still starts with Windows.
pub fn grub_cfg(esp_guid: &str, fat_id: u32) -> String {
    let args = format!(
        "root=live:PARTUUID={esp_guid} rd.live.image rd.live.dir=rimeinst/LiveOS selinux=0 console=ttyS0,115200 console=tty0"
    );
    format!(
        "# Rime OS Setup, written by the Rime installer for Windows.\n\
         if serial --unit=0 --speed=115200; then\n\
         \x20   terminal_input serial console\n\
         \x20   terminal_output serial console\n\
         fi\n\
         search --no-floppy --fs-uuid --set=root {uuid}\n\
         set default=0\n\
         set timeout=5\n\
         menuentry \"Install Rime OS\" {{\n\
         \x20   linux /rimeinst/vmlinuz {args}\n\
         \x20   initrd /rimeinst/initrd.img\n\
         }}\n\
         menuentry \"Install Rime OS (safe graphics)\" {{\n\
         \x20   linux /rimeinst/vmlinuz {args} nomodeset\n\
         \x20   initrd /rimeinst/initrd.img\n\
         }}\n\
         menuentry \"Back to Windows\" {{\n\
         \x20   exit\n\
         }}\n",
        uuid = fat_uuid(fat_id)
    )
}

/// The hand-off file the live installer reads (rime-installer-gui
/// read_handoff, rime-install's cleanup). Identifiers only; no secrets: the
/// account is created in the live installer, never on the Windows side.
pub fn handoff(i: &Inputs, disk_guid: &str) -> String {
    format!(
        "# Written by the Rime installer for Windows. Read by the Rime OS installer.\n\
         version=1\n\
         esp_partuuid={}\n\
         root_partuuid={}\n\
         disk_guid={}\n\
         bootnum={:04X}\n\
         windows_bitlocker={}\n\
         created={}\n\
         app={}\n",
        i.esp_guid, i.root_guid, disk_guid, i.boot_number,
        if i.windows_bitlocker { 1 } else { 0 }, i.created, i.app_version
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Wipe,
    Payload,
    Verify,
    Table,
}

/// Progress callback: (phase, bytes done, bytes total).
pub type Progress<'a> = &'a mut dyn FnMut(Phase, u64, u64);

const CHUNK: usize = 4 * 1024 * 1024;

fn write_at<D: Write + Seek>(d: &mut D, offset: u64, b: &[u8]) -> io::Result<()> {
    d.seek(SeekFrom::Start(offset))?;
    d.write_all(b)
}

/// Phase one: zero the root's signature areas and write the ESP's filesystem
/// and files. The disk's partition table is not touched. Returns the
/// SHA-256 of every file as it was written, for `verify_payload`.
pub fn write_payload<D: Read + Write + Seek, I: Read + Seek>(
    plan: &Plan,
    disk: &mut D,
    iso: &mut I,
    progress: Progress,
) -> io::Result<Vec<[u8; 32]>> {
    let zero = vec![0u8; MIB as usize];
    for (k, (o, l)) in plan.wipes.iter().enumerate() {
        let mut done = 0;
        while done < *l {
            let n = (*l - done).min(MIB) as usize;
            write_at(disk, o + done, &zero[..n])?;
            done += n as u64;
        }
        progress(Phase::Wipe, k as u64 + 1, plan.wipes.len() as u64);
    }
    let base = plan.esp_offset();
    for (o, b) in plan.fat.metadata() {
        write_at(disk, base + o, &b)?;
    }
    let total: u64 = plan.files.iter().map(file_len).sum();
    let mut done = 0u64;
    let mut hashes = Vec::new();
    let mut buf = vec![0u8; CHUNK];
    for (idx, f) in plan.files.iter().enumerate() {
        let (fo, fl) = plan.fat.file_extent(idx);
        if fl != file_len(f) {
            return Err(refuse("the filesystem plan and the file list disagree"));
        }
        let mut h = Sha256::new();
        match &f.source {
            Source::Bytes(b) => {
                // Pad to a whole sector: a raw disk takes whole sectors, and
                // the slack belongs to this file's own last cluster.
                let mut p = b.clone();
                p.resize(b.len().div_ceil(512) * 512, 0);
                write_at(disk, base + fo, &p)?;
                h.update(b);
                done += fl;
            }
            Source::Iso { offset, size, .. } => {
                iso.seek(SeekFrom::Start(*offset))?;
                let mut left = *size;
                let mut at = base + fo;
                while left > 0 {
                    let n = left.min(CHUNK as u64) as usize;
                    iso.read_exact(&mut buf[..n])?;
                    h.update(&buf[..n]);
                    let padded = n.div_ceil(512) * 512;
                    buf[n..padded].fill(0);
                    write_at(disk, at, &buf[..padded])?;
                    at += n as u64;
                    left -= n as u64;
                    done += n as u64;
                    progress(Phase::Payload, done, total);
                }
            }
        }
        hashes.push(h.finalize());
    }
    disk.flush()?;
    Ok(hashes)
}

fn file_len(f: &StagedFile) -> u64 {
    match &f.source {
        Source::Iso { size, .. } => *size,
        Source::Bytes(b) => b.len() as u64,
    }
}

/// Read everything back off the disk and compare: every metadata region byte
/// for byte, every file by SHA-256 against what was written, and the wipes
/// as zeros. "The writes returned success" is not the claim; this is.
pub fn verify_payload<D: Read + Seek>(
    plan: &Plan,
    disk: &mut D,
    written: &[[u8; 32]],
    progress: Progress,
) -> io::Result<()> {
    let base = plan.esp_offset();
    for (o, b) in plan.fat.metadata() {
        disk.seek(SeekFrom::Start(base + o))?;
        let mut got = vec![0u8; b.len()];
        disk.read_exact(&mut got)?;
        if got != b {
            return Err(io::Error::other(format!("the ESP's filesystem did not read back as written (at byte {o})")));
        }
    }
    let total: u64 = plan.files.iter().map(file_len).sum();
    let mut done = 0;
    let mut buf = vec![0u8; CHUNK];
    for (idx, f) in plan.files.iter().enumerate() {
        let (fo, fl) = plan.fat.file_extent(idx);
        disk.seek(SeekFrom::Start(base + fo))?;
        let mut h = Sha256::new();
        let mut left = fl;
        while left > 0 {
            // Reads stay whole sectors; the hash covers only the file's bytes.
            let n = left.min(CHUNK as u64) as usize;
            let padded = n.div_ceil(512) * 512;
            disk.read_exact(&mut buf[..padded])?;
            disk.seek(SeekFrom::Current(n as i64 - padded as i64))?;
            h.update(&buf[..n]);
            left -= n as u64;
            done += n as u64;
            progress(Phase::Verify, done, total);
        }
        if h.finalize() != written[idx] {
            return Err(io::Error::other(format!("{} did not read back as written", f.dest)));
        }
    }
    for (o, l) in &plan.wipes {
        disk.seek(SeekFrom::Start(*o))?;
        let mut got = vec![0u8; *l as usize];
        disk.read_exact(&mut got)?;
        if got.iter().any(|b| *b != 0) {
            return Err(io::Error::other("a cleared area of the new root partition is not zero"));
        }
    }
    Ok(())
}

/// Phase two, the commit: write the partition table. The caller has already
/// re-read the disk's table and found it identical to `plan.before`.
pub fn write_table<D: Write + Seek>(plan: &Plan, disk: &mut D, progress: Progress) -> io::Result<()> {
    let w = gptwrite::writes(&plan.before, &plan.after)?;
    let n = w.len() as u64;
    for (k, (o, b)) in w.iter().enumerate() {
        write_at(disk, *o, b)?;
        progress(Phase::Table, k as u64 + 1, n);
    }
    disk.flush()
}

/// A fresh read of the disk's GPT must equal the plan's starting point,
/// byte for byte: the plan is only valid for the table it was made from.
pub fn table_unchanged<D: Read + Seek>(plan: &Plan, disk: &mut D) -> io::Result<()> {
    let now = GptSnapshot::read(disk, plan.before.disk_bytes())?;
    if gptwrite::writes(&plan.before, &now)?.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other("the disk's partition table changed since the plan was made; nothing was committed"))
    }
}

/// After the commit: the table on disk is exactly `plan.after`.
pub fn table_committed<D: Read + Seek>(plan: &Plan, disk: &mut D) -> io::Result<()> {
    let now = GptSnapshot::read(disk, plan.before.disk_bytes())?;
    if gptwrite::writes(&plan.after, &now)?.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other("the partition table on disk is not the one that was written"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::process::Command;

    const GIB: u64 = 1024 * MIB;

    struct Temp(std::path::PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn temp(name: &str) -> Temp {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        Temp(std::env::temp_dir().join(format!("rime-stage-{name}-{}-{n}", std::process::id())))
    }
    fn tool(name: &str) -> bool {
        Command::new(name).arg("--version").output().is_ok()
    }

    /// Windows Setup's layout on a 64 GiB disk with a 30 GiB hole before the
    /// recovery partition, as sfdisk writes it.
    fn windows_disk() -> Temp {
        let t = temp("disk");
        File::create(&t.0).unwrap().set_len(64 * GIB).unwrap();
        let last = 64 * GIB / 512 - 1;
        let rec = (last - 33 - 600 * 2048 + 1) / 2048 * 2048;
        let script = format!(
            "label: gpt\nfirst-lba: 34\n\
             start=2048, size=100MiB, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B\n\
             size=16MiB, type=E3C9E316-0B5C-4DB8-817D-F92DF00215AE\n\
             size=30GiB, type=EBD0A0A2-B9E5-4433-87C0-68B6B72699C7\n\
             start={rec}, size=600MiB, type=DE94BBA4-06D1-4D40-A16A-BFD50179D6AC\n"
        );
        let mut c = Command::new("sfdisk").arg("-q").arg(&t.0).stdin(std::process::Stdio::piped()).spawn().unwrap();
        use std::io::Write as _;
        c.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
        assert!(c.wait().unwrap().success());
        t
    }

    /// A stand-in installer image: the six payload files at known offsets,
    /// filled with a pattern, so the test runs without the real 1.9 GB ISO.
    fn fake_iso() -> (Temp, Payload) {
        let t = temp("iso");
        let mut f = File::create(&t.0).unwrap();
        let sizes = [949_424u64, 4_046_544, 848_080, 18_745_704, 21_578_765, 160_759_398];
        let mut files = Vec::new();
        let mut off = 32768u64;
        for ((_, dest), size) in ISO_FILES.iter().zip(sizes) {
            f.seek(SeekFrom::Start(off)).unwrap();
            let pat: Vec<u8> = (0..size).map(|i| (i % 251) as u8 ^ dest.len() as u8).collect();
            f.write_all(&pat).unwrap();
            files.push((dest.to_string(), IsoFile { path: dest.to_string(), offset: off, size }));
            off = (off + size).div_ceil(2048) * 2048;
        }
        (t, Payload { files })
    }

    fn inputs<'a>(before: &'a GptSnapshot, payload: &'a Payload, space: Space) -> Inputs<'a> {
        Inputs {
            before,
            space,
            payload,
            esp_guid: "11111111-2222-4333-8444-555555555555".into(),
            root_guid: "66666666-7777-4888-9999-aaaaaaaaaaaa".into(),
            fat_volume_id: 0x1234_abcd,
            boot_number: 0x0009,
            windows_bitlocker: false,
            created: "test".into(),
            app_version: "test",
        }
    }

    /// Wraps a disk and refuses every write outside the plan's ranges: the
    /// same rule winwrite::DiskWriter enforces on Windows.
    struct Guard<'a> {
        f: File,
        allowed: &'a [(u64, u64)],
        pos: u64,
    }
    impl Read for Guard<'_> {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            let n = self.f.read(b)?;
            self.pos += n as u64;
            Ok(n)
        }
    }
    impl Seek for Guard<'_> {
        fn seek(&mut self, p: SeekFrom) -> io::Result<u64> {
            self.pos = self.f.seek(p)?;
            Ok(self.pos)
        }
    }
    impl Write for Guard<'_> {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            let end = self.pos + b.len() as u64;
            assert!(
                self.allowed.iter().any(|(o, l)| self.pos >= *o && end <= o + l),
                "write of {} bytes at {} is outside the plan's ranges",
                b.len(),
                self.pos
            );
            assert!(self.pos.is_multiple_of(512) && b.len().is_multiple_of(512), "unaligned write at {}", self.pos);
            let n = self.f.write(b)?;
            self.pos += n as u64;
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.f.flush()
        }
    }

    #[test]
    fn esp_is_sized_from_the_payload_with_headroom() {
        assert_eq!(esp_size_bytes(1), 1024 * MIB, "never below 1 GiB");
        let p = 1_850_000_000;
        let e = esp_size_bytes(p);
        assert!(e >= p + 512 * MIB && e.is_multiple_of(256 * MIB), "{e}");
    }

    #[test]
    fn stages_a_windows_disk_and_touches_nothing_else() {
        if !tool("sfdisk") || !tool("sgdisk") || !tool("fsck.fat") {
            panic!("sfdisk, sgdisk and fsck.fat are required (util-linux, gdisk, dosfstools)");
        }
        let disk = windows_disk();
        let (iso, payload) = fake_iso();
        let mut f = File::open(&disk.0).unwrap();
        let before = GptSnapshot::read(&mut f, 64 * GIB).unwrap();
        let hole = before.free_regions(2048, 2048).into_iter().max_by_key(|(a, b)| b - a).unwrap();
        let plan = plan(&inputs(&before, &payload, Space::Free { first_lba: hole.0, last_lba: hole.1 })).unwrap();
        assert_eq!(plan.esp_slot, 5);
        assert_eq!(plan.root_slot, 6);
        assert_eq!(plan.esp.first_lba, hole.0);
        assert!(plan.root_bytes() >= MIN_ROOT_BYTES);
        assert!(plan.root.last_lba <= hole.1);
        assert!(plan.grub_cfg.contains("root=live:PARTUUID=11111111-2222-4333-8444-555555555555"));
        assert!(plan.grub_cfg.contains("--fs-uuid --set=root 1234-ABCD"));
        assert!(plan.handoff.contains("bootnum=0009\n") && plan.handoff.contains("windows_bitlocker=0\n"));
        assert!(!plan.handoff.to_lowercase().contains("password"));

        let allowed = plan.write_windows();
        let mut g = Guard { f: OpenOptions::new().read(true).write(true).open(&disk.0).unwrap(), allowed: &allowed, pos: 0 };
        let mut iso_f = File::open(&iso.0).unwrap();
        let mut pr = |_: Phase, _: u64, _: u64| {};
        table_unchanged(&plan, &mut g).unwrap();
        let hashes = write_payload(&plan, &mut g, &mut iso_f, &mut pr).unwrap();
        verify_payload(&plan, &mut g, &hashes, &mut pr).unwrap();
        // Until the table is written, the disk's partition table is the old one.
        table_unchanged(&plan, &mut g).unwrap();
        write_table(&plan, &mut g, &mut pr).unwrap();
        table_committed(&plan, &mut g).unwrap();
        drop(g);

        let v = Command::new("sgdisk").arg("-v").arg(&disk.0).output().unwrap();
        assert!(String::from_utf8_lossy(&v.stdout).contains("No problems found"), "{}", String::from_utf8_lossy(&v.stdout));
        // The ESP, cut out of the disk, is a clean FAT32 filesystem.
        let esp = temp("esp");
        let mut src = File::open(&disk.0).unwrap();
        src.seek(SeekFrom::Start(plan.esp_offset())).unwrap();
        let mut out = File::create(&esp.0).unwrap();
        io::copy(&mut src.take(plan.esp_bytes()), &mut out).unwrap();
        let fsck = Command::new("fsck.fat").args(["-n", "-V"]).arg(&esp.0).output().unwrap();
        assert!(fsck.status.success(), "{}", String::from_utf8_lossy(&fsck.stdout));
    }

    #[test]
    fn a_stale_plan_is_refused_before_the_commit() {
        if !tool("sfdisk") {
            panic!("sfdisk is required (util-linux)");
        }
        let disk = windows_disk();
        let (_iso, payload) = fake_iso();
        let mut f = OpenOptions::new().read(true).write(true).open(&disk.0).unwrap();
        let before = GptSnapshot::read(&mut f, 64 * GIB).unwrap();
        let hole = before.free_regions(2048, 2048).into_iter().max_by_key(|(a, b)| b - a).unwrap();
        let plan = plan(&inputs(&before, &payload, Space::Free { first_lba: hole.0, last_lba: hole.1 })).unwrap();
        // Someone creates a partition in Disk Management in the meantime.
        let st = Command::new("sgdisk").args(["-n", "0:0:+1G"]).arg(&disk.0).output().unwrap();
        assert!(st.status.success());
        assert!(table_unchanged(&plan, &mut f).is_err());
    }

    #[test]
    fn too_little_space_is_refused_with_the_numbers() {
        if !tool("sfdisk") {
            panic!("sfdisk is required (util-linux)");
        }
        let disk = windows_disk();
        let (_iso, payload) = fake_iso();
        let mut f = File::open(&disk.0).unwrap();
        let before = GptSnapshot::read(&mut f, 64 * GIB).unwrap();
        let hole = before.free_regions(2048, 2048).into_iter().max_by_key(|(a, b)| b - a).unwrap();
        let small = Space::Free { first_lba: hole.0, last_lba: hole.0 + 10 * GIB / 512 };
        let e = plan(&inputs(&before, &payload, small)).unwrap_err().to_string();
        assert!(e.contains("needs at least"), "{e}");
        let outside = Space::Free { first_lba: 2048, last_lba: 4_000_000 };
        assert!(plan(&inputs(&before, &payload, outside)).is_err(), "overlapping Windows' partitions");
    }
}
