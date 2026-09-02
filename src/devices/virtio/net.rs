//! virtio-net, no offloads, single queue pair.
//!
//! We open the TAP with `IFF_VNET_HDR`, which means every frame the
//! kernel hands us (and every frame we hand it) is already prefixed with
//! a `virtio_net_hdr` -- the *same* struct the virtio-net queues carry
//! at the start of each chain (12 bytes under VIRTIO_F_VERSION_1; the
//! TAP is told so via TUNSETVNETHDRSZ, see tap.rs). That means TX and
//! RX both become "copy bytes between a descriptor chain and the TAP
//! fd" with no header translation at all.

use std::collections::HashMap;
use std::sync::Arc;

use virtio_queue::{Queue, QueueOwnedT, QueueT};
use vm_memory::{Bytes, GuestMemoryMmap};

use super::tap::Tap;
use super::{ValveState, VirtioDevice, VIRTIO_F_VERSION_1};
use crate::ledger::{self, Dest, GuestClock, OpenOperation};
use crate::proto;

const QUEUE_RX: u16 = 0;
const QUEUE_TX: u16 = 1;

const VIRTIO_NET_F_MAC: u64 = 1 << 5;
const MAX_FRAME: usize = 65550; // 65535 + vnet hdr + slack

/// One held egress flow: the frames of every parked TX frame that
/// shares a destination group under one identifier (see
/// docs/NETWORK-MODEL.md, "Egress parks for decisions"). A
/// retransmitted SYN joins the operation that is already parked for
/// its destination; it does not mint a second one.
struct ParkedOp {
    id: [u8; 16],
    dest: Dest,
    /// The descriptor head index and the frame bytes, oldest first
    /// -- the same shape the sidecar and the thaw delivery expect.
    frames: Vec<(u16, Vec<u8>)>,
}

pub struct Net {
    tap: Tap,
    mac: [u8; 6],
    valve: ValveState,
    /// Egress frames read from the TX ring and not yet written to the
    /// TAP: grouped into operations by destination. The guest
    /// considers these sent, and their completion is owed (see
    /// docs/DEVICE-STATE.md).
    parked: Vec<ParkedOp>,
    /// What tells an operation its guest-frame timestamp at the
    /// instant it parks (see docs/NETWORK-MODEL.md, the Operation
    /// message).
    guest_clock: Arc<dyn GuestClock>,
    /// Ledger events waiting for the run loop to append to the
    /// chronicle -- one per new operation, never one per frame.
    pending_ledger: Vec<proto::Event>,
}

/// The name of one egress frame, at the most primitive level a
/// frame has. An IPv4 frame refines to (ip, port, proto); every
/// other frame -- ARP, IPv6, anything a future stack invents --
/// is named by its ethertype and destination MAC. No frame goes
/// unnamed, thus no frame goes unparked. The frame starts with
/// the 12-byte vnet header.
fn frame_name(frame: &[u8]) -> Dest {
    let l2_name = || {
        let mut mac = [0u8; 6];
        if let Some(b) = frame.get(12..18) {
            mac.copy_from_slice(b);
        }
        let ethertype = frame
            .get(24..26)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
            .unwrap_or(0);
        Dest::L2 { ethertype, mac }
    };
    let ipv4 = || -> Option<Dest> {
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
        Some(Dest::Ipv4 {
            ip: dst,
            port,
            proto,
        })
    };
    ipv4().unwrap_or_else(l2_name)
}

/// The one open operation for a key, or nothing. The matcher never
/// guesses: zero candidates is a gap, two or more is ambiguity,
/// and both re-mint a fresh id that stays held -- a collision
/// costs a decision, never a leak.
fn match_open<'a>(open: &'a [OpenOperation], dest: &Dest) -> Option<&'a OpenOperation> {
    let mut it = open.iter().filter(|o| o.dest == *dest);
    let first = it.next()?;
    if it.next().is_some() {
        return None;
    }
    Some(first)
}

/// An id from the ledger (a Vec<u8>, since that is the wire type) as
/// the fixed array ParkedOp keeps. Short or long input pads or
/// truncates rather than panicking: a malformed ledger entry must
/// not crash the VMM.
fn id_array(bytes: &[u8]) -> [u8; 16] {
    let mut id = [0u8; 16];
    let n = bytes.len().min(16);
    id[..n].copy_from_slice(&bytes[..n]);
    id
}

