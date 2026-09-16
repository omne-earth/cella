# The terminator

The design record for the one network appliance: the machine that
terminates TLS at the pair border, resolves and caches names for
its members, and splices what it does not terminate. The law is
docs/NETWORK-MODEL.md ("The terminator"); the integrator's
contract is docs/integration/TLS-TERMINATOR.md; the board entry
is tasks/PHASE2-security.md, 2.7. This document states the
mechanism and shows each gate's walk.

Status: shipped (2026-09-15). The proxy (crates/cella-terminator),
the image (`cella build rootfs terminator`), and the pair CA
export are built. Eight gates run green (`make
smoke-tls-terminator`, TESTING.md rosters them).

The identifiers: T.R is the resolver, T.C the pair CA, T.P the
proxy lanes, T.V the probe voice, T.B the build door -- minted
here (T for this document, per the first-letter rule). All other
identifiers are borrowed: N.* from docs/NETWORK-MODEL.md, W.*
from docs/WORLD-ENGINE.md.

## T1 -- the appliance, in one map

An ordinary cella machine in the pair seat: eth1 faces the member
on a wire, eth0 faces the world. Both nics stand behind the
machine's own membrane (N.M.1) -- no exemption exists, and the
judge's standing memory (N.F.7) is what keeps the hot paths live.

```mermaid
graph LR
    MEM["the member (a cella machine)"]
    NM1["N.M.1 the member's membrane"]
    W["N.H.4 the wire"]
    NM2["N.M.1 the appliance's membrane"]
    R1["T.R.1 the resolver: answers every name with the wire address"]
    P1["T.P.1 the peek: first bytes name the lane"]
    P2["T.P.2 terminate: member-leg TLS from a minted leaf"]
    P3["T.P.3 splice: a plain byte pump, half-close honest"]
    P4["T.P.4 map: static per-port routes for nameless flows"]
    C1["T.C.1 the minter: one leaf per SNI, cached"]
    C2["T.C.2 the pair CA, baked: /etc/cella/pair-ca.pem + .key"]
    R2["T.R.2 the cache: TTL-honest, ceiling-clamped"]
    WLD(("the world"))
    MEM --- NM1
    NM1 --- W
    W --- NM2
    NM2 --- R1
    NM2 --- P1
    P1 --> P2
    P1 --> P3
    P1 --> P4
    P2 --- C1
    C1 --- C2
    R1 --- R2
    R2 -->|"upstream UDP, judged egress on eth0"| WLD
    P2 -->|"world-leg TLS, webpki roots"| WLD
    P3 --- WLD
    P4 --- WLD
```

The lanes are exclusive and the peek never guesses: a TLS
ClientHello terminates (T.P.2), an HTTP head replays into a
splice by its Host header (T.P.3), a configured port maps
(T.P.4), and a nameless bare-TCP flow on an unmapped port is an
error, never a route.

## The two legs

The member's peer is always its terminator, so the member leg's
patience is the pair's own: a member may freeze mid-handshake for
as long as judgment takes. The world leg is the terminator's own
connection at wire speed. Every frame of both legs still parks at
the appliance's membrane; the standing grants
(docs/integration/MEMBRANE-MEMORY.md) are what make the parks
live instead of frozen.

```mermaid
sequenceDiagram
    participant M as the member
    participant A as the appliance
    participant W as the world
    Note over M,A: the member leg -- pair patience,<br/>freezes allowed, pair-CA trust
    M->>A: TCP + TLS against the minted leaf
    Note over A,W: the world leg -- wire speed,<br/>webpki trust, the appliance's own
    A->>W: TCP + TLS against the world's certificate
    W-->>A: the world's bytes
    A-->>M: re-encrypted under the pair's leaf
```

## The gates, one walk each

