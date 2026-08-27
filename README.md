# cella

A minimal x86_64 KVM microVM in Rust: virtio-blk + virtio-net + serial,
no PCI, no ACPI, single vCPU, with a cryogenic freeze/thaw that survives
a host reboot. This is the concrete artifact from a design conversation
about minimizing VMM TCB; the sections below map back to that reasoning
so the "why" isn't lost alongside the code.

**Status: builds and passes its own test suite (`make test`), but the
boot path is not KVM-verified by us.** It was built in a sandbox with no
`/dev/kvm` available, so the GDT/page-table/bzImage-loading code that
actually boots a kernel has never run against real hardware. Everything
that *can* be verified without `/dev/kvm` -- the virtio-mmio protocol,
virtio-blk's descriptor-chain handling, the freeze/thaw sidecar format,
and the seccomp filter -- has real, passing, `cargo test`-driven tests
against the actual compiled code (see `TESTING.md`). Treat the boot path
as a carefully-reasoned draft; see "What to check first" below.

## What's in here, and what it deliberately isn't

- **x86_64 only**, direct bzImage boot (no BIOS, no firmware). See
  `src/boot/x86_64.rs`: hand-rolled GDT and identity-mapped page tables,
  jumping straight to the kernel's 64-bit entry point.
- **Single vCPU.** This isn't just a simplification -- it's load-bearing
  for the freeze/thaw design (see below).
- **virtio-mmio, not PCI.** The guest is told device addresses on the
  kernel command line (`virtio_mmio.device=...`), no discovery mechanism,
  no ACPI tables.
- **virtio-blk is synchronous** (`pread64`/`pwrite64` inline in the
  notify handler). No io_uring, no request reordering. This also means
  there's never anything "in flight" to drain before a freeze.
- **virtio-net has no offloads.** The TAP is opened with `IFF_VNET_HDR`,
  which makes the kernel's frame header and virtio-net's header the same
  10 bytes, so TX/RX are a direct copy with no translation.
- **No irqfd/ioeventfd.** PIO/MMIO exits are trapped and handled
  synchronously in the one thread that runs the vCPU; devices call
  `VmFd::set_irq_line` directly. This is a real perf/latency cost and a
  deliberate trade for a much simpler concurrency story: there is no
  second I/O thread, so "the run loop stops" *is* "devices are quiesced,"
  with no separate detach step needed before a freeze.
- **Dependencies are the vetted rust-vmm crates for the KVM/boot/queue
  layer** (`kvm-ioctls`, `kvm-bindings`, `vm-memory`, `linux-loader`,
  `vm-superio`, `virtio-queue`) **plus `libc`, and nothing else.**
  Everything else -- the virtio-mmio transport, block/net device logic,
  TAP handling, the freeze/thaw format, and seccomp -- is hand-written.
  The rationale: a wrong ioctl struct size silently produces a wrong
  ioctl number, which is a class of bug that's very hard to find by
  reading code and impossible to catch without hardware to test against.
  That risk is worth outsourcing to a widely-used crate; the rest isn't.

## Layout

```
src/
  lib.rs                 crate root: pub mod boot/devices/freeze/memory/seccomp/vcpu
  main.rs                 thin binary: CLI, orchestration, the run loop, freeze trigger
  memory.rs                guest RAM: a MAP_SHARED file (also the freeze image)
  vcpu.rs                   vCPU creation, KVM_RUN dispatch, register save/restore
  freeze.rs                  sidecar file format, crash-consistent write, thaw
  seccomp.rs                  hand-rolled BPF allowlist + a self-test hook
  boot/x86_64.rs                GDT, page tables, bzImage load, long-mode entry
  devices/serial.rs              16550 (vm-superio) wired to IRQ4
  devices/virtio/
    mmio.rs                       virtio-mmio v2 register file (IrqLine trait: no
                                    KVM needed to unit-test the protocol)
    block.rs                       virtio-blk backend
    net.rs                          virtio-net backend
    tap.rs                           TAP open/read/write
tests/
  virtio_block.rs         real descriptor-chain-driven virtio-blk tests
  virtio_mmio.rs            real virtio-mmio v2 protocol tests
scripts/
  make_tap.sh             one-time (per boot), needs sudo once
  jail.sh                  rootless bwrap wrapper, no jailer binary
  freeze.sh                 send SIGUSR1 to a running cella
  build-assets.sh            build a busybox rootfs + a bzImage kernel from source
  kernel-fragment.config      the driver set build-assets.sh needs beyond kernel defconfig
  busybox-fragment.config      static-link override for build-assets.sh's busybox build
  rootfs-init.sh                /sbin/init installed into the built rootfs
  bootstrap.sh                 one-time Fedora host setup (runtime deps + tap0 + kvm check)
  toolbox-setup.sh              creates/provisions the cella-build toolbox (kernel build toolchain)
  boot.sh / thaw.sh / net.sh  per-feature system tests against real KVM
  test-jail.sh / test-seccomp.sh   per-feature system tests, no KVM needed
  count_lines.py               source-vs-tests line counting for `make lines`
selinux/
  cella.te.example      policy sketch, reference only, not built here
Makefile                  one target per significant feature -- see TESTING.md
TESTING.md                 what each test target verifies, and how to reproduce
```

