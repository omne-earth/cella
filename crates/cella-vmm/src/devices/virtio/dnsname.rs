//! The name ratchet's parser: what did a delivered DNS answer say?
//!
//! The membrane never asks the guest anything -- but a resolution
//! crosses it: the answer arrives as judged ingress and the
//! membrane delivers it. This parser reads exactly that traffic so
//! the park can stamp Destination.host (the proto's promise: "the
//! host name is present when the appliance resolved it"). The
//! stamp is testimony, not truth -- it records what an adjudicated
//! answer claimed at this membrane, and a hostile resolver can
//! claim anything; the judge weighs it as testimony.
//!
//! The bytes are attacker-influenced and this code runs inside the
//! membrane process, so the parser is the strictest of the house:
//! fixed shapes only, every length bounds-checked, and anything
//! unusual -- a compressed question, a strange label, a truncated
//! record -- returns None. Drop, never guess.

/// Extract (queried name, first answered IPv4) from an ingress
/// frame -- the house shape: a 12-byte vnet header, then Ethernet
/// -- if and only if it is a well-formed DNS answer over UDP. The
/// port is convention, not identity (an upstream may serve
/// anywhere), so the strict shape is the whole filter: a released
/// UDP payload either is a clean single-question A answer or it
/// teaches nothing.
pub fn answer_in_frame(frame: &[u8]) -> Option<(String, [u8; 4])> {
    let eth = frame.get(12..)?;
    if eth.len() < 14 {
        return None;
    }
    if u16::from_be_bytes([eth[12], eth[13]]) != 0x0800 {
        return None;
    }
    let ip = &eth[14..];
    if ip.len() < 20 || ip[0] >> 4 != 4 {
        return None;
    }
    let ihl = ((ip[0] & 0x0f) as usize) * 4;
    if ihl < 20 || ip.len() < ihl + 8 || ip[9] != 17 {
        return None;
    }
    let udp = &ip[ihl..];
    let udp_len = u16::from_be_bytes([udp[4], udp[5]]) as usize;
    if udp_len < 8 || udp.len() < udp_len {
        return None;
    }
    answer(&udp[8..udp_len])
}

