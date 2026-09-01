//! Verifies the guest's own wall-clock time (its computed CLOCK_REALTIME,
//! reported via rootfs-init.sh's periodic heartbeat -- see that script)
//! lands close to the host's real time shortly after boot.
//!
//! This is the direct test for "mitigation 1" from the freeze/thaw
//! clock design discussion: with no working RTC device, does the guest
//! actually pick up a correct wall-clock seed from kvmclock's own
//! wall-clock MSR data, or is its clock meaningless from the moment it
//! boots?
//!
//! The drift number alone can't tell you *why* it failed, so this probe
//! also scrapes the guest's own boot messages for the three lines that
//! separate the candidate mechanisms (see `Evidence`):
//!
//!   - no `kvm-clock:` lines at all  -> the guest never enabled kvmclock
//!     (missing CONFIG_KVM_GUEST, or the 0x4000_0000 CPUID leaves never
//!     reached it). Nothing about the RTC config would change that.
//!   - kvm-clock present but drift huge -> kvmclock is live and the
//!     wall-clock *seed* specifically is what's broken.
//!
//! That distinction is the whole point of running this rather than
//! reasoning about which x86_platform hook wins.
//!
//! It reports the guest's RNG seeding ("random: crng init done") from
//! the same log for the same reason: it is cheap to read off a boot we
//! are doing anyway, and a kernel-config trim that broke entropy
//! seeding would otherwise stay invisible until something blocked.
//!
//! Parameters. The Makefile sets each one to the default of cella, and
//! the same defaults are in this file, thus a direct run of the binary
//! behaves in the same way.
//!
//!   CELLA_OBSERVE_SECS     The length of the control test, in real
//!                          seconds. The probe keeps the guest running
//!                          with no freeze and reports the clock errors
//!                          of the kernel. Default 60, which is
//!                          approximately 120 rounds of the clocksource
//!                          watchdog. Use 0 to omit the control test.
//!   CELLA_BIN, CELLA_TEST_KERNEL, CELLA_TEST_DISK, CELLA_TEST_TAP
//!                          The same meaning as in the smoke tests.
//!
//! Run: cargo run --manifest-path probes/wallclock/Cargo.toml
//! (needs the canonical goldens -- `make golden` -- and a
//! configured tap0 -- `make setup-tap`)

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TIMEOUT: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Format a duration in nanoseconds as "<ns> ns (<s> s)". The value in
/// seconds is the same number, exact, with nine decimals.
fn fmt_ns(ns: i128) -> String {
    let sign = if ns < 0 { "-" } else { "" };
    let a = ns.unsigned_abs();
    format!(
        "{ns} ns ({sign}{}.{:09} s)",
        a / 1_000_000_000,
        a % 1_000_000_000
    )
}

/// The same as fmt_ns, and the sign is always written. Use it for a
/// difference.
fn fmt_ns_signed(ns: i128) -> String {
    let sign = if ns < 0 { "-" } else { "+" };
    let a = ns.unsigned_abs();
    format!(
        "{sign}{a} ns ({sign}{}.{:09} s)",
        a / 1_000_000_000,
        a % 1_000_000_000
    )
}

