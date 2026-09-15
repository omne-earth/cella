//! The resolver that is the interceptor (docs/NETWORK-MODEL.md,
//! "The terminator"; ruling 2.7 (j)).
//!
//! Server side: every A query from the wire is answered with the
//! terminator's own wire address -- interception is an answer, not
//! a rule. AAAA answers empty (ipv6.disable=1 is the canonical
//! posture). Client side: the proxy resolves the *real* address at
//! connect time through the upstream provider, and the answers
//! cache here with TTL honesty -- an expired entry is re-asked,
//! never trusted (2.7 (i)(3)).
//!
//! The DNS wire codec is hand-rolled and minimal: queries and A
//! answers, nothing else. A packet this code cannot read is
//! dropped, never guessed at.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

/// One parsed question: the name (lowercase, dot-joined), qtype,
/// and the bytes needed to echo the question section back.
#[derive(Debug, PartialEq)]
pub struct Question {
    pub name: String,
    pub qtype: u16,
}

pub const QTYPE_A: u16 = 1;
pub const QTYPE_AAAA: u16 = 28;

/// Parse a query packet: id + first question. None for anything
/// this codec does not read (truncated, compressed names in the
/// question, zero questions) -- dropped upstream, never guessed.
pub fn parse_query(pkt: &[u8]) -> Option<(u16, Question)> {
    if pkt.len() < 12 {
        return None;
    }
    let id = u16::from_be_bytes([pkt[0], pkt[1]]);
    let qr = pkt[2] & 0x80;
    if qr != 0 {
        return None; // a response is not a question
    }
    let qdcount = u16::from_be_bytes([pkt[4], pkt[5]]);
    if qdcount == 0 {
        return None;
    }
    let (name, off) = parse_name(pkt, 12)?;
    if off + 4 > pkt.len() {
        return None;
    }
    let qtype = u16::from_be_bytes([pkt[off], pkt[off + 1]]);
    Some((id, Question { name, qtype }))
}

/// A label-sequence name at `off`, no compression pointers (a
/// question section never needs them; a packet using them here is
/// outside this codec).
fn parse_name(pkt: &[u8], mut off: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    loop {
        let len = *pkt.get(off)? as usize;
        if len == 0 {
            off += 1;
            break;
        }
        if len & 0xc0 != 0 {
            return None; // compression: not this codec's business
        }
        let end = off + 1 + len;
        let label = pkt.get(off + 1..end)?;
        if !label
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
        {
            return None;
        }
        labels.push(String::from_utf8_lossy(label).to_lowercase());
        off = end;
        if labels.len() > 32 {
            return None;
        }
    }
    if labels.is_empty() {
        return None;
    }
    Some((labels.join("."), off))
}

fn encode_name(name: &str, out: &mut Vec<u8>) {
    for label in name.split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
}

