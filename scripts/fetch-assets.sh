#!/usr/bin/env bash
# Downloads Firecracker's public hello-world CI kernel + rootfs into
# ./assets/. These are real, small, and long-standing public test
# artifacts (same ones Firecracker's own getting-started guide uses) --
# the fastest way to get a real x86_64/virtio-mmio kernel to boot-test
# cella against, without building one from scratch.
#
# Caveat, stated plainly: this kernel was built for Firecracker's device
# layout and boot conventions, not verified against cella's loader.
# Booting it is the actual test -- see scripts/boot.sh.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$HERE/assets"
mkdir -p "$OUT"

KERNEL_URL="https://s3.amazonaws.com/spec.ccfc.min/img/hello/kernel/hello-vmlinux.bin"
ROOTFS_URL="https://s3.amazonaws.com/spec.ccfc.min/img/hello/fsfiles/hello-rootfs.ext4"

fetch() {
    local url="$1" dest="$2"
    if [ -f "$dest" ]; then
        echo "cella: $dest already present, skipping"
        return
    fi
    echo "cella: fetching $url"
    curl -fL --progress-bar -o "$dest.tmp" "$url"
    mv "$dest.tmp" "$dest"
}

fetch "$KERNEL_URL" "$OUT/hello-vmlinux.bin"
fetch "$ROOTFS_URL" "$OUT/hello-rootfs.ext4"

# The rootfs is shipped read-only-friendly upstream; copy it so repeated
# test runs (which mount it read-write for virtio-blk) don't accumulate
# state across runs.
cp -f "$OUT/hello-rootfs.ext4" "$OUT/test-rootfs.ext4"

echo "cella: assets ready in $OUT/"
echo "  kernel: $OUT/hello-vmlinux.bin"
echo "  disk:   $OUT/test-rootfs.ext4 (fresh copy of hello-rootfs.ext4)"
