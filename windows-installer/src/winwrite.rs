//! Every Windows call in this program that changes anything.
//!
//! ═══ WHY ONE FILE ═══
//!
//! `windows.rs` reads; this file writes, and it is the only one that may.
//! `tests/test-windows-installer.sh` section 0 enforces that by name: a write
//! API, a write IOCTL or a firmware variable name anywhere else in the source
//! fails the build's gate. So "what can this program change?" is answered by
//! reading this file, which is the point.
//!
//! What it can change, completely:
//!
//! * bytes on a physical disk, through `DiskWriter`, which refuses any write
//!   outside the byte ranges it was opened with (the GPT areas, Rime's new
//!   ESP and three small areas of Rime's new root). The ranges come from the
//!   plan; a bug elsewhere that computes a wrong offset is refused here
//!   rather than written.
//! * Windows' cached view of that disk's partition table
//!   (`IOCTL_DISK_UPDATE_PROPERTIES`), so it sees what was written.
//! * the firmware variables `Boot####` (one new entry, and only entries this
//!   program made) and `BootNext` (a one-shot request). `BootOrder` is
//!   rewritten only by `remove_option`, to take out a number this program
//!   deleted; nothing else in it moves.
//! * nothing else. No volume is locked, dismounted or formatted, and no file
//!   on any Windows volume is touched except this program's own journal.

#![cfg(windows)]

use std::ffi::c_void;
use std::io::{self, Read, Seek, SeekFrom, Write};

type Handle = *mut c_void;
const INVALID_HANDLE: Handle = usize::MAX as Handle;

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_WRITE_THROUGH: u32 = 0x8000_0000;

// CTL_CODE(IOCTL_DISK_BASE 0x07, 0x0050, METHOD_BUFFERED, FILE_ANY_ACCESS).
// Tells the disk driver to re-read the partition table it caches, after the
// table was written underneath it. Measured in the lab: without it Windows
// keeps describing the old table; with it, the new one (ok=True err=0).
const IOCTL_DISK_UPDATE_PROPERTIES: u32 = 0x0007_0140;

const ERROR_ENVVAR_NOT_FOUND: u32 = 203;
const ERROR_NOT_ALL_ASSIGNED: u32 = 1300;
const TOKEN_ADJUST_PRIVILEGES: u32 = 0x20;
const TOKEN_QUERY: u32 = 0x8;
const SE_PRIVILEGE_ENABLED: u32 = 0x2;
/// EFI_VARIABLE_NON_VOLATILE | BOOTSERVICE_ACCESS | RUNTIME_ACCESS.
const VAR_ATTRS: u32 = 0x7;
const EFI_GLOBAL: &str = "{8BE4DF61-93CA-11D2-AA0D-00E098032B8C}";

/// The firmware variables this program may name. The gate in
/// tests/test-windows-installer.sh reads this list; `Boot####` is formed by
/// `crate::bootentry::option_name` from a number and nothing else.
pub const VAR_BOOT_ORDER: &str = "BootOrder";
pub const VAR_BOOT_NEXT: &str = "BootNext";
pub const VAR_BOOT_CURRENT: &str = "BootCurrent";

