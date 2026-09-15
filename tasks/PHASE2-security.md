# Cella and its thin CLIs get confined, we confine the confinement

The security phase (branch: feat/security). Phase 1.6 built the
walls and proved them inert; this phase turns them on and proves
them under enforcement. The unfinished Phase 1 tasks move here
verbatim (their rulings and numbering kept); the rulings' context
lives in tasks/PHASE1-core.md.

## Now (feat/security)

- [ ] 1.6.14f The broker shim (ruled 2026-09-02): the one
      door to privilege. The shim stays unjailed and stops
      exec'ing: it forks, the persona child runs jailed,
      and a socketpair carries a fixed per-persona request
      set that dies with the verb -- no daemon, no standing
      socket, no standing allow. Requests name objects,
      never authorities: gateway asks "kick <vm>" and the
      shim reads the pid file itself; machine asks "map",
      and the shim maps its own direct descendant; build
      asks "build <axis> <flavor>" and the shim runs the
      fixed toolbox command under a pinned, cella-owned
      XDG session. The broker table is compile-time, the
      same shape as persona_for(), and a persona invoked
      directly (without the shim) loses its
      namespace-crossing acts by construction -- the shim
      is the one door, statically gateable like
      write_egress. End state: 8 of 9 bwrap-jailed
      (cella-network joins after 1.6.14e), the shim the
      lone unjailed door, all nine under seccomp and
      SELinux. Runs after e (the broker's table is written
      against the final privilege reality).
- [ ] 1.6.14g The wiring today is exec-only: the shim routes and
      execs, no persona calls confine_self, and only the VMM
      runs inside bwrap (spawned jailed by machine). The broker
      turns the profiles from shipped text into walls.

- [ ] 1.6.14i The identity slice, finished (ruled 2026-09-03,
      from the escape table: "if the agent escaped the VM, what
      uid does it get"). Per-machine sub-uids landed in lane a;
      what remains is everything that still lands as the
      operator, and the map's inside face:
      - [ ] The translator runs as the machine's sub-uid, never
            as uid 1000. It is the one process that parses
            attacker-influenced bytes for the machine's whole
            life (every released frame, every wire and world
            reply); today ensure_translator spawns it as the
            invoking user. A jail is a view, not an identity:
            this item is the uid, with or before the jail.
      - [ ] The VMM's uid inside its namespace is not 0. The
            spawn maps 0 -> sub-uid today (newuidmap child 0
            target 1): namespace-root, a full in-namespace
            capability set, against the no-uid0-in-a-jail
            ruling. Map to an unprivileged inside uid.
      - [ ] Supplementary groups drop at the spawn. The map
            covers uid and gid; the parent's groups (kvm
            included) must not survive into the jailed process.
            One assertion in the identity gate.
      - [ ] Persona sub-uids (ruled in the parallel session: no
            uid outside is 1000): doctor, probe, and then every
            persona runs as a fixed sub-uid from the top of the
            delegated range, with per-purpose ACLs as its whole
            authority, readable as data. On escape a persona
            lands with its ACL slice, nothing of the operator's.
- [ ] 1.6.14h The join (re-sequenced 2026-09-02): the join runs AFTER e
      and f -- the single measurement-and-enforcement pass
      against the final architecture, so no wall is measured
      twice. Its mechanism is make install (the one install):
      semodule loads the ten CIL modules, semanage fcontext
      rules label the installed binaries, the checkout's
      target/smoke paths (the lab confines like the field), and
      the ~/.cella tree; restorecon applies them; the profiles
      copy to their installed home; the boot unit returns as a
      system unit in cella_network_t (the init transition rule
      lands with it). Its proof is the full battery green under
      ENFORCING on both machines with every verb transitioned
      into its domain. The deal-breakers close here, once,
      against reality: start/thaw and probe confine-after-fork
      (3), strace-derived lists for the tool-spawning verbs (4),
      make golden under strace against build's list (5), the
      labels (6), spawn's MCS labeling of machine dirs --
      refusing to start when labeling fails under enforcement
      (7), the boot unit's real domain (8) -- plus the
      neverallow assertions and the per-persona checklist, one
      line per persona, ticked as each proves out. Until the
      join, b's merged lists are provisional and the domains are
      inert: the smokes stay green and meaningful for the
      membrane's mechanics, and the join adds the in-domain
      battery as its own, separate certification.

