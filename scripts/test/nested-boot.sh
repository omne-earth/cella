#!/usr/bin/env bash
# Smoke test: cella hosts cella. Three variants, selected by $1:
#
#   airgapped  no network on either layer. The outer guest runs
#              without a TAP, and the inner guest gets the block
#              device only.
#   hybrid     the outer guest is on the network (TAP + ip=), and the
#              inner guest stays airgapped. PASS needs the inner boot
#              and an ICMP reply from the outer guest.
#   www        both layers are networked. The inner cella gets a TAP
#              inside the outer guest (see rootfs-nested.sh), and the
#              outer init pings the inner guest. PASS needs the inner
#              boot, the outer ICMP reply, and the inner ICMP reply.
#
# The outer init prints "cella-nested:" lines only, thus a
# "cella-rootfs:" line can come from the inner guest alone. On a
# nested development host this test is one layer deeper than the host
# itself; a missing /dev/kvm in the outer guest is a SKIP.
set -euo pipefail

MODE="${1:-airgapped}"
case "$MODE" in airgapped|hybrid|www) ;; *) echo "usage: $0 airgapped|hybrid|www" >&2; exit 2;; esac

cd "$(dirname "$0")/../.."
BIN=target/release/cella
CH="${CELLA_HOME:-$HOME/.cella}"
KERNEL="$CH/kernel/nested/bzImage"
DISK="$CH/rootfs/nested/rootfs.ext4"
TAP="${CELLA_TEST_TAP:-tap0}"
HOST_IP="${CELLA_TAP_CIDR:-192.168.200.1/24}"; HOST_IP="${HOST_IP%%/*}"
OUTER_IP="${CELLA_TEST_GUEST_IP:-192.168.200.2}"
TIMEOUT=120

[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build"; exit 0; }
[ -f "$KERNEL" ] && [ -f "$DISK" ] || { echo "SKIP: nested assets missing -- run: make golden-nested"; exit 0; }
"$BIN" doctor gate kvm || exit 0
if [ "$MODE" != airgapped ] && [ ! -e "/sys/class/net/$TAP" ]; then
    echo "SKIP: $TAP does not exist -- run: make setup-tap"
    exit 0
fi

TMP=$(mktemp -d /tmp/cella-nested-boot.XXXXXX)
trap 'kill $PID 2>/dev/null || true; wait $PID 2>/dev/null || true' EXIT
mkdir -p "$TMP/state"
cp "$DISK" "$TMP/disk.img"

BASE="$("$BIN" --print-default-cmdline)"
if [ "$MODE" = airgapped ]; then
    CMDLINE="$BASE root=/dev/vda rw virtio_mmio.device=4K@0xd0000000:5"
    "$BIN" --state-dir "$TMP/state" --kernel "$KERNEL" --disk "$TMP/disk.img" \
        --mem-mb 256 --cmdline "$CMDLINE" >"$TMP/serial.log" 2>"$TMP/cella.err" &
else
    CMDLINE="$BASE root=/dev/vda rw \
virtio_mmio.device=4K@0xd0000000:5 virtio_mmio.device=4K@0xd0001000:6 \
ip=${OUTER_IP}::${HOST_IP}:255.255.255.0::eth0:off"
    [ "$MODE" = www ] && CMDLINE="$CMDLINE cella_nested_mode=www"
    "$BIN" --state-dir "$TMP/state" --kernel "$KERNEL" --disk "$TMP/disk.img" \
        --tap "$TAP" --mem-mb 256 --cmdline "$CMDLINE" >"$TMP/serial.log" 2>"$TMP/cella.err" &
fi
PID=$!

echo "--- nested boot ($MODE): waiting up to ${TIMEOUT}s ---"
inner_boot=""
outer_icmp=""
inner_icmp=""
for _ in $(seq "$TIMEOUT"); do
    [ -z "$inner_boot" ] && grep -aq "cella-rootfs: init running" "$TMP/serial.log" && {
        inner_boot=yes; echo "ok: the inner guest booted"; }
    if [ "$MODE" != airgapped ] && [ -z "$outer_icmp" ]; then
        ping -c 1 -W 1 "$OUTER_IP" >/dev/null 2>&1 && {
            outer_icmp=yes; echo "ok: the outer guest answered ICMP at $OUTER_IP"; }
    fi
    [ "$MODE" = www ] && [ -z "$inner_icmp" ] && grep -aq "cella-nested: inner answered ICMP" "$TMP/serial.log" && {
        inner_icmp=yes; echo "ok: the inner guest answered ICMP inside the outer guest"; }
    done=yes
    [ -n "$inner_boot" ] || done=""
    [ "$MODE" = airgapped ] || [ -n "$outer_icmp" ] || done=""
    [ "$MODE" != www ] || [ -n "$inner_icmp" ] || done=""
    if [ -n "$done" ]; then
        echo "PASS ($MODE): all layers reported"
        grep -a "cella-nested:" "$TMP/serial.log" | head -6
        rm -rf "$TMP"
        exit 0
    fi
    grep -aq "cella-nested: FAIL" "$TMP/serial.log" && break
    kill -0 "$PID" 2>/dev/null || break
    sleep 1
done

echo "--- outer serial (last 25 lines) ---"
tail -n 25 "$TMP/serial.log" || true
echo "--- outer cella stderr (last 5 lines) ---"
tail -n 5 "$TMP/cella.err" || true
if grep -aqE "cella-nested: FAIL: no /dev/kvm" "$TMP/serial.log"; then
    echo "SKIP: the outer guest has no /dev/kvm -- this host does not offer nested virtualization one layer deeper"
    echo "(logs kept: $TMP)"
    exit 0
fi
echo "FAIL ($MODE): incomplete within ${TIMEOUT}s (inner boot=${inner_boot:-no} outer icmp=${outer_icmp:-n/a} inner icmp=${inner_icmp:-n/a})"
echo "(logs kept: $TMP)"
exit 1
