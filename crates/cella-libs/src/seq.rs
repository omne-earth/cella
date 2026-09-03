//! SOCK_SEQPACKET unix sockets, by hand (1.6.14e rung 2).
//!
//! The edge contract moves whole frames: one frame per read and
//! one frame per write, boundaries kept by the kernel. std's
//! UnixListener speaks SOCK_STREAM only, so the four calls the
//! wire plane needs live here, over libc. The fds these functions
//! return are plain fds: read(2) returns one packet, write(2)
//! sends one packet, and the VMM's Edge::from_fd uses them with
//! no knowledge of this module.

use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

fn sockaddr_un(path: &Path) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    // SAFETY: zeroed sockaddr_un is a valid value for the type.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= addr.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket path too long for sockaddr_un",
        ));
    }
    for (i, b) in bytes.iter().enumerate() {
        addr.sun_path[i] = *b as libc::c_char;
    }
    let len = std::mem::size_of::<libc::sa_family_t>() + bytes.len() + 1;
    Ok((addr, len as libc::socklen_t))
}

/// Bind and listen on `path`. A stale socket file from a dead
/// process is removed first: the listener's identity is its
/// living process, not the inode.
pub fn listen(path: &Path) -> io::Result<i32> {
    let _ = std::fs::remove_file(path);
    // SAFETY: plain socket(2); the fd is checked below.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let (addr, len) = sockaddr_un(path)?;
    // SAFETY: addr is a valid sockaddr_un of length len; fd is ours.
    if unsafe { libc::bind(fd, &addr as *const _ as *const libc::sockaddr, len) } < 0
        || unsafe { libc::listen(fd, 8) } < 0
    {
        let e = io::Error::last_os_error();
        // SAFETY: fd is ours.
        unsafe { libc::close(fd) };
        return Err(e);
    }
    Ok(fd)
}

/// Accept one connection. Blocks unless the listener is
/// non-blocking (the caller decides with fcntl).
pub fn accept(listener: i32) -> io::Result<i32> {
    // SAFETY: accept with null addr is legal; listener is a live fd.
    let fd = unsafe { libc::accept(listener, std::ptr::null_mut(), std::ptr::null_mut()) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

/// Connect to `path`. One try; the caller owns the retry loop and
/// its patience.
pub fn connect(path: &Path) -> io::Result<i32> {
    // SAFETY: plain socket(2); the fd is checked below.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let (addr, len) = sockaddr_un(path)?;
    // SAFETY: addr is a valid sockaddr_un of length len; fd is ours.
    if unsafe { libc::connect(fd, &addr as *const _ as *const libc::sockaddr, len) } < 0 {
        let e = io::Error::last_os_error();
        // SAFETY: fd is ours.
        unsafe { libc::close(fd) };
        return Err(e);
    }
    Ok(fd)
}

/// Clear CLOEXEC so a child process (the VMM after exec) inherits
/// the fd. New sockets are not CLOEXEC by default on this path,
/// but the intent deserves a call site, not an assumption.
pub fn inheritable(fd: i32) {
    // SAFETY: fcntl on our own fd.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD, 0);
        libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
    }
}
