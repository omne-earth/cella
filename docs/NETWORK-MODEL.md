# The network model

The decision record for how a cella machine touches the world.
Accepted 2026-09-01. Revised 2026-09-03: the rootless network
(1.6.14e) is the shipped architecture, and the appliance phases
are descoped (ruled 2026-09-02). The mechanism's design record is
docs/ROOTLESS-NETWORK.md; this document states the law.

The diagrams share one identifier space, prefixed N: N.G is the
guest, N.M the membrane (the VMM), N.T the translator, N.H the
host kernel, N.F the files, N.X the control plane, N.L the
lifecycle plane. An identifier keeps its meaning in every diagram
here and in docs/ROOTLESS-NETWORK.md; a node owned by another
diagram appears as a pointer, named "(see Nx)". The topology
diagrams live in docs/ROOTLESS-NETWORK.md and docs/EXAMPLES.md.

## The decision

A machine is always shielded. The membrane is the machine's own
VMM, and no configuration bypasses it. `cella create <name>` takes
one flag, `--net SPEC`, where SPEC is a comma list of nics:
`none` (the default, airgapped), `world[:PORT/proto+...]`, or
`wire:NAME`. A machine is born closed.

No host network object exists at any depth. The rule applies to a
cella inside a cella: each layer runs its own translators, and
each membrane judges its own frames.

## The shape

- **One translator per machine.** `cella start` spawns
  `cella-network edge <vm>`, one process, machine-lifetime:
  destroy kills it, and it survives freeze and thaw. It holds no
  capability. It exits on its own when its machine directory is
  removed.

- **The world side.** The translator answers ARP and the gateway's
  echo at the edge. It carries ICMP, UDP, and TCP through plain
  host sockets. DNS is UDP like any other; no resolver stands in
  the path.

- **The wire side.** A wire joins exactly two machines. Pairing is
  two manifests that name the same wire; the translators meet at
  `$CELLA_HOME/wires/<name>`. A wire imposes no address
  convention.

### N2 -- the translator

One process per machine, machine-lifetime, no capability.

```mermaid
graph LR
    M1p["N.M.1 virtio-net (see N1)"]
    T1["N.T.1 the edge loop"]
    T2["N.T.2 world side: answers ARP and the gateway echo itself"]
    T3["N.T.3 wire side"]
    H13p["N.H.1-N.H.3 protocol sockets (see N3)"]
    H4p["N.H.4 the wire socket (see N3)"]
    M1p ---|"edge fd"| T1
    T1 --- T2
    T1 --- T3
    T2 --- H13p
    T3 --- H4p
```

### N3 -- the host kernel

Plain sockets, opened by the translator. Nothing else of cella's
exists in the host's network: no tap, no bridge, no rule.

```mermaid
graph LR
    T2p["N.T.2 world side (see N2)"]
    T3p["N.T.3 wire side (see N2)"]
    H1["N.H.1 ICMP DGRAM socket, one per echo id"]
    H2["N.H.2 UDP socket, one per flow"]
    H3["N.H.3 TCP socket, one per flow, plus the knock listeners"]
    H4["N.H.4 CELLA_HOME/wires/NAME: one socket, two ends"]
    W(("the world"))
    PEER["N.T.1 of the peer machine (see N2)"]
    T2p --- H1
    T2p --- H2
    T2p --- H3
    H1 --- W
    H2 --- W
    H3 --- W
    T3p --- H4
    H4 --- PEER
```

- **The guest contract.** The guest sees a byte-identical network:
  the same virtio-net device, gateway 192.168.210.1, guest
  address 192.168.210.2 on a world nic. A guest image cannot tell
  the translator from a tap.

- **One directory.** `machines/<name>/` holds the network's whole
  state: the valve record, the verdict file, the ledger at
  `network/ledger`, and the translator's edge.sock, edge.pid, and
  edge.log. Branch, archive, and destroy carry it as one tree.

### N4 -- the files, their writers and readers

All under machines/NAME/. Each edge names the one writer or
reader; no file has two writers.

