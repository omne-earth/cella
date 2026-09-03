//! cella-doctor's seccomp table (1.6.14b).
//!
//! `check`, `fix`, `gate`, and `harvest` all shell out to trusted
//! host tools this process does not own (`bwrap --version`, `ip`,
//! `systemctl`, `nft`, `podman`, `sudo usermod`, `ausearch`, `date`,
//! and cella-network's own binary) to read or repair host facts.
//! Because a seccomp filter installed here is inherited across
//! `execve` and can only get stricter down the chain, narrowing this
//! process would also narrow every one of those external tools --
//! either killing them outright (none of their real syscall needs,
//! e.g. `ip`'s netlink sockets or `sudo`'s PAM/setuid path, are
//! enumerable from this crate) or requiring a table so wide it stops
//! meaning anything. So `check`/`fix`/`gate`/`harvest` stay
//! unconfined at this layer, same reasoning as cella-machine's
//! `start`/`thaw` (see cella-machine/src/seccomp.rs).
//!
//! `verify`/`verify_machine` are the one pure subcommand family: they
//! read manifests and hash storage layers (`cella_libs::golden`) and
//! spawn nothing, so they confine cleanly to the shared CLI floor.

use cella_libs::seccomp::{Entry, CLI_BASE};

/// The verbs safe to confine at this layer -- see the module doc.
pub const SAFE_VERBS: &[&str] = &["verify"];

pub fn allowed() -> Vec<Entry> {
    CLI_BASE.to_vec()
}

pub fn install() -> std::io::Result<()> {
    cella_libs::seccomp::install(&allowed(), None)
}

/// Self-test hook for `make test-seccomp-doctor`.
pub fn selftest_provoke_kill() -> ! {
    cella_libs::seccomp::selftest_provoke_kill(&allowed(), None)
}
