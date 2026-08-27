//! cella: a from-scratch-ish x86_64 KVM microVM.
//!
//! Boots a bzImage kernel with virtio-blk + virtio-net + serial, no PCI,
//! no ACPI, single vCPU. SIGUSR1 triggers a cryogenic freeze: guest RAM
//! (already a file, see memory.rs) is synced, vCPU/clock state is written
//! to a crash-consistent sidecar, and the process exits. Re-running
//! against the same --state-dir thaws instead of booting.
//!
//! See README.md for the full design rationale; this file is just the
//! plumbing that ties memory.rs / boot/x86_64.rs / vcpu.rs / devices/ /
//! freeze.rs / seccomp.rs together.

use cella::{boot, devices, freeze, memory, seccomp, vcpu};

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use kvm_bindings::{kvm_pit_config, kvm_userspace_memory_region};
use kvm_ioctls::Kvm;
use vm_memory::{GuestAddress, GuestMemory};

use devices::serial::SerialDevice;
use devices::virtio::block::Block;
use devices::virtio::mmio::MmioTransport;
use devices::virtio::net::Net;

const BLOCK_MMIO_BASE: u64 = 0xd000_0000;
const NET_MMIO_BASE: u64 = 0xd000_1000;
const MMIO_LEN: u64 = 0x1000;
const BLOCK_IRQ: u32 = 5;
const NET_IRQ: u32 = 6;

static FREEZE_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigusr1(_: libc::c_int) {
    FREEZE_REQUESTED.store(true, Ordering::SeqCst);
}

// SIGIO from the TAP fd (see tap.rs). The handler's only job is to exist
// without SA_RESTART so KVM_RUN returns EINTR; the run loop drains RX on
// every pass, so there is no flag to set here.
extern "C" fn on_sigio(_: libc::c_int) {}

struct Args {
    state_dir: PathBuf,
    disk: PathBuf,
    disk_ro: bool,
    tap: String,
    mac: [u8; 6],
    kernel: Option<PathBuf>,
    cmdline: String,
    mem_mb: u64,
}

fn parse_args() -> Args {
    let mut state_dir = None;
    let mut disk = None;
    let mut disk_ro = false;
    let mut tap = None;
    let mut mac = [0x02, 0xfc, 0x00, 0x00, 0x00, 0x01];
    let mut kernel = None;
    let mut cmdline = "console=ttyS0 reboot=k panic=1 pci=off".to_string();
    let mut mem_mb = 256u64;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut next = || {
            it.next()
                .unwrap_or_else(|| usage_error(&format!("missing value for {arg}")))
        };
        match arg.as_str() {
            "--state-dir" => state_dir = Some(PathBuf::from(next())),
            "--disk" => disk = Some(PathBuf::from(next())),
            "--disk-ro" => disk_ro = true,
            "--tap" => tap = Some(next()),
            "--mac" => mac = parse_mac(&next()),
            "--kernel" => kernel = Some(PathBuf::from(next())),
            "--cmdline" => cmdline = next(),
            "--mem-mb" => {
                mem_mb = next()
                    .parse()
                    .unwrap_or_else(|_| usage_error("--mem-mb must be a number"))
            }
            "-h" | "--help" => usage_error(
                "cella --state-dir DIR --disk PATH --tap NAME [--kernel PATH --cmdline STR --mem-mb N] [--mac AA:BB:CC:DD:EE:FF] [--disk-ro]",
            ),
            other => usage_error(&format!("unknown argument: {other}")),
        }
    }

    Args {
        state_dir: state_dir.unwrap_or_else(|| usage_error("--state-dir is required")),
        disk: disk.unwrap_or_else(|| usage_error("--disk is required")),
        disk_ro,
        tap: tap.unwrap_or_else(|| usage_error("--tap is required")),
        mac,
        kernel,
        cmdline,
        mem_mb,
    }
}

fn parse_mac(s: &str) -> [u8; 6] {
    let mut out = [0u8; 6];
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 6 {
        usage_error("--mac must be AA:BB:CC:DD:EE:FF");
    }
    for (i, p) in parts.iter().enumerate() {
        out[i] = u8::from_str_radix(p, 16).unwrap_or_else(|_| usage_error("bad --mac byte"));
    }
    out
}

