//! The translator, wire plane (1.6.14e rung 2).
//!
//! One process per machine, machine-lifetime: the machine's first
//! start spawns it, destroy kills it, and it stays alive across
//! each freeze and thaw. This rung carries wires only; the world
//! side arrives at rung 3 (see docs/ROOTLESS-NETWORK.md).
//!
//! The translator listens on `<machine-dir>/edge.sock`. Each VMM
//! run connects there at spawn, one connection per wire nic, and
//! opens with a one-byte hello that names the nic index. For each
//! wire nic the translator holds one wire connection to the peer
//! machine's translator, made at `$CELLA_HOME/wires/<name>`: the
//! machine with the smaller name (byte order) listens, the other
//! connects and retries until the peer is up.
//!
//! The pump does not parse. A frame from the VMM goes to the
//! wire's peer; a frame from the wire goes to the VMM. Each frame
//! was decided before a translator saw it, on both sides. While a
//! nic has no VMM connection (the machine is frozen, or between
//! runs), frames from the wire are discarded and counted -- lost
//! at the edge, by law; nothing buffers across the gap.

use std::io;

use cella_libs::{machine, seq};

use crate::world;

const MAX_FRAME: usize = 65550;

/// What stands on the far side of one nic.
enum Kind {
    /// A wire to a peer machine's translator.
    Wire {
        name: String,
        peer: Option<i32>,
        /// The listener, when this side holds the smaller machine
        /// name. None on the connecting side.
        listener: Option<i32>,
    },
    /// The world: L4 translation over unprivileged sockets
    /// (rung 3 -- see world.rs).
    World(world::World),
}

/// One nic: its index in the machine's nic order, its far side,
/// and the VMM connection (None while detached).
struct Nic {
    nic_index: u8,
    kind: Kind,
    vmm: Option<i32>,
    /// Frames discarded while no VMM connection stood.
    discarded: u64,
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

/// Read one frame. Ok(None) on EAGAIN (no frame now); Err on a
/// dead connection (EOF is a dead connection for SEQPACKET).
fn read_frame(fd: i32, buf: &mut [u8]) -> io::Result<Option<usize>> {
    // SAFETY: buf is a valid buffer; fd is live.
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n > 0 {
        return Ok(Some(n as usize));
    }
    if n == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "peer closed"));
    }
    let e = io::Error::last_os_error();
    if e.kind() == io::ErrorKind::WouldBlock {
        return Ok(None);
    }
    Err(e)
}

