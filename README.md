# cella

*a cryogenic world for agents*

cella runs workloads in hardware-isolated micro-VMs. It is written
from scratch in Rust, directly on KVM. It is ten small binaries.
It has no daemon, no capability, and no host network object. You
can stop a machine in one instant. You can keep the machine as
files. You can resume it, and the guest cannot detect the stop.
You can copy it, archive it, and judge each frame at its border.

## Design principles

1. **A machine is files.** A directory is a machine: manifest,
   disk, RAM image, sidecar. Each verb is a transaction on one
   directory (docs/LIFECYCLE.md).
2. **Time is cryogenic.** Freeze stops the instant. Thaw resumes
   it. The clocks, the timers, and the entropy of the guest
   continue as if there was no gap, at each nesting depth
   (docs/FREEZE-THAW.md).
3. **The border is total.** A machine is born closed. Each frame,
   in both directions, parks at the membrane for an external
   decision. The two books are tamper-evident
   (docs/NETWORK-MODEL.md).
4. **The network is rootless.** One translator process serves each
   machine. It changes decided frames into plain socket calls.
   There is no tap, no bridge, no NAT rule, and no privilege
   (docs/ROOTLESS-NETWORK.md).
5. **Machines are artifacts.** Branch a frozen machine into twins.
   Archive a machine into a rock. Inspect its disk as evidence,
   never as a machine (docs/LIFECYCLE.md, "The universe family").
6. **Worlds contain worlds.** A guest can run cella and can host
   its own guests. The same stack operates one level down
   (docs/NESTED-BOOT.md).

## Cryogenic scope

The word is a claim, and the claim has three parts:

1. **Time stops with the machine.** A freeze stops the guest's
   clocks, its timers, its TSC, and its entropy, not only its
   execution. A thaw continues them from the frozen instant. The
   guest does not age across the gap, and no measurement from
   inside can show that the gap existed. The probes measure this
   at each nesting depth (docs/FREEZE-THAW.md).
2. **The frozen state is inert and exact.** A frozen machine is
   two files. No process runs, nothing decays, and nothing costs.
   A thaw after an hour or after a year resumes the same instant.
   The RNG state persists across the thaw, by design: the
   cryogenic principle applies to the full machine state, and a
   reseed would be the gap made visible. A branch copies the
   instant into twins that share the CRNG state of the fork;
   divergence comes from the world, never from a reseed
   (docs/LIFECYCLE.md, "The universe family").
3. **The gap is where the world acts.** A machine freezes at the
   moment it needs something: its own network request parks, and
   the park is the freeze. A judge decides while the machine
   sleeps, the world can grow to meet the request
   (docs/WORLD-ENGINE.md), and the thaw delivers the answer into
   a guest for which no time passed. The freeze decouples the
   machine's time from the judgment's.

## Quick start

```sh
make test          # seconds, no KVM -- the no-KVM battery
make init          # once per host: deps, toolbox, goldens (the one sudo moment)
cella create room --net world
cella start room
cella gateway room open
# inside, say the workload asks for example.com:
# the request parks at the gateway border and waits
cella gateway room show          # the held crossings, one line each with id
cella gateway room release <id>  # let one crossing through
```

The machine is dark before `open`. After `open`, each crossing
parks and waits: `show` lists the holds with their ids, `release`
lets one through, and `refuse` denies one -- the workload gets an
immediate network error, not a hung connection. The worked
shapes, E1-E6, are in docs/EXAMPLES.md.

## The verbs

```
cella build <kernel|rootfs> <flavor>
cella create <machine> [--net SPEC] | start <machine> | enter <machine>
cella freeze <machine> | thaw <machine> | stop <machine> | destroy <machine>
cella list | info <machine> | selftest
cella gateway <machine> show | release <id> | refuse <id> | inspect <id> | open | close
cella branch <machine> <new-machine> | archive <machine> | inspect <machine>
cella doctor check | fix | verify
```

One shim sends each verb to its persona binary. Each binary owns
only its own verbs (docs/LIFECYCLE.md, "The verbs").

## Testing

```sh
make test          # no KVM, runs in each environment
make smoke         # the full battery: one part per binary
```

Each gate is a make target. It reports SKIP with a reason when KVM
is absent. It writes its log to `.logs/`. The full contract is in
TESTING.md.

## Security, today

Rootless and daemonless are enforced: no capability, no setuid,
and no privileged process. The one sudo moment is the host
provisioning of the install. The VMM runs in its bwrap jail with
its seccomp filter. The per-binary jails and the SELinux
enforcement are the current work (tasks/PHASE2-security.md). The
status table is in docs/LIFECYCLE.md, "The security boundary".

## The tree

```
crates/            ten binaries: the shim, the personas, the VMM
docs/              the law: seven documents, cross-referenced by id
scripts/test/      the gates, one per workflow
security/profiles/ per-binary bwrap, seccomp, and SELinux policy
tasks/             the boards: PHASE1-core (done), PHASE2-security
proto/cella.proto  the one language the verbs and the engine speak
```

The boards keep each ruling with its date. The documents state the
law. The batteries certify the law.