Line counts (`make lines`; see TESTING.md for the exact methodology):

```
SOURCE ONLY (src/, excluding inline #[cfg(test)])         2063
SOURCE + ALL TESTS (inline #[cfg(test)] + tests/)          2882
```

For scale: a full Firecracker build is about 57k lines of non-test Rust;
a file-level "keep only block+net" extraction of Firecracker lands
around 10-18k; this sits below that because it drops PCI,
snapshots-as-a-feature (vs. this narrower freeze/thaw), rate limiters,
vhost-user, and multi-queue.

## Testing

```sh
make test          # ~2s, no /dev/kvm needed -- run this first
make test-all       # + every KVM-dependent feature test (skips cleanly without KVM)
```

Full details, including a table of exactly what each target verifies and
how to add a new one, are in **`TESTING.md`**.

## Build

```sh
make build          # release build -> target/release/cella
# or: cargo build --release
```

Needs current stable Rust (this was built and clippy-clean on 1.98) and
a Linux host. No `/dev/kvm` is required to build, only to run.

## Getting a kernel and a disk image

`make build-assets` builds both a minimal rootfs and a minimal bzImage
kernel from real upstream source, into `assets/` -- which is what
`make boot` / `make thaw` / `make net` use. There's no shortcut for
either: no microVM project publishes a prebuilt bzImage with cella's
exact driver set (virtio-mmio/blk/net + 8250 serial built in, nothing
from a module or an initrd -- see `boot/x86_64.rs`), and pairing an
arbitrary downloaded rootfs with a from-scratch kernel/init risks
mismatches that are hard to tell apart from real bugs. So
`scripts/build-assets.sh` builds a static busybox (with
`scripts/rootfs-init.sh` as `/sbin/init`) and a kernel (with
`scripts/kernel-fragment.config` merged onto `x86_64_defconfig`) that
are provably matched to each other and to cella's boot path. The
actual compiling happens inside the `cella-build` toolbox
(`make .toolbox`, chained into `make init`) so the host itself
never needs a build toolchain.

For your own kernel instead, the essentials are the same as
`scripts/kernel-fragment.config`: `CONFIG_VIRTIO_MMIO`,
`CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES` (this is what lets
`virtio_mmio.device=...` on the cmdline stand in for the ACPI/DT
discovery a firmware-booted guest would get), `CONFIG_VIRTIO_BLK`,
`CONFIG_VIRTIO_NET`, `CONFIG_SERIAL_8250(_CONSOLE)`, and a filesystem
driver for your rootfs. ACPI/PCI can stay on (see the fragment's own
comment) since cella boots with `pci=off` and no ACPI tables either
way.

For a disk image, any raw filesystem image works, e.g.:

```sh
dd if=/dev/zero of=rootfs.img bs=1M count=512
mkfs.ext4 rootfs.img
# populate it (mount loopback, debootstrap/pacstrap/podman export, etc.)
```

## Run

```sh
make init                                    # once per host: deps, toolbox, tap0
make build-assets                                 # or use your own kernel/disk

scripts/jail.sh \
  --state-dir ./vm1 \
  --kernel ./assets/bzImage \
  --disk ./assets/rootfs.ext4 \
  --tap tap0 \
  --mem-mb 256 \
  --cmdline "console=ttyS0 reboot=k panic=1 pci=off virtio_mmio.device=4K@0xd0000000:5 virtio_mmio.device=4K@0xd0001000:6 ip=192.168.200.2::192.168.200.1:255.255.255.0::eth0:off"
```

(`make boot` / `make thaw` / `make net` run equivalent commands
automatically as pass/fail system tests -- see `TESTING.md`.)

Freeze it from another terminal:

```sh
scripts/freeze.sh $(pgrep -f 'target/release/cella')
```

Run the exact same `jail.sh` command again against the same
`--state-dir`: it detects the frozen `state` file and thaws instead of
booting. `--kernel`/`--cmdline`/`--mem-mb` are ignored on thaw (memory
size comes from the frozen state itself, so it can't disagree with the
RAM file being reopened).

## The freeze/thaw design, briefly

Guest RAM is a `MAP_SHARED` file from the moment the VM boots
(`memory.rs`) -- it's simultaneously "the memory KVM runs the guest
against" and "the on-disk freeze image." Freezing is `msync` plus a
small sidecar file (`freeze.rs`) holding vCPU registers, FPU state,
LAPIC, MSRs, and the kvmclock. Thawing re-`mmap`s the same RAM file and
replays that sidecar back into a fresh KVM VM.

Two properties fall out of this on purpose:

- **Crash consistency, including across a host reboot.** The sidecar is
  written to `state.tmp`, fsynced, renamed over `state`, and the
  directory is fsynced. `state`'s *existence* is the crash-safe signal
  that an image is resumable; an interrupted freeze just leaves no
  `state` file, which is indistinguishable from "never frozen."
