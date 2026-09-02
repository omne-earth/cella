//! The ledger: the chronicle of held operations, and the wire
//! primitives that write it. See docs/NETWORK-MODEL.md, "The
//! control plane" -- the ledger is a chronicle, never the store:
//! the held bytes live in the device's own memory (today, the
//! parked-frame lists of the virtio-net backend; in the appliance
//! of phase 2, its own guest RAM).

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use kvm_ioctls::VmFd;
use sha2::{Digest, Sha256};

use crate::proto::{self, Event, Message};

/// What a device needs from the VM to timestamp an operation in the
/// guest's own frame -- split out from a concrete VmFd, the same
/// reason mmio::IrqLine exists: the device stays testable in plain
/// userspace, with no /dev/kvm.
pub trait GuestClock: Send + Sync {
    fn now_ns(&self) -> u64;
}

impl GuestClock for VmFd {
    fn now_ns(&self) -> u64 {
        self.get_clock().map(|c| c.clock).unwrap_or(0)
    }
}

/// The host's own clock, for the ledger's timing math (see
/// proto/cella.proto, Operation.host_ns).
pub fn host_ns_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// A v7-shaped id: the version and variant bits land exactly where
/// RFC 9562 puts them, but the 48-bit timestamp field carries the
/// machine's own frame -- milliseconds of the guest's kvmclock,
/// which counts from guest boot, not the Unix epoch the RFC
/// defines. An external UUIDv7 decoder reads the date near 1970;
/// that is expected, not a bug. Hand-rolled: the bit layout is
/// fixed, not a landmine like a wrong ioctl struct, and getrandom
/// is already on the seccomp allowlist.
///
/// The machine's frame is the only clock the ledger honors (see
/// docs/NETWORK-MODEL.md, "Egress parks for decisions"): ids stay
/// time-ordered within one machine's ledger and across its own
/// thaws, because the kvmclock is monotonic through a freeze, and
/// a branched twin mints ids with identical timestamp bits by
/// design -- the engine keys on the machine name plus the
/// identifier, never the identifier alone. `guest_ns` is the same
/// reading the caller records as Operation.guest_ns, thus the id
/// and the field name one instant, not two independent clock
/// reads. Only the timestamp bits carry meaning; the random fill
/// is host-sourced, since it exists purely to avoid collisions.
pub fn uuid7(guest_ns: u64) -> [u8; 16] {
    let ms = guest_ns / 1_000_000;
    let mut id = [0u8; 16];
    id[0..6].copy_from_slice(&ms.to_be_bytes()[2..8]);
    let mut rand = [0u8; 10];
    // SAFETY: rand.as_mut_ptr() is valid for rand.len() bytes for
    // the duration of the call. getrandom's only failure mode here
    // is a short or absent fill; a zero-filled remainder still
    // leaves the id unique enough for its purpose (grouping within
    // one machine's own ledger, not a global identity).
    unsafe {
        libc::syscall(libc::SYS_getrandom, rand.as_mut_ptr(), rand.len(), 0);
    }
    id[6] = 0x70 | (rand[0] & 0x0f); // version 7
    id[7] = rand[1];
    id[8] = 0x80 | (rand[2] & 0x3f); // variant RFC 9562
    id[9..16].copy_from_slice(&rand[3..10]);
    id
}

/// The park key: the most primitive name a frame has. An IPv4
/// frame refines to its address, port, and protocol; every other
/// frame is named at the Ethernet layer -- ethertype and
/// destination MAC. Every egress frame carries one of the two
/// shapes: nothing crosses the membrane unnamed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dest {
    Ipv4 { ip: [u8; 4], port: u16, proto: u8 },
    L2 { ethertype: u16, mac: [u8; 6] },
}

impl std::fmt::Display for Dest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Dest::Ipv4 { ip, port, proto } => write!(
                f,
                "{}.{}.{}.{}:{port} proto {proto}",
                ip[0], ip[1], ip[2], ip[3]
            ),
            Dest::L2 { ethertype, mac } => {
                match ethertype {
                    0x0806 => write!(f, "arp")?,
                    0x86dd => write!(f, "ipv6")?,
                    other => write!(f, "0x{other:04x}")?,
                }
                write!(
                    f,
                    " {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                    mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
                )
            }
        }
    }
}