The eight criteria live in scripts/test/tls-terminator.sh (t1-t6,
a live pair on KVM) and scripts/test/tls-terminator-strict.sh
plus scripts/test/tls-terminator-python-strict.sh (t7-t8, the
minter's bytes on a loopback wire, no VMs). TESTING.md rosters
the family; `make smoke-tls-terminator` runs it.

### t1 -- the resolver is the interceptor

Every query gets the same answer: the appliance's own wire
address. Interception is an answer, not a rule; the kernel stays
quiet.

```mermaid
sequenceDiagram
    participant M as the member
    participant R as T.R.1 the resolver
    M->>R: A? w.test (resolv.conf points at the wire)
    R-->>M: A w.test = 10.77.0.1 (itself, TTL 30)
    Note over M: every name leads to the appliance;<br/>the real name rides in the flow itself
```

### t2 -- termination verified against the baked pair CA

The member's probe (T.V.1) dials the answer, offers the SNI, and
verifies the minted leaf against the pair CA it baked at build.
The world is dead by design: exit 3 says "verified, world dead"
-- the local proof that interception, minting, and SNI all hold.

```mermaid
sequenceDiagram
    participant V as T.V.1 the probe
    participant R as T.R.1 the resolver
    participant P as T.P.2 terminate
    participant C as T.C.1 the minter
    V->>R: A? w.test
    R-->>V: 10.77.0.1
    V->>P: ClientHello, SNI w.test
    P->>C: a leaf for w.test
    C-->>P: minted once, cached (chain: leaf + T.C.2 cert)
    P-->>V: ServerHello + the chain
    Note over V: verified against /etc/cella/pair-ca.pem
    V->>P: GET /
    Note over P: the world leg dials and dies
    P-->>V: nothing -- exit 3: verified, world dead
```

### t3 -- the nameless splice, and the durable ratchet

A configured map (T.P.4) routes a nameless port through a
resolved name to the world. The gate also proves two membrane
facts on the way: the world-leg park carries the resolved name
(the name ratchet, proto/cella.proto `Destination.host`), and the
name survives a freeze -- the names file (`network/names`) folds
at the thaw, so post-thaw parks of the same flow stay stamped.

```mermaid
sequenceDiagram
    participant M as the member
    participant P as T.P.4 map
    participant R as T.R.2 the cache
    participant N as N.M.1 the appliance's membrane
    participant W as the world's page
    M->>P: TCP :8080 (nameless -- the map names it w.test)
    P->>R: resolve w.test
    R-->>P: the world's address (upstream asked, then cached)
    Note over N: the DNS answer crossed as judged ingress:<br/>the ratchet learns w.test = ip,<br/>appended to network/names
    P->>W: dial, splice
    Note over N: the world-leg park carries host=w.test
    W-->>M: "the-world-answers"
    Note over N: freeze, thaw -- the names file folds,<br/>the next fetch's parks stay stamped
```

### t4 -- TTL-honest caching

The cache answers what it holds while the TTL stands. The second
flow asks the upstream nothing; the gate counts the upstream's
questions and requires zero growth.

```mermaid
sequenceDiagram
    participant M as the member
    participant R as T.R.2 the cache
    participant U as the upstream provider
    M->>R: first flow needs w.test
    R->>U: A? w.test (question 1)
    U-->>R: the address, TTL
    R-->>M: served
    M->>R: second flow needs w.test
    Note over R: the TTL stands -- no question leaves
    R-->>M: served from the cache
```

### t5 -- the unauthorized middle is refused

A probe trusting a different anchor must refuse the handshake:
no pair trust, no middle. This is the member's protection stated
as a gate -- the middle works only for members that consented at
image build.

```mermaid
sequenceDiagram
    participant V as a probe with a foreign anchor
    participant P as T.P.2 terminate
    V->>P: ClientHello, SNI w.test
    P-->>V: the pair's minted chain
    Note over V: the chain meets a trust store<br/>that never held T.C.2
    V--xP: handshake refused -- exit 1
```

### t6 -- the named world (https://example.com)

The whole seam against the real internet: real DNS upstream, a
real world certificate verified against the pinned webpki roots,
and the pair's minted leaf on the member leg. The gate SKIPs
without internet. The world-leg destination is unknowable at
policy time, so the judge's remember rule plants each released
443 park's own memory (the motor's `--remember "*:443:600"`).

