# Device state across freeze and thaw

DRAFT. This document designs the last missing piece of the cryogenic
principle: the virtio devices. The clocks, the vCPU, the interrupt
hardware, the serial registers, and the RNG state already continue
across a thaw. The virtio transports do not, and the guest wedges on
its first post-thaw disk touch.

## The gap, measured

The guest keeps its driver state in RAM, and the freeze preserves
RAM. The thaw constructs new MmioTransport objects at reset state:
status 0, no queue ready, a next-available index of 0. The two sides
disagree, and the first request lands in a ring that the device never
reads. Field evidence (2026-08-30, bare metal):

- A shell appended to its history file. The write entered the ext4
  journal, and the commit waited forever on a virtio-blk request. The
  shell stood in the D state in do_get_write_access, jbd2 in
  jbd2_journal_commit_transaction, while serial interrupts kept
  arriving.
- A first `ps` after a thaw hung the same way: the exec faulted in
  cold pages of busybox, and the read went to the dead disk.
- Networking after a thaw is dead for the same reason.

Until AC1 landed, `make demo` ran its guest with ROOT=ro, and a
thawed guest lived on what its RAM held at the freeze instant.

## What the device holds, and what RAM holds

The rings themselves live in guest RAM, and the freeze already
preserves them. The device-side state is small:

| State | Where today | Freeze today |
|---|---|---|
| descriptor, available, used rings | guest RAM | preserved |
| driver state (DRIVER_OK, addresses, indices) | guest RAM | preserved |
| device status register | MmioTransport | lost |
| queue select | MmioTransport | lost |
| per queue: ready, size | virtio-queue Queue | lost |
| per queue: desc, avail, used addresses | virtio-queue Queue | lost |
| per queue: next-available, next-used index | virtio-queue Queue | lost |

The next-available and next-used indices are the only progress
counters that RAM does not hold: they are the private position of the
device in the rings.

## Why the save is simple: nothing is in flight

- virtio-blk is synchronous: the notify handler completes each
  request with pread64/pwrite64 before it returns. The run loop stops
  only between dispatches, thus the freeze never catches a request
  half-done.
- virtio-net TX is a direct copy to the TAP in the notify handler,
  and RX only fills posted buffers during the run-loop pass. An
  inbound frame that arrives in the freeze instant is lost, and the
  protocols above retransmit; the same rule as the serial RX FIFO.

One exception exists: a held egress frame. The mechanism is
hold-then-freeze, in that order. The VMM parks an outbound frame at
a defined point -- read from the TX ring, not yet written to the
TAP, not yet completed -- and returns to the run loop. The ordinary
freeze then fires from outside, and the sidecar captures a clean
parked state. Nothing ever freezes mid-operation, and the resumption
is deliver-and-complete, not excavation.

The polarity is default-hold: the manager cannot know in advance
which destination needs the world to grow, thus every egress frame
parks, and the verdict comes from outside. A release forwards the
frame and completes it; a release can carry an allow, which installs
a pass entry for the destination, and the rest of the flow runs at
full speed: the verdict cost is amortized per destination, not paid
per frame -- one park and one verdict for each destination without a
pass entry, and an inline table match for every frame after it.
The other verdict is a freeze: the world grows first, the thaw
delivers the same frame, and the decision time never enters the
clock of the guest. The VMM offers park, report, release, and allow;
every policy lives outside it. The concrete surface: SIGUSR2 turns
the hold on, each park writes a report line with its destination,
and SIGWINCH applies the verdict file in the state directory --
`allow <ip>:<port>` lines install pass entries, and every parked
frame is then released, delivered, and completed. The freeze verdict
is the ordinary freeze signal.

Therefore the save is a copy of registers, indices, and the held
egress frames; no drain step exists beyond them.

## The design: sidecar format v7

The sidecar gains one block per transport, in device order (block,
then net when present):

- device status (u32)
- queue select (u32)
- per queue: ready (u8), size (u16), and the three ring addresses
  (u64 each), next-available (u16), next-used (u16)
- held egress frames: a count, then each frame with its descriptor
  head index and its length (frames read from the TX ring and not
  yet written to the TAP at the freeze instant)

