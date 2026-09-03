//! cella-universe's seccomp allowlist (1.6.14b): the shared CLI
//! floor, plus `kill` (machine::is_running's existence probe, read
//! by refuse_running before branch/archive/inspect) and the two
//! syscalls `std::fs::copy` issues on Linux (branch's layer and book
//! copies) -- `copy_file_range` on its fast path, falling back to
//! `sendfile` when the two files are not on the same filesystem.

use cella_libs::seccomp::{Entry, CLI_BASE};

#[rustfmt::skip]
const EXTRA: &[Entry] = &[
    (62,  "kill: machine::is_running's kill(pid, 0) probe (refuse_running)"),
    (40,  "sendfile: std::fs::copy's fallback path (branch's layer/book copies)"),
    (326, "copy_file_range: std::fs::copy's fast path (branch's layer/book copies)"),
    (91,  "fchmod: std::fs::copy setting the destination file's mode after the data copy"),
];

pub fn allowed() -> Vec<Entry> {
    let mut v = CLI_BASE.to_vec();
    v.extend_from_slice(EXTRA);
    v
}

pub fn install() -> std::io::Result<()> {
    cella_libs::seccomp::install(&allowed(), None)
}

/// Self-test hook for `make test-seccomp-universe`.
pub fn selftest_provoke_kill() -> ! {
    cella_libs::seccomp::selftest_provoke_kill(&allowed(), None)
}
