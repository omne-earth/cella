# Integrating collection: evidence out, never exec in

How an integrator's harness gets results out of a run. cella has
no exec-into, by design: the run is a sealed experiment whose
results are collected afterward. Nothing here is specific to any
one harness.

## Not the exec model

The container world drives a live workload from outside: exec
installs an agent after start, runs the verifier inside the
workload's own filesystem, and copies artifacts out of a live
machine -- the workload and its examiner share a room. With
cella, the same job becomes bake, run, collect:

1. **Bake.** The agent and its configuration enter the image at
   build time (docs/integration/ROOTFS.md); the init shim starts
   them. Nothing is installed after boot.
2. **Run.** The machine runs sealed: the console transcript (lab
   flavor) and the chronicle are the only live observations, and
   the network is judged
   (docs/integration/MEMBRANE-MEMORY.md).
3. **Collect.** The run ends -- the guest halts, or the timeout
   stops it. `cella extract` requires a still machine (stopped,
   frozen, or archived; running is the one refusal, the universe
   family's rule) and copies the declared artifacts out of the
   still disk as a tar stream:

   ```sh
   cella extract <vm> /app/report.json > report.tar
   cella extract <vm> / > rootfs.tar     # the whole tree
   ```

   The disk is read inside a throwaway appliance, never mounted
   on the host (the extract appliance signals through a trailer
   on its scratch disk, which the host polls; nothing waits on a
   guest exit). Numeric uid/gid, modes, and links survive; every
   read lands in the audit book. The verifier reads evidence,
   never a live machine.

## The trust boundary

The tar is workload-authored bytes: the trailer proves the job
completed, not that the content is honest. Unpack with traversal
protections -- no absolute paths, no `..`, no following symlinks
out of the target (modern GNU tar's defaults; assert them in the
pipeline rather than assuming).

The trade is deliberate. A live workload can lie to its examiner
interactively; a still disk cannot answer at all, only be read.
Verification against evidence is stronger than verification by
conversation, and the freeze makes the evidence exact.

## Working references

The gate scripts in scripts/test/ are working examples of driving
cella from a harness: create, start, decide, freeze, extract,
destroy, with assertions at each step
(scripts/test/extract.sh is the collection reference). The
machine directory (`~/.cella/machines/<name>/`) is plain files;
it can be read while a machine runs.
