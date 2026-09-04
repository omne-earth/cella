# Device state across freeze and thaw

This document designs the last piece of the cryogenic principle:
the virtio devices. The design is delivered -- the sidecar carries
the transports (v7, with the nested block at v8 and the ingress
lanes at v9, the current format), and the acceptance table below
records the state of each criterion. Before it, the clocks, the vCPU, the
interrupt hardware, the serial registers, and the RNG state
continued across a thaw; the virtio transports did not, and the
guest wedged on its first post-thaw disk touch.

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

Until AC1 landed, the shell gate (now `make smoke-shell`) ran its
guest with ROOT=ro, and a
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

```mermaid
graph TB
    subgraph RAM["guest RAM, ram.img -- the freeze preserves it whole"]
        R1["descriptor, available, used rings"]
        R2["driver state: DRIVER_OK, addresses, indices"]
    end
    subgraph SC["the transport block in the sidecar -- lost without it"]
        S1["device status register"]
        S2["queue select"]
        S3["per queue: ready, size, ring addresses"]
        S4["per queue: next-available, next-used"]
        S5["held egress frames, and the ingress lanes"]
    end
    RAM --- G["N.G.1 the guest's view"]
    SC --- M["N.M.1 the device's view"]
```

## Why the save is simple: nothing is in flight

- virtio-blk is synchronous: the notify handler completes each
  request with pread64/pwrite64 before it returns. The run loop stops
  only between dispatches, thus the freeze never catches a request
  half-done.
- virtio-net TX is a direct write to the edge fd in the notify
  handler, and RX only fills posted buffers during the run-loop
  pass. An inbound frame that arrives while the machine runs parks
  in the ingress lane (N.M.2) and rides the sidecar; one that
  arrives while the machine is frozen is lost at the edge, and the
  protocols above retransmit.

One exception exists: a held egress frame. The mechanism is
hold-then-freeze, in that order. The VMM parks an outbound frame at
a defined point -- read from the TX ring, not yet written to the
edge, not yet completed -- finishes the run-loop pass, flushes the
ledger, and then freezes: the park is the freeze
(docs/NETWORK-MODEL.md, "The membrane"). The sidecar captures a
clean parked state. Nothing ever freezes mid-operation, and the resumption is
decide-then-deliver, not excavation.

Under the open valve the polarity is default-hold, and the park is
the freeze: the manager cannot know in advance which destination
needs the world to grow, thus every egress frame parks -- and the
first park stops the machine. No pass table exists (the
total-membrane ruling; proto field allow_flow is retired): a
release delivers one operation and nothing more, and the next
frame to the same destination parks again. Every policy lives
outside the VMM.

```mermaid
sequenceDiagram
    participant G as N.G.1 guest
    participant M as N.M.1 virtio-net, the membrane
    participant L as N.F.3 the ledger
    participant E as the engine, the gates stand in
    E->>M: cella gateway open, via N.X.1<br/>(the machine is born closed)
    G->>M: TX frame to an undecided destination
    M->>M: park, the operation gets its id,<br/>minted in the guest frame
    M->>L: Parked, with id, destination, both clocks
    Note over M: the park is the freeze, the batch drains,<br/>the ledger flushes, the machine stops
    E->>L: read the open operations
    E->>M: a Decision by id, into the verdict file (N.F.2)
    E->>M: thaw, via N.L.1
    M->>M: the decisions apply, in park order
    M->>G: released frames deliver and complete,<br/>a refusal lapses, undecided stays held
```

The chronicle is the machine's append-only record of what its
operations did -- parked, released, lapsed, when and to where --
written as history, never read back as truth.

The concrete surface, in order:

1. A machine is born closed: nothing in or out, no
   parking, no freeze. `cella gateway <machine> open` arms the
   membrane; `close` returns the dark. The posture is the valve
   file (N.F.1) and survives every thaw.
2. A park mints the operation id (v7-shaped, the guest frame in
   the timestamp bits), appends Parked to the chronicle at
   machines/<name>/network/ledger, and writes a report line.
3. The run-loop pass completes, the ledger flushes, and the
   machine freezes itself -- the park is the freeze. Held frames
   ride the sidecar; the operation records ride the chronicle.
4. Decisions are framed proto messages in the verdict file, one
   per operation id, written by `cella gateway <machine> release|refuse
   <id>`. The thaw applies them strictly in park order: a release
   delivers and completes one operation, and installs nothing; a
   refusal lapses the operation; an operation behind an undecided
   predecessor stays held. Against a running machine the verb
   kicks the VMM and an incoming decision applies at once (the
   signals underneath are the wire, never the surface). No allow
   outlives its decision -- there is nothing for it to outlive:
   the next frame to a released destination parks again.

Therefore the save is a copy of registers, indices, the held
egress frames, and the ingress lanes; no drain step exists beyond
them.

## The design: the transport block (v7, then v8, then v9)

The sidecar gains one block per transport, in device order (block,
then net when present):

- device status (u32)
- queue select (u32)
- per queue: ready (u8), size (u16), and the three ring addresses
  (u64 each), next-available (u16), next-used (u16)
- held egress frames: a count, then each frame with its descriptor
  head index and its length (frames read from the TX ring and not
  yet written to the edge at the freeze instant)
- since v9: the ingress lane's held and deliverable frames, per
  transport

Version 8 adds the nested-virtualization block, after the
transports: the raw KVM nested-state bytes and the vendor MSRs the
host can read (MSR_VM_HSAVE_PA on AMD, IA32_FEATURE_CONTROL on
Intel). A guest can run KVM
itself, and the entry state of its inner VM lives in the host
kernel, not in guest RAM. A freeze that drops this state makes the
thawed guest triple fault on its next VM entry. A host without the
capability writes an empty block, and the thaw skips it.

