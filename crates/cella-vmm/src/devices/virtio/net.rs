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

use virtio_queue::{Queue, QueueT};
use vm_memory::{Bytes, GuestMemoryMmap};

use super::tap::Tap;
use super::{ValveState, VirtioDevice, VIRTIO_F_VERSION_1};
use cella_libs::ledger::{self, Dest, GuestClock, OpenOperation};
use cella_libs::proto;

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
    /// True when any frame parked since the last take: a frame that
    /// joins an existing operation emits no ledger event, and the
    /// park is the freeze for joins too (the one-shot rule).
    parked_flag: bool,
    /// The inbound lane: frames the world pushed under an open
    /// valve, held for a decision. An incoming hold never freezes
    /// the machine -- the world's knock is not the resident's deed
    /// -- and in the guest frame an undelivered packet is network
    /// latency. Its own lane: park order advances per direction.
    inbound: Vec<InboundOp>,
    /// Bytes held across the inbound lane. Egress holds are bounded
    /// by the ring and the freeze; ingress has neither bound, thus
    /// the cap: beyond it, frames drop exactly as closed drops them
    /// -- the protocols above retransmit -- and the counter counts.
    inbound_bytes: usize,
    inbound_dropped: u64,
    /// Released incoming frames awaiting free RX descriptors, in
    /// park order. A released operation that finds no posted buffer
    /// stays here -- delivery is stillness until the guest offers a
    /// descriptor, never a forced write.
    deliver_queue: std::collections::VecDeque<Vec<u8>>,
}

/// One held ingress flow: the frames of every inbound frame that
/// shares a source, grouped under one identifier -- the mirror of
/// ParkedOp, named by the sender.
struct InboundOp {
    id: [u8; 16],
    peer: Dest,
    frames: Vec<Vec<u8>>,
    /// True once the cap dropped a frame of this operation; the
    /// first drop logs, the rest count.
    dropped: bool,
}

/// The inbound lane's bounds. Beyond them the ear drops like the
/// closed valve drops, and the counter records that it knocked.
const MAX_INBOUND_OP_BYTES: usize = 256 * 1024;
const MAX_INBOUND_TOTAL_BYTES: usize = 1024 * 1024;

/// The name of one egress frame, at the most primitive level a
/// frame has. An IPv4 frame refines to (ip, port, proto); every
/// other frame -- ARP, IPv6, anything a future stack invents --
/// is named by its ethertype and destination MAC. No frame goes
/// unnamed, thus no frame goes unparked. The frame starts with
/// the 12-byte vnet header.
fn frame_name(frame: &[u8]) -> Dest {
    ledger::frame_dest_name(frame)
}

