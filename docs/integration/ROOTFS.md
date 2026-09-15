# Integrating a workload image as a cella rootfs flavor

How an integrator turns one workload image (a Dockerfile, a
Containerfile, or any built filesystem tree) into artifacts cella
boots. The job is well-bounded, each step is checkable, and the
target runtime is small and strict. Prerequisite reading:
docs/LIFECYCLE.md ("The homes"). Nothing here is specific to any
one harness.

## What the converter builds

One rootfs flavor per workload:

```
~/.cella/rootfs/<name>/rootfs.ext4    the root filesystem
~/.cella/rootfs/<name>/golden.json    the manifest (see below)
```

A shared kernel flavor is built once, not per workload. A run then
looks like:

```sh
cella create trial-1 --kernel <kernel-flavor> --rootfs <name> \
    --mem-mb 2048 --net none
cella start trial-1
```

Networked workloads replace `--net none` with a port map
(docs/EXAMPLES.md, E1-E2).

## The rules that shape the design

1. **Do not add flags to cella.** cella boots flavors, never bare
   paths. A flavor is a directory with an artifact and a manifest.
   The converter writes two files; cella stays unchanged.
2. **Every artifact carries a manifest.** The manifest lets
   `cella doctor verify` recompute the digest and judge the
   artifact. An artifact without a manifest does not boot.
3. **OCI ends at build time.** A container engine may build and
   flatten the image. No engine, runtime, or spec exists at run
   time: cella boots a kernel and an ext4, nothing else.

## The steps

1. **Build the image** with whatever builds it (podman on a
   Dockerfile or Containerfile is the common case).
2. **Flatten it** to a directory tree (`podman create` +
   `podman export`, or `podman unshare` with a mount). Record the
   image digest: it goes into the manifest.
3. **Lay the init shim.** A container image expects a runtime to
   start its entrypoint; cella boots `/sbin/init`. Write a small
   init (a shell script is fine) that:
   1. Mounts /proc, /sys, and a devtmpfs on /dev.
   2. Sets the image's environment variables and working
      directory (read them from `podman inspect`).
   3. Runs the image's entrypoint and command with the workload's
      instruction available.
   4. Writes its output to the console and halts when the command
      exits.
4. **Make the ext4.** Size the filesystem to the tree plus the
   workload's writable headroom, `mkfs.ext4`, copy the tree in,
   and place the shim at /sbin/init.
5. **Write the manifest.** `golden.json` beside the artifact, the
   same shape as every cella golden:

   ```json
   {
     "axis": "rootfs",
     "flavor": "<name>",
     "artifact": "rootfs.ext4",
     "sha3_256": "<sha3-256 of rootfs.ext4>",
     "bytes": <size of rootfs.ext4>,
     "built_epoch": <unix seconds>,
     "input_Dockerfile": "<sha3-256 of the build input>",
     "input_image": "<the OCI image digest>"
   }
   ```

   The `input_*` keys are free to name; each records a digest of
   something that shaped the artifact. A changed input means the
   converter rebuilds, and the manifest shows why.
6. **Verify the result.** `cella doctor verify` must pass on the
   new flavor before the conversion counts as done.

## Limitations

- **One vCPU.** Every cella machine runs a single vCPU today.
  Long compiles are slow; set honest timeouts.
- **The lab flavor for benches.** The field build has no console.
  Bench runs use the lab binaries (`make build-lab`,
  `target/lab/*`) so the console transcript lands in the machine's
  log. Artifact collection needs no console at all
  (docs/integration/COLLECTION.md).
- **Nested layers need distinct knock ports** (docs/EXAMPLES.md,
  E2).
