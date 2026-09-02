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

use cella::{
    boot, config, devices, doctor, freeze, gateway, ledger, machine, memory, proto, seccomp,
    universe, vcpu, warm,
};

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
// The attached disk of the inspect verb: a second virtio-blk,
// read-only at the device (see docs/LIFECYCLE.md, the universe
// family).
const ATTACH_MMIO_BASE: u64 = 0xd000_2000;
const ATTACH_IRQ: u32 = 7;

static FREEZE_REQUESTED: AtomicBool = AtomicBool::new(false);
static VALVE_KICKED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigusr1(_: libc::c_int) {
    FREEZE_REQUESTED.store(true, Ordering::SeqCst);
}

extern "C" fn on_sigwinch(_: libc::c_int) {
    VALVE_KICKED.store(true, Ordering::SeqCst);
}

// SIGIO from the TAP fd (see tap.rs). The handler's only job is to exist
// without SA_RESTART so KVM_RUN returns EINTR; the run loop drains RX on
// every pass, so there is no flag to set here.
extern "C" fn on_sigio(_: libc::c_int) {}

struct Args {
    state_dir: PathBuf,
    disk: PathBuf,
    disk_ro: bool,
    /// A second disk, read-only at the device, always: the rock of
    /// the inspect verb. No flag makes it writable.
    attach_ro: Option<PathBuf>,
    /// The taps, in order: eth0 is the first. --tap repeats.
    taps: Vec<String>,
    mac: [u8; 6],
    kernel: Option<PathBuf>,
    cmdline: String,
    mem_mb: u64,
    console: Option<PathBuf>,
}

