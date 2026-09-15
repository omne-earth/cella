# Integrating a rule engine and the membrane's memory

How an integrator's judge takes the seat: implement one gRPC
service, answer parks, and plant standing memory. The law is
docs/NETWORK-MODEL.md ("The membrane's memory", N.F.7) and
docs/WORLD-ENGINE.md; this document is the integrator's walk.
Nothing here is specific to any one harness.

## The seam contract

1. **Implement `service Engine`** (proto/cella.proto): one RPC,
   `Decide(stream Event) returns (stream Decision)`. The engine
   receives every park as an Event and answers by operation id.
   Accord version 4 is the current vocabulary; an older end
   refuses the handshake rather than dropping what it cannot
   carry. The worked example is `cella-engine motor`
   (crates/cella-engine/src/motor.rs) -- the smallest complete
   rule engine, written to be read, never run in production.

2. **Answer verdicts**: `Decision { id, Release }` or
   `Decision { id, Refusal { why } }`, one per park. The why is
   the policy author's sentence; it lands verbatim in the
   machine's chronicle (`Lapsed`).

3. **Plant memory with the third answer**: `Decision { id: empty,
   MembraneMemory { destination, skip_freeze: true, keep_open } }`
   -- id empty because a memory names a destination, not an
   operation. Use the park's own Destination for exactness:
   matching at the membrane is exact, (ip, port, proto) or the
   ethertype, no wildcards. `keep_open` is plain seconds. Leave
   `written` at zero: the bridge stamps it with the host clock at
   the landing, and expiry is `written + keep_open`, absolute --
   a freeze does not stretch the window, and a zero is inert.

4. **The harness spawns only the bridge**:
   `cella-engine <machine> --dial <engine-addr>`, one per machine,
   machine-lifetime. The bridge tails the ledger, streams Events,
   and lands every Decision: verdicts into the verdict file,
   memories into `machines/<vm>/membrane-memory` (append-only
   forever -- every byte is a ruling the engine chose to make),
   each landing witnessed in the machine's audit book
   (`verb=membrane-memory`, the window in the args), each followed
   by the kick.

5. **The run's shape**: the first crossing to any destination
   parks and freezes -- the engine meets it through the stream,
   and its release applies at the thaw. A crossing to a remembered
   destination parks and waits live: the machine keeps running,
   the guest's own timers tick (this is what keeps a TLS
   handshake's flights inside the peer's patience), and the
   decision applies on the kick. A remembered refusal is an
   instant error with the why on the record -- no freeze-thaw
   churn per denied probe. When `keep_open` lapses, the memory
   clears by its own arithmetic and the cryogenic default resumes.

   A policy that probes negatives MUST use the standing refusal
   (`skip_freeze=true` on the refuse line), or every TCP
   retransmit of the denied SYN pays a fresh freeze-thaw cycle
   and the probe's wall clock balloons. cella will never answer
   a refusal with a minted RST or ICMP error -- the membrane
   does not speak frames the world never sent, and a refused
   destination honestly looks filtered. The refused guest's own
   connect timeout is the probe's cost; size the client's
   patience (a single-SYN probe with a short timeout is the
   honest shape), never the membrane.

6. **What to verify**: the membrane-memory file exists after the
   first remembered grant; `cella --dump
   machines/<vm>/membrane-memory` shows the entries themselves,
   and `cella --dump machines/<vm>/audit` shows the
   `membrane-memory` landings; the
   chronicle shows every crossing, remembered or not. The
   reference assertions are the six gates,
   `scripts/test/membrane-memory.sh mm1..mm6`
   (`make smoke-membrane-memory`) -- an integration test can
   mirror them one for one.

## The recommended policy grammar

cella reads no policy file: the wire is the only contract, and
the engine's policy source is the engine's own business. But an
engine needs a file, and this grammar is the recommended shape --
designed so the file's words are the wire's words and the books'
words, with no translation table between them:

| Policy word | Wire / chronicle word |
|---|---|
| `release` / `refuse` | `Decision::Release` / `Decision::Refusal` |
| `incoming` / `outgoing` | `Operation.direction` |
| `ip:port/proto`, `arp`, `ipv6`, `0xNNNN` | the park's `Destination`, matched exactly |
| `(keep_open=5m)` | `MembraneMemory.keep_open` (sugar compiles to plain seconds) |
| `(skip_freeze=true)` | `MembraneMemory.skip_freeze` -- plants the memory |
| `(reason="...")` | `Refusal.why` -> `Lapsed.why`, verbatim in the chronicle |

