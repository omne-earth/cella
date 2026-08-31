//! virtio-net, no offloads, single queue pair.
//!
//! We open the TAP with `IFF_VNET_HDR`, which means every frame the
//! kernel hands us (and every frame we hand it) is already prefixed with
//! a `virtio_net_hdr` -- the *same* struct the virtio-net queues carry
//! at the start of each chain (12 bytes under VIRTIO_F_VERSION_1; the
//! TAP is told so via TUNSETVNETHDRSZ, see tap.rs). That means TX and
//! RX both become "copy bytes between a descriptor chain and the TAP
//! fd" with no header translation at all.

use virtio_queue::{Queue, QueueOwnedT, QueueT};
use vm_memory::{Bytes, GuestMemoryMmap};

use super::tap::Tap;
use super::{VirtioDevice, VIRTIO_F_VERSION_1};

const QUEUE_RX: u16 = 0;
const QUEUE_TX: u16 = 1;

const VIRTIO_NET_F_MAC: u64 = 1 << 5;
const MAX_FRAME: usize = 65550; // 65535 + vnet hdr + slack

pub struct Net {
    tap: Tap,
    mac: [u8; 6],
    hold: bool,
    /// Egress frames read from the TX ring and not yet written to the
    /// TAP: the descriptor head index and the frame bytes. The guest
    /// considers these sent, and their completion is owed (see
    /// docs/DEVICE-STATE.md).
    parked: Vec<(u16, Vec<u8>)>,
    /// Pass entries, installed by an allow verdict: a destination
    /// IPv4 address and port whose frames flow at full speed under
    /// hold. The verdict cost is amortized per destination: one park
    /// for a destination without a pass entry, and an inline match
    /// for every frame after it.
    allowed: Vec<([u8; 4], u16)>,
}

/// The IPv4 destination of one egress frame: the address, the port
/// (0 when the protocol has none), and the protocol number. Returns
/// None for a non-IPv4 frame (ARP stays link-local housekeeping and
/// never parks). The frame starts with the 12-byte vnet header.
fn ipv4_destination(frame: &[u8]) -> Option<([u8; 4], u16, u8)> {
    let eth = frame.get(12..)?;
    if eth.get(12..14)? != [0x08, 0x00] {
        return None;
    }
    let ip = eth.get(14..)?;
    let ihl = ((*ip.first()? & 0x0f) as usize) * 4;
    let proto = *ip.get(9)?;
    let dst: [u8; 4] = ip.get(16..20)?.try_into().ok()?;
    let port = match proto {
        6 | 17 => u16::from_be_bytes(ip.get(ihl + 2..ihl + 4)?.try_into().ok()?),
        _ => 0,
    };
    Some((dst, port, proto))
}

impl Net {
    pub fn new(tap_name: &str, mac: [u8; 6]) -> std::io::Result<Self> {
        Ok(Net {
            tap: Tap::open(tap_name)?,
            mac,
            hold: false,
            parked: Vec::new(),
            allowed: Vec::new(),
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
            if self.hold {
                let dest = ipv4_destination(&buf[..len]);
                let pass = match dest {
                    // ARP and other non-IPv4 housekeeping never parks.
                    None => true,
                    Some((ip, port, _)) => self.allowed.contains(&(ip, port)),
                };
                if !pass {
                    // The park point: after the read from the TX ring,
                    // and before the write to the TAP. No completion
                    // here -- a verdict releases the frame, or the
                    // thaw delivers and completes it. The line below
                    // is the report primitive: the engine reads it.
                    if let Some((ip, port, proto)) = dest {
                        eprintln!(
                            "cella: parked egress to {}.{}.{}.{}:{port} proto {proto}",
                            ip[0], ip[1], ip[2], ip[3]
                        );
                    }
                    self.parked.push((head_index, buf[..len].to_vec()));
                    continue;
                }
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
        VIRTIO_F_VERSION_1 | VIRTIO_NET_F_MAC
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

    fn set_hold(&mut self, on: bool) {
        self.hold = on;
    }

    fn held_frames(&self) -> Vec<(u16, Vec<u8>)> {
        self.parked.clone()
    }

    fn restore_held(&mut self, frames: Vec<(u16, Vec<u8>)>) {
        self.parked = frames;
    }

    fn take_held(&mut self) -> Vec<(u16, Vec<u8>)> {
        std::mem::take(&mut self.parked)
    }

    fn write_egress(&mut self, frame: &[u8]) {
        let _ = self.tap.write_frame(frame);
    }

    fn egress_queue(&self) -> u16 {
        QUEUE_TX
    }

    fn allow(&mut self, ip: [u8; 4], port: u16) {
        if !self.allowed.contains(&(ip, port)) {
            self.allowed.push((ip, port));
        }
    }
}
