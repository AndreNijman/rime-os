//! The payload layer: reading the Rime installer ISO and laying its boot files
//! out as a FAT32 filesystem the caller writes, raw, into a new EFI System
//! Partition.
//!
//! Three pieces, all pure std and all testable off Windows:
//!
//! - **SHA-256**, because the ISO is checked against its published digest
//!   before a single byte of it goes near a disk, and this crate takes no
//!   dependencies to get one.
//! - **An ISO 9660 reader** that only *locates* files: it answers "where in
//!   the image is `LiveOS/squashfs.img`, and how long is it". The bytes are
//!   then streamed by the caller straight from that range. It reads the
//!   primary tree only and refuses anything it would otherwise have to guess
//!   about (multi-extent files, interleaving, extents past the end).
//! - **A FAT32 planner** that never builds the filesystem in memory. A 1.6 GB
//!   squashfs cannot be buffered, so every file is given one contiguous run of
//!   clusters, the metadata (boot sectors, both FATs, directories) is handed
//!   back as small byte regions, and each file's data is a plain byte range
//!   the caller fills from the ISO. Metadata plus extents is the whole volume;
//!   nothing else needs writing.

use std::io::{self, Read, Seek, SeekFrom};

fn refuse(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Checked arithmetic that refuses instead of wrapping. Every size here comes
/// from a disk or an ISO header, and an overflow there means a hostile or
/// corrupt input, never a value worth computing with.
fn ck(v: Option<u64>, what: &str) -> io::Result<u64> {
    v.ok_or_else(|| refuse(format!("arithmetic overflow while computing {what}")))
}

// ---------------------------------------------------------------------------
// SHA-256
// ---------------------------------------------------------------------------

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Incremental SHA-256 (FIPS 180-4). Whole 64-byte blocks are compressed
/// straight from the caller's slice; only a partial tail is copied, so
/// hashing a 1.9 GB ISO in 1 MiB reads costs no allocation at all.
#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    tail: [u8; 64],
    tail_len: usize,
    total: u64,
}

impl Default for Sha256 {
    fn default() -> Self { Self::new() }
}

impl Sha256 {
    pub fn new() -> Self {
        Sha256 {
            state: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                    0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19],
            tail: [0; 64],
            tail_len: 0,
            total: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        // Bytes, not bits: the bit count is taken at the end, and 2^61 bytes
        // is not an input this program will ever see.
        self.total = self.total.wrapping_add(data.len() as u64);
        if self.tail_len > 0 {
            let take = (64 - self.tail_len).min(data.len());
            self.tail[self.tail_len..self.tail_len + take].copy_from_slice(&data[..take]);
            self.tail_len += take;
            data = &data[take..];
            if self.tail_len < 64 { return; }
            let block = self.tail;
            compress(&mut self.state, &block);
            self.tail_len = 0;
        }
        let (blocks, rest) = data.as_chunks::<64>();
        for block in blocks { compress(&mut self.state, block); }
        self.tail[..rest.len()].copy_from_slice(rest);
        self.tail_len = rest.len();
    }

    pub fn finalize(mut self) -> [u8; 32] {
        let bits = self.total.wrapping_mul(8);
        let mut pad = [0u8; 72];
        pad[0] = 0x80;
        // Pad to 56 mod 64, then the 64-bit big-endian length.
        let pad_len = if self.tail_len < 56 { 56 - self.tail_len } else { 120 - self.tail_len };
        pad[pad_len..pad_len + 8].copy_from_slice(&bits.to_be_bytes());
        let total = self.total;
        self.update(&pad[..pad_len + 8]);
        self.total = total;
        debug_assert_eq!(self.tail_len, 0);
        let mut out = [0u8; 32];
        for (o, s) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            *o = s.to_be_bytes();
        }
        out
    }
}

fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (wi, b) in w.iter_mut().zip(block.as_chunks::<4>().0) { *wi = u32::from_be_bytes(*b); }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for (k, wi) in K.iter().zip(w) {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(*k).wrapping_add(wi);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        h = g; g = f; f = e; e = d.wrapping_add(t1);
        d = c; c = b; b = a; a = t1.wrapping_add(t2);
    }
    for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) { *s = s.wrapping_add(v); }
}

/// Lowercase hex, the form `sha256sum` prints and published digests use.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[usize::from(b >> 4)] as char);
        s.push(DIGITS[usize::from(b & 15)] as char);
    }
    s
}

// ---------------------------------------------------------------------------
// ISO 9660
// ---------------------------------------------------------------------------

const ISO_BLOCK: u64 = 2048;
/// No directory in the installer ISO comes near this; anything larger is a
/// corrupt length field, and allocating it would be the bug.
const ISO_MAX_DIR_BYTES: u64 = 16 * 1024 * 1024;

/// Where a file lives inside the ISO image: `size` bytes starting at byte
/// `offset`. One contiguous extent, always, because anything else is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsoFile {
    pub path: String,
    pub offset: u64,
    pub size: u64,
}

#[derive(Clone)]
struct IsoRecord {
    lba: u64,
    size: u64,
    flags: u8,
    ext_attr: u8,
    interleaved: bool,
    name: Vec<u8>,
}

impl IsoRecord {
    fn parse(b: &[u8]) -> io::Result<IsoRecord> {
        let len = usize::from(b[0]);
        if len < 34 || len > b.len() || 33 + usize::from(b[32]) > len {
            return Err(refuse("malformed ISO 9660 directory record".into()));
        }
        Ok(IsoRecord {
            ext_attr: b[1],
            lba: u64::from(u32::from_le_bytes(b[2..6].try_into().unwrap())),
            size: u64::from(u32::from_le_bytes(b[10..14].try_into().unwrap())),
            flags: b[25],
            interleaved: b[26] != 0 || b[27] != 0,
            name: b[33..33 + usize::from(b[32])].to_vec(),
        })
    }
    fn is_dir(&self) -> bool { self.flags & 0x02 != 0 }
    /// The extent as (byte offset, length), refused if it is not wholly
    /// inside the image. Checked against the stream's real length, not the
    /// PVD's volume space size: a truncated download claims the full size.
    fn extent(&self, image_len: u64, what: &str) -> io::Result<(u64, u64)> {
        if self.ext_attr != 0 || self.interleaved {
            return Err(refuse(format!(
                "{what}: extended attribute records and interleaved files are not supported")));
        }
        let offset = ck(self.lba.checked_mul(ISO_BLOCK), "an ISO extent offset")?;
        let end = ck(offset.checked_add(self.size), "an ISO extent end")?;
        if end > image_len {
            return Err(refuse(format!(
                "{what}: its data ({offset}..{end}) lies beyond the end of the image ({image_len} \
                 bytes). The ISO is truncated or damaged.")));
        }
        Ok((offset, self.size))
    }
}

