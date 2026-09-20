//! The names file (the ratchet made durable): one codec owns it.
//!
//! `machines/<vm>/network/names` holds the membrane's name ratchet
//! -- ip -> the host a delivered DNS answer bound to it -- as
//! append-only framed Destination messages carrying host and ip
//! alone. The membrane writes at the moment of learning and folds
//! the file at construction, newest frame per ip winning. The
//! point is the freeze: a park is the freeze, and the freeze that
//! a park itself causes must not erase the name that would have
//! stamped the next park of the same flow -- a thaw wakes knowing
//! every name this membrane ever witnessed. The stamp stays
//! testimony (proto/cella.proto, Destination.host): the file
//! records what answers claimed, and a newer claim corrects an
//! older one.

use crate::proto;
use prost::Message as _;
use std::collections::HashMap;
use std::path::Path;

/// Every frame in append order -- the raw chronicle, for the dump.
/// An absent or unreadable file is empty; a torn final frame drops
/// and everything before it stands.
pub fn read_all(path: &Path) -> Vec<proto::Destination> {
    let mut out = Vec::new();
    let Ok(bytes) = std::fs::read(path) else {
        return out;
    };
    let mut buf = bytes.as_slice();
    while !buf.is_empty() {
        let before = buf.len();
        let Ok(d) = proto::Destination::decode_length_delimited(&mut buf) else {
            break;
        };
        if buf.len() == before {
            break;
        }
        out.push(d);
    }
    out
}

/// Fold the file, newest frame per ip winning.
pub fn read_names(path: &Path) -> HashMap<[u8; 4], String> {
    let mut out = HashMap::new();
    for d in read_all(path) {
        if d.host.is_empty() || d.ip.len() != 4 {
            continue;
        }
        out.insert([d.ip[0], d.ip[1], d.ip[2], d.ip[3]], d.host);
    }
    out
}

/// Append one learned binding. A write failure loses durability,
/// never the runtime ratchet -- the membrane keeps serving.
pub fn append_name(path: &Path, host: &str, ip: [u8; 4]) {
    let d = proto::Destination {
        host: host.to_string(),
        ip: ip.to_vec(),
        port: 0,
        proto: 0,
        ethertype: 0,
        mac: Vec::new(),
    };
    let mut buf = Vec::with_capacity(d.encoded_len() + 4);
    if d.encode_length_delimited(&mut buf).is_err() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(&buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_names_file_survives_and_the_newest_wins() {
        let dir = std::env::temp_dir().join(format!("cella-names-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join("network").join("names");
        assert!(read_names(&p).is_empty());
        append_name(&p, "api.example.com", [1, 2, 3, 4]);
        append_name(&p, "w.test", [5, 6, 7, 8]);
        append_name(&p, "api2.example.com", [1, 2, 3, 4]); // supersedes
        let fold = read_names(&p);
        assert_eq!(fold.len(), 2);
        assert_eq!(fold[&[1, 2, 3, 4]].as_str(), "api2.example.com");
        assert_eq!(fold[&[5, 6, 7, 8]].as_str(), "w.test");
        // A torn final frame folds what stands and drops the tear.
        let bytes = std::fs::read(&p).unwrap();
        std::fs::write(&p, &bytes[..bytes.len() - 3]).unwrap();
        let fold = read_names(&p);
        assert_eq!(fold[&[5, 6, 7, 8]].as_str(), "w.test");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
