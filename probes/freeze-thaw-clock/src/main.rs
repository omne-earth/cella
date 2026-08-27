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
//! Run: cargo run --manifest-path probes/freeze-thaw-clock/Cargo.toml
//! (needs dist/bzImage + dist/rootfs.ext4 -- `make dist` -- and a
//! configured tap0 -- `make setup-tap`)

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BOOT_TIMEOUT: Duration = Duration::from_secs(20);
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
/// How much guest-perceived time we tolerate across the freeze: a
/// couple of heartbeat ticks' worth of slop, not the multi-second real
/// gap we're deliberately creating.
const MAX_GUEST_DELTA_SECS: i64 = 3;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn env_path(var: &str, default: PathBuf) -> PathBuf {
    std::env::var(var).map(PathBuf::from).unwrap_or(default)
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

fn read_uptimes(log_path: &Path) -> Vec<f64> {
    read_file(log_path).lines().filter_map(parse_uptime).collect()
}

fn read_heartbeats(log_path: &Path) -> Vec<i64> {
    read_file(log_path).lines().filter_map(parse_heartbeat).collect()
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

fn main() {
    let root = repo_root();
    let bin = env_path("CELLA_BIN", root.join("target/release/cella"));
    let kernel = env_path("CELLA_TEST_KERNEL", root.join("dist/bzImage"));
    let disk = env_path("CELLA_TEST_DISK", root.join("dist/rootfs.ext4"));
    let tap = std::env::var("CELLA_TEST_TAP").unwrap_or_else(|_| "tap0".to_string());

    if !bin.is_file() {
        fail(&format!("{} not built -- run: make build", bin.display()));
    }
    if !kernel.is_file() || !disk.is_file() {
        fail("test assets missing -- run: make dist");
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
    let cmdline = format!(
        "console=ttyS0 reboot=k panic=1 pci=off tsc=unstable clocksource=kvm-clock root=/dev/vda rw \
         virtio_mmio.device=4K@0xd0000000:5 virtio_mmio.device=4K@0xd0001000:6 {extra}"
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
        .arg("--tap")
        .arg(&tap)
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
    // Wait for 3 heartbeats before freezing (past any boot-time jitter
    // in the print loop's own timing), but use the newest one.
    let guest_before = match wait_for_heartbeats(&boot_log, &mut child, 3, deadline) {
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
    let guest_before = read_heartbeats(&boot_log).last().copied().unwrap_or(guest_before);
    let uptime_before = read_uptimes(&boot_log).last().copied();
    println!("step 2: frozen (state file present). last pre-freeze guest wall-clock = {guest_before}");

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
    println!("step 3: staying frozen for {frozen_secs}s of real time...");
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
        .arg("--tap")
        .arg(&tap)
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
    let deadline = Instant::now() + THAW_TIMEOUT;
    let guest_after = wait_for_heartbeats(&thaw_log, &mut child2, 1, deadline);
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
                "  {} samples over {:.0} s of host time",
                samples.len(),
                samples.last().unwrap().0
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

    let real_gap = host_at_thaw - host_at_freeze;

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
        println!(
            "post-thaw kernel complaints ({} lines) -- the guest resumed, but not cleanly:",
            complaints.len()
        );
        for l in complaints.iter().take(8) {
            println!("  {l}");
        }
    }
    println!();

    // The monotonic clock of the guest, at a resolution of 10 ms. The
    // epoch value has a resolution of 1 second, and the heartbeat loop
    // runs once per second. Therefore the epoch cannot show if the
    // restore is exact to better than 1 second. This value can.
    let uptime_after = read_uptimes(&thaw_log).first().copied();
    if let (Some(before), Some(after)) = (uptime_before, uptime_after) {
        let delta = after - before;
        println!("guest monotonic clock (/proc/uptime):");
        println!("  before the freeze: {before:.2} s");
        println!("  after the thaw:    {after:.2} s");
        println!(
            "  advance across a freeze of {real_gap} real seconds: {delta:.2} s",
            real_gap = host_at_thaw - host_at_freeze
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
                "  normal interval of this loop before the freeze: mean {mean:.2} s, max \
                 {max:.2} s ({} samples)",
                pre.len()
            );
            println!(
                "  the interval that contains the freeze is {:+.2} s against that mean",
                delta - mean
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
    println!("step 4: thawed. first post-thaw guest wall-clock = {guest_after}, host = {host_at_thaw}");
    println!();
    println!("  real time spent frozen (host):    {real_gap}s");
    println!("  time the guest thinks passed:     {guest_delta}s");
    println!(
        "  guest clock vs. host now:         {}s behind",
        host_at_thaw - guest_after
    );

    if guest_delta.abs() <= MAX_GUEST_DELTA_SECS {
        let _ = std::fs::remove_dir_all(&tmp);
        println!(
            "\nPASS (FROZEN): guest clock shows ~0 elapsed time across the freeze \
             (real {real_gap}s vs. guest {guest_delta}s) -- time is cryogenic, as designed."
        );
    } else {
        // LEAKED. Distinguish "leaked exactly the frozen interval"
        // (KVM_SET_CLOCK effectively didn't take) from an arbitrary jump.
        let leaked_whole_gap = (guest_delta - real_gap).abs() <= MAX_GUEST_DELTA_SECS;
        println!(
            "\nFAIL (LEAKED): guest clock advanced {guest_delta}s across a freeze that only \
             {MAX_GUEST_DELTA_SECS}s of tolerance should allow."
        );
        if leaked_whole_gap {
            println!(
                "  The jump matches the real frozen interval ({real_gap}s) almost exactly: the \
                 guest's clock tracked host real time straight through the freeze, i.e. the \
                 restored kvmclock value did not take effect."
            );
        } else {
            println!(
                "  The jump ({guest_delta}s) does NOT match the real frozen interval \
                 ({real_gap}s), so this is not a simple 'restore didn't take' -- the restored \
                 kvmclock and the restored TSC are likely inconsistent with each other."
            );
        }
        eprintln!("(logs kept: {})", tmp.display());
        std::process::exit(1);
    }
}
