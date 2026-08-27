//! Seccomp, hand-rolled.
//!
//! No JSON, no seccompiler crate: this is small enough (one syscall-number
//! allowlist, no argument filtering) to write directly as classic BPF and
//! install with `prctl(PR_SET_SECCOMP)`.
//!
//! The list below is the steady-state set, applied once right before the
//! run loop -- after every file has been opened and every KVM object has
//! been created. It's wider than "just KVM_RUN" because freeze can happen
//! at any point during the loop (in response to SIGUSR1) and needs
//! ordinary filesystem syscalls to write the sidecar file. Each entry is
//! commented with why it's there; if you can explain why a syscall is
//! present, it's a candidate for tightening (e.g. filtering `ioctl` on
//! `args[1]` to the exact KVM request codes this VMM issues).

use std::io;

const BPF_LD: u16 = 0x00;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JMP: u16 = 0x05;
const BPF_JEQ: u16 = 0x10;
const BPF_K: u16 = 0x00;
const BPF_RET: u16 = 0x06;

const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;

// x86_64 syscall numbers (from arch/x86/entry/syscalls/syscall_64.tbl).
// Hardcoded rather than pulled from a crate: there are ~25 of them and
// they are part of the stable x86_64 ABI.
#[rustfmt::skip]
const ALLOWED: &[(u32, &str)] = &[
    (0,   "read: serial input path, tap RX"),
    (1,   "write: serial output, tap TX"),
    (3,   "close: dropping fds (block/tap on shutdown)"),
    (5,   "fstat: File::metadata (ram/state file sizing)"),
    (8,   "lseek: buffered reads of the state file on thaw"),
    (9,   "mmap: guest RAM mapping, allocator growth"),
    (10,  "mprotect: allocator, guard pages"),
    (11,  "munmap: allocator shrink, region cleanup"),
    (12,  "brk: allocator"),
    (13,  "rt_sigaction: installing the freeze-request handler"),
    (14,  "rt_sigprocmask: signal mask save/restore around handler install"),
    (15,  "rt_sigreturn: returning from the freeze-request signal handler"),
    (16,  "ioctl: every KVM_* and the one-time TUNSETIFF"),
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
    (318, "getrandom: glibc/Rust runtime init"),
    (82,  "rename: atomic state.tmp -> state"),
];
const SYNC_SYSCALLS: &[(u32, &str)] = &[(
    74,
    "fsync: state file and freeze-image directory durability",
)];

pub fn install() -> io::Result<()> {
    let mut allowed: Vec<u32> = ALLOWED.iter().map(|(n, _)| *n).collect();
    allowed.extend(SYNC_SYSCALLS.iter().map(|(n, _)| *n));

    let n = allowed.len();
    let mut prog: Vec<libc::sock_filter> = Vec::with_capacity(n + 3);
    prog.push(stmt(BPF_LD | BPF_W | BPF_ABS, 0)); // offsetof(seccomp_data, nr) == 0

    for (i, sysno) in allowed.iter().enumerate() {
        let jt = (n - i) as u8; // distance to the ALLOW instruction
        prog.push(jump(BPF_JMP | BPF_JEQ | BPF_K, *sysno, jt, 0));
    }
    prog.push(stmt_ret(SECCOMP_RET_KILL_PROCESS));
    prog.push(stmt_ret(SECCOMP_RET_ALLOW));

    let fprog = libc::sock_fprog {
        len: prog.len() as u16,
        filter: prog.as_mut_ptr(),
    };

    // SAFETY: `fprog` points at `prog`, which is alive until this
    // function returns; prctl copies it into the kernel synchronously.
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &fprog as *const _,
        ) != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn stmt(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}
fn stmt_ret(k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k,
    }
}
fn jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

