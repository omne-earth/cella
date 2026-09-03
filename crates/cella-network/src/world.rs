//! The world side of the translator, stateless half (1.6.14e
//! rung 3): ARP, ICMP echo, UDP.
//!
//! A world nic's frames come from the VMM already decided; this
//! module turns them into unprivileged socket calls and turns the
//! answers back into frames. It is not a resolver, not a cache,
//! and not a policy point: DNS is only UDP here, and nothing
//! crosses that a membrane did not release.
//!
//! The guest contract is the pool convention, byte for byte: the
//! guest is 192.168.210.2, its gateway is 192.168.210.1, and the
//! gateway's MAC is deterministic. ARP for the gateway answers
//! locally; an ICMP echo to the gateway answers locally (the edge
//! is a next hop, and next hops answer pings); everything else
//! IPv4 goes to the world through sockets.
//!
//! Flows: ICMP uses one SOCK_DGRAM ICMP socket per echo id (the
//! kernel rewrites ids per socket, so the id is the flow key);
//! UDP uses one socket per (guest port, peer ip, peer port). A
//! reply arrives on its flow's socket, and the flow key rebuilds
//! the frame's addressing. Idle flows die after a timeout sweep.

use std::collections::HashMap;

use std::time::{Duration, Instant};

use cella_libs::config;

use crate::tcp;

/// The 12-byte virtio_net_hdr both edge directions carry.
const VNET: usize = 12;
const ETH: usize = 14;

/// Idle flows die after this long without traffic, sockets and
/// all. Long enough for a judged round trip through a freeze.
const FLOW_IDLE: Duration = Duration::from_secs(300);

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

struct Flow {
    fd: i32,
    last: Instant,
    /// The guest-side address that opened the flow: the reply goes
    /// back to it. A gateway guest forwards its agent's frames with
    /// the agent's own address and no NAT, so this is not always
    /// WORLD_GUEST_IP -- the edge is an honest next hop.
    guest_ip: [u8; 4],
    /// For a knock flow: the peer's real address, when the guest
    /// was shown the gateway's instead (a loopback peer). The
    /// guest's answer goes here.
    real_peer: Option<([u8; 4], u16)>,
}

/// The address the guest is shown as a knock's source. A peer on the
/// host's loopback would make the guest answer its own loopback, so
/// the knock appears to come from the gateway -- pasta's rule --
/// and the flow remembers where the answer really goes.
fn apparent_peer(ip: [u8; 4]) -> [u8; 4] {
    if ip[0] == 127 {
        config::WORLD_GW_IP
    } else {
        ip
    }
}

/// A mapped host port (the knock, ruled option a): the translator
/// listens here as the invoking user, and an arrival becomes a
/// frame that parks in the guest's ingress lane.
pub struct PortMap {
    pub port: u16,
    pub tcp: bool,
}

/// Parse the port map of a world nic: "1709/tcp+1709/udp" after
/// "world:". Unknown text is an error at create.
pub fn parse_ports(spec: &str) -> Result<Vec<PortMap>, String> {
    let mut v = Vec::new();
    for item in spec.split('+').filter(|s| !s.is_empty()) {
        let (p, proto) = item
            .split_once('/')
            .ok_or_else(|| format!("port map {item:?} -- want PORT/tcp or PORT/udp"))?;
        let port: u16 = p
            .parse()
            .map_err(|_| format!("port map {item:?} -- {p:?} is not a port"))?;
        let tcp = match proto {
            "tcp" => true,
            "udp" => false,
            other => return Err(format!("port map {item:?} -- {other:?} is not tcp or udp")),
        };
        v.push(PortMap { port, tcp });
    }
    Ok(v)
}

struct Listener {
    fd: i32,
    port: u16,
    tcp: bool,
}

/// One world nic's translation state.
pub struct World {
    guest_mac: [u8; 6],
    /// ICMP echo flows, by echo id.
    icmp: HashMap<u16, Flow>,
    /// UDP flows, by (guest port, peer ip, peer port).
    udp: HashMap<(u16, [u8; 4], u16), Flow>,
    tcp: tcp::Tcp,
    listeners: Vec<Listener>,
    /// Frames toward the guest produced outside a poll (TCP
    /// answers to guest segments); the next guest_frame or poll
    /// drains them.
    pending: Vec<Vec<u8>>,
    swept: Instant,
}

