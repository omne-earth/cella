//! The splice: full-duplex, bytes verbatim, half-close honest.
//! Terminate-and-splice means the *connection* is split, never the
//! payload edited -- and a peer's EOF must propagate as EOF, not
//! trap both sides in an open-forever loop.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// An end the splice can pump: read, write, and the thing plain
/// Read+Write cannot say -- half-close, so one direction's EOF
/// reaches the peer while the other direction keeps flowing.
pub trait End: Read + Write {
    /// Signal end-of-stream to this end's peer: shut the write
    /// half down (for TLS, close_notify first). Called once per
    /// direction; the splice tracks that.
    fn finish_write(&mut self);

    /// Arrange an abortive close: the socket dies by RST when it
    /// drops, not by FIN. For the world end of a proxy with an
    /// eight-port reply window this is load-bearing -- an active
    /// FIN close births a 60 s TIME_WAIT on a window port, and
    /// the translator's userspace TCP speaks no timestamps, so
    /// tw_reuse can never recycle the corpse
    /// (docs/ROOTLESS-NETWORK.md, "The translator's TCP": RST
    /// from either side is a lawful end of the flow). By the time
    /// the splice aborts, the member has closed and every relayed
    /// byte is delivered; the FIN dance would be ceremony.
    fn abort(&mut self);
}

fn linger_rst(sock: &TcpStream) {
    use std::os::fd::AsRawFd;
    let lg = libc::linger {
        l_onoff: 1,
        l_linger: 0,
    };
    // SAFETY: our own socket fd, a plain setsockopt.
    unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            &lg as *const libc::linger as *const libc::c_void,
            std::mem::size_of::<libc::linger>() as libc::socklen_t,
        );
    }
}

impl End for TcpStream {
    fn finish_write(&mut self) {
        let _ = self.shutdown(std::net::Shutdown::Write);
    }
    fn abort(&mut self) {
        linger_rst(self);
    }
}

impl End for rustls::StreamOwned<rustls::ServerConnection, TcpStream> {
    fn abort(&mut self) {
        linger_rst(&self.sock);
    }
    fn finish_write(&mut self) {
        self.conn.send_close_notify();
        let _ = self.conn.complete_io(&mut self.sock);
        let _ = self.sock.shutdown(std::net::Shutdown::Write);
    }
}

impl End for rustls::StreamOwned<rustls::ClientConnection, TcpStream> {
    fn abort(&mut self) {
        linger_rst(&self.sock);
    }
    fn finish_write(&mut self) {
        self.conn.send_close_notify();
        let _ = self.conn.complete_io(&mut self.sock);
        let _ = self.sock.shutdown(std::net::Shutdown::Write);
    }
}

#[derive(PartialEq)]
enum Dir {
    Open,
    Eof,
}

/// Pump until both directions have ended. The caller hands ends
/// already handshaken and set nonblocking.
#[cfg(test)]
pub fn splice<A: End, B: End>(a: A, b: B) {
    splice_inner(a, b, false)
}

/// The proxy's splice: like `splice`, except the b end (the world
/// leg) dies by RST the moment the a end (the member) closes --
/// the reply window cannot afford FIN's TIME_WAIT (see
/// `End::abort`).
pub fn splice_rst_world<A: End, B: End>(a: A, b: B) {
    splice_inner(a, b, true)
}

