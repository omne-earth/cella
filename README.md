# cella

*a cryogenic chamber for agents*

A minimal x86_64 KVM microVM in Rust: virtio-blk, virtio-net, serial.
No PCI, no ACPI, one vCPU. It freezes a running guest to files and
thaws it later -- across a host reboot -- and the guest cannot tell
from the inside. Its monotonic clock, wall clock, TSC, timers, RNG
state, disk, network, and in-flight requests continue from the freeze
instant. The probes (`cella probe ...`) verify the clocks to
microseconds.

cella also hosts itself: a cella guest runs cella, and the same time
guarantees hold one nesting level down (`make smoke-nested-boot`,
`make probe-inception`).

**Status: verified on real KVM, on bare metal and on a nested-KVM
host.** Every gate is derived from measurement; no gate uses a tuned
constant. The full suite (`make test-all`) passes on both machine
classes with the pinned guest kernel (7.2.2). The virtio transports
now ride the freeze sidecar (format v8), the egress-hold surface --
park, report, release, allow -- is in place (`docs/DEVICE-STATE.md`),
and the universe family treats machines as artifacts: branch,
archive, inspect (`docs/LIFECYCLE.md`). The network model is
decided and being built (`docs/NETWORK-MODEL.md`): every machine
is born closed, an open valve is a membrane and never a free flow,
every egress operation waits for a decision by id, and nothing
outlives its epoch.

## The machine lifecycle

The verbs are the interface (`docs/LIFECYCLE.md`). No daemon: a
machine is a directory under `~/.cella/machines/<name>` -- its
manifest, its own disk, and, while frozen, its RAM image and sidecar.

```sh
make install       # host deps + cella -> ~/.local/bin
cella network setup --taps 4     # the pool, no sudo (install granted the capability)
cella build kernel canonical     # goldens, built natively (skips while inputs match)
cella build rootfs cella
cella create m1    # stage a machine (defaults from ~/.cella/config.json)
cella start m1     # run it: detached, jailed, ready in milliseconds
cella enter m1     # your terminal on its serial console (detach: Ctrl-])
cella freeze m1    # the machine becomes files
cella thaw m1      # the same machine, the same instant
cella list         # every machine, one line each
cella gateway m1 open      # born closed; open arms the membrane -- egress parks
cella gateway m1 show      # the held operations; release/refuse decide by id
cella branch m1 m2 # copy a still machine: frozen twin, or fresh-bootable
cella archive m1   # a rock: storage stays, nothing resumes, start refuses
cella inspect m1   # throwaway appliance; the evidence at /rock, ro + noexec
cella stop m1 && cella destroy m1
cella selftest     # the whole cycle proves itself
cella doctor check # the host judged, one fact per line (fix repairs, verify audits)
```

`make smoke-shell` narrates the freeze and the thaw end to end. `make boot`,
`make enter`, `make freeze`, `make thaw`, and `make remove` wrap the
verbs for one default machine (`VM=<name>` picks another).

## The freeze/thaw design, briefly

Guest RAM is a `MAP_SHARED` file from boot (`memory.rs`): the memory
KVM runs against is the on-disk freeze image. A freeze is `msync`
plus a small sidecar (`freeze.rs`): vCPU registers, FPU and XSAVE
state, LAPIC, MSRs, the kvmclock, the PICs, the IOAPIC, the PIT, the
serial registers, and one block per virtio transport. A thaw remaps
the RAM file and replays the sidecar into a fresh KVM VM.

Properties, each deliberate:

- **Crash consistency, across a host reboot.** The sidecar goes to
  `state.tmp`, fsync, rename to `state`, fsync of the directory. The
  existence of `state` is the resumable signal; an interrupted freeze
  leaves none, which equals "never frozen."
- **Time is cryogenic.** The thaw restores the kvmclock and the TSC
  to their frozen values, not to host time. Monotonic and wall clock
  resume where they stopped. `make probe-freeze-thaw-clock` gates
  this: the heartbeat interval that contains the freeze must equal a
  normal interval, within a prediction interval computed from the
  run's own baseline (sub-millisecond on bare metal).
- **The wake-up cost lands in host time.** Before the clock restore,
  the thaw prefills the stage-2 tables (`KVM_PRE_FAULT_MEMORY`) and
  runs the warming stub (`warm.rs`), which reaches every hypervisor
  layer below. See `docs/FREEZE-THAW.md` and `docs/NESTED-BOOT.md`.
