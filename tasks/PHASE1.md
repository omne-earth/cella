# Tasks

Running scratch of the restructure work (feat/restructure). One line
per task; move a line to Done with its commit when it lands.

## Now (feat/gateway)

The network model lives in docs/NETWORK-MODEL.md; these lines
track progress only.


Phase 1 -- the surface over the existing backend -- is closed
(every line in Done, battery green on both machines 2026-09-01).

Phase 1.6 -- the total membrane (ruled 2026-09-01): everything
requires a decision, nothing stands, and the mouth closes.

- [x] 1.6.1 The park key drops to the most primitive name a frame
      has: (ethertype, destination MAC), refined to (ip, port,
      proto) when IPv4 parses. Every egress frame parks -- ARP,
      IPv6, kernel chatter, any future protocol. No exemptions.
      Destination on the wire gains ethertype and mac; show
      renders the L2 name (arp ff:ff:ff:ff:ff:ff held).
      - [x] 1.6.1a The thaw rebind speaks the primitive key: a
            restored frame re-marries its operation id by the new
            (ethertype, mac | ip, port, proto) name.
      - [x] 1.6.1b The matcher never guesses: one candidate
            operation and a consistent frame count, or the frame
            re-mints a fresh id and stays held. Ambiguity is no
            match. A collision costs a decision, never a leak.
            Unit test: a colliding sidecar re-mints and holds.
- [x] 1.6.2 No pass entries, period. The allowed table is deleted
      from the VMM; allow_flow leaves the proto (Release becomes a
      bare message; the Released record drops the flag). Every
      park is a fresh decision -- an ARP refresh mid-flow freezes
      the flow, and that is the law working. AC3/AC4 churn; let
      them churn. Grep target after: allow_flow, allowed -- zero
      outside history. The release --no-allow flag is deleted
      with the field: the only allow that exists is the explicit
      release of one named operation, once. The Accord version
      bumps with the proto change, thus two ends can never
      disagree about whether releases carry allows.
      - [x] 1.6.2a One door: with the pass table gone, the TX
            write to the TAP is reachable from the decision
            delivery alone -- the function goes private to that
            path (the compiler holds the door), and make test
            gains a static gate asserting exactly one TX call
            site (a second door fails the battery, not a
            review).
- [x] 1.6.3 (437ecdd) Only the verb opens the valve. The SIGUSR2 open path
      is deleted (handler, flag, registration); the --valve flag
      leaves the VMM interface; every VMM -- verb-spawned or raw --
      is born closed.
      - [x] 1.6.3a The live decision-apply path is deleted: it is
            dead code under one-shot (a running machine froze at
            its first park, thus it never holds anything to
            deliver). Decisions stage; the thaw edge alone
            applies them. SIGWINCH survives solely as the wire
            under the live valve edges (open and close against a
            running machine).
- [x] 1.6.4 Two automata, two controllers (ruled 2026-09-02).
      The machine automaton (running, frozen) belongs to the
      machine verbs; the valve automaton (closed, open) belongs
      to the gateway verbs alone. Neither writes the other's
      state: the valve is born closed at create, the gateway
      verbs flip it in every machine state -- frozen included --
      and the posture holds across any number of freezes and
      thaws until the opposite verb. No thaw resets it (a reset
      would discard the response to a judged crossing, always --
      the world answers at network speed and no re-open wins a
      race against an RTT). Openness is not permission: the next
      unjudged egress parks and freezes as ever. The valve state
      gets its own record beside the machine (not the manifest's
      identity fields); the two automata touch at two wires
      alone: the park (border to time) and the thaw apply (time
      to border). Branch is canonically identical: the twin
      inherits the valve record like every other byte -- a
      posture, never a privilege (its first undecided egress
      parks like any other). docs/FREEZE-THAW.md, "The two
      automata", is the reference.
