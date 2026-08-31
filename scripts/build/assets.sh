#!/usr/bin/env bash
# Builds cella's test assets from real upstream source: a minimal
# busybox rootfs and a minimal x86_64 bzImage kernel. Replaces the old
# fetch-assets.sh, which downloaded Firecracker's own hello-world
# kernel+rootfs -- Firecracker's kernel is an uncompressed ELF vmlinux
# (their loader boots ELF, never bzImage; cella's loader only speaks
# bzImage, see README), and pairing their arbitrary rootfs with a
# from-scratch kernel/init risked mismatches we couldn't verify.
# Building both from source keeps them provably matched to what cella
# actually boots: virtio-mmio/blk/net + 8250 serial, nothing from a
# module or an initrd.
#
# Both builds happen inside the 'cella-build' toolbox (see
# scripts/build/toolbox.sh / `make .toolbox`) -- this script re-execs
# itself there. The host itself never needs a build toolchain, only
# `toolbox`.
#
# Output: dist/bzImage, dist/rootfs.ext4.
#
# Caching: source + build trees live in target/rootfs-build/ and
# target/kernel-build/ (gitignored via /target); delete either to
# force a rebuild. Each output file is skipped individually if already
# present -- rm just the one you want rebuilt.
set -euo pipefail

# Versions come from the Makefile (KERNEL_VERSION / BUSYBOX_VERSION,
# overridable in .env), with the same defaults repeated here so the
# script still works when run directly. Pinned rather than resolved from
# kernel.org at build time: a floating version makes `make dist`
# irreproducible and can move the kernel out from under a measurement.
KERNEL_VERSION="${KERNEL_VERSION:-7.2.2}"
BUSYBOX_VERSION="${BUSYBOX_VERSION:-1.37.0}"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="$HERE/dist"
mkdir -p "$OUT"

if [ -f "$OUT/bzImage" ] && [ -f "$OUT/rootfs.ext4" ]; then
    echo "cella: $OUT/{bzImage,rootfs.ext4} already present, skipping (rm one, or rm -rf target/*-build, to rebuild)"
    exit 0
fi

if [ ! -f /run/.toolboxenv ]; then
    command -v toolbox &>/dev/null || { echo "cella: 'toolbox' not found -- run: make init" >&2; exit 1; }
    [ -f "$HERE/.toolbox" ] || { echo "cella: build toolbox not set up -- run: make init (or: make .toolbox)" >&2; exit 1; }
    echo "cella: entering the cella-build toolbox to build assets"
    # `env VAR=...` rather than relying on inheritance: the re-exec goes
    # through toolbox/podman, which does not carry this shell's
    # environment into the container.
    exec toolbox run -c cella-build env \
        KERNEL_VERSION="$KERNEL_VERSION" BUSYBOX_VERSION="$BUSYBOX_VERSION" \
        "$HERE/scripts/build/assets.sh"
fi

# --- from here on we're inside the toolbox container ---

# === rootfs ===
if [ -f "$OUT/rootfs.ext4" ]; then
    echo "cella: $OUT/rootfs.ext4 already present, skipping"
