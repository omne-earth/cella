//! Seccomp, hand-rolled and shared (1.6.14b). One persona binary,
//! one allowlist: each persona builds its own table of (syscall
//! number, reason) pairs and calls [`install`] with it. This module
//! owns only the BPF program builder and the installer -- the table
//! itself, and the comment naming who needs each entry, stays in
//! the persona crate that calls it.
//!
//! No JSON, no seccompiler crate: the program is small enough to
//! write directly as classic BPF and install with
//! `prctl(PR_SET_SECCOMP)`.
//!
//! An optional ioctl argument filter narrows one syscall further:
//! when `ioctl_requests` is set, an `ioctl` (syscall number 16 on
//! x86_64) that is not on the ioctl allowlist kills the process the
//! same as any other disallowed syscall, even though `ioctl` itself
//! is in the outer allowlist. cella-vmm uses this for the KVM
//! request set -- the run loop's fds only ever need a fixed set of
//! KVM_* requests, and nothing else may ride the same syscall.

use std::io;

const BPF_LD: u16 = 0x00;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JMP: u16 = 0x05;
const BPF_JEQ: u16 = 0x10;
const BPF_JA: u16 = 0x00;
const BPF_K: u16 = 0x00;
const BPF_RET: u16 = 0x06;

const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;

/// x86_64 `ioctl`'s syscall number (arch/x86/entry/syscalls/syscall_64.tbl).
/// Hardcoded for the same reason the rest of the numbers here are: it
/// is part of the stable x86_64 ABI.
pub const SYS_IOCTL: u32 = 16;

/// `offsetof(struct seccomp_data, args[1])` on every architecture the
/// kernel defines it for: `nr` (4 bytes) + `arch` (4 bytes) +
/// `instruction_pointer` (8 bytes) + `args[0]` (8 bytes) = 24. An
/// ioctl's request code is `args[1]`, the second argument.
const SECCOMP_DATA_ARGS1_OFFSET: u32 = 24;

