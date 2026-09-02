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
closed: every egress operation parks until a decision.

The rule applies at every nesting depth -- a cella inside a cella
creates pairs too, the same inductive principle. The base case:
appliances are the one machine class that touches a pool tap
directly. An appliance gets no appliance; it is the membrane. The
cost (two VMMs per machine, at every depth) is accepted and paid.

## The shape

- **One appliance per machine.** The appliance is an ordinary
  cella machine -- the same VMM class, the same defaults, the same
  freeze machinery -- running the gateway flavors: a gateway
  kernel (the canonical fragment plus netfilter, for transparent
  termination, and nothing else gets netfilter) and the gateway
  rootfs. Two interfaces: the agent side, an L2 pair shared with
  its machine alone, and the world side, a pool tap. The machine's
  only neighbor is its appliance.
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

The chronicle is the machine's append-only record of what its
operations did -- parked, released, lapsed, when and to where --
written as history, never read back as truth.

The membrane is a holder process inside the appliance guest --
pure dataplane, no control agent. Its held state (parked
operations, warm connections, buffered bytes) lives in the guest's
own memory, thus the appliance's ram.img is the vessel: a freeze
preserves the holds, and a thaw resumes them, the cryogenic
principle doing the persistence. The sidecar carries no payload
blocks; the ledger (below) is a chronicle, never the store.

The valve has two postures, and no third: closed and open. A
machine is born closed -- the coconut. Nothing goes in or out: no
park, no ledger, no freeze. The machine runs dark, and its own
calls fail at the tap. `cella gateway open` turns the tap into the
membrane, never into a free flow: every egress frame -- an
initiation or a reply, the echo reply included -- parks for a
decision, and the first parked batch stops the machine. The VMM
completes the TX batch in hand (the batch bounds the holds per
instant, and the ring size is its ceiling), flushes the ledger,
and freezes before the guest runs again: the park is the freeze.
`close` returns the coconut and stops even the flows a decision
allowed. The posture lives in the manifest, thus it survives stop,
thaw, and restart; the verbs change it live through a Valve
message. No unmanaged posture exists on any interface: the raw
flag interface opens into the membrane like every other path.

Nothing is inherited. A pass entry lives only in the running
epoch; the rules evaluate atomically, from the canonical present,
every time. A thaw starts a fresh epoch: traffic to a destination
a past decision allowed parks again, and is decided again. Twins
park and freeze identically, thus the ratchet is deterministic in
the guest's frame. The adversarial spray cannot happen, because
the sprayer freezes after its first batch: no operation cap
exists, and none is needed. The flow: open -> park -> freeze ->
the engine decides by id -> the thaw applies the decisions in park
order -> the answers land.

The membrane holds in both directions, for two different reasons:

- **Egress parks for decisions.** An outbound operation stops at
  the appliance's world side. The frames of one flow group into
  one operation with one identifier (UUIDv7, minted in the guest
  frame; identifiers repeat across branched twins, thus the engine
  keys on machine name plus identifier -- branch demands a unique
  name, and the pair is unambiguous). A release delivers the
  operation and installs the pass entry for its destination, for
  the life of that epoch alone. A refusal answers the machine
  cleanly, in-frame, instead of by timeout.
- **Ingress waits for the frozen.** A machine may freeze while its
  appliance stands. The appliance keeps holding, decisions stay
  possible, and the answer waits at the membrane for delivery at
  thaw. Freeze, grow the world by releasing, thaw into the answer.
  With TCP ended at the appliance, the wait costs no memory: the
  membrane stops reading its world side, and flow control makes
  the sender hold the data.

TCP ends at the appliance -- a hard stop. The machine's peer is
always its appliance, in its own frame, thus a connection survives
any freeze length; the world-side flows belong to the appliance,
and it rebuilds them without the machine watching.

**The world side carries TCP only.** UDP dies at the membrane: DNS
is an appliance service (the machine resolves against its
appliance in-frame; the appliance resolves upstream over TCP, and
every resolution becomes a parkable operation on a name), NTP is
dead by design (time comes from the frame), and QUIC falls back to
TCP -- into the terminating membrane instead of around it. ICMP
dies with UDP; diagnosis is host-side.

