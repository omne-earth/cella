#!/usr/bin/env bash
# One-time host setup for Fedora: installs the system packages the build
# and the other scripts/ depend on, then checks /dev/kvm access. Every
# step is idempotent, so it's safe to re-run after e.g. a fresh install
# or a new machine.
#
# Usage: scripts/bootstrap.sh
set -euo pipefail

if ! command -v dnf &>/dev/null; then
    echo "cella: bootstrap.sh only supports Fedora (dnf not found)" >&2
    exit 1
fi

# rust/cargo/rustfmt/clippy: make build/debug/check/lint/fmt
# bubblewrap: scripts/jail.sh
# nftables/iproute: scripts/make_tap.sh
# curl: scripts/build/assets.sh (kernel/busybox source fetch)
# iputils: scripts/test/net.sh (ping)
# python3: make lines, scripts/build/assets.sh (kernel.org releases.json)
# podman/toolbox: make .toolbox -- the kernel/rootfs build toolchain
# (gcc, bison, flex, e2fsprogs, ...) lives inside the cella-build
# toolbox container provisioned from there, never on the host itself.
PACKAGES=(
    rust cargo rustfmt clippy
    bubblewrap
    nftables iproute
    curl
    iputils
    python3
    podman toolbox
)

echo "cella: installing packages: ${PACKAGES[*]}"
sudo dnf install -y "${PACKAGES[@]}"

if [ ! -e /dev/kvm ]; then
    echo "cella: /dev/kvm not present -- enable virtualization in BIOS/UEFI" \
         "and confirm kvm_intel/kvm_amd is loaded (lsmod | grep kvm)" >&2
elif [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
    echo "cella: /dev/kvm is rw for $USER, good"
elif getent group kvm &>/dev/null; then
    echo "cella: /dev/kvm exists but isn't rw for $USER -- adding $USER to the kvm group"
    sudo usermod -aG kvm "$USER"
    echo "cella: log out/in (or run 'newgrp kvm') for the new group to take effect"
else
    echo "cella: /dev/kvm exists but isn't rw for $USER, and there's no kvm group" \
         "-- check its owner/permissions manually" >&2
fi

echo "cella: bootstrap done -- next: make build, then make test"
