//! virtio-net, no offloads, single queue pair.
//!
//! We open the TAP with `IFF_VNET_HDR`, which means every frame the
//! kernel hands us (and every frame we hand it) is already prefixed with
//! a `virtio_net_hdr` -- the *same* 10-byte struct the virtio-net queues
//! carry as the first descriptor. That means TX and RX both become "copy
//! bytes between a descriptor chain and the TAP fd" with no header
//! translation at all.

use virtio_queue::{Queue, QueueOwnedT, QueueT};
use vm_memory::{Bytes, GuestMemoryMmap};

use super::tap::Tap;
use super::VirtioDevice;

const QUEUE_RX: u16 = 0;
const QUEUE_TX: u16 = 1;

const VIRTIO_NET_F_MAC: u64 = 1 << 5;
const MAX_FRAME: usize = 65550; // 65535 + vnet hdr + slack

pub struct Net {
    tap: Tap,
    mac: [u8; 6],
}

impl Net {
    pub fn new(tap_name: &str, mac: [u8; 6]) -> std::io::Result<Self> {
        Ok(Net {
            tap: Tap::open(tap_name)?,
            mac,
        })
    }

    pub fn tap_fd(&self) -> i32 {
        self.tap.as_raw_fd()
    }

    /// Drain as many pending TAP frames as there are free RX descriptors.
    /// Called both on a guest QueueNotify(0) (new buffers posted) and
    /// externally when the TAP fd becomes readable.
    #[allow(clippy::while_let_loop)] // early-continue logic inside the loop body doesn't fit while-let cleanly
    fn drain_rx(&mut self, mem: &GuestMemoryMmap, queue: &mut Queue) -> bool {
        let mut used_any = false;
        let mut buf = vec![0u8; MAX_FRAME];
        loop {
            let Some(mut chain) = queue.pop_descriptor_chain(mem) else {
                break;
            };
            let n = match self.tap.read_frame(&mut buf) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    queue.go_to_previous_position();
                    break;
                }
                Err(_) => {
                    queue.go_to_previous_position();
                    break;
                }
            };

            let head_index = chain.head_index();
            let mut off = 0usize;
            for desc in chain.by_ref() {
                if off >= n {
                    break;
                }
                let take = (n - off).min(desc.len() as usize);
                if mem.write_slice(&buf[off..off + take], desc.addr()).is_err() {
                    break;
                }
                off += take;
            }
            let _ = queue.add_used(mem, head_index, n as u32);
            used_any = true;
        }
        used_any
    }

    #[allow(clippy::while_let_loop)] // early-continue logic inside the loop body doesn't fit while-let cleanly
    fn drain_tx(&mut self, mem: &GuestMemoryMmap, queue: &mut Queue) -> bool {
        let mut used_any = false;
        let mut buf = vec![0u8; MAX_FRAME];
        loop {
            let Some(chain) = queue.pop_descriptor_chain(mem) else {
                break;
            };
            let head_index = chain.head_index();
            let mut len = 0usize;
            for desc in chain {
                let take = desc.len() as usize;
                if len + take > buf.len() {
                    break;
                }
                if mem
                    .read_slice(&mut buf[len..len + take], desc.addr())
                    .is_err()
                {
                    break;
                }
                len += take;
            }
            let _ = self.tap.write_frame(&buf[..len]);
            let _ = queue.add_used(mem, head_index, 0);
            used_any = true;
        }
        used_any
    }
}

impl VirtioDevice for Net {
    fn device_type(&self) -> u32 {
        super::VIRTIO_ID_NET
    }

    fn num_queues(&self) -> u16 {
        2
    }

    fn features(&self) -> u64 {
        VIRTIO_NET_F_MAC
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        data.fill(0);
        if offset < 6 {
            let n = data.len().min(6 - offset as usize);
            data[..n].copy_from_slice(&self.mac[offset as usize..offset as usize + n]);
        }
    }

    fn process_queue(&mut self, idx: u16, mem: &GuestMemoryMmap, queue: &mut Queue) -> bool {
        match idx {
            QUEUE_RX => self.drain_rx(mem, queue),
            QUEUE_TX => self.drain_tx(mem, queue),
            _ => false,
        }
    }
}