At thaw, after the vCPU restore and before the first KVM_RUN, each
transport rebuilds from its block: the Queue objects take their
addresses, sizes, ready flags, and indices, and the status register
returns to DRIVER_OK. The backing objects need nothing: the block
device reopens the same disk file, and the net device reconnects to
the machine's own translator through edge.sock (N.F.4), one hello
per nic.

The translator is machine-lifetime (docs/ROOTLESS-NETWORK.md, "The
translator"): it held the world sockets and the wires across the
freeze, thus the thawed guest resumes into the same flows, and
nothing on the host needs recreating.

## Order in the thaw

1. Prefill and warm the stage-2 mappings (unchanged).
2. Restore the vCPU, the clock, the irqchip and PIT, the serial
   registers (unchanged).
3. Restore each transport from its sidecar block, and rebind each
   held frame to its operation through the ledger: the chronicle
   holds the open ids, and a restored frame rejoins by its
   primitive name. The never-guess rebind is the architecture, not
   scaffolding (ruled 2026-09-02): one candidate operation and a
   consistent frame count, or the frame stays held under a fresh
   id. Nothing delivers on its own: the queued decisions of the
   verdict file apply in park order, and a released operation's
   frames then go to the edge oldest first, each buffer marked
   used and the interrupt raised -- at the trap instant the guest
   owned no completion, and without the completion its driver
   would leak the descriptor. An undecided operation stays held. Beyond the
   released frames no interrupt is raised: a virtio device signals
   when it uses a buffer, and no other buffer was used across the
   freeze.
4. First KVM_RUN. The guest continues; its next request lands in a
   ring the device now reads at the right position.

## Acceptance criteria

Each criterion stands alone, in dependency order. Each has a gate
target (`make device-state-ac1` .. `device-state-ac5`), and
`make smoke-device-state` runs all five.

| Criterion | State |
|---|---|
| **AC1 -- the disk survives the thaw** | The sidecar carries the transport state, the thaw restores it before the first KVM_RUN, and `make smoke-shell` runs on a rw root. The gate writes a file, freezes, thaws, reads it back, and syncs. |
| **AC2 -- the network survives the thaw** | The transport restore covers virtio-net, and the machine-lifetime translator holds the machine's flows across the freeze. The gate knocks, decides the guest's parked answer, freezes, thaws, and decides the answer of the new epoch: the same nic, the same translator, a fresh epoch's judgments. |
| **AC3 -- the in-flight layer is exact** | The park point sits in the net TX handler, the open verb arms the membrane, the sidecar carries the parked frames with their descriptor head indices, and the operations survive the thaw as held -- ids rebound through the ledger. A decision by id releases each one, in park order. The gate walks a fetch against the host's stand-in endpoint, one decision per frame, deterministically: a freeze loses in-flight sender segments, thus the exactness leg stays local and the true-world leg is AC5. |
| **AC4 -- the verdict is external** | Every egress frame parks under the open valve into an operation with an id, the park reports its destination, and the decisions come from outside, by id, applied in park order. The world-ratchet gate proves it end to end: the request toward an endpoint that does not exist parks and freezes the machine, the world grows while it sleeps, and the release lands the same request. The guest never knows. |
| **AC5 -- the true world** | A real internet fetch crosses the total membrane, one decision per frame (a small plain-HTTP endpoint; the gate skips when the host is offline). The leg rides the peer-patience bound: a segment that arrives at a frozen machine is lost at the edge, and the far end must retransmit -- thus AC3 keeps the deterministic stand-in, and this leg touches the world. |

The clock gates must not move: the transport restore adds host-time
work outside the clock window, and the probes verify that nothing
entered the guest clock.

## The world-ratchet gate

The main acceptance criterion, above the disk and net gates. The
world moves like a ratchet: each request for a missing part adds the
part, and the world never moves backward. The sequence is demand
paging applied to the world:

1. The valve is open, and the guest sends a request toward an
   endpoint that does not exist.
2. The machine freezes itself at the egress moment -- the park is
   the freeze -- with the outbound frames in the sidecar and the
   operation in the chronicle.
3. The world engine materializes the endpoint (the test stands in a
   listener that starts only after the freeze).
4. The engine decides the operation by id, and the host thaws the
   machine. The thaw applies the decision, the VMM writes the held
   frames to the edge, and the same request lands on the endpoint
   that now exists.
5. The guest receives the answer. No retransmission, no error, and
   no guest-visible delay: the clock of the guest did not advance
   across the materialization.

```mermaid
sequenceDiagram
    participant G as N.G.1 guest
    participant M as N.M.1 the membrane
    participant E as the engine, the gate stands in
    participant W as the endpoint, absent at first
    G->>M: request toward an endpoint that does not exist
    Note over M: park, then the freeze -- the frames in<br/>the sidecar, the operation in the chronicle (N.F.3)
    E->>W: materialize the endpoint<br/>(the listener starts after the freeze)
    E->>M: release by id, then thaw
    M->>W: the held frames cross the edge, oldest first
    W->>M: the answer
    M->>G: parked incoming, released, delivered --<br/>the guest clock never advanced
```

The park point sits in the net notify handler, between the read of
the available ring and the write to the edge: the one place where
the VMM holds the request and the guest already considers it
sent. The
park precedes the freeze it triggers, thus the freeze itself stays
ordinary. Which destination merits which decision lives outside the
VMM and outside this document; the gate here proves the mechanism
with a park, its self-freeze, a materialization, a decision by
id, and a thaw, driven by the test as the stand-in engine.

## Not in scope

- In-flight request draining: nothing is ever in flight (see above).
- Device hotplug, config-space changes, multi-queue: the transports
  are static, single-queue, and the config generation is constant.
- The serial device: its registers already ride the sidecar (v6).