fn parse_args() -> Args {
    let mut state_dir = None;
    let mut disk = None;
    let mut disk_ro = false;
    let mut attach_ro = None;
    let mut taps: Vec<String> = Vec::new();
    let mut mac = [0x02, 0xfc, 0x00, 0x00, 0x00, 0x01];
    let mut kernel = None;
    let mut console = None;
    // The defaults are in cella::config, in one place.
    let mut cmdline = config::default_cmdline();
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
            "--attach-ro" => attach_ro = Some(PathBuf::from(next())),
            "--tap" => taps.push(next()),
            "--mac" => mac = parse_mac(&next()),
            "--kernel" => kernel = Some(PathBuf::from(next())),
            "--console" => {
                if !cfg!(debug_assertions) {
                    usage_error(
                        "the console is a debug affordance -- a release VMM has no \
                         mouth and no ear",
                    );
                }
                console = Some(PathBuf::from(next()))
            }
            "--cmdline" => cmdline = next(),
            "--mem-mb" => {
                mem_mb = next()
                    .parse()
                    .unwrap_or_else(|_| usage_error("--mem-mb must be a number"))
            }
            "-h" | "--help" => usage_error(
                "cella --state-dir DIR --disk PATH [--attach-ro PATH] [--tap NAME] [--kernel PATH --cmdline STR --mem-mb N] [--mac AA:BB:CC:DD:EE:FF] [--disk-ro]",
            ),
            other => usage_error(&format!("unknown argument: {other}")),
        }
    }

    Args {
        state_dir: state_dir.unwrap_or_else(|| usage_error("--state-dir is required")),
        disk: disk.unwrap_or_else(|| usage_error("--disk is required")),
        disk_ro,
        attach_ro,
        taps,
        mac,
        kernel,
        cmdline,
        mem_mb,
        console,
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

/// The lifecycle verbs (see docs/LIFECYCLE.md). The first argument
/// selects a verb; anything else falls through to the legacy flag
/// interface, which the probes and the test scripts use.
fn run_verb(verb: &str, args: &[String]) -> ! {
    // A verb is a CLI citizen: `cella list | head` must not panic.
    // Rust ignores SIGPIPE by default, and println then panics on a
    // closed pipe; the default disposition ends the process quietly.
    // SAFETY: setting a signal disposition before any other work.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let ok = match verb {
        "build" => match args {
            [axis, flavor] => machine::build(axis, flavor),
            [axis, flavor, flag] if flag == "--fresh" => machine::build_flags(axis, flavor, true),
            _ => Err("usage: cella build <kernel|rootfs> <flavor> [--fresh]".to_string()),
        },
        "create" => {
            // cella create <name> [--kernel F] [--rootfs F] [--mem-mb N]
            //   [--net TAP|none] [--root rw|ro]
            let mut it = args.iter();
            let Some(name) = it.next() else {
                fatal("usage: cella create <name> [--kernel F] [--rootfs F] [--mem-mb N] [--net TAP|none] [--root rw|ro]")
            };
            // Precedence: flags, then ~/.cella/config.json, then the
            // built-in defaults.
            let mut m = machine::defaults();
            m.name = name.clone();
            let mut res = Ok(());
            while let Some(a) = it.next() {
                let mut val = |what: &str| {
                    it.next()
                        .cloned()
                        .unwrap_or_else(|| fatal(&format!("missing value for {what}")))
                };
                match a.as_str() {
                    "--kernel" => m.kernel = val("--kernel"),
                    "--rootfs" => m.rootfs = val("--rootfs"),
                    "--mem-mb" => {
                        m.mem_mb = val("--mem-mb")
                            .parse()
                            .unwrap_or_else(|_| fatal("--mem-mb must be a number"))
                    }
                    "--net" => m.net = val("--net"),
                    "--root" => m.root = val("--root"),
                    "--diag" => m.diag = "on".to_string(),
                    other => {
                        res = Err(format!("unknown create option {other:?}"));
                        break;
                    }
                }
            }
            res.and_then(|()| machine::create(&m)).map(|()| {
                let net = machine::read_manifest(&m.name)
                    .map(|r| r.net)
                    .unwrap_or_else(|_| m.net.clone());
                println!(
                    "cella: created machine {:?} at {} (net {net})",
                    m.name,
                    machine::machine_dir(&m.name).display()
                );
            })
        }
        "destroy" => match args {
            [name] => machine::destroy(name).map(|()| {
                println!("cella: destroyed machine {name:?}");
            }),
            _ => Err("usage: cella destroy <name>".to_string()),
        },
        "start" => match args {
            [name] => machine::start(name),
            _ => Err("usage: cella start <name>".to_string()),
        },
        "stop" => match args {
            [name] => machine::stop(name),
            _ => Err("usage: cella stop <name>".to_string()),
        },
        "freeze" => match args {
            [name] => machine::freeze(name),
            _ => Err("usage: cella freeze <name>".to_string()),
        },
        "thaw" => match args {
            [name] => machine::thaw(name),
            _ => Err("usage: cella thaw <name>".to_string()),
        },
        "enter" => match args {
            [name] => machine::enter(name),
            _ => Err("usage: cella enter <name>".to_string()),
        },
        "list" => match args {
            [] => machine::list(),
            _ => Err("usage: cella list".to_string()),
        },
        "info" => match args {
            [name] => machine::info(name),
            _ => Err("usage: cella info <name>".to_string()),
        },
        "selftest" => machine::selftest(),
        "gateway" => {
            let usage = "usage: cella gateway <vm> <show [--all] | release <id> | refuse <id> [--why TEXT] | open | close>";
            let (Some(vm), Some(verb)) = (args.first(), args.get(1)) else {
                usage_error(usage)
            };
            let rest = &args[2..];
            match verb.as_str() {
                "show" => gateway::show(vm, rest.first().map(|s| s.as_str()) == Some("--all")),
                "release" => match rest {
                    [id] => gateway::release(vm, id),
                    _ => usage_error(usage),
                },
                "refuse" => match rest {
                    [id] => gateway::refuse(vm, id, "refused"),
                    [id, flag, why] if flag == "--why" => gateway::refuse(vm, id, why),
                    _ => usage_error(usage),
                },
                "close" => gateway::close(vm),
                "open" => gateway::open(vm),
                _ => usage_error(usage),
            }
        }
        "branch" => match args {
            [src, dst] => universe::branch(src, dst),
            _ => Err("usage: cella branch <existing-vm> <new-vm>".to_string()),
        },
        "archive" => match args {
            [vm] => universe::archive(vm),
            _ => Err("usage: cella archive <vm>".to_string()),
        },
        "inspect" => match args {
            [vm] => universe::inspect(vm),
            _ => Err("usage: cella inspect <vm>".to_string()),
        },
        // The thin CLIs, through the dispatcher: cella <name> ...
        // execs the sibling cella-<name>, thus the user surface is
        // one word while the binaries keep their own confinement.
        "probe" | "network" => {
            use std::os::unix::process::CommandExt;
            let bin = sibling_cli(&format!("cella-{verb}"));
            let err = std::process::Command::new(&bin).args(args).exec();
            fatal(&format!("exec {bin}: {err}"));
        }
        "doctor" => {
            let failed = match args.first().map(|s| s.as_str()) {
                Some("check") | None => doctor::check(),
                Some("fix") => doctor::fix(),
                Some("gate") => doctor::gate(&args[1..]),
                Some("verify") => match &args[1..] {
                    [] => doctor::verify(None),
                    [vm] => doctor::verify_machine(vm),
                    [axis, flavor] => doctor::verify(Some((axis, flavor))),
                    _ => usage_error("usage: cella doctor verify [<vm> | kernel|rootfs <flavor>]"),
                },
                _ => usage_error("usage: cella doctor [check|fix|verify|gate <needs...>]"),
            };
            if failed > 0 {
                std::process::exit(1);
            }
            Ok(())
        }
        "setup" => match args.first().map(|s| s.as_str()) {
            Some("net") => {
                let mut taps = 4u32;
                let mut first = 0u32;
                let mut it = args[1..].iter();
                while let Some(a) = it.next() {
                    match a.as_str() {
                        "--taps" => {
                            taps = it
                                .next()
                                .and_then(|v| v.parse().ok())
                                .unwrap_or_else(|| usage_error("--taps needs a number"));
                        }
                        "--from" => {
                            first = it
                                .next()
                                .and_then(|v| v.parse().ok())
                                .unwrap_or_else(|| usage_error("--from needs a number"));
                        }
                        other => usage_error(&format!("unknown setup net option: {other}")),
                    }
                }
                machine::setup_net(taps, first)
            }
            _ => Err("usage: sudo cella setup net [--taps N]".to_string()),
        },
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        _ => unreachable!(),
    };
    match ok {
        Ok(()) => std::process::exit(0),
        Err(e) => fatal(&e),
    }
}

fn print_help() {
    println!(
        "cella -- a cryogenic chamber for agents\n\n\
         The machine lifecycle (see docs/LIFECYCLE.md):\n\
         \x20 cella build <kernel|rootfs> <flavor> [--fresh]   make a golden artifact\n\
         \x20 cella create <name> [options]          stage a machine from the goldens\n\
         \x20 cella start <name>                     run it (detached, jailed)\n\
         \x20 cella enter <name>                     attach to its console (Ctrl-] detaches)\n\
         \x20 cella freeze <name>                    stop it and keep the instant\n\
         \x20 cella thaw <name>                      resume the instant\n\
         \x20 cella stop <name>                      end it fast, clear the transients\n\
         \x20 cella destroy <name>                   delete it, once and for all\n\
         \x20 cella list                             every machine, one line each\n\
         \x20 cella info <name>                      everything about one machine\n\
         \x20 cella selftest                         run the lifecycle cycle end to end\n\
         \x20 sudo $(which cella) setup net --taps N provision the tap pool + NAT (the one root verb)\n\n\
         create options: --kernel F --rootfs F --mem-mb N --net TAP|auto|none --root rw|ro --diag\n\
         Defaults live in ~/.cella/config.json; flags override them.\n\n\
         The flag interface (--state-dir ...) stays for the probes and the tests."
    );
}

