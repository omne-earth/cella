# The titanium converter: a task image becomes a cella machine

This document specifies the conversion of one titanium task (a
Dockerfile or Containerfile) into artifacts that cella can boot. The job is
well-bounded, each step is checkable, and the target runtime is
small and strict. Prerequisite reading: docs/LIFECYCLE.md ("The
homes"), then docs/NETWORK-MODEL.md for the network sections.

## What the converter builds

The converter takes one task's `environment/Dockerfile` (or
`Containerfile` -- podman accepts both names) and produces one
cella rootfs flavor:

```
~/.cella/rootfs/<task-name>/rootfs.ext4    the root filesystem
~/.cella/rootfs/<task-name>/golden.json    the manifest (see below)
```

A shared task kernel flavor (`~/.cella/kernel/task/`) is built
once, not per task. A run then looks like:

```sh
cella create trial-1 --kernel task --rootfs <task-name> \
    --mem-mb 2048 --net none
cella start trial-1
```

Networked tasks replace `--net none` with a port map (see
docs/EXAMPLES.md, E1-E2).

## Limitations

- **One vCPU.** Every cella machine runs a single vCPU today.
  Long compiles are slow; set honest timeouts.
- **The lab flavor for benches.** The field build has no console.
  Bench runs use the lab binaries (`make build-lab`,
  `target/lab/*`) so the console transcript lands in the
  machine's log. Artifact collection needs no console at all:
  `cella extract` works in the field flavor.
- **The network is judged, not allowed.** With a world nic, every
  crossing waits for an external decision. The judge is a gRPC
  engine (docs/WORLD-ENGINE.md): the harness starts one, the
  bridge (`cella-engine <machine> --dial <addr>`) streams every
  park to it as an Event, and each returned Decision applies. A
  task's network policy becomes that engine's policy. Start with
  `--net none` tasks; they need none of this.

  Before, without cella (the krun-podman rung): the task declares
  `allow_internet = true|false` in task.toml, the harness derives
  an allowlist from the task's URLs (agents/network.py,
  allowlist_from_urls), and the container's edge enforces it --
  decided once, then flows run free.

  With cella: the task's policy is titanium's own source (one
  policy, one grammar, titanium's -- cella carries none), loaded
  by titanium's gRPC rule engine, which answers the Event stream:
  every frame parks; the engine releases the granted and refuses
  the rest with the grant's reason, one decision per crossing,
  each on the record; a grant with skip_freeze lands as a
  membrane memory over the stream, and later crossings to that
  destination run live until keep_open clears it.
  `allow_internet = false` is simply `--net none`. The trajectory
  gains what no other rung has: the chronicle of every crossing
  the agent attempted, including the refused ones.
- **Nested layers need distinct knock ports**
  (docs/EXAMPLES.md, E2).

## Integrating the membrane's memory

The membrane's memory (docs/NETWORK-MODEL.md, "The membrane's
memory", N.F.7) is gRPC-only: titanium's rule engine is the one
mind, and everything below is what its integration touches. The
seam contract, in the order the harness meets it:

1. **Implement `service Engine`** (proto/cella.proto): one RPC,
   `Decide(stream Event) returns (stream Decision)`. The engine
   receives every park as an Event and answers by operation id.
   Accord version 4 is the current vocabulary; an older end
   refuses the handshake rather than dropping what it cannot
   carry. The worked example is `cella-engine motor`
   (crates/cella-engine/src/motor.rs) -- the smallest complete
   rule engine, written to be read by an implementer, never run
   in production.

2. **Answer verdicts as before**: `Decision { id, Release }` or
   `Decision { id, Refusal { why } }`, one per park. The why is
   the policy author's sentence; it lands verbatim in the
   machine's chronicle (`Lapsed`).

3. **Plant memory with a third answer**: a grant whose policy says
   skip_freeze sends `Decision { id: empty, MembraneMemory {
   destination, skip_freeze: true, keep_open } }` -- id empty
   because a memory names a destination, not an operation. Use
   the park's own Destination for exactness: matching at the
   membrane is exact, (ip, port, proto) or the ethertype, no
   wildcards. `keep_open` is plain seconds. Leave `written` at
   zero: the bridge stamps it with the host clock at the landing,
   and expiry is `written + keep_open`, absolute -- a freeze does
   not stretch the window, and a zero window is inert.

4. **The harness spawns only the bridge**:
   `cella-engine <machine> --dial <engine-addr>`, one per
   machine, machine-lifetime. The bridge tails the ledger, streams
   Events, and lands every Decision: verdicts into the verdict
   file, memories into `machines/<vm>/membrane-memory` (append-only
   forever -- every byte is a ruling the engine chose to make),
   each landing witnessed in the machine's audit book
   (`verb=membrane-memory`, the window in the args), each followed
   by the kick.

5. **What the run then looks like**: the first crossing to any
   destination parks and freezes -- the engine meets it through
   the stream, and its release applies at the thaw. A crossing to
   a remembered destination parks and waits live: the machine
   keeps running, the guest's own timers tick (this is what keeps
   a TLS handshake's flights inside the peer's patience), and the
   decision applies on the kick. A remembered refusal is an
   instant error with the why on the record -- no freeze-thaw
   churn per denied probe. When `keep_open` lapses, the memory
   clears by its own arithmetic and the cryogenic default
   resumes; nothing needs to retract anything.

