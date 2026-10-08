//! Windows storage enumeration, ownership and locking.
//!
//! ═══ WHY THIS IS HAND-DECLARED FFI AND STRUCTURES PARSED BY BYTE OFFSET ═══
//!
//! The crate has no dependencies and builds `--offline --locked`, and that is
//! worth keeping: an installer that can erase a partition is a program whose
//! whole supply chain a person might reasonably want to read in an afternoon.
//! So the fourteen functions this needs are declared against kernel32 by hand,
//! and every structure Windows returns is parsed out of a `Vec<u8>` by byte
//! offset — exactly the way the GPT parser next door already reads the disk.
//!
//! The offsets are not folklore. Each one is written down next to the field
//! with the alignment rule that produced it, because a silently wrong offset
//! here does not crash: it returns a plausible number for the wrong field, and
//! the wrong number in this program is a partition offset.
//!
//! ═══ WHAT IT WILL NOT DO ═══
//!
//! Nothing in this module opens a handle for writing, and nothing dismounts,
//! offlines or force-unlocks anything. It asks Windows what it is using and
//! accepts the answer; a refusal is a refusal, not a thing to retry with more
//! force.

#![cfg(windows)]

use std::ffi::c_void;
use std::io::{self, Read, Seek, SeekFrom};

type Handle = *mut c_void;
const INVALID_HANDLE: Handle = usize::MAX as Handle;

const GENERIC_READ: u32 = 0x8000_0000;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const OPEN_EXISTING: u32 = 3;

// CTL_CODE(DeviceType, Function, Method, Access)
//   = (DeviceType << 16) | (Access << 14) | (Function << 2) | Method
// IOCTL_DISK_BASE 0x07, IOCTL_STORAGE_BASE 0x2d, IOCTL_VOLUME_BASE 0x56,
// FILE_DEVICE_FILE_SYSTEM 0x09. METHOD_BUFFERED 0, FILE_ANY_ACCESS 0,
// FILE_READ_ACCESS 1.
const IOCTL_DISK_GET_DRIVE_GEOMETRY: u32 = 0x0007_0000;
const IOCTL_DISK_GET_DRIVE_LAYOUT_EX: u32 = 0x0007_0050;
const IOCTL_DISK_GET_LENGTH_INFO: u32 = 0x0007_405C;
const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;
// FSCTL_LOCK_VOLUME (0x00090018) is deliberately ABSENT. Windows creates no
// volume object for a Linux-filesystem-type partition — measured in the lab —
// and a partition that does have one is refused before any lock would be
// attempted. A lock call here could only ever run on a partition already
// refused, and safety code that never executes reads as coverage. The write
// path's exclusivity mechanism is described in ARCHITECTURE.md.

const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_PATH_NOT_FOUND: u32 = 3;
const ERROR_NO_MORE_FILES: u32 = 18;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *mut c_void,
        disposition: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    fn CloseHandle(h: Handle) -> i32;
    fn GetLastError() -> u32;
    fn DeviceIoControl(
        h: Handle,
        code: u32,
        in_buf: *const c_void,
        in_len: u32,
        out_buf: *mut c_void,
        out_len: u32,
        returned: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn ReadFile(h: Handle, buf: *mut u8, len: u32, read: *mut u32, ov: *mut c_void) -> i32;
    fn SetFilePointerEx(h: Handle, distance: i64, new_pos: *mut i64, method: u32) -> i32;
    fn FindFirstVolumeW(name: *mut u16, len: u32) -> Handle;
    fn FindNextVolumeW(h: Handle, name: *mut u16, len: u32) -> i32;
    fn FindVolumeClose(h: Handle) -> i32;
    fn GetVolumePathNamesForVolumeNameW(
        volume: *const u16,
        names: *mut u16,
        len: u32,
        returned: *mut u32,
    ) -> i32;
    fn GetVolumeInformationW(
        root: *const u16,
        label: *mut u16,
        label_len: u32,
        serial: *mut u32,
        max_component: *mut u32,
        flags: *mut u32,
        fs_name: *mut u16,
        fs_name_len: u32,
    ) -> i32;
}

#[link(name = "bcrypt")]
unsafe extern "system" {
    fn BCryptGenRandom(alg: Handle, buf: *mut u8, len: u32, flags: u32) -> i32;
}