TLS terminates at the membrane too (a later phase): the pair CA
signs, the machine trusts its own CA from birth, and every
application-layer timestamp becomes rewritable into the machine's
frame at the one boundary.

## The control plane

`cella gateway <name> <verb>` is the surface, its own thin CLI
(cella-gateway, unprivileged: it acts on files and signals, thus
it never lives in the capability binary -- cella-network keeps
CAP_NET_ADMIN and the wiring verbs, nothing else):

- `show` -- the held operations, one line each: identifier,
  destination, age, frame count.
- `release <id>` -- deliver one operation, and install its pass
  entry.
- `refuse <id>` -- lapse one operation, with its why; the park
  order advances.
- `open` / `close` -- the valve. Closed is the coconut: nothing
  goes in or out. Open is the membrane: egress parks for
  decisions. No posture forwards freely.

The ledger lives at `machines/<name>/network/ledger`: an
append-only chronicle of operations (parked, released, lapsed,
timings, bytes). `show` reads it for the held set; `cella info`
renders a network section from it (valve, counts, last RTT,
average latency); branch and archive carry it with the tree -- a
rock keeps its own phone records.

**One language.** `proto/cella.proto` defines the vocabulary, and
every party speaks it: Message (the envelope on the control wire),
Accord (the version handshake), Event (a ledger entry: Operation
parked | Released | Lapsed), Operation (id, Destination, guest_ns,
host_ns -- both clocks, thus the timeline rewrite needs no schema
change), Decision (Release with allow_flow | Refusal), Valve
(OPEN | CLOSED), and `service Engine { rpc Preside(stream Event)
returns (stream Decision) }`.

The transport layers under the language. Between the appliance's
VMM and the membrane: length-delimited protobuf over a second
serial line (ttyS1, gateway machines only, never a console -- the
console noise of ttyS0 must not touch the framing). The VMM
shuttles opaque bytes between the wire and the control files; it
parses nothing, thus gRPC and protobuf never enter a VMM. Outside
the TCB the same messages ride real gRPC: the engine presides over
the identical schema. The gRPC here is the proof of the trade --
the engine implements the service on the longer term, and nothing
gets rewritten when it arrives: the appliance has spoken the
engine's language since birth.

## The invariant

There exists no cella machine whose frames reach a host tap
without an appliance between -- only appliances touch pool taps --
and every machine is born closed. No posture forwards freely, no
pass entry outlives its epoch, and no interface carries an
unmanaged mode.

## The phases

1. **The thin CLI first.** proto/cella.proto, and cella-gateway
   (show, release, open, close) over the hold/release that exists
   today in the machine's own VMM, as a temporary backend behind
   the final surface. No appliance yet. No regression.
2. **The membrane moves.** The gateway kernel (netfilter, ttyS1)
   and the membrane inside the appliance; `create` builds the pair
   (network/, ca/ -- the CA is born at create so the machine's
   trust store carries it from the first boot); the universe and
   lifecycle verbs go pair-wide; the machine's own VMM keeps only
   the raw hold primitive. No regression.
3. **The terminator.** TCP ends at the membrane, TLS terminates
   against the pair CA, and the timeline rewrite follows at the
   same point.

## The accepted costs

- Two VMMs per machine, at every nesting depth; the batteries
  boot pairs, and their runtime grows accordingly.
- A second kernel flavor (gateway: canonical + netfilter) and a
  second UART in the VMM, for gateway machines only.
- The datapath crosses two synchronous VMMs; `cella info` renders
  the cost per machine, permanently, from the ledger.

## Not in scope

- Shared appliances: one appliance serves one machine, always --
  branching demands it.
- Guest-side control agents: control is files and signals on a VMM;
  the membrane is dataplane.
- Direct tap access for machines: removed from the surface, not
  deprecated.
- UDP, ICMP, and every non-TCP protocol on the world side.