fn repo_root() -> PathBuf {
    // The probe modules compile inside the main package, thus the
    // manifest directory is the repo root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// The cella binary: CELLA_BIN, else the sibling of this probe, else
/// target/release/cella under the repo root.
fn sibling_cella(root: &std::path::Path) -> PathBuf {
    if let Ok(me) = std::env::current_exe() {
        let p = me.parent().unwrap().join("cella");
        if p.is_file() {
            return p;
        }
    }
    root.join("target/release/cella")
}

fn golden(axis: &str, flavor: &str, file: &str) -> PathBuf {
    let home = std::env::var("CELLA_HOME").unwrap_or_else(|_| {
        format!(
            "{}/.cella",
            std::env::var("HOME").unwrap_or_else(|_| ".".into())
        )
    });
    PathBuf::from(home).join(axis).join(flavor).join(file)
}

fn env_path(var: &str, default: PathBuf) -> PathBuf {
    std::env::var(var).map(PathBuf::from).unwrap_or(default)
}

/// Parse the epoch seconds out of a "cella-rootfs: wall-clock <N>" line.
/// Read a nanosecond field from a heartbeat line. The line is
/// "cella-rootfs: wall-clock <s> uptime <s> mono_ns <ns> real_ns <ns>".
/// A field is absent when the guest cannot produce it, thus the caller
/// must handle None.
fn parse_ns(line: &str, field: &str) -> Option<u64> {
    let (_, rest) = line.split_once(&format!(" {field} "))?;
    rest.split_whitespace().next()?.parse().ok()
}

fn parse_heartbeat(line: &str) -> Option<i64> {
    // The line is "cella-rootfs: wall-clock <epoch> uptime <seconds>".
    // Read the first field only.
    line.strip_prefix("cella-rootfs: wall-clock ")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Serial output is not guaranteed UTF-8: the guest console emits the
/// odd stray byte (a 0xff turns up right at the 8250 driver's init),
/// and `read_to_string` fails the *entire* read on one bad byte,
/// silently yielding "". That is not hypothetical -- it produced a
/// probe run reporting "no heartbeat" and "no kvm-clock evidence"
/// against an 18KB log that plainly contained both. Decode lossily.
fn read_log(path: &Path) -> String {
    let s = match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => return String::new(),
    };
    // The serial device writes a line byte by byte. A read can see a
    // partial line, and a number in a partial line parses to a wrong
    // value. Return complete lines only.
    match s.rfind('\n') {
        Some(i) => s[..=i].to_string(),
        None => String::new(),
    }
}

fn wait_for_heartbeat(log_path: &Path, child: &mut Child, deadline: Instant) -> Option<i64> {
    loop {
        if let Some(v) = read_log(log_path).lines().rev().find_map(parse_heartbeat) {
            return Some(v);
        }
        if let Ok(Some(_)) = child.try_wait() {
            return None; // exited before ever reporting
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// What the guest's own boot messages say about where its time comes
/// from. Scraped from the serial console because that's the only
/// channel out of the guest (no SSH, no shell): the guest kernel prints
/// exactly which mechanism it picked, so we can read it off rather than
/// infer it.
struct Evidence {
    /// "kvm-clock: Using msrs 4b564d01 and 4b564d00" -- proof the guest
    /// enabled kvmclock at all, and via which MSR pair.
    kvmclock_msrs: Vec<String>,
    /// "clocksource: Switched to clocksource kvm-clock" -- proof it's
    /// the *selected* clocksource, not merely compiled in.
    clocksource: Vec<String>,
    /// Anything RTC-related, including the failures that started this
    /// investigation ("Unable to read current time from RTC").
    rtc: Vec<String>,
    /// "random: crng init done" and friends. Not a clock question, but
    /// this probe already has a real boot log in hand and a working RNG
    /// is a standing requirement for the guest -- cella emulates no
    /// hwrng device, so the CRNG is seeded from RDRAND, and a config
    /// trim that broke that would otherwise go unnoticed until
    /// something in the guest blocked on entropy.
    rng: Vec<String>,
}

fn gather_evidence(log: &str) -> Evidence {
    let mut ev = Evidence {
        kvmclock_msrs: Vec::new(),
        clocksource: Vec::new(),
        rtc: Vec::new(),
        rng: Vec::new(),
    };
    for line in log.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        let lower = l.to_ascii_lowercase();
        if lower.contains("kvm-clock") {
            ev.kvmclock_msrs.push(l.to_string());
        }
        if lower.contains("clocksource:") {
            ev.clocksource.push(l.to_string());
        }
        if lower.contains("rtc") {
            ev.rtc.push(l.to_string());
        }
        if lower.contains("crng") || lower.contains("random:") {
            ev.rng.push(l.to_string());
        }
    }
    ev
}

fn print_section(title: &str, lines: &[String]) {
    println!("  {title}:");
    if lines.is_empty() {
        println!("    (none)");
    }
    for l in lines.iter().take(8) {
        println!("    {l}");
    }
}

fn print_evidence(ev: &Evidence) {
    println!("--- what the guest says about its own clock ---");
    print_section("kvm-clock", &ev.kvmclock_msrs);
    print_section("clocksource selection", &ev.clocksource);
    print_section("RTC", &ev.rtc);
    print_section("RNG", &ev.rng);
    println!("---");
}

fn fail(msg: &str) -> ! {
    eprintln!("FAIL: {msg}");
    std::process::exit(1);
}

/// Read a default from the cella binary. The values live in
/// src/config.rs, and this probe must not write a second copy of them.
/// The binary can print them, and the probe already requires it.
fn ask_cella(bin: &Path, flag: &str) -> String {
    let out = Command::new(bin)
        .arg(flag)
        .output()
        .unwrap_or_else(|e| fail(&format!("running {} {flag}: {e}", bin.display())));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn default_time_args(bin: &Path) -> String {
    // The full default command line contains the base arguments and the
    // time arguments. The time arguments are what this probe varies.
    ask_cella(bin, "--print-time-args")
}

fn default_base_args(bin: &Path) -> String {
    let full = ask_cella(bin, "--print-default-cmdline");
    let time = default_time_args(bin);
    full.replace(&time, "").trim().to_string()
}

pub fn run() {
    let root = repo_root();
    let bin = env_path("CELLA_BIN", sibling_cella(&root));
    let kernel = env_path(
        "CELLA_TEST_KERNEL",
        golden("kernel", "canonical", "bzImage"),
    );
    let disk = env_path(
        "CELLA_TEST_DISK",
        golden("rootfs", "canonical", "rootfs.ext4"),
    );
    let tap = std::env::var("CELLA_TEST_TAP").unwrap_or_else(|_| "tap0".to_string());

    if !bin.is_file() {
        fail(&format!("{} not built -- run: make build", bin.display()));
    }
    if !kernel.is_file() || !disk.is_file() {
        fail("test assets missing -- run: make golden");
    }

    let tmp = std::env::temp_dir().join(format!("cella-wallclock-probe-{}", std::process::id()));
    let state_dir = tmp.join("state");
    std::fs::create_dir_all(&state_dir).expect("create state dir");
    let disk_copy = tmp.join("disk.img");
    std::fs::copy(&disk, &disk_copy).expect("copy disk");
    let log_path = tmp.join("boot.log");

    // CELLA_TIME_ARGS holds the time arguments of cella. Set it to a
    // different value to compare the boot messages that each choice
    // produces.
    // An unset or empty value uses the default of cella. The word "none"
    // runs the guest with no time arguments at all, which is how the
    // behaviour that these arguments correct can be seen again.
    let time_args = match std::env::var("CELLA_TIME_ARGS") {
        Ok(v) if v.trim() == "none" => String::new(),
        Ok(v) if !v.trim().is_empty() => v,
        _ => default_time_args(&bin),
    };
    let base = default_base_args(&bin);
    let cmdline = format!(
        "{base} {time_args} root=/dev/vda rw \
         virtio_mmio.device=4K@0xd0000000:5 virtio_mmio.device=4K@0xd0001000:6"
    );
    println!(
        "time arguments: {}",
        if time_args.trim().is_empty() {
            "(none)"
        } else {
            time_args.trim()
        }
    );

    let host_before = SystemTime::now();
    let mut child = Command::new(&bin)
        .arg("--state-dir")
        .arg(&state_dir)
        .arg("--kernel")
        .arg(&kernel)
        .arg("--disk")
        .arg(&disk_copy)
        .arg("--tap")
        .arg(&tap)
        .arg("--mem-mb")
        .arg("128")
        .arg("--cmdline")
        .arg(&cmdline)
        .stdout(Stdio::from(
            File::create(&log_path).expect("create log file"),
        ))
        .stderr(Stdio::from(
            File::create(tmp.join("boot.err")).expect("create err file"),
        ))
        .spawn()
        .expect("spawn cella");

    let deadline = Instant::now() + TIMEOUT;
    let result = wait_for_heartbeat(&log_path, &mut child, deadline);
    let host_after = SystemTime::now();

    // Control test. Set CELLA_OBSERVE_SECS to keep the guest running for
    // this many seconds after the first heartbeat, with no freeze and no
    // thaw. The clocksource watchdog in the guest runs twice per second.
    // If the guest reports a clock error in this period, the error comes
    // from the host or from the guest, and not from freeze and thaw.
    let observe: u64 = std::env::var("CELLA_OBSERVE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    if observe > 0 && result.is_some() {
        println!(
            "observing the guest for {} with no freeze...",
            fmt_ns(observe as i128 * 1_000_000_000)
        );
        std::thread::sleep(Duration::from_secs(observe));
    }

    let _ = child.kill();
    let _ = child.wait();

    // Read the log *before* any cleanup: on failure it's the only
    // evidence, and it's what tells the two mechanisms apart.
    let log = read_log(&log_path);
    let ev = gather_evidence(&log);
    print_evidence(&ev);

    // Report the same kernel errors that the freeze and thaw probe
    // reports, so that the two results can be compared directly.
    let complaints: Vec<&str> = log
        .lines()
        .map(str::trim)
        .filter(|l| {
            let low = l.to_ascii_lowercase();
            low.contains("unstable") || low.contains("watchdog") || low.contains("oops")
        })
        .collect();
    if complaints.is_empty() {
        println!("kernel clock errors while running: none");
    } else {
        println!("kernel clock errors while running ({}):", complaints.len());
        for l in complaints.iter().take(6) {
            println!("  {l}");
        }
    }

    // Kept on failure deliberately -- a drift number with no serial log
    // to look at is exactly the kind of un-diagnosable result these
    // probes exist to avoid.
    let keep_for_diagnosis = || {
        eprintln!("(serial log kept for inspection: {})", log_path.display());
    };

    let guest_epoch = match result {
        Some(v) => v,
        None => {
            keep_for_diagnosis();
            println!("--- last 15 lines of serial output ---");
            for l in log.lines().rev().take(15).collect::<Vec<_>>().iter().rev() {
                println!("{l}");
            }
            fail(&format!(
                "no wall-clock heartbeat observed within {} -- the guest never \
                 reached the heartbeat loop (a boot problem), so this run says nothing \
                 either way about wall-clock seeding",
                fmt_ns(TIMEOUT.as_nanos() as i128)
            ))
        }
    };

    let host_epoch_before = host_before.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    let host_epoch_after = host_after.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    // The tolerance is zero. At a resolution of 1 s, zero drift means:
    // the epoch of the guest lies inside the host window from spawn to
    // observation. The drift is the distance to that window.
    let drift = if guest_epoch < host_epoch_before {
        host_epoch_before - guest_epoch
    } else if guest_epoch > host_epoch_after {
        guest_epoch - host_epoch_after
    } else {
        0
    };

    println!(
        "host epoch (spawn..observed): {host_epoch_before}..{host_epoch_after}  \
         guest-reported epoch: {guest_epoch}  drift: {}",
        fmt_ns(drift as i128 * 1_000_000_000)
    );

    // The same comparison in nanoseconds, when the guest can report them.
    // The probe reads the log every 100 ms, thus the age of the value is
    // up to 100 ms and that is the limit of this measurement, not the
    // resolution of the timestamp.
    if let Some(guest_ns) = log.lines().rev().find_map(|l| parse_ns(l, "real_ns")) {
        let host_ns = host_after.duration_since(UNIX_EPOCH).unwrap().as_nanos() as i128;
        let diff = guest_ns as i128 - host_ns;
        println!(
            "guest CLOCK_REALTIME: {}, host: {}, difference: {}",
            fmt_ns(guest_ns as i128),
            fmt_ns(host_ns),
            fmt_ns_signed(diff)
        );
        println!("  The probe reads the log every 100 ms, thus the value is up to 100 ms old.");
    }

    if drift == 0 {
        let _ = std::fs::remove_dir_all(&tmp);
        println!(
            "PASS: guest wall-clock drift is 0 ns (0.000000000 s) at a resolution \
             of 1 s (epoch seconds): the guest epoch {guest_epoch} is inside the \
             host window {host_epoch_before}..{host_epoch_after}. The tolerance is \
             zero."
        );
        if ev.kvmclock_msrs.is_empty() {
            println!(
                "NOTE: the guest's time is right, but it never printed a kvm-clock line -- \
                 the seed is coming from somewhere else. Worth knowing before changing the \
                 RTC config on the assumption kvmclock is what's carrying this."
            );
        }
    } else {
        keep_for_diagnosis();
        println!(
            "FAIL: guest wall-clock drift is {}, at a resolution of 1 s \
             (epoch seconds), outside the host window \
             {host_epoch_before}..{host_epoch_after}. The tolerance is zero -- the \
             guest did not get a valid boot-time seed.",
            fmt_ns(drift as i128 * 1_000_000_000)
        );
        if ev.kvmclock_msrs.is_empty() {
            println!(
                "  MECHANISM: no kvm-clock lines in the guest's boot log at all -- kvmclock \
                 is not enabled in this guest (CONFIG_KVM_GUEST, or the 0x4000_0000 CPUID \
                 leaves never reached it). Disabling the RTC driver would NOT fix this; the \
                 guest would just have no wall-clock source whatsoever."
            );
        } else {
            println!(
                "  MECHANISM: kvmclock IS enabled (see the kvm-clock lines above), so the \
                 wall-clock seed specifically -- MSR_KVM_WALL_CLOCK_NEW / \
                 x86_platform.get_wallclock -- is what's broken, not clocksource selection."
            );
        }
        std::process::exit(1);
    }
}