At thaw, after the vCPU restore and before the first KVM_RUN, each
transport rebuilds from its block: the Queue objects take their
addresses, sizes, ready flags, and indices, and the status register
returns to DRIVER_OK. The backing objects need nothing: the block
device reopens the same disk file, and the net device reopens the
same persistent tap.

A machine frozen with net and thawed on a host where the tap is gone
fails at start with the existing tap error; the manifest records the
claim, and setup net recreates the pool. Each pool tap carries a
deterministic MAC, by convention, thus a recreated tap is
indistinguishable from the tap that froze: the cached neighbor entry
of the guest stays valid, and the guest cannot tell.

## Order in the thaw

1. Prefill and warm the stage-2 mappings (unchanged).
2. Restore the vCPU, the clock, the irqchip and PIT, the serial
   registers (unchanged).
3. Restore each transport from its sidecar block, and write the held
   egress frames to the TAP, oldest first. A held frame is then
   completed like any transmitted frame: its buffer is marked used,
   and the device raises the interrupt -- at the trap instant the
   guest still owned no completion, and without this step its driver
   would leak the descriptor. Beyond the held frames no interrupt is
   raised: a virtio device signals when it uses a buffer, and no
   other buffer was used across the freeze.
4. First KVM_RUN. The guest continues; its next request lands in a
   ring the device now reads at the right position.

## Acceptance criteria

Each criterion stands alone, in dependency order. Each has a gate
target (`make device-state-ac1` .. `device-state-ac4`), and
`make smoke-device-state` runs all four.

| Criterion | State |
|---|---|
| **AC1 -- the disk survives the thaw** | The sidecar (v7) carries the transport state, the thaw restores it before the first KVM_RUN, and `make demo` runs on a rw root. The gate writes a file, freezes, thaws, reads it back, and syncs. |
| **AC2 -- the network survives the thaw** | The tap claim persists through the manifest, the transport restore covers virtio-net, and the gate pings the guest before the freeze and after the thaw. A missing tap fails at start; `setup net` recreates the pool by convention. |
| **AC3 -- the in-flight layer is exact** | The park point sits in the net TX handler, a signal turns the hold on, the sidecar carries the parked frames with their descriptor head indices, and the thaw delivers and completes them. The gate fetches a real www page: the fetch parks, the machine freezes, and the same request completes after the thaw. |
| **AC4 -- the verdict is external** | Every egress frame parks under hold, the park reports its destination, and the verdicts come from outside: release with allow installs a pass entry and the flow runs at full speed, or freeze, grow the world, thaw, deliver. The guest never knows. The world-ratchet gate proves it end to end against real endpoints. |

The clock gates must not move: the transport restore adds host-time
work outside the clock window, and the probes verify that nothing
entered the guest clock.

## The world-ratchet gate

The main acceptance criterion, above the disk and net gates. The
world moves like a ratchet: each request for a missing part adds the
part, and the world never moves backward. The sequence is demand
paging applied to the world:

1. The guest sends a request toward an endpoint that does not exist.
2. The host freezes the machine at the egress moment; the VMM holds
   the outbound frame in the sidecar.
3. The world engine materializes the endpoint (the test stands in a
   listener that starts only after the freeze).
4. The host thaws the machine. The VMM writes the held frame to the
   TAP; the same request lands on the endpoint that now exists.
5. The guest receives the answer. No retransmission, no error, and
   no guest-visible delay: the clock of the guest did not advance
   across the materialization.

The park point sits in the net notify handler, between the read of
the available ring and the write to the TAP: the one place where the
VMM holds the request and the guest already considers it sent. The
hold precedes the freeze, thus the freeze itself stays ordinary.
Which destination merits which verdict lives outside the VMM and
outside this document; the gate here proves the mechanism with a
park, a freeze, a materialization, and a thaw driven by the test as
the stand-in engine.

## Not in scope

- In-flight request draining: nothing is ever in flight (see above).
- Device hotplug, config-space changes, multi-queue: the transports
  are static, single-queue, and the config generation is constant.
- The serial device: its registers already ride the sidecar (v6).
