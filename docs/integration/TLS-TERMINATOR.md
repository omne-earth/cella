# Integrating the terminator: TLS across cryogenic time

How an integrator puts the one network appliance between its
members and the world. The law is docs/NETWORK-MODEL.md ("The
terminator"); the mechanism and the gates' walks are
docs/TLS-TERMINATOR.md; this is the builder's walk. Nothing here is
specific to any one harness.

## Why it exists

A TLS handshake is round trips the world expects within seconds;
a judged crossing can freeze a member for minutes. The terminator
splits every TCP connection in two: the member's peer becomes the
terminator (whose patience is configured, not negotiated -- the
member may freeze mid-handshake indefinitely), and the world leg
is the terminator's own connection, made at wire speed. Plain TCP
is spliced without termination; TLS is terminated on a leaf
minted at runtime from the pair CA.

## Say it loudly: the middle is consented

**The terminator reads member plaintext. This is the
architecture, not an attack.** A member trusts the pair CA
because its builder baked that CA into its trust store at image
build -- a deliberate, recorded act. No trust is injected at run
time; nothing is intercepted that the image's builder did not
choose to route through the appliance. If a workload must not be
read by its own appliance, do not bake the CA and do not route
it through a terminator.

One pair, one CA, one blast radius: never bake one pair's ca.pem
into members of another pair. The CA key is pair-scoped by
design -- stolen, it can mint certs trusted only by members that
already routed their traffic through the box the thief had to
compromise -- and cross-baking is the only way to widen that
radius. Build one terminator golden per trust domain.

## How interception works (so nothing feels hidden)

The resolver is the interceptor: it answers every member query
with the terminator's own wire address. The member connects to
the terminator believing it is the world; the real name rides in
the SNI (TLS, any port) or the Host header (plain HTTP); the
proxy resolves the real address upstream at connect time and
opens the world leg. No proxy environment variables, no kernel
redirect rules, no member changes beyond the two baked lines
below. A bare TCP flow that carries no name needs a static
per-port map in the terminator's configuration -- name it or it
does not route.

## The builder's steps

1. **Build the terminator golden once per host.** The build mints
   the pair CA: the key is baked into the image and never leaves
   it; the cert exports beside the golden.

   ```sh
   cella build rootfs terminator
   ls ~/.cella/rootfs/terminator/
   #   rootfs.ext4  golden.json  ca.pem
   ```

2. **Bake trust and names into every member image** (one step in
   an existing rootfs build -- docs/integration/ROOTFS.md):

   ```sh
   install -D -m 0444 ~/.cella/rootfs/terminator/ca.pem \
       rootfs-tree/etc/ssl/certs/cella-pair-ca.pem
   echo "nameserver <terminator-wire-address>" > rootfs-tree/etc/resolv.conf
   ```

3. **Stand the pair** (docs/EXAMPLES.md, E9):

   ```sh
   cella create term --rootfs terminator --net world,wire:pair
   cella create member --rootfs <task-image> --net wire:pair
   cella start term && cella start member
   cella gateway term open && cella gateway member open
   cella-engine term --dial <engine-addr> &
   cella-engine member --dial <engine-addr> &
   ```

4. **Feed the engine the terminator's border policy.** The
   appliance is just another machine: its hot paths stay live
   through the judge's standing memory
   (docs/integration/MEMBRANE-MEMORY.md), not through any
   exemption. The recommended grants for its border:

   ```sh
   release outgoing arp (keep_open=24h) (skip_freeze=true)
   release incoming arp (keep_open=24h)
   # the DNS provider: the names live at the appliance
   release outgoing 9.9.9.9:53/udp (keep_open=24h) (skip_freeze=true)
   release incoming 9.9.9.9:53/udp (keep_open=24h)
   # the world-leg 443 destinations the policy grants, exact
   release outgoing <dest-ip>:443/tcp (keep_open=1h) (skip_freeze=true)
   release incoming <dest-ip>:443/tcp (keep_open=1h)
   # the member's reply window: the terminator image pins its
   # clients to ports 50000-50007 (the consistent reply port,
   # docs/integration/MEMBRANE-MEMORY.md), so the appliance's
   # answers toward the member are eight exact destinations
   release outgoing <member-ip>:50000/tcp (keep_open=1h) (skip_freeze=true)
   # ... through 50007, and the same lines for /udp (DNS replies)
   ```

## The honest freeze

The terminator freezes like any machine. A world-leg session that
is mid-flight when it does dies at the world peer's patience --
honestly, visibly, in the chronicle. Standing memory makes such
freezes rare on remembered paths; anything that slips through is
an upstream retry, never a broken promise. Do not design around
a terminator that never freezes: design around one that rarely
does.

## Backpressure, spoken

The appliance's world side is metered by its eight-port reply
window (docs/TLS-TERMINATOR.md, "The world window"). Two HTTP
answers can therefore originate at the terminator itself, and a
member's client stack should expect both:

- `429 Too Many Requests` with `Retry-After: 5`: the window is
  saturated and the crossing could not seat within the grace.
  Back off for the stated seconds; a client that honors
  Retry-After degrades gracefully, and a client that hot-loops
  reconnects is the failure mode the 429 exists to prevent
  (titanium trial ekdh4mm: an HTTP library retried a mute
  failure at ~160 connects/s).
- `502 Bad Gateway`: the world leg got no answer within 2 s --
  the name is refused by policy, or the far side is down; the
  terminator cannot tell which and says only what it knows.

The nameless map lanes (bare TCP) carry no protocol to speak: a
saturated or unreachable mapped crossing closes fast instead,
and the client's own stack sees an ordinary reset.

## HTTP/1.1, by design

The terminator negotiates no ALPN on either leg, so clients fall
back to HTTP/1.1 (a verbose client prints "server did not agree
on a protocol"). This is a consequence of the trust model, not a
missing feature. The member's handshake completes from pair
trust alone -- minted leaf, baked CA, no reference to the
world's state (the t2 proof). Offering h2 honestly would require
either ALPN mirroring (connect the world first and echo its
agreed protocol -- the untrusted side then shapes the pair-side
handshake, and the sealed boundary gains an outside input) or
protocol translation (the splice stops being a byte-honest pump
and becomes a rewriter). Both trade away a property the pair
currently proves.

The practical consequence: no multiplexing. Every concurrent
request is its own TCP connection through the eight-permit world
window, so a parallel fetcher (uv, npm) meets the 429 gate
sooner than an h2 proxy would; those tools honor Retry-After and
degrade to pacing. A task that genuinely needs more concurrency
faces a constant of the design, not a knob: the appliance's
world width is eight (WORLD_PERMITS in
crates/cella-terminator/src/gate.rs, matched by the baked port
range in rootfs-terminator.sh), and it does not widen -- not by
policy, not by configuration. Eight enumerable reply
destinations is the point of the consistent reply port; a width
that can grow is a doctrine that can leak, and the narrowness is
also containment (eight is the standing-concurrency bound of a
compromised appliance). A workload that needs more standing
world flows scales out -- a second terminator on a second wire,
separately judged, its own enumerable eight -- never wider.
Distinguish this from the MEMBER's reply window, which is
runtime and policy: a member that widens its range under wider
grants presents more concurrent crossings, as the t11 gate does
to saturate the eight.

## What to verify, t1-t11

The reference assertions are the eleven gates (`make
smoke-tls-terminator`; docs/TLS-TERMINATOR.md shows each walk as
a diagram). An integration test mirrors them one for one, with
the harness's own tools. `<gw>` is the terminator's wire address;
`<pem>` is the baked pair cert.

1. **The interceptor (t1).** From a member, resolve any name;
   the answer is always `<gw>`.

   ```sh
   nslookup anything.example    # every answer: <gw>
   ```

2. **Termination verified (t2).** From a member, any TLS client
   that trusts only `<pem>` must verify the minted leaf. With the
   world leg dead, verification still succeeds -- the local proof
   that interception, SNI, and minting hold before the world is
   ever involved.

   ```sh
   openssl s_client -connect <gw>:443 -servername api.example \
       -CAfile <pem> -verify_return_error </dev/null
   ```

3. **The nameless splice (t3).** Configure a static map
   (`map=<port>:<name>:<world-port>`), fetch through it, and read
   the appliance's chronicle: the world-leg park carries
   `host=<name>` (the name ratchet), and still does after a
   freeze and thaw of the appliance.

   ```sh
   wget -q -O- http://<gw>:8080/
   cella --dump <machines>/term/network/ledger | grep host=
   ```

4. **The cache (t4).** Fetch twice; count questions at the
   upstream between the fetches. The second flow asks nothing
   while the TTL stands.

5. **The unauthorized middle refused (t5).** The same client with
   a different trust store (or none) must refuse the handshake.
   If this passes, stop: a member that never baked the pair's
   cert is verifying the middle, and the consent story is broken.

6. **The named world (t6).** A real name end to end: the member
   fetches `https://example.com` through the pair; the content
   arrives, the member-leg chain is the pair's, and the appliance
   verified the world's chain against its pinned roots.

7. **The strict verifier (t7).** Hold the served chain to strict
   RFC 5280 semantics with a verifier you did not build:

   ```sh
   openssl s_client -connect <gw>:443 -servername t.example \
       -showcerts </dev/null | awk '/BEGIN CERT/{n++} n==1' > leaf.pem
   openssl verify -x509_strict -CAfile <pem> leaf.pem   # must say OK
   ```

8. **The field verifier (t8).** The strictest common client
   stack: Python 3.13+ (`VERIFY_X509_STRICT` is its default)
   with hostname checking, against `<gw>:443`. This is the stack
   that first tripped a bare leaf in the field; a green t8 means
   an agent's ordinary `ssl` client verifies the mint.

   ```python
   import socket, ssl
   ctx = ssl.create_default_context(cafile="<pem>")
   ctx.verify_flags |= ssl.VERIFY_X509_STRICT
   with socket.create_connection(("<gw>", 443)) as s:
       with ctx.wrap_socket(s, server_hostname="api.example") as t:
           print(t.version())
   ```

9. **Throughput (t9).** A bulk transfer through the pair arrives
   byte-exact at real speed -- fetch a file of tens of MiB and
   compare checksums and wall time. Every crossing is still
   individually judged; if bulk crawls at tens of kB/s, the
   verdict path is polling somewhere (the bridge's ear,
   docs/WORLD-ENGINE.md, or the engine's own decision latency --
   after cella's ear, the engine's per-verdict cost is the
   ceiling, so know yours).

Also verify the freeze story once: freeze the member
mid-handshake, thaw it, and the session completes -- the member
leg's patience is the pair's own (docs/TLS-TERMINATOR.md, "The
two legs").


9. **Throughput (t9).** A bulk transfer through the pair arrives
   byte-exact at wire-adjacent pace; a stall points at the
   verdict path, not the splice.
10. **The retry storm (t10).** Paced sequential requests against
   a keep-alive upstream all answer; a lockout at eight is the
   reply window's TIME_WAIT drain resurfacing.
11. **The spoken window (t11).** Saturate the world side (more
   concurrent crossings than eight); the excess hears 429 within
   the grace, and a retry after the drain answers 200.