- **Time is cryogenic too -- the guest can't tell it was ever frozen.**
  On thaw we restore the kvmclock and each vCPU's TSC to their frozen
  values (not the host's current wall time), so both the guest's
  monotonic clock *and* its wall clock resume exactly where they left
  off: frozen at T, thawed, still T. The TSC restore is a direct `wrmsr`
  to `MSR_IA32_TSC` via `KVM_SET_MSRS`, which is *only* correct because
  there's exactly one vCPU: KVM's TSC-synchronization heuristics exist
  to keep multiple vCPUs' counters aligned with each other, and that
  concern doesn't apply here. **This is the one place where "single
  vCPU" isn't just a simplification but a correctness precondition** --
  don't add a second vCPU without also switching to the
  `KVM_VCPU_TSC_OFFSET` device-attribute API.

Deliberately not implemented, per the conversation that produced this
design: **RAM is not encrypted.** The sidecar and RAM file inherit
whatever protection the filesystem gives them (LUKS/fscrypt on Fedora by
default); there's no in-VMM AEAD layer. `harden_ram()` in `memory.rs`
does the cheap, unconditional hygiene (`MADV_DONTDUMP`, best-effort
`mlock`) but that's not encryption.

**One-shot thaw.** `finalize_thaw()` deletes the `state` sidecar right
after it's been successfully applied and before the first `KVM_RUN`. A
frozen image can be resumed exactly once; to fork one, `cp -r` the whole
state directory first and thaw the copy.

## Security layers

- **Jail:** `scripts/jail.sh` wraps the binary in `bubblewrap`
  (`--unshare-user/pid/ipc/uts/cgroup`), binding in only the specific
  kernel/disk/state paths given on the command line, `/dev/kvm`, and
  `/dev/net/tun`. No jailer binary, no root at runtime -- `make_tap.sh`
  is the only step that needs `sudo`, once per boot, purely to create the
  TAP device (`CAP_NET_ADMIN`).
- **seccomp:** `seccomp.rs` is a hand-rolled classic-BPF allowlist
  (~25 syscalls, no argument filtering), installed once right before the
  run loop. It's wider than "just `KVM_RUN`" because freezing needs
  ordinary filesystem syscalls and can happen at any point in the loop.
  Each entry is commented with why it's there; the natural next
  tightening step is filtering `ioctl` on `args[1]` to the exact KVM
  request codes this VMM issues, which is not done here.
- **SELinux:** `selinux/cella.te.example` is a reference sketch, not
  wired into the build. It's the shape discussed: a dedicated domain, a
  distinct type for freeze-image directories (so they're not lumped in
  with `user_home_t`), and read access to `kvm_device_t`/
  `tun_tap_device_t`. Per-VM MCS categories (sVirt-style) for running
  more than one image concurrently are not sketched.

## What to check first if you try to actually boot this

`make boot` (see `TESTING.md`) is the actual repro script -- point it at
`/dev/kvm` and it tells you exactly where things stand. In rough order
of "most likely to be wrong, given no hardware testing":

1. **The GDT descriptor packing in `boot/x86_64.rs`** (`gdt_entry` /
   `kvm_segment_from_gdt`). Segment descriptor bit-twiddling is exactly
   the kind of code that's easy to get subtly wrong and where the wrong
   answer is a triple fault with no useful diagnostic. Firecracker's
   `arch/x86_64/gdt.rs` is the reference to diff against. `make
   unit-test` checks the bit-packing logic in isolation (round-trips,
   known-good access bytes) but can't check it against what real
   hardware actually expects at boot.
2. **The e820 map and `setup_header` fields in `load_kernel`.** Worth
   dumping and comparing against what Firecracker or cloud-hypervisor
   actually write for the same kernel.
3. **Whether `set_tss_address`/`set_identity_map_address` need to be
   called before vs. after `create_irq_chip`** -- current KVM is lenient
   here but this has moved around across kernel versions.
4. **Lower risk than the above:** the virtio-mmio register file
   (`mmio.rs`) and virtio-blk's descriptor-chain handling (`block.rs`)
   have real passing tests (`make integration-test`) that exercise the
   actual protocol logic against real guest memory and real descriptor
   chains -- not a guarantee they match every guest driver's exact
   expectations, but a much stronger footing than the boot path has.
5. Freeze/thaw and seccomp are lower-risk still: both are fully unit- and
   system-tested without needing a real guest at all (`make thaw`,
   `make seccomp`) -- the register-restore ioctl sequence in `vcpu.rs`
   is the one piece of freeze/thaw that only real KVM exercises.

## Explicitly out of scope

Carried over from the conversation, not forgotten:

- **arm64 / seL4 / Cloud Hypervisor comparisons** discussed earlier are
  design context, not alternate build targets of this repo.
- **Multi-vCPU.** Would need real interrupt routing (currently a single
  shared legacy IRQ per device via the in-kernel PIC) and the TSC-offset
  attribute API instead of direct MSR writes.
- **GPU/PCIe passthrough, snapshots-as-a-product-feature, live migration.**
- **Encrypted RAM / SEV/TDX.** Discussed and explicitly declined for this
  design -- see "The freeze/thaw design" above.
