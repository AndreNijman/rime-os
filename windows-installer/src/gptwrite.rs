//! Editing GPT entries in place: adding Rime's ESP and root, and undoing it.
//!
//! Everything here is pure: it reads and returns byte buffers, and the only
//! I/O is through a caller-supplied `Read + Seek`. Nothing in this file opens
//! a disk. That is deliberate. The part that decides WHAT bytes go WHERE is
//! the part that can wreck a machine, and it is the part that can be tested on
//! every CI run against real `sfdisk`/`sgdisk` images. The Windows layer only
//! has to perform the writes this module hands it, in the order given.
//!
//! The binding invariants (docs/rime-owns-its-esp.md, "The second decision"):
//!
//! - the delta is exactly the intended entries (`changed_slots` proves it);
//! - both GPT copies end consistent, with correct CRCs (every snapshot this
//!   module produces is re-read through `crate::enumerate_in`, the crate's
//!   only GPT reader, and refused if that reader refuses it);
//! - both copies are saved before any change and can be restored
//!   (`to_backup_file` / `from_backup_file` / `writes`);
//! - layouts are derived from a fresh read, never reconstructed: every edit
//!   starts from a `GptSnapshot` and changes only entry slots and the four CRC
//!   fields. No function here builds a table from a description.
//!
//! Why in place and never `SET_DRIVE_LAYOUT_EX`: measured in the Windows lab,
//! that IOCTL relocated the primary entry array on a disk with FirstUsableLBA
//! 2048 and left the old array at LBA 2, i.e. two disagreeing tables. A raw
//! in-place edit leaves one. Windows permits raw writes to LBA 0-33 of its own
//! system disk (also measured), so the in-place route is available.

use std::io::{self, Read, Seek, SeekFrom};

pub const SECTOR: u64 = 512;

/// Entry array geometry. `enumerate_in` refuses anything else, so this module
/// does not pretend to handle anything else either.
const ENTRIES: usize = 128;
const ENTRY_SIZE: usize = 128;
const ARRAY_BYTES: usize = ENTRIES * ENTRY_SIZE;
const ARRAY_SECTORS: u64 = (ARRAY_BYTES as u64) / SECTOR;
/// LBA 0 (protective MBR), 1 (header), 2..33 (array).
const PRIMARY_SECTORS: usize = 34;
/// last-32..last-1 (array), last (header).
const BACKUP_SECTORS: usize = 33;
const S: usize = SECTOR as usize;

const MAGIC: &[u8; 8] = b"RIMEGPT1";
const BACKUP_FILE_BYTES: usize = 8 + 8 + 16 + PRIMARY_SECTORS * S + BACKUP_SECTORS * S + 4;

fn refuse(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn u64le(b: &[u8]) -> u64 { u64::from_le_bytes(b[..8].try_into().unwrap()) }

/// The crate's crc32 is private to lib.rs and this module may not widen
/// lib.rs, so this is a second copy of the same reflected CRC-32 (IEEE,
/// polynomial 0xEDB88320). A test pins it to the standard check value.
fn crc32(b: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in b {
        crc ^= u32::from(*byte);
        for _ in 0..8 { crc = (crc >> 1) ^ (0xedb8_8320 & (0u32.wrapping_sub(crc & 1))); }
    }
    !crc
}

/// Both GPT copies, as raw sectors, exactly as they were read.
///
/// Holding raw bytes rather than a parsed model is the point: an edit changes
/// the bytes it means to change and carries every other byte through
/// untouched, including header bytes 92..512 that no reader looks at and
/// fields this program has no opinion on. A model would have to reconstruct
/// them, and reconstruction is what the design forbids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GptSnapshot {
    disk_bytes: u64,
    /// LBA 0..=33: protective MBR, primary header, primary entry array.
    primary: Vec<u8>,
    /// LBA last-32..=last: backup entry array, backup header.
    backup: Vec<u8>,
}

/// A `Read + Seek` that is the whole disk as far as `enumerate_in` can tell:
/// the snapshot's sectors where they live, zeros everywhere else. This lets
/// every produced table go back through the one real reader without a
/// 64 GiB buffer.
struct SparseDisk<'a> {
    snap: &'a GptSnapshot,
    pos: u64,
}

impl Read for SparseDisk<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let len = self.snap.disk_bytes;
        if self.pos >= len || out.is_empty() { return Ok(0); }
        let n = (out.len() as u64).min(len - self.pos) as usize;
        let backup_start = len - (BACKUP_SECTORS as u64) * SECTOR;
        let primary_end = (PRIMARY_SECTORS as u64) * SECTOR;
        for (i, slot) in out[..n].iter_mut().enumerate() {
            let at = self.pos + i as u64;
            *slot = if at < primary_end {
                self.snap.primary[at as usize]
            } else if at >= backup_start {
                self.snap.backup[(at - backup_start) as usize]
            } else {
                0
            };
        }
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for SparseDisk<'_> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let next = match to {
            SeekFrom::Start(v) => Some(v),
            SeekFrom::End(d) => self.snap.disk_bytes.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        self.pos = next.ok_or_else(|| refuse("seek outside the disk"))?;
        Ok(self.pos)
    }
}

impl GptSnapshot {
    fn last_lba(&self) -> u64 { self.disk_bytes / SECTOR - 1 }
    fn mbr(&self) -> &[u8] { &self.primary[..S] }
    fn primary_header(&self) -> &[u8] { &self.primary[S..2 * S] }
    fn primary_array(&self) -> &[u8] { &self.primary[2 * S..] }
    fn backup_array(&self) -> &[u8] { &self.backup[..ARRAY_BYTES] }
    fn backup_header(&self) -> &[u8] { &self.backup[ARRAY_BYTES..] }

    /// Runs the crate's own reader over this snapshot. Every snapshot that
    /// leaves this module has passed this, so a table the reader would
    /// refuse can never reach a disk.
    pub fn layout(&self) -> io::Result<crate::Layout> {
        crate::enumerate_in(&mut SparseDisk { snap: self, pos: 0 }, self.disk_bytes)
    }