impl Dest {
    /// The wire Destination for this key. Every operation carries
    /// its ethertype; the MAC rides only where no IP exists.
    pub fn to_message(self) -> proto::Destination {
        match self {
            Dest::Ipv4 { ip, port, proto } => proto::Destination {
                host: String::new(),
                ip: ip.to_vec(),
                port: port as u32,
                proto: proto as u32,
                ethertype: 0x0800,
                mac: Vec::new(),
            },
            Dest::L2 { ethertype, mac } => proto::Destination {
                host: String::new(),
                ip: Vec::new(),
                port: 0,
                proto: 0,
                ethertype: ethertype as u32,
                mac: mac.to_vec(),
            },
        }
    }

    /// The key back from the wire: a non-empty ip names IPv4, and
    /// anything else names the Ethernet layer.
    pub fn from_message(d: &proto::Destination) -> Dest {
        if d.ip.is_empty() {
            let mut mac = [0u8; 6];
            let n = d.mac.len().min(6);
            mac[..n].copy_from_slice(&d.mac[..n]);
            Dest::L2 {
                ethertype: d.ethertype as u16,
                mac,
            }
        } else {
            let mut ip = [0u8; 4];
            let n = d.ip.len().min(4);
            ip[..n].copy_from_slice(&d.ip[..n]);
            Dest::Ipv4 {
                ip,
                port: d.port as u16,
                proto: d.proto as u8,
            }
        }
    }
}

/// The name of one egress frame, at the most primitive level a
/// frame has: an IPv4 frame refines to (destination ip, port,
/// proto); every other frame is named by its ethertype and
/// destination MAC. The frame starts with the 12-byte vnet header.
pub fn frame_dest_name(frame: &[u8]) -> Dest {
    frame_ipv4(frame, false).unwrap_or_else(|| frame_l2(frame, false))
}

/// The name of one inbound frame: the sender, at the most
/// primitive level -- (source ip, source port, proto) for IPv4,
/// else ethertype and source MAC.
pub fn frame_source_name(frame: &[u8]) -> Dest {
    frame_ipv4(frame, true).unwrap_or_else(|| frame_l2(frame, true))
}

fn frame_l2(frame: &[u8], source: bool) -> Dest {
    let at = if source { 18..24 } else { 12..18 };
    let mut mac = [0u8; 6];
    if let Some(b) = frame.get(at) {
        mac.copy_from_slice(b);
    }
    let ethertype = frame
        .get(24..26)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .unwrap_or(0);
    Dest::L2 { ethertype, mac }
}

fn frame_ipv4(frame: &[u8], source: bool) -> Option<Dest> {
    let eth = frame.get(12..)?;
    if eth.get(12..14)? != [0x08, 0x00] {
        return None;
    }
    let ip = eth.get(14..)?;
    let ihl = ((*ip.first()? & 0x0f) as usize) * 4;
    let proto = *ip.get(9)?;
    let addr_at = if source { 12..16 } else { 16..20 };
    let addr: [u8; 4] = ip.get(addr_at)?.try_into().ok()?;
    let port_at = if source {
        ihl..ihl + 2
    } else {
        ihl + 2..ihl + 4
    };
    let port = match proto {
        6 | 17 => u16::from_be_bytes(ip.get(port_at)?.try_into().ok()?),
        _ => 0,
    };
    Some(Dest::Ipv4 {
        ip: addr,
        port,
        proto,
    })
}

/// Wrap one Event as the Message it becomes on the wire.
pub fn event_message(event: Event) -> Message {
    Message {
        body: Some(proto::message::Body::Event(event)),
    }
}

/// An id as lowercase hex, for a report line or a gate to read.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// An operation the ledger has recorded as parked, with no matching
/// Released or Lapsed anywhere in the file: still open. Read at
/// thaw (see docs/NETWORK-MODEL.md, "Egress parks for decisions")
/// so a restored frame rejoins the id it parked under, instead of
/// a freeze minting a phantom the book never named.
#[derive(Clone)]
pub struct OpenOperation {
    pub id: Vec<u8>,
    pub dest: Dest,
    pub guest_ns: u64,
    /// Which way the crossing faces: the lanes are separate, and
    /// the never-guess matcher must never match across them.
    pub incoming: bool,
}

