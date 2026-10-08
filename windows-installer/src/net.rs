//! Downloading the installer image, through Windows' own HTTPS stack.
//!
//! WinHTTP rather than a TLS library: it is part of every Windows, it uses
//! the machine's certificate store and proxy settings, it is patched by
//! Windows Update, and it keeps this crate free of dependencies. It follows
//! GitHub's release redirect by itself and never downgrades HTTPS to HTTP.
//!
//! What protects the user is not the transport, though: the file is checked
//! against a SHA-256 compiled into this program before a single byte of it
//! is used (see `crate::pin`). A download that is wrong in any way, whether
//! truncated, corrupted or replaced, is refused there.

#![cfg(windows)]

use std::ffi::c_void;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;

type Hinternet = *mut c_void;

#[link(name = "winhttp")]
unsafe extern "system" {
    fn WinHttpOpen(agent: *const u16, access: u32, proxy: *const u16, bypass: *const u16, flags: u32) -> Hinternet;
    fn WinHttpCrackUrl(url: *const u16, len: u32, flags: u32, comp: *mut UrlComponents) -> i32;
    fn WinHttpConnect(s: Hinternet, server: *const u16, port: u16, reserved: u32) -> Hinternet;
    fn WinHttpOpenRequest(c: Hinternet, verb: *const u16, obj: *const u16, ver: *const u16, referrer: *const u16, accept: *const *const u16, flags: u32) -> Hinternet;
    fn WinHttpSendRequest(r: Hinternet, headers: *const u16, hlen: u32, opt: *const c_void, olen: u32, total: u32, ctx: usize) -> i32;
    fn WinHttpReceiveResponse(r: Hinternet, reserved: *mut c_void) -> i32;
    fn WinHttpQueryHeaders(r: Hinternet, info: u32, name: *const u16, buf: *mut c_void, len: *mut u32, index: *mut u32) -> i32;
    fn WinHttpReadData(r: Hinternet, buf: *mut c_void, len: u32, read: *mut u32) -> i32;
    fn WinHttpCloseHandle(h: Hinternet) -> i32;
    fn WinHttpSetTimeouts(h: Hinternet, resolve: i32, connect: i32, send: i32, receive: i32) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLastError() -> u32;
}

#[repr(C)]
struct UrlComponents {
    size: u32,
    scheme_ptr: *mut u16,
    scheme_len: u32,
    scheme: i32,
    host_ptr: *mut u16,
    host_len: u32,
    port: u16,
    user_ptr: *mut u16,
    user_len: u32,
    pass_ptr: *mut u16,
    pass_len: u32,
    path_ptr: *mut u16,
    path_len: u32,
    extra_ptr: *mut u16,
    extra_len: u32,
}

const WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY: u32 = 4;
const WINHTTP_FLAG_SECURE: u32 = 0x0080_0000;
const INTERNET_SCHEME_HTTPS: i32 = 2;
const WINHTTP_QUERY_STATUS_CODE: u32 = 19;
const WINHTTP_QUERY_FLAG_NUMBER: u32 = 0x2000_0000;