- [x] 1.6.5 (5b457a3) The console leaves the release build, both
      directions (ruled 2026-09-02). No console.sock, no
      console.log, no ear, no mouth: the release VMM neither
      emits ttyS0 nor accepts input, and enter refuses outright.
      The only crossings of a release machine are the disk at
      birth and decided frames at the membrane. The debug build
      keeps the full console as the lab instrument -- the probes
      and gates drink from it (the clock probes measure through
      it) -- like jail and seccomp, sanctioned in the lab, absent
      in the field. The smokes and probes pin to a release-sized
      profile with debug-assertions on (the debug profile's
      binary does not fit the nested images); the in-image cella
      of the nested and inception rootfs bakes that profile too.
      - [x] 1.6.5a selftest goes console-free: the installed
            host's acceptance gate judges by files and exit codes
            alone -- create, start (pid alive), freeze (sidecar
            present, no .tmp), thaw (state consumed, VMM alive),
            destroy -- plus the installed world's negative: born
            closed, a ping gets nothing, and no freeze happens.
            It doubles as the proof that the release binary's
            mouth is shut.
      - [x] 1.6.5b enter becomes a debug affordance: the verb
            states so against a release binary, and the README
            and LIFECYCLE stop teaching it as the front door --
            an installed machine is dark and is observed through
            files, verbs, and the chronicle.
      - [x] 1.6.5c Two install flavors, two scripts, no reuse:
            make install-release (the field: console-free binary;
            what install.sh becomes, explicitly release) and make
            install-debug (the lab, deliberate and named). No
            shared script between them, thus no accidental
            install of the wrong flavor. Debug binaries carry
            the -debug suffix (cella-debug and its personas),
            thus the flavor is visible in every invocation and
            the two never shadow each other on PATH; doctor
            names the installed flavor, and selftest proves the
            field binary's console is gone.
- [x] 1.6.6 Every network gate gains its cycles: after open, the
      first freeze is the ARP park. The grep-an-ip gates (ping,
      udp, gateway-cli, multinet, AC2-4) release the L2 operation
      first or move to the pump pattern; the pump gates absorb it
      already. The loop is open once, then park, decide, thaw,
      park again: the posture persists, and the gates assert it
      (a machine opened in epoch one parks -- not drops -- in
      epoch two). New positive step: a valve verb against a
      frozen machine works (open a sleeping machine; it parks on
      its first post-thaw egress). New negative steps: a machine
      never opened stays dark through create, freeze, and thaw
      (its egress drops, nothing parks), an IPv6 or
      unknown-ethertype frame parks (never passes), a close then
      an open confers nothing (the next egress parks -- nothing
      stands, there is nothing to remember), a release build
      writes no console byte, and a thaw over a colliding
      sidecar holds every ambiguous frame -- none delivers. One
      valve spans all transports: a park on any nic freezes the
      machine, asserted once in the multinet gate.
      - [x] 1.6.6a The colliding-sidecar gate: a real
            same-destination population (several held operations
            under one key) crosses a thaw; every ambiguous frame
            re-mints and stays held, none delivers, and the melted
            collision's stale ids get an explicit refuse so the
            chronicle closes them.
      - [x] 1.6.6b The never-opened machine stays dark through its
            whole life: create, freeze, thaw, and the machine
            still answers nothing, parks nothing, freezes on
            nothing -- the closed self-loop asserted across the
            machine automaton's edges, not only at birth.
      - [x] 1.6.6c One valve spans all transports: the multinet
            gate asserts that egress on the second tap parks under
            the same valve that governs the first -- a park on any
            nic freezes the machine.
      - [x] 1.6.6d AC3 keeps the fake for reliability and restores
            the real-internet attempt AC5 (skipped when offline), so
            the battery still touches the true world somewhere.
            The DEVICE-STATE acceptance row states both legs.
- [x] 1.6.9 (part 1: 9ac235b -- the mechanism; part 2: 001b867
      -- the choreography; full battery green) The ear gets
      customs (ruled 2026-09-02): ingress
      under an open valve holds for a decision like egress does,
      with one asymmetry -- an incoming hold never freezes the
      machine (the world's knock is not the resident's deed); the
      frame waits at the border, and in the guest frame an
      undelivered packet is network latency. The show verb splits
      by direction: bare show renders both with a DIRECTION and a
      neutral PEER column; show outgoing and show incoming narrow
      it (DESTINATION and SOURCE). release and refuse stay
      direction-blind by id; open and close stay one valve. The
      known cost, stated: the guest's clock runs while inbound
      waits, thus slow incoming decisions are the one wait the
      resident can feel -- the peer-patience shape pointing
      inward, resolved by the terminator like its twin. The
      fail-closed apply covers this direction identically: an
      incoming decision that cannot apply stays staged, its
      operation stays held, and the queue never pops on failure
      -- in either direction, order advances only on an applied
      decision or an explicit refusal.
