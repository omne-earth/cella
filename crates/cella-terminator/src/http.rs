//! The plain-HTTP name: read the request head, find the Host --
//! the one place a non-TLS flow names its destination. The read
//! bytes are replayed verbatim onto the world leg: the proxy
//! splits the connection, never edits the payload.

use std::io::Read;

/// Read until the end of the request head (CRLFCRLF) or the cap,
/// and return (head_bytes, host). No Host inside the cap is a
/// nameless flow: refused upstream, never guessed.
pub fn read_head_and_host<R: Read>(r: &mut R, cap: usize) -> Result<(Vec<u8>, String), String> {
    let mut head = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    while head.len() < cap {
        match r.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => head.push(byte[0]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(e) => return Err(e.to_string()),
        }
        if head.ends_with(b"\r\n\r\n") {
            let host = find_host(&head).ok_or("no Host header in the request head")?;
            return Ok((head, host));
        }
    }
    Err("no request head within the cap -- a nameless flow".to_string())
}

fn find_host(head: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(head).ok()?;
    for line in text.split("\r\n").skip(1) {
        let (k, v) = line.split_once(':')?;
        if k.eq_ignore_ascii_case("host") {
            // A port suffix belongs to the connection, not the name.
            let host = v.trim().rsplit_once(':').map_or(v.trim(), |(h, p)| {
                if p.chars().all(|c| c.is_ascii_digit()) {
                    h
                } else {
                    v.trim()
                }
            });
            if host.is_empty() {
                return None;
            }
            return Some(host.to_lowercase());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_is_found_and_the_head_preserved() {
        let req = b"GET /path HTTP/1.1\r\nHost: API.Example.com:8080\r\nX: y\r\n\r\n";
        let (head, host) = read_head_and_host(&mut &req[..], 8192).unwrap();
        assert_eq!(host, "api.example.com");
        assert_eq!(head, req); // verbatim, for the replay
    }

    #[test]
    fn nameless_flows_are_errors_not_guesses() {
        let req = b"GET / HTTP/1.0\r\n\r\n";
        assert!(read_head_and_host(&mut &req[..], 8192).is_err());
        let raw = b"\x00\x01binary noise without any head";
        assert!(read_head_and_host(&mut &raw[..], 32).is_err());
    }
}
