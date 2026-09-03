//! cella-vmm's seccomp allowlist (1.6.14b): the run loop's own
//! syscalls, plus the exact KVM ioctl request numbers it issues once
//! the filter is live. The BPF builder and installer live in
//! `cella_libs::seccomp`; this module owns only the table and the
//! comment naming who needs each entry, same discipline as before
//! the split.
//!
//! The filter installs once, right before the run loop -- after
//! every file has been opened and every KVM object has been created
//! (see `main::main`). Everything before that point (KVM_CREATE_VM,
//! KVM_CREATE_VCPU, KVM_SET_USER_MEMORY_REGION, cpuid/sregs setup,
//! the tap's TUNSETIFF, KVM_PRE_FAULT_MEMORY, and on a thaw the
//! KVM_SET_* restore calls) runs unfiltered by this list -- the bwrap
//! jail is the boundary there. From install() onward the run loop
//! issues exactly two things on `ioctl`: KVM_RUN, every pass, and --
//! only on the freeze path -- the KVM_GET_* calls that read the
//! vCPU's live state into the freeze image. Nothing else calls
//! `ioctl` after this point, so the KVM ioctl filter's request table
//! is also the freeze path's complete inventory.

use cella_libs::seccomp::Entry;
use kvm_bindings::{
    kvm_clock_data, kvm_fpu, kvm_irq_level, kvm_irqchip, kvm_lapic_state, kvm_mp_state, kvm_msrs,
    kvm_pit_state2, kvm_regs, kvm_sregs, kvm_vcpu_events, kvm_xcrs, kvm_xsave,
};
use std::io;
use vmm_sys_util::{ioctl_io_nr, ioctl_ioc_nr, ioctl_ior_nr, ioctl_iow_nr, ioctl_iowr_nr};

const KVMIO: std::os::raw::c_uint = 0xAE;

// Redeclared locally rather than imported: kvm-ioctls keeps its own
// copies of these private (it calls them from inside VcpuFd/VmFd
// methods, never returns the raw numbers). Same macro, same type,
// same nr -- the value this crate computes is the value kvm-ioctls's
// own call already used; nothing here is a guess.
ioctl_io_nr!(KVM_RUN, KVMIO, 0x80);
ioctl_ior_nr!(KVM_GET_REGS, KVMIO, 0x81, kvm_regs);
ioctl_ior_nr!(KVM_GET_SREGS, KVMIO, 0x83, kvm_sregs);
ioctl_iowr_nr!(KVM_GET_MSRS, KVMIO, 0x88, kvm_msrs);
ioctl_ior_nr!(KVM_GET_FPU, KVMIO, 0x8c, kvm_fpu);
ioctl_ior_nr!(KVM_GET_MP_STATE, KVMIO, 0x98, kvm_mp_state);
ioctl_ior_nr!(KVM_GET_VCPU_EVENTS, KVMIO, 0x9f, kvm_vcpu_events);
ioctl_ior_nr!(KVM_GET_XSAVE, KVMIO, 0xa4, kvm_xsave);
ioctl_ior_nr!(KVM_GET_XCRS, KVMIO, 0xa6, kvm_xcrs);
ioctl_io_nr!(KVM_GET_TSC_KHZ, KVMIO, 0xa3);
ioctl_iowr_nr!(KVM_GET_IRQCHIP, KVMIO, 0x62, kvm_irqchip);
ioctl_ior_nr!(KVM_GET_LAPIC, KVMIO, 0x8e, kvm_lapic_state);
ioctl_ior_nr!(KVM_GET_PIT2, KVMIO, 0x9f, kvm_pit_state2);
ioctl_ior_nr!(KVM_GET_CLOCK, KVMIO, 0x7c, kvm_clock_data);
ioctl_iow_nr!(KVM_IRQ_LINE, KVMIO, 0x61, kvm_irq_level);
// KVM_GET_NESTED_STATE has no fixed-size struct (it's a FAM, sized by
// the caller's buffer) and is not encoded by these macros anywhere in
// kvm-ioctls or kvm-bindings either; vcpu.rs already carries its raw
// number (0xc080_aebe, taken from the kernel header) for the same
// reason. Kept here as a plain constant so both sides of this filter
// read from one inventory.
const KVM_GET_NESTED_STATE_NR: u32 = 0xc080_aebe;

