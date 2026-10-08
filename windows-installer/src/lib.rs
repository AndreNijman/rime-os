//! GPT enumeration and all-bytes content inspection, plus — on Windows — the
//! storage, ownership and locking layer the installer is built on.
//!
//! The GPT reader here is deliberately the *only* GPT reader: on Windows it is
//! pointed at `\\.\PhysicalDriveN` and its answer is compared against what
//! Windows itself reports through `IOCTL_DISK_GET_DRIVE_LAYOUT_EX`. Two
//! independent sources that have to agree is worth more than either alone.
pub mod bootentry;
pub mod gptwrite;
pub mod payload;
pub mod pin;
pub mod plan;
pub mod stage;
#[cfg(windows)]
pub mod winwrite;
#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub mod net;
#[cfg(windows)]
pub mod winstall;
#[cfg(windows)]
pub mod gui;

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Component, Path};

fn refuse(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn u32le(b: &[u8]) -> u32 { u32::from_le_bytes(b[..4].try_into().unwrap()) }
fn u64le(b: &[u8]) -> u64 { u64::from_le_bytes(b[..8].try_into().unwrap()) }
fn crc32(b: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in b {
        crc ^= u32::from(*byte);
        for _ in 0..8 { crc = (crc >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(crc & 1))); }
    }
    !crc
}
pub fn guid(b: &[u8]) -> String {
    format!("{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        u32le(b), u16::from_le_bytes([b[4],b[5]]), u16::from_le_bytes([b[6],b[7]]),
        b[8],b[9],b[10],b[11],b[12],b[13],b[14],b[15])
}
/// Bytes from the operating system's CSPRNG: BCryptGenRandom on Windows,
/// /dev/urandom elsewhere. Partition GUIDs and the FAT volume id come from
/// here; a predictable partition GUID is a collision waiting for a cloned
/// disk.
pub fn random_bytes(n: usize) -> io::Result<Vec<u8>> {
    let mut b = vec![0u8; n];
    #[cfg(windows)]
    windows::random(&mut b)?;
    #[cfg(not(windows))]
    File::open("/dev/urandom")?.read_exact(&mut b)?;
    if n >= 8 && b.iter().all(|v| *v == 0) {
        return Err(refuse("the random number generator returned zeros"));
    }
    Ok(b)
}
/// A random (version 4) GUID in the lowercase text form the GPT code takes.
pub fn random_guid() -> io::Result<String> {
    let mut b = random_bytes(16)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    ))
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub offset: u64,
    pub length: u64,
    pub attributes: u64,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Layout {
    pub disk_id: String,
    pub partitions: Vec<Partition>,
}
/// Only regular *.img files through ordinary paths. No devices, UNC paths,
/// symlinks or Windows reparse points, including in ancestor directories.
/// This is a lab guard, not a security boundary against hostile path races.
pub fn open_image(path: &Path) -> io::Result<File> {
    if path.extension().and_then(|v| v.to_str()) != Some("img") {
        return Err(refuse("only regular .img laboratory files are supported"));
    }
    let mut part = std::path::PathBuf::new();
    for c in path.components() {
        if let Component::Prefix(prefix) = c
            && !matches!(prefix.kind(), std::path::Prefix::Disk(_)) {
            return Err(refuse("device, verbatim and network paths are disabled"));
        }
        part.push(c);
        let m = std::fs::symlink_metadata(&part)?;
        if m.file_type().is_symlink() { return Err(refuse("symlinks disabled")); }
        #[cfg(windows)] {
            use std::os::windows::fs::MetadataExt;
            if m.file_attributes() & 0x400 != 0 { return Err(refuse("reparse points disabled")); }
        }
    }
    if !std::fs::metadata(path)?.is_file() { return Err(refuse("not a regular image file")); }
    let f = File::open(path)?;
    if !f.metadata()?.is_file() { return Err(refuse("opened handle is not a regular file")); }
    Ok(f)
}
fn at<R: Read + Seek>(f: &mut R, offset: u64, size: usize) -> io::Result<Vec<u8>> {
    f.seek(SeekFrom::Start(offset))?;
    let mut b = vec![0;size];
    f.read_exact(&mut b)?;
    Ok(b)
}
fn header<R: Read + Seek>(f: &mut R, lba: u64, alternate: u64) -> io::Result<Vec<u8>> {
    let h = at(f, lba * 512, 512)?;
    if &h[..8] != b"EFI PART" || u32le(&h[8..]) != 0x10000 || u32le(&h[12..]) != 92
        || u32le(&h[20..]) != 0 || u64le(&h[24..]) != lba || u64le(&h[32..]) != alternate {
        return Err(refuse("unsupported or inconsistent GPT header"));
    }
    let mut checked = h[..92].to_vec();
    checked[16..20].fill(0);
    if crc32(&checked) != u32le(&h[16..]) { return Err(refuse("GPT header CRC mismatch")); }
    Ok(h)
}
/// Deliberately narrow: 512-byte sectors, conventional 128 x 128-byte GPT
/// entries, mirrored tables. Unsupported layouts are refused, not guessed.
pub fn enumerate(f: &mut File) -> io::Result<Layout> {
    let len = f.metadata()?.len();
    enumerate_in(f, len)
}
/// The same enumeration against anything seekable, with the length supplied
/// rather than asked of the object.
///
/// This split is not tidiness. `File::metadata().len()` answers **0** for a
/// handle on `\\.\PhysicalDriveN`, so a reader that asks the handle how big
/// it is decides every physical disk is too small to hold a GPT and refuses
/// the machine it was pointed at. On Windows the length comes from
/// `IOCTL_DISK_GET_LENGTH_INFO` instead.
pub fn enumerate_in<R: Read + Seek>(f: &mut R, len: u64) -> io::Result<Layout> {
    if len < 68 * 512 || !len.is_multiple_of(512) { return Err(refuse("invalid image length")); }
    let last = len / 512 - 1;
    let mbr = at(f, 0, 512)?;
    // SizeInLBA. UEFI 2.10 table 5.3 says "the size of the disk minus one …
    // Set to 0xFFFFFFFF if the size of the disk is too large to be
    // represented in this field", and WINDOWS WRITES 0xFFFFFFFF ALWAYS —
    // measured on a Windows Server 2022 install to a 40 GB disk, where the
    // correct value 0x04FFFFFF fits easily. Requiring the exact value refuses
    // every Windows-formatted disk there is, which is the entire population
    // this program exists to read.
    let size_in_lba = u64::from(u32le(&mbr[458..]));
    if mbr[510..] != [0x55,0xaa] || mbr[446] != 0 || mbr[450] != 0xee
        || u32le(&mbr[454..]) != 1
        || (size_in_lba != last.min(u64::from(u32::MAX)) && size_in_lba != u64::from(u32::MAX))
        || mbr[462..510].iter().any(|v| *v != 0) {
        return Err(refuse("missing protective MBR or hybrid MBR"));
    }
    let h = header(f,1,last)?;
    let backup = header(f,last,1)?;
    // The usable range is a RANGE, not a pair of constants. The first round
    // required first-usable == 34 and last-usable == last-33 exactly, which
    // is what a disk looks like when the entry table is immediately followed
    // by data. Every disk `sfdisk` produces has first-usable 2048, because it
    // aligns the first partition to 1 MiB, and that alignment is normal and
    // correct. UEFI 2.10 §5.3.2 constrains these to a range and nothing more:
    // the usable range must lie outside both copies of the header and table.
    let first_usable = u64le(&h[40..]);
    let last_usable = u64le(&h[48..]);
    if h[40..72] != backup[40..72] || h[80..92] != backup[80..92]
        || u64le(&h[72..]) != 2 || u64le(&backup[72..]) != last-32
        || u32le(&h[80..]) != 128 || u32le(&h[84..]) != 128
        || first_usable < 34 || last_usable > last-33 || first_usable > last_usable
        || h[56..72].iter().all(|v| *v == 0) {
        return Err(refuse("unsupported GPT geometry or disagreeing backup"));
    }
    let entries = at(f,1024,16384)?;
    if crc32(&entries) != u32le(&h[88..]) || entries != at(f,(last-32)*512,16384)? {
        return Err(refuse("GPT table CRC mismatch or disagreeing backup table"));
    }
    let mut partitions: Vec<Partition> = Vec::new();
    for e in entries.as_chunks::<128>().0 {
        if e[..16].iter().all(|v| *v == 0) {
            if e.iter().any(|v| *v != 0) { return Err(refuse("stale unused GPT entry")); }
            continue;
        }
        let start = u64le(&e[32..]);
        let end = u64le(&e[40..]);
        // Against the header's own usable range, not against constants: a
        // partition outside the range its own table declares is the defect
        // worth catching.
        if start < first_usable || end > last_usable || start > end
            || e[16..32].iter().all(|v| *v == 0) {
            return Err(refuse("invalid partition bounds or identity"));
        }
        let p = Partition { id: guid(&e[16..32]), kind: guid(&e[..16]),
            name: String::from_utf16(&e[56..128].as_chunks::<2>().0.iter()
                .map(|v| u16::from_le_bytes([v[0],v[1]]))
                .take_while(|v| *v != 0).collect::<Vec<_>>())
                .map_err(|_| refuse("invalid GPT name"))?,
            offset: start*512, length: (end-start+1)*512, attributes: u64le(&e[48..]) };
        if partitions.iter().any(|q| q.id == p.id || (p.offset < q.offset+q.length && q.offset < p.offset+p.length)) {
            return Err(refuse("duplicate partition GUID or overlapping partitions"));
        }
        partitions.push(p);
    }
    Ok(Layout { disk_id: guid(&h[56..72]), partitions })
}
#[derive(Debug, PartialEq, Eq)]
pub enum Scan {
    AllZero { bytes_read: u64 },
    Nonzero { bytes_read: u64, relative_offset: u64, value: u8 },
}
/// Reads every byte, without signature heuristics, until nonzero or error.
/// This is content evidence only, never permission to format or install.
pub fn scan<R: Read + Seek>(reader: &mut R, offset: u64, length: u64) -> io::Result<Scan> {
    if length == 0 || offset.checked_add(length).is_none() { return Err(refuse("invalid scan extent")); }
    reader.seek(SeekFrom::Start(offset))?;
    let mut done = 0;
    let mut buffer = vec![0;1024*1024];
    while done < length {
        let n = (length-done).min(buffer.len() as u64) as usize;
        reader.read_exact(&mut buffer[..n])?;
        if let Some(i) = buffer[..n].iter().position(|v| *v != 0) {
            return Ok(Scan::Nonzero { bytes_read: done+n as u64, relative_offset: done+i as u64, value: buffer[i] });
        }
        done += n as u64;
    }
    Ok(Scan::AllZero { bytes_read: done })
}
/// Lab-only eligibility. Windows ownership/usage evidence is not implemented.
/// Only Linux filesystem GPT type is accepted; Windows basic-data, ESP,
/// recovery, MSR, RAID, LVM and unknown types are refused even when zeroed.
pub fn lab_policy(p: &Partition) -> io::Result<()> {
    if p.kind != "0fc63daf-8483-4772-8e79-3d69d8477de4" || p.attributes != 0 {
        return Err(refuse("protected/unsupported partition type or attributes"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    #[test]
    fn scan_errors_never_produce_empty_evidence() {
        let mut short = Cursor::new(vec![0; 10]);
        assert!(scan(&mut short, 0, 11).is_err());
        assert!(scan(&mut short, 0, 0).is_err());
        assert!(scan(&mut short, u64::MAX, 2).is_err());
    }
    #[test]
    fn bounded_scan_includes_last_byte_but_excludes_neighbors() {
        let mut data = Cursor::new(vec![9, 0, 0, 7]);
        assert_eq!(scan(&mut data, 1, 2).unwrap(), Scan::AllZero { bytes_read: 2 });
        assert_eq!(scan(&mut data, 1, 3).unwrap(), Scan::Nonzero { bytes_read: 3, relative_offset: 2, value: 7 });
    }
    #[test]
    fn read_error_is_not_empty() {
        struct Broken(Cursor<Vec<u8>>);
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { Err(io::Error::other("injected read failure")) }
        }
        impl Seek for Broken {
            fn seek(&mut self, p: SeekFrom) -> io::Result<u64> { self.0.seek(p) }
        }
        assert!(scan(&mut Broken(Cursor::new(vec![0; 10])), 0, 10).is_err());
    }
}
