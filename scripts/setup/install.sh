#!/usr/bin/env bash
# One-time host setup for Fedora: installs the system packages the build
# and the other scripts/ depend on, then checks /dev/kvm access. Every
# step is idempotent, so it's safe to re-run after e.g. a fresh install
# or a new machine.
#
# Usage: scripts/setup/install.sh
set -euo pipefail

if ! command -v dnf &>/dev/null; then
    echo "cella: install.sh only supports Fedora (dnf not found)" >&2
    exit 1
fi

# rust/cargo/rustfmt/clippy: make build/debug/check/lint/fmt
# bubblewrap: scripts/jail.sh
# nftables/iproute: scripts/setup/tap.sh
# curl: scripts/build/assets.sh (kernel/busybox source fetch)
# iputils: scripts/test/net.sh (ping)
# python3: make lines, scripts/build/assets.sh (kernel.org releases.json)
# podman/toolbox: the build verb provisions the cella-build toolbox
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

# Docker hooks the forward chain with policy drop, and DOCKER-USER is
# its extension point. Without these rules a host with docker drops
# every forwarded guest packet, whatever the firewall zone says. A
# host without docker needs nothing here.
if sudo nft list chain ip filter DOCKER-USER &>/dev/null; then
    if [ "$(sudo nft list chain ip filter DOCKER-USER | grep -c 'tap\*')" = "0" ]; then
        sudo nft insert rule ip filter DOCKER-USER iifname '"tap*"' accept
        sudo nft insert rule ip filter DOCKER-USER oifname '"tap*"' accept
        echo "cella: tap forwarding allowed through DOCKER-USER"
    else
        echo "cella: DOCKER-USER already forwards the taps"
    fi
fi

cat <<EOT
cella: install done.

Next, from any directory (no make needed from here on):

  1. The network, once per host boot -- no sudo: cella-network
     carries cap_net_admin from this install (the pool feeds
     `cella create --net auto`, and the manifests are the
     allocation record):
       cella-network setup --taps 4

  2. Prove the lifecycle end to end:
       cella selftest

  3. A machine of your own:
       cella create m1 --net tap0
       cella start m1
       cella enter m1        (Ctrl-] detaches; cella freeze / thaw / destroy m1)
EOT

# The binary. A release build lands in ~/.local/bin, and PATH gains
# the directory when absent (moved here from the make install target;
# make install calls this script).
cargo build --release
install -D -m 0755 target/release/cella "$HOME/.local/bin/cella"
# The thin CLIs. cella-network is the one CAP_NET_ADMIN holder: the
# file capability makes every later invocation sudo-free -- this
# setcap is the root moment, once, here.
install -D -m 0755 target/release/cella-network "$HOME/.local/bin/cella-network"
sudo setcap 'cap_net_admin+eip' "$HOME/.local/bin/cella-network"
echo "cella: cella-network installed with cap_net_admin"
sudo setcap 'cap_net_admin+eip' target/release/cella-network 2>/dev/null || true
case ":$PATH:" in
*":$HOME/.local/bin:"*) echo "cella: ~/.local/bin is already on PATH" ;;
*)
    if ! grep -qs '\.local/bin' "$HOME/.bashrc"; then
        printf '\nexport PATH="$HOME/.local/bin:$PATH"\n' >> "$HOME/.bashrc"
        echo "cella: added ~/.local/bin to PATH in ~/.bashrc -- open a new shell"
    else
        echo "cella: ~/.bashrc already mentions ~/.local/bin -- open a new shell"
    fi
    ;;
esac
echo "cella: installed -> $HOME/.local/bin/cella"