/// BCRYPT_USE_SYSTEM_PREFERRED_RNG: the system's CSPRNG, no handle needed.
pub fn random(buf: &mut [u8]) -> io::Result<()> {
    let st = unsafe { BCryptGenRandom(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32, 2) };
    if st != 0 {
        return Err(io::Error::other(format!("BCryptGenRandom failed ({st:#x})")));
    }
    Ok(())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(b: &[u16]) -> String {
    let n = b.iter().position(|c| *c == 0).unwrap_or(b.len());
    String::from_utf16_lossy(&b[..n])
}

fn last_error() -> io::Error {
    io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
}

/// An owned Win32 handle. The only thing in this file that can leak, so it is
/// the only thing with a `Drop`.
pub struct Device {
    h: Handle,
    /// Kept for messages only. Never an identity: see `Drive::describe`.
    pub path: String,
}

impl Drop for Device {
    fn drop(&mut self) {
        if self.h != INVALID_HANDLE {
            unsafe { CloseHandle(self.h) };
        }
    }
}

impl Device {
    /// Read-only, sharing read and write. Sharing write is deliberate: this
    /// program is surveying a machine that is running, and demanding exclusive
    /// access merely to *look* would refuse every disk Windows is using —
    /// including, always, the one Windows booted from. Exclusivity is taken
    /// later, on the volume, and only when something is about to be written.
    pub fn open(path: &str) -> io::Result<Device> {
        let h = unsafe {
            CreateFileW(
                wide(path).as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE {
            return Err(last_error());
        }
        Ok(Device { h, path: path.to_string() })
    }

    fn ioctl(&self, code: u32, input: &[u8], out_len: usize) -> io::Result<Vec<u8>> {
        let mut out = vec![0u8; out_len];
        let mut returned: u32 = 0;
        let ok = unsafe {
            DeviceIoControl(
                self.h,
                code,
                if input.is_empty() { std::ptr::null() } else { input.as_ptr() as *const c_void },
                input.len() as u32,
                out.as_mut_ptr() as *mut c_void,
                out.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_error());
        }
        out.truncate(returned as usize);
        Ok(out)
    }

}

fn u32at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

impl Read for Device {
    /// Reads on a physical-drive handle must be a whole number of sectors, so
    /// this refuses anything else rather than letting Windows return
    /// ERROR_INVALID_PARAMETER from somewhere deep in a scan. The caller's
    /// buffers are all sector multiples by construction.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if !buf.len().is_multiple_of(512) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "raw device reads must be a whole number of sectors",
            ));
        }
        let mut got: u32 = 0;
        let ok = unsafe {
            ReadFile(self.h, buf.as_mut_ptr(), buf.len() as u32, &mut got, std::ptr::null_mut())
        };
        if ok == 0 {
            return Err(last_error());
        }
        Ok(got as usize)
    }
}

impl Seek for Device {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let (distance, method) = match pos {
            SeekFrom::Start(v) => (v as i64, 0u32),
            SeekFrom::Current(v) => (v, 1u32),
            SeekFrom::End(v) => (v, 2u32),
        };
        let mut out: i64 = 0;
        let ok = unsafe { SetFilePointerEx(self.h, distance, &mut out, method) };
        if ok == 0 {
            return Err(last_error());
        }
        Ok(out as u64)
    }
}

/// A physical disk, as Windows describes it.
#[derive(Debug, Clone)]
pub struct Drive {
    /// `\\.\PhysicalDriveN`. The N in it is an enumeration artefact and is
    /// never used to identify anything; it is here so a message can say which
    /// path was opened.
    pub path: String,
    pub model: String,
    pub serial: String,
    pub bus: String,
    pub bytes_per_sector: u32,
    pub length: u64,
    pub removable: bool,
}

impl Drive {
    /// The only sanctioned way to name a disk to a human. Deliberately has no
    /// access to the enumeration index: identification by device index is how
    /// people erase the wrong drive, and on this program's own reference
    /// hardware NVMe enumeration has reordered three times across ordinary
    /// reboots.
    pub fn describe(&self) -> String {
        let model = if self.model.is_empty() { "(no model reported)" } else { &self.model };
        let serial = if self.serial.is_empty() { "(no serial reported)" } else { &self.serial };
        format!("{} / {} / {} bus, {}", model, serial, self.bus, human(self.length))
    }
}