/// The DNS payload: one question (uncompressed, type A, class IN),
/// at least one answer, and the first A/IN record wins.
fn answer(pkt: &[u8]) -> Option<(String, [u8; 4])> {
    if pkt.len() < 12 || pkt[2] & 0x80 == 0 {
        return None; // not a response
    }
    if u16::from_be_bytes([pkt[4], pkt[5]]) != 1 {
        return None; // exactly one question, or nothing to name
    }
    let ancount = u16::from_be_bytes([pkt[6], pkt[7]]);
    if ancount == 0 {
        return None;
    }
    // The question name: plain labels, no compression, hostname
    // characters only. The ratchet holds names a policy could
    // hold; anything stranger stays unnamed.
    let mut i = 12usize;
    let mut name = String::new();
    loop {
        let len = *pkt.get(i)? as usize;
        if len == 0 {
            i += 1;
            break;
        }
        if len > 63 {
            return None;
        }
        let label = pkt.get(i + 1..i + 1 + len)?;
        if !label
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
        {
            return None;
        }
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(std::str::from_utf8(label).ok()?);
        if name.len() > 253 {
            return None;
        }
        i += 1 + len;
    }
    if name.is_empty() {
        return None;
    }
    if u16::from_be_bytes([*pkt.get(i)?, *pkt.get(i + 1)?]) != 1
        || u16::from_be_bytes([*pkt.get(i + 2)?, *pkt.get(i + 3)?]) != 1
    {
        return None; // question is not A/IN
    }
    i += 4;
    let name = name.to_ascii_lowercase();
    for _ in 0..ancount {
        // The answer's owner name: a pointer (two bytes, not
        // followed -- the question already named the flow) or
        // plain labels, skipped with the same bounds.
        loop {
            let b = *pkt.get(i)?;
            if b & 0xc0 == 0xc0 {
                i = i.checked_add(2)?;
                break;
            }
            if b == 0 {
                i += 1;
                break;
            }
            if b > 63 {
                return None;
            }
            i = i.checked_add(1 + b as usize)?;
            pkt.get(i)?;
        }
        let atype = u16::from_be_bytes([*pkt.get(i)?, *pkt.get(i + 1)?]);
        let aclass = u16::from_be_bytes([*pkt.get(i + 2)?, *pkt.get(i + 3)?]);
        let rdlen = u16::from_be_bytes([*pkt.get(i + 8)?, *pkt.get(i + 9)?]) as usize;
        i += 10;
        let rd = pkt.get(i..i.checked_add(rdlen)?)?;
        if atype == 1 && aclass == 1 && rdlen == 4 {
            return Some((name, [rd[0], rd[1], rd[2], rd[3]]));
        }
        i += rdlen;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dns_answer(qname: &[&str], ip: [u8; 4]) -> Vec<u8> {
        let mut p = vec![0x12, 0x34, 0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0];
        for l in qname {
            p.push(l.len() as u8);
            p.extend_from_slice(l.as_bytes());
        }
        p.push(0);
        p.extend_from_slice(&[0, 1, 0, 1]); // A IN
        p.extend_from_slice(&[0xc0, 0x0c]); // owner: pointer to question
        p.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 60, 0, 4]); // A IN ttl rdlen
        p.extend_from_slice(&ip);
        p
    }

    fn frame(udp_payload: &[u8], src_port: u16) -> Vec<u8> {
        let udp_len = 8 + udp_payload.len();
        let mut f = vec![0u8; 26]; // vnet header + Ethernet
        f[24] = 0x08; // IPv4
        let mut ip = vec![
            0x45, 0, 0, 0, 0, 0, 0, 0, 64, 17, 0, 0, 9, 9, 9, 9, 10, 0, 0, 2,
        ];
        let total = (20 + udp_len) as u16;
        ip[2..4].copy_from_slice(&total.to_be_bytes());
        f.extend_from_slice(&ip);
        f.extend_from_slice(&src_port.to_be_bytes());
        f.extend_from_slice(&[0, 53]);
        f.extend_from_slice(&(udp_len as u16).to_be_bytes());
        f.extend_from_slice(&[0, 0]);
        f.extend_from_slice(udp_payload);
        f
    }

    #[test]
    fn a_plain_answer_names_its_ip() {
        let f = frame(
            &dns_answer(&["api", "example", "com"], [93, 184, 216, 34]),
            53,
        );
        let (name, ip) = answer_in_frame(&f).unwrap();
        assert_eq!(name, "api.example.com");
        assert_eq!(ip, [93, 184, 216, 34]);
    }

    #[test]
    fn the_parser_drops_and_never_guesses() {
        // Any source port serves: the shape is the filter.
        let f = frame(&dns_answer(&["a", "b"], [1, 1, 1, 1]), 5353);
        assert!(answer_in_frame(&f).is_some());
        // A query, not a response.
        let mut q = dns_answer(&["a", "b"], [1, 1, 1, 1]);
        q[2] = 0;
        assert!(answer_in_frame(&frame(&q, 53)).is_none());
        // A compressed question is unusual: dropped, not followed.
        let mut c = vec![0x12, 0x34, 0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0];
        c.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1]);
        assert!(answer_in_frame(&frame(&c, 53)).is_none());
        // A label with bytes no policy could hold.
        assert!(answer_in_frame(&frame(&dns_answer(&["a b"], [1, 1, 1, 1]), 53)).is_none());
        // Truncations at every depth die by bounds, not by panic.
        let whole = frame(&dns_answer(&["api", "example", "com"], [1, 2, 3, 4]), 53);
        for cut in 0..whole.len() {
            let _ = answer_in_frame(&whole[..cut]);
        }
    }

    #[test]
    fn a_cname_first_answer_still_finds_the_a() {
        let mut p = vec![0x12, 0x34, 0x81, 0x80, 0, 1, 0, 2, 0, 0, 0, 0];
        p.extend_from_slice(&[1, b'w', 0, 0, 1, 0, 1]); // question w A IN
                                                        // First answer: CNAME, skipped by rdlen.
        p.extend_from_slice(&[0xc0, 0x0c, 0, 5, 0, 1, 0, 0, 0, 60, 0, 3, 1, b'x', 0]);
        // Second: the A.
        p.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 7, 7, 7, 7]);
        let (name, ip) = answer_in_frame(&frame(&p, 53)).unwrap();
        assert_eq!(name, "w");
        assert_eq!(ip, [7, 7, 7, 7]);
    }
}