fn listen_on(port: u16, tcp: bool) -> Option<i32> {
    let ty = if tcp {
        libc::SOCK_STREAM
    } else {
        libc::SOCK_DGRAM
    };
    // SAFETY: plain socket(2); checked below.
    let fd = unsafe { libc::socket(libc::AF_INET, ty, 0) };
    if fd < 0 {
        return None;
    }
    let one: libc::c_int = 1;
    // SAFETY: setsockopt with a valid int option on our fd.
    unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            &one as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
    let addr = sockaddr_in([0, 0, 0, 0], port);
    // SAFETY: addr is a valid sockaddr_in; fd is ours.
    if unsafe {
        libc::bind(
            fd,
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    } < 0
    {
        close_fd(fd);
        return None;
    }
    // SAFETY: listen on our bound stream socket.
    if tcp && unsafe { libc::listen(fd, 16) } < 0 {
        close_fd(fd);
        return None;
    }
    set_nonblocking(fd);
    Some(fd)
}

impl World {
    pub fn new(guest_mac: [u8; 6], ports: &[PortMap]) -> Self {
        let mut listeners = Vec::new();
        for p in ports {
            match listen_on(p.port, p.tcp) {
                Some(fd) => listeners.push(Listener {
                    fd,
                    port: p.port,
                    tcp: p.tcp,
                }),
                None => eprintln!(
                    "cella-network: cannot listen on {}/{} -- the knock has no door here",
                    p.port,
                    if p.tcp { "tcp" } else { "udp" }
                ),
            }
        }
        World {
            guest_mac,
            icmp: HashMap::new(),
            udp: HashMap::new(),
            tcp: tcp::Tcp::new(),
            listeners,
            pending: Vec::new(),
            swept: Instant::now(),
        }
    }

    /// A frame from the VMM (vnet header first). Returns frames to
    /// send back to the VMM (ARP and gateway-echo answers happen
    /// here, at the edge); world-bound payloads leave via sockets.
    pub fn guest_frame(&mut self, frame: &[u8]) -> Vec<Vec<u8>> {
        self.sweep();
        let mut out = Vec::new();
        if frame.len() < VNET + ETH {
            return out;
        }
        let eth = &frame[VNET..];
        let ethertype = u16::from_be_bytes([eth[12], eth[13]]);
        match ethertype {
            0x0806 => {
                if let Some(reply) = self.arp_reply(eth) {
                    out.push(reply);
                }
            }
            0x0800 => {
                if let Some(reply) = self.ipv4_from_guest(&eth[ETH..]) {
                    out.push(reply);
                }
            }
            _ => {}
        }
        out.append(&mut self.pending);
        out
    }

    /// ARP request for the gateway -> reply with the gateway MAC.
    /// The edge answers for itself and for nothing else.
    fn arp_reply(&self, eth: &[u8]) -> Option<Vec<u8>> {
        let arp = &eth[ETH..];
        if arp.len() < 28 {
            return None;
        }
        let oper = u16::from_be_bytes([arp[6], arp[7]]);
        let target_ip: [u8; 4] = arp[24..28].try_into().ok()?;
        if oper != 1 || target_ip != config::WORLD_GW_IP {
            return None;
        }
        let sender_mac = &arp[8..14];
        let sender_ip = &arp[14..18];
        let mut f = vec![0u8; VNET + ETH + 28];
        let (eth_out, arp_out) = {
            let (a, b) = f[VNET..].split_at_mut(ETH);
            (a, b)
        };
        eth_out[0..6].copy_from_slice(sender_mac);
        eth_out[6..12].copy_from_slice(&config::WORLD_GW_MAC);
        eth_out[12..14].copy_from_slice(&0x0806u16.to_be_bytes());
        arp_out[0..8].copy_from_slice(&[0, 1, 8, 0, 6, 4, 0, 2]);
        arp_out[8..14].copy_from_slice(&config::WORLD_GW_MAC);
        arp_out[14..18].copy_from_slice(&config::WORLD_GW_IP);
        arp_out[18..24].copy_from_slice(sender_mac);
        arp_out[24..28].copy_from_slice(sender_ip);
        Some(f)
    }

    fn ipv4_from_guest(&mut self, ip: &[u8]) -> Option<Vec<u8>> {
        if ip.len() < 20 {
            return None;
        }
        let ihl = ((ip[0] & 0x0f) as usize) * 4;
        let proto = ip[9];
        let src: [u8; 4] = ip[12..16].try_into().ok()?;
        let dst: [u8; 4] = ip[16..20].try_into().ok()?;
        let payload = &ip[ihl..];
        match proto {
            1 => self.icmp_from_guest(src, dst, payload),
            17 => {
                self.udp_from_guest(src, dst, payload);
                None
            }
            6 => {
                // TCP (rung 4): the state machine answers with zero
                // or more segments; the first is returned here and
                // the rest ride the next poll.
                let outs = self.tcp.guest_segment(src, dst, payload);
                for o in outs {
                    let seg = tcp::segment(o.peer_ip, o.guest_ip, &o);
                    self.pending.push(build_ipv4_frame(
                        &self.guest_mac,
                        o.peer_ip,
                        o.guest_ip,
                        6,
                        &seg,
                    ));
                }
                None
            }
            _ => None,
        }
    }

    /// ICMP echo. To the gateway: answered at the edge (a next hop
    /// answers pings). To the world: one DGRAM ICMP socket per
    /// echo id carries it out.
    fn icmp_from_guest(&mut self, src: [u8; 4], dst: [u8; 4], icmp: &[u8]) -> Option<Vec<u8>> {
        if icmp.len() < 8 || icmp[0] != 8 {
            return None;
        }
        if dst == config::WORLD_GW_IP {
            let mut reply = icmp.to_vec();
            reply[0] = 0;
            fix_checksum(&mut reply, 2, None);
            return Some(build_ipv4_frame(&self.guest_mac, dst, src, 1, &reply));
        }
        let id = u16::from_be_bytes([icmp[4], icmp[5]]);
        let flow = self.icmp.entry(id).or_insert_with(|| {
            // SAFETY: plain socket(2); checked below.
            let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, libc::IPPROTO_ICMP) };
            if fd >= 0 {
                set_nonblocking(fd);
            }
            Flow {
                fd,
                last: Instant::now(),
                guest_ip: src,
                real_peer: None,
            }
        });
        flow.guest_ip = src;
        if flow.fd < 0 {
            return None;
        }
        flow.last = Instant::now();
        let addr = sockaddr_in(dst, 0);
        // SAFETY: icmp is a valid buffer; addr is a valid sockaddr_in.
        unsafe {
            libc::sendto(
                flow.fd,
                icmp.as_ptr() as *const libc::c_void,
                icmp.len(),
                0,
                &addr as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            );
        }
        None
    }

    fn udp_from_guest(&mut self, src: [u8; 4], dst: [u8; 4], udp: &[u8]) {
        if udp.len() < 8 {
            return;
        }
        let sport = u16::from_be_bytes([udp[0], udp[1]]);
        let dport = u16::from_be_bytes([udp[2], udp[3]]);
        let payload = &udp[8..];
        let key = (sport, dst, dport);
        let flow = self.udp.entry(key).or_insert_with(|| {
            // SAFETY: plain socket(2); checked below.
            let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
            if fd >= 0 {
                set_nonblocking(fd);
            }
            Flow {
                fd,
                last: Instant::now(),
                guest_ip: src,
                real_peer: None,
            }
        });
        flow.guest_ip = src;
        if flow.fd < 0 {
            return;
        }
        flow.last = Instant::now();
        let (to_ip, to_port) = flow.real_peer.unwrap_or((dst, dport));
        let addr = sockaddr_in(to_ip, to_port);
        // SAFETY: payload is valid; addr is a valid sockaddr_in.
        unsafe {
            libc::sendto(
                flow.fd,
                payload.as_ptr() as *const libc::c_void,
                payload.len(),
                0,
                &addr as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            );
        }
    }

    /// Poll every flow socket; world answers become frames for the
    /// VMM (which parks them in the ingress lane, like any frame).
    pub fn poll(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        out.append(&mut self.pending);
        let mut buf = vec![0u8; 65536];
        // The knock: arrivals on mapped ports. TCP accepts open a
        // handshake toward the guest; UDP datagrams become frames
        // to the mapped guest port, and the listener socket is the
        // flow's socket for the answer.
        let mut inbound_tcp: Vec<(i32, [u8; 4], u16, u16)> = Vec::new();
        for l in &self.listeners {
            if l.tcp {
                loop {
                    // SAFETY: zeroed sockaddr_in and a valid len out-param.
                    let mut peer: libc::sockaddr_in = unsafe { std::mem::zeroed() };
                    let mut len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
                    // SAFETY: accept on our listening fd with valid out-params.
                    let fd = unsafe {
                        libc::accept(l.fd, &mut peer as *mut _ as *mut libc::sockaddr, &mut len)
                    };
                    if fd < 0 {
                        break;
                    }
                    let ip = apparent_peer(u32::from_be(peer.sin_addr.s_addr).to_be_bytes());
                    let port = u16::from_be(peer.sin_port);
                    inbound_tcp.push((fd, ip, port, l.port));
                }
            } else {
                loop {
                    // SAFETY: zeroed sockaddr_in and a valid len out-param.
                    let mut peer: libc::sockaddr_in = unsafe { std::mem::zeroed() };
                    let mut len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
                    // SAFETY: recvfrom into a valid buffer with valid out-params.
                    let n = unsafe {
                        libc::recvfrom(
                            l.fd,
                            buf.as_mut_ptr() as *mut libc::c_void,
                            buf.len(),
                            0,
                            &mut peer as *mut _ as *mut libc::sockaddr,
                            &mut len,
                        )
                    };
                    if n <= 0 {
                        break;
                    }
                    let real_ip = u32::from_be(peer.sin_addr.s_addr).to_be_bytes();
                    let port = u16::from_be(peer.sin_port);
                    let ip = apparent_peer(real_ip);
                    // The answer path: the flow keyed by the guest's
                    // mapped port and the apparent peer sends on the
                    // listener, to the real peer.
                    let fl = self.udp.entry((l.port, ip, port)).or_insert(Flow {
                        fd: l.fd,
                        last: Instant::now(),
                        guest_ip: config::WORLD_GUEST_IP,
                        real_peer: None,
                    });
                    fl.last = Instant::now();
                    fl.real_peer = Some((real_ip, port));
                    let mut udp = Vec::with_capacity(8 + n as usize);
                    udp.extend_from_slice(&port.to_be_bytes());
                    udp.extend_from_slice(&l.port.to_be_bytes());
                    udp.extend_from_slice(&(((8 + n) as u16).to_be_bytes()));
                    udp.extend_from_slice(&[0, 0]);
                    udp.extend_from_slice(&buf[..n as usize]);
                    fix_udp_checksum(&mut udp, ip, config::WORLD_GUEST_IP);
                    out.push(build_ipv4_frame(
                        &self.guest_mac,
                        ip,
                        config::WORLD_GUEST_IP,
                        17,
                        &udp,
                    ));
                }
            }
        }
        for (fd, ip, port, guest_port) in inbound_tcp {
            for o in self
                .tcp
                .accept_inbound(fd, ip, port, guest_port, config::WORLD_GUEST_IP)
            {
                let seg = tcp::segment(o.peer_ip, o.guest_ip, &o);
                out.push(build_ipv4_frame(
                    &self.guest_mac,
                    o.peer_ip,
                    o.guest_ip,
                    6,
                    &seg,
                ));
            }
        }
        for o in self.tcp.poll() {
            let seg = tcp::segment(o.peer_ip, o.guest_ip, &o);
            out.push(build_ipv4_frame(
                &self.guest_mac,
                o.peer_ip,
                o.guest_ip,
                6,
                &seg,
            ));
        }
        for (id, flow) in self.icmp.iter_mut() {
            loop {
                // SAFETY: buf is valid; fd is this flow's own.
                let n = unsafe {
                    libc::recv(flow.fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0)
                };
                if n <= 0 {
                    break;
                }
                let mut icmp = buf[..n as usize].to_vec();
                if icmp.len() >= 8 {
                    // The kernel rewrote the id on the wire; the
                    // guest knows the flow by the id it sent.
                    icmp[4..6].copy_from_slice(&id.to_be_bytes());
                    fix_checksum(&mut icmp, 2, None);
                    flow.last = Instant::now();
                    out.push(build_ipv4_frame(
                        &self.guest_mac,
                        config::WORLD_GW_IP,
                        flow.guest_ip,
                        1,
                        &icmp,
                    ));
                }
            }
        }
        for ((sport, peer_ip, peer_port), flow) in self.udp.iter_mut() {
            loop {
                // SAFETY: buf is valid; fd is this flow's own.
                let n = unsafe {
                    libc::recv(flow.fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0)
                };
                if n <= 0 {
                    break;
                }
                flow.last = Instant::now();
                let mut udp = Vec::with_capacity(8 + n as usize);
                udp.extend_from_slice(&peer_port.to_be_bytes());
                udp.extend_from_slice(&sport.to_be_bytes());
                udp.extend_from_slice(&(((8 + n) as u16).to_be_bytes()));
                udp.extend_from_slice(&[0, 0]);
                udp.extend_from_slice(&buf[..n as usize]);
                fix_udp_checksum(&mut udp, *peer_ip, flow.guest_ip);
                out.push(build_ipv4_frame(
                    &self.guest_mac,
                    *peer_ip,
                    flow.guest_ip,
                    17,
                    &udp,
                ));
            }
        }
        out
    }

    fn sweep(&mut self) {
        if self.swept.elapsed() < Duration::from_secs(30) {
            return;
        }
        self.swept = Instant::now();
        self.icmp.retain(|_, f| {
            let live = f.last.elapsed() < FLOW_IDLE;
            if !live {
                close_fd(f.fd);
            }
            live
        });
        let listener_fds: Vec<i32> = self.listeners.iter().map(|l| l.fd).collect();
        self.udp.retain(|_, f| {
            let live = f.last.elapsed() < FLOW_IDLE;
            if !live && !listener_fds.contains(&f.fd) {
                close_fd(f.fd);
            }
            live
        });
    }
}