/// Self-test hook used by `make test-seccomp`: install the real filter,
/// then deliberately make a disallowed syscall (`socket(2)`, not in
/// `ALLOWED`). If the filter is wired correctly, the kernel kills this
/// process with SIGSYS before `socket()` returns -- so the *expected*
/// outcome of calling this function is that it never returns. A test
/// harness runs this as a subprocess and asserts on the signal, not the
/// exit code.
pub fn selftest_provoke_kill() -> ! {
    install().expect("installing seccomp filter for selftest");
    // SAFETY: deliberately calling a forbidden syscall to verify the
    // filter kills us. socket() takes plain integer arguments; nothing
    // here can be unsound, only refused by the kernel.
    unsafe {
        libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
    }
    // Only reachable if the filter failed to kill us -- exit distinctly
    // from a normal 0/1 so the harness can tell "filter didn't fire"
    // apart from an unrelated crash.
    std::process::exit(42);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The BPF program itself: every syscall we claim to allow must
    /// resolve to the ALLOW instruction, and the jump distances must
    /// land exactly on it (an off-by-one here means either an allowed
    /// syscall falls through to KILL, or -- worse -- lands on the wrong
    /// instruction and silently permits something unintended). We can't
    /// install the real filter in a unit test (it would nuke the test
    /// process's own ability to do I/O), so this interprets the BPF
    /// program logically instead, the same way the kernel would.
    fn build_program() -> Vec<libc::sock_filter> {
        let mut allowed: Vec<u32> = ALLOWED.iter().map(|(n, _)| *n).collect();
        allowed.extend(SYNC_SYSCALLS.iter().map(|(n, _)| *n));
        let n = allowed.len();
        let mut prog = Vec::with_capacity(n + 3);
        prog.push(stmt(BPF_LD | BPF_W | BPF_ABS, 0));
        for (i, sysno) in allowed.iter().enumerate() {
            prog.push(jump(BPF_JMP | BPF_JEQ | BPF_K, *sysno, (n - i) as u8, 0));
        }
        prog.push(stmt_ret(SECCOMP_RET_KILL_PROCESS));
        prog.push(stmt_ret(SECCOMP_RET_ALLOW));
        prog
    }

    /// Minimal classic-BPF interpreter covering exactly the instruction
    /// shapes this program uses (load nr, jeq, ret). Not a general BPF
    /// VM -- just enough to check "does syscall N resolve to ALLOW."
    fn eval(prog: &[libc::sock_filter], syscall_nr: u32) -> u32 {
        let mut pc = 0usize;
        let mut acc = 0u32;
        loop {
            let ins = prog[pc];
            let class = ins.code & 0x07;
            match class {
                0x00 => acc = syscall_nr, // BPF_LD (we only ever load nr)
                0x05 => {
                    // BPF_JMP: only BPF_JEQ|BPF_K is used here.
                    pc += 1 + if acc == ins.k {
                        ins.jt as usize
                    } else {
                        ins.jf as usize
                    };
                    continue;
                }
                0x06 => return ins.k, // BPF_RET
                _ => unreachable!("test interpreter doesn't model this instruction class"),
            }
            pc += 1;
        }
    }

    #[test]
    fn every_allowed_syscall_resolves_to_allow() {
        let prog = build_program();
        let mut all = ALLOWED.iter().map(|(n, _)| *n).collect::<Vec<_>>();
        all.extend(SYNC_SYSCALLS.iter().map(|(n, _)| *n));
        for sysno in all {
            assert_eq!(
                eval(&prog, sysno),
                SECCOMP_RET_ALLOW,
                "syscall {sysno} should resolve to ALLOW"
            );
        }
    }

    #[test]
    fn unlisted_syscalls_resolve_to_kill() {
        let prog = build_program();
        let allowed: std::collections::HashSet<u32> = ALLOWED
            .iter()
            .chain(SYNC_SYSCALLS.iter())
            .map(|(n, _)| *n)
            .collect();
        // A sample spanning the syscall table, deliberately including
        // execve/ptrace/socket -- the ones a jail most needs to deny.
        for sysno in [2u32, 41, 49, 56, 57, 59, 101, 165, 175, 435] {
            assert!(!allowed.contains(&sysno), "test sample overlaps ALLOWED");
            assert_eq!(
                eval(&prog, sysno),
                SECCOMP_RET_KILL_PROCESS,
                "syscall {sysno} should resolve to KILL"
            );
        }
    }

    #[test]
    fn no_duplicate_or_conflicting_entries() {
        let mut all = ALLOWED.iter().map(|(n, _)| *n).collect::<Vec<_>>();
        all.extend(SYNC_SYSCALLS.iter().map(|(n, _)| *n));
        let unique: std::collections::HashSet<u32> = all.iter().copied().collect();
        assert_eq!(
            all.len(),
            unique.len(),
            "duplicate syscall number in the allowlist (harmless but a sign of a copy/paste error)"
        );
    }
}
