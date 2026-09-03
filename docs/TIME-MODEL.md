# The time model

This document names the clocks of the guest, states what each clock
reads across a thaw, and separates the claims that a probe measures
from the claims that rest on a kernel argument. `FREEZE-THAW.md`
describes the mechanism. This document describes the model that the
mechanism must satisfy.

## The clocks

The guest has four clocks. Each one has a different reader, and each
one has a different path through a thaw.

| Clock | Source | Reader in the guest | Across a thaw |
|---|---|---|---|
| TSC | Host TSC plus a per-vCPU offset in KVM | `rdtsc` in any process, `rdtscp`, the TSC-deadline timer | Restored by a write of MSR_IA32_TSC |
| kvm-clock | The pvclock page: `system_time + (rdtsc - tsc_timestamp) * mul` | The kernel: CLOCK_MONOTONIC, CLOCK_BOOTTIME, the scheduler, every timer | Restored by KVM_SET_CLOCK; KVM rewrites the page before the first KVM_RUN |
| CLOCK_REALTIME | kvm-clock plus an offset the kernel keeps | `date`, `clock_gettime`, file timestamps | Follows kvm-clock; the offset is in guest RAM |
| The LAPIC timer | A TSC deadline | The kernel's tick and hrtimers | Restored by a write of MSR_IA32_TSC_DEADLINE after the TSC |

The guest has no RTC, no HPET, and no PIT clocksource. The PIT exists
and its state is saved, but the kernel does not read time from it.

## The invariant

Every clock of the guest continues from the instant of the stop. No
clock advances across the freeze. No clock steps against another
clock. A reader inside the guest that compares two clocks across a
thaw sees the same relation that it saw before the thaw.

The first two sentences describe continuity. The third describes
consistency. Continuity is measured. Consistency is measured for one
pair of clocks and assumed for the other.

## What the clocks do while the vCPU is stopped

A stopped vCPU does not stop its clocks. The guest TSC is the host TSC
plus an offset. kvm-clock is host time plus an offset. Both run while
the vCPU sits outside KVM_RUN. Therefore every host microsecond between
the exit from KVM_RUN and the read of the state enters the guest as
elapsed time, and every host microsecond between the write of the
state and the next KVM_RUN enters the guest in the same way.

This time is a forward step. It is the same for the TSC and for
kvm-clock, thus it does not break consistency. It breaks continuity by
the size of the step. The guest sees it as lateness of one wake-up,
not as a fault.

The freeze sequence reads the state after the msync of guest RAM. The
msync waits for the disk. The thaw sequence writes the clock before the
restore of the irqchip, the devices, the ledger, and the seccomp
filter. Both orders put work inside the window that the guest counts.
The VMM prints the size of each window at every freeze and thaw. No
gate bounds them on their own; the heartbeat gate bounds their sum
together with the wake-up cost of the first cycle.

## What is measured

The probe `freeze-thaw-clock` measures continuity of kvm-clock. The
guest prints its CLOCK_MONOTONIC once per second. The probe compares
the interval that contains the freeze against the baseline intervals,
with a 3-sigma prediction interval as the gate. It passes on bare metal
and on nested KVM with the warming stub. The gate sees a step larger
than the remaining sleep of the heartbeat loop. It does not see a
smaller step, because the wake-up is scheduled in the same clock that
stepped.

The probe also reads the VMM's own timing lines. The read of the TSC
and the read of kvm-clock at the freeze are 2 us to 5 us apart. The two
writes at the thaw are 2 us apart. This proves that the VMM handed KVM
a consistent pair. It does not prove what the guest reads afterwards.

The probe `wallclock` measures that CLOCK_REALTIME seeds from kvm-clock
at boot. It does not measure a thaw.

## What is assumed

