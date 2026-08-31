#!/usr/bin/env bash
# Builds the nested test assets: dist/bzImage-nested and
# dist/rootfs-nested.ext4. The canonical dist/bzImage and
# dist/rootfs.ext4 are proof artifacts, and this script does not touch
# them or their build trees' configuration. The nested kernel builds
# from the same pinned source into a separate object directory. The
# nested rootfs is the canonical busybox root plus /opt: the static
# cella binary and the canonical assets for the inner guest.
#
# Requires: the canonical build trees in target/ (make dist) and the
# static binary (make build-static). The bare-metal machine receives
# the finished artifacts through the shared copy and does not build.
set -euo pipefail

KERNEL_VERSION="${KERNEL_VERSION:-7.2.2}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="$HERE/dist"

if [ -f "$OUT/bzImage-nested" ] && [ -f "$OUT/rootfs-nested.ext4" ] && [ -f "$OUT/rootfs-inception.ext4" ]; then
    echo "cella: nested and inception artifacts already present, skipping"
    exit 0
fi

if [ ! -f /run/.toolboxenv ]; then
    command -v toolbox &>/dev/null || { echo "cella: 'toolbox' not found -- run: make init" >&2; exit 1; }
    exec toolbox run -c cella-build env KERNEL_VERSION="$KERNEL_VERSION" "$HERE/scripts/build/assets-nested.sh"
fi

STATIC_BIN="$HERE/target/x86_64-unknown-linux-gnu/release/cella"
[ -f "$STATIC_BIN" ] || { echo "cella: $STATIC_BIN missing -- run: make build-static" >&2; exit 1; }
[ -f "$OUT/bzImage" ] && [ -f "$OUT/rootfs.ext4" ] || { echo "cella: canonical dist missing -- run: make dist" >&2; exit 1; }

# === nested kernel ===
if [ -f "$OUT/bzImage-nested" ]; then
    echo "cella: $OUT/bzImage-nested already present, skipping"
else
    SRC_DIR="$HERE/target/kernel-build/linux-$KERNEL_VERSION"
    [ -d "$SRC_DIR" ] || { echo "cella: kernel source missing -- run: make dist first" >&2; exit 1; }
    # The canonical build runs in its own tree, and that tree is not
    # clean. A kernel build with O= needs a clean tree. The nested
    # build therefore gets a copy of the source, and the canonical
    # tree stays as the canonical cache.
    NSRC="$HERE/target/kernel-build/linux-$KERNEL_VERSION-nested"
    if [ ! -d "$NSRC" ]; then
        echo "cella: copying the kernel source for the nested build"
        cp -a "$SRC_DIR" "$NSRC"
        make -C "$NSRC" mrproper >/dev/null
    fi
    cd "$NSRC"
    echo "cella: configuring the nested kernel (defconfig + fragment + nested fragment)"
    make x86_64_defconfig >/dev/null
    scripts/kconfig/merge_config.sh -m .config \
        "$HERE/scripts/build/kernel-fragment.config" \
        "$HERE/scripts/build/kernel-fragment-nested.config" >/dev/null
    make olddefconfig >/dev/null
    # The nested fragment must survive the resolution. A silent loss of
    # CONFIG_KVM gives an outer guest with no /dev/kvm, and the failure
    # then appears one layer away from its cause.
    for sym in CONFIG_KVM CONFIG_SECCOMP_FILTER CONFIG_DEVTMPFS_MOUNT; do
        grep -q "^$sym=y" .config \
            || { echo "cella: FAIL: $sym did not survive in the nested kernel config" >&2; exit 1; }
    done
    grep -qE "^CONFIG_KVM_INTEL=y|^CONFIG_KVM_AMD=y" .config \
        || { echo "cella: FAIL: no vendor KVM module in the nested kernel config" >&2; exit 1; }
    echo "cella: building bzImage-nested (-j$(nproc))"
    make -j"$(nproc)" bzImage >/dev/null
    cp arch/x86/boot/bzImage "$OUT/bzImage-nested"
    echo "cella: nested kernel built -> $OUT/bzImage-nested"
fi

# === nested rootfs ===
ROOTDIR="$HERE/target/rootfs-build/root"
[ -d "$ROOTDIR" ] || { echo "cella: canonical rootfs tree missing -- run: make distclean-rootfs && make dist" >&2; exit 1; }
NROOT="$HERE/target/rootfs-build/root-nested"
rm -rf "$NROOT"
cp -a "$ROOTDIR" "$NROOT"
mkdir -p "$NROOT/opt" "$NROOT/tmp"
install -m 0755 "$STATIC_BIN" "$NROOT/opt/cella"
cp "$OUT/bzImage" "$NROOT/opt/bzImage"
cp "$OUT/rootfs.ext4" "$NROOT/opt/rootfs.ext4"
install -m 0755 "$HERE/scripts/build/rootfs-nested.sh" "$NROOT/sbin/init"

IMG="$HERE/target/rootfs-build/rootfs-nested.ext4"
rm -f "$IMG"
dd if=/dev/zero of="$IMG" bs=1M count=64 status=none
mkfs.ext4 -q -F -d "$NROOT" "$IMG"
cp "$IMG" "$OUT/rootfs-nested.ext4"
echo "cella: nested rootfs built -> $OUT/rootfs-nested.ext4"

# === inception rootfs ===
# The nested root plus the static probe, with the inception init. The
# probe freezes and thaws the inner guest and prints the verdict.
PROBE_BIN="$HERE/probes/freeze-thaw-clock/target/x86_64-unknown-linux-gnu/release/freeze-thaw-clock-probe"
[ -f "$PROBE_BIN" ] || { echo "cella: $PROBE_BIN missing -- run: make build-static" >&2; exit 1; }
IROOT="$HERE/target/rootfs-build/root-inception"
rm -rf "$IROOT"
cp -a "$NROOT" "$IROOT"
install -m 0755 "$PROBE_BIN" "$IROOT/opt/freeze-thaw-clock-probe"
install -m 0755 "$HERE/scripts/build/rootfs-inception.sh" "$IROOT/sbin/init"
IIMG="$HERE/target/rootfs-build/rootfs-inception.ext4"
rm -f "$IIMG"
dd if=/dev/zero of="$IIMG" bs=1M count=64 status=none
mkfs.ext4 -q -F -d "$IROOT" "$IIMG"
cp "$IIMG" "$OUT/rootfs-inception.ext4"
echo "cella: inception rootfs built -> $OUT/rootfs-inception.ext4"