fn write_frame(fd: i32, frame: &[u8]) -> io::Result<()> {
    // SAFETY: frame is valid; fd is live.
    let n = unsafe { libc::write(fd, frame.as_ptr() as *const libc::c_void, frame.len()) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The wire rendezvous role: the smaller machine name (byte
/// order) listens, the larger connects. Deterministic, raceless.
fn is_listener(my_name: &str, peer_name: &str) -> bool {
    my_name < peer_name
}

/// The peer machine of `wire`: the one other marker under the
/// wires directory that names it. A translator runs as its own
/// machine's sub-uid and cannot read another machine's manifest,
/// thus each one leaves a marker `<wire>.<machine>` in the shared
/// wires directory at startup, and the peer is the other marker.
/// Scanned fresh on each reconnect attempt, so a peer created
/// after this translator still pairs.
fn wire_peer(wires_dir: &std::path::Path, my_name: &str, wire: &str) -> Option<String> {
    let prefix = format!("{wire}.");
    let entries = std::fs::read_dir(wires_dir).ok()?;
    for e in entries.flatten() {
        let file = e.file_name().to_string_lossy().to_string();
        if let Some(name) = file.strip_prefix(&prefix) {
            if name != my_name && !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// The listener's socket must admit the peer's sub-uid: a unix
/// connect needs write permission on the socket inode. The wires
/// directory itself admits only the machines' sub-uids (by ACL),
/// thus a world-writable socket inside it is open to exactly them.
fn admit_peer(path: &std::path::Path) {
    if let Ok(c) = std::ffi::CString::new(path.to_string_lossy().as_bytes()) {
        // SAFETY: a valid C path; chmod on our own socket file.
        unsafe { libc::chmod(c.as_ptr(), 0o666) };
    }
}

/// The translator's main loop. Runs until killed (destroy owns
/// the kill). `vm` is this machine's name.
pub fn run(vm: &str) -> Result<(), String> {
    let dir = machine::machine_dir(vm);
    let m = machine::read_manifest(vm)?;
    let mut nics: Vec<Nic> = m
        .net
        .split(',')
        .enumerate()
        .filter_map(|(i, n)| {
            let n = n.trim();
            let kind = if let Some(w) = n.strip_prefix("wire:") {
                Kind::Wire {
                    name: w.to_string(),
                    peer: None,
                    listener: None,
                }
            } else if n == "world" || n.starts_with("world:") {
                // The guest MAC convention of main.rs: base MAC,
                // last byte + nic index. "world:PORTS" carries the
                // knock's port map (create validated it).
                let mut mac: [u8; 6] = [0x02, 0xfc, 0x00, 0x00, 0x00, 0x01];
                mac[5] = mac[5].wrapping_add(i as u8);
                let ports = n
                    .strip_prefix("world:")
                    .map(world::parse_ports)
                    .transpose()
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                Kind::World(world::World::new(mac, &ports))
            } else {
                return None;
            };
            Some(Nic {
                nic_index: i as u8,
                kind,
                vmm: None,
                discarded: 0,
            })
        })
        .collect();
    if nics.is_empty() {
        return Err(format!(
            "machine {vm:?} has no wire or world nics -- no edge to run"
        ));
    }

    // The pid file: destroy reads it to kill this process; a stale
    // one from a dead run is overwritten (the machine's start
    // checks liveness before spawning a second translator).
    std::fs::write(dir.join("edge.pid"), format!("{}\n", std::process::id()))
        .map_err(|e| format!("writing edge.pid: {e}"))?;

    let edge_sock = dir.join("edge.sock");
    let vmm_listener =
        seq::listen(&edge_sock).map_err(|e| format!("listening on {edge_sock:?}: {e}"))?;
    set_nonblocking(vmm_listener);

    // The spawn made the wires directory and granted this sub-uid
    // rwx on it. The markers: one per wire nic, naming this machine.
    let wires_dir = machine::home().join("wires");
    for nic in &nics {
        if let Kind::Wire { name, .. } = &nic.kind {
            let marker = wires_dir.join(format!("{name}.{vm}"));
            std::fs::write(&marker, b"")
                .map_err(|e| format!("writing the wire marker {}: {e}", marker.display()))?;
        }
    }

    println!(
        "cella-network: edge for {vm:?} up -- {} nic(s), listening on edge.sock",
        nics.len()
    );

    let mut buf = vec![0u8; MAX_FRAME];
    loop {
        // New VMM connections: a hello byte names the nic index.
        // The VMM side blocks on connect+hello only at spawn, so a
        // blocking read here is bounded; still, be defensive and
        // take the hello non-blockingly with a short patience.
        if let Ok(conn) = seq::accept(vmm_listener) {
            let mut hello = [0u8; 1];
            // SAFETY: one byte into a valid buffer from a live fd.
            let n = unsafe { libc::read(conn, hello.as_mut_ptr() as *mut libc::c_void, 1) };
            if n == 1 {
                if let Some(nic) = nics.iter_mut().find(|w| w.nic_index == hello[0]) {
                    if let Some(old) = nic.vmm.take() {
                        close_fd(old);
                    }
                    set_nonblocking(conn);
                    nic.vmm = Some(conn);
                    println!(
                        "cella-network: nic {} attached (epoch connection)",
                        hello[0]
                    );
                } else {
                    close_fd(conn);
                }
            } else {
                close_fd(conn);
            }
        }

        // Wire ends: listen or connect per role, retried each pass
        // until they stand. World nics have no peer to raise.
        for nic in nics.iter_mut() {
            let Kind::Wire {
                name,
                peer,
                listener,
            } = &mut nic.kind
            else {
                continue;
            };
            if peer.is_some() {
                continue;
            }
            let Some(peer_name) = wire_peer(&wires_dir, vm, name) else {
                continue;
            };
            let path = wires_dir.join(&*name);
            if is_listener(vm, &peer_name) {
                if listener.is_none() {
                    if let Ok(l) = seq::listen(&path) {
                        admit_peer(&path);
                        set_nonblocking(l);
                        *listener = Some(l);
                    }
                }
                if let Some(l) = *listener {
                    if let Ok(conn) = seq::accept(l) {
                        set_nonblocking(conn);
                        *peer = Some(conn);
                        println!("cella-network: wire {name:?} accepted");
                    }
                }
            } else if let Ok(conn) = seq::connect(&path) {
                set_nonblocking(conn);
                *peer = Some(conn);
                println!("cella-network: wire {name:?} connected");
            }
        }

        // The pump. VMM -> far side, then far side -> VMM (or the
        // drain, when no VMM stands).
        let mut moved = false;
        for nic in nics.iter_mut() {
            // Guest -> far side.
            if let Some(vfd) = nic.vmm {
                loop {
                    match read_frame(vfd, &mut buf) {
                        Ok(Some(n)) => {
                            moved = true;
                            match &mut nic.kind {
                                Kind::Wire { peer, .. } => {
                                    if let Some(pfd) = *peer {
                                        if write_frame(pfd, &buf[..n]).is_err() {
                                            close_fd(pfd);
                                            *peer = None;
                                        }
                                    }
                                    // No peer: the wire is not up
                                    // yet; lost at the edge, like a
                                    // tap with no cable.
                                }
                                Kind::World(w) => {
                                    for reply in w.from_guest(&buf[..n]) {
                                        let _ = write_frame(vfd, &reply);
                                    }
                                }
                            }
                        }
                        Ok(None) => break,
                        Err(_) => {
                            close_fd(vfd);
                            nic.vmm = None;
                            println!("cella-network: nic {} detached", nic.nic_index);
                            break;
                        }
                    }
                }
            }
            // Far side -> guest (or the drain).
            let mut inbound: Vec<Vec<u8>> = Vec::new();
            let mut wire_dropped = false;
            match &mut nic.kind {
                Kind::Wire { peer, .. } => {
                    if let Some(pfd) = *peer {
                        loop {
                            match read_frame(pfd, &mut buf) {
                                Ok(Some(n)) => inbound.push(buf[..n].to_vec()),
                                Ok(None) => break,
                                Err(_) => {
                                    close_fd(pfd);
                                    *peer = None;
                                    wire_dropped = true;
                                    break;
                                }
                            }
                        }
                    }
                }
                Kind::World(w) => inbound = w.poll(),
            }
            if wire_dropped {
                if let Kind::Wire { name, .. } = &nic.kind {
                    println!("cella-network: wire {name:?} dropped");
                }
            }
            for frame in inbound {
                moved = true;
                match nic.vmm {
                    Some(vfd) => {
                        if write_frame(vfd, &frame).is_err() {
                            close_fd(vfd);
                            nic.vmm = None;
                            nic.discarded += 1;
                            println!(
                                "cella-network: nic {} detached; frame discarded (total {})",
                                nic.nic_index, nic.discarded
                            );
                        }
                    }
                    None => {
                        nic.discarded += 1;
                        println!(
                            "cella-network: frame discarded at the edge, nic {} detached                              (total {})",
                            nic.nic_index, nic.discarded
                        );
                    }
                }
            }
        }
        if !moved {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
}