- **Devices continue.** Sidecar v8 carries each transport: status,
  queue select, ISR, negotiated features, and per queue the ready
  flag, size, ring addresses, and the next-available and next-used
  indices -- the only progress counters RAM does not hold. The shell
  gate runs on a rw root. See `docs/DEVICE-STATE.md`.
- **Egress can hold.** Under hold, an outbound frame parks between
  the TX ring and the TAP, and the verdict comes from outside:
  release (with an optional allow pass entry), or freeze -- the
  frame rides the sidecar, and the thaw delivers and completes it.
  The world-ratchet gate (`make device-state-ac4`) proves the
  sequence against real endpoints.
- **One-shot thaw.** `finalize_thaw()` deletes `state` before the
  first `KVM_RUN`. Forking is a verb: `cella branch` copies a still
  machine and records the layer digests of the fork instant.

The TSC restore is a direct write to `MSR_IA32_TSC`. That is correct
only with one vCPU: KVM's TSC-synchronization heuristics exist to
align multiple vCPUs. Do not add a second vCPU without switching to
the `KVM_VCPU_TSC_OFFSET` attribute API.

## Limitations

- **x86_64 only**, direct bzImage boot: no BIOS, no firmware. See
  `src/boot/x86_64.rs` -- hand-rolled GDT, identity-mapped page
  tables, a jump to the 64-bit entry point.
- **Single vCPU.** Load-bearing for the freeze/thaw design (above),
  not only a simplification.
- **virtio-mmio, not PCI.** The kernel command line names each
  device (`virtio_mmio.device=...`). No discovery, no ACPI tables.
- **virtio-blk is synchronous**: `pread64`/`pwrite64` inline in the
  notify handler. No io_uring, no reordering. Therefore nothing is
  ever in flight to drain before a freeze.
- **virtio-net has no offloads.** The TAP opens with `IFF_VNET_HDR`,
  thus TX and RX are a direct copy with no header translation.
- **No irqfd/ioeventfd.** One thread runs the vCPU and handles every
  exit; devices call `set_irq_line` directly. A real latency cost,
  traded for one concurrency story: when the run loop stops, the
  devices are quiesced, with no detach step before a freeze.
- **RAM is not encrypted.** The RAM file and the sidecar inherit the
  filesystem's protection (LUKS/fscrypt). `harden_ram()` applies
  `MADV_DONTDUMP` and best-effort `mlock`; that is hygiene, not
  encryption.
- **Dependencies**: the vetted rust-vmm crates for the KVM, boot,
  and queue layer (`kvm-ioctls`, `kvm-bindings`, `vm-memory`,
  `linux-loader`, `vm-superio`, `virtio-queue`) plus `libc`, and
  nothing else. The virtio-mmio transport, the device backends, the
  TAP handling, the freeze format, the machine registry, the native
  build, and seccomp are hand-written. A wrong ioctl struct size
  silently makes a wrong ioctl number; that risk goes to a
  widely-used crate, and the rest does not.

## Layout

```
src/
  main.rs               the multi-call binary: personas, the run loop, freeze/thaw, verdicts
  doctor.rs             cella doctor: check, fix, verify
  gateway.rs            cella gateway: show, release, refuse, open, close
  ledger.rs             the chronicle, the id mint, the framed wire helpers
  universe.rs           branch, archive, inspect: machines as artifacts
  golden.rs             golden manifests: sha3-256, write/read (the seed of cella-libs)
  bin/cella-network.rs  the one CAP_NET_ADMIN holder (a real binary: file capability)
  bin/cella-probe/      the diagnostics: wallclock, freeze-thaw-clock, sregs
  machine.rs            the verbs: registry, spawn/jail, taps, setup net
  build.rs              native golden builds (kernel, rootfs) via the toolbox
  config.rs             guest defaults in one place (cmdline, thaw warming)
  memory.rs             guest RAM: a MAP_SHARED file (also the freeze image)
  vcpu.rs               vCPU creation, KVM_RUN dispatch, register save/restore
  freeze.rs             sidecar format (v8), crash-consistent write, thaw
  warm.rs               stage-2 warming stub, run at thaw before the clock restore
  seccomp.rs            hand-rolled BPF allowlist + a self-test hook
  boot/x86_64.rs        GDT, page tables, bzImage load, long-mode entry
  devices/serial.rs     16550 (vm-superio) wired to IRQ4
  devices/virtio/
    mmio.rs             virtio-mmio v2 register file + transport state save/restore
    block.rs            virtio-blk backend
    net.rs              virtio-net backend + the egress park point
    tap.rs              TAP open/read/write
tests/
  virtio_block.rs       descriptor-chain-driven virtio-blk tests
  virtio_mmio.rs        virtio-mmio v2 protocol tests
docs/
  LIFECYCLE.md          the verbs, the machine directory, the golden artifacts
  NETWORK-MODEL.md      the decision record: always shielded, the membrane, the phases
  FREEZE-THAW.md        time and state across freeze/thaw: design, gates, measurements
  DEVICE-STATE.md       virtio state, the egress hold, the world-ratchet gate
  NESTED-BOOT.md        cella hosts cella: layers, the fix, the depth tables
scripts/
  jail.sh               rootless bwrap wrapper for the raw flag interface
  setup/install.sh      host setup: deps, forward rules, the binary to ~/.local/bin
  build/                kernel/busybox config fragments, the init of each rootfs,
                        kernel-config-check.sh
  test/                 one script per system test: boot, thaw, ping, shell,
                        device-state (the four acceptance gates), universe,
                        multinet, gateway, gateway-cli, ledger, nested-boot,
                        inception, jail, seccomp, machine
  utils/count_lines.py  source-vs-tests line counting for `make lines`
security/profiles/<cli>/  seccomp + SELinux placeholders per thin CLI (shakedown fills them)
selinux/cella.te.example  policy sketch, reference only
Makefile                one target per workflow -- see TESTING.md
TESTING.md              what each target verifies, and how to reproduce
```