/// The run loop's syscall allowlist (steady-state, applied once).
/// Wider than "just KVM_RUN" because freeze can happen at any point
/// (in response to SIGUSR1) and needs ordinary filesystem syscalls to
/// write the sidecar file. Each entry is commented with why it's
/// there; if you can't explain why a syscall is present, it's a
/// candidate for tightening.
#[rustfmt::skip]
pub const ALLOWED: &[Entry] = &[
    (0,   "read: serial input path, tap RX"),
    (7,   "poll: tap RX readiness check after each VM exit (poll_net_rx)"),
    (1,   "write: serial output, tap TX"),
    (3,   "close: dropping fds (block/tap on shutdown)"),
    (5,   "fstat: File::metadata (ram/state file sizing)"),
    (332, "statx: fs::create_dir_all confirming an existing path is a directory, on some libc versions"),
    (8,   "lseek: buffered reads of the state file on thaw"),
    (9,   "mmap: guest RAM mapping, allocator growth"),
    (10,  "mprotect: allocator, guard pages"),
    (11,  "munmap: allocator shrink, region cleanup"),
    (12,  "brk: allocator"),
    (13,  "rt_sigaction: installing the freeze-request handler"),
    (14,  "rt_sigprocmask: signal mask save/restore around handler install"),
    (15,  "rt_sigreturn: returning from the freeze-request signal handler"),
    (131, "sigaltstack: glibc/Rust runtime thread teardown when exiting after a freeze"),
    (cella_libs::seccomp::SYS_IOCTL, "ioctl: KVM_RUN and the freeze path's KVM_GET_* only -- see KVM_IOCTL_REQUESTS"),
    (17,  "pread64: virtio-blk reads"),
    (18,  "pwrite64: virtio-blk writes"),
    (21,  "access: std::fs path checks on some libc versions"),
    (28,  "madvise: MADV_DONTDUMP / MADV_DONTNEED hygiene calls"),
    (39,  "getpid: used by some libc allocators/signal paths"),
    (72,  "fcntl: setting O_NONBLOCK on the tap fd"),
    (79,  "getcwd: relative path resolution in std::fs"),
    (83,  "mkdir: fs::create_dir_all for the freeze image directory"),
    (202, "futex: glibc malloc arena locking, even single-threaded"),
    (218, "set_tid_address: glibc thread setup, runs once at startup"),
    (231, "exit_group: process exit, including after a freeze"),
    (257, "openat: opening kernel/disk/tap/state files"),
    (258, "mkdirat: fs::create_dir_all on some libc versions"),
    (293, "pipe2: not currently used, reserved for a future self-pipe"),
    (302, "prlimit64: std::fs / allocator introspection on some libcs"),
    (318, "getrandom: glibc/Rust runtime init, and ledger::uuid7's random fill"),
    // The vDSO serves clock_gettime on most hosts, and the syscall then
    // never reaches this filter. Inside a guest, kvm-clock without
    // PVCLOCK_TSC_STABLE_BIT makes the vDSO refuse, and glibc falls
    // back to the real syscall. The timing instrumentation of the
    // freeze and the thaw reads the clock, thus a cella that runs
    // inside a cella guest dies with SIGSYS in do_freeze without this
    // entry. probe-inception found this.
    (228, "clock_gettime: freeze/thaw timing instrumentation; vDSO fallback inside a guest"),
    (288, "accept4: the console socket client; the listener binds before this filter, and socket(2) stays the canary"),
    (45,  "recvfrom: std reads a unix stream with recv, not read (console client input)"),
    (44,  "sendto: std writes a unix stream with send, not write (console client output)"),
    (82,  "rename: atomic state.tmp -> state"),
    (87,  "unlink: finalize_thaw removing the one-shot state file, on some libc versions"),
    (263, "unlinkat: finalize_thaw removing the one-shot state file"),
    (74,  "fsync: state file and freeze-image directory durability"),
    // The chain (1.6.14d) reaches the run loop: every ledger event
    // appends through append_chained, which flocks the book and
    // reads the last entry before it writes the next link. Without
    // this entry the first park kills the VMM -- the cross-lane
    // leak the merge order exists to catch, caught.
    (73,  "flock: ledger::append_chained's exclusive lock on the event book"),
    (268, "fchmodat: the lab console socket's group bits (0770) after bind -- the ACL mask fix of 1.6.14a; dead code in the field flavor, whose console does not exist"),
    (26,  "msync: guest RAM durability before the state sidecar is written"),
];

