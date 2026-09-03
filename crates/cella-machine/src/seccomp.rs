//! cella-machine's seccomp allowlist (1.6.14b).
//!
//! IMPORTANT SCOPING NOTE (flagged for the reviewer, not a guess): a
//! seccomp filter installed with `prctl(PR_SET_SECCOMP)` is inherited
//! across `execve` and is additive across the chain -- a child can
//! only be *more* restricted than its parent's filter, never less.
//! `start` and `thaw` fork+exec the bwrap+cella-vmm process tree,
//! which installs its *own*, separately-scoped filter once it is
//! running (see cella-vmm/src/seccomp.rs) right before its run loop.
//! If cella-machine also installed a narrow filter of its own before
//! that fork, cella-vmm would inherit it too and could die on its
//! *own* legitimate setup syscalls (bwrap's unshare/mount, KVM_CREATE_VM,
//! newuidmap, etc.) -- syscalls this table was never meant to judge.
//!
//! So: this filter installs only for the verbs that do not fork a
//! child process tree with different needs, and that do not need
//! `socket` (this crate's self-test canary -- see
//! `cella_libs::seccomp::selftest_provoke_kill` -- must stay outside
//! every installed persona filter, or the negative gate cannot tell
//! "the filter is live" from "the filter permits everything"):
//! `list`, `info`, `create`, `destroy`, `stop`, `freeze`. `enter`
//! (it opens a UnixStream to the running VMM's console.sock, hence
//! needs `socket`/`connect`), `start`, `thaw`, and `selftest` (which
//! drives the full lifecycle, KVM included) run unconfined at this
//! layer -- cella-vmm's own filter is where their sensitive work ends
//! up bounded.

use cella_libs::seccomp::{Entry, CLI_BASE};

#[rustfmt::skip]
const EXTRA: &[Entry] = &[
    (62,  "kill: is_running/stop/freeze's kill(pid, ...) -- probe, SIGKILL, SIGUSR1"),
    (35,  "nanosleep: freeze's poll loop (std::thread::sleep) waiting for the VMM to exit"),
    (230, "clock_nanosleep: the clock_nanosleep-backed form std::thread::sleep can take"),
];

/// The verbs safe to confine at this layer -- see the module doc.
pub const SAFE_VERBS: &[&str] = &["list", "info", "create", "destroy", "stop", "freeze"];

pub fn allowed() -> Vec<Entry> {
    let mut v = CLI_BASE.to_vec();
    v.extend_from_slice(EXTRA);
    v
}

pub fn install() -> std::io::Result<()> {
    cella_libs::seccomp::install(&allowed(), None)
}

/// Self-test hook for `make test-seccomp-machine`.
pub fn selftest_provoke_kill() -> ! {
    cella_libs::seccomp::selftest_provoke_kill(&allowed(), None)
}
