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
const FROZEN_REAL_SECS: u64 = 6;
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
    line.strip_prefix("cella-rootfs: wall-clock ")?
        .trim()
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

    let cmdline = "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw virtio_mmio.device=4K@0xd0000000:5 virtio_mmio.device=4K@0xd0001000:6";

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
        .arg(cmdline)
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
    println!("step 2: frozen (state file present). last pre-freeze guest wall-clock = {guest_before}");

    // --- step 3: stay frozen for a real, known interval ---
    println!("step 3: staying frozen for {FROZEN_REAL_SECS}s of real time...");
    std::thread::sleep(Duration::from_secs(FROZEN_REAL_SECS));

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
        .arg(cmdline)
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

    let thaw_stderr = read_file(&thaw_err);
    let thaw_stdout = read_file(&thaw_log);
    let _ = child2.kill();
    let _ = child2.wait();

    let real_gap = host_at_thaw - host_at_freeze;

    // --- verdict ---
    let guest_after = match guest_after {
        Some(v) => v,
        None => {
            // SILENT: not a clock-value result. Say so explicitly --
            // reading this as "the clock is broken" would send the next
            // hour of work at entirely the wrong subsystem.
            println!();
            println!("--- diagnosis: guest is SILENT after thaw ---");
            println!(
                "  VMM reported a successful thaw: {}",
                thaw_stderr.contains("thawed")
            );
            println!("  VMM process still running when we gave up: {still_running}");
            println!(
                "  bytes of serial output produced after thaw: {}",
                thaw_stdout.len()
            );
            println!("  --- thaw stderr ---");
            for l in tail(&thaw_stderr, 10) {
                println!("  {l}");
            }
            if !thaw_stdout.is_empty() {
                println!("  --- last serial output after thaw ---");
                for l in tail(&thaw_stdout, 10) {
                    println!("  {l}");
                }
            }
            println!(
                "\n  This is NOT evidence about kvmclock/KVM_SET_CLOCK. The heartbeat loop \
                 needs `sleep 1` to return, which needs an armed LAPIC timer. Freeze/thaw \
                 currently does not save:\n\
                 \x20   - MSR_IA32_TSC_DEADLINE (0x6e0), the pending deadline for the \
                 TSC-deadline LAPIC timer Linux almost certainly selected here -- it is not \
                 in vcpu.rs's SAVED_MSRS, so KVM_SET_LAPIC restarts the timer from a zero \
                 deadline and arms nothing.\n\
                 \x20   - in-kernel irqchip + PIT state (KVM_GET/SET_IRQCHIP, \
                 KVM_GET/SET_PIT2): main.rs builds a fresh irqchip on thaw, so the IOAPIC \
                 redirection entries the guest programmed for IRQ 4/5/6 are gone.\n\
                 \x20   - virtio device-model state: main.rs rebuilds MmioTransport::new on \
                 thaw (status 0, queues not ready, next_avail 0) while the guest's drivers \
                 believe both devices are live.\n\
                 Rule those out before touching the clock restore."
            );
            eprintln!("(logs kept: {})", tmp.display());
            std::process::exit(1);
        }
    };

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
