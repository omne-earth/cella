# The network model

The decision record for how a cella machine touches the world.
Status: accepted 2026-09-01. The phases below implement it; the
current code (a pool tap per machine, hold by signal at the agent
VMM) is the starting state, not the model.

## The decision

A machine is always shielded. `cella create <name>` builds one
thing: the machine and its network appliance, as a pair. No flag
creates an unshielded machine, and no configuration connects a
machine to a host tap directly. The appliance's birth state is
closed: every egress operation parks until a verdict.

## The shape

- **One appliance per machine.** The appliance is a small cella
  guest (the gateway rootfs flavor) with two interfaces: the agent
  side, an L2 pair shared with its machine alone, and the world
  side, a pool tap on the host. The machine's only neighbor is its
  appliance.
- **One directory.** The appliance lives inside its machine:
  `machines/<name>/network/` holds its manifest, disk, RAM, and
  sidecar; `machines/<name>/ca/` holds the pair's certificate
  authority. The pair is one tree, thus every operation on the
  machine as an artifact (branch, archive, destroy) carries the
  appliance and the CA with it. A branch forks the pair and its
  lineage CA together.
- **Pair order.** Freeze: the machine first, the appliance last.
  Thaw: the appliance first, the machine last. The world side
  stands before the machine wakes, and the verbs own the order.

## The membrane

The appliance holds in both directions, for two different reasons:

- **Egress parks for verdicts.** An outbound operation stops at the
  appliance's world side. The frames of one flow group into one
  operation with one identifier (UUIDv7: time-ordered); a release
  delivers the operation and installs the pass entry for its
  destination, thus the rest of the flow runs at full speed -- one
  verdict per new part of the world.
- **Ingress buffers for the frozen.** A machine may freeze while
  its appliance stands. The appliance keeps holding, verdicts stay
  possible, and inbound traffic for the sleeping machine buffers at
  the appliance for delivery at thaw. Freeze, grow the world by
  releasing, thaw into the answer.

TCP ends at the appliance -- a hard stop. The machine's peer is
always its appliance, in its own frame, thus a connection survives
any freeze length; the world-side flows belong to the appliance,
and it rebuilds them without the machine watching. TLS terminates
there too (a later phase): the pair CA signs, the machine trusts
its own CA from birth, and every application-layer timestamp
becomes rewritable into the machine's frame at the one boundary.

## The control plane

`cella network <name> <verb>` is the surface:

- `show` -- the held operations, one line each: identifier,
  destination, protocol, age, frame count.
- `release <id>` -- deliver one operation, and install its pass
  entry.
- `open` / `close` -- the posture of the valve. Open forwards
  (a standing release); close parks everything. Either way the
  frames pass through the appliance: open opens the valve, it
  never removes the pipe.

The verbs act on the appliance's VMM through its files and signals
under `machines/<name>/network/`. No agent runs inside any guest
for control; the appliance guest is pure dataplane, and its VMM is
the control seam. The same seam later carries the engine protocol
(the relay speaks it outside the TCB; gRPC never enters a VMM).

## The invariant

There exists no cella machine whose frames reach a host tap
without an appliance between, and the appliance's birth state is
closed. Posture is a runtime verb on the appliance, never a
property a machine is created with.

## The phases

1. **The thin CLI first.** `cella network` (show, release, open,
   close) over the hold/release that exists today in the machine's
   own VMM. No appliance yet. No regression.
2. **The membrane moves.** Hold/release migrates to the appliance's
   world side; `create` builds the pair; the machine's own VMM
   keeps only the raw hold primitive. No regression.
3. **The terminator.** TCP ends at the appliance; the pair CA
   lands in `ca/` and the machine's trust store at create; the
   timeline rewrite follows at the same point.

## Not in scope

- Shared appliances: one appliance serves one machine, always --
  branching demands it.
- Guest-side control agents: control is files and signals on a VMM.
- Direct tap access for machines: removed from the surface, not
  deprecated.
