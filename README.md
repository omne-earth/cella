# cella

*a cryogenic chamber for agents*

A minimal x86_64 KVM microVM in Rust: virtio-blk + virtio-net + serial,
no PCI, no ACPI, single vCPU. It freezes a running guest to two files
and thaws it later -- across a host reboot, with the guest unable to
tell from the inside: its monotonic clock, wall clock, TSC, timers,
and RNG state continue from the instant of the freeze, verified to
microseconds by the probes in `probes/`. It also hosts itself: a cella
guest can run cella, and the same time guarantees hold one nesting
level down (`make smoke-nested-boot`, `make probe-inception`).

**Status: verified on real KVM, on bare metal and on a nested-KVM
host.** Every gate is derived from measurement, none uses a tuned
constant, and the full suite (`make test-all`) passes on both machine
classes with the pinned guest kernel (7.2.2). The known gap: virtio
device state is not saved across a freeze -- see
`docs/FREEZE-THAW.md`, "Next steps: virtio state".

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
  lib.rs                 crate root
  main.rs                 thin binary: CLI, orchestration, the run loop, freeze trigger
  config.rs                the guest defaults, in one place (cmdline, thaw warming)
  memory.rs                 guest RAM: a MAP_SHARED file (also the freeze image)
  vcpu.rs                    vCPU creation, KVM_RUN dispatch, register save/restore
  freeze.rs                   sidecar file format, crash-consistent write, thaw
  warm.rs                      stage-2 warming stub, run at thaw before the clock restore
  seccomp.rs                    hand-rolled BPF allowlist + a self-test hook
  boot/x86_64.rs                 GDT, page tables, bzImage load, long-mode entry
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
probes/
  freeze-thaw-clock/      does a thaw leak real time into the guest's clocks? (the main gate)
  wallclock/                does the guest wall-clock seed correctly at boot?
  sregs/                      KVM_SET_SREGS ordering check, no guest needed
docs/
  FREEZE-THAW.md          time and state across freeze/thaw: design, gates, measurements
  NESTED-BOOT.md            cella hosts cella: the layers, the fix, the depth tables
scripts/
  jail.sh                  rootless bwrap wrapper, no jailer binary
  setup/
    install.sh                 host setup: deps, toolbox prerequisites, the cella binary to ~/.local/bin
  build/
    static.sh                   static cella + probe binaries, for inside a guest
    kernel-fragment.config       the driver set beyond kernel defconfig
    kernel-fragment-nested.config  + a KVM host stack, for the nested kernel only
    kernel-config-check.sh          verify the resolved config before compiling it
    busybox-fragment.config          static-link override for the busybox build
    rootfs.sh / rootfs-nested.sh / rootfs-inception.sh   the /sbin/init of each rootfs
    toolbox.sh                          creates/provisions the cella-build toolbox
  test/
    boot.sh / thaw.sh / net.sh     per-feature system tests against real KVM
    nested-boot.sh / inception.sh   cella-inside-cella tests (three network variants; the deep clock probe)
    jail.sh / seccomp.sh             per-feature system tests, no KVM needed
  utils/
    count_lines.py               source-vs-tests line counting for `make lines`
selinux/
  cella.te.example      policy sketch, reference only, not built here