The consistency of the TSC against kvm-clock is not measured from
inside the guest. The kernel's clocksource watchdog is the one reader
in the guest that compares the two, and the command line stops it with
`tsc=reliable`. Before that argument, the watchdog reported a
difference of 5 ms to 27 ms after each thaw. The cause of that
difference is not known. The comments in `vcpu.rs`, `main.rs`, and
`config.rs` do not agree on whether the difference is present today.

With the watchdog stopped, the kernel does not read the TSC for time.
Processes do. `rdtsc` is not privileged in the guest, and runtimes,
JITs, and benchmark loops use it. A process that samples `rdtsc` and
`clock_gettime` together, before and after a thaw, measures the
difference that the watchdog measured. No probe does this. The repo
contains no `rdtsc`.

The claim "the guest cannot tell" therefore has two parts. The part
about kvm-clock is measured. The part about the TSC is asserted, and
the assertion rests on the guest not looking.

## The witness

The witness is a small static program in the guest rootfs. It runs in
the heartbeat loop. Once per second it reads `rdtsc`, then
CLOCK_MONOTONIC, then `rdtsc` again, and prints the three values with
the pair of `rdtsc` values as the bracket of the read. From two
consecutive lines the probe computes the TSC delta in nanoseconds,
using the frequency the guest reports in `/proc/cpuinfo`, and the
kvm-clock delta. Their difference is the skew of that interval.

The probe `freeze-thaw-clock` collects the baseline skews before the
freeze and the skew of the interval that contains the freeze. The gate
is the same prediction interval that the continuity check uses. A
thaw that steps the TSC against kvm-clock by more than the noise fails
the gate. The output shows the skew in nanoseconds, with its sign, so
that a failure states which clock ran ahead.

The witness turns the assumption into a measurement. If the measured
skew is zero within the noise, the sentence "cella rewinds the TSC,
thus the guest must not compare it" leaves `config.rs` and the
Makefile, and `tsc=reliable` becomes a choice about the watchdog's
cost, not a cover for a fault. If the skew is not zero, the fault has
a number and a sign, and the work starts from there.

## The reorder

Two moves shrink the window that the guest counts. Neither changes what
is saved.

1. At the freeze, read the vCPU state and kvm-clock first, then msync
   guest RAM. The msync grows with the dirty part of the image, and a
   model-sized image makes it the largest term in the window. The
   sidecar still lands after the RAM, thus the rule "no sidecar, no
   valid image" holds.
2. At the thaw, restore everything that is not a clock, then write the
   TSC, the TSC deadline, and kvm-clock as the last three operations
   before the first KVM_RUN. The seccomp filter installs before them,
   because the filter must allow the three ioctls in any case.

After the reorder, the window holds four ioctls and the entry into
KVM_RUN. The VMM keeps the timing lines, and the witness gate measures
the result from inside.

## Hardware identity

A thaw checks one number: the TSC frequency. CPUID is not saved and is
not compared. The thaw takes the CPUID of the host at that moment.

An inference runtime selects its kernels from CPUID when it starts:
AVX-512, AMX, VNNI. A thaw onto a host that lacks a feature the
runtime selected faults with an illegal instruction inside a forward
pass. The sidecar therefore carries the CPUID that the machine booted
with, and the thaw compares the host's supported CPUID against it,
leaf by leaf, and refuses on a missing feature. The frequency check
becomes one row of that comparison.

## State outside the sidecar

The sidecar does not hold the debug registers or MSR_IA32_CR_PAT. A
hardware watchpoint set before the freeze is gone after the thaw. The
PAT layout of the guest reverts to the KVM default. Both are readable
from inside. Neither affects a guest that does not use them. They are
listed here so that the list of what a thaw does not restore is
complete.

## Not in scope

External references. A network peer, a TLS certificate, a DNS TTL, and
a host clock all carry real time into the guest. The membrane holds
the frames, and the guest's own stack continues from the freeze
instant, but the world on the other side of the valve moved. This is
the accepted cost of the cryogenic principle, and the time model does
not try to hide it.