/// How a name is compared: version suffix (`;1`) and a trailing dot dropped,
/// ASCII uppercased, and optionally `-` folded to `_` because mkisofs maps
/// characters outside d-characters to `_` in the primary tree.
fn iso_key(name: &[u8], fold_dash: bool) -> String {
    let name = match name.iter().position(|b| *b == b';') { Some(i) => &name[..i], None => name };
    let name = name.strip_suffix(b".").unwrap_or(name);
    String::from_utf8_lossy(name).chars()
        .map(|c| if fold_dash && c == '-' { '_' } else { c.to_ascii_uppercase() })
        .collect()
}

fn read_exact_at<R: Read + Seek>(r: &mut R, offset: u64, len: u64) -> io::Result<Vec<u8>> {
    let n = usize::try_from(len).map_err(|_| refuse("read too large for this platform".into()))?;
    r.seek(SeekFrom::Start(offset))?;
    let mut b = vec![0; n];
    r.read_exact(&mut b)?;
    Ok(b)
}

/// Every record of one directory, `.` and `..` excluded.
fn iso_dir<R: Read + Seek>(r: &mut R, image_len: u64, dir: &IsoRecord, what: &str) -> io::Result<Vec<IsoRecord>> {
    let (offset, size) = dir.extent(image_len, what)?;
    if size > ISO_MAX_DIR_BYTES {
        return Err(refuse(format!("{what}: directory claims {size} bytes, which is not plausible")));
    }
    let data = read_exact_at(r, offset, size)?;
    let block = ISO_BLOCK as usize;
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        let room = (block - pos % block).min(data.len() - pos);
        let len = usize::from(data[pos]);
        // A zero length byte is padding to the end of the sector: records
        // never straddle a sector boundary, so the next one starts there.
        if len == 0 { pos += room; continue; }
        if len > room {
            return Err(refuse(format!("{what}: a directory record crosses a sector boundary")));
        }
        let rec = IsoRecord::parse(&data[pos..pos + len])?;
        if !(rec.name == [0] || rec.name == [1]) { out.push(rec); }
        pos += len;
    }
    Ok(out)
}

/// Locate `path` (components separated by `/`, case-insensitive) in the
/// ISO's primary directory tree. Joliet and Rock Ridge are ignored on
/// purpose: the primary tree is always present, and one tree read one way is
/// easier to trust than three trees reconciled.
pub fn iso_find<R: Read + Seek>(r: &mut R, path: &str) -> io::Result<IsoFile> {
    let image_len = r.seek(SeekFrom::End(0))?;
    if image_len < 17 * ISO_BLOCK {
        return Err(refuse(format!("not an ISO 9660 image: only {image_len} bytes")));
    }
    let pvd = read_exact_at(r, 16 * ISO_BLOCK, ISO_BLOCK)?;
    if pvd[0] != 1 || &pvd[1..6] != b"CD001" || pvd[6] != 1 {
        return Err(refuse("not an ISO 9660 image: no primary volume descriptor at sector 16".into()));
    }
    let block_size = u16::from_le_bytes([pvd[128], pvd[129]]);
    if u64::from(block_size) != ISO_BLOCK {
        return Err(refuse(format!("ISO logical block size is {block_size}, only 2048 is supported")));
    }
    let mut dir = IsoRecord::parse(&pvd[156..190])?;
    if !dir.is_dir() { return Err(refuse("ISO root directory record is not a directory".into())); }

    let parts: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    if parts.is_empty() || parts.len() > 32 {
        return Err(refuse(format!("unusable ISO path {path:?}")));
    }
    for (i, want) in parts.iter().enumerate() {
        let here = parts[..=i].join("/");
        let records = iso_dir(r, image_len, &dir, &format!("ISO directory containing {here}"))?;
        // An exact (case-insensitive) match wins over a dash-folded one, so
        // a directory holding both `A-B` and `A_B` still resolves exactly.
        let mut found: Vec<IsoRecord> = Vec::new();
        for fold in [false, true] {
            let key = iso_key(want.as_bytes(), fold);
            found = records.iter().filter(|r| iso_key(&r.name, fold) == key).cloned().collect();
            if !found.is_empty() { break; }
        }
        if found.is_empty() { return Err(refuse(format!("{here} is not in the ISO"))); }
        // A multi-extent file is several records with one name, all but the
        // last flagged 0x80. Checked before ambiguity so the message names
        // the real reason.
        if found.iter().any(|r| r.flags & 0x80 != 0) {
            return Err(refuse(format!(
                "{here} is stored in several extents in the ISO; only single-extent files are supported")));
        }
        if found.len() > 1 { return Err(refuse(format!("{here} matches more than one ISO entry"))); }
        let rec = found.pop().unwrap();
        let last = i + 1 == parts.len();
        if last {
            if rec.is_dir() { return Err(refuse(format!("{here} is a directory in the ISO, not a file"))); }
            let (offset, size) = rec.extent(image_len, &here)?;
            return Ok(IsoFile { path: here, offset, size });
        }
        if !rec.is_dir() { return Err(refuse(format!("{here} is a file in the ISO, not a directory"))); }
        dir = rec;
    }
    unreachable!("the loop returns on its last component")
}

// ---------------------------------------------------------------------------
// FAT32
// ---------------------------------------------------------------------------

const SECTOR: u64 = 512;
const RESERVED_SECTORS: u64 = 32;
const FAT_COPIES: u64 = 2;
/// The spec's own rule: fewer clusters than this is FAT16, whatever the BPB says.
const FAT32_MIN_CLUSTERS: u64 = 65525;
const FAT32_MAX_VOLUME: u64 = 32 * 1024 * 1024 * 1024;
const END_OF_CHAIN: u32 = 0x0FFF_FFFF;
/// 2026-01-01 00:00:00, in FAT's packed date (time is 0). Fixed so that two
/// plans from the same inputs are byte-identical, which is what lets a test,
/// or a person, compare them.
const FAT_DATE: u16 = ((2026 - 1980) << 9) | (1 << 5) | 1;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
/// Windows NT case flags in byte 12 of a short entry.
const CASE_LOWER_BASE: u8 = 0x08;
const CASE_LOWER_EXT: u8 = 0x10;

/// A file to place: `path` uses `/` and its directories are implied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FatFile {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone)]
enum Child { Dir(usize), File(usize) }

