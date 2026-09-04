#!/usr/bin/env bash
# The field install, explicitly release: the binary has no console --
# no console.log, no console.sock, no enter. One-time host setup for
# Fedora: installs the system packages the build and the other
# scripts/ depend on, then checks /dev/kvm access. Every step is
# idempotent, so it's safe to re-run after e.g. a fresh install or a
# new machine. The lab flavor never installs (ruled 2026-09-02):
# the lab is the checkout, target/smoke/* is its home, and this
# field install is the only install.
#
# Usage: scripts/setup/install.sh
set -euo pipefail

if ! command -v dnf &>/dev/null; then
    echo "cella: install.sh only supports Fedora (dnf not found)" >&2
    exit 1
fi

# rust/cargo/rustfmt/clippy: make build/debug/check/lint/fmt
# bubblewrap: scripts/jail.sh
# curl: scripts/build/assets.sh (kernel/busybox source fetch)
# iputils: scripts/test/net.sh (ping)
# python3: make lines, scripts/build/assets.sh (kernel.org releases.json)
# podman/toolbox: the build verb provisions the cella-build toolbox
# (gcc, bison, flex, e2fsprogs, ...) lives inside the cella-build
# toolbox container provisioned from there, never on the host itself.
PACKAGES=(
    rust cargo rustfmt clippy
    bubblewrap
    iproute protobuf-compiler
    curl
    iputils
    python3
    podman toolbox
)

echo "cella: installing packages: ${PACKAGES[*]}"
sudo dnf install -y "${PACKAGES[@]}"

# The identity slice (1.6.14a): each machine runs as its own
# sub-user; the spawn maps it from a range delegated to $USER, and
# grants the machine directory by POSIX ACL. shadow-utils carries
# newuidmap/newgidmap; acl carries setfacl.
sudo dnf install -y shadow-utils acl
if ! grep -q "^$USER:" /etc/subuid 2>/dev/null || ! grep -q "^$USER:" /etc/subgid 2>/dev/null; then
    echo "cella: delegating sub-id range 524288-589823 to $USER"
    sudo usermod --add-subuids 524288-589823 --add-subgids 524288-589823 "$USER"
else
    echo "cella: sub-id range already delegated to $USER"
fi

if [ ! -e /dev/kvm ]; then
    echo "cella: /dev/kvm not present -- enable virtualization in BIOS/UEFI" \
         "and confirm kvm_intel/kvm_amd is loaded (lsmod | grep kvm)" >&2
elif [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
    echo "cella: /dev/kvm is rw for $USER"
elif getent group kvm &>/dev/null; then
    echo "cella: /dev/kvm exists but isn't rw for $USER -- adding $USER to the kvm group"
    sudo usermod -aG kvm "$USER"
    echo "cella: log out/in (or run 'newgrp kvm') for the new group to take effect"
else
    echo "cella: /dev/kvm exists but isn't rw for $USER, and there's no kvm group" \
         "-- check its owner/permissions manually" >&2
fi



# The binaries. A release build lands in ~/.cella/bin -- inside
# the one artifact home, whose traversal the spawn already grants
# each machine's sub-uid by ACL, thus bwrap can resolve the VMM
# binary on any host, whatever the mode of $HOME (found on bare
# metal, 2026-09-03: a 0700 home refused the sub-uid at bwrap's
# source resolution). PATH gains the directory when absent.
# make install calls this script.
# The workspace's own crates rebuild unconditionally: cargo trusts
# mtimes, and a synced checkout (rsync keeps source times) under an
# older target/ ships stale binaries with a 0.04s "Finished". The
# dependency cache stays; only cella's crates are cleaned.
for crate in $(sed -n 's/^name = "\(cella[a-z-]*\)"/\1/p' crates/*/Cargo.toml | sort -u); do
    cargo clean --release -p "$crate" 2>/dev/null || true
done
cargo build --release
# Every persona is its own binary since the split (1.6.13): the
# shim routes, the personas own their verbs, and the shakedown
# confines each inode. No binary carries a capability (1.6.14e).
for name in cella cella-machine cella-vmm cella-gateway cella-universe cella-build cella-doctor cella-network cella-probe; do
    install -D -m 0755 "target/release/$name" "$HOME/.cella/bin/$name"
done
# The build inputs (kernel fragments, init scripts) install beside
# the workshop: an installed binary builds the goldens from any
# directory. The manifests digest the inputs, thus a stale copy
# rebuilds and names itself.
for f in scripts/build/*.config scripts/build/rootfs*.sh; do
    install -D -m 0644 "$f" "$HOME/.cella/build/scripts/$(basename "$f")"
done

# The network is rootless (1.6.14e): no capability, no unit, no
# host object -- this install creates none. It also removes none:
# a tap or table left by an older install is removed by hand, by
# the one who knows it is cella's (ruled 2026-09-03; a name is not
# ownership, and deleting by name is what docker does to others).
NEXT="cella doctor check"
case ":$PATH:" in
*":$HOME/.cella/bin:"*) : ;;
*)
    if ! grep -qs '\.cella/bin' "$HOME/.bashrc"; then
        printf '\nexport PATH="$HOME/.cella/bin:$PATH"\n' >> "$HOME/.bashrc"
        echo "cella: added ~/.cella/bin to PATH in ~/.bashrc"
    fi
    NEXT="source ~/.bashrc && cella doctor check"
    ;;
esac
echo "cella: installed -> $HOME/.cella/bin/cella"
echo "cella: the thin CLIs installed -- run: $NEXT"