One grant per line:

    <release|refuse> <incoming|outgoing> <destination> (key=value)*

The discipline that keeps it honest: parse strictly (an unreadable
line is an error naming its number, never a skipped rule);
everything not granted is refused (default-refuse is the ground
state); destinations are exact (a matcher that never guesses
cannot hold a grant it cannot enumerate -- an engine may judge
broader patterns live, but it cannot plant memory for them);
`keep_open` is mandatory (eternal is not expressible);
`skip_freeze` is outgoing only (an incoming park never freezes);
`reason` is refuse only.

## An exhaustive example

Every shape the grammar holds, one file:

```sh
# The L2 plumbing: ARP must flow or nothing else does. Incoming
# never freezes, thus its lane needs only the window.
release outgoing arp (keep_open=24h) (skip_freeze=true)
release incoming arp (keep_open=24h)

# A raw ethertype (here LLDP), released briefly: the seconds sugar.
release outgoing 0x88cc (keep_open=30s)

# The agent's API endpoint: judged live so the TLS handshake's
# flights stay inside the peer's patience. Reply twin carries the
# window only.
release outgoing 160.79.104.10:443/tcp (keep_open=5m) (skip_freeze=true)
release incoming 160.79.104.10:443/tcp (keep_open=5m)

# DNS over UDP, live, on a short leash.
release outgoing 9.9.9.9:53/udp (keep_open=90s) (skip_freeze=true)
release incoming 9.9.9.9:53/udp (keep_open=90s)

# ICMP: a bare IP protocol number (1), port 0 -- the echo's shape.
release outgoing 192.0.2.1:0/1 (keep_open=10m) (skip_freeze=true)
release incoming 192.0.2.1:0/1 (keep_open=10m)

# A slow internal service where cryogenic waiting is the point:
# skip_freeze=false written out, pinning the choice on the line.
release outgoing 10.77.3.1:8080/tcp (keep_open=60m) (skip_freeze=false)

# A destination this workload must never reach -- and without
# churn: the standing refusal answers instantly, the reason lands
# in every Lapsed record, and each attempt still chronicles.
refuse outgoing 169.254.169.254:80/tcp (keep_open=24h) (skip_freeze=true) (reason="metadata is never granted")

# A refusal without skip_freeze: legal, and deliberately costly --
# every denied probe pays a freeze-thaw cycle. Pick it when the
# denial should hurt.
refuse outgoing 198.51.100.9:9/udp (keep_open=1h) (reason="the discard port teaches nothing")
```

What is deliberately absent: wildcards in any position, a `deny`
synonym (refuse is the word), a MAC in any destination (a policy
that named one would break on every machine rebuild), an eternal
window, and `skip_freeze` on an incoming line -- a strict parser
rejects each of these rather than guessing.

## The consistent reply port

A destination the policy cannot enumerate cannot hold a grant,
and the classic unnameable destination is the ephemeral reply
port: a service's answer to a client goes to whatever port the
client's kernel picked, so the service side's egress toward that
port would freeze on every flow. The remedy is ruled
(2026-09-15) as a guest-side contract, not a membrane mechanism:
the client machine pins its ephemeral range to a narrow, agreed
window --

```sh
echo "50000 50007" > /proc/sys/net/ipv4/ip_local_port_range
```

-- and the judge grants the window as exact destinations, one
line per port, on the serving machine's membrane:

```sh
release outgoing 10.77.0.2:50000/tcp (keep_open=10m) (skip_freeze=true)
# ... through 50007, and the same lines for /udp if UDP serves
```

The trust direction is the point. A destination is routing, not
assertion: stamping a granted destination on a frame sends the
frame there, so the match cannot be forged for benefit -- unlike
matching on a frame's source, which the sender authors freely. A
client that ignores the window (hostile or misconfigured) sends
from an ungrantable port and the serving side's reply simply
freezes: non-compliance costs liveness and nothing else,
fail-closed. The window's width is the concurrency budget --
TCP demuxes on the whole 4-tuple, so eight ports is eight live
flows per remote service -- and sizing it is policy, stated in
the client's image (the terminator image bakes exactly this
window; docs/integration/TLS-TERMINATOR.md).