6. **What to verify from the harness side**: the membrane-memory
   file exists after the first remembered grant;
   `cella --dump-ledger machines/<vm>/audit` shows the
   `membrane-memory` landings; the chronicle shows every crossing,
   remembered or not. The reference assertions are the six gates,
   `scripts/test/membrane-memory.sh mm1..mm6` (`make
   smoke-membrane-memory`) -- mm1 is the live park, mm4 the live
   refusal, mm3/mm6 the expiry and fail-closed edges; a titanium
   integration test can mirror them one for one.

## Not the exec model: the collection model

The container rungs drive a live workload from outside. cella
does not, by design: there is no exec-into, and the run is a
sealed experiment whose results are collected afterward. The
converter and the harness must both honor this.

Before, without cella (the podman family): the harness reaches
into the running container at will -- `podman exec` installs the
agent after start, runs the verifier inside the workload's own
filesystem, and copies artifacts out of a live machine. The
workload and its examiner share a room.

With cella, the same task becomes bake, run, collect:

1. **Bake.** The agent and its configuration enter the image at
   build time; the init shim starts them. Nothing is installed
   after boot.
2. **Run.** The machine runs sealed: the console transcript and
   the chronicle are the only live observations, and the network
   is judged (the section below).
3. **Collect.** The run ends (the guest halts, or the timeout
   stops it -- and the extract appliance signals through a
   trailer on its scratch disk, which the host polls; it never
   waits on a guest exit). `cella extract` copies the task's declared
   artifacts out of the still disk as a tar stream -- for
   example `cella extract <vm> /app/report.json` for
   `artifacts = ["/app/report.json"]` from task.toml -- and
   `cella extract <vm> /` carries the whole rootfs between exec
   cycles. The disk is read inside a throwaway appliance, never
   mounted on the host; numeric uid/gid, modes, and links
   survive; every read lands in the audit book. The verifier
   reads evidence, never a live machine.

The trade is deliberate. A live workload can lie to its examiner
interactively; a still disk cannot answer at all, only be read.
Verification against evidence is stronger than verification by
conversation, and the freeze makes the evidence exact.

## The rules that shape the design

1. **Do not add flags to cella.** cella boots flavors, never bare
   paths. A flavor is a directory with an artifact and a manifest.
   The converter writes two files; cella stays unchanged.
2. **Every artifact carries a manifest.** The manifest lets
   `cella doctor verify` recompute the digest and judge the
   artifact. An artifact without a manifest does not boot.
3. **OCI ends at build time.** podman may build and flatten the
   image. No container engine, runtime, or spec exists
   at run time: cella boots a kernel and an ext4, nothing else.
4. **The verifier reads evidence, not a live machine.** There is
   no exec-into. When the run ends, `cella extract` streams the
   artifacts (for example /app/report.json) out of the still
   disk as tar; the disk is read in a throwaway appliance, never
   on the host, and never as a machine.

## The steps

1. **Build the image.** `podman build` on the task's Dockerfile
   or Containerfile.
2. **Flatten it.** Create a container from the image and export
   its filesystem to a directory tree (`podman create` +
   `podman export`, or `podman unshare` with a mount). Record the
   image digest: it goes into the manifest.
3. **Lay the init shim.** A container image expects a runtime to
   start its entrypoint. cella boots `/sbin/init`. Write a small
   init (a shell script is fine) that:
   1. Mounts /proc, /sys, and a devtmpfs on /dev.
   2. Sets the image's environment variables and working
      directory (read them from `podman inspect`).
   3. Runs the image's entrypoint and command with the task
      instruction available.
   4. Writes its output to the console and halts when the
      command exits.
4. **Make the ext4.** Size the filesystem to the tree plus the
   task's writable headroom, `mkfs.ext4`, copy the tree in, and
   place the shim at /sbin/init.
5. **Write the manifest.** `golden.json` beside the artifact, the
   same shape as every cella golden:

   ```json
   {
     "axis": "rootfs",
     "flavor": "<task-name>",
     "artifact": "rootfs.ext4",
     "sha3_256": "<sha3-256 of rootfs.ext4>",
     "bytes": <size of rootfs.ext4>,
     "built_epoch": <unix seconds>,
     "input_Dockerfile": "<sha3-256 of the Dockerfile or Containerfile>",
     "input_image": "<the OCI image digest>"
   }
   ```

   The `input_*` keys are free to name; each records a digest of
   something that shaped the artifact. A changed input means the
   converter rebuilds, and the manifest shows why.
6. **Verify the result.** `cella doctor verify` must pass on the
   new flavor before the conversion counts as done.

## Working references

The gate scripts in scripts/test/ are working examples of driving
cella from a script: create, start, decide, freeze, inspect,
destroy, with assertions at each step. The machine directory
(`~/.cella/machines/<name>/`) is plain files; it can be read
while a machine runs.