pub fn human(bytes: u64) -> String {
    // Decimal, because that is what is printed on the label of the drive the
    // user is looking at.
    const UNITS: [&str; 5] = ["bytes", "kB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} bytes") } else { format!("{v:.1} {}", UNITS[u]) }
}

const BUS_TYPES: [&str; 19] = [
    "unknown", "SCSI", "ATAPI", "ATA", "1394", "SSA", "Fibre", "USB", "RAID", "iSCSI", "SAS",
    "SATA", "SD", "MMC", "virtual", "FileBackedVirtual", "Spaces", "NVMe", "SCM",
];

/// Enumerate `\\.\PhysicalDrive0..63`. A drive that cannot be opened is
/// reported, never silently skipped: "permission denied" is not "absent", and
/// a survey that quietly loses a disk is a survey that can call a machine
/// empty when it is not.
pub fn drives() -> (Vec<Drive>, Vec<String>) {
    let mut found = Vec::new();
    let mut problems = Vec::new();
    for i in 0..64u32 {
        let path = format!("\\\\.\\PhysicalDrive{i}");
        let dev = match Device::open(&path) {
            Ok(d) => d,
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0) as u32;
                if code != ERROR_FILE_NOT_FOUND && code != ERROR_PATH_NOT_FOUND {
                    problems.push(format!("{path}: {e}"));
                }
                continue;
            }
        };
        match describe_drive(&dev, path.clone()) {
            Ok(d) => found.push(d),
            Err(e) => problems.push(format!("{path}: {e}")),
        }
    }
    (found, problems)
}

fn describe_drive(dev: &Device, path: String) -> io::Result<Drive> {
    // GET_LENGTH_INFORMATION: a single LARGE_INTEGER.
    let len = dev.ioctl(IOCTL_DISK_GET_LENGTH_INFO, &[], 8)?;
    let length = u64at(&len, 0);

    // DISK_GEOMETRY: LARGE_INTEGER Cylinders (0); MEDIA_TYPE (8);
    // ULONG TracksPerCylinder (12); ULONG SectorsPerTrack (16);
    // ULONG BytesPerSector (20).
    let geo = dev.ioctl(IOCTL_DISK_GET_DRIVE_GEOMETRY, &[], 24)?;
    let bytes_per_sector = u32at(&geo, 20);

    // STORAGE_PROPERTY_QUERY { StorageDeviceProperty = 0, PropertyStandardQuery = 0 }
    let query = [0u8; 12];
    let (mut model, mut serial, mut bus, mut removable) =
        (String::new(), String::new(), "unknown".to_string(), false);
    if let Ok(d) = dev.ioctl(IOCTL_STORAGE_QUERY_PROPERTY, &query, 4096)
        && d.len() >= 36
    {
        // STORAGE_DEVICE_DESCRIPTOR:
        //   0 Version, 4 Size, 8 DeviceType, 9 DeviceTypeModifier,
        //   10 RemovableMedia, 11 CommandQueueing, 12 VendorIdOffset,
        //   16 ProductIdOffset, 20 ProductRevisionOffset,
        //   24 SerialNumberOffset, 28 BusType, 32 RawPropertiesLength.
        // The *Offset fields are byte offsets into this same buffer, and 0
        // means the device did not report that string.
        let str_at = |off: usize| -> String {
            if off == 0 || off >= d.len() {
                return String::new();
            }
            let end = d[off..].iter().position(|c| *c == 0).map_or(d.len(), |p| off + p);
            String::from_utf8_lossy(&d[off..end]).trim().to_string()
        };
        removable = d[10] != 0;
        let vendor = str_at(u32at(&d, 12) as usize);
        let product = str_at(u32at(&d, 16) as usize);
        model = if vendor.is_empty() { product } else { format!("{vendor} {product}").trim().to_string() };
        serial = str_at(u32at(&d, 24) as usize);
        let bt = u32at(&d, 28) as usize;
        bus = BUS_TYPES.get(bt).unwrap_or(&"unknown").to_string();
    }
    Ok(Drive { path, model, serial, bus, bytes_per_sector, length, removable })
}

/// One partition, as *Windows* reports it. Deliberately a different type from
/// the crate's GPT `Partition`: the whole point is that the two are produced
/// by independent code and then compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WinPartition {
    pub number: u32,
    pub offset: u64,
    pub length: u64,
    pub type_guid: String,
    pub id: String,
    pub name: String,
    pub attributes: u64,
}

