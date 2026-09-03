//! The syscalls `cella_build::orchestrate` needs to spawn an external
//! process (`toolbox`, `podman`, `ldd`) and wait for it -- shared
//! between cella-build's own binary and cella-doctor's `fix` (its
//! other user, see tasks/PHASE1-core.md 1.6.13). Read from
//! `std::process::Command`'s Linux implementation: recent glibc/std
//! prefer `posix_spawn`, which itself clones with `vfork`-shaped
//! flags before `execve`; the exact clone flavor a given libc picks
//! is not part of any ABI guarantee, so both `clone` and `vfork` are
//! listed rather than assumed.

use cella_libs::seccomp::Entry;

#[rustfmt::skip]
pub const SPAWN_EXTRA: &[Entry] = &[
    (56,  "clone: std::process::Command spawning toolbox/podman/ldd"),
    (58,  "vfork: the posix_spawn fast path some libcs take for a plain exec"),
    (59,  "execve: the child side of every Command spawn, before it becomes the external program"),
    (61,  "wait4: Command::status()/output() waiting on the child"),
    (247, "waitid: an alternate reap path some libc/std versions use instead of wait4"),
    (293, "pipe2: capturing a child's stdout for Command::output() (ldd, toolbox list)"),
    (33,  "dup2: wiring a piped fd to the child's stdin/stdout/stderr"),
    (435, "clone3: the newer clone entry point; glibc picks it over clone on recent kernels"),
];
