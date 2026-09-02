//! Verifies "for the VM, freeze/thaw does not exist w.r.t. time": the
//! guest's own wall-clock heartbeat (see rootfs-init.sh) should show
//! ~0 elapsed time across a freeze/thaw cycle, no matter how long the
//! VM was actually frozen in real (host) time.
//!
//! This is the direct test for "mitigation 2" from the freeze/thaw
//! clock design discussion -- whether vcpu::restore_vm_clock's
//! KVM_SET_CLOCK call (which currently runs *after* the architectural
//! TSC is restored, see vcpu.rs) actually reconciles cleanly with
//! kvmclock's internal TSC-reference tracking, or whether the guest
//! sees real frozen time leak into its own clock.
//!
//! Three outcomes, deliberately kept distinct -- collapsing them into
//! one pass/fail is what would make this probe useless:
//!
//!   SILENT   the guest produces no heartbeat at all after thaw. This
//!            is NOT a clock-value result: the heartbeat loop needs a
//!            working LAPIC timer (`sleep 1`) to run at all, so a guest
//!            that resumed with no armed timer looks identical to one
//!            whose clock is wrong. Reported separately, with the
//!            things freeze/thaw is known not to save.
//!   LEAKED   heartbeats resume, but the guest's clock jumped forward
//!            by roughly the real frozen interval -- real time leaked
//!            in, which is the thing mitigation 2 was about.
//!   FROZEN   heartbeats resume at ~the pre-freeze value: time is
//!            cryogenic, as designed.
//!
//! Parameters. The Makefile sets each one to the default of cella, and
//! the same defaults are in this file, thus a direct run of the binary
//! behaves in the same way.
//!
//!   CELLA_FROZEN_SECS      The length of the freeze, in real seconds.
//!                          Default 6, which is several heartbeat
//!                          periods. Use 0 to thaw at once.
//!   CELLA_POST_THAW_SECS   The length of the measurement of the clock
//!                          rate after the thaw. Default 30, which gives
//!                          a resolution of approximately 350 ppm. Use 0
//!                          to omit the measurement.
//!   CELLA_EXTRA_CMDLINE    Text to append to the kernel command line.
//!                          Empty by default.
//!   CELLA_BIN, CELLA_TEST_KERNEL, CELLA_TEST_DISK, CELLA_TEST_TAP
//!                          The same meaning as in the smoke tests.
//!
//! `make smoke-thaw` runs this probe after scripts/test/thaw.sh. The
//! script checks that a thaw restores the process and the sidecar. This
//! probe checks that the thaw restores the time of the guest, which the
//! script cannot see.
//!
//! Run: cargo run --manifest-path probes/freeze-thaw-clock/Cargo.toml
//! (needs the canonical goldens -- `make golden` -- and a
//! configured tap0 -- `make setup-tap`)

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// 12 heartbeats at ~1 s each, plus the boot, plus slack for a deep
// nesting level.
const BOOT_TIMEOUT: Duration = Duration::from_secs(40);
const THAW_TIMEOUT: Duration = Duration::from_secs(20);
const FREEZE_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Real wall-clock seconds the VM stays frozen. Deliberately several
/// times the heartbeat interval (1s, see rootfs-init.sh) so a guest
/// that *did* leak real time would show an unmistakable jump, not
/// something explainable by heartbeat-timing quantization.
const FROZEN_REAL_SECS_DEFAULT: u64 = 6;

