#!/usr/bin/env bash
# probe-inception: the freeze and thaw clock probe, one layer deep.
# The outer machine runs bzImage-nested with rootfs-inception.ext4
# through the verbs, and its init runs the probe against an inner
# cella. The probe freezes and thaws the inner guest and prints its
# verdict, and the verdict arrives on the outer console. This test
# relays that verdict.
#
# The outer machine runs without a TAP, and the inner guest runs with
# the block device only. Depth counts the hypervisors between the
# metal and the inner guest: two on bare metal, three on a nested
# development host. A missing /dev/kvm in the outer guest is a SKIP,
# and bare metal is the reference.
set -euo pipefail

cd "$(dirname "$0")/../.."
BIN=target/smoke/cella
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
"$BIN" doctor gate kvm golden:kernel:nested golden:rootfs:inception || exit 0

# A sandbox home: the smoke must not touch the real machines.
REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-inception.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/nested" "$CELLA_HOME/rootfs/inception"
cp "$REAL_HOME/kernel/nested/bzImage" "$CELLA_HOME/kernel/nested/"
cp "$REAL_HOME/kernel/nested/golden.json" "$CELLA_HOME/kernel/nested/" 2>/dev/null || true
cp "$REAL_HOME/rootfs/inception/rootfs.ext4" "$CELLA_HOME/rootfs/inception/"
cp "$REAL_HOME/rootfs/inception/golden.json" "$CELLA_HOME/rootfs/inception/" 2>/dev/null || true

VM=inception
CON="$CELLA_HOME/machines/$VM/console.log"
KEEP=0
teardown() {
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    VMM_PID=$(cat "$CELLA_HOME/machines/$VM/pid" 2>/dev/null || true)
    [ -n "${VMM_PID:-}" ] && kill -9 "$VMM_PID" 2>/dev/null || true
    if [ "$KEEP" = 1 ]; then echo "(logs kept: $CELLA_HOME)"; else rm -rf "$CELLA_HOME"; fi
}
trap teardown EXIT

"$BIN" create "$VM" --kernel nested --rootfs inception --mem-mb "$MEM_MB" --net none >/dev/null
"$BIN" start "$VM" >/dev/null
VMM_PID=$(cat "$CELLA_HOME/machines/$VM/pid")

echo "--- inception: waiting up to ${TIMEOUT}s for the probe verdict from one layer deep ---"
for _ in $(seq "$TIMEOUT"); do
    if grep -aq "cella-inception: probe exited" "$CON" 2>/dev/null; then
        break
    fi
    kill -0 "$VMM_PID" 2>/dev/null || break
    sleep 1
done

echo "--- the probe, as seen through the outer console ---"
grep -aE "cella-inception:|^SKIP|^FAIL|^PASS|difference:|prediction interval|complaints|real time spent|thinks passed" "$CON" || true

if grep -aq "PASS (FROZEN)" "$CON"; then
    echo "PASS: time is cryogenic one layer deep"
    exit 0
fi
KEEP=1
if grep -aqE "cella-inception: FAIL: no /dev/kvm|^SKIP" "$CON"; then
    echo "SKIP: the layer below the probe offers no virtualization"
    exit 0
fi
echo "--- outer console (last 30 lines) ---"
tail -n 30 "$CON" || true
echo "FAIL: no PASS (FROZEN) from the inner probe within ${TIMEOUT}s"
exit 1
