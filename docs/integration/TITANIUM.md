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
  Bench runs use the lab (-debug) binaries so the console
  transcript lands in the machine's log.
- **The network is judged, not allowed.** With a world nic,
  every crossing waits for an external decision
  (`cella gateway <vm> release <id>`). The harness supplies the
  judge; a task's allowlist becomes that judge's policy. Start
  with `--net none` tasks; they need none of this.
- **Nested layers need distinct knock ports**
  (docs/EXAMPLES.md, E2).

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
   no exec-into. When the run ends, `cella inspect` mounts the
   disk read-only, and the verifier reads the artifacts (for
   example /app/report.json) from that view.

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
