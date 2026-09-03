//! cella-gateway's seccomp allowlist (1.6.14b): the shared CLI floor
//! (`cella_libs::seccomp::CLI_BASE`), plus `kill` -- `open`/`close`
//! signal the running VMM with SIGWINCH (the live valve wire), and
//! `is_running` probes a pid with `kill(pid, 0)` before every verb
//! that touches a running machine. Traced and read against
//! `gateway.rs` on 2026-09-02.

use cella_libs::seccomp::{Entry, CLI_BASE};

#[rustfmt::skip]
const EXTRA: &[Entry] = &[
    (62, "kill: is_running's kill(pid, 0) probe, and the SIGWINCH valve edge (gateway::open/close)"),
];

pub fn allowed() -> Vec<Entry> {
    let mut v = CLI_BASE.to_vec();
    v.extend_from_slice(EXTRA);
    v
}

pub fn install() -> std::io::Result<()> {
    cella_libs::seccomp::install(&allowed(), None)
}

/// Self-test hook for `make test-seccomp-gateway`.
pub fn selftest_provoke_kill() -> ! {
    cella_libs::seccomp::selftest_provoke_kill(&allowed(), None)
}
