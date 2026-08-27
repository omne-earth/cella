#!/usr/bin/env bash
# Boots the guest with a static IP on the TAP subnet and pings it from
# the host. Networking here is entirely in-kernel -- the ip= cmdline
# param is applied by CONFIG_IP_PNP_STATIC before our /sbin/init even
# runs (see scripts/kernel-fragment.config / rootfs-init.sh), and ICMP
# replies need no userspace daemon either. So unlike a downloaded
# rootfs of unknown provenance, a FAIL here is a real signal about
# cella's virtio-net TX/RX path, not a guest-userspace question.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$HERE/.."
BIN="${CELLA_BIN:-$ROOT/target/release/cella}"
KERNEL="${CELLA_TEST_KERNEL:-$ROOT/dist/bzImage}"
DISK="${CELLA_TEST_DISK:-$ROOT/dist/rootfs.ext4}"
TAP="${CELLA_TEST_TAP:-tap0}"
HOST_IP="${CELLA_TEST_HOST_IP:-192.168.200.1}"
GUEST_IP="${CELLA_TEST_GUEST_IP:-192.168.200.2}"
BOOT_WAIT_SECS="${CELLA_BOOT_WAIT:-10}"

if [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
    echo "SKIP: no rw access to /dev/kvm on this machine"
    exit 0
fi
if [ ! -x "$BIN" ]; then
    echo "FAIL: $BIN not built (run: make build)"
    exit 1
fi
if [ ! -f "$KERNEL" ] || [ ! -f "$DISK" ]; then
    echo "SKIP: test assets not found -- run: make dist"
    exit 0
fi
if ! ip addr show "$TAP" 2>/dev/null | grep -q "$HOST_IP"; then
    echo "SKIP: $TAP is not configured with $HOST_IP -- run: sudo scripts/make_tap.sh $TAP $HOST_IP/24"
    exit 0
fi

TMP="$(mktemp -d)"
STATE_DIR="$TMP/state"
mkdir -p "$STATE_DIR"
cp "$DISK" "$TMP/disk.img"
trap 'kill %1 2>/dev/null; wait 2>/dev/null; rm -rf "$TMP"' EXIT

"$BIN" \
    --state-dir "$STATE_DIR" \
    --kernel "$KERNEL" \
    --disk "$TMP/disk.img" \
    --tap "$TAP" \
    --mem-mb 128 \
    --cmdline "console=ttyS0 reboot=k panic=1 pci=off virtio_mmio.device=4K@0xd0000000:5 virtio_mmio.device=4K@0xd0001000:6 ip=${GUEST_IP}::${HOST_IP}:255.255.255.0::eth0:off" \
    >"$TMP/boot.log" 2>"$TMP/boot.err" &
PID=$!

echo "cella: waiting ${BOOT_WAIT_SECS}s for the guest to bring up networking"
sleep "$BOOT_WAIT_SECS"

if ! kill -0 "$PID" 2>/dev/null; then
    echo "FAIL: process exited during boot"
    cat "$TMP/boot.err"
    exit 1
fi

if ping -c 3 -W 2 "$GUEST_IP" >"$TMP/ping.log" 2>&1; then
    echo "PASS: guest at $GUEST_IP answered ICMP over $TAP"
    exit_code=0
else
    echo "FAIL: no ICMP reply from $GUEST_IP"
    cat "$TMP/ping.log"
    exit_code=1
fi

kill "$PID" 2>/dev/null
wait "$PID" 2>/dev/null
exit "$exit_code"
