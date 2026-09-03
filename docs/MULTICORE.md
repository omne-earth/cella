# Multicore

This document states what a second vCPU would change in cella, what
it would cost, and the ruling. It comes before `TIME-MODEL.md`, because
the time model is the part of cella that a second core disturbs, and
the reader should know the shape of the disturbance before the model.

## The workload

The workload of a machine is an inference runtime. The model weights
live in guest RAM, and the runtime executes on the guest's cores. The
runtime is compute-bound during a forward pass and memory-bound
between passes. It uses every core it can see and tens of gigabytes
of RAM. A freeze stops it inside a forward pass, with the KV cache in
RAM and the operands in the vector registers.

A machine with one vCPU bounds this workload at one core. The bound
is a property of the machine, not of the workload. This document
removes it.

## The shape today

- One vCPU, id 0, created after the in-kernel irqchip and the PIT.
- One thread. KVM_RUN, exit, dispatch, KVM_RUN. No irqfd, no
  ioeventfd. A device raises an interrupt with a direct call into KVM.
- No PCI, no ACPI. The kernel finds its one CPU because there is
  nothing else to find.
- The sidecar (format v9) holds one `VcpuState`, one irqchip state,
  one kvm-clock value, and one TSC frequency.
- The freeze reads the TSC and kvm-clock as one pair. The thaw writes
  them as one pair. The pair is the instant.

## What changes

### Enumeration

Without ACPI, Linux finds secondary CPUs through the Intel
MultiProcessor table. The VMM writes the table into low memory, in the
extended BIOS data area, with one processor entry per vCPU, the local
APIC address, the I/O APIC, and the ISA bus. About 150 lines. No ACPI,
no PCI.

### vCPU threads

One KVM vCPU per core, one host thread each, one run loop each. Each
loop handles its own exits. The exits reach shared devices: the serial
port and the virtio-mmio transports. Every device therefore takes a
lock, and the notify path of a virtqueue must be safe from any thread.
The freeze signal must reach every thread, and the freeze must wait
for every thread to leave KVM_RUN before it reads any state.

### Bring-up

Linux starts a secondary CPU with INIT and two SIPIs. With the
in-kernel irqchip, KVM delivers them. The VMM gives each vCPU a CPUID
with its own APIC id in leaf 1 and leaf 0xB, and leaves every vCPU
after the first in the uninitialized MP state. KVM holds it there
until the SIPI arrives.

### Freeze

Per vCPU: registers, sregs, FPU, xsave, XCR0, MSRs, LAPIC, MP state,
vCPU events, nested state. The sidecar grows to a list of vCPU states
and goes to format v10. The irqchip, the PIT, and kvm-clock stay
singular.

### Thaw

Per vCPU, in the same order as today. The warming stub runs on vCPU 0
alone; the stage-2 tables are shared. The MP table is written again,
because guest RAM is the frozen image and the table lives in it
already; the second write is a no-op that keeps the boot path and the
thaw path the same.

### The jail

More threads, the same allowlist plus `clone3` and the thread-exit
path. The futex entry already exists.

## The instant becomes a set

Today the instant is one TSC value and one kvm-clock value. With N
vCPUs it is N TSC values and one kvm-clock value, and the guest can
compare them. Linux compares them at every AP bring-up, and the
clocksource watchdog compares them at runtime. Both checks are off
under `tsc=reliable`. That argument then carries more weight than it
carries today, and `TIME-MODEL.md` describes what it carries.

The set collapses back to one number if the thaw derives every vCPU
from one frozen value. KVM keeps a per-vCPU TSC offset, readable and
writable as a vCPU attribute. The freeze reads the offset of vCPU 0
and one host TSC value with it. The thaw computes one new offset from
the frozen guest value and the host TSC of that moment, and writes
that one offset to every vCPU. No vCPU takes a value of its own. The
write goes through the attribute and not through the MSR, because the
MSR path runs KVM's synchronization heuristics and those heuristics
may ignore the value on the second vCPU.

Each vCPU has its own pvclock page. Each page is rewritten by KVM
before the first KVM_RUN of that vCPU, from the one kvm-clock and the
one offset. The pages then agree by construction.

The witness of `TIME-MODEL.md` runs once per CPU. The gate is the same
prediction interval, applied to each CPU and to the difference between
CPUs.

## The cost

About a thousand lines in the trusted computing base: the MP table,
the thread pool, the locks, the per-vCPU save and restore, the offset
path. The line count is not the price. The price is that every time
guarantee becomes a guarantee about a set, and the probes must prove
that the set behaves as one, on bare metal and one nesting level down.

## The ruling

The core count is a property of the machine. `create` sets it, the
manifest records it, the sidecar carries it, and a thaw refuses a host
that cannot supply it. The default is the host's count at `create`.
The single-core path is the N=1 case of the same code, not a separate
path.

The RAM of a model-sized machine reaches the scratch slot of the
warming stub at 3 GiB, and the stub then skips. The slot moves above
the largest RAM the manifest allows. The msync of the RAM image at the
freeze sits inside the clock window and grows with the image. The
reorder in `TIME-MODEL.md` moves it out.
