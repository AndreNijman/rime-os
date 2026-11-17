//! One file descriptor alongside a line on a Unix socket (`SCM_RIGHTS`).
//!
//! `rime remote send` hands rime-remoted the file to send as an open
//! descriptor, not a path: the service has a private `/tmp`, and a path is a
//! name the service would resolve again, later, as itself. The descriptor is
//! the file the person could read when they asked.
//!
//! The descriptor rides the `sendmsg` that carries the first byte of the
//! line. The receiver reads with [`recv`], which keeps every descriptor it is
//! handed; a plain `read(2)` would close them unseen.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

/// Write all of `bytes` to `stream`, passing `fd` with the first byte.
pub fn send_with_fd(stream: &UnixStream, bytes: &[u8], fd: RawFd) -> io::Result<()> {
    if bytes.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "a descriptor needs at least one byte to ride on"));
    }
    let fd_len = std::mem::size_of::<RawFd>() as u32;
    // SAFETY: CMSG_SPACE is arithmetic on its argument.
    let space = unsafe { libc::CMSG_SPACE(fd_len) } as usize;
    // u64s, so the control buffer has cmsghdr's alignment.
    let mut control = vec![0u64; space.div_ceil(8)];
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr() as *mut libc::c_void,
        iov_len: bytes.len(),
    };
    // SAFETY: a zeroed msghdr is valid (null pointers, zero lengths); every
    // pointer set below outlives the sendmsg call.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = space as _;
    // SAFETY: msg_control points at `space` bytes, enough for one header and
    // one descriptor, so CMSG_FIRSTHDR is non-null and its data in bounds.
    let sent = unsafe {
        let c = libc::CMSG_FIRSTHDR(&msg);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN(fd_len) as _;
        (libc::CMSG_DATA(c) as *mut RawFd).write_unaligned(fd);
        loop {
            let n = libc::sendmsg(stream.as_raw_fd(), &msg, libc::MSG_NOSIGNAL);
            if n >= 0 {
                break n as usize;
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    };
    use std::io::Write;
    (&*stream).write_all(&bytes[sent..])
}

/// Read up to `buf.len()` bytes, appending every descriptor that came with
/// them to `fds` (close-on-exec). `Ok(0)` is end of stream.
pub fn recv(stream: &UnixStream, buf: &mut [u8], fds: &mut Vec<OwnedFd>) -> io::Result<usize> {
    // Room for a few descriptors; a client sends one. More than fits is
    // MSG_CTRUNC and refused, so a descriptor is never dropped silently.
    let mut control = [0u64; 16];
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    // SAFETY: as in send_with_fd; the buffers outlive the call.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = std::mem::size_of_val(&control) as _;
    let n = loop {
        // SAFETY: msg describes live buffers of the stated lengths.
        let n = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
        if n >= 0 {
            break n as usize;
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    };
    // SAFETY: the kernel filled msg_control with msg_controllen bytes of
    // well-formed headers; CMSG_FIRSTHDR/NXTHDR stay inside them, and an
    // SCM_RIGHTS header's data holds (cmsg_len - CMSG_LEN(0)) / 4 descriptors
    // that are now this process's to own.
    unsafe {
        let mut c = libc::CMSG_FIRSTHDR(&msg);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                let data = libc::CMSG_DATA(c) as *const RawFd;
                let count = ((*c).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / std::mem::size_of::<RawFd>();
                for i in 0..count {
                    fds.push(OwnedFd::from_raw_fd(data.add(i).read_unaligned()));
                }
            }
            c = libc::CMSG_NXTHDR(&msg, c);
        }
    }
    if msg.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "more descriptors were sent than one request carries"));
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, Write};

    #[test]
    fn a_descriptor_crosses_with_its_line_and_reads_the_same_file() {
        let (a, b) = UnixStream::pair().expect("pair");
        // Opened, then unlinked: the descriptor is all that names it.
        let path = std::env::temp_dir().join(format!("fdpass-{}-{:?}", std::process::id(), std::thread::current().id()));
        let mut f = std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path).expect("open");
        std::fs::remove_file(&path).expect("unlink");
        f.write_all(b"the bytes").expect("write");
        f.rewind().expect("rewind");
        send_with_fd(&a, b"{\"cmd\":\"send\"}\n", f.as_raw_fd()).expect("send");
        drop(f);
        let mut fds = Vec::new();
        let mut buf = [0u8; 64];
        let n = recv(&b, &mut buf, &mut fds).expect("recv");
        assert_eq!(&buf[..n], b"{\"cmd\":\"send\"}\n");
        assert_eq!(fds.len(), 1, "the descriptor was lost");
        let mut got = String::new();
        std::fs::File::from(fds.pop().unwrap()).read_to_string(&mut got).expect("read");
        assert_eq!(got, "the bytes");
    }

    #[test]
    fn a_plain_line_brings_no_descriptor() {
        let (a, b) = UnixStream::pair().expect("pair");
        (&a).write_all(b"x\n").expect("write");
        let mut fds = Vec::new();
        let mut buf = [0u8; 8];
        assert_eq!(recv(&b, &mut buf, &mut fds).expect("recv"), 2);
        assert!(fds.is_empty());
    }
}
