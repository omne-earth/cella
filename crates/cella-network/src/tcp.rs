//! The world side, stateful half (1.6.14e rung 4): TCP.
//!
//! One flow per (guest port, peer ip, peer port). Outbound: the
//! guest's SYN becomes a non-blocking connect; the connect's
//! completion becomes the SYN-ACK. Inbound (the knock, ruled
//! option a): a connection accepted on a mapped host port becomes
//! a SYN to the guest, and the guest's SYN-ACK becomes the ACK.
//! From there both directions are the same pump: guest segments
//! write to the socket in order; socket bytes become segments to
//! the guest, retransmitted from the unacked buffer on a timer --
//! a frozen machine loses frames at the edge by law, and the
//! translator, not the world's peer, carries that patience.
//!
//! What this is not: a full TCP. No SACK, no window scaling, no
//! congestion control, no urgent data. The guest's own stack does
//! the retransmission in its direction; this side does the minimum
//! a socket-backed translator needs, and every segment it emits is
//! checksummed against the pseudo-header like any other.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

const MSS: usize = 1400;
const RTO: Duration = Duration::from_millis(1000);
const MAX_RETRANSMITS: u32 = 30;
const WINDOW: u16 = 65535;

pub const FLAG_FIN: u8 = 0x01;
pub const FLAG_SYN: u8 = 0x02;
pub const FLAG_RST: u8 = 0x04;
pub const FLAG_PSH: u8 = 0x08;
pub const FLAG_ACK: u8 = 0x10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    /// Outbound: connect() in flight; the SYN-ACK waits on it.
    Connecting,
    /// Outbound: SYN-ACK sent; the guest's ACK completes it.
    SynAckSent,
    /// Inbound: SYN sent to the guest; its SYN-ACK completes it.
    SynToGuest,
    Established,
}

struct Unacked {
    seq: u32,
    data: Vec<u8>,
    sent: Instant,
    tries: u32,
}

pub struct Flow {
    pub fd: i32,
    state: State,
    /// The peer's address as the guest sees it.
    peer_ip: [u8; 4],
    peer_port: u16,
    guest_port: u16,
    /// The guest-side address that owns this flow (a forwarding
    /// guest's agent, not necessarily the contract address).
    guest_ip: [u8; 4],
    /// Next sequence number this side sends.
    snd_nxt: u32,
    /// Oldest unacknowledged byte this side sent.
    snd_una: u32,
    /// Next sequence number expected from the guest.
    rcv_nxt: u32,
    /// The guest's advertised window.
    guest_wnd: u32,
    unacked: VecDeque<Unacked>,
    guest_fin: bool,
    world_fin_sent: bool,
    /// Set when the socket reached EOF and the FIN is queued.
    world_eof: bool,
    pub last: Instant,
}

/// (guest ip, guest port, peer ip, peer port).
pub type Key = ([u8; 4], u16, [u8; 4], u16);

/// A segment to send to the guest, addressed by the flow's key.
pub struct Out {
    pub peer_ip: [u8; 4],
    pub peer_port: u16,
    pub guest_port: u16,
    pub guest_ip: [u8; 4],
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub data: Vec<u8>,
}

pub struct Tcp {
    flows: HashMap<Key, Flow>,
}

fn set_nonblocking(fd: i32) {
    // SAFETY: fcntl on a live fd this process owns.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL, 0);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
}

fn close_fd(fd: i32) {
    // SAFETY: the fd is this process's own.
    unsafe { libc::close(fd) };
}

fn isn() -> u32 {
    let mut b = [0u8; 4];
    // SAFETY: getrandom into a valid 4-byte buffer.
    unsafe { libc::getrandom(b.as_mut_ptr() as *mut libc::c_void, 4, 0) };
    u32::from_be_bytes(b)
}