/// IOCTL_DISK_GET_DRIVE_LAYOUT_EX.
///
/// DRIVE_LAYOUT_INFORMATION_EX, x64:
///   0  ULONG PartitionStyle        (0 MBR, 1 GPT, 2 RAW)
///   4  ULONG PartitionCount
///   8  union { MBR; GPT }          8-aligned because the GPT arm holds
///                                  LARGE_INTEGERs. The GPT arm is
///                                  GUID DiskId(16) + StartingUsableOffset(8)
///                                  + UsableLength(8) + MaxPartitionCount(4)
///                                  = 36, padded to 40.
///   48 PARTITION_INFORMATION_EX PartitionEntry[]
///
/// PARTITION_INFORMATION_EX, x64, 144 bytes each:
///   0   ULONG PartitionStyle
///   8   LARGE_INTEGER StartingOffset      (8-aligned)
///   16  LARGE_INTEGER PartitionLength
///   24  ULONG PartitionNumber
///   28  BOOLEAN RewritePartition
///   29  BOOLEAN IsServicePartition
///   32  union { MBR; GPT }                8-aligned
///       GPT arm: PartitionType GUID(16), PartitionId GUID(16),
///                DWORD64 Attributes(8), WCHAR Name[36] (72)
pub fn layout(dev: &Device) -> io::Result<(String, Vec<WinPartition>)> {
    let b = dev.ioctl(IOCTL_DISK_GET_DRIVE_LAYOUT_EX, &[], 48 + 144 * 128)?;
    if b.len() < 48 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "short drive layout"));
    }
    let style = u32at(&b, 0);
    if style != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "partition style {} is not GPT; this installer supports GPT disks only",
                match style {
                    0 => "MBR".to_string(),
                    2 => "RAW (no partition table)".to_string(),
                    other => format!("{other}"),
                }
            ),
        ));
    }
    let count = u32at(&b, 4) as usize;
    let disk_id = crate::guid(&b[8..24]);
    let mut out = Vec::new();
    for i in 0..count {
        let o = 48 + i * 144;
        if o + 144 > b.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated partition entry"));
        }
        let e = &b[o..o + 144];
        // Windows reports every slot the table can hold; unused ones have a
        // zero type GUID. Those are not partitions and must not be listed as
        // candidates.
        if e[32..48].iter().all(|v| *v == 0) {
            continue;
        }
        // Name is WCHAR[36] at offset 72: 32 (union start) + 16 (type GUID)
        // + 16 (partition GUID) + 8 (attributes). Reading it from 104 — one
        // GUID too far in — produced "tion" for "EFI system partition" on a
        // real Windows disk, which is a plausible-looking string and exactly
        // the failure mode a byte-offset parser has.
        let name_u16: Vec<u16> =
            e[72..144].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        out.push(WinPartition {
            number: u32at(e, 24),
            offset: u64at(e, 8),
            length: u64at(e, 16),
            type_guid: crate::guid(&e[32..48]),
            id: crate::guid(&e[48..64]),
            attributes: u64at(e, 64),
            name: from_wide(&name_u16),
        });
    }
    Ok((disk_id, out))
}

/// Everything Windows currently considers a volume, and the disk extents each
/// one occupies. This is the ownership question: a partition covered by a
/// volume is a partition Windows believes it owns.
#[derive(Debug, Clone)]
pub struct Volume {
    pub name: String,
    pub mount_points: Vec<String>,
    pub filesystem: String,
    pub label: String,
    /// (physical drive number, offset, length) — a volume can span several.
    pub extents: Vec<(u32, u64, u64)>,
}

pub fn volumes() -> (Vec<Volume>, Vec<String>) {
    let mut out = Vec::new();
    let mut problems = Vec::new();
    let mut buf = [0u16; 512];
    let h = unsafe { FindFirstVolumeW(buf.as_mut_ptr(), buf.len() as u32) };
    if h == INVALID_HANDLE {
        problems.push(format!("FindFirstVolumeW: {}", last_error()));
        return (out, problems);
    }
    loop {
        let name = from_wide(&buf);
        match describe_volume(&name) {
            Ok(v) => out.push(v),
            Err(e) => problems.push(format!("{name}: {e}")),
        }
        if unsafe { FindNextVolumeW(h, buf.as_mut_ptr(), buf.len() as u32) } == 0 {
            let code = unsafe { GetLastError() };
            if code != ERROR_NO_MORE_FILES {
                problems.push(format!("FindNextVolumeW: {}", io::Error::from_raw_os_error(code as i32)));
            }
            break;
        }
    }
    unsafe { FindVolumeClose(h) };
    (out, problems)
}