fn usage_error(msg: &str) -> ! {
    eprintln!("cella: {msg}");
    std::process::exit(2);
}

fn main() {
    // Hidden self-test hook for `make test-seccomp`: install the real
    // filter and deliberately trip it. See seccomp::selftest_provoke_kill
    // for what the harness expects to observe.
    if std::env::args().nth(1).as_deref() == Some("--selftest-seccomp") {
        seccomp::selftest_provoke_kill();
    }

    // Print the contents of a frozen sidecar. Use this option to examine
    // freeze and thaw problems, because there is no debugger in the
    // guest. This code does not use KVM or the devices. Therefore it runs
    // before the code that makes them. A thaw that is successful deletes
    // the state file. Therefore you must dump the state before you thaw
    // it, not after.
    if std::env::args().nth(1).as_deref() == Some("--dump-state") {
        let dir = std::env::args()
            .nth(2)
            .unwrap_or_else(|| usage_error("--dump-state needs a state directory"));
        dump_state(&PathBuf::from(dir));
    }

    let args = parse_args();
    let frozen = freeze::is_frozen(&args.state_dir);
    // Set at the thaw, and read immediately before the first KVM_RUN.
    let mut thaw_clock_written: Option<std::time::Instant> = None;

    let mem_size_bytes = if frozen {
        // Thawing: mem size comes from the frozen state, not the CLI, so
        // it can't disagree with the RAM file we're about to reopen.
        match freeze::read_state(&args.state_dir) {
            Ok(s) => s.mem_size,
            Err(e) => fatal(&format!("reading frozen state: {e:?}")),
        }
    } else {
        args.mem_mb * 1024 * 1024
    };

    let kvm = Kvm::new().unwrap_or_else(|e| fatal(&format!("open /dev/kvm: {e}")));
    let vm = kvm
        .create_vm()
        .unwrap_or_else(|e| fatal(&format!("KVM_CREATE_VM: {e}")));

    // Required before creating an in-kernel irqchip on x86_64.
    vm.set_tss_address(0xffff_d000).unwrap();
    vm.set_identity_map_address(0xffff_c000).unwrap();
    vm.create_irq_chip()
        .unwrap_or_else(|e| fatal(&format!("KVM_CREATE_IRQCHIP: {e}")));
    vm.create_pit2(kvm_pit_config::default())
        .unwrap_or_else(|e| fatal(&format!("KVM_CREATE_PIT2: {e}")));

    let ram_path = freeze::ram_path(&args.state_dir);
    let (_ram_file, mem) = memory::open_ram_file(&ram_path, mem_size_bytes, !frozen)
        .unwrap_or_else(|e| fatal(&format!("guest RAM: {e}")));
    memory::harden_ram(&mem);

    let base_ptr = mem
        .find_region(GuestAddress(0))
        .expect("region at guest address 0")
        .as_ptr() as u64;
    // SAFETY: `mem` covers exactly [0, mem_size_bytes) at `base_ptr`,
    // backed by `_ram_file`, which is kept alive for the whole process
    // lifetime by staying in this function's scope.
    unsafe {
        vm.set_user_memory_region(kvm_userspace_memory_region {
            slot: 0,
            guest_phys_addr: 0,
            memory_size: mem_size_bytes,
            userspace_addr: base_ptr,
            flags: 0,
        })
        .unwrap_or_else(|e| fatal(&format!("KVM_SET_USER_MEMORY_REGION: {e}")));
    }

    let cpuid = vcpu::supported_cpuid(&kvm).unwrap_or_else(|e| fatal(&format!("cpuid: {e:?}")));
    let vcpu_fd =
        vcpu::create_vcpu(&vm, &cpuid).unwrap_or_else(|e| fatal(&format!("create vcpu: {e:?}")));

    let vm = Arc::new(vm);
    let irq_raiser: Arc<dyn devices::virtio::mmio::IrqLine> = vm.clone();

    let block = Block::new(&args.disk, args.disk_ro)
        .unwrap_or_else(|e| fatal(&format!("open disk {:?}: {e}", args.disk)));
    // Must precede Tap::open (inside Net::new): the TAP fd is O_ASYNC,
    // and SIGIO's default action is to terminate the process. A frame
    // can arrive the instant the fd exists.
    install_sigio_handler();
    let net = Net::new(&args.tap, args.mac)
        .unwrap_or_else(|e| fatal(&format!("open tap {:?}: {e}", args.tap)));
    let net_fd = net.tap_fd();

    let mut mmio_devices: Vec<(u64, u64, MmioTransport)> = vec![
        (
            BLOCK_MMIO_BASE,
            MMIO_LEN,
            MmioTransport::new(Box::new(block), irq_raiser.clone(), BLOCK_IRQ),
        ),
        (
            NET_MMIO_BASE,
            MMIO_LEN,
            MmioTransport::new(Box::new(net), irq_raiser.clone(), NET_IRQ),
        ),
    ];
    let net_transport_idx = 1usize;

    let mut serial = SerialDevice::new(vm.clone());

    if frozen {
        let frozen_state =
            freeze::read_state(&args.state_dir).unwrap_or_else(|e| fatal(&format!("{e:?}")));
        let actual_khz = vcpu_fd.get_tsc_khz().unwrap_or(0);
        if let Err(e) = freeze::check_hardware(&frozen_state, actual_khz) {
            fatal(&format!(
                "refusing to thaw onto different hardware: {e:?} (this image was \
                 frozen on a host with a different TSC frequency)"
            ));
        }
        // The MSR batch at the end of `restore` contains MSR_IA32_TSC. The
        // clock write follows it immediately.
        //
        // Both sequences give the same result. A test of the opposite
        // sequence (clock first, then the vCPU state) gave a skew of 7 ms
        // to 18 ms, which is the same range as this sequence. Therefore
        // the sequence of these two calls is not the cause of the skew.
        vcpu::restore(&vcpu_fd, &frozen_state.vcpu)
            .unwrap_or_else(|e| fatal(&format!("restoring vcpu state: {e:?}")));
        let t_after_restore = std::time::Instant::now();
        vcpu::restore_vm_clock(&vm, &frozen_state.clock)
            .unwrap_or_else(|e| fatal(&format!("restoring clock: {e:?}")));
        let t_after_clock = std::time::Instant::now();
        eprintln!(
            "cella: thaw timing: TSC write to clock write {} us",
            (t_after_clock - t_after_restore).as_micros()
        );
        thaw_clock_written = Some(t_after_clock);
        // The code above made a new irqchip and a new PIT. This call puts
        // back the IOAPIC routing and the PIT programming that the guest
        // set before the freeze. Without this call, a halted guest waits
        // for a timer interrupt that does not occur.
        vcpu::restore_irqchip(&vm, &frozen_state.irqchip)
            .unwrap_or_else(|e| fatal(&format!("restoring irqchip/PIT: {e:?}")));
        // Tell the guest that it was stopped. KVM sets PVCLOCK_GUEST_STOPPED
        // in the pvclock page at the next update, which occurs at the
        // first vCPU entry. Linux reads this flag in the kvmclock code and
        // calls pvclock_touch_watchdogs(). This resets the clocksource
        // watchdog and the soft-lockup watchdog for the interval that
        // contains the freeze.
        //
        // Without this call the guest measures its TSC against kvm-clock
        // across the freeze, finds a difference of 8 ms to 23 ms, and
        // marks the TSC unstable. The difference does not come from the
        // VMM: the delay from the read of the TSC to the read of the
        // clock is 1 us, the delay between the two writes is 0 us, and
        // the delay from the write of the clock to the first KVM_RUN is
        // approximately 200 us.
        //
        // This call must come after the restore of MSR_KVM_SYSTEM_TIME_NEW,
        // because KVM refuses the request if the pvclock page of the guest
        // is not active.
        if let Err(e) = vcpu_fd.kvmclock_ctrl() {
            eprintln!("cella: warning: KVM_KVMCLOCK_CTRL failed: {e}");
        }

        freeze::finalize_thaw(&args.state_dir)
            .unwrap_or_else(|e| fatal(&format!("finalizing thaw: {e}")));
        eprintln!("cella: thawed {:?}", args.state_dir);
    } else {
        let kernel = args.kernel.clone().unwrap_or_else(|| {
            usage_error("--kernel is required when booting fresh (no frozen state found)")
        });
        let boot_info = boot::load_kernel(&mem, &kernel, &args.cmdline, mem_size_bytes)
            .unwrap_or_else(|e| fatal(&format!("loading kernel: {e:?}")));
        boot::build_page_tables(&mem, mem_size_bytes)
            .unwrap_or_else(|e| fatal(&format!("page tables: {e:?}")));
        // enable_long_mode must run before setup_gdt -- see its doc
        // comment for why KVM rejects the other order.
        boot::enable_long_mode(&vcpu_fd)
            .unwrap_or_else(|e| fatal(&format!("enabling long mode: {e:?}")));
        boot::setup_gdt(&mem, &vcpu_fd).unwrap_or_else(|e| fatal(&format!("gdt: {e:?}")));
        boot::set_entry_point(&vcpu_fd, &boot_info)
            .unwrap_or_else(|e| fatal(&format!("entry point: {e:?}")));
        eprintln!("cella: booting {:?}", kernel);
    }

    install_sigusr1_handler();
    seccomp::install().unwrap_or_else(|e| fatal(&format!("seccomp: {e}")));

    // The guest starts to run at the first KVM_RUN in run_loop. Measure
    // the delay from the write of the clock to that moment.
    if let Some(t) = thaw_clock_written {
        eprintln!(
            "cella: thaw timing: clock write to first KVM_RUN {} us",
            t.elapsed().as_micros()
        );
    }

    run_loop(
        vcpu_fd,
        &vm,
        &mem,
        &mut serial,
        &mut mmio_devices,
        net_transport_idx,
        net_fd,
        &args.state_dir,
        mem_size_bytes,
    );
}

