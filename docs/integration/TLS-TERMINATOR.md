# Integrating the terminator: TLS across cryogenic time

How an integrator puts the one network appliance between its
members and the world. The law is docs/NETWORK-MODEL.md ("The
terminator"); this is the builder's walk. Nothing here is
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
   cella create term --rootfs terminator --net wire:pair,world
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
   ```

## The honest freeze

The terminator freezes like any machine. A world-leg session that
is mid-flight when it does dies at the world peer's patience --
honestly, visibly, in the chronicle. Standing memory makes such
freezes rare on remembered paths; anything that slips through is
an upstream retry, never a broken promise. Do not design around
a terminator that never freezes: design around one that rarely
does.

## What to verify

The member's handshake completes across a member freeze (freeze
the member mid-handshake, thaw, the session lives). Plain TCP
splices. A name resolves from the member with no world DNS
crossing on the second lookup (the cache). The reference
assertions are the gates, scripts/test/tls-terminator.sh
(`make smoke-tls-terminator`).
