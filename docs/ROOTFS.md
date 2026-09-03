# The rootfs

This document states what a cella rootfs is, which images exist, what
the base of every image is, and how an image comes from a
Containerfile. The VMM is not in this document: it boots an ext4 and
knows nothing about how the ext4 was made.

## What a rootfs is

A rootfs is one ext4 file. `cella build rootfs <flavor>` writes it as
a golden under `CELLA_HOME`, with a manifest beside it that records the
flavor and the inputs. `cella create` copies the golden into the
machine's directory as the machine's own disk. The disk is then part of
the machine: a freeze keeps it, a branch copies it, an archive seals
it.

The rootfs is not part of the trusted computing base. The guest is
untrusted. The rootfs decides what the guest can do, not what the host
can lose.

## The images today

Five flavors, one base:

| Flavor | Contents | Use |
|---|---|---|
| canonical | Static busybox, the heartbeat init | The probes and the smoke tests |
| cella | canonical plus a shell on the console | An operator's machine |
| gateway | canonical plus the membrane, on the gateway kernel | The appliance |
| nested | canonical plus cella itself | A machine that runs machines |
| inception | nested plus the probe | The clock probe one level down |

The base is busybox, built static from a pinned source with a pinned
fragment. Every flavor is the base plus a few files. The init is a
shell script that mounts proc and sys and prints one heartbeat line
per second. The heartbeat is the only channel the probes have into the
guest's clocks.

## The base

The base stays one family for every image. Two facts decide it:

- Twins must be the same bytes. A dynamic linker, a package database,
  and shared libraries that differ between builds are drift between
  images, and drift is a hidden variable in every experiment that
  compares twins.
- A machine that runs tools needs packages. A hand-built userland
  turns into a worse distribution over time.

Alpine satisfies both. Its userland is busybox. Its libc is musl. Its
addition is apk and a package archive, and apk pins a repository
snapshot and a package set, thus a rebuild gives the same bytes. The
canonical image is Alpine with nothing installed, which is the present
base plus a linker that nothing uses. Every other image is the same
base plus a package list.

The move from the built busybox to the Alpine base is a build change.
No flavor changes its contents, and the probes see the same init.

## The node image

The node runs an inference runtime. Its image holds three programs on
the base:

- the init, as today;
- the witness (`TIME-MODEL.md`), one static binary that pairs `rdtsc`
  with CLOCK_MONOTONIC per CPU;
- the runtime, one static binary with its math library and thread
  pool, built for a named instruction set.

The runtime's instruction set is recorded in the manifest with the
image. A thaw compares it against the host (`TIME-MODEL.md`,
"Hardware identity"). The weights are the largest decision of this
image and are not made here: on the disk they enter RAM through the
page cache and the frozen image holds them twice; in RAM alone the
rootfs is the model and every branch copies it. The decision lands
with the first runtime.

## The worker image

The worker runs tools: a compiler, an interpreter, a version control
client, a network client. Its image is the base plus a package list,
pinned. Its output crosses the membrane and is judged there, thus the
worker is not part of the determinism claim and may carry shared
libraries.

## From a Containerfile

A task arrives as a Containerfile or as an OCI image. The build verb
turns it into a rootfs outside the VMM, without root and without a
daemon:

1. `buildah build` from the Containerfile, rootless. An OCI image
   skips this step.
2. Export the image's filesystem to a directory.
3. `mke2fs -d <dir>` writes the ext4 from the directory. No loop
   device, no mount, no root.
4. Inject the init. A container image has no PID 1. cella adds one
   static shim that mounts proc and sys, configures the interface from
   the kernel command line, runs the image's entrypoint or the mailbox
   loop, and reaps children.

The output is a golden the same as any other, with the image digest
and the shim's hash in the manifest. A machine created from it cites
the exact image it came from, and a twin cites the same one.

The shim is the one file cella owns inside a task image. It is static,
it is small enough to read, and its hash is pinned with the image.

## Inside a machine

The build above runs on the host. The same verb can run inside a
worker machine. Every package fetch is then an egress operation:
parked, decided, recorded in the ledger by name, address, and byte
count, in order. The ledger is the lockfile of that build, produced by
the membrane and not by the build tool. A build is reproducible when a
twin of the build machine, given the same decisions, writes the same
bytes.

This form needs the worker image and the appliance. The rootless
buildah form needs neither and ships first.

## The hosting image

A machine that hosts machines carries the inner goldens inside its
own rootfs, and it keeps its machine home on tmpfs, because the inner
RAM file is larger than the root disk. The inner machine therefore
lives in the outer machine's RAM. An outer freeze captures it, and a
host reboot is crossed by that freeze. No verb reboots a guest kernel,
thus nothing clears the tmpfs.

This is the ruling for now: RAM is the vessel, and the recursion
stays at three layers. The cost is RAM that compounds with depth and
with the size of the inner machine. When the depth or the inner size
outgrows it, a dedicated subvolume mount for the machine home enters
the picture. Not before.

## What the VMM knows

An ext4 on a virtio-blk device. Nothing else. No image format, no
manifest, no package database enters the VMM, and no build step runs
with a capability.