#[derive(Debug, Clone)]
struct Dir {
    name: [u8; 11],
    case: u8,
    parent: usize,
    children: Vec<Child>,
    first: u32,
    count: u32,
}

#[derive(Debug, Clone)]
struct Placed {
    name: [u8; 11],
    case: u8,
    first: u32,
    count: u32,
    size: u64,
}

/// A planned FAT32 volume. Writing every [`metadata`](Self::metadata) region
/// and filling every [`file_extent`](Self::file_extent) produces the complete
/// filesystem; bytes outside those ranges are free space and may hold
/// anything.
#[derive(Debug, Clone)]
pub struct FatLayout {
    volume_bytes: u64,
    total_sectors: u32,
    sectors_per_cluster: u8,
    fat_sectors: u32,
    clusters: u32,
    label: [u8; 11],
    volume_id: u32,
    dirs: Vec<Dir>,
    files: Vec<Placed>,
    used: u32,
}

/// Characters a short name may hold besides A-Z and 0-9.
const SHORT_NAME_EXTRA: &[u8] = b"!#$%&'()-@^_`{}~";

fn short_char_ok(c: u8) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit() || SHORT_NAME_EXTRA.contains(&c)
}

/// One path component as an 8.3 short name plus its NT case flags. No long
/// file names are written, so a component that does not fit 8.3 is refused
/// rather than mangled into `SQUASH~1.IMG`, which nothing downstream expects.
fn short_name(component: &str) -> io::Result<([u8; 11], u8)> {
    let bad = || refuse(format!(
        "{component:?} is not a valid 8.3 name (up to 8 characters, optionally a dot and up to 3 \
         more, using A-Z 0-9 and !#$%&'()-@^_`{{}}~). This volume writes no long file names."));
    let (base, ext) = match component.split_once('.') {
        Some((b, e)) => (b, e),
        None => (component, ""),
    };
    if base.is_empty() || base.len() > 8 || ext.len() > 3 || ext.contains('.')
        || (component.contains('.') && ext.is_empty()) {
        return Err(bad());
    }
    let mut name = [b' '; 11];
    let mut case = 0u8;
    let (base_slot, ext_slot) = name.split_at_mut(8);
    for (part, slot, flag) in [(base, base_slot, CASE_LOWER_BASE), (ext, ext_slot, CASE_LOWER_EXT)] {
        let lower = part.bytes().any(|c| c.is_ascii_lowercase());
        let upper = part.bytes().any(|c| c.is_ascii_uppercase());
        for (s, c) in slot.iter_mut().zip(part.bytes()) {
            let c = c.to_ascii_uppercase();
            if !short_char_ok(c) { return Err(bad()); }
            *s = c;
        }
        // Mixed case ("LiveOS") cannot be expressed without a long name; it
        // is stored uppercase, which FAT's case-insensitive lookup accepts.
        if lower && !upper { case |= flag; }
    }
    Ok((name, case))
}

fn volume_label(label: &str) -> io::Result<[u8; 11]> {
    let up = label.to_ascii_uppercase();
    if up.is_empty() || up.len() > 11 || up.starts_with(' ')
        || !up.bytes().all(|c| c == b' ' || short_char_ok(c)) {
        return Err(refuse(format!(
            "volume label {label:?} must be 1 to 11 characters of A-Z 0-9 space and !#$%&'()-@^_`{{}}~")));
    }
    let mut out = [b' '; 11];
    out[..up.len()].copy_from_slice(up.as_bytes());
    Ok(out)
}

/// Sectors per cluster from Microsoft's FAT32 table (fatgen103): 4 KiB
/// clusters up to 8 GiB, 8 KiB to 16 GiB, 16 KiB to 32 GiB.
fn sectors_per_cluster(volume_bytes: u64) -> io::Result<u8> {
    const GIB: u64 = 1024 * 1024 * 1024;
    Ok(match volume_bytes {
        v if v <= 8 * GIB => 8,
        v if v <= 16 * GIB => 16,
        v if v <= FAT32_MAX_VOLUME => 32,
        v => return Err(refuse(format!(
            "a {v}-byte volume is larger than 32 GiB; this program only lays out FAT32 volumes up to \
             32 GiB, which is far more than an installer ESP needs"))),
    })
}

