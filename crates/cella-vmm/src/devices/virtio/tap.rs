//! The edge: one nic's backend fd (1.6.14e, rung 1).
//!
//! A nic's backend is a file descriptor the VMM reads and writes
//! whole frames on -- historically a TAP the host provisioned
//! (`Edge::tap`), and under the rootless network a socketpair to the
//! machine's own cella-network translator (`Edge::from_fd`). The
//! frame contract is identical on both: 12-byte virtio_net_hdr
//! prefix, one frame per read/write, O_ASYNC + SIGIO as the kick
//! that interrupts KVM_RUN when the world has mail. The membrane
//! (park, decide, release -- net.rs) never knows which kind it
//! stands on.

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

pub struct Edge {
    file: File,
}

impl Edge {
    /// Attaches to an existing TAP interface `name` (created out-of-band,
    /// see scripts/setup/tap.sh). Requires only rw on /dev/net/tun and
    /// that the interface is already owned by the calling user
    /// (`ip tuntap ... user $USER`).
    pub fn tap(name: &str) -> io::Result<Self> {
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

        Self::arm(&file);
        Ok(Edge { file })
    }

    /// Wrap an inherited backend fd (the spawn passes the VMM its
    /// translator socketpair end by number). The fd must speak the
    /// same frame contract as a TAP: one whole frame per datagram,
    /// 12-byte vnet header first. Ownership transfers here.
    pub fn from_fd(fd: i32) -> io::Result<Self> {
        // SAFETY: the caller (parse_args) hands an fd this process
        // inherited and owns; From_raw_fd takes ownership exactly
        // once.
        let file = unsafe { <File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
        Self::arm(&file);
        Ok(Edge { file })
    }

    /// Non-blocking + O_ASYNC + F_SETOWN, both kinds: RX is driven
    /// by an external poll (see main.rs), and an arriving frame
    /// raises SIGIO at this process, which interrupts KVM_RUN
    /// (EINTR) so the run loop can drain RX. With the in-kernel
    /// irqchip an idle guest blocks inside KVM_RUN indefinitely;
    /// without this kick, world->guest mail would sit unread until
    /// some unrelated VM exit.
    fn arm(file: &File) {
        let fd = file.as_raw_fd();
        // SAFETY: fd is valid and open for the lifetime of `file`.
        unsafe {
            libc::fcntl(fd, libc::F_SETOWN, libc::getpid());
            let flags = libc::fcntl(fd, libc::F_GETFL, 0);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK | libc::O_ASYNC);
        }
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