/// The persona: the basename this binary was invoked as. The thin
/// CLIs of the map are names of one multi-call binary (install.sh
/// makes the symlinks); each name admits only its own verbs, and the
/// confinement of the shakedown branch attaches per name. Two stay
/// real separate binaries: cella-network (a file capability binds to
/// an inode, and only that inode may hold it) and cella-probe.
fn persona() -> String {
    let name = std::env::args()
        .next()
        .as_deref()
        .map(std::path::Path::new)
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "cella".to_string());
    // The lab flavor carries the -debug suffix (cella-debug and its
    // personas), thus the two flavors never shadow each other on
    // PATH; the persona is the name under the suffix.
    name.strip_suffix("-debug")
        .map(String::from)
        .unwrap_or(name)
}

const UNIVERSE_VERBS: &[&str] = &["branch", "archive", "inspect"];
const MACHINE_VERBS: &[&str] = &[
    "create", "start", "stop", "enter", "freeze", "thaw", "destroy", "list", "info", "selftest",
];

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match persona().as_str() {
        "cella-machine" => {
            let Some(first) = argv.first() else {
                usage_error("usage: cella-machine <create|start|stop|enter|freeze|thaw|destroy|list|info|selftest> ...")
            };
            if MACHINE_VERBS.contains(&first.as_str()) {
                run_verb(first.as_str(), &argv[1..]);
            }
            usage_error(&format!(
                "cella-machine does not own the verb {first:?} -- its verbs: {}",
                MACHINE_VERBS.join(", ")
            ));
        }
        "cella-gateway" => {
            let Some(_) = argv.first() else {
                usage_error("usage: cella-gateway <vm> <show|release|refuse|open|close> ...")
            };
            run_verb("gateway", &argv);
        }
        "cella-universe" => {
            let Some(first) = argv.first() else {
                usage_error("usage: cella-universe <branch|archive|inspect> ...")
            };
            if UNIVERSE_VERBS.contains(&first.as_str()) {
                run_verb(first.as_str(), &argv[1..]);
            }
            usage_error(&format!(
                "cella-universe does not own the verb {first:?} -- its verbs: {}",
                UNIVERSE_VERBS.join(", ")
            ));
        }
        "cella-build" => run_verb("build", &argv),
        "cella-doctor" => run_verb("doctor", &argv),
        "cella-vmm" => {
            // The flag interface only: the jailed run loop. Every
            // verb belongs to another name.
        }
        _ => {}
    }
    if persona() != "cella-vmm" && argv.is_empty() {
        print_help();
        std::process::exit(0);
    }
    if let Some(first) = argv.first() {
        if matches!(
            first.as_str(),
            "build"
                | "create"
                | "destroy"
                | "start"
                | "stop"
                | "freeze"
                | "thaw"
                | "enter"
                | "list"
                | "info"
                | "selftest"
                | "setup"
                | "doctor"
                | "probe"
                | "network"
                | "gateway"
                | "branch"
                | "archive"
                | "inspect"
                | "help"
                | "--help"
                | "-h"
        ) {
            run_verb(first.clone().as_str(), &argv[1..]);
        }
    }

    // Hidden self-test hook for `make test-seccomp`: install the real
    // filter and deliberately trip it. See seccomp::selftest_provoke_kill
    // for what the harness expects to observe.
    if std::env::args().nth(1).as_deref() == Some("--selftest-seccomp") {
        seccomp::selftest_provoke_kill();
    }

    // The shell scripts build their own command line, because they add
    // the root filesystem and the virtio devices. They read the defaults
    // from here, so that the values are not written a second time.
    if std::env::args().nth(1).as_deref() == Some("--print-default-cmdline") {
        println!("{}", config::default_cmdline());
        std::process::exit(0);
    }
    if std::env::args().nth(1).as_deref() == Some("--print-time-args") {
        println!("{}", config::DEFAULT_TIME_ARGS);
        std::process::exit(0);
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

    // Print the chronicle of one ledger file, one line per event.
    // There is no protobuf tool in the guest or in a test script, so
    // this option is how a gate (or a person) examines it.
    if std::env::args().nth(1).as_deref() == Some("--dump-ledger") {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| usage_error("--dump-ledger needs a ledger file path"));
        dump_ledger(&PathBuf::from(path));
    }

    let args = parse_args();
    let frozen = freeze::is_frozen(&args.state_dir);
    // The chronicle of held operations (see docs/NETWORK-MODEL.md,
    // "The control plane"). Read at thaw, to rebind restored frames
    // to the ids the ledger still holds open; appended to on every
    // run-loop pass thereafter.
    let ledger_path = args.state_dir.join("network").join("ledger");
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
    let mut vcpu_fd =
        vcpu::create_vcpu(&vm, &cpuid).unwrap_or_else(|e| fatal(&format!("create vcpu: {e:?}")));

    let vm = Arc::new(vm);
    let irq_raiser: Arc<dyn devices::virtio::mmio::IrqLine> = vm.clone();
    let guest_clock: Arc<dyn ledger::GuestClock> = vm.clone();

    let block = Block::new(&args.disk, args.disk_ro)
        .unwrap_or_else(|e| fatal(&format!("open disk {:?}: {e}", args.disk)));
    let mut mmio_devices: Vec<(u64, u64, MmioTransport)> = vec![(
        BLOCK_MMIO_BASE,
        MMIO_LEN,
        MmioTransport::new(Box::new(block), irq_raiser.clone(), BLOCK_IRQ),
    )];
    if let Some(path) = &args.attach_ro {
        // Read-only at the device: the guest sees VIRTIO_BLK_F_RO,
        // and the backend refuses writes regardless.
        let attached = Block::new(path, true)
            .unwrap_or_else(|e| fatal(&format!("open attached disk {path:?}: {e}")));
        mmio_devices.push((
            ATTACH_MMIO_BASE,
            MMIO_LEN,
            MmioTransport::new(Box::new(attached), irq_raiser.clone(), ATTACH_IRQ),
        ));
    }
    // The network is optional, and a machine takes N taps: eth<i>
    // is the i-th --tap. net0 keeps its address and IRQ; each later
    // nic takes base + i*0x2000 and IRQ 6 + 2*i, thus the attach
    // slot between them (0xd0002000:7) never moves -- the ABI of
    // every existing manifest holds. A guest without --tap gets the
    // block device only.
    let mut net_poll: Vec<(usize, i32)> = Vec::new();
    for (i, tap) in args.taps.iter().enumerate() {
        // Must precede Tap::open (inside Net::new): the TAP fd is
        // O_ASYNC, and SIGIO's default action is to terminate the
        // process. A frame can arrive the instant the fd exists.
        install_sigio_handler();
        let mut mac = args.mac;
        mac[5] = mac[5].wrapping_add(i as u8);
        let net = Net::new(tap, mac, guest_clock.clone())
            .unwrap_or_else(|e| fatal(&format!("open tap {tap:?}: {e}")));
        let net_fd = net.tap_fd();
        mmio_devices.push((
            NET_MMIO_BASE + (i as u64) * 0x2000,
            MMIO_LEN,
            MmioTransport::new(Box::new(net), irq_raiser.clone(), NET_IRQ + 2 * i as u32),
        ));
        net_poll.push((mmio_devices.len() - 1, net_fd));
    }

    // The valve automaton's posture, at birth: the record beside the
    // machine, written by the gateway verbs alone. Born closed when
    // no record exists; the posture holds across any number of
    // freezes and thaws until the opposite verb (see
    // docs/FREEZE-THAW.md, "The two automata").
    apply_valve_record(&args.state_dir, &mut mmio_devices);

    // The console socket. The listener binds before the seccomp
    // filter, thus socket(2) stays outside the allowlist (the canary
    // of test-seccomp); only accept4 runs at runtime. The client
    // handle is shared with the serial output, which tees to it.
    let console_client: devices::serial::ConsoleClient =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let console_listener = args.console.as_ref().map(|path| {
        let _ = std::fs::remove_file(path);
        // A unix socket path caps at ~108 bytes. Bind by the file name
        // from inside the parent directory, thus any home path works.
        // Single-threaded startup: the chdir dance is safe here.
        let cwd = std::env::current_dir().unwrap_or_else(|e| fatal(&format!("cwd: {e}")));
        let parent = path
            .parent()
            .unwrap_or_else(|| fatal("console path has no parent"));
        std::env::set_current_dir(parent)
            .unwrap_or_else(|e| fatal(&format!("entering {parent:?}: {e}")));
        let l = std::os::unix::net::UnixListener::bind(path.file_name().unwrap())
            .unwrap_or_else(|e| fatal(&format!("binding the console socket {path:?}: {e}")));
        std::env::set_current_dir(&cwd)
            .unwrap_or_else(|e| fatal(&format!("returning to {cwd:?}: {e}")));
        l.set_nonblocking(true).expect("nonblocking listener");
        set_async(&l);
        l
    });

    let mut serial = SerialDevice::new(vm.clone(), console_client.clone());
    // Serial input needs the SIGIO handler even without a TAP.
    install_sigio_handler();
    setup_stdin_async();

    if frozen {
        // Warm the stage-2 mappings before the restore of the clock,
        // so that the cost stays out of the clock window of the guest.
        // "ept" fills the tables of the direct host with an ioctl.
        // "deep" also runs the warming stub, and that reaches every
        // layer below (see src/warm.rs and the measurements in
        // config::DEFAULT_THAW_PREFAULT).
        let mode = std::env::var("CELLA_THAW_PREFAULT")
            .unwrap_or_else(|_| config::DEFAULT_THAW_PREFAULT.to_string());
        match mode.as_str() {
            "off" => {}
            "ept" => {
                prefault_ept(&vcpu_fd, mem_size_bytes);
            }
            _ => {
                prefault_ept(&vcpu_fd, mem_size_bytes);
                warm::warm_stage2(&vm, &mut vcpu_fd, mem_size_bytes);
            }
        }
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
            "cella: thaw timing: TSC write to clock write {}",
            fmt_ns((t_after_clock - t_after_restore).as_nanos() as i128)
        );
        thaw_clock_written = Some(t_after_clock);
        // The code above made a new irqchip and a new PIT. This call puts
        // back the IOAPIC routing and the PIT programming that the guest
        // set before the freeze. Without this call, a halted guest waits
        // for a timer interrupt that does not occur.
        serial = SerialDevice::restore(vm.clone(), frozen_state.serial, console_client.clone());
        vcpu::restore_irqchip(&vm, &frozen_state.irqchip)
            .unwrap_or_else(|e| fatal(&format!("restoring irqchip/PIT: {e:?}")));
        // A guest that runs KVM itself froze with inner-VM entry state
        // in the host kernel. Put it back before the first KVM_RUN, or
        // the guest triple faults on its next VM entry.
        if !frozen_state.nested.is_empty() {
            vcpu::restore_nested(&vcpu_fd, &frozen_state.nested)
                .unwrap_or_else(|e| fatal(&format!("restoring nested state: {e:?}")));
        }
        vcpu::restore_nested_msrs(&vcpu_fd, &frozen_state.nested_msrs)
            .unwrap_or_else(|e| fatal(&format!("restoring nested MSRs: {e:?}")));
        // The transports above came up at reset state, and the guest
        // driver in RAM holds a negotiated state. Put the device side
        // back before the first KVM_RUN, or the first request lands in
        // a ring the device never reads (see docs/DEVICE-STATE.md).
        if frozen_state.devices.len() != mmio_devices.len() {
            fatal(&format!(
                "refusing to thaw: the image froze with {} virtio device(s), \
                 and this command line makes {} (a machine frozen with a tap \
                 must thaw with a tap)",
                frozen_state.devices.len(),
                mmio_devices.len()
            ));
        }
        // The still-open operations of the machine's own ledger,
        // read once before the per-transport restore: the index a
        // restored frame rejoins by destination, so its id survives
        // the freeze instead of a remint (see docs/NETWORK-MODEL.md,
        // "Egress parks for decisions"). The clock is already the
        // restored frozen instant, thus a genuine ledger gap (none
        // expected in normal operation) mints in the guest's frame.
        let open_ops = ledger::open_operations(&ledger_path)
            .unwrap_or_else(|e| fatal(&format!("reading the ledger at thaw: {e}")));
        for (st, (_, _, transport)) in frozen_state.devices.iter().zip(mmio_devices.iter_mut()) {
            transport.restore_state(st, &open_ops);
        }
        // No unconditional delivery: the freeze verdict means the
        // world grows and the hold resumes. The decisions that
        // arrived while the machine slept apply now, in park order
        // -- a released operation delivers, an undecided one stays
        // held (see docs/NETWORK-MODEL.md, "Release names an id").
        apply_verdicts(&args.state_dir, &mut mmio_devices, &mem);
        // Deliberately no KVM_KVMCLOCK_CTRL. That call sets
        // PVCLOCK_GUEST_STOPPED in the pvclock page, and the flag tells
        // the guest that it was stopped. The freeze must not exist for
        // the guest, thus cella does not send the flag. The guest does
        // not need it either:
        // - The clocksource watchdog does not run, because the command
        //   line contains tsc=reliable (see src/config.rs).
        // - The soft-lockup, RCU-stall, and hung-task watchdogs measure
        //   elapsed guest time. The clock of the guest does not advance
        //   across the freeze, thus the watchdogs see no pause.
        // An earlier version sent the flag to stop the guest from
        // marking its TSC unstable. That symptom came from the era in
        // which the thaw skewed the clock by 8 ms to 23 ms, before
        // tsc=reliable and before the paired TSC and kvmclock restore.
        // The probes now verify the absence of watchdog complaints
        // after each thaw.

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
            "cella: thaw timing: clock write to first KVM_RUN {}",
            fmt_ns(t.elapsed().as_nanos() as i128)
        );
    }

    // Readiness for the start verb: one line on the inherited pipe,
    // immediately before the first KVM_RUN. See machine::start.
    if let Ok(fd) = std::env::var("CELLA_READY_FD") {
        if let Ok(fd) = fd.parse::<i32>() {
            // SAFETY: the fd comes from the parent's pipe and belongs
            // to this process; write and close are its whole use.
            unsafe {
                libc::write(fd, b"ready\n".as_ptr() as *const libc::c_void, 6);
                libc::close(fd);
            }
        }
    }

    run_loop(
        vcpu_fd,
        &vm,
        &mem,
        &mut serial,
        &mut mmio_devices,
        &net_poll,
        console_listener,
        console_client,
        &args.state_dir,
        &ledger_path,
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
    net_poll: &[(usize, i32)],
    console_listener: Option<std::os::unix::net::UnixListener>,
    console_client: devices::serial::ConsoleClient,
    state_dir: &std::path::Path,
    ledger_path: &std::path::Path,
    mem_size_bytes: u64,
) {
    loop {
        if VALVE_KICKED.swap(false, Ordering::SeqCst) {
            // The live path carries valve edges alone. Decisions
            // stage in the verdict file and the thaw edge applies
            // them: under one-shot, a running machine froze at its
            // first park, thus it never holds anything to deliver.
            apply_valve_record(state_dir, mmio_devices);
        }
        if FREEZE_REQUESTED.load(Ordering::SeqCst) {
            let device_states: Vec<_> = mmio_devices
                .iter()
                .map(|(_, _, t)| t.save_state())
                .collect();
            let held: usize = device_states.iter().map(|d| d.held_frames.len()).sum();
            if held > 0 {
                eprintln!("cella: freezing with {held} held egress frame(s)");
            }
            do_freeze(
                &vcpu_fd,
                vm,
                mem,
                state_dir,
                mem_size_bytes,
                serial.registers(),
                &device_states,
            );
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
        for (idx, net_fd) in net_poll {
            poll_net_rx(mmio_devices, *idx, *net_fd, mem);
        }
        // Drain host stdin into the serial RX FIFO on every pass. Stdin
        // carries O_ASYNC (see setup_stdin_async), thus a keystroke
        // raises SIGIO, KVM_RUN returns with EINTR, and the byte lands
        // here even when the guest is idle in HLT.
        poll_stdin_rx(serial);
        // The console socket: accept a client, and drain its input into
        // the serial RX FIFO. The accepted stream carries O_ASYNC, thus
        // a keystroke from the client wakes an idle guest the same way
        // stdin does.
        if let Some(listener) = &console_listener {
            poll_console(listener, &console_client, serial);
        }
        // One-shot: the park is the freeze. The pass completes (the
        // whole TX batch drained above, thus every destination of
        // the batch parked), the ledger flushes, and the machine
        // stops before the guest runs again. No accumulation exists:
        // latency is spent frozen, never waiting awake, and the
        // engine always works against a frozen machine.
        if flush_ledger(mmio_devices, ledger_path) {
            eprintln!("cella: parked -- the machine freezes (one-shot)");
            FREEZE_REQUESTED.store(true, Ordering::SeqCst);
        }
    }
}

/// Drain every device's pending ledger events and append them to the
/// chronicle. Returns true when a new operation parked this pass:
/// the one-shot trigger reads it. The flush precedes any freeze,
/// thus the Parked events are on disk before the sidecar exists --
/// the rebinding at thaw depends on this order.
fn flush_ledger(
    mmio_devices: &mut [(u64, u64, MmioTransport)],
    ledger_path: &std::path::Path,
) -> bool {
    let mut events = Vec::new();
    for (_, _, t) in mmio_devices.iter_mut() {
        events.extend(t.drain_ledger_events());
    }
    // The freeze trigger: any parked frame, not any Parked event. A
    // frame that joins an existing held operation emits no event,
    // and the park is the freeze for joins too (the one-shot rule).
    let mut parked_any = false;
    for (_, _, t) in mmio_devices.iter_mut() {
        if t.take_parked_flag() {
            parked_any = true;
        }
    }
    for event in events {
        let msg = ledger::event_message(event);
        if let Err(e) = ledger::append(ledger_path, &msg) {
            eprintln!("cella: ledger append failed: {e}");
        }
    }
    parked_any
}

/// Read pending host stdin bytes into the serial device. Non-blocking:
/// stdin carries O_NONBLOCK, and a pass with no input costs one poll.
fn poll_stdin_rx(serial: &mut SerialDevice) {
    let mut pfd = libc::pollfd {
        fd: 0,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: pfd is a valid pollfd for the duration of this call.
    let ready = unsafe { libc::poll(&mut pfd, 1, 0) };
    if ready > 0 && (pfd.revents & libc::POLLIN) != 0 {
        let mut buf = [0u8; 64];
        // SAFETY: buf is a valid writable buffer of the given length.
        let n = unsafe { libc::read(0, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n > 0 {
            serial.enqueue(&buf[..n as usize]);
        }
    }
}

/// Give stdin O_ASYNC and O_NONBLOCK, so that a keystroke interrupts
/// KVM_RUN the same way a TAP frame does (see tap.rs), and so that the
/// run-loop read never blocks. Runs before the seccomp filter.
fn setup_stdin_async() {
    // SAFETY: fcntl on fd 0 with valid commands.
    unsafe {
        libc::fcntl(0, libc::F_SETOWN, libc::getpid());
        let flags = libc::fcntl(0, libc::F_GETFL);
        libc::fcntl(0, libc::F_SETFL, flags | libc::O_ASYNC | libc::O_NONBLOCK);
    }
}

/// Accept a console client and drain its input. One client at a
/// time: a new connection replaces a dead one, and a second live
/// client is refused by the accept order (the first stays).
fn poll_console(
    listener: &std::os::unix::net::UnixListener,
    client: &devices::serial::ConsoleClient,
    serial: &mut SerialDevice,
) {
    if client.borrow().is_none() {
        if let Ok((stream, _)) = listener.accept() {
            let _ = stream.set_nonblocking(true);
            set_async(&stream);
            *client.borrow_mut() = Some(stream);
        }
    }
    let mut drop_client = false;
    if let Some(stream) = client.borrow_mut().as_mut() {
        use std::io::Read;
        let mut buf = [0u8; 64];
        match stream.read(&mut buf) {
            Ok(0) => drop_client = true,
            Ok(n) => serial.enqueue(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => drop_client = true,
        }
    }
    if drop_client {
        *client.borrow_mut() = None;
    }
}

/// Give a socket O_ASYNC with this process as the owner, so that its
/// readiness raises SIGIO and interrupts KVM_RUN (see tap.rs for the
/// same pattern).
fn set_async<F: std::os::fd::AsRawFd>(f: &F) {
    let fd = f.as_raw_fd();
    // SAFETY: fcntl on an owned fd with valid commands.
    unsafe {
        libc::fcntl(fd, libc::F_SETOWN, libc::getpid());
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_ASYNC);
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

/// The decisions, from outside: read the verdict file's framed
/// Decision messages, and let every device resolve whatever they
/// let resolve right now, oldest-parked first (see
/// docs/NETWORK-MODEL.md, "Release names an id"). Release-all does
/// not exist: a decision names one operation, and an operation
/// behind an undecided predecessor waits its turn. The engine
/// appends to the file and sends SIGWINCH. Reapplying an
/// already-seen decision is harmless: it names an id no longer at
/// the front of any device's queue, and nothing happens for it.
/// The valve automaton's record, read from beside the machine: the
/// gateway verbs alone write it, and the posture holds across any
/// number of freezes and thaws until the opposite verb (see
/// docs/FREEZE-THAW.md, "The two automata"). A missing record is
/// the birth state: closed.
fn apply_valve_record(state_dir: &std::path::Path, mmio_devices: &mut [(u64, u64, MmioTransport)]) {
    let word = std::fs::read_to_string(state_dir.join("valve"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let state = if word == "open" {
        devices::virtio::ValveState::Open
    } else {
        devices::virtio::ValveState::Closed
    };
    for (_, _, t) in mmio_devices.iter_mut() {
        t.set_valve(state);
    }
    eprintln!("cella: valve {:?}", state);
}

/// The staged decisions of the verdict file, at the thaw edge: they
/// apply in park order.
fn apply_verdicts(
    state_dir: &std::path::Path,
    mmio_devices: &mut [(u64, u64, MmioTransport)],
    mem: &vm_memory::GuestMemoryMmap,
) {
    let messages = ledger::read_all(&state_dir.join("verdict")).unwrap_or_default();
    let mut decisions: std::collections::HashMap<Vec<u8>, proto::Decision> =
        std::collections::HashMap::new();
    for msg in &messages {
        if let Some(proto::message::Body::Decision(d)) = &msg.body {
            decisions.insert(d.id.clone(), d.clone());
        }
    }
    eprintln!(
        "cella: applying {} decision(s) from the verdict file",
        decisions.len()
    );
    for (_, _, t) in mmio_devices.iter_mut() {
        t.apply_decisions(&decisions, mem);
    }
    // The bookkeeping lapse: a refusal whose id is open in the
    // chronicle but held by no device closes the book and touches
    // no frame. The case exists after a collision melt (the
    // never-guess rebind re-mints, and the stale ids stay open
    // with nothing behind them); the engine refuses them, and the
    // chronicle closes.
    let held: std::collections::HashSet<Vec<u8>> = mmio_devices
        .iter()
        .flat_map(|(_, _, t)| t.held_op_ids())
        .collect();
    let ledger_path = state_dir.join("network").join("ledger");
    let open: std::collections::HashSet<Vec<u8>> = ledger::open_operations(&ledger_path)
        .map(|ops| ops.into_iter().map(|o| o.id).collect())
        .unwrap_or_default();
    for (id, decision) in &decisions {
        let Some(proto::decision::Decision::Refusal(refusal)) = &decision.decision else {
            continue;
        };
        if held.contains(id) || !open.contains(id) {
            continue;
        }
        let msg = ledger::event_message(proto::Event {
            event: Some(proto::event::Event::Lapsed(proto::Lapsed {
                id: id.clone(),
                why: refusal.why.clone(),
            })),
        });
        if let Err(e) = ledger::append(&ledger_path, &msg) {
            eprintln!("cella: ledger append failed: {e}");
        }
        eprintln!(
            "cella: lapsed {} by the book -- refused, and held by nothing",
            ledger::hex(id)
        );
    }
}

fn do_freeze(
    vcpu_fd: &kvm_ioctls::VcpuFd,
    vm: &kvm_ioctls::VmFd,
    mem: &vm_memory::GuestMemoryMmap,
    state_dir: &std::path::Path,
    mem_size_bytes: u64,
    serial_regs: [u8; 9],
    device_states: &[devices::virtio::mmio::TransportState],
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
        "cella: freeze timing: guest stop to TSC read {}, TSC read to clock read {}",
        fmt_ns((t_tsc - t_stopped).as_nanos() as i128),
        fmt_ns((t_clock - t_after_save).as_nanos() as i128)
    );
    let irqchip =
        vcpu::save_irqchip(vm).unwrap_or_else(|e| fatal(&format!("saving irqchip/PIT: {e:?}")));
    // The guest can run KVM itself: the entry state of its inner VM
    // lives in the host kernel, not in guest RAM. Without this block a
    // thawed nested host triple faults on its next VM entry.
    let nested = vcpu::save_nested(vcpu_fd)
        .unwrap_or_else(|e| fatal(&format!("saving nested state: {e:?}")));
    let nested_msrs = vcpu::save_nested_msrs(vcpu_fd);
    if !nested.is_empty() {
        eprintln!(
            "cella: nested state: {} bytes, {} MSR(s)",
            nested.len(),
            nested_msrs.len()
        );
    }
    let tsc_khz = vcpu_fd.get_tsc_khz().unwrap_or(0);

    let frozen = freeze::FrozenState {
        mem_size: mem_size_bytes,
        serial: serial_regs,
        tsc_khz,
        vcpu: vcpu_state,
        clock,
        irqchip,
        devices: device_states.to_vec(),
        nested,
        nested_msrs,
    };
    freeze::write_state(state_dir, &frozen)
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
        // SIGWINCH is the wire under the live valve edges (open
        // and close against a running machine); decisions ride the
        // verdict file and apply at the thaw.
        let mut sa3: libc::sigaction = std::mem::zeroed();
        sa3.sa_sigaction = on_sigwinch as *const () as usize;
        libc::sigemptyset(&mut sa3.sa_mask);
        sa3.sa_flags = 0;
        libc::sigaction(libc::SIGWINCH, &sa3, std::ptr::null_mut());
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
/// A sibling thin CLI: beside this binary when present, else PATH.
fn sibling_cli(name: &str) -> String {
    if let Ok(me) = std::env::current_exe() {
        let p = me.parent().unwrap().join(name);
        if p.is_file() {
            return p.to_string_lossy().to_string();
        }
    }
    name.to_string()
}

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
    // Decode the pvclock page of the guest. MSR_KVM_SYSTEM_TIME_NEW holds
    // the guest physical address of the page in its upper bits, and bit 0
    // enables the page. The page is in guest RAM, thus the freeze image
    // contains it.
    //
    // The flags byte is the important field. Linux runs the clocksource
    // watchdog against the TSC only when PVCLOCK_TSC_STABLE_BIT is not
    // set. If KVM clears that bit at a thaw, the guest starts to compare
    // the TSC against kvm-clock, and a small step then marks the TSC
    // unstable.
    let system_time_msr = st
        .vcpu
        .msrs
        .iter()
        .find(|(i, _)| *i == 0x4b56_4d01)
        .map(|(_, d)| *d)
        .unwrap_or(0);
    println!();
    println!(
        "serial:      IER={:#04x} IIR={:#04x} LCR={:#04x} LSR={:#04x} MCR={:#04x} MSR={:#04x}",
        st.serial[2], st.serial[3], st.serial[4], st.serial[5], st.serial[6], st.serial[7]
    );
    println!();
    println!("pvclock page (from MSR_KVM_SYSTEM_TIME_NEW):");
    if system_time_msr & 1 == 0 {
        println!("  the page is not enabled");
    } else {
        let gpa = system_time_msr & !1u64;
        println!("  guest physical address: {gpa:#x}");
        match std::fs::File::open(freeze::ram_path(dir)) {
            Ok(mut f) => {
                use std::io::{Read, Seek, SeekFrom};
                let mut buf = [0u8; 32];
                if f.seek(SeekFrom::Start(gpa)).is_ok() && f.read_exact(&mut buf).is_ok() {
                    let u32at = |o: usize| u32::from_le_bytes(buf[o..o + 4].try_into().unwrap());
                    let u64at = |o: usize| u64::from_le_bytes(buf[o..o + 8].try_into().unwrap());
                    let flags = buf[30];
                    println!("  version:           {}", u32at(0));
                    println!("  tsc_timestamp:     {:#x}", u64at(8));
                    println!("  system_time:       {} ns", u64at(16));
                    println!("  tsc_to_system_mul: {:#x}", u32at(24));
                    println!("  tsc_shift:         {}", buf[28] as i8);
                    println!("  flags:             {flags:#04x}");
                    // State what the bit means, and do not state what the
                    // guest does. The two are no longer the same: cella
                    // passes tsc=reliable, thus the guest does not run the
                    // watchdog even when this bit is clear.
                    if flags & 1 != 0 {
                        println!(
                            "    TSC_STABLE    set. The host KVM declares the TSC of the \
                             guest stable."
                        );
                    } else {
                        println!(
                            "    TSC_STABLE    not set. The host KVM does not declare the \
                             TSC of the guest"
                        );
                        println!(
                            "                  stable, because the TSC of the host is not \
                             stable. cella"
                        );
                        println!(
                            "                  cannot change this bit: KVM writes the page \
                             at every"
                        );
                        println!(
                            "                  update. Linux runs the clocksource watchdog \
                             against the TSC"
                        );
                        println!(
                            "                  when this bit is clear, unless the command \
                             line contains"
                        );
                        println!(
                            "                  tsc=reliable or tsc=nowatchdog. cella passes \
                             tsc=reliable"
                        );
                        println!("                  (see src/config.rs).");
                    }
                    println!(
                        "    GUEST_STOPPED {}",
                        if flags & 2 != 0 { "set" } else { "not set" }
                    );
                } else {
                    println!("  could not read the page from the RAM file");
                }
            }
            Err(e) => println!("  could not open the RAM file: {e}"),
        }
    }

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
            0x0000_0da0 => "MSR_IA32_XSS",
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

/// Print every framed Message of a ledger file, one line per event.
fn dump_ledger(path: &PathBuf) -> ! {
    let messages =
        ledger::read_all(path).unwrap_or_else(|e| fatal(&format!("reading {path:?}: {e}")));
    for msg in &messages {
        let Some(proto::message::Body::Event(ev)) = &msg.body else {
            println!("(not an event)");
            continue;
        };
        match &ev.event {
            Some(proto::event::Event::Parked(op)) => {
                let dest = op
                    .destination
                    .as_ref()
                    .map(ledger::Dest::from_message)
                    .map(|d| match d {
                        ledger::Dest::Ipv4 { ip, port, .. } => {
                            format!("ip={}.{}.{}.{} port={port}", ip[0], ip[1], ip[2], ip[3])
                        }
                        l2 => format!("l2={l2}"),
                    })
                    .unwrap_or_default();
                println!(
                    "parked id={} {dest} guest_ns={} host_ns={}",
                    ledger::hex(&op.id),
                    op.guest_ns,
                    op.host_ns
                );
            }
            Some(proto::event::Event::Released(r)) => println!(
                "released id={} first_response_ns={} bytes_in={} bytes_out={}",
                ledger::hex(&r.id),
                r.first_response_ns,
                r.bytes_in,
                r.bytes_out
            ),
            Some(proto::event::Event::Lapsed(l)) => {
                println!("lapsed id={} why={:?}", ledger::hex(&l.id), l.why)
            }
            None => println!("(empty event)"),
        }
    }
    std::process::exit(0);
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

fn fatal(msg: &str) -> ! {
    eprintln!("cella: fatal: {msg}");
    std::process::exit(1);
}

fn prefault_ept(vcpu: &kvm_ioctls::VcpuFd, size: u64) {
    use std::os::fd::AsRawFd;
    #[repr(C)]
    struct KvmPreFaultMemory {
        gpa: u64,
        size: u64,
        flags: u64,
        padding: [u64; 5],
    }
    // _IOWR(KVMIO=0xAE, 0xd5, 64-byte struct)
    const KVM_PRE_FAULT_MEMORY: libc::c_ulong = 0xc040_aed5;
    let t = std::time::Instant::now();
    let mut arg = KvmPreFaultMemory {
        gpa: 0,
        size,
        flags: 0,
        padding: [0; 5],
    };
    // The ioctl can return success with part of the range done, and it
    // updates gpa and size to the remainder. Loop until the remainder is
    // zero, and stop only on an error other than EINTR or on no progress.
    while arg.size > 0 {
        let before = arg.size;
        // SAFETY: arg is a valid kvm_pre_fault_memory and the fd is a vCPU.
        let r = unsafe { libc::ioctl(vcpu.as_raw_fd(), KVM_PRE_FAULT_MEMORY, &mut arg) };
        if r != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            eprintln!(
                "cella: thaw timing: prefault(ept) failed at gpa {:#x}: {err}",
                arg.gpa
            );
            return;
        }
        if arg.size == before {
            break;
        }
    }
    eprintln!(
        "cella: thaw timing: prefault(ept) done in {}",
        fmt_ns(t.elapsed().as_nanos() as i128)
    );
}
