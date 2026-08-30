# Freeze and thaw: time and state

This document describes how cella freezes a VM, how it thaws the VM,
and what the guest can observe.

## The principle

A freeze must not exist for the guest. The guest stops, and the guest
continues. Time, timers, register state, and entropy continue from the
point of the stop. We call this the cryogenic principle. The probes in
`probes/` measure it, and the gates in the probes enforce it.

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
  cella also calls KVM_KVMCLOCK_CTRL, and the clocksource watchdog of
  the guest then accepts the pause.
- **Timers, FPU and xstate, RNG state.** All continue from the point of
  the stop. This is the cryogenic principle applied to the full machine
  state. The RNG state persists across the thaw by design; do not
  reseed it.

Two qualifications:

1. The guest experiences its own resume. The first cycle after the thaw
   runs in real time. On nested KVM, 2.5 ms to 4 ms of outer-hypervisor
   work lands inside that cycle. On bare metal the same value is zero
   within the noise.
2. "Seamless" holds at the resolution that we can verify. The host-side
   pairing instrumentation proves the sub-millisecond continuity. No
   measurement from inside the guest can prove it. From inside, the
   guest can discover a freeze only through an external reference: a
   network peer, or a host clock.

## Architecture

Guest RAM is one MAP_SHARED file (`ram.img`). The RAM file is the
freeze image. A sidecar file (`state`) holds the vCPU state, the
irqchip and PIT state, the kvmclock value, and the format version.
`finalize_thaw` deletes the sidecar after a successful thaw, thus a
sidecar thaws one time only.

```mermaid
flowchart LR
    subgraph state["state dir"]
        RAM["ram.img (guest RAM, MAP_SHARED)"]
        SC["state (sidecar, format v5)"]
    end
    C1["cella (freeze run)"] -->|msync + write| state
    state -->|read + restore| C2["cella (thaw run)"]
    C2 -->|finalize_thaw deletes| SC
```

## The freeze sequence

SIGUSR1 triggers the freeze. The sequence reads the TSC and the
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
    C->>K: KVM_KVMCLOCK_CTRL (tell the guest it was stopped)
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

## Results (2026-08-30)

| Machine    | Prefill | Difference against the baseline mean | Verdict |
|------------|---------|--------------------------------------|---------|
| nested KVM | off     | +23 ms to +28 ms                     | FAIL    |
| nested KVM | on      | +2.5 ms to +4.3 ms                   | FAIL    |
| bare metal | on      | -0.128 ms                            | PASS    |

The excess is a constant cost of each thaw. It does not change with
the length of the freeze (0 s, 6 s, and 20 s give the same value). The
nested KVM remainder comes from the outer hypervisor: a thaw makes a
new VM, and the outer hypervisor rebuilds its shadow of the stage-2
tables on the first guest access. That work is below the reach of the
VMM. It does not exist on bare metal. Bare metal is the reference for
this gate.

## Nested VMM

On a nested machine, three layers run: L0 is the outer hypervisor, L1
is the host that runs cella, and L2 is the guest. A thaw makes a new
KVM VM in L1. The prefill fills the stage-2 tables of L1, and that
removes ~21 ms of the excess. But L0 keeps its own combined mapping
for L2, a shadow that L0 builds from the stage-2 tables of L1 and from
its own tables. L0 builds that shadow on the first access of L2 to
each page. The prefill cannot reach it: the L1 kernel performs the
prefill as normal memory writes, and L0 sees those writes as L1
activity, not as L2 accesses. The shadow of L0 stays cold.

In the first heartbeat cycle after the thaw, each page that the guest
touches therefore exits to L0 one time for the shadow fill. The exits
occur while the guest runs. The kvmclock tracks host real time while
the guest runs, thus the clock of the guest counts the stall: the
+2.5 ms to +4.3 ms remainder. The value is constant for each thaw
(the working set is the same), it does not change with the length of
the freeze, and no L1 ioctl can fill the structures of L0 in advance.

```mermaid
flowchart TD
    L2["L2 guest touches a page"] --> Q1{"stage-2 entry in L1?"}
    Q1 -->|"yes (prefill)"| Q2{"shadow entry in L0?"}
    Q1 -->|no| F1["fault to L1<br/>(the prefill removes this, ~21 ms)"]
    Q2 -->|yes| RUN["guest continues"]
    Q2 -->|no| F0["exit to L0, shadow fill<br/>(+2.5 ms to +4.3 ms per thaw,<br/>below the reach of the VMM)"]
    F0 --> RUN
```

On bare metal, no L0 exists. The prefilled stage-2 tables are the only
translation layer, and the measurement confirms it: -0.128 ms, zero
within the noise. Bare metal is therefore the reference for this gate.

## Reproduce

```
make smoke-thaw                                   # full workflow + both probes
make probe-freeze-thaw-clock                      # the crossing measurement
CELLA_THAW_PREFAULT=off make probe-freeze-thaw-clock   # the cold thaw
make probe-prefault-ept                           # explicit prefill variant
```
