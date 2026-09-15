# Titanium on cella: the mapping

What is titanium-specific in a cella integration. The generic
walks live per topic, and titanium follows them unchanged:

- docs/integration/ROOTFS.md -- a task's Dockerfile becomes a
  rootfs flavor (the converter's steps, the init shim, the
  manifest).
- docs/integration/MEMBRANE-MEMORY.md -- titanium's gRPC rule
  engine takes the seat: the seam contract, the recommended
  policy grammar, the exhaustive example.
- docs/integration/COLLECTION.md -- bake, run, collect: the
  verifier reads evidence through `cella extract` (a still
  machine only), never a live machine.

## The task.toml mapping

- `allow_internet = false` -- `--net none`: no membrane, no
  crossings, no engine. Start here; most tasks need nothing else.
- `allow_internet = true` -- a world nic with a port map
  (docs/EXAMPLES.md, E1-E2), titanium's engine on the seam, and
  the task's policy checked in beside the task as its source
  (titanium's own file; cella reads no policy -- the wire is the
  contract).
- `artifacts = [...]` -- the collect step's extract paths.
- Resource keys (`mem_mb`, `storage_mb`) -- create flags and the
  ext4 sizing at conversion.

## What the move buys, and costs

Before, without cella (the krun-podman rung): the harness derives
an allowlist from the task's URLs, the container's edge enforces
it -- decided once, then flows run free -- and `podman exec`
reaches into the live workload at will.

With cella: every crossing parks and is decided, one decision per
crossing, each on the record; a grant with skip_freeze lands as a
membrane memory and later crossings run live until keep_open
clears it; and there is no exec-into -- the trajectory gains what
no other rung has, the chronicle of every crossing the agent
attempted, including the refused ones. The costs are stated in
ROOTFS.md's limitations (one vCPU; the lab flavor for benches;
distinct knock ports when nesting).
