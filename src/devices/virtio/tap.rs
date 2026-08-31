//! Minimal TAP wrapper.
//!
//! Opens an already-created TAP interface (see `sudo cella setup net`) --
//! this process never needs `CAP_NET_ADMIN` to create one, only read/write
//! on the fd once it exists and is owned by the invoking user.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::io::AsRawFd;

const IFF_TAP: libc::c_short = 0x0002;
const IFF_NO_PI: libc::c_short = 0x1000;
const IFF_VNET_HDR: libc::c_short = 0x4000;
const TUNSETIFF: libc::c_ulong = 0x4004_54ca;
const TUNSETVNETHDRSZ: libc::c_ulong = 0x4004_54d8;

/// The TAP's default vnet header is the legacy 10-byte
/// `virtio_net_hdr`, but our device offers VIRTIO_F_VERSION_1, under
/// which the guest always uses the 12-byte variant (`num_buffers` is
/// unconditionally present). Both sides must agree or every frame is
/// sheared by 2 bytes; this keeps the "no header translation" property
/// net.rs relies on.
const VNET_HDR_SIZE: libc::c_int = 12;

#[repr(C)]
struct IfReq {
    name: [libc::c_char; 16],
    flags: libc::c_short,
    _pad: [u8; 22],
}

pub struct Tap {
    file: File,
}

impl Tap {
    /// Attaches to an existing TAP interface `name` (created out-of-band,
    /// see scripts/setup/tap.sh). Requires only rw on /dev/net/tun and
    /// that the interface is already owned by the calling user
    /// (`ip tuntap ... user $USER`).
    pub fn open(name: &str) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/net/tun")?;

        let mut req = IfReq {
            name: [0; 16],
            flags: IFF_TAP | IFF_NO_PI | IFF_VNET_HDR,
            _pad: [0; 22],
        };
        let name_bytes = name.as_bytes();
        if name_bytes.len() >= req.name.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "tap name too long",
            ));
        }
        for (i, b) in name_bytes.iter().enumerate() {
            req.name[i] = *b as libc::c_char;
        }

        // SAFETY: `req` is a valid, correctly-sized ifreq for TUNSETIFF.
        let ret = unsafe { libc::ioctl(file.as_raw_fd(), TUNSETIFF, &req as *const _) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: valid fd, TUNSETVNETHDRSZ takes a pointer to int.
        let ret = unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                TUNSETVNETHDRSZ,
                &VNET_HDR_SIZE as *const _,
            )
        };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }

        // Non-blocking: RX is driven by an external poll (see main.rs),
        // and we don't want a spurious read to stall the vCPU thread.
        // O_ASYNC + F_SETOWN: an arriving frame raises SIGIO at this
        // process, which interrupts KVM_RUN (EINTR) so the run loop can
        // drain RX. With the in-kernel irqchip an idle guest blocks
        // inside KVM_RUN indefinitely, so without this kick host->guest
        // traffic would sit unread until some unrelated VM exit.
        let fd = file.as_raw_fd();
        // SAFETY: fd is valid and open for the lifetime of `file`.
        unsafe {
            libc::fcntl(fd, libc::F_SETOWN, libc::getpid());
            let flags = libc::fcntl(fd, libc::F_GETFL, 0);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK | libc::O_ASYNC);
        }

        Ok(Tap { file })
    }

    pub fn read_frame(&self, buf: &mut [u8]) -> io::Result<usize> {
        use std::io::Read;
        (&self.file).read(buf)
    }

    pub fn write_frame(&self, buf: &[u8]) -> io::Result<usize> {
        use std::io::Write;
        (&self.file).write(buf)
    }

    pub fn as_raw_fd(&self) -> i32 {
        self.file.as_raw_fd()
    }
}