- [x] 1.6.10 cella gateway <vm> inspect <id>: the operator reads
      a held operation's frames, both directions, read-only and
      evidence-grade (the sidecar stays byte-identical). Judgment
      requires sight -- a decision on the address alone approves
      a package by its shipping label. The looking is itself
      recorded: an Inspected event lands in the chronicle. Until
      the terminator, an encrypted payload renders as the sealed
      envelope it is. Rulings (2026-09-02): the verb gets its own
      gate, smoke-inspection, in the aggregate, and the gate is
      CLI operations alone -- park, inspect, show, refuse,
      thaw -- nothing else (the name stays grep-distinct from
      the universe verb's inspect inside smoke-universe).
      Inspect is frozen-only (ruled 2026-09-02): sight requires
      stillness -- a running lane mutates under the render, and
      evidence-grade means a consistent instant. The verb reads
      the sidecar alone (the vessel; the ledger is a chronicle,
      never the store, and no second store appears beside it).
      Nothing is lost: an egress hold implies frozen by law, and
      a judge who wants sight of held mail freezes first -- one
      machine verb, cheap, cryogenic, itself witnessed. Against
      a running machine the verb refuses and says so. The DFA
      diagrams and transition tables of docs/FREEZE-THAW.md
      ("The two automata") gain the input: frozen | inspect |
      frozen | the render, and an Inspected event in the
      chronicle; running | inspect | running | refused. The look
      changes no state in either automaton.
- [x] 1.6.11 The witnessed border (ruled 2026-09-02, sharpened
      2026-09-02): every verb is an event, no exception -- show
      and inspect included; every human action is as auditable as
      the machine's. The chronicle stays the operations ledger
      (parked, released, lapsed); the verbs get their own audit
      stream in the same proto language: an Audit message with
      verb, args, uid, gid, persona, and host_ns (gid rides for
      SELinux debugging). The operator acts in host time: a CLI
      has no guest clock, and against a stopped or frozen machine
      no VMM exists to ask -- guest_ns rides only on the border
      events the VMM itself emits, and the audit stream carries
      the host clock alone. Machine-scoped verbs append to
      machines/<vm>/audit; the placeless verbs (list, doctor,
      build, setup) to the audit file at the CELLA_HOME root.
      The pump's five shows a second make a thick file, and that
      is the truth of what the pump does. Both books carry the
      reserved predecessor-hash field (the chain lands with the
      shakedown); branch and archive carry both books with the
      tree. AVC denials are correlated, never captured: the
      audit entries' clocks are the ausearch join key, and the
      harvest verb lands HERE, before the shakedown needs it --
      a privileged, optional doctor verb that files matching
      denials beside the audit log (the debugger exists before
      the lane that generates the denials). Gate: smoke-witness,
      in the aggregate -- one of each verb class runs against a
      sandbox machine, and every one lands in the right book
      (machine-scoped in machines/<vm>/audit, placeless in the
      CELLA_HOME audit file) with uid, gid, and persona; the
      negatives: a verb that only reads still writes its entry
      (show twice makes two entries), and the AVC harvest on a
      permissive host files an empty set and says so. The static
      gate rides make test, the one-door pattern applied to the
      witness: every verb arm of the dispatch calls the audit
      append, thus an unwitnessed verb fails the battery, not a
      review. Builder's own calls, unruled: the real uid and gid
      of the invoking process, the harvest file's name, and the
      Audit variant's field number in the Message envelope.

- [x] 1.6.12 IPv6 leaves the canonical kernel fragment.
      ipv6.disable=1 in DEFAULT_BASE_ARGS silences the stack at
      boot; necessity says the stack itself goes -- the guest
      carries no code nobody chose. Costs a rebuild of every
      golden and the nested images, thus it lands alone, with
      the digest churn named in its commit. The L2-park law
      keeps its proof regardless: ARP is the in-guest
      representative, and the unit tests carry the exotic
      ethertypes. Groomed 2026-09-02: ipv6.disable=1 STAYS in
      DEFAULT_BASE_ARGS after the kill -- the flag is the
      VMM-wide floor, harmless on an IPv6-less kernel and
      protective on a foreign flavor a user brings; the fragment
      kill governs the goldens. Both kernel flavors rebuild
      (nested builds atop the canonical fragment), and the
      nested and inception images rebake -- they carry the inner
      goldens. Gates: kernel-config-check asserts CONFIG_IPV6 is
      not set, and smoke-udp gains the in-guest negative --
      /proc/sys/net/ipv6 must not exist. The full local battery
      follows the rebuild in the same sitting.

- [x] 1.6.13 The source goes thin with the surface (ruled
      2026-09-02): one workspace split, per-persona crates over a
      real cella-libs -- cella-vmm, cella-machine, cella-gateway,
      cella-universe, cella-build, cella-doctor (cella-network and
      cella-probe already stand alone). A binary contains only its
      own verbs: the field VMM carries no build orchestrator, no
      universe verbs, no doctor in its text pages, thus the
      shakedown confines binaries that already hold nothing
      extra. A file move, not a refactor: the module boundaries
      tighten first (pub(crate) discipline), and the split lands
      before the shakedown (1.6.14), after the border work
      (1.6.9-1.6.11). Gate: make test + full battery unchanged;
      make lines names each crate.
      Rulings (2026-09-02): N separate binaries, one per persona
      -- the multi-call dispatch retires; the source is cut so
      each binary is its own audit chunk, and cella-libs
      membership then falls out by observation (what two or more
      chunks genuinely share is libs -- proto, ledger framing,
      golden digests, the home-resolution path fn -- never by
      taste). The gateway carries no machine code, provably: the
      <vm>/ directory is its complete interface -- five files
      (ledger, verdict, valve, pid, the audit book) plus
      SIGWINCH; its imports today (machine_dir, is_running,
      set_valve_record) are path joins and file ops wearing
      machine-flavored names, and they become gateway-local path
      constants. The seam between personas is a directory of
      framed files and a signal -- a protocol, never an API --
      and the 1.6.14 profile proves it: write on verdict, valve,
      and audit alone; read on ledger and pid; no exec.
      Groomed 2026-09-02, final: cella becomes a pure shim -- a
      launcher that owns zero verbs and execs the persona binary
      named by the first argument; the interface stays intact,
      and only the backend plumbing changes. Cross-persona needs
      do not exist: the commons move to cella-libs by the
      observation rule -- the spawn machinery has two genuine
      users (machine's start, universe's inspect) and joins
      proto, the ledger framing, the golden digests, the
      home-resolution path fn, and the sidecar READER (second
      sighting recorded at 1.6.10: the vmm writes it, the
      gateway's inspect reads it). The only execs are boundaries
      by nature: the shim into the personas, and cella-machine
      into cella-vmm (a separate process by design). Sibling and
      flavor resolution: a persona finds its siblings beside its
      own binary, same flavor -- -debug pairs with -debug (the
      probe's rule, generalized); the field shim never execs a
      lab binary. The static gates move with the split, in the
      same commit: test-witness counts one door per persona
      binary (the shim owns none), and test-one-door's grep
      follows net.rs into the vmm crate. Layout:
      crates/<persona> and crates/cella-libs; the workspace root
      keeps the smoke profile; the install scripts, CELLA_DEV,
      and the Makefile paths follow.

- [ ] 1.6.14 The shakedown (ruled 2026-09-02): the three
      confinement layers go from built to tightened, per persona,
      now that each binary holds only its own verbs (1.6.13).
      Jail: the bwrap profile per persona, and the identity
      decision lands (the Later item: one cella uid, per-VMM
      subuids, or MCS categories -- one gets picked and built).
      Seccomp: per-binary allowlists (the VMM's shrinks to the
      run loop's needs; ioctl filtering on the KVM request set,
      the long-standing candidate). SELinux: the example policy
      becomes the enforced policy -- lateral movement between
      machine directories dies, and LIFECYCLE's one "planned"
      cell turns "enforced". The ledger's tamper-evidence hash
      chain lands here with the SELinux work, as 1.6.11 reserved.
      Parallelizable: two rulings serialize in front -- 1.6.13
      lands, then the identity decision (option c couples the
      jail and SELinux lanes until it is picked) -- and then
      four lanes run independently on disjoint surfaces: jail
      (jail.sh, security/profiles/), seccomp (seccomp.rs),
      SELinux (policy files), and the hash chain (ledger.rs; it
      rides with SELinux by schedule, not by dependency). The
      join is one battery at the end; the known cross-lane leak
      (a jail or SELinux mechanism needing a syscall the seccomp
      lane removed) fails loudly there.
      Gate: full battery green under enforcement on both
      machines, and a negative per layer (a verb outside its
      profile refuses, a syscall outside the list kills, a
      cross-machine touch denies).

      Rulings (2026-09-02): identity is a distinct sub-user per
      machine, with the SELinux profile bound to THAT user --
      secure by default, no shared identity anywhere. The seccomp
      lane blocks every ioctl the run loop does not require: the
      gvisor shape, stricter -- the exact KVM request numbers, and
      anything else kills the process. SELinux goes ENFORCING the
      moment the shakedown completes, every host, no
      permissive-forever dev exception.
      The lanes, one subtask each -- authorship parallelizes on
      Sonnet subagents in isolated worktrees, the batteries
      serialize at each merge, the reviewer merges one lane at a
      time:
      - [ ] 1.6.14a Identity and the jail, one fused lane (the
            subuid mapping lives inside the bwrap invocation --
            the same edit surface). Each machine runs as its own
            sub-user; the spawn maps it; the bwrap profile goes
            per persona (jail.sh, security/profiles/, the spawn
            in cella-libs). Lane gate: a cross-machine file touch
            fails by uid before SELinux exists to deny it, and a
            persona runs under its own profile alone.
      - [ ] 1.6.14b Seccomp, per binary. Each persona's allowlist
            shrinks to its own verbs' needs; the VMM's shrinks to
            the run loop, and the ioctl filter lands: the exact
            KVM request numbers, anything else kills -- the
            gvisor shape, stricter. Lane gate: the per-persona
            selftest dies by SIGSYS on a syscall outside its own
            list, and a KVM ioctl outside the request set kills
            the VMM.
      - [ ] 1.6.14c SELinux, bound to the identity. The example
            policy becomes the enforced policy, per persona, per
            machine sub-user; lateral movement between machine
            directories dies; LIFECYCLE's one "planned" cell
            turns "enforced"; ENFORCING lands on every host at
            the merge. Lane gate: a cross-machine touch denies
            with an AVC, and cella doctor harvest files that
            denial -- the 1.6.11 verb meets its purpose.
      - [ ] 1.6.14d The hash chain. Field 15 fills: every Audit
            and Event entry carries the digest of its
            predecessor, both books, and the chain survives
            branch and archive (the twin's chain forks with its
            history). Rides with the SELinux lane by schedule,
            not dependency. Lane gate: a book with one edited
            entry fails verification loudly, and an intact book
            verifies end to end.
      Convergence: every worktree branches from the same
      post-split commit. The reviewer merges serially onto
      feat/gateway in the order a, c, d, b -- identity first (the
      foundation), SELinux second (it binds to a's sub-users),
      the chain third (it rides c's schedule), and seccomp LAST:
      the allowlist shrinks against the final syscall reality of
      the jail and SELinux mechanisms, and the known cross-lane
      leak dies by construction instead of by debugging. Each
      merge is one reviewed, signed commit naming its lane; after
      each merge: make test, the lane's own gate, and the touched
      KVM gates -- serially, one pool, no concurrent batteries. A
      conflict belongs to the reviewer, resolved with the lane's
      agent before the merge commit, never inside it. The join
      stays the parent's gate: the full battery green under
      enforcement on both machines.

- [ ] 1.6.7 The documents state the tightened law, and the
      retired phases make "Not in scope" the permanent scope
      statement. The law: no frame leaves undecided at any layer,
      no allow outlives its decision (there is nothing for it to
      outlive), the ARP sentence dies, and every failure of the
      apply is stillness. The scope: cella judges every frame,
      both directions, at its own seam -- named, held, decided
      externally, witnessed. Permanently outside cella, named at
      the proto seam where a judge builds them: the appliance
      pair, TCP termination, TLS against a pair CA, DNS-in-frame
      and every world-side service, ownership of peer patience,
      and the timeline rewrite. NOT outside: ingress judgment
      (1.6.9 builds the ear's customs at this seam) and UDP
      judgment (shipped; UDP death was terminator territory and
      retires with it). The temporary-backend language dies: the
      never-guess rebind is the architecture, not scaffolding.
      The residuals, named once, plainly: a resident can modulate
      its compute and I/O shadow on the host (host-local,
      listener-required, silenced by the freeze); an open
      machine's ingress delivers freely until 1.6.9 holds it;
      frames that arrive during a freeze are lost at the tap (no
      process listens; the protocols above retransmit); the
      peer-patience bound on multi-cycle exchanges is a permanent
      boundary, the judge's to manage; the field machine is dark
      to the world, not to its host's logbook (vmm.log carries
      the park lines); the pool's neighbor pins assume the
      default guest MAC (a custom --mac on a pool tap breaks the
      convention, stated); and the canonical kernel's quietness
      is chosen (ipv6.disable=1), never an exemption -- chatter
      that exists still parks. NETWORK-MODEL's phases section
      retires; DEVICE-STATE's acceptance rows speak AC3's
      stand-in and AC5's real-world leg (1.6.6d).
      The pass runs once, after the mechanisms (1.6.9-11):
      the documents describe the phase that ships, and the
      residual list drops the entries those items retire. The
      "aperture" wording in AC5 is replaced with plain speech,
      and the sweep hunts any siblings of it.
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

Phases 2 and 3 are descoped (ruled 2026-09-02). The appliance
and the TLS terminator are not cella's to build: cella ends at
1.6 as the enforcement primitive -- every crossing named, held,
decided by someone else, and witnessed. The policy side (the
engine, world-keeping, peer patience, termination) belongs to
whoever presides, over the proto seam that has waited for them
since birth. Consequences the 1.6.7 doc pass states: the
temporary backend is the backend (the never-guess rebind is
permanent, not scaffolding); the peer-patience bound on
multi-cycle exchanges is a permanent, documented boundary of the
mechanism; the phases section of NETWORK-MODEL.md retires.

## Later

- [ ] The host image (post-1.6): the bare-metal host built the
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



- [ ] Identity separation for the VMMs (claimed by 1.6.14, which picks and builds one option; granularity open until then).
      Today the jail separates mounts, not identity: the same uid
      maps into every user namespace, and any unjailed process of
      the user touches every machine's files. Options, one to pick:
      (a) one dedicated cella uid for all VMMs -- separates them
      from the user's session; (b) per-VMM subuid ranges via the
      user namespace map -- separates VMMs from each other too;
      (c) SELinux MCS categories per VM (sVirt) on top of either.
      Per-VMM is undecided.
- [ ] The cella <-> engine protocol: a .proto for the verdict
      vocabulary; gRPC never enters a VMM (see NETWORK-MODEL.md,
      the control plane).
- [ ] Augmenting world engine (AWE): the engine over the appliance
      seam; materializer = the Artifact Keeper fork with timeline
      translation (response time = request T + delta).

## Done

- [x] 1.5 Regression close (2026-09-01): the full battery green on
      both machines -- make smoke (test first, then every gate) and
      the device-state gates, bare metal and the development host.
      smoke-udp joined the aggregate the same day and passed on
      both: no datagram leaves undecided, proven from within the
      guest. The one bare-metal interruption was the internet
      itself (AC3 fetches a real page); it passed on the rerun.

- [x] The battery is one call. make smoke runs the no-KVM test
      battery first (fail fast), then every gate; the demo target
      retires into smoke-shell (the one gate that drives the
      machine through enter), and device-state joins the
      aggregate. The gateway gate pumps after the pair thaw (a
      fresh epoch re-decides every hop) and waits out the gap
      between the sidecar landing and the old VMM exiting.

- [x] Plain words for the closed valve: the coconut metaphor left
      the documents, the comments, and the gate output -- a closed
      machine is stated as what it does. The task scratch left the
      tree for .claude/, and the repository history was rewritten
      to carry no trace of it (all branches, all refs, the remote
      force-updated; backup bundle kept beside the workspace).

- [x] The freeze of a hypervisor guest (sidecar v8). The first
      freeze of a machine that hosted a live inner VM triple
      faulted at thaw: the inner VM's entry state lives in the
      host kernel, not in guest RAM. The sidecar now carries the
      raw KVM nested-state block (vendor neutral) and the nested
      MSRs the host can read (MSR_VM_HSAVE_PA on AMD was the
      loss). A host without the capability writes an empty block.

- [x] Every smoke speaks the verbs. boot, thaw, inception, and
      nested-boot run through create/start/freeze/thaw in sandbox
      homes; jail and seccomp stay raw as instruments of the layer
      under the verbs, with the probes. The nested-boot gates
      carry negative steps: a closed machine answers nothing and
      never freezes on inbound; an open machine answers nothing
      before a decision.

- [x] Born closed, the coconut, the two-posture valve. A machine
      is created closed: nothing in or out, no parking, no freeze
      -- dark. cella gateway open arms the membrane (never a free
      flow): every egress frame, replies included, parks and the
      park is the freeze; close returns the dark, killing even the
      once-allowed. The posture rides the manifest (survives stop
      and thaw); the verbs flip it live via Valve messages. The
      Instrument posture is deprecated: no unmanaged mode exists
      on any interface (the raw flag interface opens into the
      membrane). No standing allows, ever: a pass entry lives only
      in the running epoch, rules evaluate atomically every time,
      and a thaw re-parks the once-allowed (Released.allow_flow
      stays in the book as audit alone). smoke-net retires;
      smoke-ping replaces it (fail, freeze, release, reply, fail).
      The nested www init becomes the inner machine's engine, one
      level down. All gates re-plumbed and green.

- [x] 1.4 The gates speak the verbs: close replaces the raw
      SIGUSR2, release/refuse replace --write-decision and the
      WINCH kicks, show supplies the ids; the --write-decision
      flag is deleted. The signals survive only as the wire under
      the verbs, never in a script. A wholesale sweep confirms:
      the remaining mentions are the mechanism (gateway.rs,
      main.rs), history (Done entries), and --dump-ledger, kept
      as the debug view like --dump-state.

- [x] 1.3 cella gateway <vm> <verb>: show (held; --all is the
      audit view), release <id> (allow_flow on by default;
      --no-allow opts out), refuse <id> [--why], close (one-way),
      open (refuses, stating the ratchet). Ids resolve by
      unambiguous hex prefix. A decision against a running machine
      kicks it; against a sleeping one it defers to the thaw, in
      park order. Unprivileged persona of the multi-call binary
      (cella-gateway symlink); files and signals only. Gate:
      make smoke-gateway-cli (in make smoke).

- [x] Bare-metal validation of the one-shot trio (2026-09-01):
      test, smoke (with the ledger gate), and every AC green; AC3's
      engine loop landed the fetch in six ratchet cycles -- the
      same count as the nested machine, the determinism visible.

- [x] One way, one-shot, hold-ratchet. The park is the freeze: a
      closed valve's first parked batch drains, the ledger flushes
      (Parked on disk before the sidecar exists), and the machine
      stops before the guest runs again -- no accumulation, no
      operation cap needed, the sprayer is frozen after its first
      batch. The valve ratchets one way: a machine whose chronicle
      exists parks across every thaw, no re-arm. The thaw applies
      queued decisions in park order; SIGWINCH stays as the live
      kick. Gates: smoke-ledger proves self-freeze, valve
      persistence, and the cross-cycle order case; AC3 runs the
      engine loop (six ratchet cycles land one www fetch); AC4's
      both legs decide by id against the self-frozen machine. The
      four docs state the semantics (freeze has two numbered
      triggers; homes carries verdict and network/ledger; the
      polarity section is a sequence diagram and a numbered
      surface). Full battery green locally; bare metal pending.

- [x] 1.2+ Release names an id. The thaw delivers nothing:
      operations survive it as held, each restored frame rebound to
      its original id through the ledger (the chronicle is the
      index; a genuine gap mints in the guest frame and logs the
      anomaly). The verdict file carries framed Decision messages;
      decisions apply strictly in park order -- an operation behind
      an undecided predecessor waits; Refusal drops, emits Lapsed,
      and advances the order. --write-decision is the gates' tool
      until the 1.3 CLI. Gates: smoke-ledger (nine steps: id and
      clocks, held across freeze/thaw, release by id, no phantom,
      and the two-operation order case), AC3 and AC4 replumbed to
      decisions by id (the freeze leg decides while the machine
      sleeps; the thaw applies in park order). The valve does not
      survive a thaw in this backend -- the gate re-arms it; the
      one-shot layer replaces that with valve persistence.

- [x] Bare-metal validation of the branch so far: make test, make
      smoke (including smoke-gateway and smoke-multinet), and make
      smoke-device-state all green on bare metal, 2026-09-01.

- [x] Docs carry the gateway rung: the gateway flavor in
      LIFECYCLE, smoke-multinet and smoke-gateway rows in TESTING,
      the network-model pointer and line counts in README.

- [x] 1.1 proto/cella.proto + generated types: prost codegen in
      build.rs, src/proto.rs with the length-delimited framing
      (frame/unframe), protoc in install.sh and the toolbox list
      (provisioning converges, existing toolboxes heal). Gate
      green: unit-test round-trips every Message body, partial
      frames wait, frames stream back to back.

- [x] Multi-net: a machine takes N taps (--net tap1,tap2; --tap
      repeats at the VMM). eth<i> sits at 0xd0001000 + i*0x2000,
      IRQ 6 + 2*i -- the attach slot between them never moves, thus
      every existing manifest's ABI holds. ip= configures eth0
      only; later nics belong to the image init (the gateway
      flavor). Claims are exclusive per tap of the list. Gate:
      make smoke-multinet (in make smoke) -- both nics in the
      guest, host pings eth0, claims refused, and a freeze/thaw
      with two transports in the sidecar.

- [x] SKIP guards collapsed: `cella doctor gate <needs...>` (kvm,
      bwrap, tap, golden:<axis>:<flavor>) -- quiet, one SKIP line
      on the first unmet need; the eight test scripts call it and
      keep only their CELLA_TEST_* override checks local.

- [x] cella doctor check|fix|verify, delivered across the
      restructure and universe branches: check judges the host,
      fix repairs without root or deletion, verify judges the
      goldens and now the machine layers (verify <vm>); selftest
      opens with doctor check. Build makes, doctor judges.

- [x] The universe family, cella-universe (a persona of the
      multi-call binary): branch, archive, inspect. The matrix:
      running is the only state a universe verb refuses. branch
      copies a still machine -- a frozen source yields a frozen
      twin, a stopped source a fresh-bootable copy, a rock copies
      to a rock (the latch carries; nothing resurrects by side
      effect) -- and the copy carries net none. archive keeps the
      storage layers, drops the runtime state, latches
      state=archived; start/thaw/enter refuse a rock. inspect
      attaches the disk of any still machine to a throwaway
      appliance (<vm>-inspector) as a second virtio-blk, read-only
      at the device, mounted /rock with
      ro,noexec,nosuid,nodev,norecovery; the detach destroys the
      inspector, and the evidence stays byte-identical. Every
      operation records layer digests into the manifest it
      produces; list gained the DISK-SHA3 column, info the full
      digests, doctor verify <vm> recomputes them. Gate:
      make smoke-universe (in make smoke) -- it watches the
      inspector console live: /rock must mount, the evidence must
      read back, a write must fail loudly, the inspector must die,
      and the evidence must stay byte-identical.
- [x] Stale-golden detection: build skips only while the recorded
      input digests match; a changed init script or fragment
      rebuilds and names the input (found on bare metal: an
      inspect appliance booted an image whose init predated
      /rock).

- [x] Thin CLI split: one multi-call binary, persona dispatch on
      argv0 -- cella-machine (the lifecycle verbs, refuses the
      rest), cella-build, cella-doctor, cella-vmm (the flag
      interface alone; the jail binds and runs /cella-vmm).
      install.sh makes the symlinks. cella-network and cella-probe
      stay real binaries (a file capability binds to an inode).
      cella-libs is satisfied by the lib crate for now. Profile
      contents remain the shakedown branch's.
- [x] Docs aligned with the thin-CLI world: LIFECYCLE.md (personas,
      verb table, the file-capability network story, golden.json),
      README, FREEZE-THAW (sidecar v7 in the diagram), NESTED-BOOT,
      TESTING (device-state, doctor, probe rows).

- [x] `cella-probe`: one installable binary, the probes as
      subcommands (wallclock | freeze-thaw-clock | sregs), moved
      with history from probes/ into src/bin/cella-probe/. No cargo
      at run time: probes resolve cella as their sibling binary;
      make probe-* targets and the inception image migrated (the
      in-guest probe is now /bin/cella-probe). Acceptance held: all
      AC gates, demo, smoke-thaw, wallclock, freeze-thaw-clock, and
      probe-inception green after the move.

- [x] Manifests baked into the nested/inception images beside the
      inherited goldens (mode 444 in-image): an inner cella doctor
      verify judges them before booting deeper. A golden without a
      manifest is a build error, not a heal case. doctor check also
      gained the boot-unit fact (cella-network.service enabled).

- [x] Tap pool boot persistence: install.sh writes and enables
      cella-network.service, a root oneshot at boot (SUDO_UID pins
      the tap owner to the installing user); doctor check names the
      unit when the pool is absent. Verified by a manual unit start;
      the next real reboot is the live proof.

- [x] `cella-network` -- the first thin CLI, pulled forward: the one
      CAP_NET_ADMIN holder as a file capability (install.sh setcap,
      the root moment happens once, at install). No sudo anywhere in
      the runtime path: `cella-network setup` provisions and
      CONVERGES the pool (an interrupted setup heals), and `cella
      doctor fix` invokes it for net FAILs. make setup-tap migrated.
      security/profiles/<cli>/ paths created for every planned CLI;
      contents deferred to the shakedown branch.

- [x] Build manifests: `build` writes `golden.json` beside each
      artifact -- sha3-256, sources, input digests, mode 444; a
      pre-manifest golden gets one on the next build call. sha3
      in-binary via RustCrypto `sha3` (src/golden.rs, the seed of
      cella-libs).

# Reference

## The CLI map (thin split target)

| CLI | Verbs | Notes |
|---|---|---|
| cella | dispatcher, help | execs the others by argv |
| cella-machine | create start stop enter freeze thaw destroy list info selftest | list gains short digests, info full sha3; selftest starts with doctor check |
| cella-universe | branch archive inspect | the branch family; inspect is its one KVM-touching verb |
| cella-build | kernel rootfs | writes golden.json (sha3, RO); verification belongs to doctor, not build |
| cella-doctor | check fix verify | check: host facts. fix: repair what the uid can, print the command for the rest. verify [target]: host goldens, then every VM against its manifest digests; a target narrows it |
| cella-network | setup, pair | the one CAP_NET_ADMIN holder: wiring only, nothing else |
| cella-gateway | show release refuse open close | the membrane surface; unprivileged (files and signals), its own persona |
| cella-probe | wallclock freeze-thaw-clock sregs | /dev/kvm + tap, nothing else |
| cella-vmm | the flag interface + signals (USR1/USR2/WINCH) | internal, spawned by start/thaw; the tightest profile |
| cella-libs | (crate, not a CLI) | sha3, manifests, registry I/O, shared by all |

Implementation note: cella-machine, cella-build, cella-doctor, and
cella-vmm are personas of the one multi-call binary (argv0);
cella-network and cella-probe are real binaries -- a file
capability binds to an inode, and only that inode may hold it. The
lib crate stands in for cella-libs until a workspace split earns
its keep.