/// The still-open operations of one ledger: every Parked whose id
/// never appears in a Released or Lapsed, in park order. A ledger
/// that does not exist yet has no open operations -- the machine
/// has never parked anything -- which is not an error.
pub fn open_operations(path: &Path) -> std::io::Result<Vec<OpenOperation>> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let messages = read_all(path)?;
    let mut resolved: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
    let mut parked: Vec<OpenOperation> = Vec::new();
    for msg in &messages {
        let Some(proto::message::Body::Event(ev)) = &msg.body else {
            continue;
        };
        match &ev.event {
            Some(proto::event::Event::Released(r)) => {
                resolved.insert(r.id.clone());
            }
            Some(proto::event::Event::Lapsed(l)) => {
                resolved.insert(l.id.clone());
            }
            // A look resolves nothing: the operation stays open.
            Some(proto::event::Event::Inspected(_)) => {}
            Some(proto::event::Event::Parked(op)) => {
                let d = op.destination.clone().unwrap_or_default();
                parked.push(OpenOperation {
                    id: op.id.clone(),
                    dest: Dest::from_message(&d),
                    guest_ns: op.guest_ns,
                    incoming: op.direction == proto::operation::Direction::Incoming as i32,
                });
            }
            None => {}
        }
    }
    parked.retain(|op| !resolved.contains(&op.id));
    Ok(parked)
}

/// Append one framed Message to the ledger file (create it, and
/// its parent directory, on first use).
pub fn append(path: &Path, msg: &Message) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(&proto::frame(msg))
}

/// Read every framed Message from a ledger file, in order. Used by
/// `--dump-ledger` and by the tests; a real reader (cella-gateway)
/// arrives in phase 1.3.
pub fn read_all(path: &Path) -> std::io::Result<Vec<Message>> {
    let bytes = std::fs::read(path)?;
    let mut out = Vec::new();
    let mut rest = bytes.as_slice();
    while let Some((msg, used)) = proto::unframe(rest) {
        out.push(msg);
        rest = &rest[used..];
    }
    Ok(out)
}

/// The SHA-256 of `bytes`, as raw digest bytes.
pub fn sha256(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}

/// The SHA-256 of zero bytes: the genesis link of every chained
/// book (see proto/cella.proto, Audit.predecessor and
/// Event.predecessor). The first entry in a book has no
/// predecessor to name, so it names this constant instead --
/// verification recognizes it as the start of a chain, not a
/// break.
pub fn empty_digest() -> Vec<u8> {
    sha256(&[])
}

/// The SHA-256 of the last complete framed Message in `bytes`, or
/// None when `bytes` holds no complete frame (an empty or
/// brand-new book).
fn last_frame_digest(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut rest = bytes;
    let mut last: Option<&[u8]> = None;
    while let Some((_, used)) = proto::unframe(rest) {
        last = Some(&rest[..used]);
        rest = &rest[used..];
    }
    last.map(sha256)
}

/// Append one Message to a chained book (the ledger's Events, the
/// audit's Audits) under an exclusive lock: `build` is handed the
/// SHA-256 of the book's last entry's framed bytes (the empty
/// digest for a book with no entries yet), and returns the
/// Message to append, predecessor field already set.
///
/// The read of the last entry and the append share one flock on
/// the book file, held for the whole call: two processes racing to
/// write the same book (two placeless verbs at once, say) cannot
/// both read the same predecessor and fork the chain. The second
/// writer blocks on the lock, then reads the entry the first one
/// just wrote before it mints its own link. The lock releases when
/// the file handle drops at the end of this function, or if the
/// process dies first, when the kernel closes the fd -- flock
/// never survives a crash as a stale lock.
pub fn append_chained(path: &Path, build: impl FnOnce(Vec<u8>) -> Message) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)?;
    // SAFETY: f.as_raw_fd() names this open file for the duration
    // of the call below; LOCK_EX blocks until no other holder
    // remains.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    let predecessor = last_frame_digest(&bytes).unwrap_or_else(empty_digest);
    let msg = build(predecessor);
    f.write_all(&proto::frame(&msg))
    // f drops here, closing the fd and releasing the flock.
}

