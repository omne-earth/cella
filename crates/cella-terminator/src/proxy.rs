//! The assembly: accept a member flow, find its name, open the
//! world leg, splice. TLS terminates on a minted leaf; plain HTTP
//! names itself by Host; a nameless port routes only by static
//! map; anything else is refused -- never guessed.

use std::io::Write;
use std::net::{Ipv4Addr, TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::ca::Minter;
use crate::config::{Config, PortMap};
use crate::dns;
use crate::http;
use crate::splice::splice_rst_world;

/// Resolve a name to the real world address. Production resolves
/// through the upstream provider with the cache; the tests inject.
pub type Resolver = dyn Fn(&str) -> Result<Ipv4Addr, String> + Send + Sync;

/// One member connection, end to end. `world_port` overrides the
/// dialed port (the tests' listener is ephemeral); production
/// passes None and the world leg uses the port the member dialed
/// -- the resolver-interceptor preserves it.
pub fn handle_conn(
    mut member: TcpStream,
    minter: &Minter,
    resolve: &Resolver,
    world_roots: Arc<rustls::RootCertStore>,
    maps: &[PortMap],
    world_port: Option<u16>,
) -> Result<(), String> {
    let dialed = member.local_addr().map_err(|e| e.to_string())?.port();

    // A statically mapped port is a nameless splice: the map names it.
    if let Some(m) = maps.iter().find(|m| m.listen_port == dialed) {
        let ip = resolve(&m.host)?;
        // Raw TCP has no words: a bounced splice drops, and the
        // member's stack sees the reset it already understands.
        let _permit = crate::gate::acquire()
            .map_err(|t| format!("world queue: ticket {t} bounced (splice, no voice)"))?;
        let world = TcpStream::connect((ip, world_port.unwrap_or(m.port)))
            .map_err(|e| format!("world {}:{}: {e}", m.host, m.port))?;
        member.set_nonblocking(true).map_err(|e| e.to_string())?;
        world.set_nonblocking(true).map_err(|e| e.to_string())?;
        splice_rst_world(member, world);
        return Ok(());
    }

    // Peek one byte: 0x16 is a TLS record, everything else walks
    // the HTTP path (which refuses namelessness on its own).
    let mut first = [0u8; 1];
    let n = member.peek(&mut first).map_err(|e| e.to_string())?;
    if n == 1 && first[0] == 0x16 {
        return terminate_tls(member, minter, resolve, world_roots, dialed, world_port);
    }
    let (head, host) = http::read_head_and_host(&mut member, 16 * 1024)?;
    let ip = resolve(&host)?;
    let permit = match crate::gate::acquire() {
        Ok(p) => p,
        Err(ticket) => {
            // The window is full and the grace lapsed: say so in
            // the member's own protocol, so its client backs off
            // instead of guessing at silence.
            let _ = member.write_all(crate::gate::BUSY_REPLY);
            return Err(format!(
                "world queue: ticket {ticket} bounced -- 429 spoken"
            ));
        }
    };
    let _permit = permit;
    let mut world = TcpStream::connect((ip, world_port.unwrap_or(dialed)))
        .map_err(|e| format!("world {host}:{dialed}: {e}"))?;
    world.write_all(&head).map_err(|e| e.to_string())?;
    member.set_nonblocking(true).map_err(|e| e.to_string())?;
    world.set_nonblocking(true).map_err(|e| e.to_string())?;
    splice_rst_world(member, world);
    Ok(())
}

fn terminate_tls(
    mut member: TcpStream,
    minter: &Minter,
    resolve: &Resolver,
    world_roots: Arc<rustls::RootCertStore>,
    dialed: u16,
    world_port: Option<u16>,
) -> Result<(), String> {
    // The ClientHello names the world peer: read it through the
    // acceptor, mint the leaf, finish the member handshake.
    let mut acceptor = rustls::server::Acceptor::default();
    let accepted = loop {
        acceptor
            .read_tls(&mut member)
            .map_err(|e| format!("member hello: {e}"))?;
        match acceptor.accept() {
            Ok(Some(accepted)) => break accepted,
            Ok(None) => continue,
            Err((e, _)) => return Err(format!("member hello: {e}")),
        }
    };
    let sni = accepted
        .client_hello()
        .server_name()
        .map(|s| s.to_string())
        .ok_or("a TLS flow without SNI is nameless -- refused, never guessed")?;
    let server_cfg = minter.server_config_for(&sni)?;
    let mut member_conn = accepted
        .into_connection(server_cfg)
        .map_err(|(e, _)| format!("member handshake: {e}"))?;
    // Drive the member handshake to completion, blocking.
    while member_conn.is_handshaking() {
        member_conn
            .complete_io(&mut member)
            .map_err(|e| format!("member handshake: {e}"))?;
    }

    // The world leg: the terminator's own connection, verified
    // against real roots -- never blindly (2.7 (j)).
    let ip = resolve(&sni)?;
    let permit = match crate::gate::acquire() {
        Ok(p) => p,
        Err(ticket) => {
            // The member handshake already stands, so the refusal
            // rides the minted leaf as proper HTTP.
            let mut tls = rustls::StreamOwned::new(member_conn, member);
            let _ = tls.write_all(crate::gate::BUSY_REPLY);
            tls.conn.send_close_notify();
            let _ = tls.conn.complete_io(&mut tls.sock);
            return Err(format!(
                "world queue: ticket {ticket} bounced -- 429 spoken (tls)"
            ));
        }
    };
    let _permit = permit;
    let mut world = TcpStream::connect((ip, world_port.unwrap_or(dialed)))
        .map_err(|e| format!("world {sni}:{dialed}: {e}"))?;
    let client_cfg = rustls::ClientConfig::builder()
        .with_root_certificates(world_roots)
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(sni.clone())
        .map_err(|_| format!("unusable SNI {sni:?}"))?;
    let mut world_conn =
        rustls::ClientConnection::new(Arc::new(client_cfg), name).map_err(|e| e.to_string())?;
    while world_conn.is_handshaking() {
        world_conn
            .complete_io(&mut world)
            .map_err(|e| format!("world handshake {sni}: {e}"))?;
    }

    member.set_nonblocking(true).map_err(|e| e.to_string())?;
    world.set_nonblocking(true).map_err(|e| e.to_string())?;
    splice_rst_world(
        rustls::StreamOwned::new(member_conn, member),
        rustls::StreamOwned::new(world_conn, world),
    );
    Ok(())
}

/// Serve one DNS packet: the interceptor's answer (dns.rs). Public
/// for the tests; run() loops it.
pub fn serve_dns_once(sock: &UdpSocket, self_ip: Ipv4Addr) {
    let mut buf = [0u8; 512];
    let Ok((n, peer)) = sock.recv_from(&mut buf) else {
        return;
    };
    let Some((id, q)) = dns::parse_query(&buf[..n]) else {
        return; // unreadable: dropped, never guessed at
    };
    let ans = dns::answer_with_self(id, &q, self_ip);
    let _ = sock.send_to(&ans, peer);
}

/// The production resolver: ask the upstream provider, cache with
/// TTL honesty.
pub fn upstream_resolver(
    upstream: Ipv4Addr,
    upstream_port: u16,
) -> impl Fn(&str) -> Result<Ipv4Addr, String> {
    let cache = std::sync::Mutex::new(dns::Cache::new(Duration::from_secs(300)));
    move |name: &str| {
        let now = Instant::now();
        if let Some(ip) = cache.lock().unwrap().get(name, now) {
            return Ok(ip);
        }
        let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
        sock.set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        let id = (std::process::id() as u16) ^ (now.elapsed().subsec_nanos() as u16);
        let q = dns::build_query(id, name);
        sock.send_to(&q, (upstream, upstream_port))
            .map_err(|e| e.to_string())?;
        let mut buf = [0u8; 512];
        let (n, _) = sock
            .recv_from(&mut buf)
            .map_err(|e| format!("upstream: {e}"))?;
        let (ip, ttl) =
            dns::parse_answer(&buf[..n], id).ok_or_else(|| format!("no A answer for {name}"))?;
        cache.lock().unwrap().put(name, ip, ttl, now);
        Ok(ip)
    }
}

pub fn run(cfg: Config) -> Result<(), String> {
    let minter = Arc::new(Minter::load(&cfg.ca_cert, &cfg.ca_key)?);
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let roots = Arc::new(roots);
    let resolve: Arc<Resolver> = Arc::new(upstream_resolver(cfg.upstream_dns, cfg.upstream_port));
    let maps = Arc::new(cfg.maps.clone());

    // The interceptor's ear.
    let dns_sock = UdpSocket::bind((cfg.wire_ip, cfg.dns_port))
        .map_err(|e| format!("bind {}:{}: {e}", cfg.wire_ip, cfg.dns_port))?;
    let self_ip = cfg.wire_ip;
    std::thread::spawn(move || loop {
        serve_dns_once(&dns_sock, self_ip);
    });

    // One listener per named port, plus each static map's port.
    let mut ports = cfg.listen.clone();
    ports.extend(cfg.maps.iter().map(|m| m.listen_port));
    let mut handles = Vec::new();
    for port in ports {
        let l = TcpListener::bind((cfg.wire_ip, port))
            .map_err(|e| format!("bind {}:{port}: {e}", cfg.wire_ip))?;
        let (minter, resolve, roots, maps) =
            (minter.clone(), resolve.clone(), roots.clone(), maps.clone());
        handles.push(std::thread::spawn(move || {
            for conn in l.incoming().flatten() {
                let (minter, resolve, roots, maps) =
                    (minter.clone(), resolve.clone(), roots.clone(), maps.clone());
                std::thread::spawn(move || {
                    if let Err(e) = handle_conn(conn, &minter, &*resolve, roots, &maps, None) {
                        eprintln!("cella_terminator: {e}");
                    }
                });
            }
        }));
    }
    eprintln!("cella_terminator: serving (wire {})", cfg.wire_ip);
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ca;
    use std::io::Read;

    fn pair_minter(tag: &str) -> (Minter, String) {
        let (ca_pem, key_pem) = ca::mint_pair_ca(tag).unwrap();
        let d = std::env::temp_dir().join(format!("cella-term-proxy-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("ca.pem"), &ca_pem).unwrap();
        std::fs::write(d.join("ca.key"), &key_pem).unwrap();
        (
            Minter::load(&d.join("ca.pem"), &d.join("ca.key")).unwrap(),
            ca_pem,
        )
    }

    /// A fake world: its own CA, a TLS echo server for one name.
    fn world_tls(name: &'static str) -> (std::net::SocketAddr, String) {
        let (world_ca_pem, world_key_pem) = ca::mint_pair_ca("world").unwrap();
        let d =
            std::env::temp_dir().join(format!("cella-term-world-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("ca.pem"), &world_ca_pem).unwrap();
        std::fs::write(d.join("ca.key"), &world_key_pem).unwrap();
        let minter = Minter::load(&d.join("ca.pem"), &d.join("ca.key")).unwrap();
        let cfg = minter.server_config_for(name).unwrap();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            for tcp in l.incoming().flatten() {
                let cfg = cfg.clone();
                std::thread::spawn(move || {
                    let mut tcp = tcp;
                    let mut conn = rustls::ServerConnection::new(cfg).unwrap();
                    let mut tls = rustls::Stream::new(&mut conn, &mut tcp);
                    let mut buf = [0u8; 5];
                    if tls.read_exact(&mut buf).is_ok() {
                        let _ = tls.write_all(b"world");
                        let _ = tls.write_all(&buf);
                    }
                    tls.conn.send_close_notify();
                    let _ = tls.flush();
                });
            }
        });
        (addr, world_ca_pem)
    }

    fn roots_of(pem: &str) -> Arc<rustls::RootCertStore> {
        let mut reader = std::io::BufReader::new(pem.as_bytes());
        let mut roots = rustls::RootCertStore::empty();
        for cert in rustls_pemfile::certs(&mut reader) {
            roots.add(cert.unwrap()).unwrap();
        }
        Arc::new(roots)
    }

    /// The whole terminated walk on loopback: member (trusting the
    /// pair CA) -> proxy (minting for the SNI, verifying the world
    /// against the world's root) -> world echo.
    #[test]
    fn tls_terminates_and_splices_end_to_end() {
        let (minter, pair_pem) = pair_minter("e2e");
        let (world_addr, world_pem) = world_tls("api.world.test");
        let minter = Arc::new(minter);

        let proxy_l = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_addr = proxy_l.local_addr().unwrap();
        let world_roots = roots_of(&world_pem);
        let m2 = minter.clone();
        std::thread::spawn(move || {
            let (conn, _) = proxy_l.accept().unwrap();
            let resolve = |_: &str| Ok(Ipv4Addr::LOCALHOST);
            handle_conn(
                conn,
                &m2,
                &resolve,
                world_roots,
                &[],
                Some(world_addr.port()),
            )
            .unwrap();
        });

        // The member dials the proxy believing it is the world.
        let mut tcp = TcpStream::connect(proxy_addr).unwrap();
        let client_cfg = rustls::ClientConfig::builder()
            .with_root_certificates(Arc::try_unwrap(roots_of(&pair_pem)).unwrap())
            .with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from("api.world.test").unwrap();
        let mut conn = rustls::ClientConnection::new(Arc::new(client_cfg), name).unwrap();
        let mut tls = rustls::Stream::new(&mut conn, &mut tcp);
        tls.write_all(b"ping!").unwrap();
        let mut buf = [0u8; 10];
        tls.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"worldping!");
    }

    /// Plain HTTP: the Host names the flow, the head replays
    /// verbatim, the splice carries the response.
    #[test]
    fn http_names_itself_and_splices() {
        let (minter, _) = pair_minter("http");
        let world_l = TcpListener::bind("127.0.0.1:0").unwrap();
        let world_addr = world_l.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = world_l.accept().unwrap();
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                s.read_exact(&mut b).unwrap();
                head.push(b[0]);
            }
            assert!(head.starts_with(b"GET /x HTTP/1.1"));
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .unwrap();
        });

        let proxy_l = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_addr = proxy_l.local_addr().unwrap();
        let minter = Arc::new(minter);
        std::thread::spawn(move || {
            let (conn, _) = proxy_l.accept().unwrap();
            let resolve = |_: &str| Ok(Ipv4Addr::LOCALHOST);
            handle_conn(
                conn,
                &minter,
                &resolve,
                Arc::new(rustls::RootCertStore::empty()),
                &[],
                Some(world_addr.port()),
            )
            .unwrap();
        });
        let mut s = TcpStream::connect(proxy_addr).unwrap();
        s.write_all(b"GET /x HTTP/1.1\r\nHost: plain.world.test\r\n\r\n")
            .unwrap();
        let mut resp = Vec::new();
        s.read_to_end(&mut resp).unwrap();
        assert!(resp.ends_with(b"ok"));
    }

    /// The interceptor's ear answers home.
    #[test]
    fn dns_answers_with_self() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_addr = server.local_addr().unwrap();
        let home = Ipv4Addr::new(10, 77, 7, 1);
        std::thread::spawn(move || serve_dns_once(&server, home));
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client
            .send_to(&dns::build_query(9, "any.name.test"), server_addr)
            .unwrap();
        let mut buf = [0u8; 512];
        let (n, _) = client.recv_from(&mut buf).unwrap();
        let (ip, _) = dns::parse_answer(&buf[..n], 9).unwrap();
        assert_eq!(ip, home);
    }
}
