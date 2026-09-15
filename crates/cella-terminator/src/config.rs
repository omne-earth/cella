//! The terminator's one configuration file, strict like every
//! parser in the house: an unreadable line is an error naming its
//! number, never a skipped rule.
//!
//!     wire_ip=10.77.7.1
//!     upstream_dns=9.9.9.9
//!     listen=443,80
//!     ca_cert=/etc/cella/pair-ca.pem
//!     ca_key=/etc/cella/pair-ca.key
//!     # a nameless bare-TCP port routes only by a static map:
//!     map=2222:git.internal.example:22

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct PortMap {
    pub listen_port: u16,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub wire_ip: Ipv4Addr,
    pub upstream_dns: Ipv4Addr,
    /// TCP ports the proxy listens on for named flows (TLS by SNI
    /// on any of them; plain HTTP by Host on any of them).
    pub listen: Vec<u16>,
    pub ca_cert: PathBuf,
    pub ca_key: PathBuf,
    /// The static maps for nameless flows.
    pub maps: Vec<PortMap>,
}

pub fn load(path: &Path) -> Result<Config, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn parse(text: &str) -> Result<Config, String> {
    let mut wire_ip = None;
    let mut upstream = None;
    let mut listen: Vec<u16> = Vec::new();
    let mut ca_cert = None;
    let mut ca_key = None;
    let mut maps = Vec::new();
    for (idx, raw) in text.lines().enumerate() {
        let n = idx + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("line {n}: no '=' in {line:?}"))?;
        match key.trim() {
            "wire_ip" => wire_ip = Some(parse_ip(value).map_err(|e| format!("line {n}: {e}"))?),
            "upstream_dns" => {
                upstream = Some(parse_ip(value).map_err(|e| format!("line {n}: {e}"))?)
            }
            "listen" => {
                for p in value.split(',') {
                    listen.push(
                        p.trim()
                            .parse()
                            .map_err(|_| format!("line {n}: unreadable port {p:?}"))?,
                    );
                }
            }
            "ca_cert" => ca_cert = Some(PathBuf::from(value.trim())),
            "ca_key" => ca_key = Some(PathBuf::from(value.trim())),
            "map" => {
                let mut parts = value.trim().splitn(3, ':');
                let (a, b, c) = (parts.next(), parts.next(), parts.next());
                let (Some(lp), Some(host), Some(rp)) = (a, b, c) else {
                    return Err(format!(
                        "line {n}: map takes listen_port:host:port, not {value:?}"
                    ));
                };
                maps.push(PortMap {
                    listen_port: lp
                        .parse()
                        .map_err(|_| format!("line {n}: unreadable port {lp:?}"))?,
                    host: host.to_string(),
                    port: rp
                        .parse()
                        .map_err(|_| format!("line {n}: unreadable port {rp:?}"))?,
                });
            }
            k => return Err(format!("line {n}: unknown key {k:?}")),
        }
    }
    Ok(Config {
        wire_ip: wire_ip.ok_or("wire_ip is mandatory")?,
        upstream_dns: upstream.ok_or("upstream_dns is mandatory")?,
        listen: if listen.is_empty() {
            vec![443, 80]
        } else {
            listen
        },
        ca_cert: ca_cert.unwrap_or_else(|| PathBuf::from("/etc/cella/pair-ca.pem")),
        ca_key: ca_key.unwrap_or_else(|| PathBuf::from("/etc/cella/pair-ca.key")),
        maps,
    })
}

fn parse_ip(v: &str) -> Result<Ipv4Addr, String> {
    v.trim().parse().map_err(|_| format!("unreadable ip {v:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_parses() {
        let c = parse(
            "wire_ip=10.77.7.1\nupstream_dns=9.9.9.9\nlisten=443,80\nmap=2222:git.internal:22\n",
        )
        .unwrap();
        assert_eq!(c.wire_ip, Ipv4Addr::new(10, 77, 7, 1));
        assert_eq!(c.listen, vec![443, 80]);
        assert_eq!(
            c.maps,
            vec![PortMap {
                listen_port: 2222,
                host: "git.internal".into(),
                port: 22
            }]
        );
    }

    #[test]
    fn errors_name_their_line() {
        for (text, needle) in [
            ("wire_ip=10.0.0.1", "upstream_dns is mandatory"),
            ("upstream_dns=9.9.9.9", "wire_ip is mandatory"),
            ("frob=1", "unknown key"),
            ("wire_ip=not-an-ip", "unreadable ip"),
            ("map=weird", "map takes"),
        ] {
            let err = parse(text).unwrap_err();
            assert!(err.contains(needle), "{text:?} -> {err:?}");
        }
    }
}