```mermaid
graph LR
    X1p["N.X.1 cella-gateway (see N5)"]
    L1p["N.L.1 cella-machine (see N5)"]
    M1p["N.M.1 virtio-net (see N1)"]
    T1p["N.T.1 the edge loop (see N2)"]
    F1["N.F.1 valve"]
    F2["N.F.2 verdict"]
    F3["N.F.3 network/ledger"]
    F4["N.F.4 edge.sock"]
    F5["N.F.5 edge.pid"]
    F6["N.F.6 edge.log"]
    X1p -->|"writes the posture"| F1
    M1p -->|"reads"| F1
    X1p -->|"appends decisions"| F2
    M1p -->|"reads on the kick"| F2
    M1p -->|"appends the chronicle"| F3
    X1p -->|"reads the held set"| F3
    T1p -->|"listens; exits when it is gone (the tether)"| F4
    L1p -->|"connects, one hello byte per nic"| F4
    L1p -->|"writes at spawn; destroy kills by it"| F5
    T1p -->|"stdout, redirected at spawn"| F6
```

## The membrane

The chronicle is the machine's append-only record of what its
operations did -- parked, released, refused, when and to where --
written as history, never read back as truth. Field 15 chains each
entry to its predecessor by SHA-256; `cella doctor verify` proves
the book.

### N1 -- the frame path, machine side

```mermaid
graph LR
    G1["N.G.1 eth0 / ethN"]
    M1["N.M.1 virtio-net: parks, ledgers, freezes"]
    M2["N.M.2 the ingress lane: parks, never freezes"]
    T1p["N.T.1 the edge loop (see N2)"]
    G1 --- M1
    M1 ---|"edge fd, SEQPACKET"| T1p
    T1p ---|"inbound frames"| M2
    M2 -->|"release delivers"| G1
```

The valve has two postures and no third: closed and open. Closed
is dark: no park, no ledger, no crossing, in either direction.
`cella gateway open` opens into the membrane, never into a free
flow. The posture is a file beside the machine; it survives stop,
freeze, thaw, and restart.

### The two automata

The valve's record is N.F.1; the machine's freeze is N.M.1's act.

```mermaid
stateDiagram-v2
    state "the valve (N.F.1, written by N.X.1)" as V {
        closed --> open: cella gateway open
        open --> closed: cella gateway close
    }
    state "the machine (frozen by N.M.1)" as A {
        running --> frozen: its own egress parks
        frozen --> running: thaw -- a fresh epoch,<br/>nothing inherited
    }
```

The two automata are independent: a valve verb works against a
frozen machine, and the posture survives freeze, thaw, stop, and
restart. Closed drops at the membrane in both directions. Open
parks everything: egress freezes the machine, ingress never does.

Every egress frame parks by its most primitive name: (ethertype,
destination MAC), refined to (ip, port, proto) when IPv4 parses.
No frame is exempt -- ARP, IPv6, kernel chatter, any future
protocol. The frames of one flow group into one operation with one
UUIDv7 identifier, minted in the guest's frame.

The park is the freeze:

1. The VMM completes the TX batch in hand (the ring size bounds
   the holds).
2. It flushes the ledger.
3. It freezes before the guest runs again.
4. A release delivers the operation; a refusal lapses it
   cleanly, and the park order advances.

### The egress walk -- the machine's own action

The identifiers are N1-N5's: the frame walks N.G.1 into N.M.1, and a
release walks it out through N.T.1.

```mermaid
sequenceDiagram
    participant G1 as N.G.1 guest
    participant M1 as N.M.1 virtio-net
    participant X1 as N.X.1 cella-gateway
    participant T1 as N.T.1 / N.T.2 / N.T.3 translator
    participant H as N.H.1-N.H.4 host sockets
    G1->>M1: TX frame
    Note over M1: valve (N.F.1) closed: the frame drops here.<br/>No park, no ledger, no freeze.
    M1->>M1: park by primitive key,<br/>append the park to the ledger (N.F.3)
    Note over M1: the park is the freeze:<br/>the TX batch completes, then stillness.
    X1->>M1: release ID: append to the verdict (N.F.2),<br/>kick by SIGWINCH
    M1->>T1: the frame, over the edge fd
    alt world nic (N.T.2)
        Note over T1: ARP and the gateway echo:<br/>answered inside N.T.2, never leaves it.
        T1->>H: ICMP to N.H.1, UDP to N.H.2, TCP to N.H.3
    else wire nic (N.T.3)
        T1->>H: the frame crosses the wire socket (N.H.4)
        Note over H: the peer's N.M.1 judges it again.
    end
```

Nothing is inherited, within an epoch or across one. No pass
table exists: a release delivers one operation, and the next frame
to the same destination parks again. A thaw starts a fresh epoch
and every staged judgment applies in park order. Twins park and
freeze identically; the ratchet is deterministic in the guest's
frame.

## The ingress lane