Line counts (`make lines`):

```
SOURCE ONLY (src/, excluding inline #[cfg(test)])        10058
SOURCE + ALL TESTS (inline #[cfg(test)] + tests/)        11393
```

The count now carries the probes (they moved into src/bin at the
CLI split). For scale: a full Firecracker build is about 57k lines
of non-test Rust, and a block+net extraction of Firecracker lands
near 10-18k. cella sits below that because it drops PCI, rate
limiters, vhost-user, and multi-queue -- and adds the lifecycle,
the native build, the universe family, the doctor, and the
cryogenic freeze in exchange.

## Build and test

```sh
make build         # release build -> target/release/cella
make test          # ~2s, no /dev/kvm -- run this first
make test-all      # + every KVM smoke test (skips cleanly without KVM)
make smoke-device-state   # the four device-state acceptance gates
```

Needs current stable Rust and a Linux host. `/dev/kvm` is required
only to run. `TESTING.md` maps each target to what it proves.

## Goldens: the kernel and the disk

`make golden` and `make golden-nested` wrap
`cella build <kernel|rootfs> <flavor>`. The verb compiles every
golden natively into `~/.cella/` from upstream source, inside the
`cella-build` toolbox that it creates and provisions itself. A golden
that exists is skipped only while its recorded input digests still
match; a changed fragment or init script rebuilds it, and `--fresh`
rebuilds unconditionally. There is no shortcut: no
project publishes a bzImage with cella's exact driver set
(virtio-mmio/blk/net + 8250 built in, no modules, no initrd), and an
arbitrary rootfs against a from-scratch kernel produces mismatches
that look like real bugs.

For your own kernel, the essentials match
`scripts/build/kernel-fragment.config`: `CONFIG_VIRTIO_MMIO`,
`CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES`, `CONFIG_VIRTIO_BLK`,
`CONFIG_VIRTIO_NET`, `CONFIG_SERIAL_8250(_CONSOLE)`, and a
filesystem driver. Any raw filesystem image works as a disk.

## Security layers

- **Jail**: the start verb spawns the VMM under bubblewrap
  (`--unshare-user/pid/ipc/uts/cgroup`, `--as-pid-1`), binding only
  the machine directory, the kernel, `/dev/kvm`, and `/dev/net/tun`.
  No root at runtime: cella-network carries CAP_NET_ADMIN as a file
  capability (the single setcap at install), provisions the pool,
  the deterministic MACs, forwarding, and the NAT, and a boot
  oneshot recreates the pool after a reboot.
- **seccomp**: a hand-rolled classic-BPF allowlist (~30 syscalls, no
  argument filtering), installed before the run loop. Each entry
  carries its reason. The next tightening: filter `ioctl` to the
  exact KVM request codes.
- **SELinux**: `selinux/cella.te.example` is a reference sketch, not
  wired into the build.

## Out of scope

- Multi-vCPU: needs real interrupt routing and the TSC-offset
  attribute API instead of direct MSR writes.
- GPU/PCIe passthrough, live migration.
- Encrypted RAM / SEV / TDX: declined for this design (see
  Limitations).
- arm64: design context only, not a build target.

---

<sub>made by omne with claude</sub>