    /// Captures both GPT copies from a disk.
    ///
    /// `enumerate_in` runs FIRST, against the real disk, and anything it
    /// refuses is refused here: this module never edits a table the reader
    /// does not fully understand. Then the sectors are captured, and the
    /// capture is checked against that first read, so a disk that changed
    /// between the two reads is refused rather than half-believed.
    pub fn read<R: Read + Seek>(r: &mut R, disk_bytes: u64) -> io::Result<GptSnapshot> {
        let before = crate::enumerate_in(r, disk_bytes)?;
        let last = disk_bytes / SECTOR - 1;
        let mut primary = vec![0; PRIMARY_SECTORS * S];
        r.seek(SeekFrom::Start(0))?;
        r.read_exact(&mut primary)?;
        let mut backup = vec![0; BACKUP_SECTORS * S];
        r.seek(SeekFrom::Start((last - ARRAY_SECTORS) * SECTOR))?;
        r.read_exact(&mut backup)?;
        let snap = GptSnapshot { disk_bytes, primary, backup };
        if snap.layout()? != before {
            return Err(refuse("the GPT changed while it was being read; read it again"));
        }
        Ok(snap)
    }

    /// The undo file, written BEFORE any change.
    ///
    /// Layout (all little-endian), 34340 bytes exactly:
    ///
    /// | bytes | content |
    /// |---|---|
    /// | 8 | magic `RIMEGPT1` |
    /// | 8 | disk length in bytes |
    /// | 16 | disk GUID, raw GPT encoding (header bytes 56..72) |
    /// | 17408 | LBA 0..=33: protective MBR, primary header, primary array |
    /// | 16896 | LBA last-32..=last: backup array, backup header |
    /// | 4 | CRC-32 of every byte before it |
    ///
    /// The CRC is not security (anyone who can edit the file can recompute
    /// it); it catches a truncated or bit-rotted file, which is the failure
    /// an undo file actually meets. The tables inside are re-validated by the
    /// reader on load in any case.
    pub fn to_backup_file(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(BACKUP_FILE_BYTES);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.disk_bytes.to_le_bytes());
        out.extend_from_slice(&self.primary_header()[56..72]);
        out.extend_from_slice(&self.primary);
        out.extend_from_slice(&self.backup);
        let crc = crc32(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out
    }

    /// Loads an undo file, refusing anything that is not exactly what
    /// `to_backup_file` wrote: wrong length, wrong magic, a CRC mismatch, a
    /// disk GUID field that disagrees with the header it describes, or tables
    /// the reader refuses.
    pub fn from_backup_file(b: &[u8]) -> io::Result<GptSnapshot> {
        if b.len() != BACKUP_FILE_BYTES {
            return Err(refuse(format!(
                "not a Rime GPT backup: {} bytes, expected {BACKUP_FILE_BYTES}", b.len())));
        }
        if &b[..8] != MAGIC { return Err(refuse("not a Rime GPT backup: wrong magic")); }
        let (body, trailer) = b.split_at(b.len() - 4);
        if crc32(body) != u32::from_le_bytes(trailer.try_into().unwrap()) {
            return Err(refuse("the GPT backup file is damaged (checksum mismatch); it cannot be used"));
        }
        let disk_bytes = u64le(&b[8..]);
        if disk_bytes < 68 * SECTOR || !disk_bytes.is_multiple_of(SECTOR) {
            return Err(refuse("the GPT backup file records an impossible disk length"));
        }
        let guid_field = &b[16..32];
        let p0 = 32;
        let p1 = p0 + PRIMARY_SECTORS * S;
        let snap = GptSnapshot {
            disk_bytes,
            primary: b[p0..p1].to_vec(),
            backup: b[p1..p1 + BACKUP_SECTORS * S].to_vec(),
        };
        if guid_field != &snap.primary_header()[56..72] {
            return Err(refuse("the GPT backup file's disk GUID disagrees with the table it holds"));
        }
        snap.layout()?;
        Ok(snap)
    }

    pub fn disk_bytes(&self) -> u64 { self.disk_bytes }
    pub fn disk_guid(&self) -> String { crate::guid(&self.primary_header()[56..72]) }
    pub fn first_usable_lba(&self) -> u64 { u64le(&self.primary_header()[40..]) }
    pub fn last_usable_lba(&self) -> u64 { u64le(&self.primary_header()[48..]) }

    /// Unallocated regions inside the usable range, as inclusive
    /// `(first_lba, last_lba)` pairs, with the start rounded UP and the end
    /// rounded DOWN to `align_lba` so the whole region is usable as-is.
    /// Regions shorter than `min_lba` sectors after rounding are dropped.
    ///
    /// Computed from the reader's answer, not from raw entries, so it can
    /// never see a partition the reader did not.
    pub fn free_regions(&self, align_lba: u64, min_lba: u64) -> Vec<(u64, u64)> {
        let align = align_lba.max(1);
        // Every constructor (read, from_backup_file, with_*) has already
        // passed this snapshot through the reader, so this cannot fail. If it
        // ever did, an empty answer would read as "no free space", which is a
        // guess; a panic is the honest answer to a broken invariant.
        let layout = self.layout().expect("GptSnapshot is validated by the reader at construction");
        let mut used: Vec<(u64, u64)> = layout.partitions.iter()
            .map(|p| (p.offset / SECTOR, (p.offset + p.length) / SECTOR - 1)).collect();
        used.sort();
        let (first, last) = (self.first_usable_lba(), self.last_usable_lba());
        let mut gaps = Vec::new();
        let mut cursor = first;
        for (a, b) in used {
            if a > cursor { gaps.push((cursor, a - 1)); }
            cursor = cursor.max(b + 1);
        }
        if cursor <= last { gaps.push((cursor, last)); }
        gaps.into_iter().filter_map(|(a, b)| {
            let start = a.div_ceil(align) * align;
            let end = ((b + 1) / align * align).checked_sub(1)?;
            (end >= start && end - start + 1 >= min_lba.max(1)).then_some((start, end))
        }).collect()
    }

    /// Rewrites the entry array into both copies and recomputes all four
    /// CRCs: the array CRC (88..92) in both headers, then each header's own
    /// CRC (16..20, over its first 92 bytes with the field zeroed). Nothing
    /// else in either header is touched; headers 92..512 included.
    fn with_array(&self, array: &[u8]) -> io::Result<GptSnapshot> {
        debug_assert_eq!(array.len(), ARRAY_BYTES);
        let mut next = self.clone();
        next.primary[2 * S..].copy_from_slice(array);
        next.backup[..ARRAY_BYTES].copy_from_slice(array);
        let array_crc = crc32(array).to_le_bytes();
        for header in [&mut next.primary[S..2 * S], &mut next.backup[ARRAY_BYTES..]] {
            header[88..92].copy_from_slice(&array_crc);
            header[16..20].fill(0);
            let crc = crc32(&header[..92]).to_le_bytes();
            header[16..20].copy_from_slice(&crc);
        }
        next.layout()?;
        Ok(next)
    }
}

