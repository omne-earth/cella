//! The probe: a member's TLS voice, for the gates and for field
//! diagnosis. It walks exactly the member's path -- resolve the
//! name at the given resolver (the interceptor, in a pair), dial
//! what the answer says, handshake with SNI against the given
//! trust anchor, then try one HTTP request through.
//!
//! Exit meanings, precise for the gates:
//!   0  handshake verified and the world answered
//!   3  handshake verified, but nothing answered behind it (a
//!      terminated leg with a dead world -- the local gates' case)
//!   1  anything else, the reason on stderr

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpStream, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use crate::dns;

pub struct ProbeArgs {
    pub name: String,
    pub port: u16,
    pub ns: Ipv4Addr,
    pub ns_port: u16,
    pub ca_pem: std::path::PathBuf,
}

pub fn run(a: &ProbeArgs) -> i32 {
    match probe(a) {
        Ok(answered) => {
            if answered {
                0
            } else {
                3
            }
        }
        Err(e) => {
            eprintln!("probe: {e}");
            1
        }
    }
}

fn probe(a: &ProbeArgs) -> Result<bool, String> {
    // Resolve at the named resolver -- in a pair, the interceptor.
    let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    sock.set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    let id = std::process::id() as u16;
    sock.send_to(&dns::build_query(id, &a.name), (a.ns, a.ns_port))
        .map_err(|e| e.to_string())?;
    let mut buf = [0u8; 512];
    let (n, _) = sock
        .recv_from(&mut buf)
        .map_err(|e| format!("resolver {}: {e}", a.ns))?;
    let (ip, _) =
        dns::parse_answer(&buf[..n], id).ok_or_else(|| format!("no A answer for {}", a.name))?;
    println!("probe: {} -> {ip}", a.name);

    // Dial what the answer said, handshake with SNI against the
    // given anchor: a wrong or unauthorized middle fails here.
    let mut tcp =
        TcpStream::connect((ip, a.port)).map_err(|e| format!("dial {ip}:{}: {e}", a.port))?;
    tcp.set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    let pem = std::fs::read_to_string(&a.ca_pem)
        .map_err(|e| format!("reading {}: {e}", a.ca_pem.display()))?;
    let mut roots = rustls::RootCertStore::empty();
    let mut reader = std::io::BufReader::new(pem.as_bytes());
    for cert in rustls_pemfile::certs(&mut reader) {
        roots
            .add(cert.map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server = rustls::pki_types::ServerName::try_from(a.name.clone())
        .map_err(|_| format!("unusable name {:?}", a.name))?;
    let mut conn =
        rustls::ClientConnection::new(Arc::new(cfg), server).map_err(|e| e.to_string())?;
    while conn.is_handshaking() {
        conn.complete_io(&mut tcp)
            .map_err(|e| format!("handshake: {e}"))?;
    }
    println!("probe: verified {}", a.name);

    // One request through; a dead world behind a live handshake is
    // its own answer (exit 3).
    let mut tls = rustls::Stream::new(&mut conn, &mut tcp);
    let req = format!(
        "GET / HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        a.name
    );
    if tls.write_all(req.as_bytes()).is_err() {
        return Ok(false);
    }
    let mut first = [0u8; 64];
    match tls.read(&mut first) {
        Ok(n) if n > 0 => {
            let line = String::from_utf8_lossy(&first[..n]);
            println!("probe: answered: {}", line.lines().next().unwrap_or(""));
            Ok(true)
        }
        _ => Ok(false),
    }
}