- [ ] 1.6.7 The documents state the tightened law, and the
      retired phases make "Not in scope" the permanent scope
      statement. One pass, itemized:
- [ ] 1.6.7a The law, stated: no frame leaves undecided at any
      layer, no allow outlives its decision (there is
      nothing for it to outlive), the ARP sentence dies,
      and every failure of the apply is stillness.
- [ ] 1.6.7b The scope, stated: cella judges every frame, both
      directions, at its own seam -- named, held, decided
      externally, witnessed.
- [ ] 1.6.7c Permanently outside cella, named at the proto seam
      where a judge builds them: the appliance pair, TCP
      termination, TLS against a pair CA, DNS-in-frame and
      every world-side service, ownership of peer patience,
      and the timeline rewrite. NOT outside: ingress
      judgment (the ear's customs, shipped) and UDP
      judgment (shipped; UDP death was terminator territory
      and retires with it).
- [ ] 1.6.7d The temporary-backend language dies: the never-guess
      rebind is the architecture, not scaffolding.
- [ ] 1.6.7e The residuals, named once, plainly: a resident can
      modulate its compute and I/O shadow on the host
      (host-local, listener-required, silenced by the
      freeze); frames that arrive during a freeze are lost
      at the tap (no process listens; the protocols above
      retransmit); the peer-patience bound on multi-cycle
      exchanges is a permanent boundary, the judge's to
      manage; the field machine is dark to the world, not
      to its host's logbook (vmm.log carries the park
      lines); the pool's neighbor pins assume the default
      guest MAC (a custom --mac on a pool tap breaks the
      convention, stated); the canonical kernel's quietness
      is chosen (ipv6.disable=1), never an exemption --
      chatter that exists still parks. Entries the shipped
      mechanisms retired (the open-ingress residual) drop.
- [ ] 1.6.7f The security boundary, post-shakedown (ruled
      2026-09-02): cella-network is the one persona with no
      bwrap jail -- a user namespace severs host-netns
      capabilities by kernel design, and non-setuid bwrap
      refuses ambient capabilities outright -- so it is
      confined by its seccomp allowlist and its SELinux
      domain instead, stated as data in its profile file
      and in LIFECYCLE's boundary table. The tap's
      ownership follows the machine: at start, the spawn
      calls the file-capability cella-network to re-own the
      tap to the machine's own sub-uid (TUNSETOWNER), so
      only that machine can attach it.
- [ ] 1.6.7g The identity slice, documented: per-machine sub-users
      from the delegated /etc/subuid range, the host
      prerequisites (subuid/subgid delegation, setfacl, a
      traversable path to CELLA_HOME) laid by the install
      scripts and checked/fixed by doctor.
- [ ] 1.6.7h NETWORK-MODEL's phases section retires; DEVICE-STATE's
      acceptance rows speak AC3's stand-in and AC5's
      real-world leg (1.6.6d).
- [ ] 1.6.7i The "aperture" wording in AC5 is replaced with plain
      speech, and the sweep hunts any siblings of it.
      The pass runs after every mechanism -- the border work,
      the fragment, the split, and the shakedown -- and
      immediately before the battery: the documents describe
      what ships, and the battery certifies what the documents
      describe, once each.

