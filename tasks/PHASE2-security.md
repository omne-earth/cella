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