#[repr(C)]
struct Luid {
    low: u32,
    high: i32,
}
#[repr(C)]
struct TokenPrivileges {
    count: u32,
    luid: Luid,
    attributes: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(n: *const u16, a: u32, s: u32, sec: *mut c_void, d: u32, f: u32, t: Handle) -> Handle;
    fn CloseHandle(h: Handle) -> i32;
    fn GetLastError() -> u32;
    fn ReadFile(h: Handle, b: *mut u8, l: u32, r: *mut u32, o: *mut c_void) -> i32;
    fn WriteFile(h: Handle, b: *const u8, l: u32, w: *mut u32, o: *mut c_void) -> i32;
    fn SetFilePointerEx(h: Handle, d: i64, n: *mut i64, m: u32) -> i32;
    fn FlushFileBuffers(h: Handle) -> i32;
    fn DeviceIoControl(h: Handle, c: u32, i: *const c_void, il: u32, o: *mut c_void, ol: u32, r: *mut u32, ov: *mut c_void) -> i32;
    fn GetCurrentProcess() -> Handle;
    fn GetFirmwareEnvironmentVariableExW(n: *const u16, g: *const u16, b: *mut c_void, s: u32, a: *mut u32) -> u32;
    fn SetFirmwareEnvironmentVariableExW(n: *const u16, g: *const u16, b: *const c_void, s: u32, a: u32) -> i32;
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(p: Handle, a: u32, t: *mut Handle) -> i32;
    fn LookupPrivilegeValueW(sys: *const u16, name: *const u16, luid: *mut Luid) -> i32;
    fn AdjustTokenPrivileges(t: Handle, all: i32, new: *const TokenPrivileges, l: u32, prev: *mut c_void, r: *mut u32) -> i32;
}
#[link(name = "user32")]
unsafe extern "system" {
    fn ExitWindowsEx(flags: u32, reason: u32) -> i32;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
fn last_error() -> io::Error {
    io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
}

/// A physical disk opened for writing, which refuses every write that is not
/// a whole number of sectors inside one of the ranges it was given.
pub struct DiskWriter {
    h: Handle,
    pos: u64,
    allowed: Vec<(u64, u64)>,
}

impl Drop for DiskWriter {
    fn drop(&mut self) {
        unsafe {
            FlushFileBuffers(self.h);
            CloseHandle(self.h);
        }
    }
}

impl DiskWriter {
    pub fn open(path: &str, allowed: Vec<(u64, u64)>) -> io::Result<DiskWriter> {
        if !path.starts_with("\\\\.\\PhysicalDrive") {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "only a physical drive can be opened for writing"));
        }
        let h = unsafe {
            CreateFileW(
                wide(path).as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_WRITE_THROUGH,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE {
            return Err(last_error());
        }
        Ok(DiskWriter { h, pos: 0, allowed })
    }

    fn permitted(&self, offset: u64, len: u64) -> bool {
        let Some(end) = offset.checked_add(len) else { return false };
        self.allowed.iter().any(|(o, l)| offset >= *o && end <= o.saturating_add(*l))
    }

    /// IOCTL_DISK_UPDATE_PROPERTIES: make Windows re-read the table.
    pub fn update_properties(&self) -> io::Result<()> {
        let mut r = 0u32;
        let ok = unsafe {
            DeviceIoControl(self.h, IOCTL_DISK_UPDATE_PROPERTIES, std::ptr::null(), 0, std::ptr::null_mut(), 0, &mut r, std::ptr::null_mut())
        };
        if ok == 0 { Err(last_error()) } else { Ok(()) }
    }
}

impl Read for DiskWriter {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if !buf.len().is_multiple_of(512) || !self.pos.is_multiple_of(512) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "raw disk reads must be whole, aligned sectors"));
        }
        let mut got = 0u32;
        let n = buf.len().min(64 * 1024 * 1024) as u32;
        if unsafe { ReadFile(self.h, buf.as_mut_ptr(), n, &mut got, std::ptr::null_mut()) } == 0 {
            return Err(last_error());
        }
        self.pos += u64::from(got);
        Ok(got as usize)
    }
}

impl Write for DiskWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if !buf.len().is_multiple_of(512) || !self.pos.is_multiple_of(512) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "raw disk writes must be whole, aligned sectors"));
        }
        if !self.permitted(self.pos, buf.len() as u64) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("refused a write of {} bytes at {} that is outside the planned ranges", buf.len(), self.pos),
            ));
        }
        let n = buf.len().min(64 * 1024 * 1024) as u32;
        let mut put = 0u32;
        if unsafe { WriteFile(self.h, buf.as_ptr(), n, &mut put, std::ptr::null_mut()) } == 0 {
            return Err(last_error());
        }
        self.pos += u64::from(put);
        Ok(put as usize)
    }
    fn flush(&mut self) -> io::Result<()> {
        if unsafe { FlushFileBuffers(self.h) } == 0 { Err(last_error()) } else { Ok(()) }
    }
}

impl Seek for DiskWriter {
    fn seek(&mut self, p: SeekFrom) -> io::Result<u64> {
        let (d, m) = match p {
            SeekFrom::Start(v) => (v as i64, 0),
            SeekFrom::Current(v) => (v, 1),
            SeekFrom::End(v) => (v, 2),
        };
        let mut out = 0i64;
        if unsafe { SetFilePointerEx(self.h, d, &mut out, m) } == 0 {
            return Err(last_error());
        }
        self.pos = out as u64;
        Ok(self.pos)
    }
}

fn enable_privilege(name: &str) -> io::Result<()> {
    let mut token: Handle = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token) } == 0 {
        return Err(last_error());
    }
    let mut luid = Luid { low: 0, high: 0 };
    let r = (|| {
        if unsafe { LookupPrivilegeValueW(std::ptr::null(), wide(name).as_ptr(), &mut luid) } == 0 {
            return Err(last_error());
        }
        let tp = TokenPrivileges { count: 1, luid, attributes: SE_PRIVILEGE_ENABLED };
        if unsafe { AdjustTokenPrivileges(token, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut()) } == 0 {
            return Err(last_error());
        }
        // AdjustTokenPrivileges "succeeds" when it assigned nothing; this is
        // the only way to tell, and it is how a non-elevated run shows up.
        if unsafe { GetLastError() } == ERROR_NOT_ALL_ASSIGNED {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{name} is not held; run the installer as an administrator")));
        }
        Ok(())
    })();
    unsafe { CloseHandle(token) };
    r
}