fn describe_volume(name: &str) -> io::Result<Volume> {
    // Mount points. A volume with none is still in use if Windows has it
    // open — the letter is a convenience for the message, not the test.
    let mut mount_points = Vec::new();
    let mut names = vec![0u16; 1024];
    let mut returned: u32 = 0;
    let ok = unsafe {
        GetVolumePathNamesForVolumeNameW(
            wide(name).as_ptr(),
            names.as_mut_ptr(),
            names.len() as u32,
            &mut returned,
        )
    };
    if ok != 0 {
        let mut i = 0usize;
        while i < names.len() && names[i] != 0 {
            let s = from_wide(&names[i..]);
            i += s.encode_utf16().count() + 1;
            mount_points.push(s);
        }
    }

    // Label and filesystem. GetVolumeInformationW wants a trailing backslash.
    let (mut label, mut filesystem) = (String::new(), String::new());
    let root = if name.ends_with('\\') { name.to_string() } else { format!("{name}\\") };
    let mut lbuf = [0u16; 256];
    let mut fbuf = [0u16; 64];
    let ok = unsafe {
        GetVolumeInformationW(
            wide(&root).as_ptr(),
            lbuf.as_mut_ptr(),
            lbuf.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fbuf.as_mut_ptr(),
            fbuf.len() as u32,
        )
    };
    if ok != 0 {
        label = from_wide(&lbuf);
        filesystem = from_wide(&fbuf);
    }

    // Extents. The device path for a volume drops the trailing backslash.
    let device = name.trim_end_matches('\\');
    let mut extents = Vec::new();
    if let Ok(dev) = Device::open(device)
        && let Ok(b) = dev.ioctl(IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, &[], 8 + 24 * 64)
        && b.len() >= 8
    {
        // VOLUME_DISK_EXTENTS: ULONG NumberOfDiskExtents (0); 4 bytes padding
        // because DISK_EXTENT starts with a ULONG but contains
        // LARGE_INTEGERs and is therefore 8-aligned; DISK_EXTENT[] at 8.
        // DISK_EXTENT: ULONG DiskNumber (0); pad; LARGE_INTEGER
        // StartingOffset (8); LARGE_INTEGER ExtentLength (16). 24 bytes.
        let n = u32at(&b, 0) as usize;
        for i in 0..n {
            let o = 8 + i * 24;
            if o + 24 > b.len() {
                break;
            }
            extents.push((u32at(&b, o), u64at(&b, o + 8), u64at(&b, o + 16)));
        }
    }
    Ok(Volume { name: name.to_string(), mount_points, filesystem, label, extents })
}

impl Volume {
    /// Does this volume overlap the byte range \[offset, offset+length) on the
    /// given physical drive? Overlap, not equality: a volume that covers part
    /// of the candidate is every bit as disqualifying as one that covers all
    /// of it, and equality would miss it.
    pub fn overlaps(&self, disk: u32, offset: u64, length: u64) -> bool {
        self.extents.iter().any(|(d, o, l)| {
            *d == disk && offset < o.saturating_add(*l) && *o < offset.saturating_add(length)
        })
    }

    pub fn describe_use(&self) -> String {
        let where_ = if self.mount_points.is_empty() {
            "no drive letter or mount point".to_string()
        } else {
            format!("mounted at {}", self.mount_points.join(", "))
        };
        let fs = if self.filesystem.is_empty() { "unrecognised filesystem".to_string() } else { self.filesystem.clone() };
        let label =
            if self.label.is_empty() { String::new() } else { format!(", label {:?}", self.label) };
        format!("{} -- {}{}, volume {}", where_, fs, label, self.name)
    }
}

/// The physical-drive number Windows uses in a `DISK_EXTENT`, parsed back out
/// of the device path this program opened. This is the one place a device
/// index is legitimate: it is a join key between two Windows APIs within a
/// single enumeration, never an identity that outlives it.
pub fn drive_number(path: &str) -> Option<u32> {
    path.rsplit("PhysicalDrive").next()?.parse().ok()
}