- [ ] 1.6.8 Full battery both machines on the finished phase --
      the regression close runs last, after the ear's customs
      (1.6.9 changes every network gate's choreography again),
      the inspect verb, and the witnessed border, so it certifies
      the law that ships, once.

- [ ] 2.1 smoke-rootless asserts the installed shim exists and
      matches the build.
- [x] 2.2 docs/EXAMPLES.md notes that nested layers must use
      distinct knock ports (2026-09-03, the knockable example).
- [ ] 2.7 The terminator (proposed 2026-09-15, branch
      feat/gateway-tls-terminator; P1 -- the TLS-EOF blocker):
      the one network appliance, an ordinary cella machine
      wearing the `terminator` rootfs flavor in the pair seat
      (member on a wire, world on the other nic). The rulings:
      (a) two legs -- the member's peer is always its terminator
      (that leg's patience is ours; a member freezes mid-
      handshake for as long as judgment takes), and the world
      leg is the terminator's own connection at wire speed;
      (b) terminate-and-splice on every TCP port -- a peeked TLS
      ClientHello terminates (SNI -> leaf minted at runtime from
      the pair CA -> world-leg TLS of the terminator's own),
      anything else byte-splices, which alone moves peer-patience
      off the member for all TCP; (c) the pair CA -- key baked
      into the terminator image at build, never exported; ca.pem
      exported beside the golden, digested in the manifest, and
      baked by the member's builder into its trust store: the
      middle is the architecture, consented at image build,
      stated loudly in docs/integration/TLS-TERMINATOR.md;
      (d) the names live at the appliance -- the terminator
      resolves and caches for its members (resolv.conf points at
      its wire address), upstream a configured provider ip over
      judged, remembered UDP (roadmap 5 lands in a guest);
      (e) just another machine -- no freeze exemption, no
      attentiveness contract: the judge's standing memory (2.6)
      keeps 443 and the provider live, a terminator freeze kills
      a mid-flight world session honestly, and what slips
      through is an upstream retry; (f) busybox, not systemd --
      one static proxy under the house init's respawn loop; a
      systemd variant, if ever, is an integrator's build, not
      cella's golden; (g) the cella-terminator crate is a new
      category, guest userland only -- no witness door (doors
      stay 7), no install, no shim row, no persona gate; it
      ships inside the image like busybox; (h) the proto gains
      nothing -- frames are frames, and 2.6's vocabulary
      suffices; (i) the shakedown surface, enumerated before it
      is entered (2026-09-15). The host surface grows by zero:
      no new binary, socket, door, or capability -- a fully
      compromised proxy still stands inside a jailed, judged
      cella machine. The guest-internal fire, itemized: (1) the
      proxy parses attacker bytes from both directions -- a
      hostile member's ClientHello and a hostile world's
      responses -- which is why it is memory-safe rustls, never
      a C daemon; (2) the CA key lives in the image but is
      pair-scoped: stealing it mints certs trusted only by
      members that baked this pair's cert -- traffic already
      routed through the very box the thief had to own; one
      pair, one CA, one blast radius, and no pair's CA is ever
      baked into an unrelated member; (3) the resolver-cache is
      a poisoning surface: upstream rides the translator's
      per-flow sockets (5-tuple bound), and cache discipline --
      TTL honesty, no glue trust -- is a gate assertion;
      (4) rustls and rcgen enter the tree: pinned in the
      lockfile, and the built artifact is digested in the golden
      manifest like every artifact, judged by doctor verify;
      (5) the plaintext concentration is the consented design,
      and the shakedown confirms the proxy never writes payload
      anywhere durable -- no payload logs, nothing on disk
      beyond the DNS cache; (j) interception is the resolver
      (ruled 2026-09-15): it answers every member query with the
      terminator's own wire address, the real name rides in the
      SNI or the Host header, and the proxy resolves upstream at
      connect time -- no netfilter (the canonical kernel stays
      quiet), no redirect, no proxy variables in members; a
      nameless bare-TCP flow routes only by a static per-port
      map, because a matcher that never guesses cannot route a
      nameless flow. webpki-roots joins rustls and rcgen in the
      pinned, manifest-digested supply chain: the world leg
      verifies its peers against compiled-in roots, never
      blindly. Gates: smoke-tls-terminator =
      tls-terminator-t1..tN (scripts/test/tls-terminator.sh),
      the split-with-aggregate pattern. Phases: A docs (this
      entry rides them), B the proxy crate with no-KVM units,
      C the image build and the CA export, D the gates.
- [ ] 2.6 The membrane's memory (proposed 2026-09-15, branch
      feat/membrane-memory): the judge leaves standing memory at
      the membrane -- one MembraneMemory entry per destination in
      the memory file (N.F.7), written by the gateway persona like
      the valve, read on the kick. skip_freeze parks the matching
      egress and keeps the machine running while the decision
      arrives (outgoing only: ingress never freezes); keep_open is
      the entry's window in plain seconds (the policy grammar
      carries unit sugar, titanium-side), anchored at the
      membrane's read. Rulings settled in design: (a) a memory
      affects freezing, never crossing -- release and refuse stay
      the judge's alone, every crossing still parks, ids, and
      chronicles; (b) every proto zero decodes to the cryogenic
      default, thus absent file = absent entry = zero field =
      today's behavior, and the existing battery is the
      backward-compatibility proof, unchanged; (c) fail-closed
      expiry, ruled absolute (2026-09-15): the bridge stamps
      written (epoch seconds) at the write, an entry stands while
      now < written + keep_open, any membrane at any read
      computes it, and a thaw re-anchors nothing -- an expired
      memory stays expired (mm6). The window burns in host time,
      deliberately: the memory is the judge's property and its
      risk window is real-world time. An abandoned memory cannot
      outlive its window, eternal is not expressible, and a zero
      written or keep_open is inert; (d) the VMM obeys and expires,
      never sets -- and the write surface is the engine seam
      alone (ruled 2026-09-15): the bridge lands what the engine
      grants, no CLI verb writes memory, and the transport is ruled
      (2026-09-15): a memory rides the Decide stream as a
      Decision -- the oneof gains membrane_memory = 4, its id
      empty (a memory names a destination, not an operation), the
      bridge writes the machine's membrane-memory file and kicks,
      and Accord version 4 announces the extension so an older
      end refuses the handshake rather than dropping entries
      silently; (e) no
      counter, no sidecar change -- the skip is a stateless
      per-park predicate (sidecar stays v9); (f) the honest trade,
      documented: a skipped freeze is ordinary waiting -- the
      guest sees the latency, and cryogenic scope shrinks by
      exactly the entries the judge grants. Gates:
      (g) gRPC-only, ruled (2026-09-15, superseding the same
      day's presider sketch): membrane memory has no CLI verb, no
      policy file in cella, and no persona -- the judge is a gRPC
      rule engine (titanium implements its own), its policy
      source is its own business, and a memory reaches the
      membrane one way: over the Decide stream, landed by the
      bridge (the written stamp, the kick, the witness). The
      production walk: the first park pays the freeze, the engine
      answers Release plus a membrane_memory, later parks to the
      remembered destination run live, and keep_open clears the
      memory by its own arithmetic. The motor is a worked
      example and the gates' stand-in, never a production
      component: it grows the ability to answer with a
      membrane_memory so the example (and the gates) exercise
      the whole seam. Gates:
      smoke-membrane-memory = membrane-memory-mm1..mm6
      (scripts/test/membrane-memory.sh: the live park, isolation,
      expiry, the live refusal with its reason, the engine-seam
      door witnessed, the fail-closed edges), chained into make smoke
      as its own family. Landed 2026-09-15: all six gates green
      (the live park, isolation, self-expiry, the live refusal,
      the witnessed door, the fail-closed edges), the existing
      battery untouched as the backward-compatibility proof.
- [ ] 2.5 cella extract (proposed 2026-09-09, for the titanium
      collection model): a fourth universe verb -- `cella extract
      <machine> <guest-path>` emits the evidence at that path from
      the still disk as a tar stream on stdout; `/` is the whole
      rootfs. No flags: a file is a shell redirection. The
      mechanism is inspect's appliance without the human: the
      `<machine>-extractor` boots the stock rootfs, the evidence
      mounts at /rock exactly as inspect mounts it, the guest init
      tars the named path to the raw scratch (offset 512), writes
      the trailer to sector 0 last (byte length + sha256 on
      success, the reason on failure), and halts; the host polls
      for the trailer (ruled by physics 2026-09-09: the canonical
      kernel has no power-off device, and a reset may boot again
      rather than end the VMM -- an exit is not a reliable
      signal), stops the appliance, verifies the digest, and
      streams -- a bad trailer is exit 1 with the reason, never a
      truncated tar. Still machines only (running is the one
      refusal, the family rule); frozen counts as still
      (norecovery already handles the dirty journal). Witnessed as
      a plain Audit event (verb=extract) -- no proto change
      required. No console anywhere, thus the verb works in the
      field flavor: this deliberately breaks inspect's accidental
      lab coupling rather than inheriting it. Settled by the
      implementation: (a) the trailer lives at sector 0, written
      last, with a failure variant; (b) the scratch is
      source-disk-sized plus slack, sparse; (d) the gate asserts
      numeric uid/gid against the golden's builder ids, and link
      fidelity (busybox installs hardlinks, and they ride as
      links). Open: (c) whether the tar digest also rides in the
      audit args, or earns a typed field later; (e) the trust
      boundary -- the tar is workload-authored bytes: the trailer
      proves the job completed, not that the tar is honest, and a
      consumer unpacks it with traversal protections (no absolute
      paths, no .., no symlink-following out of the target).
      Landed 2026-09-09 with scripts/test/extract.sh green; the
      titanium doc rides the same commit.
- [ ] 2.3 cella selftest picks a random knock port (the gates
      already do; the selftest still pins 1709).

## Later

- [ ] 2.4 The host image (post-1.6): the bare-metal host built the
      way the guests are built. Today the metal is the least
      audited resident in the tower -- a general-purpose distro
      under machines that are born closed. The image applies the
      guest discipline to layer zero: a host kernel from the
      canonical-fragment philosophy (necessity, not cost), a
      userland of nothing that was not chosen, the field flavor
      baked (no lab tools on the metal), the pool at boot, and --
      because nothing stands below the metal to park it -- the
      strongest law available there: egress enumerable. nftables
      default-drop with an explicit allow set, one line per
      permitted destination, and a counter for everything that
      tried. "Who knows what is reaching out" becomes "here is
      the complete list." Gate: a host battery -- the image
      boots, runs the full smoke, and its egress counters show
      zero outside the list.



- [x] 2.5 Identity separation for the VMMs: option (b) was
      picked and built by lane a (per-machine sub-uids from the
      delegated range, allocated at first spawn, persisted in
      the machine dir), with (c) -- MCS categories per VM --
      landing at the join (deal-breaker 7). What the choice left
      open moved to 1.6.14i: the inside face of the map, the
      translator's uid, the personas, and the groups.
- [x] 2.6 The cella <-> engine protocol (2026-09-04): the .proto
      was the vocabulary since birth; the wire landed as
      crates/cella-engine -- the bridge streams Events over rpc
      Decide and lands Decisions in the verdict file; gRPC never
      enters a VMM. Five gates green (engine-w1..w5,
      docs/WORLD-ENGINE.md). The engine itself stays external:
      cella ships the seam, the world ships the judge (2.7).
- [ ] 2.7 Augmenting world engine (AWE): the engine over the appliance
      seam; materializer = the Artifact Keeper fork with timeline
      translation (response time = request T + delta).

## Done

(nothing yet)