/// The exact KVM ioctl request numbers the run loop issues once this
/// filter is live: KVM_RUN every pass, and the freeze path's KVM_GET_*
/// reads of the live vCPU/VM state (see the module doc for the full
/// accounting -- everything KVM-shaped that happens earlier runs
/// before install() and is outside this table by construction).
/// Anything else on `ioctl` -- any other KVM request, and definitely
/// anything not KVM at all -- kills the process (the gvisor shape,
/// stricter than allowing the syscall outright).
pub fn kvm_ioctl_requests() -> Vec<Entry> {
    vec![
        (KVM_RUN() as u32, "KVM_RUN: every pass of the run loop"),
        (
            KVM_IRQ_LINE() as u32,
            "KVM_IRQ_LINE: every device interrupt -- serial output and \
             virtio kicks raise legacy IRQs through VmFd::set_irq_line \
             on each run-loop pass (mmio.rs, serial.rs); the first \
             guest boot line dies without it",
        ),
        (
            KVM_GET_REGS() as u32,
            "KVM_GET_REGS: do_freeze -> vcpu::save",
        ),
        (
            KVM_GET_SREGS() as u32,
            "KVM_GET_SREGS: do_freeze -> vcpu::save",
        ),
        (
            KVM_GET_MSRS() as u32,
            "KVM_GET_MSRS: do_freeze -> vcpu::save, and vcpu::save_nested_msrs",
        ),
        (KVM_GET_FPU() as u32, "KVM_GET_FPU: do_freeze -> vcpu::save"),
        (
            KVM_GET_MP_STATE() as u32,
            "KVM_GET_MP_STATE: do_freeze -> vcpu::save",
        ),
        (
            KVM_GET_VCPU_EVENTS() as u32,
            "KVM_GET_VCPU_EVENTS: do_freeze -> vcpu::save",
        ),
        (
            KVM_GET_XSAVE() as u32,
            "KVM_GET_XSAVE: do_freeze -> vcpu::save",
        ),
        (
            KVM_GET_XCRS() as u32,
            "KVM_GET_XCRS: do_freeze -> vcpu::save",
        ),
        (
            KVM_GET_TSC_KHZ() as u32,
            "KVM_GET_TSC_KHZ: do_freeze reads vcpu_fd.get_tsc_khz()",
        ),
        (
            0x5421,
            "FIONBIO: std's set_nonblocking on an accepted console \
             client (main.rs, after accept4) issues this ioctl, not \
             fcntl. The one non-KVM request in the table, and it \
             exists only in the lab flavor -- the field VMM has no \
             console listener to accept on. strace-proven 2026-09-02.",
        ),
        (
            KVM_GET_LAPIC() as u32,
            "KVM_GET_LAPIC: do_freeze -> vcpu::save (the local APIC state; strace-proven 2026-09-02)",
        ),
        (
            KVM_GET_IRQCHIP() as u32,
            "KVM_GET_IRQCHIP: do_freeze -> vcpu::save_irqchip",
        ),
        (
            KVM_GET_PIT2() as u32,
            "KVM_GET_PIT2: do_freeze -> vcpu::save_irqchip",
        ),
        (
            KVM_GET_CLOCK() as u32,
            "KVM_GET_CLOCK: do_freeze -> vcpu::save_vm_clock",
        ),
        (
            KVM_GET_NESTED_STATE_NR,
            "KVM_GET_NESTED_STATE: do_freeze -> vcpu::save_nested, a raw ioctl (FAM struct, no fixed size)",
        ),
    ]
}

pub fn install() -> io::Result<()> {
    let requests = kvm_ioctl_requests();
    cella_libs::seccomp::install(ALLOWED, Some(&requests))
}

/// Self-test hook used by `make test-seccomp`: see
/// `cella_libs::seccomp::selftest_provoke_kill`.
pub fn selftest_provoke_kill() -> ! {
    let requests = kvm_ioctl_requests();
    cella_libs::seccomp::selftest_provoke_kill(ALLOWED, Some(&requests))
}

/// Self-test hook for the KVM ioctl filter alone: proves a request
/// outside the KVM_* table above kills the process even though
/// `ioctl` itself is allowed. Used by `make test-seccomp-vmm-kvm`.
pub fn selftest_provoke_ioctl_kill() -> ! {
    let requests = kvm_ioctl_requests();
    cella_libs::seccomp::selftest_provoke_ioctl_kill(ALLOWED, &requests)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn no_duplicate_syscalls_in_allowed() {
        let all: Vec<u32> = ALLOWED.iter().map(|(n, _)| *n).collect();
        let unique: HashSet<u32> = all.iter().copied().collect();
        assert_eq!(all.len(), unique.len(), "duplicate syscall in ALLOWED");
    }

    #[test]
    fn no_duplicate_kvm_requests() {
        let requests = kvm_ioctl_requests();
        let all: Vec<u32> = requests.iter().map(|(n, _)| *n).collect();
        let unique: HashSet<u32> = all.iter().copied().collect();
        assert_eq!(all.len(), unique.len(), "duplicate KVM ioctl request");
    }

    /// KVM_RUN itself has no direction bits (it's a plain _IO), so
    /// it must not collide with a syscall number or with 0 by
    /// accident -- a sanity floor, not a real risk given how these
    /// constants are computed, but cheap to assert.
    #[test]
    fn kvm_run_is_nonzero() {
        assert_ne!(KVM_RUN(), 0);
    }
}
