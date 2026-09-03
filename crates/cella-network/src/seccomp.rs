//! cella-network's seccomp table (1.6.14b) -- NOT installed in
//! production yet, deliberately (flagged for the reviewer).
//!
//! `setup`/`pair`/`own` provision TAP devices and netlink addresses:
//! `ioctl(TUNSETIFF, ...)`, `AF_NETLINK` sockets for addressing and
//! deterministic MACs, and the NAT table (via `nft`, an external
//! process). tasks/PHASE1.md 1.6.14e retires exactly these verbs
//! (setup/pair/own as host-netns verbs, nftables NAT) and rewrites
//! cella-network around fd-passing instead -- "the seccomp list
//! re-shrinks with it" is 1.6.14e's own text. Building this
//! persona's list twice, once now against a surface that is about to
//! be deleted and once after 1.6.14e, is waste; this module carries
//! the shared CLI floor and the self-test hook the gate requires, and
//! `main()` does not call `install()` for the real verbs -- doing so
//! now would either be too narrow (breaking the tap/netlink/nft work)
//! or too wide (no real narrowing, thrown away in a few commits
//! anyway).

use cella_libs::seccomp::{Entry, CLI_BASE};

pub fn allowed() -> Vec<Entry> {
    CLI_BASE.to_vec()
}

/// Self-test hook for `make test-seccomp-network`. Proves the BPF
/// mechanism itself is sound for this persona's floor; the real
/// verbs stay unconfined until 1.6.14e rewrites their surface (see
/// the module doc).
pub fn selftest_provoke_kill() -> ! {
    cella_libs::seccomp::selftest_provoke_kill(&allowed(), None)
}
