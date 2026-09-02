//! The ledger: the chronicle of held operations, and the wire
//! primitives that write it. See docs/NETWORK-MODEL.md, "The
//! control plane" -- the ledger is a chronicle, never the store:
//! the held bytes live in the device's own memory (today, the
//! parked-frame lists of the virtio-net backend; in the appliance
//! of phase 2, its own guest RAM).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use kvm_ioctls::VmFd;

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

/// Wrap one Event as the Message it becomes on the wire.
pub fn event_message(event: Event) -> Message {
    Message {
        body: Some(proto::message::Body::Event(event)),
    }
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
            }),
            guest_ns: 111,
            host_ns: 222,
        };
        let msg = event_message(Event {
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
}
