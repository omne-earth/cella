# Cella grows more cores - SMP under the cryogenic clock

A body for a resident mind should not cap the mind at one core:
every real workload in these guests (an agent's builds, inference,
the terminator under burst) is multithreaded and today serializes
on one vCPU. This phase gives a machine `--vcpus N` without
surrendering the crown jewel: the freeze stays cryogenic, the
one-shot stays one-shot, and every clock proof generalizes per
vCPU instead of being averaged away.

Evidence that schedules this phase: (fill in the trial or workload
that is measurably starved on one core -- titanium trial books, or
volve's phase runs. Evidence first; until then this document is
the plan, not the work.)

## Rulings

- No ACPI (standing): CPU discovery rides the Intel MP table
  (firecracker's road) -- ~200 lines of table construction in
  boot/x86_64.rs, guest-visible surface stays thin.
- First scope is `--vcpus 2`. Every coordination problem appears
  at two; nothing new appears at sixteen. Default stays 1, and a
  manifest without the key means 1 (old manifests boot unchanged).
- The freeze format bumps (v9 -> v10): the sidecar carries N vCPU
  states. A v9 sidecar thaws as one vCPU -- old frozen machines
  keep their thaw.

## Now (feat/smp)

- The design note first: docs/SMP-FREEZE.md -- the stop protocol
  (who parks, who halts whom, in what order), the TSC restore
  contract (all vCPUs resume with one coherent offset; the guest
  must not see a torn clock), and what "the machine stops before
  the guest runs again" means when the guest is plural.
- MP table in boot/x86_64.rs: N processor entries, the ioapic
  entry, checksummed at the canonical address; `nproc` in the
  guest reports N.
- vCPU threads: one run loop per vCPU, KVM_RUN each on its own
  thread; the serial, MMIO, and ledger paths audited for the
  single-loop assumptions they were born with (the flush, the
  valve kick, the console poll live on one thread today).
- The one-shot generalized: a park on any vCPU freezes the
  machine -- every other vCPU halts coherently (immediate_exit,
  then a barrier) before the ledger flush declares the park; no
  vCPU runs an instruction past the freeze line.
- Freeze/thaw v10: save and restore N vCPU states; TSCs restored
  in lockstep (one offset, applied to all, verified after
  KVM_SET_MSRS); kvmclock stays the single source it is.
- The gates, before the features count as landed:
  - smoke-smp: boot at --vcpus 2, `nproc` says 2, a two-thread
    workload finishes ~2x the one-thread wall (a wide floor, not
    a benchmark).
  - the clock probes per vCPU: freeze-thaw clock gate reads both
    TSCs; the 3-sigma band holds on each, and the cross-vCPU skew
    has its own bound (the torn-clock gate).
  - smoke-memory at --vcpus 2: the address map and the plural
    guest coexist.
  - the battery entire, both flavors, before merge (SMP touches
    every run-loop line; this is boot-line class).

## Later

- --vcpus beyond 2 (the table and the threads already carry it;
  the proof burden is the scheduler under freeze, not the count).
- vCPU pinning / host-core affinity, if a resident's latency
  matters enough to measure.
- The membrane under SMP load: the reply-window arithmetic and
  the engine's verdict path re-measured with a plural guest
  driving them (t9/t10 at --vcpus 2).

## Done

(nothing yet -- the phase opens when the evidence arrives)
