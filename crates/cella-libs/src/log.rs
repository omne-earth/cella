//! The operator logs' clock stamps. Logs are streams and streams
//! lead with time: every line of vmm.log and edge.log starts with
//! `host_ns=<ns>`, and lines born where a guest clock is in hand
//! carry `guest_ns=<ns>` beside it -- the same two clocks, the
//! same spellings, as the books (proto/cella.proto, Operation).
//! The books themselves render their stamps trailing, as fields
//! on a record; a log leads, so `sort -m` interleaves the streams
//! and the eye finds the clock where it rests. Raw nanoseconds,
//! never a formatted date: rendering for humans is the reader's
//! job, and the books' arithmetic stays the only arithmetic.

/// The host clock, epoch nanoseconds -- self-contained so every
/// persona logs without the wire feature.
pub fn host_ns_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// A log line stamped with the host clock.
#[macro_export]
macro_rules! logln {
    ($($arg:tt)*) => {
        eprintln!(
            "host_ns={} {}",
            $crate::log::host_ns_now(),
            format!($($arg)*)
        )
    };
}

/// A log line stamped with both clocks -- for call sites that
/// hold the guest's frame (a park, a delivery, the ratchet).
#[macro_export]
macro_rules! logln_guest {
    ($guest_ns:expr, $($arg:tt)*) => {
        eprintln!(
            "host_ns={} guest_ns={} {}",
            $crate::log::host_ns_now(),
            $guest_ns,
            format!($($arg)*)
        )
    };
}