/// Plan a FAT32 volume of exactly `volume_bytes` holding `files`, in that
/// order, each in one contiguous cluster run.
///
/// Fixed choices the caller can rely on: 512-byte sectors, 32 reserved
/// sectors, two mirrored FATs, FSInfo at sector 1, backup boot sector at 6
/// with its FSInfo copy at 7, root directory at cluster 2, media 0xF8.
/// **Hidden sectors is written as 0**: it is the partition's starting LBA,
/// which only a BIOS boot sector reads, and neither UEFI firmware nor Linux
/// nor Windows uses it to mount a volume, so the plan does not need to know
/// where the partition will be. `volume_id` goes in the BPB unchanged; the
/// caller chooses it (Windows derives it from the format time).
pub fn plan_fat32(volume_bytes: u64, label: &str, volume_id: u32, files: &[FatFile]) -> io::Result<FatLayout> {
    if !volume_bytes.is_multiple_of(SECTOR) {
        return Err(refuse(format!("volume size {volume_bytes} is not a whole number of 512-byte sectors")));
    }
    let spc = sectors_per_cluster(volume_bytes)?;
    let spc64 = u64::from(spc);
    let cluster_bytes = spc64 * SECTOR;
    let total_sectors = volume_bytes / SECTOR;
    let label = volume_label(label)?;
    let too_small = || refuse(format!(
        "a {volume_bytes}-byte volume holds fewer than {FAT32_MIN_CLUSTERS} clusters of {cluster_bytes} \
         bytes, which by the FAT specification's own rule would be FAT16, not FAT32. It needs to be at \
         least about {} bytes.", (FAT32_MIN_CLUSTERS + 2) * cluster_bytes + 2 * 1024 * 1024));

    // The FAT has to describe the clusters that are left once the FATs
    // themselves are carved out. Grow it until it covers them; it only ever
    // grows, so this settles in a few rounds, and a FAT slightly larger than
    // needed is legal (its tail stays zero).
    let mut fat_sectors = 1u64;
    let clusters = loop {
        let overhead = ck(fat_sectors.checked_mul(FAT_COPIES).and_then(|v| v.checked_add(RESERVED_SECTORS)),
            "the FAT size")?;
        let data = total_sectors.checked_sub(overhead).ok_or_else(too_small)?;
        let clusters = data / spc64;
        let need = ck(clusters.checked_add(2).and_then(|v| v.checked_mul(4)), "the FAT size")?.div_ceil(SECTOR);
        if need <= fat_sectors { break clusters; }
        fat_sectors = need;
    };
    if clusters < FAT32_MIN_CLUSTERS { return Err(too_small()); }

    let (mut dirs, mut placed) = (vec![Dir { name: label, case: 0, parent: 0, children: Vec::new(), first: 0, count: 0 }], Vec::new());
    for (fi, f) in files.iter().enumerate() {
        let parts: Vec<&str> = f.path.split('/').collect();
        if f.path.is_empty() || parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
            return Err(refuse(format!("unusable file path {:?}: use relative paths like EFI/BOOT/BOOTX64.EFI", f.path)));
        }
        if f.size > u64::from(u32::MAX) {
            return Err(refuse(format!("{} is {} bytes; FAT32 files stop at 4 GiB - 1", f.path, f.size)));
        }
        let mut at = 0usize;
        for (i, part) in parts.iter().enumerate() {
            let (name, case) = short_name(part)?;
            let here = parts[..=i].join("/");
            let existing = dirs[at].children.iter().find(|c| match c {
                Child::Dir(d) => dirs[*d].name == name,
                Child::File(p) => placed.get(*p).is_some_and(|p: &Placed| p.name == name),
            }).cloned();
            let last = i + 1 == parts.len();
            match (existing, last) {
                (Some(Child::Dir(_)), true) => return Err(refuse(format!(
                    "{} names a file at {here}, which another path already uses as a directory", f.path))),
                (Some(Child::File(_)), true) => return Err(refuse(format!(
                    "{} is listed twice (FAT names are case-insensitive)", f.path))),
                (Some(Child::File(_)), false) => return Err(refuse(format!(
                    "{} needs {here} to be a directory, but it is already a file", f.path))),
                (Some(Child::Dir(d)), false) => at = d,
                (None, false) => {
                    dirs.push(Dir { name, case, parent: at, children: Vec::new(), first: 0, count: 0 });
                    let d = dirs.len() - 1;
                    dirs[at].children.push(Child::Dir(d));
                    at = d;
                }
                (None, true) => {
                    debug_assert_eq!(placed.len(), fi);
                    placed.push(Placed { name, case, first: 0, count: 0, size: f.size });
                    dirs[at].children.push(Child::File(fi));
                }
            }
        }
    }

    // Directories first (root is index 0, so it lands on cluster 2), then
    // files in the order given, each from a fresh cluster.
    let mut next = 2u64;
    let mut needed_clusters = 0u64;
    let mut take = |count: u64| -> io::Result<u32> {
        let first = next;
        next = ck(next.checked_add(count), "cluster allocation")?;
        needed_clusters = ck(needed_clusters.checked_add(count), "cluster allocation")?;
        // Clamping is harmless: anything past u32 is past `clusters` too,
        // and the fit check below refuses before a clamped value is used.
        Ok(u32::try_from(first).unwrap_or(u32::MAX))
    };
    for (d, dir) in dirs.iter_mut().enumerate() {
        let entries = dir.children.len() as u64 + if d == 0 { 1 } else { 2 };
        let count = ck(entries.checked_mul(32), "a directory size")?.div_ceil(cluster_bytes).max(1);
        dir.first = take(count)?;
        dir.count = u32::try_from(count).unwrap_or(u32::MAX);
    }
    for p in &mut placed {
        let count = p.size.div_ceil(cluster_bytes);
        if count == 0 { continue; }
        p.first = take(count)?;
        p.count = u32::try_from(count).unwrap_or(u32::MAX);
    }
    if needed_clusters > clusters {
        return Err(refuse(format!(
            "the files and directories need {} bytes of clusters but the volume has only {} bytes of \
             data space ({clusters} clusters of {cluster_bytes} bytes)",
            ck(needed_clusters.checked_mul(cluster_bytes), "the space needed")?, clusters * cluster_bytes)));
    }

    Ok(FatLayout {
        volume_bytes,
        // All of these fit: the volume is at most 32 GiB, checked above.
        total_sectors: u32::try_from(total_sectors).map_err(|_| refuse("too many sectors".into()))?,
        sectors_per_cluster: spc,
        fat_sectors: u32::try_from(fat_sectors).map_err(|_| refuse("FAT too large".into()))?,
        clusters: u32::try_from(clusters).map_err(|_| refuse("too many clusters".into()))?,
        label,
        volume_id,
        dirs,
        files: placed,
        used: u32::try_from(needed_clusters).map_err(|_| refuse("too many clusters".into()))?,
    })
}

fn put16(b: &mut [u8], at: usize, v: u16) { b[at..at + 2].copy_from_slice(&v.to_le_bytes()); }
fn put32(b: &mut [u8], at: usize, v: u32) { b[at..at + 4].copy_from_slice(&v.to_le_bytes()); }

/// The text a BIOS shows if someone tries to boot this volume directly.
const NOT_BOOTABLE: &[u8] = b"This is not a bootable disk. It holds the Rime OS installer's files,\r\n\
which start from the UEFI boot menu.\r\nPress any key to try again.\r\n\0";

fn dir_entry(name: &[u8; 11], case: u8, attr: u8, cluster: u32, size: u32) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[..11].copy_from_slice(name);
    e[11] = attr;
    e[12] = case;
    put16(&mut e, 16, FAT_DATE); // created
    put16(&mut e, 18, FAT_DATE); // accessed
    put16(&mut e, 20, (cluster >> 16) as u16);
    put16(&mut e, 24, FAT_DATE); // written
    put16(&mut e, 26, cluster as u16);
    put32(&mut e, 28, size);
    e
}

impl FatLayout {
    pub fn volume_bytes(&self) -> u64 { self.volume_bytes }

    fn cluster_bytes(&self) -> u64 { u64::from(self.sectors_per_cluster) * SECTOR }

    fn cluster_offset(&self, cluster: u32) -> u64 {
        // Bounded by the volume size (at most 32 GiB), validated in the plan.
        (RESERVED_SECTORS + FAT_COPIES * u64::from(self.fat_sectors)) * SECTOR
            + u64::from(cluster - 2) * self.cluster_bytes()
    }

    /// Where file `i` (index into the `files` given to [`plan_fat32`]) must be
    /// written: (byte offset within the volume, length). The length is the
    /// file's size; slack after it in the last cluster is never read. A
    /// zero-length file has no clusters and answers (0, 0): nothing to write.
    /// Panics if `i` is out of range, like indexing.
    pub fn file_extent(&self, i: usize) -> (u64, u64) {
        let p = &self.files[i];
        if p.size == 0 { return (0, 0); }
        (self.cluster_offset(p.first), p.size)
    }