#[allow(clippy::too_many_arguments)]
fn run_loop(
    mut vcpu_fd: kvm_ioctls::VcpuFd,
    vm: &Arc<kvm_ioctls::VmFd>,
    mem: &vm_memory::GuestMemoryMmap,
    serial: &mut SerialDevice,
    mmio_devices: &mut [(u64, u64, MmioTransport)],
    net_transport_idx: usize,
    net_fd: i32,
    state_dir: &std::path::Path,
    mem_size_bytes: u64,
) {
    loop {
        if FREEZE_REQUESTED.load(Ordering::SeqCst) {
            do_freeze(&vcpu_fd, vm, mem, state_dir, mem_size_bytes);
            std::process::exit(0);
        }

        match vcpu_fd.run() {
            Ok(exit) => {
                let mut devices = vcpu::Devices {
                    serial,
                    mmio_devices,
                    mem,
                };
                match vcpu::dispatch(exit, &mut devices) {
                    vcpu::RunResult::Continue | vcpu::RunResult::Halted => {}
                    vcpu::RunResult::Shutdown => {
                        eprintln!("cella: guest requested shutdown");
                        std::process::exit(0);
                    }
                }
            }
            Err(e) if e.errno() == libc::EINTR => {
                // FREEZE_REQUESTED just got set (checked at the top of
                // the next iteration), or the TAP raised SIGIO because a
                // frame arrived (drained below), or a spurious signal.
            }
            Err(e) => fatal(&format!("KVM_RUN: {e}")),
        }

        // Drain host->guest frames on every pass. With the in-kernel
        // irqchip an idle vCPU blocks *inside* KVM_RUN (HLT never
        // reaches userspace), so the TAP fd's SIGIO (see tap.rs) is
        // what forces the EINTR that lands us here; safe to call
        // unconditionally -- a no-op if nothing is pending or no RX
        // buffers are posted (see mmio.rs::poll_queue and net.rs).
        poll_net_rx(mmio_devices, net_transport_idx, net_fd, mem);
    }
}