struct H(Hinternet);
impl Drop for H {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
fn err(what: &str) -> io::Error {
    let code = unsafe { GetLastError() };
    io::Error::other(format!("{what} failed (WinHTTP error {code})"))
}

/// Download `url` into `dest`, resuming a partial file, never writing more
/// than `expected` bytes. `progress(done, total)` is called as data arrives.
pub fn download(url: &str, dest: &Path, expected: u64, progress: &mut dyn FnMut(u64, u64)) -> io::Result<()> {
    if !url.starts_with("https://") {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "only https downloads are allowed"));
    }
    let have = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    if have > expected {
        std::fs::remove_file(dest)?;
        return download(url, dest, expected, progress);
    }
    if have == expected {
        progress(have, expected);
        return Ok(());
    }

    let wurl = wide(url);
    let mut host = vec![0u16; 256];
    let mut path = vec![0u16; 2048];
    let mut c = UrlComponents {
        size: std::mem::size_of::<UrlComponents>() as u32,
        scheme_ptr: std::ptr::null_mut(), scheme_len: 0, scheme: 0,
        host_ptr: host.as_mut_ptr(), host_len: host.len() as u32, port: 0,
        user_ptr: std::ptr::null_mut(), user_len: 0, pass_ptr: std::ptr::null_mut(), pass_len: 0,
        path_ptr: path.as_mut_ptr(), path_len: path.len() as u32,
        extra_ptr: std::ptr::null_mut(), extra_len: 0,
    };
    if unsafe { WinHttpCrackUrl(wurl.as_ptr(), 0, 0, &mut c) } == 0 || c.scheme != INTERNET_SCHEME_HTTPS {
        return Err(err("parsing the download address"));
    }
    let session = H(unsafe {
        WinHttpOpen(wide("RimeInstaller/1").as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, std::ptr::null(), std::ptr::null(), 0)
    });
    if session.0.is_null() {
        return Err(err("starting WinHTTP"));
    }
    // Resolve 30 s, connect 30 s, send 30 s, and 60 s for each read: a stalled
    // connection fails, and the next attempt resumes where this one stopped.
    unsafe { WinHttpSetTimeouts(session.0, 30_000, 30_000, 30_000, 60_000) };
    let conn = H(unsafe { WinHttpConnect(session.0, host.as_ptr(), c.port, 0) });
    if conn.0.is_null() {
        return Err(err("connecting to the download server"));
    }
    let req = H(unsafe {
        WinHttpOpenRequest(conn.0, wide("GET").as_ptr(), path.as_ptr(), std::ptr::null(), std::ptr::null(), std::ptr::null(), WINHTTP_FLAG_SECURE)
    });
    if req.0.is_null() {
        return Err(err("preparing the download"));
    }
    let range = if have > 0 { format!("Range: bytes={have}-\r\n") } else { String::new() };
    let hdr = wide(&range);
    let hlen = if range.is_empty() { 0 } else { u32::MAX };
    if unsafe { WinHttpSendRequest(req.0, if range.is_empty() { std::ptr::null() } else { hdr.as_ptr() }, hlen, std::ptr::null(), 0, 0, 0) } == 0 {
        return Err(err("sending the download request"));
    }
    if unsafe { WinHttpReceiveResponse(req.0, std::ptr::null_mut()) } == 0 {
        return Err(err("receiving the download"));
    }
    let mut status = 0u32;
    let mut slen = 4u32;
    if unsafe {
        WinHttpQueryHeaders(req.0, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, std::ptr::null(), &mut status as *mut u32 as *mut c_void, &mut slen, std::ptr::null_mut())
    } == 0 {
        return Err(err("reading the server's answer"));
    }
    let mut file = match status {
        206 if have > 0 => OpenOptions::new().append(true).open(dest)?,
        200 => {
            let f = OpenOptions::new().create(true).write(true).truncate(true).open(dest)?;
            if have > 0 {
                // The server ignored the range; start over rather than append
                // a second copy.
                progress(0, expected);
            }
            f
        }
        other => return Err(io::Error::other(format!("the download server answered HTTP {other}"))),
    };
    let mut done = if status == 206 { have } else { 0 };
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let mut got = 0u32;
        if unsafe { WinHttpReadData(req.0, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut got) } == 0 {
            file.flush()?;
            return Err(err("reading the download"));
        }
        if got == 0 {
            break;
        }
        done += u64::from(got);
        if done > expected {
            drop(file);
            std::fs::remove_file(dest)?;
            return Err(io::Error::other("the download is larger than the installer image this program expects; it was discarded"));
        }
        file.write_all(&buf[..got as usize])?;
        progress(done, expected);
    }
    file.flush()?;
    file.sync_all()?;
    if done != expected {
        return Err(io::Error::other(format!(
            "the download stopped at {done} of {expected} bytes; run the installer again to resume"
        )));
    }
    Ok(())
}