fn sockaddr_in(ip: [u8; 4], port: u16) -> libc::sockaddr_in {
    // SAFETY: zeroed sockaddr_in is a valid value for the type.
    let mut a: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    a.sin_family = libc::AF_INET as libc::sa_family_t;
    a.sin_port = port.to_be();
    a.sin_addr.s_addr = u32::from_be_bytes(ip).to_be();
    a
}

impl Flow {
    fn out(&self, flags: u8, data: Vec<u8>, seq: u32) -> Out {
        Out {
            peer_ip: self.peer_ip,
            peer_port: self.peer_port,
            guest_port: self.guest_port,
            guest_ip: self.guest_ip,
            seq,
            ack: self.rcv_nxt,
            flags,
            data,
        }
    }

    /// Queue bytes toward the guest and emit their segment.
    fn push(&mut self, flags: u8, data: Vec<u8>, out: &mut Vec<Out>) {
        let seq = self.snd_nxt;
        let len = data.len() as u32 + u32::from(flags & (FLAG_SYN | FLAG_FIN) != 0);
        out.push(self.out(flags | FLAG_ACK, data.clone(), seq));
        if len > 0 {
            self.unacked.push_back(Unacked {
                seq,
                data,
                sent: Instant::now(),
                tries: 0,
            });
            self.snd_nxt = self.snd_nxt.wrapping_add(len);
        }
    }
}

impl Default for Tcp {
    fn default() -> Self {
        Self::new()
    }
}