fn poll_net_rx(
    mmio_devices: &mut [(u64, u64, MmioTransport)],
    idx: usize,
    net_fd: i32,
    mem: &vm_memory::GuestMemoryMmap,
) {
    let mut pfd = libc::pollfd {
        fd: net_fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: pfd is a valid pollfd for the duration of this call.
    let ready = unsafe { libc::poll(&mut pfd, 1, 0) };
    if ready > 0 && (pfd.revents & libc::POLLIN) != 0 {
        mmio_devices[idx].2.poll_queue(0, mem);
    }
}

fn do_freeze(
    vcpu_fd: &kvm_ioctls::VcpuFd,
    vm: &kvm_ioctls::VmFd,
    mem: &vm_memory::GuestMemoryMmap,
    state_dir: &std::path::Path,
    mem_size_bytes: u64,
) {
    eprintln!("cella: freezing to {:?}", state_dir);
    // The guest stopped when KVM_RUN returned. Measure the delay from
    // that moment to the read of the TSC and the kvmclock. The values
    // that the code reads are the values at the read, not at the stop.
    let t_stopped = std::time::Instant::now();

    // Note: KVM_KVMCLOCK_CTRL is not called here. It sets a request on
    // the vCPU, and KVM applies the request at the next update of the
    // pvclock page. This vCPU does not run again, and the process exits.
    // Therefore a call here has no effect. The thaw makes the call, on
    // the new vCPU, before the guest runs.

    if let Err(e) = memory::sync_ram(mem) {
        fatal(&format!("msync guest RAM during freeze: {e}"));
    }

    // Measure the delay between the read of the TSC and the read of the
    // kvmclock. `save` reads the MSRs last, thus the end of `save` is
    // when the code reads the TSC. The thaw must write the two values
    // with the same delay between them. If the two delays are different,
    // the guest sees a step between its TSC and its kvmclock.
    let t_tsc = std::time::Instant::now();
    let vcpu_state =
        vcpu::save(vcpu_fd).unwrap_or_else(|e| fatal(&format!("saving vcpu state: {e:?}")));
    let t_after_save = std::time::Instant::now();
    let clock = vcpu::save_vm_clock(vm).unwrap_or_else(|e| fatal(&format!("saving clock: {e:?}")));
    let t_clock = std::time::Instant::now();
    eprintln!(
        "cella: freeze timing: guest stop to TSC read {} us, TSC read to clock read {} us",
        (t_tsc - t_stopped).as_micros(),
        (t_clock - t_after_save).as_micros()
    );
    let irqchip =
        vcpu::save_irqchip(vm).unwrap_or_else(|e| fatal(&format!("saving irqchip/PIT: {e:?}")));
    let tsc_khz = vcpu_fd.get_tsc_khz().unwrap_or(0);

    let host_check = freeze::HostCheck { tsc_khz };
    freeze::write_state(
        state_dir,
        mem_size_bytes,
        &host_check,
        &vcpu_state,
        &clock,
        &irqchip,
    )
    .unwrap_or_else(|e| fatal(&format!("writing frozen state: {e:?}")));

    eprintln!("cella: frozen");
}

fn install_sigusr1_handler() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_sigusr1 as *const () as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        // Deliberately not SA_RESTART: we want KVM_RUN to return EINTR so
        // the loop notices the freeze flag promptly instead of blocking
        // through it.
        sa.sa_flags = 0;
        libc::sigaction(libc::SIGUSR1, &sa, std::ptr::null_mut());
    }
}

