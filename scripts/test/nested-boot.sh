#!/usr/bin/env bash
# Smoke test: cella hosts cella. The outer guest boots
# dist/bzImage-nested with dist/rootfs-nested.ext4, and its init runs
# the inner cella with the canonical assets. PASS: the inner guest's
# own init line ("cella-rootfs: init running") appears on the outer
# serial console. The outer init prints "cella-nested:" lines only,
# thus a "cella-rootfs:" line can come from the inner guest alone.
#
# The outer guest runs without a TAP: the test needs no network on
# either layer. On a nested development host this test is one layer
# deeper than the host itself; a timeout there is a SKIP, not a FAIL,
# and bare metal is the reference.
set -euo pipefail

cd "$(dirname "$0")/../.."
BIN=target/release/cella
KERNEL=dist/bzImage-nested
DISK=dist/rootfs-nested.ext4
TIMEOUT=90

[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build"; exit 0; }
[ -f "$KERNEL" ] && [ -f "$DISK" ] || { echo "SKIP: nested assets missing -- run: make dist-nested"; exit 0; }
if ! [ -r /dev/kvm ] || ! [ -w /dev/kvm ]; then
    echo "SKIP: no read and write access to /dev/kvm"
    exit 0
fi

TMP=$(mktemp -d /tmp/cella-nested-boot.XXXXXX)
trap 'kill $PID 2>/dev/null || true; wait $PID 2>/dev/null || true' EXIT
mkdir -p "$TMP/state"
cp "$DISK" "$TMP/disk.img"

CMDLINE="$("$BIN" --print-default-cmdline) root=/dev/vda rw virtio_mmio.device=4K@0xd0000000:5"
"$BIN" --state-dir "$TMP/state" --kernel "$KERNEL" --disk "$TMP/disk.img" \
    --mem-mb 256 --cmdline "$CMDLINE" >"$TMP/serial.log" 2>"$TMP/cella.err" &
PID=$!

echo "--- nested boot: waiting up to ${TIMEOUT}s for the inner guest ---"
for _ in $(seq "$TIMEOUT"); do
    if grep -aq "cella-rootfs: init running" "$TMP/serial.log"; then
        echo "PASS: the inner guest booted under the outer cella"
        grep -a "cella-nested:\|cella-rootfs: init running" "$TMP/serial.log" | head -6
        rm -rf "$TMP"
        exit 0
    fi
    if grep -aq "cella-nested: FAIL" "$TMP/serial.log"; then
        break
    fi
    if ! kill -0 "$PID" 2>/dev/null; then
        break
    fi
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
echo "FAIL: no inner heartbeat within ${TIMEOUT}s"
echo "(logs kept: $TMP)"
exit 1
