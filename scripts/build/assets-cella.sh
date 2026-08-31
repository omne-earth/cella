#!/usr/bin/env bash
# Builds dist/rootfs-cella.ext4: the canonical busybox root with the
# interactive init of rootfs-cella.sh. The canonical rootfs stays
# untouched. Requires the canonical root tree in target/ (make dist).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="$HERE/dist"

if [ ! -f /run/.toolboxenv ]; then
    command -v toolbox &>/dev/null || { echo "cella: 'toolbox' not found -- run: make init" >&2; exit 1; }
    exec toolbox run -c cella-build "$HERE/scripts/build/assets-cella.sh"
fi

ROOTDIR="$HERE/target/rootfs-build/root"
[ -d "$ROOTDIR" ] || { echo "cella: canonical rootfs tree missing -- run: make distclean-rootfs && make dist" >&2; exit 1; }

# A real bash for the interactive image, static, from pinned source.
# The canonical rootfs stays busybox-only.
BASH_VERSION="${GUEST_BASH_VERSION:-5.3}"
BBUILD="$HERE/target/rootfs-build/bash-$BASH_VERSION"
if [ ! -x "$BBUILD/bash" ]; then
    TARBALL="$HERE/target/rootfs-build/bash-$BASH_VERSION.tar.gz"
    URL="https://ftp.gnu.org/gnu/bash/bash-$BASH_VERSION.tar.gz"
    if [ ! -d "$BBUILD" ]; then
        echo "cella: downloading bash $BASH_VERSION source ($URL)"
        curl -fL --progress-bar --retry 5 --retry-delay 2 --retry-all-errors -C - -o "$TARBALL" "$URL"
        tar -xf "$TARBALL" -C "$HERE/target/rootfs-build"
        rm -f "$TARBALL"
    fi
    cd "$BBUILD"
    echo "cella: building a static bash"
    ./configure --enable-static-link --without-bash-malloc >/dev/null
    make -j"$(nproc)" >/dev/null
    cd "$HERE"
fi

CROOT="$HERE/target/rootfs-build/root-cella"
rm -rf "$CROOT"
cp -a "$ROOTDIR" "$CROOT"
install -m 0755 "$BBUILD/bash" "$CROOT/bin/bash"
install -m 0755 "$HERE/scripts/build/rootfs-cella.sh" "$CROOT/sbin/init"
IMG="$HERE/target/rootfs-build/rootfs-cella.ext4"
rm -f "$IMG"
dd if=/dev/zero of="$IMG" bs=1M count=16 status=none
mkfs.ext4 -q -F -d "$CROOT" "$IMG"
cp "$IMG" "$OUT/rootfs-cella.ext4"
echo "cella: interactive rootfs built -> $OUT/rootfs-cella.ext4"