fn sockaddr_in(ip: [u8; 4], port: u16) -> libc::sockaddr_in {
    // SAFETY: zeroed sockaddr_in is a valid value for the type.
    let mut a: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    a.sin_family = libc::AF_INET as libc::sa_family_t;
    a.sin_port = port.to_be();
    a.sin_addr.s_addr = u32::from_be_bytes(ip).to_be();
    a
}

/// Build a full edge frame (vnet header + ethernet + IPv4 +
/// payload) addressed to the guest.
fn build_ipv4_frame(
    guest_mac: &[u8; 6],
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    proto: u8,
    payload: &[u8],
) -> Vec<u8> {
    let total = 20 + payload.len();
    let mut f = vec![0u8; VNET + ETH + total];
    let eth = &mut f[VNET..];
    eth[0..6].copy_from_slice(guest_mac);
    eth[6..12].copy_from_slice(&config::WORLD_GW_MAC);
    eth[12..14].copy_from_slice(&0x0800u16.to_be_bytes());
    let ip = &mut eth[ETH..];
    ip[0] = 0x45;
    ip[2..4].copy_from_slice(&(total as u16).to_be_bytes());
    ip[8] = 64;
    ip[9] = proto;
    ip[12..16].copy_from_slice(&src_ip);
    ip[16..20].copy_from_slice(&dst_ip);
    let csum = internet_checksum(&ip[..20]);
    ip[10..12].copy_from_slice(&csum.to_be_bytes());
    ip[20..].copy_from_slice(payload);
    f
}

fn internet_checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    let mut chunks = data.chunks_exact(2);
    for c in &mut chunks {
        sum += u32::from(u16::from_be_bytes([c[0], c[1]]));
    }
    if let [last] = chunks.remainder() {
        sum += u32::from(u16::from_be_bytes([*last, 0]));
    }
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// Recompute a checksum stored at `data[at..at+2]`, optionally over
/// a pseudo-header sum already folded in by the caller.
fn fix_checksum(data: &mut [u8], at: usize, pseudo: Option<u32>) {
    data[at] = 0;
    data[at + 1] = 0;
    let mut sum = pseudo.unwrap_or(0);
    let mut chunks = data.chunks_exact(2);
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
    data[at..at + 2].copy_from_slice(&csum.to_be_bytes());
}

fn fix_udp_checksum(udp: &mut [u8], src: [u8; 4], dst: [u8; 4]) {
    let mut pseudo = 0u32;
    for pair in [&src[0..2], &src[2..4], &dst[0..2], &dst[2..4]] {
        pseudo += u32::from(u16::from_be_bytes([pair[0], pair[1]]));
    }
    pseudo += 17;
    pseudo += udp.len() as u32;
    fix_checksum(udp, 6, Some(pseudo));
}