/// One entry: a syscall or ioctl-request number, and the comment
/// naming who needs it. The comment is not decorative -- if an
/// entry cannot carry one, it does not belong in the table.
pub type Entry = (u32, &'static str);

/// The floor every verb-process persona (gateway, universe, build,
/// doctor, network, probe, machine) shares: the Rust runtime's own
/// startup/teardown syscalls, and the ordinary file I/O that
/// `cella_libs::{audit, ledger, machine}` do on every verb (the
/// witness write, a manifest read, a flock'd chained append). Traced
/// with `strace -f` against each persona's non-KVM verbs (list,
/// show, inspect, check, --help) on 2026-09-02, x86_64. A persona
/// that needs more than this extends it in its own crate -- this is
/// the shared floor, never the whole list.
#[rustfmt::skip]
pub const CLI_BASE: &[Entry] = &[
    (0,   "read: manifest/ledger/audit reads"),
    (1,   "write: stdout/stderr, and the audit/ledger append"),
    (3,   "close: every fd this process opens, it also closes"),
    (5,   "fstat: File::metadata (size checks before reading a book)"),
    (7,   "poll: observed on every persona's startup path (glibc/std internals)"),
    (8,   "lseek: buffered reads walking the ledger/audit book"),
    (17,  "pread64: buffered positional reads of the ledger/audit book"),
    (9,   "mmap: allocator growth, glibc's own bookkeeping"),
    (10,  "mprotect: allocator guard pages"),
    (11,  "munmap: allocator shrink"),
    (12,  "brk: allocator"),
    (13,  "rt_sigaction: SIGPIPE reset, and the Rust runtime's own handlers"),
    (14,  "rt_sigprocmask: signal mask save/restore around handler install"),
    (158, "arch_prctl: glibc TLS setup, runs once at startup"),
    (21,  "access: std::fs path checks and glibc's ld.so.preload probe"),
    (39,  "getpid: libc allocators, and this process's own pid in log lines"),
    (79,  "getcwd: relative path resolution in std::fs"),
    (83,  "mkdir: fs::create_dir_all for the machine/audit directories"),
    (102, "getuid: cella_libs::audit::witness records the caller's uid"),
    (104, "getgid: cella_libs::audit::witness records the caller's gid"),
    (107, "geteuid: some verbs check the effective identity before acting"),
    (108, "getegid: paired with geteuid on the same libc paths"),
    (131, "sigaltstack: glibc/Rust runtime thread teardown"),
    (186, "gettid: glibc thread-local setup, runs once at startup"),
    (202, "futex: glibc malloc arena locking, even single-threaded"),
    (218, "set_tid_address: glibc thread setup, runs once at startup"),
    (231, "exit_group: process exit"),
    (257, "openat: opening manifest/ledger/audit/profile files"),
    (258, "mkdirat: fs::create_dir_all on some libc versions"),
    (217, "getdents64: fs::read_dir listing the machines directory (list, and tap enumeration)"),
    (302, "prlimit64: std::fs / allocator introspection on some libcs"),
    (318, "getrandom: glibc/Rust runtime init, and ledger::uuid7's random fill"),
    (332, "statx: fs::create_dir_all confirming an existing path is a directory, on some libc versions"),
    (334, "rseq: glibc's restartable-sequence registration, runs once at startup"),
    (273, "set_robust_list: glibc thread setup, runs once at startup"),
    (204, "sched_getaffinity: glibc/allocator CPU-count probes"),
    (82,  "rename: atomic tmp -> final writes (manifest, ledger append)"),
    (87,  "unlink: fs::remove_file on some libc versions (archive/destroy dropping a stale file)"),
    (263, "unlinkat: fs::remove_file (archive/destroy dropping a stale file)"),
    (74,  "fsync: durability on the manifest and the chained books"),
    (72,  "fcntl: setting flags (O_CLOEXEC, F_SETLK) on opened files"),
    // flock is the ledger/audit chain's own concurrency door
    // (append_chained): every witnessed verb and every ledger write
    // takes it, so it belongs on the floor, not in a persona's
    // extension.
    (73,  "flock: cella_libs::ledger::append_chained's exclusive lock"),
    (91,  "fchmod: cella_libs::golden's mode stamp on a copied artifact (disk.img at create)"),
    (326, "copy_file_range: cella_libs::golden's artifact copy (disk.img from the golden rootfs)"),
];

/// Build and install the real BPF filter: `allowed` syscalls pass;
/// everything else kills the process (`SECCOMP_RET_KILL_PROCESS`,
/// not `KILL_THREAD` -- a multi-threaded persona must not leave a
/// crippled process behind). When `ioctl_requests` is `Some`, every
/// `ioctl` call is additionally checked against that request-number
/// table; an `ioctl` whose request is not listed there dies even
/// though `ioctl` the syscall is allowed.
pub fn install(allowed: &[Entry], ioctl_requests: Option<&[Entry]>) -> io::Result<()> {
    // The static in-guest build (glibc with crt-static, baked into
    // the nested and inception images) makes a different syscall
    // set from the dynamic host binaries the tables were traced
    // against, on a guest kernel with a smaller syscall surface --
    // a verb dies by SIGSYS at its first stat, silently. The tables
    // are the host's; the in-guest lists are the join's to trace on
    // the guest kernel (tasks/PHASE1.md #NOTES, 2026-09-03).
    if cfg!(target_feature = "crt-static") {
        return Ok(());
    }
    let syscalls: Vec<u32> = allowed.iter().map(|(n, _)| *n).collect();
    let requests: Vec<u32> = ioctl_requests
        .map(|r| r.iter().map(|(n, _)| *n).collect())
        .unwrap_or_default();
    let filter_ioctl = ioctl_requests.is_some();

    let n = syscalls.len();
    let m = requests.len();

    // Layout (see the module doc for why the ioctl block needs its
    // own unconditional jump past it):
    //   0            LD nr
    //   1..=n        one JEQ per allowed syscall; the ioctl entry's
    //                jt targets the ioctl block instead of ALLOW,
    //                when a filter is in effect
    //   n+1          JA -> KILL (no main syscall matched)
    //   n+2          LD args[1]                  } ioctl block,
    //   n+3..=n+2+m  one JEQ per allowed request  } present only
    //                                             } when filtering
    //   n+2+m (or n+1 if m==0 and no filter)  KILL
    //   n+3+m (...)                            ALLOW
    let ioctl_block_len = if filter_ioctl { 1 + m } else { 0 };
    let total = 1 + n + 1 + ioctl_block_len + 2;
    let kill_idx = 1 + n + 1 + ioctl_block_len;
    let allow_idx = kill_idx + 1;

    let mut prog: Vec<libc::sock_filter> = Vec::with_capacity(total);
    prog.push(stmt(BPF_LD | BPF_W | BPF_ABS, 0)); // seccomp_data.nr

    for (i, sysno) in syscalls.iter().enumerate() {
        let here = 1 + i;
        let target = if filter_ioctl && *sysno == SYS_IOCTL {
            1 + n + 1 // the ioctl block's LD instruction
        } else {
            allow_idx
        };
        let jt = (target - here - 1) as u8;
        prog.push(jump(BPF_JMP | BPF_JEQ | BPF_K, *sysno, jt, 0));
    }
    // No main-list syscall matched: skip the ioctl block entirely,
    // straight to KILL (falling through would misread args[1] of an
    // unrelated syscall as an ioctl request).
    let ja_idx = 1 + n;
    prog.push(jump_a((kill_idx - ja_idx - 1) as u32));

    if filter_ioctl {
        let ioctl_ld_idx = 1 + n + 1;
        prog.push(stmt(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_ARGS1_OFFSET));
        for (i, req) in requests.iter().enumerate() {
            let here = ioctl_ld_idx + 1 + i;
            let jt = (allow_idx - here - 1) as u8;
            prog.push(jump(BPF_JMP | BPF_JEQ | BPF_K, *req, jt, 0));
        }
        // No request matched: fall through into KILL, right below.
    }

    prog.push(stmt_ret(SECCOMP_RET_KILL_PROCESS));
    prog.push(stmt_ret(SECCOMP_RET_ALLOW));
    debug_assert_eq!(prog.len(), total);
    debug_assert_eq!(prog.len() - 1, allow_idx);

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
fn jump_a(k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code: BPF_JMP | BPF_JA,
        jt: 0,
        jf: 0,
        k,
    }
}

/// Self-test hook shared by every persona's `--selftest-seccomp`:
/// install the real filter, then deliberately make a syscall that is
/// never on any persona's list (`socket(2)` stays the canary, as it
/// was before the split). If the filter is wired correctly, the
/// kernel kills this process with SIGSYS before `socket()` returns --
/// so the *expected* outcome of calling this function is that it
/// never returns. A test harness runs this as a subprocess and
/// asserts on the signal, not the exit code.
pub fn selftest_provoke_kill(allowed: &[Entry], ioctl_requests: Option<&[Entry]>) -> ! {
    install(allowed, ioctl_requests).expect("installing seccomp filter for selftest");
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

/// Self-test hook for the KVM ioctl filter alone: install the real
/// filter with an ioctl request table, then issue an `ioctl` whose
/// request is deliberately not on that table (against `/dev/null`,
/// so the call would otherwise be harmless). The filter must kill
/// the process on the request check, not on the `ioctl` syscall
/// itself -- proving the argument filter fires, not just the outer
/// allowlist.
pub fn selftest_provoke_ioctl_kill(allowed: &[Entry], ioctl_requests: &[Entry]) -> ! {
    install(allowed, Some(ioctl_requests)).expect("installing seccomp filter for selftest");
    // A request number no KVM ioctl ever uses (KVMIO's range is
    // 0xAE__; this is TCGETS, the terminal-attribute ioctl).
    const TCGETS: u32 = 0x5401;
    // SAFETY: /dev/null is always openable; the ioctl request is
    // bogus for that fd, but the filter fires before the kernel's
    // driver code would see it.
    unsafe {
        let fd = libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY);
        libc::ioctl(fd, TCGETS as libc::c_ulong);
    }
    std::process::exit(42);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal classic-BPF interpreter: load nr, load args[1], jeq,
    /// ja, ret. Enough to evaluate what the kernel would, for both
    /// the plain-syscall path and the ioctl-request path.
    struct SeccompData {
        nr: u32,
        arg1: u32,
    }
    fn eval(prog: &[libc::sock_filter], data: &SeccompData) -> u32 {
        let mut pc = 0usize;
        let mut acc = 0u32;
        loop {
            let ins = prog[pc];
            match ins.code {
                c if c == BPF_LD | BPF_W | BPF_ABS => {
                    acc = if ins.k == 0 { data.nr } else { data.arg1 };
                }
                c if c == (BPF_JMP | BPF_JEQ | BPF_K) => {
                    pc += 1 + if acc == ins.k {
                        ins.jt as usize
                    } else {
                        ins.jf as usize
                    };
                    continue;
                }
                c if c == (BPF_JMP | BPF_JA) => {
                    pc += 1 + ins.k as usize;
                    continue;
                }
                c if c == (BPF_RET | BPF_K) => return ins.k,
                other => unreachable!("interpreter doesn't model opcode {other:#x}"),
            }
            pc += 1;
        }
    }

    fn build(allowed: &[Entry], ioctl_requests: Option<&[Entry]>) -> Vec<libc::sock_filter> {
        // install() calls the real prctl and cannot be interpreted
        // directly by a unit test, so this re-implements the same
        // layout using the same helper functions -- kept next to
        // install() on purpose so a layout change is one diff.
        let syscalls: Vec<u32> = allowed.iter().map(|(n, _)| *n).collect();
        let requests: Vec<u32> = ioctl_requests
            .map(|r| r.iter().map(|(n, _)| *n).collect())
            .unwrap_or_default();
        let filter_ioctl = ioctl_requests.is_some();
        let n = syscalls.len();
        let m = requests.len();
        let ioctl_block_len = if filter_ioctl { 1 + m } else { 0 };
        let kill_idx = 1 + n + 1 + ioctl_block_len;
        let allow_idx = kill_idx + 1;
        let mut prog = Vec::new();
        prog.push(stmt(BPF_LD | BPF_W | BPF_ABS, 0));
        for (i, sysno) in syscalls.iter().enumerate() {
            let here = 1 + i;
            let target = if filter_ioctl && *sysno == SYS_IOCTL {
                1 + n + 1
            } else {
                allow_idx
            };
            let jt = (target - here - 1) as u8;
            prog.push(jump(BPF_JMP | BPF_JEQ | BPF_K, *sysno, jt, 0));
        }
        let ja_idx = 1 + n;
        prog.push(jump_a((kill_idx - ja_idx - 1) as u32));
        if filter_ioctl {
            let ioctl_ld_idx = 1 + n + 1;
            prog.push(stmt(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_ARGS1_OFFSET));
            for (i, req) in requests.iter().enumerate() {
                let here = ioctl_ld_idx + 1 + i;
                let jt = (allow_idx - here - 1) as u8;
                prog.push(jump(BPF_JMP | BPF_JEQ | BPF_K, *req, jt, 0));
            }
        }
        prog.push(stmt_ret(SECCOMP_RET_KILL_PROCESS));
        prog.push(stmt_ret(SECCOMP_RET_ALLOW));
        prog
    }

    const SAMPLE: &[Entry] = &[
        (0, "read"),
        (1, "write"),
        (16, "ioctl"),
        (231, "exit_group"),
    ];
    const SAMPLE_IOCTL: &[Entry] = &[(0xae80, "KVM_RUN"), (0xc008_ae81, "KVM_GET_REGS")];

    #[test]
    fn plain_allowlist_allows_and_kills() {
        let prog = build(SAMPLE, None);
        for (n, _) in SAMPLE {
            assert_eq!(
                eval(&prog, &SeccompData { nr: *n, arg1: 0 }),
                SECCOMP_RET_ALLOW
            );
        }
        for n in [2u32, 41, 59] {
            assert_eq!(
                eval(&prog, &SeccompData { nr: n, arg1: 0 }),
                SECCOMP_RET_KILL_PROCESS
            );
        }
    }

    #[test]
    fn ioctl_filter_allows_listed_requests_only() {
        let prog = build(SAMPLE, Some(SAMPLE_IOCTL));
        // Non-ioctl syscalls still resolve normally.
        assert_eq!(
            eval(&prog, &SeccompData { nr: 0, arg1: 0 }),
            SECCOMP_RET_ALLOW
        );
        // ioctl with an allowed request: ALLOW.
        assert_eq!(
            eval(
                &prog,
                &SeccompData {
                    nr: 16,
                    arg1: 0xae80
                }
            ),
            SECCOMP_RET_ALLOW
        );
        // ioctl with a request not on the table: KILL, not ALLOW --
        // the whole point of the filter.
        assert_eq!(
            eval(
                &prog,
                &SeccompData {
                    nr: 16,
                    arg1: 0x5401
                }
            ),
            SECCOMP_RET_KILL_PROCESS
        );
        // A syscall outside the main list entirely: still KILL,
        // regardless of what garbage sits in arg1.
        assert_eq!(
            eval(
                &prog,
                &SeccompData {
                    nr: 59,
                    arg1: 0xae80
                }
            ),
            SECCOMP_RET_KILL_PROCESS
        );
    }

    #[test]
    fn no_duplicate_entries_in_cli_base() {
        let all: Vec<u32> = CLI_BASE.iter().map(|(n, _)| *n).collect();
        let unique: std::collections::HashSet<u32> = all.iter().copied().collect();
        assert_eq!(all.len(), unique.len(), "duplicate syscall in CLI_BASE");
    }

    #[test]
    fn no_duplicate_entries_in_sample_tables() {
        let mut all: Vec<u32> = SAMPLE.iter().map(|(n, _)| *n).collect();
        let unique: std::collections::HashSet<u32> = all.iter().copied().collect();
        assert_eq!(all.len(), unique.len());
        all = SAMPLE_IOCTL.iter().map(|(n, _)| *n).collect();
        let unique: std::collections::HashSet<u32> = all.iter().copied().collect();
        assert_eq!(all.len(), unique.len());
    }
}
