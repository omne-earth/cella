# Freeze and thaw: time and state

This document describes how cella freezes a VM, how it thaws the VM,
and what the guest can observe.

## The principle

A freeze must not exist for the guest. The guest stops, and the guest
continues. Time, timers, register state, and entropy continue from the
point of the stop. We call this the cryogenic principle. The probes
(`cella probe ...`) measure it, and their gates enforce it.

## Is time seamless for the guest?

Yes, within what we can measure, the freeze does not exist for the
guest:

- **Monotonic clock (kvmclock).** Continuous to microseconds. cella
  reads the TSC and the kvmclock 2 us to 5 us apart at the freeze, and
  writes them 2 us apart at the thaw. On bare metal, the heartbeat
  interval that contains the freeze differs from a normal interval by
  -0.128 ms, inside the measurement noise. A `sleep` that the freeze
  interrupts continues, and it completes its remaining time.
- **Wall clock (CLOCK_REALTIME).** Also frozen, by design. The restore
  does not use the KVM_CLOCK_REALTIME flag. The guest continues at the
  epoch of the freeze, 6 s behind the host after a freeze of 6 s. This
  is the design choice, not a defect.
- **TSC.** Restored to the frozen value, consistent with the kvmclock.
  The guest cannot compare the two clocks and find a discontinuity.
  cella does not call KVM_KVMCLOCK_CTRL: that call sets the
  PVCLOCK_GUEST_STOPPED flag, and the flag tells the guest that it was
  stopped. The guest does not need the flag. The command line contains
  tsc=reliable, thus the clocksource watchdog does not run, and the
  other watchdogs measure guest time, which does not advance across
  the freeze. The probes verify the absence of watchdog complaints
  after each thaw.
- **Timers, FPU and xstate, RNG state.** All continue from the point of
  the stop. This is the cryogenic principle applied to the full machine
  state. The RNG state persists across the thaw by design; do not
  reseed it.

Two qualifications:

1. The guest experiences its own resume. The first cycle after the
   thaw runs in real time. The warming stub moves the wake-up cost
   into host time on every layer; without it (thaw mode "ept"),
   2.5 ms to 4 ms of outer-hypervisor work lands inside that cycle
   on nested KVM. On bare metal the value is zero within the noise
   in either mode.
2. "Seamless" holds at the resolution that we can verify. The host-side
   pairing instrumentation proves the sub-millisecond continuity. No
   measurement from inside the guest can prove it. From inside, the
   guest can discover a freeze only through an external reference: a
   network peer, or a host clock.

## Architecture

Guest RAM is one MAP_SHARED file (`ram.img`). The RAM file is the
freeze image. A sidecar file (`state`, format v7) holds the vCPU
state, the irqchip and PIT state, the kvmclock value, the serial
registers, one block per virtio transport (with any held egress
frames), and the format version.
`finalize_thaw` deletes the sidecar after a successful thaw, thus a
sidecar thaws one time only.

```mermaid
stateDiagram-v2
    [*] --> Running: boot (cella with a kernel, RAM maps ram.img)
    Running --> Frozen: SIGUSR1, or a park under a closed valve<br/>(msync ram.img, write the sidecar, process exit)
    Frozen --> Running: thaw (new process on the same state dir -- prefill, restore, delete the sidecar)
    Running --> [*]: guest shutdown
```

The Frozen state is only the two files. No process runs, and no time
passes for the guest. The sidecar thaws one time: the thaw deletes it,
and a second thaw of the same sidecar cannot occur.

A freeze and a thaw are two separate cella processes. The first
process runs from the boot to the freeze, and it exits. The second
process starts against the same state dir, and it runs from the thaw
onward. Nothing survives in memory between the two processes. The two
files are the complete guest.

```mermaid
flowchart LR
    C1["cella, first process<br/>(boot, then freeze, then exit)"] -->|"msync"| RAM
    C1 -->|"write"| SC
    subgraph state["state dir"]
        RAM["ram.img (guest RAM, MAP_SHARED)"]
        SC["state (sidecar, format v7)"]
    end
    RAM -->|"map"| C2["cella, second process<br/>(thaw, then continue)"]
    SC -->|"restore, then delete"| C2
```

## The freeze sequence

Two triggers reach the same freeze path:

1. SIGUSR1 -- the freeze verb, from outside.
2. A park under a closed valve -- the one-shot rule of the network
   model (docs/NETWORK-MODEL.md): the run-loop pass completes, the
   ledger flushes, and the machine stops before the guest runs
   again.

The sequence reads the TSC and the
kvmclock as close together as possible, because the thaw writes them
as close together as possible. A gap between the two reads becomes a
step in the clock of the guest.