impl Net {
    pub fn new(
        tap_name: &str,
        mac: [u8; 6],
        guest_clock: Arc<dyn GuestClock>,
    ) -> std::io::Result<Self> {
        Ok(Net {
            tap: Tap::open(tap_name)?,
            mac,
            valve: ValveState::Closed,
            parked: Vec::new(),
            guest_clock,
            pending_ledger: Vec::new(),
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
        // Closed: nothing goes in. The TAP drains (the host must
        // not see backpressure from a dark machine) and every frame
        // discards; the guest's posted buffers stay posted.
        if self.valve == ValveState::Closed {
            while self.tap.read_frame(&mut buf).is_ok() {}
            return false;
        }
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
            match self.valve {
                // Closed: nothing goes out. The frame drops and
                // completes (the guest owns its buffer back); no
                // park, no ledger, no freeze.
                ValveState::Closed => {
                    let _ = queue.add_used(mem, head_index, 0);
                    used_any = true;
                    continue;
                }
                // The membrane: every egress frame parks under
                // its most primitive name -- ARP, IPv6, kernel
                // chatter, initiations and replies alike. No
                // exemptions, no pass entries: every park is a
                // fresh decision. The park point sits after the
                // read from the TX ring, and before any write to
                // the TAP. No completion here -- a decision
                // releases the operation, or the thaw delivers
                // it. The one door to the TAP is write_egress,
                // on the decision-delivery path alone.
                ValveState::Open => {
                    self.park(frame_name(&buf[..len]), head_index, buf[..len].to_vec());
                }
            }
        }
        used_any
    }

    /// Join a frame to the operation already parked for its
    /// destination, or open a new one. A retransmitted SYN joins
    /// silently; a new destination mints an id, reports (the line
    /// the engine reads today), and queues the Parked ledger event
    /// -- one per operation, never one per frame (see
    /// docs/NETWORK-MODEL.md, "one decision per new part of the
    /// world").
    fn park(&mut self, dest: Dest, head_index: u16, frame: Vec<u8>) {
        if let Some(op) = self.parked.iter_mut().find(|op| op.dest == dest) {
            op.frames.push((head_index, frame));
            return;
        }
        eprintln!("cella: parked egress to {dest}");
        // One clock read names both the id's timestamp bits and the
        // Operation.guest_ns field: the id and the field must agree
        // on the instant, not describe two independent reads of it.
        let guest_ns = self.guest_clock.now_ns();
        let id = ledger::uuid7(guest_ns);
        self.pending_ledger.push(proto::Event {
            event: Some(proto::event::Event::Parked(proto::Operation {
                id: id.to_vec(),
                destination: Some(dest.to_message()),
                guest_ns,
                host_ns: ledger::host_ns_now(),
            })),
        });
        self.parked.push(ParkedOp {
            id,
            dest,
            frames: vec![(head_index, frame)],
        });
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

    fn set_valve(&mut self, v: ValveState) {
        self.valve = v;
    }

    fn held_frames(&self) -> Vec<(u16, Vec<u8>)> {
        self.parked
            .iter()
            .flat_map(|op| op.frames.clone())
            .collect()
    }

    fn restore_held(&mut self, frames: Vec<(u16, Vec<u8>)>, open: &[OpenOperation]) {
        // The freeze suspended these operations; it did not resolve
        // them. Each restored frame rejoins the id it parked under
        // -- the chronicle, not a remint, is the index across a
        // freeze (see docs/NETWORK-MODEL.md, "Egress parks for
        // decisions"). Frames of one destination are grouped here
        // exactly as park() groups them live, so a multi-frame
        // operation (a SYN and its retransmits) becomes one
        // ParkedOp again, not one per frame.
        let mut rebuilt: Vec<ParkedOp> = Vec::new();
        for (head, bytes) in frames {
            let dest = frame_name(&bytes);
            if let Some(existing) = rebuilt.iter_mut().find(|op| op.dest == dest) {
                existing.frames.push((head, bytes));
                continue;
            }
            let id = match match_open(open, &dest) {
                Some(op) => id_array(&op.id),
                None => {
                    // No single open ledger entry names this key:
                    // zero is a gap in the book, two or more is a
                    // collision, and the matcher never guesses.
                    // Mint fresh, and say so both on the console
                    // and in the chronicle, so the anomaly is
                    // visible rather than silently absorbed. The
                    // frame stays held: a collision costs a
                    // decision, never a leak.
                    let guest_ns = self.guest_clock.now_ns();
                    let fresh = ledger::uuid7(guest_ns);
                    eprintln!(
                        "cella: no unambiguous open operation at thaw for \
                         {dest}, minted {}",
                        ledger::hex(&fresh)
                    );
                    self.pending_ledger.push(proto::Event {
                        event: Some(proto::event::Event::Parked(proto::Operation {
                            id: fresh.to_vec(),
                            destination: Some(dest.to_message()),
                            guest_ns,
                            host_ns: ledger::host_ns_now(),
                        })),
                    });
                    fresh
                }
            };
            rebuilt.push(ParkedOp {
                id,
                dest,
                frames: vec![(head, bytes)],
            });
        }
        self.parked = rebuilt;
    }

    fn write_egress(&mut self, frame: &[u8]) {
        let _ = self.tap.write_frame(frame);
    }

    fn egress_queue(&self) -> u16 {
        QUEUE_TX
    }

    fn drain_ledger_events(&mut self) -> Vec<proto::Event> {
        std::mem::take(&mut self.pending_ledger)
    }

    fn resolve_decisions(
        &mut self,
        decisions: &HashMap<Vec<u8>, proto::Decision>,
    ) -> Vec<(u16, Vec<u8>)> {
        // Oldest-parked first, and strictly in that order: an
        // operation resolves only once every operation parked
        // before it has itself resolved (see docs/NETWORK-MODEL.md
        // -- the ratchet is deterministic in the guest's frame). A
        // decision for an operation not at the front waits;
        // reapplying an already-resolved decision finds nothing at
        // the front to match and is harmless.
        let mut released_frames = Vec::new();
        while let Some(front) = self.parked.first() {
            let Some(decision) = decisions.get(front.id.as_slice()) else {
                break;
            };
            let op = self.parked.remove(0);
            match &decision.decision {
                Some(proto::decision::Decision::Release(_)) => {
                    let bytes_out: u64 = op.frames.iter().map(|(_, f)| f.len() as u64).sum();
                    self.pending_ledger.push(proto::Event {
                        event: Some(proto::event::Event::Released(proto::Released {
                            id: op.id.to_vec(),
                            first_response_ns: 0,
                            bytes_in: 0,
                            bytes_out,
                        })),
                    });
                    released_frames.extend(op.frames);
                }
                Some(proto::decision::Decision::Refusal(refusal)) => {
                    self.pending_ledger.push(proto::Event {
                        event: Some(proto::event::Event::Lapsed(proto::Lapsed {
                            id: op.id.to_vec(),
                            why: refusal.why.clone(),
                        })),
                    });
                }
                None => {}
            }
        }
        released_frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(id: u8, dest: Dest) -> OpenOperation {
        OpenOperation {
            id: vec![id; 16],
            dest,
            guest_ns: 0,
        }
    }

    const ARP: Dest = Dest::L2 {
        ethertype: 0x0806,
        mac: [0xff; 6],
    };
    const WWW: Dest = Dest::Ipv4 {
        ip: [10, 0, 0, 2],
        port: 443,
        proto: 6,
    };

    /// A frame with one open operation for its key rejoins that
    /// operation; the matcher needs exactly one candidate.
    #[test]
    fn one_candidate_matches() {
        let open = vec![op(1, ARP), op(2, WWW)];
        assert_eq!(match_open(&open, &ARP).unwrap().id, vec![1; 16]);
        assert_eq!(match_open(&open, &WWW).unwrap().id, vec![2; 16]);
    }

    /// The matcher never guesses: a colliding sidecar (two open
    /// operations under one key) matches nothing, thus the restore
    /// path re-mints a fresh id and the frame stays held. A
    /// collision costs a decision, never a leak.
    #[test]
    fn a_colliding_sidecar_matches_nothing() {
        let open = vec![op(1, WWW), op(2, WWW)];
        assert!(match_open(&open, &WWW).is_none());
        let none: Vec<OpenOperation> = Vec::new();
        assert!(match_open(&none, &WWW).is_none());
    }

    /// Every frame gets a name: IPv4 refines, ARP and IPv6 name at
    /// the Ethernet layer, and a runt frame still names (zeroed) --
    /// no frame goes unnamed, thus no frame goes unparked.
    #[test]
    fn every_frame_has_a_name() {
        // 12B vnet header, then dst MAC, src MAC, ethertype.
        let mut arp = vec![0u8; 12];
        arp.extend_from_slice(&[0xff; 6]);
        arp.extend_from_slice(&[0x02; 6]);
        arp.extend_from_slice(&[0x08, 0x06]);
        arp.extend_from_slice(&[0u8; 28]);
        assert_eq!(frame_name(&arp), ARP);

        let mut v6 = vec![0u8; 12];
        v6.extend_from_slice(&[0x33, 0x33, 0, 0, 0, 0x16]);
        v6.extend_from_slice(&[0x02; 6]);
        v6.extend_from_slice(&[0x86, 0xdd]);
        v6.extend_from_slice(&[0u8; 40]);
        assert_eq!(
            frame_name(&v6),
            Dest::L2 {
                ethertype: 0x86dd,
                mac: [0x33, 0x33, 0, 0, 0, 0x16],
            }
        );

        let runt = vec![0u8; 4];
        assert_eq!(
            frame_name(&runt),
            Dest::L2 {
                ethertype: 0,
                mac: [0; 6],
            }
        );
    }
}