fn install_sigio_handler() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_sigio as *const () as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        // No SA_RESTART, same as SIGUSR1: the whole point is the EINTR
        // that lets the run loop drain TAP RX while the guest is idle.
        sa.sa_flags = 0;
        libc::sigaction(libc::SIGIO, &sa, std::ptr::null_mut());
    }
}

/// Print the contents of a frozen sidecar, then exit. The output shows
/// the fields that control if a thawed guest can continue: the address of
/// the next instruction, the halt state, and the timer and clock state.
fn dump_state(dir: &PathBuf) -> ! {
    let st = match freeze::read_state(dir) {
        Ok(s) => s,
        Err(e) => fatal(&format!("reading state from {dir:?}: {e:?}")),
    };

    println!("state dir:   {dir:?}");
    println!("mem_size:    {} MiB", st.mem_size / (1024 * 1024));
    println!("tsc_khz:     {}", st.tsc_khz);
    println!();
    println!(
        "mp_state:    {} ({})",
        st.vcpu.mp_state.mp_state,
        match st.vcpu.mp_state.mp_state {
            0 => "RUNNABLE",
            3 => "HALTED -- the vCPU was in HLT, so it needs an interrupt to run again",
            other => {
                if other == 1 {
                    "UNINITIALIZED"
                } else {
                    "other"
                }
            }
        }
    );
    println!("rip:         {:#018x}", st.vcpu.regs.rip);
    println!("rsp:         {:#018x}", st.vcpu.regs.rsp);
    println!(
        "rflags:      {:#x} (IF={})",
        st.vcpu.regs.rflags,
        (st.vcpu.regs.rflags >> 9) & 1
    );
    println!(
        "cr0/cr3/cr4: {:#x} / {:#x} / {:#x}",
        st.vcpu.sregs.cr0, st.vcpu.sregs.cr3, st.vcpu.sregs.cr4
    );
    println!("efer:        {:#x}", st.vcpu.sregs.efer);
    println!();
    // Decode the LAPIC timer registers. MSR_IA32_TSC_DEADLINE reads 0
    // when the LVT timer is not in TSC-deadline mode. Therefore a value of
    // 0 does not show that no timer is in operation. The timer mode and
    // the two count registers show if a timer was in operation at the
    // freeze.
    let reg = |off: usize| -> u32 {
        let b = &st.vcpu.lapic.regs;
        u32::from_le_bytes([
            b[off] as u8,
            b[off + 1] as u8,
            b[off + 2] as u8,
            b[off + 3] as u8,
        ])
    };
    let lvtt = reg(0x320);
    let mode = match (lvtt >> 17) & 0x3 {
        0 => "one-shot",
        1 => "periodic",
        2 => "TSC-deadline",
        _ => "reserved",
    };
    println!("LAPIC timer:");
    println!(
        "  LVTT       {lvtt:#010x}  vector {} mode {mode} masked {}",
        lvtt & 0xff,
        (lvtt >> 16) & 1
    );
    println!("  TMICT      {:#010x}  (initial count)", reg(0x380));
    println!(
        "  TMCCT      {:#010x}  (current count -- 0 means nothing is counting down)",
        reg(0x390)
    );
    println!("  TDCR       {:#010x}  (divide config)", reg(0x3e0));
    println!(
        "  SPIV       {:#010x}  (APIC software-enabled bit 8 = {})",
        reg(0xf0),
        (reg(0xf0) >> 8) & 1
    );
    println!();
    println!(
        "kvmclock:    {} ns (flags {:#x})",
        st.clock.clock, st.clock.flags
    );
    println!();
    println!("MSRs:");
    let tsc = st
        .vcpu
        .msrs
        .iter()
        .find(|(i, _)| *i == 0x10)
        .map(|(_, d)| *d)
        .unwrap_or(0);
    for (index, data) in &st.vcpu.msrs {
        let name = match *index {
            0x0000_0010 => "MSR_IA32_TSC",
            0xc000_0080 => "MSR_EFER",
            0x0000_001b => "MSR_IA32_APICBASE",
            0x0000_0174 => "MSR_IA32_SYSENTER_CS",
            0x0000_0175 => "MSR_IA32_SYSENTER_ESP",
            0x0000_0176 => "MSR_IA32_SYSENTER_EIP",
            0xc000_0081 => "MSR_STAR",
            0xc000_0082 => "MSR_LSTAR",
            0xc000_0083 => "MSR_CSTAR",
            0xc000_0084 => "MSR_SYSCALL_MASK",
            0xc000_0102 => "MSR_KERNEL_GS_BASE",
            0x4b56_4d00 => "MSR_KVM_WALL_CLOCK_NEW",
            0x4b56_4d01 => "MSR_KVM_SYSTEM_TIME_NEW",
            0x4b56_4d02 => "MSR_KVM_ASYNC_PF_EN",
            0x4b56_4d03 => "MSR_KVM_STEAL_TIME",
            0x4b56_4d04 => "MSR_KVM_PV_EOI_EN",
            0x0000_06e0 => "MSR_IA32_TSC_DEADLINE",
            _ => "(unknown)",
        };
        print!("  {index:#010x} {name:<24} {data:#018x}");
        if *index == 0x0000_06e0 {
            if *data == 0 {
                print!("  <- ZERO: no timer was armed at freeze, so restoring it arms nothing");
            } else if *data < tsc {
                print!(
                    "  <- already past the frozen TSC ({tsc:#x}): should fire immediately on thaw"
                );
            } else {
                print!("  <- {} TSC ticks in the future", data - tsc);
            }
        }
        println!();
    }
    std::process::exit(0);
}

fn fatal(msg: &str) -> ! {
    eprintln!("cella: fatal: {msg}");
    std::process::exit(1);
}