    fn boot_sector(&self) -> [u8; 512] {
        let mut b = [0u8; 512];
        b[..3].copy_from_slice(&[0xEB, 0x58, 0x90]); // jmp to the stub at 0x5A
        b[3..11].copy_from_slice(b"RIMEOS  ");
        put16(&mut b, 11, SECTOR as u16);
        b[13] = self.sectors_per_cluster;
        put16(&mut b, 14, RESERVED_SECTORS as u16);
        b[16] = FAT_COPIES as u8;
        // 17 root entries, 19 total sectors (16-bit): 0 on FAT32.
        b[21] = 0xF8;
        // 22 sectors per FAT (16-bit): 0 on FAT32.
        put16(&mut b, 24, 63); // sectors per track, nominal
        put16(&mut b, 26, 255); // heads, nominal
        put32(&mut b, 28, 0); // hidden sectors: see plan_fat32
        put32(&mut b, 32, self.total_sectors);
        put32(&mut b, 36, self.fat_sectors);
        // 40 ext flags 0 = FATs mirrored; 42 version 0.0.
        put32(&mut b, 44, 2); // root directory cluster
        put16(&mut b, 48, 1); // FSInfo sector
        put16(&mut b, 50, 6); // backup boot sector
        b[64] = 0x80; // drive number
        b[66] = 0x29; // extended boot signature: the next three fields exist
        put32(&mut b, 67, self.volume_id);
        b[71..82].copy_from_slice(&self.label);
        b[82..90].copy_from_slice(b"FAT32   ");
        // Real-mode stub (the same one mkfs.fat writes): print the message at
        // 0x7C77, wait for a key, ask the BIOS for the next boot device.
        const STUB: [u8; 29] = [0x0E, 0x1F, 0xBE, 0x77, 0x7C, 0xAC, 0x22, 0xC0, 0x74, 0x0B, 0x56,
            0xB4, 0x0E, 0xBB, 0x07, 0x00, 0xCD, 0x10, 0x5E, 0xEB, 0xF0, 0x32, 0xE4, 0xCD, 0x16,
            0xCD, 0x19, 0xEB, 0xFE];
        b[0x5A..0x77].copy_from_slice(&STUB);
        b[0x77..0x77 + NOT_BOOTABLE.len()].copy_from_slice(NOT_BOOTABLE);
        b[510] = 0x55;
        b[511] = 0xAA;
        b
    }

    fn fsinfo(&self) -> [u8; 512] {
        let mut b = [0u8; 512];
        put32(&mut b, 0, 0x4161_5252);
        put32(&mut b, 484, 0x6141_7272);
        let free = self.clusters - self.used;
        put32(&mut b, 488, free);
        put32(&mut b, 492, if free == 0 { u32::MAX } else { 2 + self.used });
        put32(&mut b, 508, 0xAA55_0000);
        b
    }

    fn fat(&self) -> Vec<u8> {
        let mut fat = vec![0u8; self.fat_sectors as usize * SECTOR as usize];
        put32(&mut fat, 0, 0x0FFF_FFF8);
        // Entry 1's top bits are the clean-shutdown and no-error flags: set.
        put32(&mut fat, 4, 0x0FFF_FFFF);
        let runs = self.dirs.iter().map(|d| (d.first, d.count))
            .chain(self.files.iter().filter(|p| p.count > 0).map(|p| (p.first, p.count)));
        for (first, count) in runs {
            for c in first..first + count {
                let next = if c + 1 == first + count { END_OF_CHAIN } else { c + 1 };
                put32(&mut fat, c as usize * 4, next);
            }
        }
        fat
    }

    fn directory(&self, d: usize) -> Vec<u8> {
        let dir = &self.dirs[d];
        let mut out = Vec::with_capacity(dir.count as usize * self.cluster_bytes() as usize);
        if d == 0 {
            out.extend_from_slice(&dir_entry(&self.label, 0, ATTR_VOLUME_ID, 0, 0));
        } else {
            let parent = if dir.parent == 0 { 0 } else { self.dirs[dir.parent].first };
            out.extend_from_slice(&dir_entry(b".          ", 0, ATTR_DIRECTORY, dir.first, 0));
            out.extend_from_slice(&dir_entry(b"..         ", 0, ATTR_DIRECTORY, parent, 0));
        }
        for child in &dir.children {
            let e = match child {
                Child::Dir(c) => { let c = &self.dirs[*c]; dir_entry(&c.name, c.case, ATTR_DIRECTORY, c.first, 0) }
                Child::File(f) => { let f = &self.files[*f]; dir_entry(&f.name, f.case, ATTR_ARCHIVE, f.first, f.size as u32) }
            };
            out.extend_from_slice(&e);
        }
        // Zero-filled to the end of its clusters: a zero first byte is the
        // end-of-directory marker, and stale bytes here would read as entries.
        out.resize(dir.count as usize * self.cluster_bytes() as usize, 0);
        out
    }

    /// Every metadata region as (byte offset within the volume, bytes),
    /// sorted and non-overlapping: the whole reserved area (boot sector,
    /// FSInfo, backups, zeros between), both FATs in full, and every
    /// directory's clusters in full. Whole regions are written, zeros
    /// included, so nothing left in the partition from before can be read
    /// back as a FAT entry or a directory entry.
    pub fn metadata(&self) -> Vec<(u64, Vec<u8>)> {
        let mut reserved = vec![0u8; (RESERVED_SECTORS * SECTOR) as usize];
        let boot = self.boot_sector();
        let info = self.fsinfo();
        for base in [0usize, 6] {
            reserved[base * 512..base * 512 + 512].copy_from_slice(&boot);
            reserved[(base + 1) * 512..(base + 2) * 512].copy_from_slice(&info);
            // Sector 2 of each boot region carries only the signature.
            reserved[(base + 3) * 512 - 2..(base + 3) * 512].copy_from_slice(&[0x55, 0xAA]);
        }
        let fat = self.fat();
        let fat_bytes = u64::from(self.fat_sectors) * SECTOR;
        let mut out = vec![(0, reserved), (RESERVED_SECTORS * SECTOR, fat.clone()),
            (RESERVED_SECTORS * SECTOR + fat_bytes, fat)];
        for d in 0..self.dirs.len() {
            out.push((self.cluster_offset(self.dirs[d].first), self.directory(d)));
        }
        out.sort_by_key(|(o, _)| *o);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File, OpenOptions};
    use std::io::{Cursor, Write};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn digest(data: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(data);
        hex(&h.finalize())
    }

