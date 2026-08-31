#!/usr/bin/env bash
# probe-inception: the freeze and thaw clock probe, one layer deep.
# The outer guest boots bzImage-nested with rootfs-inception.ext4, and
# its init runs the probe against an inner cella. The probe freezes
# and thaws the inner guest and prints its verdict, and the verdict
# arrives on the outer serial console. This test relays that verdict.
#
# The outer guest runs without a TAP, and the inner guest runs with
# the block device only. Depth counts the hypervisors between the
# metal and the inner guest: two on bare metal, three on a nested
# development host. A missing /dev/kvm in the outer guest is a SKIP,
# and bare metal is the reference.
set -euo pipefail

cd "$(dirname "$0")/../.."
BIN=target/release/cella
KERNEL=dist/bzImage-nested
DISK=dist/rootfs-inception.ext4
TIMEOUT=240
# The RAM of the outer guest. 384 MB starves the outer guest: its own
# reclaim then evicts the warmed mappings between the warming and the
# first heartbeat, and the inner thaw showed +30 ms at depth three.
# Measured boundary: 384 MB FAIL; 512, 768, and 1024 MB PASS. The
# default is 768 MB, one step above the measured minimum, because a
# 512 MB run sits close to the boundary. Headroom is part of the
# seamlessness contract: warming builds the mappings, headroom keeps
# them.
MEM_MB="${CELLA_INCEPTION_MEM_MB:-768}"

[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build"; exit 0; }
[ -f "$KERNEL" ] && [ -f "$DISK" ] || { echo "SKIP: inception assets missing -- run: make dist-nested"; exit 0; }
if ! [ -r /dev/kvm ] || ! [ -w /dev/kvm ]; then
    echo "SKIP: no read and write access to /dev/kvm"
    exit 0
fi

TMP=$(mktemp -d /tmp/cella-inception.XXXXXX)
trap 'kill $PID 2>/dev/null || true; wait $PID 2>/dev/null || true' EXIT
mkdir -p "$TMP/state"
cp "$DISK" "$TMP/disk.img"

CMDLINE="$("$BIN" --print-default-cmdline) root=/dev/vda rw virtio_mmio.device=4K@0xd0000000:5"
"$BIN" --state-dir "$TMP/state" --kernel "$KERNEL" --disk "$TMP/disk.img" \
    --mem-mb "$MEM_MB" --cmdline "$CMDLINE" >"$TMP/serial.log" 2>"$TMP/cella.err" &
PID=$!

echo "--- inception: waiting up to ${TIMEOUT}s for the probe verdict from one layer deep ---"
for _ in $(seq "$TIMEOUT"); do
    if grep -aq "cella-inception: probe exited" "$TMP/serial.log"; then
        break
    fi
    kill -0 "$PID" 2>/dev/null || break
    sleep 1
done

echo "--- the probe, as seen through the outer serial console ---"
grep -aE "cella-inception:|^SKIP|^FAIL|^PASS|difference:|prediction interval|complaints|real time spent|thinks passed" "$TMP/serial.log" || true

if grep -aq "PASS (FROZEN)" "$TMP/serial.log"; then
    echo "PASS: time is cryogenic one layer deep"
    rm -rf "$TMP"
    exit 0
fi
if grep -aqE "cella-inception: FAIL: no /dev/kvm|^SKIP" "$TMP/serial.log"; then
    echo "SKIP: the layer below the probe offers no virtualization"
    echo "(logs kept: $TMP)"
    exit 0
fi
echo "--- outer serial (last 30 lines) ---"
tail -n 30 "$TMP/serial.log" || true
echo "FAIL: no PASS (FROZEN) from the inner probe within ${TIMEOUT}s"
echo "(logs kept: $TMP)"
exit 1
