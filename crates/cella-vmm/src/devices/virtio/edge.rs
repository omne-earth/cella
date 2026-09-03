//! The edge: one nic's backend fd (1.6.14e).
//!
//! A nic's backend is a file descriptor the VMM reads and writes
//! whole frames on: a SOCK_SEQPACKET connection to the machine's
//! own cella-network translator, handed in by the spawn as
//! --edge-fd. The frame contract: 12-byte virtio_net_hdr prefix,
//! one frame per read/write, O_ASYNC + SIGIO as the kick that
//! interrupts KVM_RUN when the world has mail. The membrane (park,
//! decide, release -- net.rs) never knows what stands beyond it.

use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;

pub struct Edge {
    file: File,
}

impl Edge {
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