/// The length of the freeze, in real seconds. Set CELLA_FROZEN_SECS to
/// change it. A test of several lengths shows if an error is a step,
/// which does not change with the length, or a rate error, which
/// increases with the length.
fn frozen_real_secs() -> u64 {
    std::env::var("CELLA_FROZEN_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(FROZEN_REAL_SECS_DEFAULT)
}
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

/// fmt_ns for a value in seconds, as f64. The value is rounded to a
/// whole number of nanoseconds first.
fn fmt_secs(s: f64) -> String {
    fmt_ns((s * 1e9).round() as i128)
}

fn repo_root() -> PathBuf {
    // The probe modules compile inside the main package, thus the
    // manifest directory is the repo root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// The cella binary: CELLA_BIN, else the VMM sibling of this
/// probe (the probe drives the flag interface, and the flags are
/// the VMM's alone since 1.6.13).
fn sibling_cella(root: &std::path::Path) -> PathBuf {
    // The probe is a lab instrument: it reads consoles, thus it must
    // drive its own flavor. The -debug sibling wins (the installed
    // lab), then the plain sibling (the repo's target/smoke, where
    // both carry bare names), then the lab build under the root --
    // never the field binary, whose machines are dark.
    if let Ok(me) = std::env::current_exe() {
        for name in ["cella-vmm-debug", "cella-vmm"] {
            let p = me.parent().unwrap().join(name);
            if p.is_file() {
                return p;
            }
        }
    }
    root.join("target/smoke/cella-vmm")
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
fn read_file(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => String::new(),
    }
}

/// Read the monotonic clock from a heartbeat line. The format is
/// "cella-rootfs: wall-clock <epoch> uptime <seconds>". The resolution is
/// 10 ms, which is better than the 1 second resolution of the epoch.
fn parse_uptime(line: &str) -> Option<f64> {
    let (_, rest) = line.split_once(" uptime ")?;
    rest.trim().parse().ok()
}

/// The file content up to the last newline. The serial device writes a
/// line byte by byte. A read can therefore see a partial line, and a
/// number in a partial line parses to a wrong value. Every parser must
/// read complete lines only.
fn read_complete_lines(path: &Path) -> String {
    let s = read_file(path);
    match s.rfind('\n') {
        Some(i) => s[..=i].to_string(),
        None => String::new(),
    }
}

fn read_uptimes(log_path: &Path) -> Vec<f64> {
    read_complete_lines(log_path)
        .lines()
        .filter_map(parse_uptime)
        .collect()
}

/// The monotonic clock of the guest, in nanoseconds, from each heartbeat
/// line. The list is empty when the guest cannot produce the field.
fn read_mono_ns(log_path: &Path) -> Vec<u64> {
    read_complete_lines(log_path)
        .lines()
        .filter_map(|l| parse_ns(l, "mono_ns"))
        .collect()
}

fn read_heartbeats(log_path: &Path) -> Vec<i64> {
    read_complete_lines(log_path)
        .lines()
        .filter_map(parse_heartbeat)
        .collect()
}

/// Waits until at least `n` heartbeat lines have appeared, then returns
/// the *latest* one -- not the n-th. The n-th is up to a full heartbeat
/// interval stale by the time we act on it, and this value is compared
/// against a post-thaw reading, so staleness here shows up directly as
/// apparent elapsed time.
fn wait_for_heartbeats(
    log_path: &Path,
    child: &mut Child,
    n: usize,
    deadline: Instant,
) -> Option<i64> {
    loop {
        let hb = read_heartbeats(log_path);
        if hb.len() >= n {
            return hb.last().copied();
        }
        if let Ok(Some(_)) = child.try_wait() {
            return None;
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The host CLOCK_REALTIME in nanoseconds. Use it for an interval. The
/// value in whole seconds below is for a comparison against the epoch
/// field of a heartbeat, which has a resolution of 1 s.
fn host_epoch_ns() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i128
}

fn host_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn tail(text: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|s| s.to_string())
        .collect()
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
    // CELLA_TEST_TAP=none runs the guest without a network device. The
    // probe then names one virtio_mmio device on the command line, not
    // two. probe-inception uses this mode: inside a guest no TAP
    // exists, and the clock measurement needs no network.
    let tap = match std::env::var("CELLA_TEST_TAP") {
        Ok(v) if v == "none" => None,
        Ok(v) => Some(v),
        Err(_) => Some("tap0".to_string()),
    };

    if !bin.is_file() {
        fail(&format!("{} not built -- run: make build", bin.display()));
    }
    if !kernel.is_file() || !disk.is_file() {
        fail("test assets missing -- run: make golden");
    }

    // Skip, and do not fail, when this machine cannot run a guest. This
    // probe is part of `make smoke-thaw`, and the smoke tests skip in the
    // same conditions, so that `make smoke` passes on a machine with no
    // KVM and no tap device.
    let kvm_ok = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .is_ok();
    if !kvm_ok {
        println!("SKIP: no read and write access to /dev/kvm on this machine");
        std::process::exit(0);
    }
    if let Some(tap) = &tap {
        if !std::path::Path::new(&format!("/sys/class/net/{tap}")).exists() {
            println!("SKIP: {tap} does not exist -- run: make setup-tap");
            std::process::exit(0);
        }
    }

    let tmp = std::env::temp_dir().join(format!(
        "cella-freeze-thaw-clock-probe-{}",
        std::process::id()
    ));
    let state_dir = tmp.join("state");
    std::fs::create_dir_all(&state_dir).expect("create state dir");
    let disk_copy = tmp.join("disk.img");
    std::fs::copy(&disk, &disk_copy).expect("copy disk");

    // CELLA_EXTRA_CMDLINE appends to the kernel command line. Use it to
    // test a change to the time configuration of the guest without an
    // edit to this file.
    let extra = std::env::var("CELLA_EXTRA_CMDLINE").unwrap_or_default();
    // CELLA_TIME_ARGS holds the time arguments of cella. Set it to an
    // empty string to run without them, and to see the behaviour that
    // they correct.
    // An unset or empty value uses the default of cella. The word "none"
    // runs the guest with no time arguments at all, which is how the
    // behaviour that these arguments correct can be seen again.
    let time_args = match std::env::var("CELLA_TIME_ARGS") {
        Ok(v) if v.trim() == "none" => String::new(),
        Ok(v) if !v.trim().is_empty() => v,
        _ => default_time_args(&bin),
    };
    let base = default_base_args(&bin);
    let devices = if tap.is_some() {
        "virtio_mmio.device=4K@0xd0000000:5 virtio_mmio.device=4K@0xd0001000:6"
    } else {
        "virtio_mmio.device=4K@0xd0000000:5"
    };
    let cmdline = format!("{base} {time_args} root=/dev/vda rw {devices} {extra}");
    println!(
        "time arguments: {}",
        if time_args.trim().is_empty() {
            "(none)"
        } else {
            time_args.trim()
        }
    );
    let cmdline = cmdline.trim().to_string();
    if !extra.is_empty() {
        println!("extra kernel command line: {extra}");
    }

    // --- step 1: boot, collect a few heartbeats ---
    let boot_log = tmp.join("boot.log");
    let boot_err = tmp.join("boot.err");
    let mut child = Command::new(&bin)
        .arg("--state-dir")
        .arg(&state_dir)
        .arg("--kernel")
        .arg(&kernel)
        .arg("--disk")
        .arg(&disk_copy)
        .args(tap.iter().flat_map(|t| ["--tap".to_string(), t.clone()]))
        .arg("--mem-mb")
        .arg("128")
        .arg("--cmdline")
        .arg(&cmdline)
        .stdout(Stdio::from(
            File::create(&boot_log).expect("create boot log"),
        ))
        .stderr(Stdio::from(
            File::create(&boot_err).expect("create boot err"),
        ))
        .spawn()
        .expect("spawn cella (boot)");
    let pid = child.id() as i32;

    let deadline = Instant::now() + BOOT_TIMEOUT;
    // Wait for 12 heartbeats before the freeze. The first ones pass any
    // jitter from the boot. The rest give the normal interval of the
    // loop, which is the baseline that the interval across the freeze is
    // compared against. The prediction interval of the gate comes from
    // the sample standard deviation of these intervals, thus more
    // samples give a tighter and a more stable gate.
    let guest_before = match wait_for_heartbeats(&boot_log, &mut child, 12, deadline) {
        Some(v) => v,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            eprintln!("--- last 15 lines of serial output ---");
            for l in tail(&read_file(&boot_log), 15) {
                eprintln!("{l}");
            }
            eprintln!("(logs kept: {})", tmp.display());
            fail("guest never reported 3 wall-clock heartbeats before boot timeout");
        }
    };
    let host_at_freeze = host_epoch();
    let host_at_freeze_ns = host_epoch_ns();
    println!("step 1: booted. guest wall-clock = {guest_before}, host = {host_at_freeze}");

    // --- step 2: freeze ---
    // SAFETY: pid is a live child we just spawned; SIGUSR1 is cella's
    // documented freeze trigger (see README).
    unsafe {
        libc::kill(pid, libc::SIGUSR1);
    }
    let freeze_deadline = Instant::now() + FREEZE_TIMEOUT;
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        if Instant::now() >= freeze_deadline {
            let _ = child.kill();
            let _ = child.wait();
            eprintln!("(logs kept: {})", tmp.display());
            fail("process did not exit within timeout of SIGUSR1");
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    if !state_dir.join("state").is_file() {
        eprintln!("(logs kept: {})", tmp.display());
        fail("no state file after freeze");
    }
    // The guest may have printed one more heartbeat between our reading
    // and the signal landing; take the very last one it ever managed.
    let guest_before = read_heartbeats(&boot_log)
        .last()
        .copied()
        .unwrap_or(guest_before);
    let uptime_before = read_uptimes(&boot_log).last().copied();
    println!(
        "step 2: frozen (state file present). last pre-freeze guest wall-clock = {guest_before}"
    );

    // Dump the sidecar now, while the file exists. A thaw that is
    // successful deletes it (see finalize_thaw). This is the only time at
    // which the probe can read the state that the guest resumes from.
    let frozen_dump = Command::new(&bin)
        .arg("--dump-state")
        .arg(&state_dir)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_else(|e| format!("(could not dump frozen state: {e})"));

    // --- step 3: stay frozen for a real, known interval ---
    let frozen_secs = frozen_real_secs();
    println!(
        "step 3: staying frozen for {} of real time...",
        fmt_ns(frozen_secs as i128 * 1_000_000_000)
    );
    std::thread::sleep(Duration::from_secs(frozen_secs));

    // --- step 4: thaw, observe the first post-thaw heartbeat ---
    // No --kernel: the state dir already has a frozen state, so cella
    // thaws instead of booting (same as scripts/test/thaw.sh).
    let thaw_log = tmp.join("thaw.log");
    let thaw_err = tmp.join("thaw.err");
    let mut child2 = Command::new(&bin)
        .arg("--state-dir")
        .arg(&state_dir)
        .arg("--disk")
        .arg(&disk_copy)
        .args(tap.iter().flat_map(|t| ["--tap".to_string(), t.clone()]))
        .arg("--mem-mb")
        .arg("128")
        .arg("--cmdline")
        .arg(&cmdline)
        .stdout(Stdio::from(
            File::create(&thaw_log).expect("create thaw log"),
        ))
        .stderr(Stdio::from(
            File::create(&thaw_err).expect("create thaw err"),
        ))
        .spawn()
        .expect("spawn cella (thaw)");

    let host_at_thaw = host_epoch();
    let host_at_thaw_ns = host_epoch_ns();
    let deadline = Instant::now() + THAW_TIMEOUT;
    let guest_after = wait_for_heartbeats(&thaw_log, &mut child2, 1, deadline);
    // The host time at which the probe saw the first heartbeat. The poll
    // interval is 100 ms, thus a comparison against a guest timestamp
    // has that resolution, not the resolution of the clocks.
    let host_first_hb_ns = host_epoch_ns();
    let still_running = matches!(child2.try_wait(), Ok(None));

    // Measure the rate of the clock of the guest after the thaw. The
    // values above show only the step at the thaw. A guest can restore
    // the correct time and then run its clock at the wrong rate.
    //
    // The method is a least-squares fit of the monotonic clock of the
    // guest against the clock of the host. A comparison of the first and
    // last value gives an error of one heartbeat period, which is 1 s and
    // is larger than any rate error that this test can find. The fit uses
    // the host time at which each new heartbeat becomes visible, and it
    // averages the observation delay over all samples.
    let post_secs: u64 = std::env::var("CELLA_POST_THAW_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    if post_secs > 0 {
        let t0 = Instant::now();
        let mut samples: Vec<(f64, f64)> = Vec::new();
        let mut last_seen = f64::MIN;
        while t0.elapsed() < Duration::from_secs(post_secs) {
            if let Some(u) = read_uptimes(&thaw_log).last().copied() {
                if u > last_seen {
                    last_seen = u;
                    samples.push((t0.elapsed().as_secs_f64(), u));
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if samples.len() >= 3 {
            let n = samples.len() as f64;
            let mx = samples.iter().map(|s| s.0).sum::<f64>() / n;
            let my = samples.iter().map(|s| s.1).sum::<f64>() / n;
            let num: f64 = samples.iter().map(|s| (s.0 - mx) * (s.1 - my)).sum();
            let den: f64 = samples.iter().map(|s| (s.0 - mx).powi(2)).sum();
            let slope = num / den;
            println!("rate of the guest clock after the thaw:");
            println!(
                "  {} samples over {} of host time",
                samples.len(),
                fmt_secs(samples.last().unwrap().0)
            );
            println!("  guest seconds per host second: {slope:.6}");
            println!("  error: {:+.0} ppm", (slope - 1.0) * 1e6);
            println!();
        }
    }

    let thaw_stderr = read_file(&thaw_err);
    let thaw_stdout = read_file(&thaw_log);
    let _ = child2.kill();
    let _ = child2.wait();

    // Report the clock of the host. It decides what the result below
    // means. KVM sets PVCLOCK_TSC_STABLE_BIT in the pvclock page of the
    // guest only when the TSC of the host is stable. A host that reports
    // a clocksource other than "tsc", or that does not offer "tsc" at
    // all, is a host on which that bit stays clear, and the guest then
    // runs the clocksource watchdog against its TSC unless the command
    // line stops it.
    let host_cs =
        std::fs::read_to_string("/sys/devices/system/clocksource/clocksource0/current_clocksource")
            .unwrap_or_else(|_| "unknown".into());
    let host_avail = std::fs::read_to_string(
        "/sys/devices/system/clocksource/clocksource0/available_clocksource",
    )
    .unwrap_or_else(|_| "unknown".into());
    println!(
        "host clocksource: {} (available: {})",
        host_cs.trim(),
        host_avail.trim()
    );
    if host_avail.contains("tsc") {
        println!("  The host offers the TSC. Expect PVCLOCK_TSC_STABLE_BIT to be set below.");
    } else {
        println!(
            "  The host does not offer the TSC, therefore its own TSC is not stable.\n               Expect PVCLOCK_TSC_STABLE_BIT to be clear below. On such a host the guest\n               would report a clock fault after a thaw without tsc=reliable."
        );
    }
    println!();

    // Show the pvclock flags from the freeze image on every path. Linux
    // runs the clocksource watchdog against the TSC only when
    // PVCLOCK_TSC_STABLE_BIT is not set, so this flag decides whether the
    // guest checks the TSC at all.
    let mut in_pvclock = false;
    for line in frozen_dump.lines() {
        if line.starts_with("pvclock page") {
            in_pvclock = true;
        } else if in_pvclock && line.starts_with("kvmclock:") {
            break;
        }
        if in_pvclock && !line.trim().is_empty() {
            println!("  {line}");
        }
    }

    // Show the timing that the VMM measured. The delay between the read
    // of the TSC and the read of the kvmclock at the freeze must be equal
    // to the delay between the two writes at the thaw. A difference
    // between the two delays becomes a step in the clock of the guest.
    let boot_err = read_file(&tmp.join("boot.err"));
    for line in boot_err.lines().chain(thaw_stderr.lines()) {
        if line.contains("timing:") {
            println!("  {}", line.trim());
        }
    }

    let real_gap_ns = host_at_thaw_ns - host_at_freeze_ns;

    // --- verdict ---
    let guest_after = match guest_after {
        Some(v) => v,
        None => {
            // SILENT: not a clock-value result. Say so explicitly --
            // reading this as "the clock is broken" would send the next
            // hour of work at entirely the wrong subsystem.
            println!();
            if thaw_stdout.is_empty() {
                println!("--- diagnosis: guest is SILENT after thaw (no output at all) ---");
            } else {
                println!(
                    "--- diagnosis: guest RESUMED but never ticked ({} bytes of output, no \
                     heartbeat) ---",
                    thaw_stdout.len()
                );
                println!(
                    "    The guest runs. Therefore the problem is not the restore of the \
                     vCPU\n    or of the interrupt hardware. The problem is what the guest \
                     does after it starts."
                );
            }
            println!(
                "  VMM reported a successful thaw: {}",
                thaw_stderr.contains("thawed")
            );
            println!("  VMM process still running when we gave up: {still_running}");
            println!(
                "  bytes of serial output produced after thaw: {}",
                thaw_stdout.len()
            );

            // Show which timer the guest selected. The probe reads this
            // from the boot log of the guest. Do not assume the answer. A
            // statement such as "Linux selects the TSC-deadline
            // clockevent" needs evidence.
            let boot = read_file(&boot_log);
            let timer: Vec<&str> = boot
                .lines()
                .map(str::trim)
                .filter(|l| {
                    let low = l.to_ascii_lowercase();
                    low.contains("tsc deadline")
                        || low.contains("clockevent")
                        || low.contains("lapic")
                        || low.contains("apic timer")
                })
                .collect();
            println!("  --- which timer the guest armed (from its own boot log) ---");
            if timer.is_empty() {
                println!("    (nothing matched -- cannot attribute the silence to a timer)");
            }
            for l in timer.iter().take(8) {
                println!("    {l}");
            }

            println!("  --- what the guest was frozen from (sidecar dump) ---");
            for line in frozen_dump.lines() {
                println!("    {line}");
            }
            println!("  --- thaw stderr ---");
            for l in tail(&thaw_stderr, 10) {
                println!("  {l}");
            }
            if !thaw_stdout.is_empty() {
                println!("  --- FIRST serial output after thaw (where a fault begins) ---");
                for l in thaw_stdout.lines().take(20) {
                    println!("  {l}");
                }
                println!("  --- last serial output after thaw ---");
                for l in tail(&thaw_stdout, 6) {
                    println!("  {l}");
                }
            }
            println!(
                "\n  The heartbeat loop calls `sleep 1`. That call needs a timer. \
                 Therefore a guest\n  that resumes without a timer looks the same as a \
                 guest that has a wrong clock.\n  This output is not evidence about \
                 kvmclock or KVM_SET_CLOCK.\n\n  \
                 This branch already restores these items. Read the dump above before you \
                 examine them again:\n  \
                 - MSR_IA32_TSC_DEADLINE. It reads 0 here, because this guest masks its \
                 LAPIC timer and uses the PIT.\n  \
                 - The irqchip and PIT state (KVM_GET/SET_IRQCHIP, KVM_GET/SET_PIT2). \
                 This state is what lets the guest run again.\n  \
                 - The xsave area and XCR0. Without them the guest gets an invalid-opcode \
                 fault in AVX code.\n\n  \
                 Freeze does not save the virtio device state. main.rs makes a new \
                 MmioTransport at thaw. Its status is 0, its queues are not ready, and \
                 next_avail is 0. The drivers in the guest expect both devices to be in \
                 operation."
            );
            eprintln!("(logs kept: {})", tmp.display());
            std::process::exit(1);
        }
    };

    // A guest can resume, run its timer, and keep the correct time, and
    // also report an error about the thaw. If the probe shows only the
    // time difference, it hides that error. The first error that this
    // probe found after a thaw was of this type: the clocksource watchdog
    // marked the TSC unstable. That is a defect in the clock, even when
    // the time difference is correct.
    let complaints: Vec<&str> = thaw_stdout
        .lines()
        .map(str::trim)
        .filter(|l| {
            let low = l.to_ascii_lowercase();
            low.contains("unstable")
                || low.contains("oops")
                || low.contains("warning:")
                || low.contains("bug:")
                || low.contains("call trace")
                || low.contains("watchdog")
        })
        .collect();
    if complaints.is_empty() {
        println!("post-thaw kernel complaints: none");
    } else {
        // A complaint is a verdict, not a note. A guest can keep the
        // correct time and also report a fault about the thaw. The first
        // error that this probe found was of this type: the clocksource
        // watchdog marked the TSC unstable. The observation window is
        // CELLA_POST_THAW_SECS; a window of 0 can miss a late complaint.
        println!(
            "post-thaw kernel complaints ({} lines) -- the guest resumed, but not cleanly:",
            complaints.len()
        );
        for l in complaints.iter().take(8) {
            println!("  {l}");
        }
        eprintln!("(logs kept: {})", tmp.display());
        fail("the guest kernel complained after the thaw (see the lines above)");
    }
    println!();

    // The monotonic clock of the guest, at a resolution of 10 ms. The
    // epoch value has a resolution of 1 second, and the heartbeat loop
    // runs once per second. Therefore the epoch cannot show if the
    // restore is exact to better than 1 second. This value can.
    // Nanoseconds, when the guest can report them. The resolution of the
    // timestamp is 1 ns. The resolution of the measurement is not: the
    // loop runs once per second and starts two programs each cycle, thus
    // the interval that contains the freeze must be compared against a
    // normal interval of the same loop, and the difference between them
    // is the result.
    // Keep the nanosecond measurement for the verdict. The verdict must
    // state only what the data shows, and the wall-clock comparison
    // below has a resolution of 1 s.
    let mut mono_measure: Option<(i128, i128, i128)> = None;
    // The gate below compares one new interval against n baseline
    // intervals. The correct bound for that comparison is a prediction
    // interval: |difference| <= 3 * s * sqrt(1 + 1/n), where s is the
    // sample standard deviation of the baseline. The range (max - min)
    // of a small sample is not a test statistic.
    let before_ns = read_mono_ns(&boot_log);
    let after_ns = read_mono_ns(&thaw_log).first().copied();
    if let (Some(&last_before), Some(after)) = (before_ns.last(), after_ns) {
        let across = after as i128 - last_before as i128;
        let intervals: Vec<i128> = before_ns
            .windows(2)
            .map(|w| w[1] as i128 - w[0] as i128)
            .collect();
        println!("guest monotonic clock (/proc/timer_list):");
        println!("  before the freeze: {}", fmt_ns(last_before as i128));
        println!("  after the thaw:    {}", fmt_ns(after as i128));
        println!("  across the freeze: {}", fmt_ns(across));
        if intervals.len() >= 2 {
            let n = intervals.len() as f64;
            let mean = intervals.iter().sum::<i128>() / intervals.len() as i128;
            let max = *intervals.iter().max().unwrap();
            let min = *intervals.iter().min().unwrap();
            let m = intervals.iter().sum::<i128>() as f64 / n;
            let var = intervals
                .iter()
                .map(|&x| (x as f64 - m).powi(2))
                .sum::<f64>()
                / (n - 1.0);
            let sd = var.sqrt();
            let bound = (3.0 * sd * (1.0 + 1.0 / n).sqrt()).round() as i128;
            println!(
                "  normal interval:   mean {}, min {}, max {}, s {} ({} samples)",
                fmt_ns(mean),
                fmt_ns(min),
                fmt_ns(max),
                fmt_ns(sd.round() as i128),
                intervals.len()
            );
            println!("  difference:        {}", fmt_ns_signed(across - mean));
            println!(
                "  The 3-sigma prediction interval for one new interval is \
                 +/-{}.\n  A difference inside that interval is not a measurement \
                 of the freeze.",
                fmt_ns(bound)
            );
            mono_measure = Some((across, mean, bound));
        }
        println!();
    }

    let uptime_after = read_uptimes(&thaw_log).first().copied();
    if let (Some(before), Some(after)) = (uptime_before, uptime_after) {
        let delta = after - before;
        println!("guest monotonic clock (/proc/uptime):");
        println!("  before the freeze: {}", fmt_secs(before));
        println!("  after the thaw:    {}", fmt_secs(after));
        println!(
            "  advance across a freeze of {}: {}",
            fmt_ns(real_gap_ns),
            fmt_secs(delta)
        );
        // Compare the interval that contains the freeze against a normal
        // interval of the same loop. The loop calls `sleep 1`, and it also
        // starts two programs each cycle, so a normal interval is more
        // than 1.00 s. Only this comparison shows whether the freeze added
        // time, because a value of "about 1 second" alone does not.
        let pre: Vec<f64> = read_uptimes(&boot_log)
            .windows(2)
            .map(|w| w[1] - w[0])
            .collect();
        if !pre.is_empty() {
            let mean = pre.iter().sum::<f64>() / pre.len() as f64;
            let max = pre.iter().cloned().fold(f64::MIN, f64::max);
            println!(
                "  normal interval of this loop before the freeze: mean {}, max \
                 {} ({} samples)",
                fmt_secs(mean),
                fmt_secs(max),
                pre.len()
            );
            println!(
                "  the interval that contains the freeze is {} against that mean",
                fmt_ns_signed(((delta - mean) * 1e9).round() as i128)
            );
        }
        if delta < 2.0 {
            println!(
                "  The monotonic clock did not advance during the freeze. The guest \
                 continues\n  from the time at which it stopped."
            );
        } else {
            println!(
                "  The monotonic clock advanced by more than one heartbeat interval. Real \
                 time\n  entered the guest."
            );
        }
        println!();
    }

    let guest_delta = guest_after - guest_before;
    println!(
        "step 4: thawed. first post-thaw guest wall-clock = {guest_after}, host = {host_at_thaw}"
    );
    println!();
    println!(
        "  real time spent frozen (host):    {}",
        fmt_ns(real_gap_ns)
    );
    // The monotonic clock of the guest has a resolution of 1 ns. The
    // epoch field has a resolution of 1 s. Use the monotonic value.
    match mono_measure {
        Some((across, _, _)) => println!("  time the guest thinks passed:     {}", fmt_ns(across)),
        None => println!(
            "  time the guest thinks passed:     {} (resolution 1 s)",
            fmt_ns(guest_delta as i128 * 1_000_000_000)
        ),
    }
    // The guest reports its CLOCK_REALTIME in nanoseconds (real_ns, see
    // rootfs.sh). The comparison point is the observation of the first
    // heartbeat, at the 100 ms poll of the probe.
    match read_complete_lines(&thaw_log)
        .lines()
        .find_map(|l| parse_ns(l, "real_ns"))
    {
        Some(guest_real_ns) => println!(
            "  guest clock vs. host now:         {} behind (resolution 100 ms, the poll)",
            fmt_ns(host_first_hb_ns - guest_real_ns as i128)
        ),
        None => println!(
            "  guest clock vs. host now:         {} behind (resolution 1 s)",
            fmt_ns((host_at_thaw - guest_after) as i128 * 1_000_000_000)
        ),
    }

    // The verdict needs the nanosecond measurement. Without it, the 1 s
    // wall-clock comparison cannot verify a frozen clock.
    let (across, mean, bound) = match mono_measure {
        Some(m) => m,
        None => {
            eprintln!("(logs kept: {})", tmp.display());
            fail(
                "no nanosecond monotonic data from the guest -- the 1 s \
                 wall-clock comparison cannot verify a frozen clock",
            );
        }
    };

    // The epoch delta and the monotonic measurement describe the same
    // crossing. At a resolution of 1 s the two agree when
    // |delta - across| <= 1 s. That bound is the quantization of the
    // epoch, not a tuned tolerance. A disagreement means the wall-clock
    // and the monotonic clock of the guest moved differently across the
    // freeze, and that is a fault on its own.
    let delta_ns = guest_delta as i128 * 1_000_000_000;
    if (delta_ns - across).abs() > 1_000_000_000 {
        println!(
            "\nFAIL (INCONSISTENT): the wall-clock of the guest advanced {} \
             across the freeze, and its monotonic clock advanced {}. The two \
             must agree to within the 1 s resolution of the epoch. The two \
             clocks of the guest moved differently across the freeze.",
            fmt_ns(delta_ns),
            fmt_ns(across)
        );
        eprintln!("(logs kept: {})", tmp.display());
        std::process::exit(1);
    }

    let diff = across - mean;
    if diff.abs() <= bound {
        let _ = std::fs::remove_dir_all(&tmp);
        println!(
            "\nPASS (FROZEN): the freeze took {} of real time. The monotonic \
             clock of the guest advanced {} across it, {} \
             against a normal heartbeat interval, inside the 3-sigma \
             prediction interval (+/-{}) -- time is cryogenic, as designed.",
            fmt_ns(real_gap_ns),
            fmt_ns(across),
            fmt_ns_signed(diff),
            fmt_ns(bound)
        );
    } else {
        println!(
            "\nFAIL (LEAKED): the freeze took {} of real time. The monotonic \
             clock of the guest advanced {} across it, {} \
             against a normal heartbeat interval, outside the 3-sigma \
             prediction interval (+/-{}). That difference is time that \
             entered the clock of the guest.",
            fmt_ns(real_gap_ns),
            fmt_ns(across),
            fmt_ns_signed(diff),
            fmt_ns(bound)
        );
        // Distinguish "leaked exactly the frozen interval" (KVM_SET_CLOCK
        // did not take effect) from an arbitrary jump. The comparison
        // bound is the same prediction interval.
        if (diff - real_gap_ns).abs() <= bound {
            println!(
                "  The excess matches the real frozen interval ({}): the clock \
                 of the guest tracked host real time straight through the \
                 freeze, i.e. the restored kvmclock value did not take effect.",
                fmt_ns(real_gap_ns)
            );
        }
        eprintln!("(logs kept: {})", tmp.display());
        std::process::exit(1);
    }
}
