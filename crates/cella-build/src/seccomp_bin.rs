//! cella-build's seccomp allowlist (1.6.14b): the shared CLI floor,
//! plus the spawn set every orchestrate.rs step needs to run
//! `toolbox`/`podman`/`curl`/`ldd` and wait for it.

use cella_build::seccomp::SPAWN_EXTRA;
use cella_libs::seccomp::{Entry, CLI_BASE};

pub fn allowed() -> Vec<Entry> {
    let mut v = CLI_BASE.to_vec();
    v.extend_from_slice(SPAWN_EXTRA);
    v
}

pub fn install() -> std::io::Result<()> {
    cella_libs::seccomp::install(&allowed(), None)
}

/// Self-test hook for `make test-seccomp-build`.
pub fn selftest_provoke_kill() -> ! {
    cella_libs::seccomp::selftest_provoke_kill(&allowed(), None)
}