```mermaid
sequenceDiagram
    participant S as SIGUSR1
    participant V as vCPU
    participant K as KVM
    participant F as state dir
    S->>V: request freeze
    V->>V: stop (exit KVM_RUN)
    Note over V,K: save regs, sregs, fpu, lapic,<br/>events, xsave, xcrs
    V->>K: KVM_GET_MSRS (TSC last, with XSS)
    V->>K: KVM_GET_CLOCK (1 us to 5 us after the TSC read)
    V->>K: KVM_GET_IRQCHIP, KVM_GET_PIT2
    V->>F: msync ram.img, write sidecar
    V->>V: process exit
```

## The thaw sequence

A run against a state dir that holds a sidecar thaws instead of
booting. The stage-2 prefill runs before the clock restore, thus its
cost falls outside the clock window of the guest.

```mermaid
sequenceDiagram
    participant C as cella (new process)
    participant K as KVM (new VM)
    participant G as guest
    C->>K: create VM, map ram.img, create vCPU
    C->>K: KVM_PRE_FAULT_MEMORY (fill stage-2 tables)
    Note over C,K: ~20 ms of host time.<br/>The guest clock does not run.
    C->>K: restore vCPU state (MSR batch: TSC first,<br/>XSS restored, TSC_DEADLINE last)
    C->>K: KVM_SET_CLOCK (2 us after the TSC write)
    C->>K: KVM_SET_IRQCHIP, KVM_SET_PIT2
    C->>C: finalize_thaw (delete the sidecar)
    C->>G: first KVM_RUN (~200 us after the clock write)
    G->>G: continue at the frozen clock value
```

Order rules, and the reason for each rule:

- The TSC read is the last read before the clock read, and the TSC
  write is the first write before the clock write. The pair must move
  together.
- MSR_IA32_TSC_DEADLINE is the last MSR in the batch. The deadline is a
  TSC value, thus the TSC must be correct first.
- MSR_IA32_XSS is in the batch. XRSTORS takes a #GP fault when the
  XCOMP_BV of an xsave area holds a component that XCR0 | IA32_XSS does
  not enable. A thaw without XSS panicked the guest on bare metal at
  its first context switch.
- XCR0 is written before the xsave area. KVM selects the components to
  load from the current XCR0.
- The prefill runs before the clock restore. A thaw makes a new KVM VM
  with empty stage-2 page tables. Without the prefill, the first
  heartbeat cycle of the guest pays one stage-2 fault for each page
  that it touches, and the clock of the guest counts that time
  (measured on nested KVM: ~25 ms; ~4 ms with the prefill).

## Measurement and gates

`make probe-freeze-thaw-clock` measures the crossing: the heartbeat
interval of the guest that contains the freeze, in nanoseconds, from
/proc/timer_list. The probe measures wake-up lateness after the thaw,
not a clock step. A clock step smaller than the remaining sleep does
not show in the crossing interval, because the wake-up is scheduled in
the same clock. The host-side pairing instrumentation bounds the clock
step itself to microseconds.

```mermaid
flowchart TD
    A0["kernel complaint after the thaw?<br/>(watchdog, unstable, oops)"] -->|yes| F0["FAIL: the guest resumed,<br/>but not cleanly"]
    A0 -->|no| A
    A["mono_ns data present?"] -->|no| F1["FAIL: cannot verify"]
    A -->|yes| B["|epoch delta - across| <= 1 s?<br/>(quantization of the epoch)"]
    B -->|no| F2["FAIL (INCONSISTENT):<br/>the two guest clocks disagree"]
    B -->|yes| C["|across - mean| <= 3 * s * sqrt(1 + 1/n)?<br/>(prediction interval of the baseline)"]
    C -->|yes| P["PASS (FROZEN)"]
    C -->|no| F3["FAIL (LEAKED)"]
    F3 --> D["excess matches the frozen interval?<br/>-> the restored kvmclock did not take effect"]
```

No gate uses a tuned constant. The 1 s bound is the resolution of the
epoch field. The prediction interval comes from the sample standard
deviation s of the n baseline intervals. `make probe-wallclock` applies
the same rule at boot: the drift tolerance is zero, and zero means that
the guest epoch lies inside the host window from spawn to observation.

The complaint gate runs first. Any watchdog, unstable, or oops line
from the guest kernel after the thaw is a FAIL, before the clock
verdict. `make probe-thaw-gate` runs the probe with a 30 s observation
window, because `make smoke-thaw` runs it with a window of 0 s, and a
window of 0 s can miss a late complaint.

## Results (2026-08-30, guest kernel 7.2.2)

