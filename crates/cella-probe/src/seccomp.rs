//! cella-probe's seccomp table (1.6.14b) -- NOT installed in
//! production yet, deliberately (flagged for the reviewer).
//!
//! Every probe (wallclock, freeze-thaw-clock, sregs) opens /dev/kvm
//! directly and drives a broad, probe-specific set of KVM ioctls to
//! answer "is time cryogenic here" -- an evolving diagnostic
//! surface, not the enforced product (docs/LIFECYCLE.md: "the probe
//! is the instrument, not the product"; the inception image's clock
//! probe is the one deliberately unjailed exception in the whole
//! system). Narrowing it to an exact KVM request table the way
//! cella-vmm's run loop is narrowed belongs with whichever lane next
//! touches the probes' KVM surface, not this shakedown's per-binary
//! floor. This module carries the shared CLI floor and the self-test
//! hook the gate requires; `main()` does not call `install()`.

use cella_libs::seccomp::{Entry, CLI_BASE};

pub fn allowed() -> Vec<Entry> {
    CLI_BASE.to_vec()
}

/// Self-test hook for `make test-seccomp-probe`.
pub fn selftest_provoke_kill() -> ! {
    cella_libs::seccomp::selftest_provoke_kill(&allowed(), None)
}
