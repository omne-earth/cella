//! The world gate: the reply window spoken, not suffered.
//!
//! The appliance's world legs source from eight ports (the
//! consistent reply window, rootfs-terminator.sh), so at most
//! eight world crossings live at once. Before this gate a ninth
//! died mutely -- TCP accepted, then nothing -- and the member's
//! client hot-looped against the silence (titanium's ekdh4mm
//! trial). The gate makes the constraint legible: a crossing
//! takes a FIFO ticket for one of eight permits; a short grace
//! absorbs bursts that clear in milliseconds; a crossing the
//! grace cannot seat is REFUSED IN WORDS -- the HTTP paths answer
//! 429 Too Many Requests with Retry-After -- so the resident
//! knows to back off instead of guessing. One line per bounced
//! ticket is the queue's own book.

use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The window's width. Must match the guest's
/// ip_local_port_range (rootfs-terminator.sh, 50000-50007).
pub const WORLD_PERMITS: u32 = 8;

/// How long a ticket waits for a permit before it is bounced.
/// Long enough that a burst which clears at RST speed (~ms per
/// crossing) seats everyone; short enough that a saturated
/// window answers promptly.
pub const GRACE: Duration = Duration::from_millis(500);

/// What a bounced HTTP crossing hears. Retry-After matches the
/// drain a released permit makes possible.
pub const BUSY_REPLY: &[u8] = b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

#[derive(Debug)]
struct State {
    next_ticket: u64,
    serving: u64,
    in_use: u32,
    /// Tickets that left the line before being served. The line
    /// advances past a ghost wherever it meets one -- a mid-line
    /// timeout must not wedge everyone behind it.
    abandoned: std::collections::HashSet<u64>,
}

impl State {
    fn advance_past_ghosts(&mut self) {
        while self.abandoned.remove(&self.serving) {
            self.serving += 1;
        }
    }
}

#[derive(Debug)]
struct Gate {
    state: Mutex<State>,
    cv: Condvar,
}

impl Gate {
    fn new() -> Gate {
        Gate {
            state: Mutex::new(State {
                next_ticket: 0,
                serving: 0,
                in_use: 0,
                abandoned: std::collections::HashSet::new(),
            }),
            cv: Condvar::new(),
        }
    }

    /// Take a place in this gate's line; see `acquire`.
    fn acquire_on(&self, grace: Duration) -> Result<Permit<'_>, u64> {
        let deadline = Instant::now() + grace;
        let mut st = self.state.lock().unwrap();
        let ticket = st.next_ticket;
        st.next_ticket += 1;
        loop {
            st.advance_past_ghosts();
            if st.serving == ticket && st.in_use < WORLD_PERMITS {
                st.serving += 1;
                st.in_use += 1;
                self.cv.notify_all();
                return Ok(Permit { gate: self });
            }
            let now = Instant::now();
            if now >= deadline {
                // Leaving the line: mark the ticket abandoned so
                // the line advances past it wherever it stands --
                // a head bounce moves serving now, and a mid-line
                // bounce leaves a ghost the next advance skips.
                st.abandoned.insert(ticket);
                st.advance_past_ghosts();
                self.cv.notify_all();
                return Err(ticket);
            }
            let (guard, _) = self.cv.wait_timeout(st, deadline - now).unwrap();
            st = guard;
        }
    }
}

fn gate() -> &'static Gate {
    static GATE: OnceLock<Gate> = OnceLock::new();
    GATE.get_or_init(Gate::new)
}

/// One seated crossing; the permit frees when it drops.
#[derive(Debug)]
pub struct Permit<'a> {
    gate: &'a Gate,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut st = self.gate.state.lock().unwrap();
        st.in_use -= 1;
        self.gate.cv.notify_all();
    }
}