/// One entry to add. GUIDs are the usual text form; LBAs are inclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPartition {
    pub type_guid: String,
    pub unique_guid: String,
    pub first_lba: u64,
    pub last_lba: u64,
    pub name: String,
    pub attributes: u64,
}

/// GUID text to the 16-byte GPT encoding: the first three groups
/// little-endian, the last two in byte order. The inverse of `crate::guid`.
/// Only the canonical 8-4-4-4-12 form is accepted (either case); braces and
/// other spellings are refused, not guessed at.
pub fn guid_bytes(s: &str) -> io::Result<[u8; 16]> {
    let bad = || refuse(format!("not a GUID: {s:?}"));
    let groups: Vec<&str> = s.split('-').collect();
    if groups.len() != 5 || groups.iter().zip([8, 4, 4, 4, 12]).any(|(g, n)| g.len() != n)
        || !s.bytes().all(|c| c == b'-' || c.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let hex = |g: &str| -> io::Result<Vec<u8>> {
        (0..g.len()).step_by(2)
            .map(|i| u8::from_str_radix(&g[i..i + 2], 16).map_err(|_| bad()))
            .collect()
    };
    let mut out = [0u8; 16];
    let mut a = hex(groups[0])?; a.reverse();
    let mut b = hex(groups[1])?; b.reverse();
    let mut c = hex(groups[2])?; c.reverse();
    out[..4].copy_from_slice(&a);
    out[4..6].copy_from_slice(&b);
    out[6..8].copy_from_slice(&c);
    out[8..10].copy_from_slice(&hex(groups[3])?);
    out[10..].copy_from_slice(&hex(groups[4])?);
    Ok(out)
}

/// A GPT name field: UTF-16LE, 36 code units, zero-padded. Longer is
/// refused rather than truncated (a truncated name is a different name), and
/// an embedded NUL is refused because readers stop there.
fn name_bytes(name: &str) -> io::Result<[u8; 72]> {
    let units: Vec<u16> = name.encode_utf16().collect();
    if units.len() > 36 {
        return Err(refuse(format!("partition name {name:?} is longer than 36 UTF-16 code units")));
    }
    if units.contains(&0) { return Err(refuse("partition name contains a NUL")); }
    let mut out = [0u8; 72];
    for (i, u) in units.iter().enumerate() { out[2 * i..2 * i + 2].copy_from_slice(&u.to_le_bytes()); }
    Ok(out)
}

fn is_empty_slot(e: &[u8]) -> bool { e.iter().all(|v| *v == 0) }

/// The snapshot as it would be after adding `adds`, each in the
/// lowest-numbered empty slot, in order.
///
/// Refused unless every new entry: lies inside the usable range; has
/// first <= last; starts on a 1 MiB boundary (LBA multiple of 2048); overlaps
/// no existing partition and no other new one; has a non-nil type GUID and a
/// non-nil unique GUID that no existing partition, no other new one and not
/// the disk itself already uses; has a valid name; and fits in an empty slot.
/// The result is then re-read by `crate::enumerate_in` and checked to be the
/// old layout plus exactly these entries.
pub fn with_added(s: &GptSnapshot, adds: &[NewPartition]) -> io::Result<GptSnapshot> {
    if adds.is_empty() { return Err(refuse("nothing to add")); }
    let layout = s.layout()?;
    let (first_usable, last_usable) = (s.first_usable_lba(), s.last_usable_lba());
    let mut taken: Vec<(u64, u64)> = layout.partitions.iter()
        .map(|p| (p.offset / SECTOR, (p.offset + p.length) / SECTOR - 1)).collect();
    let mut ids: Vec<[u8; 16]> = vec![guid_bytes(&layout.disk_id)?];
    for p in &layout.partitions { ids.push(guid_bytes(&p.id)?); }
    let mut array = s.primary_array().to_vec();
    let mut expected = layout.partitions.clone();
    for add in adds {
        let what = format!("new partition {:?} (LBA {}..{})", add.name, add.first_lba, add.last_lba);
        let kind = guid_bytes(&add.type_guid)?;
        let id = guid_bytes(&add.unique_guid)?;
        if kind == [0; 16] { return Err(refuse(format!("{what}: the type GUID is nil"))); }
        if id == [0; 16] { return Err(refuse(format!("{what}: the unique GUID is nil"))); }
        if ids.contains(&id) {
            return Err(refuse(format!("{what}: unique GUID {} is already in use on this disk", add.unique_guid)));
        }
        if add.first_lba > add.last_lba { return Err(refuse(format!("{what}: first LBA is after last LBA"))); }
        if add.first_lba < first_usable || add.last_lba > last_usable {
            return Err(refuse(format!(
                "{what}: outside the disk's usable range LBA {first_usable}..{last_usable}")));
        }
        if !add.first_lba.is_multiple_of(2048) {
            return Err(refuse(format!("{what}: does not start on a 1 MiB boundary")));
        }
        if let Some((a, b)) = taken.iter().find(|(a, b)| add.first_lba <= *b && *a <= add.last_lba) {
            return Err(refuse(format!("{what}: overlaps the partition at LBA {a}..{b}")));
        }
        let name = name_bytes(&add.name)?;
        let slot = array.chunks(ENTRY_SIZE).position(is_empty_slot)
            .ok_or_else(|| refuse(format!("{what}: the partition table has no empty entry left")))?;
        let e = &mut array[slot * ENTRY_SIZE..(slot + 1) * ENTRY_SIZE];
        e[..16].copy_from_slice(&kind);
        e[16..32].copy_from_slice(&id);
        e[32..40].copy_from_slice(&add.first_lba.to_le_bytes());
        e[40..48].copy_from_slice(&add.last_lba.to_le_bytes());
        e[48..56].copy_from_slice(&add.attributes.to_le_bytes());
        e[56..128].copy_from_slice(&name);
        taken.push((add.first_lba, add.last_lba));
        ids.push(id);
        expected.push(crate::Partition {
            id: crate::guid(&id), kind: crate::guid(&kind), name: add.name.clone(),
            offset: add.first_lba * SECTOR, length: (add.last_lba - add.first_lba + 1) * SECTOR,
            attributes: add.attributes,
        });
    }
    let next = s.with_array(&array)?;
    // Belt and braces: the reader's answer for the new table must be the old
    // partitions, unchanged and in place, plus exactly the new ones.
    let mut got = next.layout()?.partitions;
    got.sort_by_key(|p| p.offset);
    expected.sort_by_key(|p| p.offset);
    if got != expected || next.disk_guid() != s.disk_guid() {
        return Err(refuse("internal error: the edited table does not read back as intended"));
    }
    Ok(next)
}

/// The snapshot with the entry whose unique GUID matches zeroed, all 128
/// bytes (a zero type with leftover bytes is a "stale entry" the reader
/// refuses). Refused if no entry matches, or more than one does.
pub fn with_removed(s: &GptSnapshot, unique_guid: &str) -> io::Result<GptSnapshot> {
    s.layout()?;
    let id = guid_bytes(unique_guid)?;
    let mut array = s.primary_array().to_vec();
    let hits: Vec<usize> = array.chunks(ENTRY_SIZE).enumerate()
        .filter(|(_, e)| !is_empty_slot(e) && e[16..32] == id).map(|(i, _)| i).collect();
    match hits[..] {
        [slot] => array[slot * ENTRY_SIZE..(slot + 1) * ENTRY_SIZE].fill(0),
        [] => return Err(refuse(format!("no partition with unique GUID {unique_guid} on this disk"))),
        _ => return Err(refuse(format!("unique GUID {unique_guid} appears more than once; refusing"))),
    }
    s.with_array(&array)
}

/// Header bytes that may legitimately differ between two versions of one
/// table: the header CRC and the entry-array CRC.
fn header_differs_only_in_crcs(a: &[u8], b: &[u8]) -> bool {
    a.iter().zip(b).enumerate().all(|(i, (x, y))| x == y || (16..20).contains(&i) || (88..92).contains(&i))
}

/// Which entry slots (0-based) differ between two snapshots, after proving
/// that NOTHING else does: same disk length, identical protective MBR, each
/// header identical to its counterpart except the two CRC fields (all 512
/// bytes compared, not just the 92 a reader checks), and in both snapshots
/// the backup array equal to the primary array.
pub fn changed_slots(before: &GptSnapshot, after: &GptSnapshot) -> io::Result<Vec<usize>> {
    if before.disk_bytes != after.disk_bytes {
        return Err(refuse("the two tables are for disks of different lengths"));
    }
    if before.mbr() != after.mbr() { return Err(refuse("the protective MBR differs")); }
    if !header_differs_only_in_crcs(before.primary_header(), after.primary_header()) {
        return Err(refuse("the primary GPT header differs in more than its CRC fields"));
    }
    if !header_differs_only_in_crcs(before.backup_header(), after.backup_header()) {
        return Err(refuse("the backup GPT header differs in more than its CRC fields"));
    }
    for s in [before, after] {
        if s.backup_array() != s.primary_array() {
            return Err(refuse("a backup entry array disagrees with its primary"));
        }
    }
    Ok(before.primary_array().chunks(ENTRY_SIZE).zip(after.primary_array().chunks(ENTRY_SIZE))
        .enumerate().filter(|(_, (a, b))| a != b).map(|(i, _)| i).collect())
}

/// The sector writes that take a disk from `before` to `after`, as
/// `(byte offset, 512 bytes)`, in the order they must be performed:
///
/// 1. changed backup entry-array sectors (LBA last-32..last-1),
/// 2. the backup header (LBA last),
/// 3. changed primary entry-array sectors (LBA 2..33),
/// 4. the primary header (LBA 1), LAST.
///
/// The primary header is the commit point: until it lands, the primary
/// table's CRC still describes the old array, so an interruption leaves a
/// primary table that is intact and old (and a backup that is new), never a
/// primary that is half-written and believed. Only sectors whose bytes change
/// are included; the protective MBR is never written.
///
/// Used for both directions: applying an edit, and restoring from an undo
/// file (`writes(&fresh_read, &restored)`). Refused if the two snapshots are
/// for different disks (length or disk GUID) or if the difference is
/// anything but entries and CRCs. `before` MUST be a fresh read of the disk
/// about to be written; this function cannot check that, the caller must.
pub fn writes(before: &GptSnapshot, after: &GptSnapshot) -> io::Result<Vec<(u64, Vec<u8>)>> {
    if before.disk_bytes != after.disk_bytes {
        return Err(refuse(format!("these tables are for different disks: {} bytes vs {} bytes",
            before.disk_bytes, after.disk_bytes)));
    }
    if before.disk_guid() != after.disk_guid() {
        return Err(refuse(format!("these tables are for different disks: disk GUID {} vs {}",
            before.disk_guid(), after.disk_guid())));
    }
    before.layout()?;
    after.layout()?;
    changed_slots(before, after)?;
    let last = before.last_lba();
    let mut out = Vec::new();
    let sector = |buf: &[u8], i: usize| buf[i * S..(i + 1) * S].to_vec();
    for i in 0..ARRAY_SECTORS as usize {
        if before.backup[i * S..(i + 1) * S] != after.backup[i * S..(i + 1) * S] {
            out.push(((last - ARRAY_SECTORS + i as u64) * SECTOR, sector(&after.backup, i)));
        }
    }
    if before.backup_header() != after.backup_header() {
        out.push((last * SECTOR, after.backup_header().to_vec()));
    }
    for i in 2..PRIMARY_SECTORS {
        if before.primary[i * S..(i + 1) * S] != after.primary[i * S..(i + 1) * S] {
            out.push((i as u64 * SECTOR, sector(&after.primary, i)));
        }
    }
    if before.primary_header() != after.primary_header() {
        out.push((SECTOR, after.primary_header().to_vec()));
    }
    Ok(out)
}

/// The 1-based partition number of the entry with that unique GUID, which is
/// how Linux (`/dev/nvme0n1pN`) and UEFI device paths count: by slot, not by
/// position on the disk, and empty slots still count.
pub fn slot_of(s: &GptSnapshot, unique_guid: &str) -> Option<u32> {
    let id = guid_bytes(unique_guid).ok()?;
    s.primary_array().chunks(ENTRY_SIZE)
        .position(|e| !is_empty_slot(e) && e[16..32] == id)
        .map(|i| i as u32 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::io::{Cursor, Write};
    use std::path::PathBuf;
    use std::process::Command;

    const ESP: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
    const MSR: &str = "e3c9e316-0b5c-4db8-817d-f92df00215ae";
    const DATA: &str = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7";
    const RECOVERY: &str = "de94bba4-06d1-4d40-a16a-bfd50179d6ac";
    const LINUX_ROOT: &str = "4f68bce3-e8cd-4db1-96e7-fbcaf984b709";
    const MIB: u64 = 2048;
    const GIB: u64 = 1024 * MIB;

    /// A sparse image file, deleted when the test ends, pass or fail.
    struct Img(PathBuf);
    impl Drop for Img { fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); } }
    impl Img {
        fn new(tag: &str, sectors: u64) -> Img {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!("rime-gptwrite-{}-{}-{tag}.img",
                std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
            File::create(&p).unwrap().set_len(sectors * SECTOR).unwrap();
            Img(p)
        }
        fn len(&self) -> u64 { std::fs::metadata(&self.0).unwrap().len() }
        fn snapshot(&self) -> GptSnapshot {
            GptSnapshot::read(&mut File::open(&self.0).unwrap(), self.len()).unwrap()
        }
        fn apply(&self, w: &[(u64, Vec<u8>)]) {
            let mut f = OpenOptions::new().write(true).open(&self.0).unwrap();
            for (off, bytes) in w { f.seek(SeekFrom::Start(*off)).unwrap(); f.write_all(bytes).unwrap(); }
            f.sync_all().unwrap();
        }
        fn poke(&self, off: u64, bytes: &[u8]) { self.apply(&[(off, bytes.to_vec())]); }
        fn peek(&self, off: u64, n: usize) -> Vec<u8> {
            let mut f = File::open(&self.0).unwrap();
            f.seek(SeekFrom::Start(off)).unwrap();
            let mut b = vec![0; n];
            f.read_exact(&mut b).unwrap();
            b
        }
    }

    /// Runs a real partitioning tool. A missing tool is a FAILURE that names
    /// the package, never a skip: a skipped case is the one that would have
    /// caught the bug.
    fn tool(cmd: &str, args: &[&str], input: Option<&str>) -> String {
        let mut c = Command::new(cmd);
        c.args(args).stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        let mut child = c.spawn().unwrap_or_else(|e| panic!(
            "{cmd} is required by these tests (Fedora: sfdisk is in util-linux, sgdisk in gdisk): {e}"));
        if let Some(text) = input { child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap(); }
        drop(child.stdin.take());
        let out = child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{cmd} {args:?} failed:\n{text}");
        text
    }

    fn sfdisk_make(img: &Img, script: &str) {
        tool("sfdisk", &["--no-reread", "--no-tell-kernel", "-q", img.0.to_str().unwrap()], Some(script));
    }

    /// `sfdisk -d`, minus the device-path prefixes so two dumps compare.
    fn dump(img: &Img) -> Vec<String> {
        let path = img.0.to_str().unwrap();
        tool("sfdisk", &["-d", path], None).lines()
            .map(|l| l.replace(path, "IMG").to_lowercase()).collect()
    }

    fn sgdisk_verify(img: &Img) {
        let out = tool("sgdisk", &["-v", img.0.to_str().unwrap()], None);
        assert!(out.contains("No problems found"), "sgdisk -v:\n{out}");
    }

    fn part(kind: &str, id: &str, first: u64, last: u64, name: &str) -> NewPartition {
        NewPartition { type_guid: kind.into(), unique_guid: id.into(), first_lba: first, last_lba: last,
            name: name.into(), attributes: 0 }
    }

    fn in_gpt_areas(len: u64, off: u64, n: usize) -> bool {
        let last = len / SECTOR - 1;
        let end = off + n as u64;
        end <= 34 * SECTOR || off >= (last - 32) * SECTOR
    }

    /// The whole job, against a real image made by sfdisk: find the hole, add
    /// an ESP and a root into it, write, then have two independent tools and
    /// the crate's own reader agree on the result.
    fn add_into_hole(first_lba: u64) {
        let sectors = 64 * GIB;
        let img = Img::new("hole", sectors);
        let last_usable = sectors - 34;
        let rec_start = (last_usable + 1 - 600 * MIB) / MIB * MIB;
        let data_start = 2048 + 100 * MIB + 16 * MIB;
        sfdisk_make(&img, &format!(
            "label: gpt\nlabel-id: 6A1F3C2E-0B7D-4E59-9C61-2D3E4F506172\nfirst-lba: {first_lba}\n\
             start=2048, size={}, type={ESP}, uuid=11111111-2222-4333-8444-555555555501, name=\"EFI system partition\"\n\
             start={}, size={}, type={MSR}, uuid=11111111-2222-4333-8444-555555555502, name=\"Microsoft reserved partition\"\n\
             start={data_start}, size={}, type={DATA}, uuid=11111111-2222-4333-8444-555555555503, name=\"Basic data partition\"\n\
             start={rec_start}, size={}, type={RECOVERY}, uuid=11111111-2222-4333-8444-555555555504, name=\"Recovery\", attrs=\"RequiredPartition GUID:63\"\n",
            100 * MIB, 2048 + 100 * MIB, 16 * MIB, 43 * GIB, 600 * MIB));
        let before_dump = dump(&img);
        let before = img.snapshot();
        assert_eq!(before.first_usable_lba(), first_lba);
        assert_eq!(before.disk_guid(), "6a1f3c2e-0b7d-4e59-9c61-2d3e4f506172");

        // Markers: inside every partition, in the hole, and (for a
        // Linux-made disk) in the gap between the array and FirstUsableLBA.
        // None of them may move.
        let hole_start = data_start + 43 * GIB;
        let mut markers = [2048 * SECTOR + 7, data_start * SECTOR + 4096, rec_start * SECTOR + 99,
            (hole_start + 5 * GIB) * SECTOR, (rec_start - 1) * SECTOR + 3, 40 * SECTOR, 2000 * SECTOR];
        markers.sort();
        for (i, m) in markers.iter().enumerate() { img.poke(*m, &[0xa5, i as u8, 0x5a]); }

        let regions = before.free_regions(2048, GIB);
        assert_eq!(regions, vec![(hole_start, rec_start - 1)], "the hole and only the hole");
        let (a, b) = regions[0];
        let esp_last = a + 2 * GIB + 256 * MIB - 1;
        let adds = [
            NewPartition { attributes: 1 << 63, ..part(ESP, "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeee0001", a, esp_last, "Rime ESP") },
            part(LINUX_ROOT, "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeee0002", esp_last + 1, b, "Rime OS"),
        ];
        let after = with_added(&before, &adds).unwrap();
        assert_eq!(changed_slots(&before, &after).unwrap(), vec![4, 5]);
        assert_eq!(slot_of(&after, "AAAAAAAA-bbbb-4ccc-8ddd-eeeeeeee0001"), Some(5));
        assert_eq!(slot_of(&after, "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeee0002"), Some(6));
        let w = writes(&before, &after).unwrap();
        for (off, bytes) in &w { assert!(in_gpt_areas(img.len(), *off, bytes.len()), "write at {off}"); }
        img.apply(&w);

        sgdisk_verify(&img);
        let after_dump = dump(&img);
        // Header lines (label, label-id, device, unit, first-lba, last-lba,
        // sector-size) and the four old partitions: unchanged.
        for line in &before_dump { assert!(after_dump.contains(line), "lost or changed: {line}"); }
        let new: Vec<&String> = after_dump.iter().filter(|l| !before_dump.contains(l)).collect();
        assert_eq!(new.len(), 2, "{new:?}");
        assert!(new[0].contains(&format!("start={a:>12}")) && new[0].contains(&format!("size={:>12}", esp_last - a + 1))
            && new[0].contains(ESP) && new[0].contains("uuid=aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeee0001")
            && new[0].contains("name=\"rime esp\"") && new[0].contains("attrs=\"guid:63\""), "{}", new[0]);
        assert!(new[1].contains(&format!("start={:>12}", esp_last + 1)) && new[1].contains(&format!("size={:>12}", b - esp_last))
            && new[1].contains(LINUX_ROOT) && new[1].contains("uuid=aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeee0002")
            && new[1].contains("name=\"rime os\""), "{}", new[1]);
        // The crate's reader, from the real file, agrees with the snapshot.
        assert_eq!(img.snapshot(), after);
        for (i, m) in markers.iter().enumerate() { assert_eq!(img.peek(*m, 3), vec![0xa5, i as u8, 0x5a]); }
    }

    #[test]
    fn adds_esp_and_root_on_a_windows_made_disk() { add_into_hole(34); }

    #[test]
    fn adds_esp_and_root_on_a_linux_made_disk() { add_into_hole(2048); }

    #[test]
    fn backup_file_round_trip_restores_both_copies_byte_exact() {
        let img = Img::new("undo", 4 * GIB);
        sfdisk_make(&img, &format!("label: gpt\nfirst-lba: 34\nstart=2048, size={}, type={ESP}\nstart={}, size={}, type={DATA}\n",
            100 * MIB, 4096 * 64, GIB));
        let len = img.len();
        let last = len / SECTOR - 1;
        let primary0 = img.peek(0, 34 * S);
        let backup0 = img.peek((last - 32) * SECTOR, 33 * S);
        let before = img.snapshot();
        let file = before.to_backup_file();
        assert_eq!(file.len(), 34340);
        let (a, b) = before.free_regions(2048, 2048)[0];
        let after = with_added(&before, &[part(LINUX_ROOT, "0b1c2d3e-4f50-4617-8829-3a4b5c6d7e8f", a, b, "root")]).unwrap();
        img.apply(&writes(&before, &after).unwrap());
        sgdisk_verify(&img);
        assert_ne!(img.peek(0, 34 * S), primary0);

        let restored = GptSnapshot::from_backup_file(&file).unwrap();
        assert_eq!(restored, before);
        let fresh = img.snapshot();
        img.apply(&writes(&fresh, &restored).unwrap());
        assert_eq!(img.peek(0, 34 * S), primary0);
        assert_eq!(img.peek((last - 32) * SECTOR, 33 * S), backup0);
        sgdisk_verify(&img);
    }

    #[test]
    fn writes_orders_primary_header_last_and_skips_unchanged_sectors() {
        let img = Img::new("order", 2 * GIB);
        sfdisk_make(&img, &format!("label: gpt\nstart=2048, size={}, type={ESP}\n", 100 * MIB));
        let before = img.snapshot();
        let last = img.len() / SECTOR - 1;
        let (a, b) = before.free_regions(2048, 2048)[0];
        let after = with_added(&before, &[part(LINUX_ROOT, "0b1c2d3e-4f50-4617-8829-3a4b5c6d7e80", a, b, "root")]).unwrap();
        let w = writes(&before, &after).unwrap();
        // Slot 1 lives in array sector 0 (four entries per sector): one
        // array sector per copy plus two headers. Nothing else.
        let offs: Vec<u64> = w.iter().map(|(o, _)| *o).collect();
        assert_eq!(offs, vec![(last - 32) * SECTOR, last * SECTOR, 2 * SECTOR, SECTOR]);
        assert!(w.iter().all(|(_, b)| b.len() == S));
        assert!(writes(&before, &before).unwrap().is_empty());
        // A slot in the second array sector touches that sector only.
        let mut many = before.clone();
        for i in 0..4u64 {
            let start = a + i * 2048;
            many = with_added(&many, &[part(LINUX_ROOT, &format!("0b1c2d3e-4f50-4617-8829-3a4b5c6d7e9{i}"), start, start + 2047, "x")]).unwrap();
        }
        let fifth = with_added(&many, &[part(LINUX_ROOT, "0b1c2d3e-4f50-4617-8829-3a4b5c6d7ea0", a + 8 * 2048, a + 9 * 2048 - 1, "y")]).unwrap();
        assert_eq!(slot_of(&fifth, "0b1c2d3e-4f50-4617-8829-3a4b5c6d7ea0"), Some(6));
        let offs: Vec<u64> = writes(&many, &fifth).unwrap().iter().map(|(o, _)| *o).collect();
        assert_eq!(offs, vec![(last - 31) * SECTOR, last * SECTOR, 3 * SECTOR, SECTOR]);
    }

    #[test]
    fn removal_changes_exactly_that_slot_and_reuses_it() {
        let img = Img::new("remove", 2 * GIB);
        sfdisk_make(&img, &format!(
            "label: gpt\nstart=2048, size={MIB}, type={ESP}, uuid=01010101-0000-4000-8000-000000000001\n\
             start={}, size={MIB}, type={DATA}, uuid=01010101-0000-4000-8000-000000000002\n\
             start={}, size={MIB}, type={DATA}, uuid=01010101-0000-4000-8000-000000000003\n", 2 * MIB, 3 * MIB));
        let before = img.snapshot();
        let after = with_removed(&before, "01010101-0000-4000-8000-000000000002").unwrap();
        assert_eq!(changed_slots(&before, &after).unwrap(), vec![1]);
        assert_eq!(slot_of(&after, "01010101-0000-4000-8000-000000000002"), None);
        assert_eq!(slot_of(&after, "01010101-0000-4000-8000-000000000003"), Some(3));
        img.apply(&writes(&before, &after).unwrap());
        sgdisk_verify(&img);
        assert_eq!(img.snapshot(), after);
        // The lowest empty slot is the one just emptied.
        let again = with_added(&after, &[part(LINUX_ROOT, "01010101-0000-4000-8000-0000000000aa", 2 * MIB, 3 * MIB - 1, "r")]).unwrap();
        assert_eq!(slot_of(&again, "01010101-0000-4000-8000-0000000000aa"), Some(2));
        assert!(with_removed(&before, "01010101-0000-4000-8000-0000000000ff").is_err());
    }

    fn small_disk() -> (Img, GptSnapshot) {
        let img = Img::new("refuse", 256 * MIB);
        sfdisk_make(&img, &format!("label: gpt\nstart=2048, size={}, type={ESP}, uuid=02020202-0000-4000-8000-000000000001\n", 16 * MIB));
        let s = img.snapshot();
        (img, s)
    }

    #[test]
    fn refuses_bad_additions() {
        let (_img, s) = small_disk();
        let (lo, hi) = (s.first_usable_lba(), s.last_usable_lba());
        let ok = |first, last| part(LINUX_ROOT, "03030303-0000-4000-8000-000000000001", first, last, "r");
        let free = 2048 + 16 * MIB;
        assert!(with_added(&s, &[ok(free, free + MIB - 1)]).is_ok());
        let cases: Vec<(&str, Vec<NewPartition>)> = vec![
            ("overlap", vec![ok(2048 + 15 * MIB, free + MIB)]),
            ("misaligned", vec![ok(free + 1, free + MIB)]),
            ("before usable", vec![ok(0, MIB)]),
            ("past usable", vec![ok(free, hi + 1)]),
            ("inverted", vec![ok(free + MIB, free)]),
            ("duplicate of existing", vec![NewPartition { unique_guid: "02020202-0000-4000-8000-000000000001".into(), ..ok(free, free + MIB - 1) }]),
            ("duplicate of disk guid", vec![NewPartition { unique_guid: s.disk_guid(), ..ok(free, free + MIB - 1) }]),
            ("duplicate within adds", vec![ok(free, free + MIB - 1), ok(free + MIB, free + 2 * MIB - 1)]),
            ("overlap within adds", vec![ok(free, free + MIB - 1),
                NewPartition { unique_guid: "03030303-0000-4000-8000-000000000002".into(), ..ok(free, free + 2 * MIB - 1) }]),
            ("nil unique", vec![NewPartition { unique_guid: "00000000-0000-0000-0000-000000000000".into(), ..ok(free, free + MIB - 1) }]),
            ("nil type", vec![NewPartition { type_guid: "00000000-0000-0000-0000-000000000000".into(), ..ok(free, free + MIB - 1) }]),
            ("bad guid", vec![NewPartition { unique_guid: "{03030303-0000-4000-8000-000000000001}".into(), ..ok(free, free + MIB - 1) }]),
            ("long name", vec![NewPartition { name: "x".repeat(37), ..ok(free, free + MIB - 1) }]),
            ("nul in name", vec![NewPartition { name: "a\0b".into(), ..ok(free, free + MIB - 1) }]),
            ("empty", vec![]),
        ];
        assert!(lo >= 34);
        for (what, adds) in cases {
            assert!(with_added(&s, &adds).is_err(), "{what} was accepted");
        }
        assert!(with_added(&s, &[NewPartition { name: "y".repeat(36), ..ok(free, free + MIB - 1) }]).is_ok());
    }

    #[test]
    fn refuses_when_no_slot_is_free() {
        // 128 one-MiB partitions from ONE sfdisk script, then a 129th.
        let img = Img::new("full", 160 * MIB);
        let mut script = String::from("label: gpt\n");
        for i in 0..128u64 { script += &format!("start={}, size={MIB}, type={DATA}\n", 2048 + i * MIB); }
        sfdisk_make(&img, &script);
        let s = img.snapshot();
        let (a, b) = s.free_regions(2048, 2048)[0];
        let err = with_added(&s, &[part(LINUX_ROOT, "04040404-0000-4000-8000-000000000001", a, b, "r")]).unwrap_err();
        assert!(err.to_string().contains("no empty entry"), "{err}");
        // And 129 at once on an empty table.
        let (_img2, small) = small_disk();
        let empty = with_removed(&small, "02020202-0000-4000-8000-000000000001").unwrap();
        let adds: Vec<NewPartition> = (0..129u64).map(|i| part(LINUX_ROOT,
            &format!("05050505-0000-4000-8000-{i:012x}"), 2048 + i * 2048, 2048 + i * 2048 + 2047, "p")).collect();
        assert!(with_added(&empty, &adds[..128]).is_ok());
        assert!(with_added(&empty, &adds).is_err());
    }

    #[test]
    fn backup_file_refusals() {
        let (_img, s) = small_disk();
        let file = s.to_backup_file();
        assert_eq!(GptSnapshot::from_backup_file(&file).unwrap(), s);
        for at in [0, 9, 20, 40, 600, 1100, 20000, file.len() - 600, file.len() - 1] {
            let mut bad = file.clone();
            bad[at] ^= 0x01;
            assert!(GptSnapshot::from_backup_file(&bad).is_err(), "flip at {at} accepted");
        }
        assert!(GptSnapshot::from_backup_file(&file[..file.len() - 1]).is_err());
        // A consistent file (valid CRC) whose GUID field disagrees with its table.
        let mut forged = file.clone();
        forged[16] ^= 0xff;
        let n = forged.len() - 4;
        let crc = crc32(&forged[..n]).to_le_bytes();
        forged[n..].copy_from_slice(&crc);
        assert!(GptSnapshot::from_backup_file(&forged).is_err());

        // A backup of another disk: refused by `writes`, both ways.
        let other = Img::new("other", 256 * MIB);
        sfdisk_make(&other, &format!("label: gpt\nstart=2048, size={}, type={ESP}\n", 16 * MIB));
        let o = other.snapshot();
        assert!(writes(&s, &GptSnapshot::from_backup_file(&o.to_backup_file()).unwrap()).is_err());
        assert!(writes(&o, &s).is_err());
        let longer = Img::new("longer", 512 * MIB);
        sfdisk_make(&longer, &format!("label: gpt\nlabel-id: {}\nstart=2048, size={}, type={ESP}\n", s.disk_guid(), 16 * MIB));
        let l = longer.snapshot();
        assert_eq!(l.disk_guid(), s.disk_guid());
        assert!(writes(&s, &l).is_err());
    }

    /// Re-stamps both header CRCs so a tampered header still parses: the
    /// refusal must come from `changed_slots`, not from a CRC check.
    fn recrc(s: &mut GptSnapshot) {
        for h in [&mut s.primary[S..2 * S], &mut s.backup[ARRAY_BYTES..]] {
            h[16..20].fill(0);
            let c = crc32(&h[..92]).to_le_bytes();
            h[16..20].copy_from_slice(&c);
        }
    }

    #[test]
    fn changed_slots_refuses_anything_but_entries_and_crcs() {
        let (_img, s) = small_disk();
        let edited = with_added(&s, &[part(LINUX_ROOT, "06060606-0000-4000-8000-000000000001", 2048 + 16 * MIB, 2048 + 17 * MIB - 1, "r")]).unwrap();
        assert_eq!(changed_slots(&s, &edited).unwrap(), vec![1]);

        let mut guid = edited.clone();
        guid.primary[S + 60] ^= 1;
        guid.backup[ARRAY_BYTES + 60] ^= 1;
        recrc(&mut guid);
        assert!(guid.layout().is_ok(), "the tampered table must still parse");
        assert!(changed_slots(&s, &guid).is_err());
        assert!(writes(&s, &guid).is_err());

        let mut tail = edited.clone();
        tail.primary[S + 300] ^= 1; // header bytes no reader checks
        assert!(tail.layout().is_ok());
        assert!(changed_slots(&s, &tail).is_err());

        let mut mbr = edited.clone();
        mbr.primary[0] ^= 1; // boot code: still a valid protective MBR
        assert!(mbr.layout().is_ok());
        assert!(changed_slots(&s, &mbr).is_err());
        assert!(writes(&s, &mbr).is_err());

        let mut split = edited.clone();
        split.backup[200] ^= 1; // backup array no longer mirrors primary
        assert!(changed_slots(&s, &split).is_err());
    }

    #[test]
    fn read_refuses_what_the_reader_refuses() {
        let img = Img::new("mbr", 64 * MIB);
        sfdisk_make(&img, "label: dos\nstart=2048, size=4096, type=83\n");
        assert!(GptSnapshot::read(&mut File::open(&img.0).unwrap(), img.len()).is_err());
        let (img2, _) = small_disk();
        img2.poke(2 * SECTOR + 100, &[0x42]); // primary array without a CRC update
        assert!(GptSnapshot::read(&mut File::open(&img2.0).unwrap(), img2.len()).is_err());
        let mut short = Cursor::new(vec![0u8; 4096]);
        assert!(GptSnapshot::read(&mut short, 4096).is_err());
    }

    #[test]
    fn free_regions_rounds_inward() {
        let img = Img::new("free", 64 * MIB);
        // Gaps: 34..2047 (Windows-style slack, smaller than 1 MiB once aligned),
        // 3073..8192 unaligned on both sides (-> 4096..8191), and the tail.
        sfdisk_make(&img, "label: gpt\nfirst-lba: 34\nstart=2048, size=1025, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4\n\
            start=8193, size=2047, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4\n");
        let s = img.snapshot();
        let last = s.last_usable_lba();
        let tail_end = (last + 1) / 2048 * 2048 - 1;
        assert_eq!(s.free_regions(2048, 1), vec![(4096, 8191), (10240, tail_end)]);
        assert_eq!(s.free_regions(2048, 4096)[0], (4096, 8191)); // exactly min: kept
        assert_eq!(s.free_regions(2048, 4097), vec![(10240, tail_end)]);
        assert_eq!(s.free_regions(1, 1)[0], (34, 2047));
    }

    #[test]
    fn crc32_and_guid_encoding() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(guid_bytes(ESP).unwrap(),
            [0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b]);
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        for _ in 0..200 {
            let mut b = [0u8; 16];
            for v in &mut b { x ^= x << 13; x ^= x >> 7; x ^= x << 17; *v = x as u8; }
            assert_eq!(guid_bytes(&crate::guid(&b)).unwrap(), b);
            assert_eq!(guid_bytes(&crate::guid(&b).to_uppercase()).unwrap(), b);
        }
        for bad in ["", "c12a7328f81f11d2ba4b00a0c93ec93b", "c12a7328-f81f-11d2-ba4b-00a0c93ec93", "g12a7328-f81f-11d2-ba4b-00a0c93ec93b",
            "c12a7328-f81f-11d2-ba4b00-a0c93ec93b", "+12a7328-f81f-11d2-ba4b-00a0c93ec93b", "c12a7328-f81f-11d2-ba4b-00a0c93ec93\u{e9}"] {
            assert!(guid_bytes(bad).is_err(), "{bad:?} accepted");
        }
        assert!(name_bytes(&"\u{1F600}".repeat(18)).is_ok()); // 36 code units
        assert!(name_bytes(&"\u{1F600}".repeat(19)).is_err());
    }

}
