# cella

*a cryogenic world for agents*

cella runs workloads in hardware-isolated micro-VMs. It is written
from scratch in Rust, directly on KVM. It is ten small binaries.
It has no daemon, no capability, and no host network object. You
can stop a machine in one instant. You can keep the machine as
files. You can resume it, and the guest cannot detect the stop.
You can copy it, archive it, and judge each frame at its border.

## The ideas, in order

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
lets one through, and `refuse` answers it cleanly. The worked
shapes, E1-E6, are in docs/EXAMPLES.md.

## The verbs

```
cella build | create | start | enter | freeze | thaw | stop | destroy
cella list | info | selftest
cella gateway <vm> show | release | refuse | inspect | open | close
cella branch | archive | inspect
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
