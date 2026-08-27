#!/usr/bin/env bash
# One-time (per boot) TAP setup. Run with sudo once; cella itself never
# needs CAP_NET_ADMIN or root -- it just opens the TAP that's already
# owned by your user.
#
# Usage: sudo scripts/setup/tap.sh [tap-name] [host-cidr]
#   sudo scripts/setup/tap.sh tap0 192.168.200.1/24

set -euo pipefail

TAP="${1:-tap0}"
HOST_CIDR="${2:-192.168.200.1/24}"
USER_NAME="${SUDO_USER:-$USER}"

if [ "$(id -u)" -ne 0 ]; then
    echo "run with sudo (creating a TAP device needs CAP_NET_ADMIN once)" >&2
    exit 1
fi

if ip link show "$TAP" &>/dev/null; then
    echo "cella: $TAP already exists, leaving it alone"
else
    ip tuntap add mode tap name "$TAP" user "$USER_NAME"
    ip addr add "$HOST_CIDR" dev "$TAP"
    ip link set "$TAP" up
    echo "cella: created $TAP owned by $USER_NAME, host side $HOST_CIDR"
fi

# NAT so the guest can reach the outside world. Adjust the egress
# interface if it's not the default route's device.
EGRESS_IF="$(ip route show default | awk '/default/ {print $5; exit}')"
if [ -n "$EGRESS_IF" ]; then
    nft list table inet cella_nat &>/dev/null || nft add table inet cella_nat
    nft list chain inet cella_nat postrouting &>/dev/null || \
        nft add chain inet cella_nat postrouting '{ type nat hook postrouting priority 100; }'
    nft add rule inet cella_nat postrouting oifname "$EGRESS_IF" masquerade 2>/dev/null || true
    echo "cella: NAT via $EGRESS_IF enabled (nft table inet cella_nat)"
else
    echo "cella: no default route found, skipping NAT setup" >&2
fi

echo "cella: guest should use a static IP in the $HOST_CIDR range, gateway ${HOST_CIDR%/*}"