/// The interceptor's answer: the question echoed, one A record
/// pointing at `self_ip`, a short TTL (the member should keep
/// asking us -- we are not the truth, we are the door). For AAAA
/// (or anything else) the answer section is empty: a clean
/// no-data, and the member falls back to A.
pub fn answer_with_self(id: u16, q: &Question, self_ip: Ipv4Addr) -> Vec<u8> {
    let answers = if q.qtype == QTYPE_A { 1u16 } else { 0u16 };
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&[0x81, 0x80]); // response, RD+RA, NOERROR
    out.extend_from_slice(&1u16.to_be_bytes()); // qdcount
    out.extend_from_slice(&answers.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // ns
    out.extend_from_slice(&0u16.to_be_bytes()); // ar
    encode_name(&q.name, &mut out);
    out.extend_from_slice(&q.qtype.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // class IN
    if answers == 1 {
        encode_name(&q.name, &mut out);
        out.extend_from_slice(&QTYPE_A.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&30u32.to_be_bytes()); // ttl: keep asking us
        out.extend_from_slice(&4u16.to_be_bytes());
        out.extend_from_slice(&self_ip.octets());
    }
    out
}

/// Build an upstream A query for the real resolution.
pub fn build_query(id: u16, name: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&[0x01, 0x00]); // RD
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    encode_name(name, &mut out);
    out.extend_from_slice(&QTYPE_A.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out
}

/// Parse an upstream response: the first A record's address and
/// TTL, for the asked id. Answer names may use compression; this
/// parser skips names by structure without following pointers,
/// reading only what it must.
pub fn parse_answer(pkt: &[u8], want_id: u16) -> Option<(Ipv4Addr, u32)> {
    if pkt.len() < 12 {
        return None;
    }
    if u16::from_be_bytes([pkt[0], pkt[1]]) != want_id {
        return None;
    }
    if pkt[2] & 0x80 == 0 {
        return None; // not a response
    }
    if pkt[3] & 0x0f != 0 {
        return None; // rcode: the upstream said no
    }
    let qdcount = u16::from_be_bytes([pkt[4], pkt[5]]) as usize;
    let ancount = u16::from_be_bytes([pkt[6], pkt[7]]) as usize;
    let mut off = 12;
    for _ in 0..qdcount {
        off = skip_name(pkt, off)?;
        off += 4;
    }
    for _ in 0..ancount {
        off = skip_name(pkt, off)?;
        if off + 10 > pkt.len() {
            return None;
        }
        let rtype = u16::from_be_bytes([pkt[off], pkt[off + 1]]);
        let ttl = u32::from_be_bytes([pkt[off + 4], pkt[off + 5], pkt[off + 6], pkt[off + 7]]);
        let rdlen = u16::from_be_bytes([pkt[off + 8], pkt[off + 9]]) as usize;
        off += 10;
        if off + rdlen > pkt.len() {
            return None;
        }
        if rtype == QTYPE_A && rdlen == 4 {
            return Some((
                Ipv4Addr::new(pkt[off], pkt[off + 1], pkt[off + 2], pkt[off + 3]),
                ttl,
            ));
        }
        off += rdlen;
    }
    None
}

fn skip_name(pkt: &[u8], mut off: usize) -> Option<usize> {
    loop {
        let len = *pkt.get(off)? as usize;
        if len == 0 {
            return Some(off + 1);
        }
        if len & 0xc0 == 0xc0 {
            return Some(off + 2); // a pointer ends the name
        }
        off += 1 + len;
    }
}

/// The cache: TTL honesty (2.7 (i)(3)) -- an expired entry is
/// re-asked, never served. TTLs are clamped to a ceiling so a
/// hostile upstream cannot pin a name for a week.
pub struct Cache {
    entries: HashMap<String, (Ipv4Addr, Instant)>,
    ceiling: Duration,
}

impl Cache {
    pub fn new(ceiling: Duration) -> Self {
        Cache {
            entries: HashMap::new(),
            ceiling,
        }
    }
    pub fn get(&self, name: &str, now: Instant) -> Option<Ipv4Addr> {
        let (ip, expires) = self.entries.get(name)?;
        if now >= *expires {
            return None;
        }
        Some(*ip)
    }
    pub fn put(&mut self, name: &str, ip: Ipv4Addr, ttl: u32, now: Instant) {
        let ttl = Duration::from_secs(ttl as u64).min(self.ceiling);
        self.entries.insert(name.to_string(), (ip, now + ttl));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_round_trips_and_the_answer_points_home() {
        let q = build_query(0x1234, "api.example.com");
        let (id, parsed) = parse_query(&q).unwrap();
        assert_eq!(id, 0x1234);
        assert_eq!(parsed.name, "api.example.com");
        assert_eq!(parsed.qtype, QTYPE_A);
        let home = Ipv4Addr::new(10, 77, 1, 1);
        let ans = answer_with_self(id, &parsed, home);
        // The member reads our answer as an upstream A record.
        let (ip, ttl) = parse_answer(&ans, 0x1234).unwrap();
        assert_eq!(ip, home);
        assert_eq!(ttl, 30);
    }

    #[test]
    fn aaaa_gets_a_clean_no_data() {
        let mut q = build_query(7, "v6.example.com");
        // Rewrite qtype to AAAA.
        let n = q.len();
        q[n - 4..n - 2].copy_from_slice(&QTYPE_AAAA.to_be_bytes());
        let (id, parsed) = parse_query(&q).unwrap();
        assert_eq!(parsed.qtype, QTYPE_AAAA);
        let ans = answer_with_self(id, &parsed, Ipv4Addr::LOCALHOST);
        assert!(parse_answer(&ans, 7).is_none()); // no A record
        assert_eq!(u16::from_be_bytes([ans[6], ans[7]]), 0); // ancount 0
    }

    #[test]
    fn unreadable_packets_are_dropped_not_guessed() {
        assert!(parse_query(&[]).is_none());
        assert!(parse_query(&[0; 11]).is_none());
        let mut resp = build_query(1, "x.example");
        resp[2] |= 0x80; // a response is not a question
        assert!(parse_query(&resp).is_none());
        // A compressed question name is outside the codec.
        let mut q = build_query(1, "a.b");
        q[12] = 0xc0;
        assert!(parse_query(&q).is_none());
    }

    #[test]
    fn the_cache_honors_ttl_and_the_ceiling() {
        let mut c = Cache::new(Duration::from_secs(300));
        let t0 = Instant::now();
        let ip = Ipv4Addr::new(93, 184, 216, 34);
        c.put("example.com", ip, 60, t0);
        assert_eq!(c.get("example.com", t0), Some(ip));
        assert_eq!(c.get("example.com", t0 + Duration::from_secs(59)), Some(ip));
        // Expired is re-asked, never served.
        assert_eq!(c.get("example.com", t0 + Duration::from_secs(61)), None);
        // A hostile week-long TTL clamps to the ceiling.
        c.put("pin.example.com", ip, 604800, t0);
        assert_eq!(
            c.get("pin.example.com", t0 + Duration::from_secs(301)),
            None
        );
    }
}