```mermaid
sequenceDiagram
    participant V as T.V.1 the probe
    participant R as T.R.1/T.R.2 resolver + cache
    participant P as T.P.2 terminate
    participant W as example.com
    V->>R: A? example.com
    R-->>V: 10.77.0.1 (the interceptor's answer)
    R->>W: upstream A? example.com (9.9.9.9, judged UDP)
    V->>P: ClientHello, SNI example.com
    P-->>V: minted leaf -- verified against the pair CA
    P->>W: TLS, SNI example.com
    Note over P: the world's chain verifies<br/>against the pinned webpki roots
    W-->>P: 200, the page
    P-->>V: re-encrypted -- exit 0: the world answered
```

### t7 -- the strict verifier (openssl)

rustls tolerates a bare leaf; a member's stricter stack is
entitled not to (RFC 5280). The gate stands the proxy on
loopback as an ordinary user, fetches the served chain with
`openssl s_client`, and holds it to `openssl verify
-x509_strict` plus the extensions by name. No VMs: the subject
is the minter's bytes on a real wire.

```mermaid
sequenceDiagram
    participant G as the gate
    participant B as T.B.1 --mint-pair-ca
    participant P as T.P.2 on loopback
    participant O as openssl, the strict judge
    G->>B: mint a pair into a scratch dir
    G->>P: run the proxy (dns_port unprivileged)
    G->>O: s_client -servername strict.test -showcerts
    O->>P: ClientHello
    P-->>O: the served chain
    O->>O: verify -x509_strict against ca.pem
    Note over O: OK -- and the leaf carries AKI,<br/>digitalSignature, serverAuth, SAN=SNI
```

### t8 -- the field verifier (python, VERIFY_X509_STRICT)

The stack that tripped in the field: Python's ssl module under
VERIFY_X509_STRICT (default-on since 3.13) raised
"missing Authority Key Identifier" against a bare leaf while
curl forgave it. The gate replays that exact stack, hostname
verification included.

```mermaid
sequenceDiagram
    participant G as the gate
    participant P as T.P.2 on loopback
    participant Y as python ssl, VERIFY_X509_STRICT
    G->>Y: connect 127.0.0.1, server_hostname strict.test
    Y->>P: ClientHello
    P-->>Y: the served chain
    Y->>Y: strict verification + hostname check
    Y-->>G: strict-ok TLSv1.3
```

## The minted leaf, exactly

The member's verifier is not the pair's to choose, so the leaf
serves the strictest honest one (t7 and t8 hold it there):

| Field | Value | Why |
|---|---|---|
| Subject / SAN | the SNI, verbatim | hostname verification's anchor |
| Validity | 1975-01-01 to 2200-01-01 | the frozen-member corollary (2.7 (c)): a member validates against its own past clock |
| AKI | the pair CA's SKI | RFC 5280 chain building under strict semantics |
| KeyUsage | digitalSignature only | exactly what a TLS server leaf is for |
| EKU | serverAuth only | nothing implicit, nothing more |
| Issuer | T.C.2, the baked pair CA | one pair, one CA, one blast radius |

The CA itself is a constrained root: CA:TRUE with pathlen 0,
KeyCertSign + CrlSign, its own SKI, self-signed (a root's missing
AKI is exempt under strict). Rebuilding the image mints a new
pair: the old members' trust dies with the old CA, deliberately
(docs/integration/TLS-TERMINATOR.md, "one pair, one CA").

## What the terminator is not

- **Not a router.** No forwarding exists; the resolver's answer
  is the only redirection, and a nameless flow without a map is
  an error.
- **Not exempt.** Every frame of both legs parks at the
  appliance's membrane; a terminator freeze kills a mid-flight
  world session honestly, and what slips through is an upstream
  retry (the honest freeze, stated in
  docs/integration/TLS-TERMINATOR.md).
- **Not a time machine, yet.** Timestamps pass through untouched;
  the timeline rewrite is board item 2.8, held.
- **Not host surface.** The crate is guest userland only (2.7
  (g)): no witness door, no install, no shim row. A fully
  compromised proxy still stands inside a jailed, judged cella
  machine.