/// The name of one inbound frame: the sender, at the most
/// primitive level. An IPv4 frame refines to (source ip, source
/// port, proto); every other frame is named by its ethertype and
/// source MAC. The frame starts with the 12-byte vnet header.
fn frame_source_name(frame: &[u8]) -> Dest {
    ledger::frame_source_name(frame)
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
            parked_flag: false,
            inbound: Vec::new(),
            inbound_bytes: 0,
            inbound_dropped: 0,
            deliver_queue: std::collections::VecDeque::new(),
        })
    }

    pub fn tap_fd(&self) -> i32 {
        self.tap.as_raw_fd()
    }

    /// The RX pass. Released incoming frames deliver first, into
    /// free descriptors, in park order; a frame that finds no
    /// posted buffer stays queued -- stillness, never a forced
    /// write. Then the TAP drains: under Closed every frame
    /// discards; under Open every frame parks in the inbound lane
    /// under its source's most primitive name -- the ear's customs.
    /// No freeze: the world's knock is not the resident's deed.
    /// Called on a guest QueueNotify(0), on the TAP turning
    /// readable, and after an incoming decision applies.
    fn drain_rx(&mut self, mem: &GuestMemoryMmap, queue: &mut Queue) -> bool {
        let mut used_any = false;
        while let Some(frame) = self.deliver_queue.front() {
            let Some(mut chain) = queue.pop_descriptor_chain(mem) else {
                break;
            };
            let n = frame.len();
            let head_index = chain.head_index();
            let mut off = 0usize;
            for desc in chain.by_ref() {
                if off >= n {
                    break;
                }
                let take = (n - off).min(desc.len() as usize);
                if mem
                    .write_slice(&frame[off..off + take], desc.addr())
                    .is_err()
                {
                    break;
                }
                off += take;
            }
            let _ = queue.add_used(mem, head_index, n as u32);
            used_any = true;
            self.deliver_queue.pop_front();
        }
        let mut buf = vec![0u8; MAX_FRAME];
        while let Ok(n) = self.tap.read_frame(&mut buf) {
            match self.valve {
                // Closed: nothing goes in. The TAP drains (the
                // host must not see backpressure from a dark
                // machine) and every frame discards.
                ValveState::Closed => {}
                ValveState::Open => self.park_inbound(&buf[..n]),
            }
        }
        used_any
    }

    /// Park one inbound frame in its lane: join the held operation
    /// of its source, or mint a new one. The cap bounds the lane --
    /// beyond it the frame drops as closed drops it, counted, the
    /// first drop of each operation logged.
    fn park_inbound(&mut self, frame: &[u8]) {
        let peer = frame_source_name(frame);
        if let Some(op) = self.inbound.iter_mut().find(|op| op.peer == peer) {
            let op_bytes: usize = op.frames.iter().map(Vec::len).sum();
            if op_bytes + frame.len() > MAX_INBOUND_OP_BYTES
                || self.inbound_bytes + frame.len() > MAX_INBOUND_TOTAL_BYTES
            {
                self.inbound_dropped += 1;
                if !op.dropped {
                    op.dropped = true;
                    eprintln!(
                        "cella: inbound hold at its cap for {} -- dropping, the protocols retransmit",
                        op.peer
                    );
                }
                return;
            }
            self.inbound_bytes += frame.len();
            op.frames.push(frame.to_vec());
            return;
        }
        if self.inbound_bytes + frame.len() > MAX_INBOUND_TOTAL_BYTES {
            self.inbound_dropped += 1;
            return;
        }
        eprintln!("cella: parked ingress from {peer}");
        let guest_ns = self.guest_clock.now_ns();
        let id = ledger::uuid7(guest_ns);
        self.pending_ledger.push(proto::Event {
            predecessor: Vec::new(),
            event: Some(proto::event::Event::Parked(proto::Operation {
                id: id.to_vec(),
                destination: Some(peer.to_message()),
                guest_ns,
                host_ns: ledger::host_ns_now(),
                direction: proto::operation::Direction::Incoming as i32,
            })),
        });
        self.inbound_bytes += frame.len();
        self.inbound.push(InboundOp {
            id,
            peer,
            frames: vec![frame.to_vec()],
            dropped: false,
        });
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
        self.parked_flag = true;
        if let Some(op) = self.parked.iter_mut().find(|op| op.dest == dest) {
            if cfg!(debug_assertions) {
                eprintln!("cella: parked egress to {dest} (joined)");
            }
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
            predecessor: Vec::new(),
            event: Some(proto::event::Event::Parked(proto::Operation {
                id: id.to_vec(),
                destination: Some(dest.to_message()),
                guest_ns,
                host_ns: ledger::host_ns_now(),
                direction: proto::operation::Direction::Outgoing as i32,
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
                        predecessor: Vec::new(),
                        event: Some(proto::event::Event::Parked(proto::Operation {
                            id: fresh.to_vec(),
                            destination: Some(dest.to_message()),
                            guest_ns,
                            host_ns: ledger::host_ns_now(),
                            direction: proto::operation::Direction::Outgoing as i32,
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

    fn take_parked_flag(&mut self) -> bool {
        std::mem::take(&mut self.parked_flag)
    }

    fn held_op_ids(&self) -> Vec<Vec<u8>> {
        self.parked
            .iter()
            .map(|op| op.id.to_vec())
            .chain(self.inbound.iter().map(|op| op.id.to_vec()))
            .collect()
    }

    fn held_ingress(&self) -> Vec<Vec<u8>> {
        self.inbound
            .iter()
            .flat_map(|op| op.frames.clone())
            .collect()
    }

    fn deliverable_ingress(&self) -> Vec<Vec<u8>> {
        self.deliver_queue.iter().cloned().collect()
    }

    fn restore_ingress(
        &mut self,
        frames: Vec<Vec<u8>>,
        deliverable: Vec<Vec<u8>>,
        open: &[OpenOperation],
    ) {
        // The mirror of restore_held, for the inbound lane: each
        // restored frame rejoins its operation by its source's
        // primitive name, and the matcher never guesses -- zero or
        // many candidates re-mint fresh, and the frames stay held.
        let mut rebuilt: Vec<InboundOp> = Vec::new();
        let mut bytes = 0usize;
        for frame in frames {
            let peer = frame_source_name(&frame);
            bytes += frame.len();
            if let Some(existing) = rebuilt.iter_mut().find(|op| op.peer == peer) {
                existing.frames.push(frame);
                continue;
            }
            let id = match match_open(open, &peer) {
                Some(op) => id_array(&op.id),
                None => {
                    let guest_ns = self.guest_clock.now_ns();
                    let fresh = ledger::uuid7(guest_ns);
                    eprintln!(
                        "cella: no unambiguous open ingress operation at thaw for \
                         {peer}, minted {}",
                        ledger::hex(&fresh)
                    );
                    self.pending_ledger.push(proto::Event {
                        predecessor: Vec::new(),
                        event: Some(proto::event::Event::Parked(proto::Operation {
                            id: fresh.to_vec(),
                            destination: Some(peer.to_message()),
                            guest_ns,
                            host_ns: ledger::host_ns_now(),
                            direction: proto::operation::Direction::Incoming as i32,
                        })),
                    });
                    fresh
                }
            };
            rebuilt.push(InboundOp {
                id,
                peer,
                frames: vec![frame],
                dropped: false,
            });
        }
        self.inbound = rebuilt;
        self.inbound_bytes = bytes;
        self.deliver_queue = deliverable.into();
    }

    fn resolve_ingress(&mut self, decisions: &HashMap<Vec<u8>, proto::Decision>) -> bool {
        // The inbound lane's apply: its own park order, front
        // first, independent of the egress lane -- an undecided
        // egress must not block the mail, and undecided mail must
        // not block the thaw. A release moves the frames to the
        // deliver queue (free descriptors permitting -- see
        // drain_rx); a refusal drops them silently, in-frame
        // nothing arrived. Fail-closed: an undecided front stops
        // the lane, and nothing pops on failure.
        let mut moved = false;
        while let Some(front) = self.inbound.first() {
            let Some(decision) = decisions.get(front.id.as_slice()) else {
                break;
            };
            let op = self.inbound.remove(0);
            let op_bytes: usize = op.frames.iter().map(Vec::len).sum();
            self.inbound_bytes = self.inbound_bytes.saturating_sub(op_bytes);
            match &decision.decision {
                Some(proto::decision::Decision::Release(_)) => {
                    self.pending_ledger.push(proto::Event {
                        predecessor: Vec::new(),
                        event: Some(proto::event::Event::Released(proto::Released {
                            id: op.id.to_vec(),
                            first_response_ns: 0,
                            bytes_in: op_bytes as u64,
                            bytes_out: 0,
                        })),
                    });
                    self.deliver_queue.extend(op.frames);
                    moved = true;
                }
                Some(proto::decision::Decision::Refusal(refusal)) => {
                    self.pending_ledger.push(proto::Event {
                        predecessor: Vec::new(),
                        event: Some(proto::event::Event::Lapsed(proto::Lapsed {
                            id: op.id.to_vec(),
                            why: refusal.why.clone(),
                        })),
                    });
                    // The frames die unseen: no descriptor was
                    // ever posted for them, thus nothing completes
                    // and nothing wedges -- in the guest frame the
                    // packet simply never arrived.
                }
                None => {}
            }
        }
        moved
    }

    fn resolve_decisions(
        &mut self,
        decisions: &HashMap<Vec<u8>, proto::Decision>,
    ) -> (Vec<(u16, Vec<u8>)>, Vec<u16>) {
        // Oldest-parked first, and strictly in that order: an
        // operation resolves only once every operation parked
        // before it has itself resolved (see docs/NETWORK-MODEL.md
        // -- the ratchet is deterministic in the guest's frame). A
        // decision for an operation not at the front waits;
        // reapplying an already-resolved decision finds nothing at
        // the front to match and is harmless.
        let mut released_frames = Vec::new();
        let mut refused_heads = Vec::new();
        while let Some(front) = self.parked.first() {
            let Some(decision) = decisions.get(front.id.as_slice()) else {
                break;
            };
            let op = self.parked.remove(0);
            match &decision.decision {
                Some(proto::decision::Decision::Release(_)) => {
                    let bytes_out: u64 = op.frames.iter().map(|(_, f)| f.len() as u64).sum();
                    self.pending_ledger.push(proto::Event {
                        predecessor: Vec::new(),
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
                        predecessor: Vec::new(),
                        event: Some(proto::event::Event::Lapsed(proto::Lapsed {
                            id: op.id.to_vec(),
                            why: refusal.why.clone(),
                        })),
                    });
                    // The frames die, and the buffers return: a
                    // refusal answers the machine cleanly, in-frame
                    // -- an uncompleted descriptor would wedge the
                    // guest's TX queue (NETDEV watchdog).
                    refused_heads.extend(op.frames.iter().map(|(head, _)| *head));
                }
                None => {}
            }
        }
        (released_frames, refused_heads)
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
            incoming: false,
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

    /// The inbound name is the sender's: source MAC for the L2
    /// shape, source ip and port for IPv4 -- the mirror of the
    /// egress name, thus one frame carries two names and each lane
    /// reads its own side.
    #[test]
    fn the_inbound_name_is_the_senders() {
        // 12B vnet header, dst MAC, src MAC, ethertype 0x0800,
        // then an IPv4 header: src 192.168.200.1, dst .2, UDP
        // sport 9053, dport 68.
        let mut f = vec![0u8; 12];
        f.extend_from_slice(&[0x02; 6]); // dst MAC
        f.extend_from_slice(&[0xaa; 6]); // src MAC
        f.extend_from_slice(&[0x08, 0x00]);
        let mut ip = vec![0x45, 0, 0, 0, 0, 0, 0, 0, 64, 17, 0, 0];
        ip.extend_from_slice(&[192, 168, 200, 1]); // src
        ip.extend_from_slice(&[192, 168, 200, 2]); // dst
        ip.extend_from_slice(&9053u16.to_be_bytes()); // sport
        ip.extend_from_slice(&68u16.to_be_bytes()); // dport
        f.extend_from_slice(&ip);
        assert_eq!(
            frame_source_name(&f),
            Dest::Ipv4 {
                ip: [192, 168, 200, 1],
                port: 9053,
                proto: 17
            }
        );
        assert_eq!(
            frame_name(&f),
            Dest::Ipv4 {
                ip: [192, 168, 200, 2],
                port: 68,
                proto: 17
            }
        );
        // An ARP frame names inbound by its source MAC.
        let mut arp = vec![0u8; 12];
        arp.extend_from_slice(&[0xff; 6]);
        arp.extend_from_slice(&[0xbb; 6]);
        arp.extend_from_slice(&[0x08, 0x06]);
        arp.extend_from_slice(&[0u8; 28]);
        assert_eq!(
            frame_source_name(&arp),
            Dest::L2 {
                ethertype: 0x0806,
                mac: [0xbb; 6]
            }
        );
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
