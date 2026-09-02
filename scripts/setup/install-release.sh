#!/usr/bin/env bash
# The field install, explicitly release: the binary has no console --
# no console.log, no console.sock, no enter. One-time host setup for
# Fedora: installs the system packages the build and the other
# scripts/ depend on, then checks /dev/kvm access. Every step is
# idempotent, so it's safe to re-run after e.g. a fresh install or a
# new machine. The lab flavor is its own script (install-debug.sh),
# with no shared code between them.
#
# Usage: scripts/setup/install-release.sh
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
    nftables iproute protobuf-compiler
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

cat <<'EOT'
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
# the directory when absent. make install-release calls this script.
cargo build --release
# Every persona is its own binary since the split (1.6.13): the
# shim routes, the personas own their verbs, and the shakedown
# confines each inode. cella-network is the one CAP_NET_ADMIN
# holder -- the file capability makes every later invocation
# sudo-free; that setcap is the root moment, once, below.
for name in cella cella-machine cella-vmm cella-gateway cella-universe cella-build cella-doctor cella-network cella-probe; do
    install -D -m 0755 "target/release/$name" "$HOME/.local/bin/$name"
done
echo "cella: nine persona binaries installed (the shim routes; each owns its verbs)"
sudo setcap 'cap_net_admin+eip' "$HOME/.local/bin/cella-network"
echo "cella: cella-network installed with cap_net_admin"
sudo setcap 'cap_net_admin+eip' target/release/cella-network 2>/dev/null || true

# The tap pool at boot: TUNSETPERSIST is kernel-lifetime, thus a
# reboot deletes the pool. This oneshot recreates it. The unit runs
# as root at boot (file capabilities matter only for the runtime
# path); SUDO_UID pins the tap owner to the installing user.
# The pool at boot: a systemd USER unit with linger, not a system
# unit. A system service runs in init_t with no exec type on the
# installed binary, and SELinux denies both the exec from the
# user's home and the witness's append on the root book -- the
# fail-closed witness then kills the pool at every boot (the AVCs
# of 2026-09-02). The user manager runs the unit as the operator
# in the operator's own domain: the exec, the witness, and the
# file capability all hold. The shakedown's join revisits this
# when the installed binaries gain their SELinux types.
mkdir -p "$HOME/.config/systemd/user"
tee "$HOME/.config/systemd/user/cella-network.service" >/dev/null <<UNIT
[Unit]
Description=cella tap pool (cella-network setup)
After=network-online.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=%h/.local/bin/cella-network setup --taps 4

[Install]
WantedBy=default.target
UNIT
systemctl --user daemon-reload
systemctl --user enable cella-network.service >/dev/null 2>&1
sudo loginctl enable-linger "$USER"
echo "cella: cella-network.service enabled -- the pool survives a reboot"
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