impl Tcp {
    pub fn new() -> Self {
        Tcp {
            flows: HashMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.flows.is_empty()
    }

    /// A TCP segment from the guest (the IP payload). `src`/`dst`
    /// are the IP addresses. Returns segments for the guest.
    pub fn from_guest(&mut self, src: [u8; 4], dst: [u8; 4], seg: &[u8]) -> Vec<Out> {
        let mut out = Vec::new();
        if seg.len() < 20 {
            return out;
        }
        let sport = u16::from_be_bytes([seg[0], seg[1]]);
        let dport = u16::from_be_bytes([seg[2], seg[3]]);
        let seq = u32::from_be_bytes([seg[4], seg[5], seg[6], seg[7]]);
        let ack = u32::from_be_bytes([seg[8], seg[9], seg[10], seg[11]]);
        let doff = ((seg[12] >> 4) as usize) * 4;
        let flags = seg[13];
        let wnd = u32::from(u16::from_be_bytes([seg[14], seg[15]]));
        if seg.len() < doff {
            return out;
        }
        let payload = &seg[doff..];
        let key: Key = (src, sport, dst, dport);

        // A fresh SYN: outbound connect.
        if flags & FLAG_SYN != 0 && flags & FLAG_ACK == 0 {
            if self.flows.contains_key(&key) {
                return out; // a retransmitted SYN; the connect is in flight
            }
            // SAFETY: plain socket(2); checked below.
            let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
            if fd < 0 {
                return out;
            }
            set_nonblocking(fd);
            let addr = sockaddr_in(dst, dport);
            // SAFETY: addr is a valid sockaddr_in; fd is ours.
            let r = unsafe {
                libc::connect(
                    fd,
                    &addr as *const _ as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                )
            };
            if r < 0 {
                let e = std::io::Error::last_os_error();
                if e.raw_os_error() != Some(libc::EINPROGRESS) {
                    close_fd(fd);
                    // Refused at once: RST the guest.
                    out.push(Out {
                        peer_ip: dst,
                        peer_port: dport,
                        guest_port: sport,
                        guest_ip: src,
                        seq: 0,
                        ack: seq.wrapping_add(1),
                        flags: FLAG_RST | FLAG_ACK,
                        data: Vec::new(),
                    });
                    return out;
                }
            }
            let my_isn = isn();
            self.flows.insert(
                key,
                Flow {
                    fd,
                    state: State::Connecting,
                    peer_ip: dst,
                    peer_port: dport,
                    guest_port: sport,
                    guest_ip: src,
                    snd_nxt: my_isn,
                    snd_una: my_isn,
                    rcv_nxt: seq.wrapping_add(1),
                    guest_wnd: wnd,
                    unacked: VecDeque::new(),
                    guest_fin: false,
                    world_fin_sent: false,
                    world_eof: false,
                    last: Instant::now(),
                },
            );
            return out;
        }

        let Some(flow) = self.flows.get_mut(&key) else {
            // No flow: a stray segment gets a RST so the guest's
            // stack gives up cleanly.
            if flags & FLAG_RST == 0 {
                out.push(Out {
                    peer_ip: dst,
                    peer_port: dport,
                    guest_port: sport,
                    guest_ip: src,
                    seq: ack,
                    ack: seq.wrapping_add(payload.len() as u32),
                    flags: FLAG_RST | FLAG_ACK,
                    data: Vec::new(),
                });
            }
            return out;
        };
        flow.last = Instant::now();
        flow.guest_wnd = wnd;

        if flags & FLAG_RST != 0 {
            close_fd(flow.fd);
            self.flows.remove(&key);
            return out;
        }

        // Inbound handshake: the guest's SYN-ACK answers our SYN.
        if flow.state == State::SynToGuest {
            if flags & (FLAG_SYN | FLAG_ACK) == (FLAG_SYN | FLAG_ACK) {
                flow.rcv_nxt = seq.wrapping_add(1);
                flow.snd_una = ack;
                flow.unacked.clear();
                flow.state = State::Established;
                out.push(flow.out(FLAG_ACK, Vec::new(), flow.snd_nxt));
            }
            return out;
        }

        // Acknowledgements advance the unacked buffer.
        if flags & FLAG_ACK != 0 {
            flow.snd_una = ack;
            while let Some(front) = flow.unacked.front() {
                let len = front.data.len() as u32 + u32::from(front.data.is_empty()); // SYN/FIN occupy one
                if front.seq.wrapping_add(len).wrapping_sub(ack) as i32 <= 0 {
                    flow.unacked.pop_front();
                } else {
                    break;
                }
            }
            if flow.state == State::SynAckSent {
                flow.state = State::Established;
            }
        }

        if flow.state != State::Established {
            return out;
        }

        // In-order data writes through; anything else is re-acked.
        if seq == flow.rcv_nxt {
            if !payload.is_empty() {
                // SAFETY: payload is valid; fd is the flow's own.
                let n = unsafe {
                    libc::write(
                        flow.fd,
                        payload.as_ptr() as *const libc::c_void,
                        payload.len(),
                    )
                };
                if n < 0 {
                    // The world hung up: RST toward the guest.
                    out.push(flow.out(FLAG_RST | FLAG_ACK, Vec::new(), flow.snd_nxt));
                    close_fd(flow.fd);
                    self.flows.remove(&key);
                    return out;
                }
                flow.rcv_nxt = flow.rcv_nxt.wrapping_add(payload.len() as u32);
            }
            if flags & FLAG_FIN != 0 && !flow.guest_fin {
                flow.guest_fin = true;
                flow.rcv_nxt = flow.rcv_nxt.wrapping_add(1);
                // SAFETY: shutdown on the flow's own fd.
                unsafe { libc::shutdown(flow.fd, libc::SHUT_WR) };
            }
            if !payload.is_empty() || flags & FLAG_FIN != 0 {
                out.push(flow.out(FLAG_ACK, Vec::new(), flow.snd_nxt));
            }
        } else if !payload.is_empty() || flags & FLAG_FIN != 0 {
            out.push(flow.out(FLAG_ACK, Vec::new(), flow.snd_nxt));
        }
        if flow.guest_fin && flow.world_fin_sent && flow.unacked.is_empty() {
            close_fd(flow.fd);
            self.flows.remove(&key);
        }
        out
    }

    /// An accepted inbound connection on a mapped port: open the
    /// handshake toward the guest. The guest sees the peer's real
    /// address; the guest port is the mapped port.
    pub fn accept_inbound(
        &mut self,
        fd: i32,
        peer_ip: [u8; 4],
        peer_port: u16,
        guest_port: u16,
        guest_ip: [u8; 4],
    ) -> Vec<Out> {
        set_nonblocking(fd);
        let key: Key = (guest_ip, guest_port, peer_ip, peer_port);
        if let Some(old) = self.flows.remove(&key) {
            close_fd(old.fd);
        }
        let my_isn = isn();
        let mut flow = Flow {
            fd,
            state: State::SynToGuest,
            peer_ip,
            peer_port,
            guest_port,
            guest_ip,
            snd_nxt: my_isn,
            snd_una: my_isn,
            rcv_nxt: 0,
            guest_wnd: u32::from(WINDOW),
            unacked: VecDeque::new(),
            guest_fin: false,
            world_fin_sent: false,
            world_eof: false,
            last: Instant::now(),
        };
        let mut out = Vec::new();
        // A bare SYN (no ACK flag): push() ORs in ACK, so build it
        // by hand.
        out.push(Out {
            peer_ip,
            peer_port,
            guest_port,
            guest_ip,
            seq: my_isn,
            ack: 0,
            flags: FLAG_SYN,
            data: Vec::new(),
        });
        flow.unacked.push_back(Unacked {
            seq: my_isn,
            data: Vec::new(),
            sent: Instant::now(),
            tries: 0,
        });
        flow.snd_nxt = my_isn.wrapping_add(1);
        self.flows.insert(key, flow);
        out
    }

    /// The poll: connects completing, socket bytes becoming
    /// segments, retransmissions, and dead flows swept.
    pub fn poll(&mut self) -> Vec<Out> {
        let mut out = Vec::new();
        let mut dead: Vec<Key> = Vec::new();
        let mut buf = vec![0u8; MSS];
        for (key, flow) in self.flows.iter_mut() {
            match flow.state {
                State::Connecting => {
                    let mut err: libc::c_int = 0;
                    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
                    let mut pfd = libc::pollfd {
                        fd: flow.fd,
                        events: libc::POLLOUT,
                        revents: 0,
                    };
                    // SAFETY: pfd is a valid pollfd; zero timeout.
                    let ready = unsafe { libc::poll(&mut pfd, 1, 0) };
                    if ready <= 0 {
                        if flow.last.elapsed() > Duration::from_secs(20) {
                            out.push(flow.out(FLAG_RST | FLAG_ACK, Vec::new(), flow.snd_nxt));
                            dead.push(*key);
                        }
                        continue;
                    }
                    // SAFETY: err/len are valid out-params for SO_ERROR.
                    unsafe {
                        libc::getsockopt(
                            flow.fd,
                            libc::SOL_SOCKET,
                            libc::SO_ERROR,
                            &mut err as *mut _ as *mut libc::c_void,
                            &mut len,
                        );
                    }
                    if err != 0 {
                        out.push(flow.out(FLAG_RST | FLAG_ACK, Vec::new(), flow.snd_nxt));
                        dead.push(*key);
                        continue;
                    }
                    flow.push(FLAG_SYN, Vec::new(), &mut out);
                    flow.state = State::SynAckSent;
                }
                State::SynAckSent | State::SynToGuest => {}
                State::Established => {}
            }
            if flow.state == State::Established && !flow.world_eof {
                // Respect the guest's window: bytes in flight.
                let inflight = flow.snd_nxt.wrapping_sub(flow.snd_una);
                let room = flow.guest_wnd.saturating_sub(inflight) as usize;
                let mut budget = room.min(16 * MSS);
                while budget >= MSS {
                    // SAFETY: buf is valid; fd is the flow's own.
                    let n = unsafe {
                        libc::recv(flow.fd, buf.as_mut_ptr() as *mut libc::c_void, MSS, 0)
                    };
                    if n > 0 {
                        flow.push(FLAG_PSH, buf[..n as usize].to_vec(), &mut out);
                        budget -= n as usize;
                        continue;
                    }
                    if n == 0 {
                        flow.world_eof = true;
                        if !flow.world_fin_sent {
                            flow.push(FLAG_FIN, Vec::new(), &mut out);
                            flow.world_fin_sent = true;
                        }
                        break;
                    }
                    let e = std::io::Error::last_os_error();
                    if e.kind() != std::io::ErrorKind::WouldBlock {
                        out.push(flow.out(FLAG_RST | FLAG_ACK, Vec::new(), flow.snd_nxt));
                        dead.push(*key);
                    }
                    break;
                }
            }
            // Retransmit what the guest has not acknowledged. The
            // flow's addressing is copied out first: the buffer is
            // borrowed mutably for the whole walk.
            let now = Instant::now();
            let (peer_ip, peer_port, guest_port, guest_ip, rcv_nxt, state, fin_sent, snd_nxt) = (
                flow.peer_ip,
                flow.peer_port,
                flow.guest_port,
                flow.guest_ip,
                flow.rcv_nxt,
                flow.state,
                flow.world_fin_sent,
                flow.snd_nxt,
            );
            let mut gone = false;
            for u in flow.unacked.iter_mut() {
                if now.duration_since(u.sent) >= RTO {
                    if u.tries >= MAX_RETRANSMITS {
                        gone = true;
                        break;
                    }
                    let flags = if u.data.is_empty() {
                        if state == State::SynToGuest {
                            FLAG_SYN
                        } else if fin_sent && u.seq == snd_nxt.wrapping_sub(1) {
                            FLAG_FIN | FLAG_ACK
                        } else {
                            FLAG_SYN | FLAG_ACK
                        }
                    } else {
                        FLAG_PSH | FLAG_ACK
                    };
                    out.push(Out {
                        peer_ip,
                        peer_port,
                        guest_port,
                        guest_ip,
                        seq: u.seq,
                        ack: if state == State::SynToGuest {
                            0
                        } else {
                            rcv_nxt
                        },
                        flags,
                        data: u.data.clone(),
                    });
                    u.sent = now;
                    u.tries += 1;
                }
            }
            if gone {
                dead.push(*key);
            }
            if flow.guest_fin && flow.world_fin_sent && flow.unacked.is_empty() {
                dead.push(*key);
            }
        }
        for key in dead {
            if let Some(f) = self.flows.remove(&key) {
                close_fd(f.fd);
            }
        }
        out
    }
}

/// Build the TCP segment bytes (header + payload) with the checksum
/// computed over the pseudo-header for src -> dst.
pub fn segment(src: [u8; 4], dst: [u8; 4], o: &Out) -> Vec<u8> {
    let mut s = vec![0u8; 20 + o.data.len()];
    s[0..2].copy_from_slice(&o.peer_port.to_be_bytes());
    s[2..4].copy_from_slice(&o.guest_port.to_be_bytes());
    s[4..8].copy_from_slice(&o.seq.to_be_bytes());
    s[8..12].copy_from_slice(&o.ack.to_be_bytes());
    s[12] = 5 << 4;
    s[13] = o.flags;
    s[14..16].copy_from_slice(&WINDOW.to_be_bytes());
    s[20..].copy_from_slice(&o.data);
    let mut sum = 0u32;
    for pair in [&src[0..2], &src[2..4], &dst[0..2], &dst[2..4]] {
        sum += u32::from(u16::from_be_bytes([pair[0], pair[1]]));
    }
    sum += 6;
    sum += s.len() as u32;
    let mut chunks = s.chunks_exact(2);
    for c in &mut chunks {
        sum += u32::from(u16::from_be_bytes([c[0], c[1]]));
    }
    if let [last] = chunks.remainder() {
        sum += u32::from(u16::from_be_bytes([*last, 0]));
    }
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    let csum = !(sum as u16);
    s[16..18].copy_from_slice(&csum.to_be_bytes());
    s
}