/// Take a place in line. FIFO by ticket: a burst seats in arrival
/// order. `Err` carries the ticket id for the bounce line -- the
/// caller speaks the refusal in its own protocol.
pub fn acquire() -> Result<Permit<'static>, u64> {
    gate().acquire_on(GRACE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn a_window_wide_burst_all_seats() {
        let g = Gate::new();
        let permits: Vec<_> = (0..WORLD_PERMITS)
            .map(|_| g.acquire_on(Duration::from_millis(10)).unwrap())
            .collect();
        drop(permits);
    }

    #[test]
    fn the_ninth_bounces_after_the_grace_with_its_ticket() {
        let g = Gate::new();
        let _held: Vec<_> = (0..WORLD_PERMITS)
            .map(|_| g.acquire_on(Duration::from_millis(10)).unwrap())
            .collect();
        let t0 = Instant::now();
        let bounced = g.acquire_on(Duration::from_millis(50));
        assert_eq!(bounced.unwrap_err(), WORLD_PERMITS as u64);
        assert!(t0.elapsed() >= Duration::from_millis(50), "bounced early");
    }

    #[test]
    fn a_dropped_permit_seats_the_waiter() {
        let g = std::sync::Arc::new(Gate::new());
        let mut held: Vec<_> = (0..WORLD_PERMITS)
            .map(|_| g.acquire_on(Duration::from_millis(10)).unwrap())
            .collect();
        let g2 = g.clone();
        let (tx, rx) = mpsc::channel();
        let h = std::thread::spawn(move || {
            let p = g2.acquire_on(Duration::from_secs(2));
            tx.send(p.is_ok()).unwrap();
            drop(p);
        });
        std::thread::sleep(Duration::from_millis(50));
        held.pop();
        assert!(
            rx.recv().unwrap(),
            "the freed permit did not seat the waiter"
        );
        h.join().unwrap();
        drop(held);
    }

    #[test]
    fn a_mid_line_ghost_does_not_wedge_the_line() {
        let g = std::sync::Arc::new(Gate::new());
        let mut held: Vec<_> = (0..WORLD_PERMITS)
            .map(|_| g.acquire_on(Duration::from_millis(10)).unwrap())
            .collect();
        // Ticket 8 waits patiently; ticket 9 gives up mid-line.
        let g2 = g.clone();
        let patient = std::thread::spawn(move || g2.acquire_on(Duration::from_secs(3)).is_ok());
        std::thread::sleep(Duration::from_millis(30));
        assert!(
            g.acquire_on(Duration::from_millis(30)).is_err(),
            "ticket 9 should bounce"
        );
        // Drain everything; the patient waiter must seat, and a
        // fresh arrival after the ghost must seat immediately.
        held.clear();
        assert!(
            patient.join().unwrap(),
            "the ghost wedged the patient waiter"
        );
        assert!(
            g.acquire_on(Duration::from_millis(100)).is_ok(),
            "the ghost wedged the line for later arrivals"
        );
    }

    #[test]
    fn the_line_is_fifo_by_ticket() {
        let g = std::sync::Arc::new(Gate::new());
        let mut held: Vec<_> = (0..WORLD_PERMITS)
            .map(|_| g.acquire_on(Duration::from_millis(10)).unwrap())
            .collect();
        let (tx, rx) = mpsc::channel();
        let mut waiters = Vec::new();
        for id in 0..3u32 {
            let g2 = g.clone();
            let tx2 = tx.clone();
            waiters.push(std::thread::spawn(move || {
                let p = g2.acquire_on(Duration::from_secs(5)).unwrap();
                tx2.send(id).unwrap();
                // Hold briefly so the next waiter seats after us.
                std::thread::sleep(Duration::from_millis(20));
                drop(p);
            }));
            // Stagger arrivals so ticket order is the spawn order.
            std::thread::sleep(Duration::from_millis(50));
        }
        held.pop();
        let order: Vec<u32> = (0..3).map(|_| rx.recv().unwrap()).collect();
        assert_eq!(order, vec![0, 1, 2], "the line jumped");
        for w in waiters {
            w.join().unwrap();
        }
        drop(held);
    }
}
