//! The membrane's memory (N.F.7): one codec owns the file.
//!
//! The membrane-memory file beside a machine frames MembraneMemory
//! messages directly, valve-style -- no Message envelope, one entry
//! per frame, the whole file rewritten never (append-only; the
//! newest entry for a destination wins). The membrane reads it on
//! the kick and at boot; the presider (cella-membrane) and the
//! bridge are the writers. Expiry is absolute and stateless: an
//! entry stands while now < written + keep_open, computable by any
//! reader at any time -- nothing re-anchors at a thaw, and an
//! expired memory stays expired. Every zero decodes to the
//! cryogenic default: a zero written or keep_open is inert
//! (docs/NETWORK-MODEL.md, "The membrane's memory").

use crate::proto;
use prost::Message as _;
use std::path::Path;

/// One standing entry, already validated against the clock.
#[derive(Debug, Clone, PartialEq)]
pub struct Standing {
    pub dest: proto::Destination,
    pub skip_freeze: bool,
    /// Epoch seconds: written + keep_open.
    pub expires: u64,
}

/// Decode every entry in the file and keep the ones standing at
/// `now_s` (epoch seconds). Inert entries (zero written or zero
/// keep_open) and expired entries drop; the newest frame for a
/// destination wins, thus a later landing supersedes an earlier
/// one. An absent or unreadable file is an empty memory: the park
/// is the freeze.
pub fn read_standing(path: &Path, now_s: u64) -> Vec<Standing> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let mut out: Vec<Standing> = Vec::new();
    let mut buf = bytes.as_slice();
    while !buf.is_empty() {
        let before = buf.len();
        let Ok(m) = proto::MembraneMemory::decode_length_delimited(&mut buf) else {
            break;
        };
        if buf.len() == before {
            break;
        }
        let Some(dest) = m.destination else { continue };
        // Fail-closed arithmetic: zeros are inert, and an expiry
        // that cannot be computed does not stand.
        if m.written == 0 || m.keep_open == 0 {
            continue;
        }
        let Some(expires) = m.written.checked_add(m.keep_open) else {
            continue;
        };
        if now_s >= expires {
            continue;
        }
        // The newest frame for a destination wins.
        out.retain(|s| !same_dest(&s.dest, &dest));
        out.push(Standing {
            dest,
            skip_freeze: m.skip_freeze,
            expires,
        });
    }
    out
}

/// Append one entry. The writer stamps `written` before calling;
/// this codec only frames.
pub fn append(path: &Path, m: &proto::MembraneMemory) -> Result<(), String> {
    let mut buf = Vec::with_capacity(m.encoded_len() + 4);
    m.encode_length_delimited(&mut buf)
        .map_err(|e| e.to_string())?;
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("appending {path:?}: {e}"))?;
    f.write_all(&buf).map_err(|e| e.to_string())
}

/// Does a standing memory with skip_freeze name this destination at
/// `now_s`? Exact match, the two shapes every park is named by: an
/// IPv4 crossing matches on (ip, port, proto); everything else on
/// the ethertype alone (the grammar holds no MACs -- a policy that
/// named one would break on every rebuild). A matcher that never
/// guesses: no wildcard exists at this layer.
pub fn skips_freeze(standing: &[Standing], dest: &proto::Destination, now_s: u64) -> bool {
    standing
        .iter()
        .any(|s| s.skip_freeze && now_s < s.expires && same_dest(&s.dest, dest))
}

fn same_dest(a: &proto::Destination, b: &proto::Destination) -> bool {
    if !a.ip.is_empty() || !b.ip.is_empty() {
        return a.ip == b.ip && a.port == b.port && a.proto == b.proto;
    }
    a.ethertype == b.ethertype
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ip: &[u8], port: u32, written: u64, keep_open: u64) -> proto::MembraneMemory {
        proto::MembraneMemory {
            destination: Some(proto::Destination {
                host: String::new(),
                ip: ip.to_vec(),
                port,
                proto: 6,
                ethertype: 0x0800,
                mac: Vec::new(),
            }),
            skip_freeze: true,
            keep_open,
            written,
        }
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cella-memory-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_file(&dir);
        dir
    }

    #[test]
    fn standing_expiring_and_inert() {
        let p = tmp("expiry");
        append(&p, &entry(&[1, 1, 1, 1], 443, 1000, 300)).unwrap();
        append(&p, &entry(&[2, 2, 2, 2], 80, 0, 300)).unwrap(); // inert: no stamp
        append(&p, &entry(&[3, 3, 3, 3], 80, 1000, 0)).unwrap(); // inert: no window
        let now_alive = 1100;
        let now_dead = 1300;
        let alive = read_standing(&p, now_alive);
        assert_eq!(alive.len(), 1);
        assert_eq!(alive[0].expires, 1300);
        assert!(skips_freeze(
            &alive,
            &proto::Destination {
                host: String::new(),
                ip: vec![1, 1, 1, 1],
                port: 443,
                proto: 6,
                ethertype: 0x0800,
                mac: Vec::new(),
            },
            now_alive
        ));
        // The expired memory stays expired at any later read: a thaw
        // re-anchors nothing.
        assert!(read_standing(&p, now_dead).is_empty());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn matching_is_exact_and_the_newest_wins() {
        let p = tmp("match");
        append(&p, &entry(&[1, 1, 1, 1], 443, 1000, 300)).unwrap();
        let mut superseding = entry(&[1, 1, 1, 1], 443, 1100, 300);
        superseding.skip_freeze = false;
        append(&p, &superseding).unwrap();
        let standing = read_standing(&p, 1150);
        assert_eq!(standing.len(), 1);
        assert!(!standing[0].skip_freeze); // the newest frame won
                                           // Exact means exact: a different port is a different world.
        let other = proto::Destination {
            host: String::new(),
            ip: vec![1, 1, 1, 1],
            port: 80,
            proto: 6,
            ethertype: 0x0800,
            mac: Vec::new(),
        };
        assert!(!skips_freeze(&standing, &other, 1150));
        // An L2 entry matches on the ethertype alone.
        let arp = proto::MembraneMemory {
            destination: Some(proto::Destination {
                host: String::new(),
                ip: Vec::new(),
                port: 0,
                proto: 0,
                ethertype: 0x0806,
                mac: Vec::new(),
            }),
            skip_freeze: true,
            keep_open: 300,
            written: 1000,
        };
        append(&p, &arp).unwrap();
        let standing = read_standing(&p, 1150);
        let arp_park = proto::Destination {
            host: String::new(),
            ip: Vec::new(),
            port: 0,
            proto: 0,
            ethertype: 0x0806,
            mac: vec![0xff; 6],
        };
        assert!(skips_freeze(&standing, &arp_park, 1150));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn an_absent_file_is_an_empty_memory() {
        assert!(read_standing(Path::new("/nonexistent/cella-memory"), 1).is_empty());
    }
}