Makefile                  one target per significant feature -- see TESTING.md
TESTING.md                 what each test target verifies, and how to reproduce
```

Line counts (`make lines`; see TESTING.md for the exact methodology):

```
SOURCE ONLY (src/, excluding inline #[cfg(test)])         2975
SOURCE + ALL TESTS (inline #[cfg(test)] + tests/)          3836
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

`make golden` and `make golden-nested` (thin wrappers over
`cella build <kernel|rootfs> <flavor>`) build every golden natively
into `~/.cella/` from real upstream source -- which is what the smoke
tests and the probes use. There's no shortcut: no microVM project
publishes a prebuilt bzImage with cella's exact driver set
(virtio-mmio/blk/net + 8250 serial built in, nothing from a module or
an initrd -- see `boot/x86_64.rs`), and pairing an arbitrary
downloaded rootfs with a from-scratch kernel/init risks mismatches
that are hard to tell apart from real bugs. So `cella build` compiles
a static busybox (with `scripts/build/rootfs.sh` as `/sbin/init`) and
a kernel (with `scripts/build/kernel-fragment.config` merged onto
`x86_64_defconfig`) that are provably matched to each other and to
cella's boot path. The compiling happens inside the `cella-build`
toolbox (`make .toolbox`, chained into `make init`) so the host
itself never needs a build toolchain.

For your own kernel instead, the essentials are the same as
`scripts/build/kernel-fragment.config`: `CONFIG_VIRTIO_MMIO`,
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
make install       # cella -> ~/.local/bin
cella build kernel canonical && cella build rootfs cella
cella create m1    # stage a machine (defaults from ~/.cella/config.json)
cella start m1     # run it: detached, jailed, ready in milliseconds
cella enter m1     # your terminal on its serial console (detach: Ctrl-] or exit)
cella freeze m1    # the machine becomes files
cella thaw m1      # the same machine, the same instant
cella list         # every machine, one line each
cella stop m1 && cella destroy m1
cella selftest     # the whole cycle proves itself
```

The machine lifecycle is the interface (see `docs/LIFECYCLE.md`).
`make demo` narrates the freeze and the thaw end to end; `make boot`,
`make enter`, `make freeze`, `make thaw`, and `make remove` are thin
wrappers over the verbs for one default machine (`VM=<name>` to pick
another). A machine lives at `~/.cella/machines/<name>`: its manifest,
its own disk, and -- while frozen -- its RAM image and sidecar. The
flag interface below remains for the probes and the tests:

```sh
make golden        # or use your own kernel/disk

scripts/jail.sh \
  --state-dir ./vm1 \
  --kernel ~/.cella/kernel/canonical/bzImage \
  --disk ~/.cella/rootfs/canonical/rootfs.ext4 \
  --tap tap0 \
  --mem-mb 256 \
  --cmdline "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw virtio_mmio.device=4K@0xd0000000:5 virtio_mmio.device=4K@0xd0001000:6 ip=192.168.200.2::192.168.200.1:255.255.255.0::eth0:off"
```

(`make smoke-boot` / `make smoke-thaw` / `make smoke-net` run equivalent commands
automatically as pass/fail system tests -- see `TESTING.md`.)

Freeze it from another terminal:

```sh
kill -USR1 $(pgrep -f 'target/release/cella')
```

Run the exact same `jail.sh` command again against the same
`--state-dir` (or `make thaw`): it detects the frozen `state` file and
thaws instead of booting. `--kernel`/`--cmdline`/`--mem-mb` are ignored on thaw (memory
size comes from the frozen state itself, so it can't disagree with the
RAM file being reopened).

## The freeze/thaw design, briefly

Guest RAM is a `MAP_SHARED` file from the moment the VM boots
(`memory.rs`) -- it's simultaneously "the memory KVM runs the guest
against" and "the on-disk freeze image." Freezing is `msync` plus a
small sidecar file (`freeze.rs`) holding vCPU registers, FPU state
(including the XSAVE area and XCR0), LAPIC, MSRs, the kvmclock, and the
interrupt hardware that lives in KVM rather than in guest RAM: both PICs,
the IOAPIC, and the PIT. Thawing re-`mmap`s the same RAM file and
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
  off: frozen at T, thawed, still T. `make probe-freeze-thaw-clock`
  gates exactly this: the heartbeat interval of the guest that contains
  the freeze must equal a normal interval within a prediction interval
  computed from the run's own baseline (sub-millisecond on bare metal).
  Before the clock restore, the thaw warms the stage-2 mappings --
  first `KVM_PRE_FAULT_MEMORY` for the direct host, then the stub of
  `warm.rs` for every hypervisor layer below -- so the wake-up cost
  lands in host time, not guest time. The full account, including the
  nested measurements, is in `docs/FREEZE-THAW.md` and
  `docs/NESTED-BOOT.md`. The TSC restore is a direct `wrmsr`
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
  `/dev/net/tun`. No jailer binary, no root at runtime -- `sudo cella setup net`
  is the only step that needs root, once per boot, purely to provision
  the TAP pool and the NAT (`CAP_NET_ADMIN`).
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

## Verification

Everything above runs against real KVM on two machine classes -- bare
metal, and a host that is itself a KVM guest -- and the results live
in the documents below. The boot path, the freeze/thaw ioctl
sequences, and the nested hosting are exercised by `make smoke` on
every change; the clock gates are statistical, computed from each
run's own baseline, and hold on both machines. `TESTING.md` maps each
target to what it proves.

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

## Documents

- **`docs/FREEZE-THAW.md`** -- time and state across freeze and thaw:
  the cryogenic principle, the freeze and thaw sequences and their
  order rules, the derived gates, the measurements on both machines,
  and the virtio-state gap that is the next work item.
- **`docs/NESTED-BOOT.md`** -- cella hosts cella: the layer model, the
  nested artifacts, the three network variants, the clock probe one
  nesting level down, the depth tables, and "The fix" -- the five
  changes that made inception seamless.
- **`TESTING.md`** -- what each make target verifies, and how to add a
  new one.

---

<sub>made by omne with claude</sub>