    fn have(tool: &str) -> bool {
        Command::new(tool).arg("--help").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok()
    }

    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) { let _ = fs::remove_file(&self.0); }
    }
    fn temp(tag: &str) -> Temp {
        static N: AtomicU32 = AtomicU32::new(0);
        Temp(std::env::temp_dir().join(format!("rime-payload-{}-{tag}-{}", std::process::id(),
            N.fetch_add(1, Ordering::Relaxed))))
    }

    /// Deterministic pseudo-random bytes (xorshift64*), no crate needed.
    fn noise(seed: u64, n: usize) -> Vec<u8> {
        let mut x = seed | 1;
        (0..n).map(|_| { x ^= x >> 12; x ^= x << 25; x ^= x >> 27; (x.wrapping_mul(0x2545F4914F6CDD1D) >> 56) as u8 }).collect()
    }

    #[test]
    fn sha256_fips_vectors() {
        assert_eq!(digest(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(digest(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
        let mut h = Sha256::new();
        let block = [b'a'; 997];
        let mut left = 1_000_000usize;
        let mut step = 1usize;
        while left > 0 {
            let n = step.min(left).min(block.len());
            h.update(&block[..n]);
            left -= n;
            step = step * 7 % 991 + 1;
        }
        assert_eq!(hex(&h.finalize()), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    }

    #[test]
    fn sha256_agrees_with_sha256sum() {
        if !have("sha256sum") { println!("SKIP: sha256sum not installed"); return; }
        let data = noise(0x5eed, 10 * 1024 * 1024);
        let t = temp("sha.bin");
        fs::write(&t.0, &data).unwrap();
        let out = Command::new("sha256sum").arg(&t.0).output().unwrap();
        assert!(out.status.success());
        let theirs = String::from_utf8(out.stdout).unwrap();
        let mut h = Sha256::new();
        for chunk in data.chunks(65_537) { h.update(chunk); }
        assert_eq!(theirs.split_whitespace().next().unwrap(), hex(&h.finalize()));
    }

    // ---- ISO -------------------------------------------------------------

    fn iso_record(name: &[u8], lba: u32, size: u32, flags: u8) -> Vec<u8> {
        let mut len = 33 + name.len();
        if len % 2 == 1 { len += 1; }
        let mut r = vec![0u8; len];
        r[0] = len as u8;
        r[2..6].copy_from_slice(&lba.to_le_bytes());
        r[6..10].copy_from_slice(&lba.to_be_bytes());
        r[10..14].copy_from_slice(&size.to_le_bytes());
        r[14..18].copy_from_slice(&size.to_be_bytes());
        r[25] = flags;
        r[28] = 1; // volume sequence number
        r[32] = name.len() as u8;
        r[33..33 + name.len()].copy_from_slice(name);
        r
    }

    /// Records laid into sectors the way mastering tools do: never across a
    /// sector boundary, zero padding to the next sector instead.
    fn iso_dir_bytes(records: &[Vec<u8>], sectors: usize) -> Vec<u8> {
        let mut out = vec![0u8; sectors * 2048];
        let mut pos = 0;
        for r in records {
            if pos % 2048 + r.len() > 2048 { pos = (pos / 2048 + 1) * 2048; }
            out[pos..pos + r.len()].copy_from_slice(r);
            pos += r.len();
        }
        assert!(pos <= out.len());
        out
    }

    /// Hand-built ISO 9660: PVD, terminator, root at 18, SUB_DIR at 19-20
    /// (two sectors, TARGET.IMG in the second), file data from 21.
    fn synthetic_iso() -> Vec<u8> {
        let mut img = vec![0u8; 30 * 2048];
        let pvd = &mut img[16 * 2048..17 * 2048];
        pvd[0] = 1; pvd[1..6].copy_from_slice(b"CD001"); pvd[6] = 1;
        pvd[80..84].copy_from_slice(&30u32.to_le_bytes());
        pvd[128..130].copy_from_slice(&2048u16.to_le_bytes());
        pvd[156..190].copy_from_slice(&iso_record(&[0], 18, 2048, 2));
        let term = &mut img[17 * 2048..18 * 2048];
        term[0] = 255; term[1..6].copy_from_slice(b"CD001"); term[6] = 1;

        let root = iso_dir_bytes(&[
            iso_record(&[0], 18, 2048, 2), iso_record(&[1], 18, 2048, 2),
            iso_record(b"HELLO.TXT;1", 21, 5, 0),
            iso_record(b"BIG.BIN;1", 22, 2048, 0x80),
            iso_record(b"BIG.BIN;1", 23, 100, 0),
            iso_record(b"BEYOND.BIN;1", 1000, 10, 0),
            iso_record(b"SUB_DIR", 19, 4096, 2),
        ], 1);
        img[18 * 2048..19 * 2048].copy_from_slice(&root);
        let mut sub = vec![iso_record(&[0], 19, 4096, 2), iso_record(&[1], 18, 2048, 2)];
        for i in 0..60 { sub.push(iso_record(format!("FILE{i:04}.DAT;1").as_bytes(), 21, 1, 0)); }
        sub.push(iso_record(b"TARGET.IMG;1", 24, 9, 0));
        let sub = iso_dir_bytes(&sub, 2);
        assert!(sub[2048] != 0, "the subdirectory must actually spill into its second sector");
        img[19 * 2048..21 * 2048].copy_from_slice(&sub);
        img[21 * 2048..21 * 2048 + 5].copy_from_slice(b"hello");
        img[24 * 2048..24 * 2048 + 9].copy_from_slice(b"hsqstarg!");
        img
    }

    #[test]
    fn iso_synthetic_lookups_and_refusals() {
        let mut iso = Cursor::new(synthetic_iso());
        let f = iso_find(&mut iso, "hello.txt").unwrap();
        assert_eq!((f.offset, f.size), (21 * 2048, 5));
        let t = iso_find(&mut iso, "/sub-dir/target.img").unwrap();
        assert_eq!((t.offset, t.size, t.path.as_str()), (24 * 2048, 9, "sub-dir/target.img"));
        assert_eq!(&iso.get_ref()[t.offset as usize..t.offset as usize + 4], b"hsqs");
        let err = |p: &str, iso: &mut Cursor<Vec<u8>>| iso_find(iso, p).unwrap_err().to_string();
        assert!(err("BIG.BIN", &mut iso).contains("several extents"));
        assert!(err("beyond.bin", &mut iso).contains("beyond the end"));
        assert!(err("sub_dir", &mut iso).contains("is a directory"));
        assert!(err("hello.txt/x", &mut iso).contains("not a directory"));
        assert!(err("missing.txt", &mut iso).contains("not in the ISO"));
        assert!(err("", &mut iso).contains("unusable"));
        let mut bad = synthetic_iso();
        bad[16 * 2048 + 128..16 * 2048 + 130].copy_from_slice(&512u16.to_le_bytes());
        assert!(err("hello.txt", &mut Cursor::new(bad)).contains("block size"));
        let mut bad = synthetic_iso();
        bad[16 * 2048 + 1] = b'X';
        assert!(err("hello.txt", &mut Cursor::new(bad)).contains("primary volume descriptor"));
        // A record that would straddle the sector boundary is refused.
        let mut bad = synthetic_iso();
        let at = 19 * 2048 + 68 + 48 * 41; // the padding after the last record of the first sector
        bad[at] = 40;
        assert!(err("sub-dir/target.img", &mut Cursor::new(bad)).contains("sector boundary"));
    }

    const REAL_ISO: &str = "/var/lab-scratch/winiso/rime-os-netinstall-x86_64.iso";

    #[test]
    fn iso_real_installer_files() {
        let Ok(mut f) = File::open(REAL_ISO) else { println!("SKIP: {REAL_ISO} not present"); return; };
        for (path, size) in [
            ("LiveOS/squashfs.img", 1_607_593_984u64), ("images/pxeboot/vmlinuz", 18_745_704),
            ("images/pxeboot/initrd.img", 215_787_658), ("EFI/BOOT/BOOTX64.EFI", 949_424),
            ("EFI/BOOT/grubx64.efi", 4_046_544), ("EFI/BOOT/mmx64.efi", 848_080),
        ] {
            let found = iso_find(&mut f, path).unwrap_or_else(|e| panic!("{path}: {e}"));
            assert_eq!(found.size, size, "{path}");
            if path.ends_with("squashfs.img") {
                f.seek(SeekFrom::Start(found.offset)).unwrap();
                let mut magic = [0u8; 4];
                f.read_exact(&mut magic).unwrap();
                assert_eq!(&magic, b"hsqs");
            }
        }
    }

    // ---- FAT32 -----------------------------------------------------------

    const MIB: u64 = 1024 * 1024;

    fn real_files() -> Vec<FatFile> {
        [("EFI/rimeinst/shimx64.efi", 949_424u64), ("EFI/rimeinst/grubx64.efi", 4_046_544),
         ("EFI/rimeinst/mmx64.efi", 848_080), ("EFI/rimeinst/grub.cfg", 1500),
         ("rimeinst/vmlinuz", 18_745_704), ("rimeinst/initrd.img", 215_787_658),
         ("rimeinst/LiveOS/squashfs.img", 1_607_593_984), ("rimeinst/handoff.cfg", 400)]
            .into_iter().map(|(p, s)| FatFile { path: p.into(), size: s }).collect()
    }

    fn pattern(file: usize, from: u64, n: usize) -> Vec<u8> {
        (0..n as u64).map(|k| { let k = from + k; ((k.wrapping_mul(2_654_435_761) >> 11) as u8) ^ (k as u8) ^ (file as u8).wrapping_mul(37) }).collect()
    }

    /// Writes the plan into a sparse file: metadata, then file data either in
    /// full or only the first and last MiB (`partial`).
    fn build(layout: &FatLayout, files: &[FatFile], partial: bool, tag: &str) -> Temp {
        let t = temp(tag);
        let mut img = OpenOptions::new().create_new(true).read(true).write(true).open(&t.0).unwrap();
        img.set_len(layout.volume_bytes()).unwrap();
        let regions = layout.metadata();
        for w in regions.windows(2) { assert!(w[0].0 + w[0].1.len() as u64 <= w[1].0, "regions overlap"); }
        for (off, bytes) in &regions {
            img.seek(SeekFrom::Start(*off)).unwrap();
            img.write_all(bytes).unwrap();
        }
        for (i, f) in files.iter().enumerate() {
            let (off, len) = layout.file_extent(i);
            assert_eq!(len, f.size);
            let chunks: Vec<(u64, u64)> = if partial && len > 2 * MIB {
                vec![(0, MIB), (len - MIB, MIB)]
            } else {
                (0..len.div_ceil(4 * MIB)).map(|c| (c * 4 * MIB, (len - c * 4 * MIB).min(4 * MIB))).collect()
            };
            for (from, n) in chunks {
                img.seek(SeekFrom::Start(off + from)).unwrap();
                img.write_all(&pattern(i, from, n as usize)).unwrap();
            }
        }
        img.sync_all().unwrap();
        t
    }

    fn fsck(path: &Path) {
        let out = Command::new("/usr/bin/fsck.fat").arg("-n").arg("-V").arg(path).output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "fsck.fat failed:\n{text}");
        assert!(!text.lines().any(|l| l.contains("Dirty") || l.to_ascii_lowercase().contains("error")), "{text}");
    }

    fn blkid(path: &Path, label: &str) {
        let out = Command::new("blkid").args(["-p", "-o", "export"]).arg(path).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(text.lines().any(|l| l == "TYPE=vfat"), "{text}");
        assert!(text.lines().any(|l| l == format!("LABEL={label}")), "{text}");
        assert!(text.lines().any(|l| l == "VERSION=FAT32"), "{text}");
    }

    fn listed(path: &Path) -> Vec<String> {
        let out = Command::new("/usr/bin/7z").args(["l", "-slt"]).arg(path).output().unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.strip_prefix("Path = "))
            .filter(|p| !p.starts_with('/')).map(String::from).collect()
    }

    /// Streams one file out of the image through 7z and checks its length
    /// and either every byte or the first and last MiB against the pattern.
    fn check_extract(img: &Path, i: usize, name: &str, size: u64, partial: bool) {
        let mut child = Command::new("/usr/bin/7z").args(["x", "-so"]).arg(img).arg(name)
            .stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let mut out = child.stdout.take().unwrap();
        let mut buf = vec![0u8; 4 * MIB as usize];
        let mut pos = 0u64;
        loop {
            let n = out.read(&mut buf).unwrap();
            if n == 0 { break; }
            let check = !partial || size <= 2 * MIB || pos < MIB || pos + n as u64 > size - MIB;
            if check {
                for (k, (got, want)) in buf[..n].iter().zip(pattern(i, pos, n)).enumerate() {
                    let at = pos + k as u64;
                    if !partial || size <= 2 * MIB || at < MIB || at >= size - MIB {
                        assert_eq!(*got, want, "{name} differs at byte {at}");
                    }
                }
            }
            pos += n as u64;
        }
        assert!(child.wait().unwrap().success(), "7z could not extract {name}");
        assert_eq!(pos, size, "{name} has the wrong length");
    }

    fn tools_present() -> bool {
        let ok = ["/usr/bin/fsck.fat", "/usr/bin/7z"].iter().all(|t| Path::new(t).exists()) && have("blkid");
        if !ok { println!("SKIP: fsck.fat, 7z or blkid missing"); }
        ok
    }

    #[test]
    fn fat32_small_volume_full_contents() {
        if !tools_present() { return; }
        let mut files: Vec<FatFile> = real_files().into_iter().map(|mut f| { f.size = f.size.min(23 * MIB + 17); f }).collect();
        files.push(FatFile { path: "rimeinst/empty.txt".into(), size: 0 });
        // 150 entries: the directory needs two 4 KiB clusters.
        for n in 0..150 { files.push(FatFile { path: format!("rimeinst/many/f{n:03}.bin"), size: 1 + n * 13 }); }
        let layout = plan_fat32(300 * MIB, "RimeInst", 0x1234_ABCD, &files).unwrap();
        assert_eq!(layout.sectors_per_cluster, 8);
        let many = layout.dirs.iter().find(|d| &d.name == b"MANY       ").unwrap();
        assert_eq!(many.count, 2);
        assert_eq!(layout.file_extent(8), (0, 0));
        let img = build(&layout, &files, false, "small.img");
        fsck(&img.0);
        blkid(&img.0, "RIMEINST");
        let paths = listed(&img.0);
        for want in ["EFI", "EFI/rimeinst", "EFI/rimeinst/grubx64.efi", "rimeinst/LIVEOS/squashfs.img",
                     "rimeinst/empty.txt", "rimeinst/many/f149.bin", "rimeinst/handoff.cfg"] {
            assert!(paths.iter().any(|p| p == want), "7z does not list {want}: {paths:?}");
        }
        for (i, f) in files.iter().enumerate() {
            if i > 8 && i % 10 != 0 { continue; } // a sample of the 150 small ones
            let shown = f.path.replace("LiveOS", "LIVEOS");
            check_extract(&img.0, i, &shown, f.size, false);
        }
    }

    #[test]
    fn fat32_real_file_set_on_sparse_volume() {
        if !tools_present() { return; }
        let files = real_files();
        let volume = 2304 * MIB; // 2.25 GiB
        let layout = plan_fat32(volume, "RIMEINST", 0xC0FFEE01, &files).unwrap();
        let img = build(&layout, &files, true, "real.img");
        fsck(&img.0);
        blkid(&img.0, "RIMEINST");
        let paths = listed(&img.0);
        for f in &files {
            let shown = f.path.replace("LiveOS", "LIVEOS");
            assert!(paths.contains(&shown), "{shown} missing from {paths:?}");
        }
        for (i, f) in files.iter().enumerate() {
            check_extract(&img.0, i, &f.path.replace("LiveOS", "LIVEOS"), f.size, true);
        }
        // Each extent is cluster aligned, inside the volume, and in order.
        let mut last_end = 0;
        for i in 0..files.len() {
            let (off, len) = layout.file_extent(i);
            assert!(off >= last_end && off + len <= volume);
            assert_eq!((off - layout.cluster_offset(2)) % 4096, 0);
            last_end = off + len;
        }
    }

    #[test]
    fn fat32_plans_are_deterministic() {
        let a = plan_fat32(2304 * MIB, "RIMEINST", 7, &real_files()).unwrap();
        let b = plan_fat32(2304 * MIB, "RIMEINST", 7, &real_files()).unwrap();
        assert_eq!(a.metadata(), b.metadata());
        assert_eq!((0..8).map(|i| a.file_extent(i)).collect::<Vec<_>>(), (0..8).map(|i| b.file_extent(i)).collect::<Vec<_>>());
    }

    #[test]
    fn fat32_refusals() {
        let msg = |r: io::Result<FatLayout>| r.unwrap_err().to_string();
        let one = |p: &str| vec![FatFile { path: p.into(), size: 10 }];
        assert!(msg(plan_fat32(200 * MIB, "X", 0, &one("a.txt"))).contains("FAT16"));
        assert!(msg(plan_fat32(33 * 1024 * MIB, "X", 0, &one("a.txt"))).contains("32 GiB"));
        assert!(msg(plan_fat32(300 * MIB + 1, "X", 0, &one("a.txt"))).contains("sectors"));
        let small = msg(plan_fat32(1024 * MIB, "X", 0, &real_files()));
        assert!(small.contains("need") && small.contains("only"), "{small}");
        for bad in ["squashfs-long.img", "a.toolong", "a.b.c", "sp ace.txt", "/abs.txt", "a//b", "noext.", "LiveOS/../x"] {
            assert!(plan_fat32(300 * MIB, "X", 0, &one(bad)).is_err(), "{bad} was accepted");
        }
        let dup = vec![FatFile { path: "a/b.txt".into(), size: 1 }, FatFile { path: "A/B.TXT".into(), size: 2 }];
        assert!(msg(plan_fat32(300 * MIB, "X", 0, &dup)).contains("twice"));
        let file_then_dir = vec![FatFile { path: "a/b".into(), size: 1 }, FatFile { path: "a/b/c".into(), size: 2 }];
        assert!(msg(plan_fat32(300 * MIB, "X", 0, &file_then_dir)).contains("already a file"));
        let dir_then_file = vec![FatFile { path: "a/b/c".into(), size: 1 }, FatFile { path: "a/b".into(), size: 2 }];
        assert!(msg(plan_fat32(300 * MIB, "X", 0, &dir_then_file)).contains("as a directory"));
        assert!(msg(plan_fat32(300 * MIB, "much too long", 0, &one("a"))).contains("label"));
    }

    #[test]
    fn short_names_and_case_flags() {
        assert_eq!(short_name("grubx64.efi").unwrap(), (*b"GRUBX64 EFI", CASE_LOWER_BASE | CASE_LOWER_EXT));
        assert_eq!(short_name("LiveOS").unwrap(), (*b"LIVEOS     ", 0));
        assert_eq!(short_name("BOOTX64.EFI").unwrap(), (*b"BOOTX64 EFI", 0));
        assert_eq!(short_name("README.txt").unwrap(), (*b"README  TXT", CASE_LOWER_EXT));
    }
}