The world's knock is not the machine's own action. An inbound frame --
a flow's reply, or a connection on a mapped port -- parks in the
ingress lane, and the machine keeps running. The knock reaches a
machine only through the port map its manifest names
(`world:1709/tcp+1709/udp` maps host port 1709 to the guest).

Release is a live wire:

1. Against a running machine, the decision applies now, no thaw
   needed.
2. Against a frozen machine, it stages, and the thaw applies it
   in park order.
3. A frame that arrives while no VMM is attached is discarded at
   the edge and counted, never buffered across the gap.

### The ingress walk -- the world's knock

```mermaid
sequenceDiagram
    participant H as N.H.1-N.H.4 host sockets
    participant T1 as N.T.1 / N.T.2 translator
    participant M2 as N.M.2 ingress lane
    participant X1 as N.X.1 cella-gateway
    participant G1 as N.G.1 guest
    H->>T1: a reply on a flow socket (N.H.1-N.H.2-N.H.4),<br/>or a knock on a mapped port (N.H.3)
    Note over T1: no VMM attached (a frozen epoch):<br/>discarded at the edge, counted in edge.log (N.F.6).
    T1->>M2: the frame, over the edge fd
    M2->>M2: park in the ingress lane,<br/>append the park to the ledger (N.F.3)
    Note over M2: the machine keeps running --<br/>the world's knock is not the machine's own action.
    X1->>M2: release ID: a live wire --<br/>applies now, no thaw needed
    M2->>G1: the frame lands
```

## The control plane

`cella gateway <name> <verb>` is the surface, its own thin CLI. It
acts on files and signals alone: it writes the valve record, reads
the ledger, and kicks the running VMM. It holds no capability, and
neither does any other network binary.

- `show [incoming|outgoing]` -- the held operations, one line
  each: identifier, destination or source, age, state.
- `release <id>` / `refuse <id>` -- decide one operation by id
  prefix.
- `open` / `close` -- the valve.
- `inspect <id>` -- render a frozen hold's plaintext; the look is
  witnessed in the chronicle. Judgment requires sight, and sight
  requires stillness.

**One language.** `proto/cella.proto` defines the vocabulary:
Event (an operation parked, released, or lapsed), Operation (id,
destination, both clocks), Decision, Valve, and `service Engine {
rpc Decide(stream Event) returns (stream Decision) }`. The verbs
speak it today; an engine that arrives later speaks the same
schema, and nothing gets rewritten. gRPC never enters a VMM.

### N5 -- the control and lifecycle planes

Files and signals only. Neither binary holds a capability.

```mermaid
graph LR
    X1["N.X.1 cella-gateway: show, release, refuse, open, close, inspect"]
    L1["N.L.1 cella-machine: create, start, freeze, thaw, destroy"]
    M1p["N.M.1 virtio-net (see N1)"]
    T1p["N.T.1 the edge loop (see N2)"]
    F14p["N.F.1, N.F.2, N.F.3 (see N4)"]
    X1 --- F14p
    X1 -->|"the kick: SIGWINCH"| M1p
    L1 -->|"start spawns; destroy kills"| T1p
    L1 -->|"spawns jailed; hands it the edge fds"| M1p
```

## The invariant

No cella process holds a capability, and no host network object of
cella's exists -- no tap, no bridge, no NAT rule, no daemon. Every
frame in both directions parks and is decided at the membrane.
Every machine is born closed. No allow outlives its decision --
a release moves one operation and installs nothing -- and no
interface carries an unmanaged mode.

## The accepted costs

- Every crossing is judged: the datapath costs a decision per
  operation, and multi-cycle exchanges ride the peer's patience.
- One translator process per machine, for the machine's life.
- A forwarding topology (a gateway guest between wires) costs a
  judged crossing at every membrane it traverses, deliberately.

## Roadmap

Cella ends as the enforcement primitive: every crossing named,
held, decided by someone else, witnessed. The items below build on
that primitive, in order, at the proto seam -- each one belongs to
the world engine, not to cella.

1. The engine: `service Engine` receives the Event stream and
   answers with Decisions -- the judge becomes a program.
2. The appliance pair: a gateway machine between a member and the
   world, judging as a resident.
3. TCP termination at the appliance: the member's peer is always
   its appliance, and world-side flows survive any freeze length.
4. TLS against a pair CA: the appliance terminates, the member
   trusts its own CA from birth.
5. DNS as a service: resolution becomes a parkable operation on a
   name.
6. Ownership of peer patience: the engine manages what the
   world's timeouts will bear.
7. The timeline rewrite: application-layer timestamps translated
   into the machine's frame at the one boundary.