fn splice_inner<A: End, B: End>(mut a: A, mut b: B, rst_b_on_a_eof: bool) {
    let mut a2b = Dir::Open;
    let mut b2a = Dir::Open;
    let mut buf = [0u8; 16 * 1024];
    loop {
        let mut progressed = false;
        if a2b == Dir::Open {
            match pump_once(&mut a, &mut b, &mut buf) {
                Pump::Moved => progressed = true,
                Pump::Idle => {}
                Pump::Done => {
                    a2b = Dir::Eof;
                    if rst_b_on_a_eof {
                        // The a end is done: its bytes are
                        // delivered, and the b end dies by RST,
                        // not FIN -- no TIME_WAIT on the reply
                        // window (see abort). The cost is the
                        // half-close idiom: an a that FINs while
                        // still listening loses the rest of b's
                        // answer. The proxy's crossings accept
                        // that trade; the plain splice does not.
                        b.abort();
                        return;
                    }
                    b.finish_write();
                    progressed = true;
                }
            }
        }
        if b2a == Dir::Open {
            match pump_once(&mut b, &mut a, &mut buf) {
                Pump::Moved => progressed = true,
                Pump::Idle => {}
                Pump::Done => {
                    b2a = Dir::Eof;
                    a.finish_write();
                    progressed = true;
                }
            }
        }
        if a2b == Dir::Eof && b2a == Dir::Eof {
            return;
        }
        if !progressed {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

enum Pump {
    Moved,
    Idle,
    Done,
}

fn pump_once<R: Read, W: Write>(r: &mut R, w: &mut W, buf: &mut [u8]) -> Pump {
    match r.read(buf) {
        Ok(0) => Pump::Done,
        Ok(n) => {
            // The write side may be momentarily full: drain with
            // patience -- dropping spliced bytes is editing the
            // payload.
            let mut off = 0;
            while off < n {
                match w.write(&buf[off..n]) {
                    Ok(0) => return Pump::Done,
                    Ok(m) => off += m,
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    Err(_) => return Pump::Done,
                }
            }
            let _ = w.flush();
            Pump::Moved
        }
        Err(e) if e.kind() == ErrorKind::WouldBlock => Pump::Idle,
        Err(e) if e.kind() == ErrorKind::Interrupted => Pump::Idle,
        Err(_) => Pump::Done,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn spliced_pair() -> (TcpStream, TcpStream, std::thread::JoinHandle<()>) {
        let left_l = TcpListener::bind("127.0.0.1:0").unwrap();
        let right_l = TcpListener::bind("127.0.0.1:0").unwrap();
        let (la, ra) = (left_l.local_addr().unwrap(), right_l.local_addr().unwrap());
        let middle = std::thread::spawn(move || {
            let (a, _) = left_l.accept().unwrap();
            let (b, _) = right_l.accept().unwrap();
            a.set_nonblocking(true).unwrap();
            b.set_nonblocking(true).unwrap();
            splice(a, b);
        });
        (
            TcpStream::connect(la).unwrap(),
            TcpStream::connect(ra).unwrap(),
            middle,
        )
    }

    /// Both directions, concurrently, far past one buffer: the
    /// alternation-deadlock regression.
    #[test]
    fn full_duplex_survives_large_concurrent_transfers() {
        let (left, mut right, middle) = spliced_pair();
        let big_l = vec![0xabu8; 300 * 1024];
        let big_r = vec![0xcdu8; 300 * 1024];
        let (bl, brl) = (big_l.clone(), big_r.len());
        let mut left2 = left.try_clone().unwrap();
        let l_writer = std::thread::spawn(move || {
            left2.write_all(&bl).unwrap();
            left2.shutdown(std::net::Shutdown::Write).unwrap();
        });
        let mut right2 = right.try_clone().unwrap();
        let r_writer = std::thread::spawn(move || {
            right2.write_all(&big_r).unwrap();
            right2.shutdown(std::net::Shutdown::Write).unwrap();
        });
        let mut got_r = Vec::new();
        let mut left_r = left;
        let l_reader = std::thread::spawn(move || {
            left_r.read_to_end(&mut got_r).unwrap();
            got_r
        });
        let mut got_l = Vec::new();
        right.read_to_end(&mut got_l).unwrap();
        assert_eq!(got_l.len(), big_l.len());
        assert!(got_l.iter().all(|b| *b == 0xab));
        let got_r = l_reader.join().unwrap();
        assert_eq!(got_r.len(), brl);
        assert!(got_r.iter().all(|b| *b == 0xcd));
        l_writer.join().unwrap();
        r_writer.join().unwrap();
        middle.join().unwrap();
    }

    /// The hang regression: one side half-closes, the other must
    /// see EOF promptly while the reverse direction still works.
    #[test]
    fn half_close_propagates() {
        let (mut left, mut right, middle) = spliced_pair();
        left.write_all(b"x").unwrap();
        left.shutdown(std::net::Shutdown::Write).unwrap();
        let mut got = Vec::new();
        right.read_to_end(&mut got).unwrap(); // EOF must arrive
        assert_eq!(got, b"x");
        right.write_all(b"reply").unwrap();
        right.shutdown(std::net::Shutdown::Write).unwrap();
        let mut back = Vec::new();
        left.read_to_end(&mut back).unwrap();
        assert_eq!(back, b"reply");
        middle.join().unwrap();
    }
}