else
    RBUILD="$HERE/target/rootfs-build"
    ROOTDIR="$RBUILD/root"
    SRC_DIR="$RBUILD/busybox-$BUSYBOX_VERSION"
    TARBALL="$RBUILD/busybox-$BUSYBOX_VERSION.tar.bz2"
    URL="https://busybox.net/downloads/busybox-$BUSYBOX_VERSION.tar.bz2"
    mkdir -p "$RBUILD"

    if [ -d "$SRC_DIR" ]; then
        echo "cella: busybox $BUSYBOX_VERSION source already present in $SRC_DIR, skipping download"
    else
        echo "cella: downloading busybox $BUSYBOX_VERSION source ($URL)"
        curl -fL --progress-bar --retry 5 --retry-delay 2 --retry-all-errors -C - -o "$TARBALL" "$URL"
        echo "cella: extracting"
        tar -xf "$TARBALL" -C "$RBUILD"
        rm -f "$TARBALL"
    fi

    cd "$SRC_DIR"
    echo "cella: configuring busybox (defconfig + scripts/build/busybox-fragment.config)"
    # busybox's own scripts/kconfig/ doesn't ship merge_config.sh (that's
    # a Linux-kernel-specific addition), so apply the fragment directly.
    # oldconfig's parser keeps the *first* definition of a symbol in
    # .config, so any symbol the fragment overrides has to be deleted
    # from defconfig's output first, or the override is silently ignored.
    make defconfig >/dev/null
    while read -r sym; do
        [ -n "$sym" ] && sed -i "/^${sym}=/d; /^# ${sym} is not set\$/d" .config
    done < <(grep -oE '^(CONFIG_[A-Z0-9_]+=|# CONFIG_[A-Z0-9_]+ is not set)' "$HERE/scripts/build/busybox-fragment.config" | grep -oE 'CONFIG_[A-Z0-9_]+')
    cat "$HERE/scripts/build/busybox-fragment.config" >>.config
    make oldconfig >/dev/null </dev/null

    echo "cella: building a static busybox (-j$(nproc))"
    make -j"$(nproc)" busybox >/dev/null

    echo "cella: assembling the rootfs"
    rm -rf "$ROOTDIR"
    mkdir -p "$ROOTDIR"/bin "$ROOTDIR"/sbin "$ROOTDIR"/proc "$ROOTDIR"/sys "$ROOTDIR"/dev
    cp busybox "$ROOTDIR/bin/busybox"
    # Hardlinks, not -s (symlinks): busybox --install writes symlink
    # targets as the *absolute path it was invoked with*, which here is
    # a host build path that doesn't exist inside the guest -- every
    # applet (including /bin/sh) would be a dangling symlink. Hardlinks
    # share the inode directly, no path to resolve, and cost nothing
    # extra on disk since they share blocks.
    "$ROOTDIR/bin/busybox" --install "$ROOTDIR/bin"
    install -m 0755 "$HERE/scripts/build/rootfs.sh" "$ROOTDIR/sbin/init"

    IMG="$RBUILD/rootfs.ext4"
    rm -f "$IMG"
    dd if=/dev/zero of="$IMG" bs=1M count=16 status=none
    mkfs.ext4 -q -F -d "$ROOTDIR" "$IMG"
    cp "$IMG" "$OUT/rootfs.ext4"
    echo "cella: rootfs built -> $OUT/rootfs.ext4"
fi

# === kernel ===
if [ -f "$OUT/bzImage" ]; then
    echo "cella: $OUT/bzImage already present, skipping (rm it, or rm -rf target/kernel-build, to rebuild)"
    exit 0
fi

KBUILD="$HERE/target/kernel-build"
mkdir -p "$KBUILD"

VERSION="$KERNEL_VERSION"
MAJOR="${VERSION%%.*}"
SRC_DIR="$KBUILD/linux-$VERSION"
TARBALL="$KBUILD/linux-$VERSION.tar.xz"
URL="https://cdn.kernel.org/pub/linux/kernel/v${MAJOR}.x/linux-${VERSION}.tar.xz"

if [ -d "$SRC_DIR" ]; then
    echo "cella: kernel source $VERSION already present in $SRC_DIR, skipping download"
else
    echo "cella: downloading kernel $VERSION source ($URL)"
    # -C - resumes a partial file across retries; kernel.org's CDN
    # occasionally drops the HTTP/2 stream mid-transfer on a large file.
    curl -fL --progress-bar --retry 5 --retry-delay 2 --retry-all-errors -C - -o "$TARBALL" "$URL"
    echo "cella: extracting"
    tar -xf "$TARBALL" -C "$KBUILD"
    rm -f "$TARBALL"
fi

cd "$SRC_DIR"
echo "cella: configuring (x86_64_defconfig + scripts/build/kernel-fragment.config)"
make x86_64_defconfig >/dev/null
scripts/kconfig/merge_config.sh -m .config "$HERE/scripts/build/kernel-fragment.config" >/dev/null
make olddefconfig >/dev/null

# Verify the resolved config before spending four minutes compiling it.
# kconfig overrules a fragment silently, so "the fragment says X" and
# "the kernel we are about to build does X" are different claims -- and
# the failure mode that matters (cutting CONFIG_TTY, hence the serial
# console, which is the only channel out of the guest) produces a kernel
# that boots and says nothing at all. Cheap here, expensive to diagnose
# later. Runs in-place: we are already inside the toolbox, so the
# script's own re-exec guard is a no-op.
"$HERE/scripts/build/kernel-config-check.sh"

echo "cella: building bzImage (-j$(nproc))"
make -j"$(nproc)" bzImage

cp arch/x86/boot/bzImage "$OUT/bzImage"
echo "cella: kernel $VERSION built -> $OUT/bzImage"