The history of the measurement, on the nested KVM machine (the harder
case: one hypervisor already sits between it and the metal):

| Thaw mode                | Difference against the baseline mean | Verdict |
|--------------------------|--------------------------------------|---------|
| cold (off)               | +23 ms to +28 ms                     | FAIL    |
| ioctl prefill (ept)      | +2.5 ms to +4.3 ms                   | FAIL    |
| prefill + warming (deep) | +0.15 ms                             | PASS    |

Bare metal passes in every mode from "ept" on: -0.128 ms to +0.33 ms
across runs, inside the interval, with no kernel complaints over a
30 s watch. The excess of the cold thaw is a constant cost of each
thaw: it does not change with the length of the freeze (0 s, 6 s, and
20 s give the same value). "deep" is the default
(config::DEFAULT_THAW_PREFAULT); see "The fix" in
docs/NESTED-BOOT.md for the full account across nesting depths.

## Nested VMM

On a nested machine, three layers run: L0 is the outer hypervisor, L1
is the host that runs cella, and L2 is the guest. A thaw makes a new
KVM VM in L1. The prefill fills the stage-2 tables of L1, and that
removes ~21 ms of the excess. But L0 keeps its own combined mapping
for L2, a shadow that L0 builds from the stage-2 tables of L1 and from
its own tables. L0 builds that shadow on the first access of L2 to
each page. The prefill cannot reach it: the L1 kernel performs the
prefill as normal memory writes, and L0 sees those writes as L1
activity, not as L2 accesses. The shadow of L0 stays cold. One thing
does reach it: a real access by the guest. The warming stub of
src/warm.rs performs those accesses before the clock restore, and
that is the fix (see docs/NESTED-BOOT.md, "The fix").

In the first heartbeat cycle after the thaw, each page that the guest
touches therefore exits to L0 one time for the shadow fill. The exits
occur while the guest runs. The kvmclock tracks host real time while
the guest runs, thus the clock of the guest counts the stall: the
+2.5 ms to +4.3 ms remainder of the prefill-only thaw. The value is
constant for each thaw (the working set is the same), and it does not
change with the length of the freeze. No L1 ioctl fills the
structures of L0 in advance; the warming stub fills them through the
one channel every layer obeys, a guest access.

```mermaid
flowchart TD
    L2["L2 guest touches a page"] --> Q1{"stage-2 entry in L1?"}
    Q1 -->|"yes (prefill)"| Q2{"shadow entry in L0?"}
    Q1 -->|no| F1["fault to L1<br/>(the prefill removes this, ~21 ms)"]
    Q2 -->|yes| RUN["guest continues"]
    Q2 -->|no| F0["exit to L0, shadow fill<br/>(+2.5 ms to +4.3 ms per thaw;<br/>the warming stub moves this cost<br/>outside the clock window)"]
    F0 --> RUN
```

On bare metal, no L0 exists. The prefilled stage-2 tables are the only
translation layer, and the measurement confirms it: -0.128 ms, zero
within the noise. With the warming stub the same zero holds on the
nested machine, and one nesting level deeper on both machines.

## Virtio state (landed)

The freeze did not save the virtio device state, and the gap showed
in the field (2026-08-30, bare metal): after a thaw, a shell write
entered the ext4 journal, and the commit waited on a virtio-blk
request that a reset transport never processed -- the shell in the
D state in do_get_write_access, jbd2 in
jbd2_journal_commit_transaction, serial interrupts still arriving.
The guest keeps its driver state in RAM, the freeze preserves RAM,
and the thaw built new transports at reset state: the two sides
disagreed. The smoke tests and the clock probes cannot see this,
because the heartbeat needs no virtio.

Sidecar format v7 closed it: one block per transport -- the status
register, the queue select, the interrupt status, the negotiated
features, and per queue the ready flag, the size, the ring
addresses, and the next-available and next-used indices (the
private progress counters RAM does not hold) -- plus any held
egress frames. The thaw restores each transport before the first
KVM_RUN, and the demo runs on a rw root. The design, the egress
hold, and the acceptance gates (`make smoke-device-state`) live in
docs/DEVICE-STATE.md.

## Reproduce

```
make smoke-thaw                                   # full workflow + both probes
make probe-freeze-thaw-clock                      # the crossing measurement
CELLA_THAW_PREFAULT=off make probe-freeze-thaw-clock   # the cold thaw
CELLA_THAW_PREFAULT=ept make probe-freeze-thaw-clock   # ioctl prefill, no warming
make probe-prefault-ept                           # explicit prefill variant
make probe-thaw-gate                              # 30 s complaint watch after the thaw
```
