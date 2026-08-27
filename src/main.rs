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

    let args = parse_args();
    let frozen = freeze::is_frozen(&args.state_dir);

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
        vcpu::restore(&vcpu_fd, &frozen_state.vcpu)
            .unwrap_or_else(|e| fatal(&format!("restoring vcpu state: {e:?}")));
        vcpu::restore_vm_clock(&vm, &frozen_state.clock)
            .unwrap_or_else(|e| fatal(&format!("restoring clock: {e:?}")));
        freeze::finalize_thaw(&args.state_dir)
            .unwrap_or_else(|e| fatal(&format!("finalizing thaw: {e}")));
        eprintln!("cella: thawed {:?}", args.state_dir);
    } else {
        let kernel = args.kernel.clone().unwrap_or_else(|| {
            usage_error("--kernel is required when booting fresh (no frozen state found)")
        });
        let boot_info = boot::load_kernel(&mem, &kernel, &args.cmdline, mem_size_bytes)
            .unwrap_or_else(|e| fatal(&format!("loading kernel: {e:?}")));
        boot::setup_gdt(&mem, &vcpu_fd).unwrap_or_else(|e| fatal(&format!("gdt: {e:?}")));
        boot::setup_long_mode(&mem, &vcpu_fd, &boot_info, mem_size_bytes)
            .unwrap_or_else(|e| fatal(&format!("long mode setup: {e:?}")));
        eprintln!("cella: booting {:?}", kernel);
    }

    install_sigusr1_handler();
    seccomp::install().unwrap_or_else(|e| fatal(&format!("seccomp: {e}")));

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
                    vcpu::RunResult::Continue => {}
                    vcpu::RunResult::Halted => {
                        // Idle: give the net RX path a chance to run --
                        // see devices/virtio/mmio.rs::poll_queue and
                        // net.rs for why this is safe to call
                        // unconditionally (a no-op if nothing is pending
                        // or no RX buffers are posted).
                        poll_net_rx(mmio_devices, net_transport_idx, net_fd, mem);
                    }
                    vcpu::RunResult::Shutdown => {
                        eprintln!("cella: guest requested shutdown");
                        std::process::exit(0);
                    }
                }
            }
            Err(e) if e.errno() == libc::EINTR => {
                // Either FREEZE_REQUESTED just got set (checked at the
                // top of the next iteration) or a spurious signal;
                // either way, loop back around.
            }
            Err(e) => fatal(&format!("KVM_RUN: {e}")),
        }
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

    let _ = vcpu_fd.kvmclock_ctrl();

    if let Err(e) = memory::sync_ram(mem) {
        fatal(&format!("msync guest RAM during freeze: {e}"));
    }

    let vcpu_state =
        vcpu::save(vcpu_fd).unwrap_or_else(|e| fatal(&format!("saving vcpu state: {e:?}")));
    let clock = vcpu::save_vm_clock(vm).unwrap_or_else(|e| fatal(&format!("saving clock: {e:?}")));
    let tsc_khz = vcpu_fd.get_tsc_khz().unwrap_or(0);

    let host_check = freeze::HostCheck { tsc_khz };
    freeze::write_state(state_dir, mem_size_bytes, &host_check, &vcpu_state, &clock)
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

fn fatal(msg: &str) -> ! {
    eprintln!("cella: fatal: {msg}");
    std::process::exit(1);
}