/// Read access to firmware variables needs SE_SYSTEM_ENVIRONMENT_NAME too.
pub fn firmware_access() -> io::Result<()> {
    enable_privilege("SeSystemEnvironmentPrivilege")
}

fn get_var(name: &str) -> io::Result<Option<Vec<u8>>> {
    let mut buf = vec![0u8; 8192];
    let mut attrs = 0u32;
    let n = unsafe {
        GetFirmwareEnvironmentVariableExW(wide(name).as_ptr(), wide(EFI_GLOBAL).as_ptr(), buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut attrs)
    };
    if n == 0 {
        let e = unsafe { GetLastError() };
        if e == ERROR_ENVVAR_NOT_FOUND {
            return Ok(None);
        }
        return Err(io::Error::from_raw_os_error(e as i32));
    }
    buf.truncate(n as usize);
    Ok(Some(buf))
}

fn set_var(name: &str, value: &[u8]) -> io::Result<()> {
    let ok = unsafe {
        SetFirmwareEnvironmentVariableExW(wide(name).as_ptr(), wide(EFI_GLOBAL).as_ptr(), value.as_ptr() as *const c_void, value.len() as u32, VAR_ATTRS)
    };
    if ok == 0 { Err(last_error()) } else { Ok(()) }
}

pub fn boot_order() -> io::Result<Vec<u16>> {
    Ok(get_var(VAR_BOOT_ORDER)?.map(|b| crate::bootentry::decode_order(&b)).unwrap_or_default())
}
pub fn boot_current() -> io::Result<Option<u16>> {
    Ok(get_var(VAR_BOOT_CURRENT)?.and_then(|b| crate::bootentry::decode_order(&b).first().copied()))
}
pub fn boot_next() -> io::Result<Option<u16>> {
    Ok(get_var(VAR_BOOT_NEXT)?.and_then(|b| crate::bootentry::decode_order(&b).first().copied()))
}
pub fn boot_option(n: u16) -> io::Result<Option<Vec<u8>>> {
    get_var(&crate::bootentry::option_name(n))
}

/// Every Boot#### from 0000 to 00FF plus every number BootOrder names.
/// Windows has no call that lists firmware variables, so they are asked for
/// by name; entries above 00FF that BootOrder does not mention are rare
/// enough that `free_number` also avoids every number BootOrder uses.
pub fn boot_options() -> io::Result<Vec<(u16, Vec<u8>)>> {
    let mut nums: Vec<u16> = (0..=0xFFu16).collect();
    for n in boot_order()? {
        if !nums.contains(&n) {
            nums.push(n);
        }
    }
    let mut out = Vec::new();
    for n in nums {
        if let Some(b) = boot_option(n)? {
            out.push((n, b));
        }
    }
    Ok(out)
}

/// Create a new option. Refuses to overwrite: the number must be unused at
/// the moment of writing, re-checked here, not trusted from earlier.
pub fn create_option(n: u16, bytes: &[u8]) -> io::Result<()> {
    if boot_option(n)?.is_some() {
        return Err(io::Error::other(format!("{} already exists; refusing to overwrite it", crate::bootentry::option_name(n))));
    }
    set_var(&crate::bootentry::option_name(n), bytes)?;
    if boot_option(n)?.as_deref() != Some(bytes) {
        return Err(io::Error::other("the firmware did not store the boot entry as written"));
    }
    Ok(())
}

/// The commit point for the firmware: start option `n` once, on the next
/// boot only. The firmware deletes BootNext before it starts anything, so a
/// failed start lands back on the normal boot order with nothing to undo.
pub fn set_boot_next(n: u16) -> io::Result<()> {
    set_var(VAR_BOOT_NEXT, &n.to_le_bytes())?;
    if boot_next()? != Some(n) {
        return Err(io::Error::other("the firmware did not keep BootNext"));
    }
    Ok(())
}

/// Delete option `n`, and take its number out of BootOrder and BootNext if
/// either names it. Used by undo, and only for options whose decoded
/// contents the caller has already proven are Rime's.
pub fn remove_option(n: u16) -> io::Result<()> {
    let order = boot_order()?;
    if order.contains(&n) {
        let kept: Vec<u16> = order.into_iter().filter(|x| *x != n).collect();
        set_var(VAR_BOOT_ORDER, &crate::bootentry::encode_order(&kept))?;
    }
    if boot_next()? == Some(n) {
        set_var(VAR_BOOT_NEXT, &[])?;
    }
    set_var(&crate::bootentry::option_name(n), &[])
}

/// Restart now. Same privilege dance as the firmware variables.
pub fn restart() -> io::Result<()> {
    enable_privilege("SeShutdownPrivilege")?;
    // EWX_REBOOT; reason: operating system reconfiguration, planned.
    if unsafe { ExitWindowsEx(0x2, 0x8000_0000 | 0x0002_0000 | 0x4) } == 0 { Err(last_error()) } else { Ok(()) }
}