/// Append one Event to a book's chain (see `append_chained`).
pub fn append_event(path: &Path, mut event: Event) -> std::io::Result<()> {
    append_chained(path, |predecessor| {
        event.predecessor = predecessor;
        event_message(event)
    })
}

/// One break in a chained book: the 1-based position of the first
/// entry whose predecessor field does not match the SHA-256 of the
/// entry that came before it (position 1 names the very first
/// entry, whose predecessor must be the empty digest).
#[derive(Debug, PartialEq, Eq)]
pub struct ChainBreak {
    pub position: usize,
}

/// Walk a chained book once (see `append_chained`), verifying that
/// every entry's predecessor field is the SHA-256 of the framed
/// bytes of the entry before it -- the empty digest for the first
/// entry. `predecessor_of` reads the predecessor field out of the
/// body this book carries (Audit or Event); a frame of any other
/// body shape is not this book's chain to judge, and is skipped.
/// Returns the first break, if any. A book that has never been
/// written has nothing to verify -- absence is not a failure.
pub fn verify_chain(
    path: &Path,
    predecessor_of: impl Fn(&Message) -> Option<Vec<u8>>,
) -> std::io::Result<Option<ChainBreak>> {
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = std::fs::read(path)?;
    let mut rest = bytes.as_slice();
    let mut expect = empty_digest();
    let mut position = 0usize;
    while !rest.is_empty() {
        // A byte flipped inside an entry's own content (its verb,
        // its args) usually breaks protobuf decoding outright --
        // an invalid UTF-8 string, say -- before the predecessor
        // field is ever compared. `unframe` returning None with
        // bytes still left is exactly that: a corrupted or
        // truncated entry, not a clean end of book, and it is a
        // break at the entry it corrupted, same as a mismatched
        // predecessor is a break at the entry that carries it.
        let Some((msg, used)) = proto::unframe(rest) else {
            return Ok(Some(ChainBreak {
                position: position + 1,
            }));
        };
        let frame = &rest[..used];
        if let Some(got) = predecessor_of(&msg) {
            position += 1;
            if got != expect {
                return Ok(Some(ChainBreak { position }));
            }
            expect = sha256(frame);
        }
        rest = &rest[used..];
    }
    Ok(None)
}

/// Verify a ledger's Event chain (see `verify_chain`).
pub fn verify_ledger_chain(path: &Path) -> std::io::Result<Option<ChainBreak>> {
    verify_chain(path, |m| match &m.body {
        Some(proto::message::Body::Event(e)) => Some(e.predecessor.clone()),
        _ => None,
    })
}

/// Verify an audit book's Audit chain (see `verify_chain`).
pub fn verify_audit_chain(path: &Path) -> std::io::Result<Option<ChainBreak>> {
    verify_chain(path, |m| match &m.body {
        Some(proto::message::Body::Audit(a)) => Some(a.predecessor.clone()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The version and variant nibbles land where RFC 9562 puts
    /// them, and two ids minted apart are not equal -- the two
    /// facts a UUIDv7 promises.
    #[test]
    fn uuid7_carries_its_version_and_variant() {
        // Two mints at the same guest instant: the timestamp bits
        // legitimately match (many ids can share a millisecond),
        // and the random fill still keeps them from colliding.
        let a = uuid7(1_700_000_000_000_000_000);
        let b = uuid7(1_700_000_000_000_000_000);
        assert_eq!(a[6] & 0xf0, 0x70, "version nibble");
        assert_eq!(a[8] & 0xc0, 0x80, "variant bits");
        assert_ne!(a, b, "two mints do not collide, same instant or not");
    }

    /// The timestamp bits encode the guest instant passed in, not
    /// wall-clock time: the id is minted in the guest's frame (see
    /// docs/NETWORK-MODEL.md).
    #[test]
    fn uuid7_encodes_the_guest_instant_it_is_given() {
        let guest_ns = 1_700_000_000_123_000_000u64; // an arbitrary ms-aligned instant
        let id = uuid7(guest_ns);
        let mut ms_bytes = [0u8; 8];
        ms_bytes[2..8].copy_from_slice(&id[0..6]);
        let encoded_ms = u64::from_be_bytes(ms_bytes);
        assert_eq!(encoded_ms, guest_ns / 1_000_000);
    }

    /// Append, then read back: the chronicle round-trips, and a
    /// parent directory that does not exist yet gets created.
    #[test]
    fn the_ledger_round_trips_through_a_fresh_directory() {
        let dir = std::env::temp_dir().join(format!("cella-ledger-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("network").join("ledger");

        let op = crate::proto::Operation {
            id: uuid7(111).to_vec(),
            destination: Some(crate::proto::Destination {
                host: String::new(),
                ip: vec![192, 168, 200, 1],
                port: 8080,
                proto: 6,
                ethertype: 0x0800,
                mac: Vec::new(),
            }),
            guest_ns: 111,
            host_ns: 222,
            direction: 0,
        };
        let msg = event_message(Event {
            predecessor: Vec::new(),
            event: Some(proto::event::Event::Parked(op.clone())),
        });
        append(&path, &msg).unwrap();

        let back = read_all(&path).unwrap();
        assert_eq!(back.len(), 1);
        match &back[0].body {
            Some(proto::message::Body::Event(e)) => match &e.event {
                Some(proto::event::Event::Parked(got)) => assert_eq!(*got, op),
                other => panic!("expected Parked, got {other:?}"),
            },
            other => panic!("expected Event, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn parked_event(id: &[u8], ip: [u8; 4], port: u32, proto: u32, guest_ns: u64) -> Message {
        event_message(Event {
            predecessor: Vec::new(),
            event: Some(super::proto::event::Event::Parked(
                crate::proto::Operation {
                    id: id.to_vec(),
                    destination: Some(crate::proto::Destination {
                        host: String::new(),
                        ip: ip.to_vec(),
                        port,
                        proto,
                        ethertype: 0x0800,
                        mac: Vec::new(),
                    }),
                    guest_ns,
                    host_ns: 0,
                    direction: 0,
                },
            )),
        })
    }

    fn released_event(id: &[u8]) -> Message {
        event_message(Event {
            predecessor: Vec::new(),
            event: Some(super::proto::event::Event::Released(
                crate::proto::Released {
                    id: id.to_vec(),
                    first_response_ns: 0,
                    bytes_in: 0,
                    bytes_out: 0,
                },
            )),
        })
    }

    /// A ledger with no file yet has no open operations -- absence
    /// is not an error at the first-ever thaw of a machine that
    /// never parked anything.
    #[test]
    fn no_ledger_file_is_no_open_operations() {
        let path =
            std::env::temp_dir().join(format!("cella-ledger-missing-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert!(open_operations(&path).unwrap().is_empty());
    }

    /// A Parked with no matching Released or Lapsed is open; one
    /// that is resolved drops out, in park order.
    #[test]
    fn open_operations_excludes_the_resolved() {
        let dir = std::env::temp_dir().join(format!("cella-ledger-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("ledger");
        let id_a = [1u8; 16];
        let id_b = [2u8; 16];
        append(&path, &parked_event(&id_a, [10, 0, 0, 1], 80, 6, 100)).unwrap();
        append(&path, &parked_event(&id_b, [10, 0, 0, 2], 443, 6, 200)).unwrap();
        append(&path, &released_event(&id_a)).unwrap();

        let open = open_operations(&path).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, id_b.to_vec());
        assert_eq!(
            open[0].dest,
            Dest::Ipv4 {
                ip: [10, 0, 0, 2],
                port: 443,
                proto: 6
            }
        );
        assert_eq!(open[0].guest_ns, 200);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 1.6.14d: field 15 fills the ledger's Event chain -- each
    /// entry's predecessor is the SHA-256 of the framed bytes of
    /// the entry before it, and an intact book of several entries
    /// verifies end to end.
    #[test]
    fn an_intact_chain_verifies_end_to_end() {
        let path = std::env::temp_dir().join(format!("cella-chain-ok-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let id_a = [1u8; 16];
        let id_b = [2u8; 16];
        append_event(
            &path,
            Event {
                predecessor: Vec::new(),
                event: Some(proto::event::Event::Parked(crate::proto::Operation {
                    id: id_a.to_vec(),
                    ..Default::default()
                })),
            },
        )
        .unwrap();
        append_event(
            &path,
            Event {
                predecessor: Vec::new(),
                event: Some(proto::event::Event::Parked(crate::proto::Operation {
                    id: id_b.to_vec(),
                    ..Default::default()
                })),
            },
        )
        .unwrap();
        append_event(
            &path,
            Event {
                predecessor: Vec::new(),
                event: Some(proto::event::Event::Lapsed(proto::Lapsed {
                    id: id_a.to_vec(),
                    why: "gave up".into(),
                })),
            },
        )
        .unwrap();
        assert_eq!(verify_ledger_chain(&path).unwrap(), None);
        let _ = std::fs::remove_file(&path);
    }

    /// The genesis link: the first entry a book ever gets carries
    /// the empty digest as its predecessor -- there is nothing
    /// before it to name.
    #[test]
    fn the_first_entry_chains_from_the_empty_digest() {
        let path = std::env::temp_dir().join(format!("cella-chain-genesis-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        append_event(
            &path,
            Event {
                predecessor: Vec::new(),
                event: Some(proto::event::Event::Parked(crate::proto::Operation {
                    id: vec![9u8; 16],
                    ..Default::default()
                })),
            },
        )
        .unwrap();
        let messages = read_all(&path).unwrap();
        let Some(proto::message::Body::Event(e)) = &messages[0].body else {
            panic!("expected an Event");
        };
        assert_eq!(e.predecessor, empty_digest());
        let _ = std::fs::remove_file(&path);
    }

    /// A tampered entry breaks the chain loudly, naming the entry:
    /// a byte flipped inside an entry's own content (here, its id)
    /// invalidates that entry's protobuf decoding outright -- a
    /// break at the entry itself, not a silent pass.
    #[test]
    fn a_tampered_entry_fails_loudly_naming_the_break() {
        let path = std::env::temp_dir().join(format!("cella-chain-tamper-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        append_event(
            &path,
            Event {
                predecessor: Vec::new(),
                event: Some(proto::event::Event::Parked(crate::proto::Operation {
                    id: vec![9u8; 16],
                    ..Default::default()
                })),
            },
        )
        .unwrap();
        append_event(
            &path,
            Event {
                predecessor: Vec::new(),
                event: Some(proto::event::Event::Lapsed(proto::Lapsed {
                    id: vec![9u8; 16],
                    why: "gave up".into(),
                })),
            },
        )
        .unwrap();
        assert_eq!(verify_ledger_chain(&path).unwrap(), None, "intact first");

        // Flip the last byte of the first frame: `predecessor`
        // (field 15) serializes last on the wire, so this lands
        // inside the first entry's own predecessor digest -- a
        // bytes field, so the frame still decodes; only its
        // content, and thus the genesis check, changes. The break
        // surfaces immediately, at the entry it corrupted.
        let mut bytes = std::fs::read(&path).unwrap();
        let a = &parked_event_bytes(&[9u8; 16]);
        assert_eq!(&bytes[..a.len()], a.as_slice(), "layout assumption");
        bytes[a.len() - 1] ^= 0xff;
        std::fs::write(&path, &bytes).unwrap();

        let brk = verify_ledger_chain(&path)
            .unwrap()
            .expect("the tampered book fails to verify");
        assert_eq!(brk.position, 1);
        let _ = std::fs::remove_file(&path);
    }

    fn parked_event_bytes(id: &[u8; 16]) -> Vec<u8> {
        proto::frame(&event_message(Event {
            predecessor: empty_digest(),
            event: Some(proto::event::Event::Parked(crate::proto::Operation {
                id: id.to_vec(),
                ..Default::default()
            })),
        }))
    }
}
