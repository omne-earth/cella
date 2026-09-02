#!/usr/bin/env bash
# Boots a machine through the verbs and waits for a *complete* boot --
# root filesystem mounted, /sbin/init actually running (rootfs-init.sh
# prints "cella-rootfs: init running" on success) -- not just an early
# kernel banner. This is the one test in this repo that exercises the
# boot path (GDT/page tables/bzImage load, virtio-mmio device
# negotiation, root mount) end to end.
#
# The kernel banner alone used to be the pass criterion here, which
# was a real false positive for a long stretch of this project's
# history: the guest was panicking on "unable to mount root fs" (no
# VIRTIO_F_VERSION_1, see src/devices/virtio/mod.rs) well after
# "Linux version" had already printed, and this test kept passing
# through all of it.
#
# Honesty note (see README "What to check first"): distinguishing a
# real regression from an environment issue (missing /dev/kvm, a bad
# kernel/rootfs pairing) is still on you -- a clean pass here is strong
# evidence the loader and device negotiation are correct, not proof
# nothing downstream in the guest's own userspace is broken.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$HERE/../.."
BIN="${CELLA_BIN:-$ROOT/target/smoke/cella}"
REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
TAP="${CELLA_TEST_TAP:-tap0}"
TIMEOUT_SECS="${CELLA_BOOT_TIMEOUT:-20}"

if [ ! -x "$BIN" ]; then
    echo "FAIL: $BIN not built (run: make build)"
    exit 1
fi
"$BIN" doctor gate kvm golden:kernel:canonical golden:rootfs:canonical || exit 0
if ! ip link show "$TAP" &>/dev/null; then
    echo "SKIP: $TAP does not exist -- run: cella doctor fix"
    exit 0
fi

# A sandbox home: the boot smoke must not touch the real machines.
export CELLA_HOME=$(mktemp -d /tmp/cella-boot.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/canonical"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/kernel/canonical/golden.json" "$CELLA_HOME/kernel/canonical/" 2>/dev/null || true
cp "$REAL_HOME/rootfs/canonical/rootfs.ext4" "$CELLA_HOME/rootfs/canonical/"
cp "$REAL_HOME/rootfs/canonical/golden.json" "$CELLA_HOME/rootfs/canonical/" 2>/dev/null || true

VM=boot
teardown() {
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    [ -n "${VMM_PID:-}" ] && kill -9 "$VMM_PID" 2>/dev/null || true
    rm -rf "$CELLA_HOME"
}
trap teardown EXIT
CON="$CELLA_HOME/machines/$VM/console.log"

echo "cella: booting through the verbs (timeout ${TIMEOUT_SECS}s)"
boot_start=$(date +%s.%N)
"$BIN" create "$VM" --kernel canonical --rootfs canonical --mem-mb 128 --net "$TAP" >/dev/null || { echo "FAIL: create failed"; exit 1; }
"$BIN" start "$VM" >/dev/null || { echo "FAIL: start failed"; exit 1; }
VMM_PID=$(cat "$CELLA_HOME/machines/$VM/pid")

deadline=$((SECONDS + TIMEOUT_SECS))
kernel_seen=0
init_seen=0
exited_early=0
boot_elapsed=""
while [ $SECONDS -lt $deadline ]; do
    if grep -aq "cella-rootfs: init running" "$CON" 2>/dev/null; then
        init_seen=1
        boot_elapsed=$(awk -v s="$boot_start" -v e="$(date +%s.%N)" 'BEGIN { printf "%.2f", e - s }')
        break
    fi
    if ! kill -0 "$VMM_PID" 2>/dev/null; then
        exited_early=1
        break
    fi
    if grep -aq "Linux version" "$CON" 2>/dev/null; then
        kernel_seen=1
    fi
    sleep 0.1
done

if [ "$init_seen" -eq 1 ]; then
    echo "PASS: kernel booted, mounted root, and reached a running init in ${boot_elapsed}s (full boot confirmed)"
    exit 0
fi
echo "--- vmm.log ---"
tail -10 "$CELLA_HOME/machines/$VM/vmm.log" 2>/dev/null || true
echo "--- last 20 lines of console output ---"
tail -20 "$CON" 2>/dev/null || true
echo "---"
if [ "$exited_early" -eq 1 ]; then
    echo "FAIL: the VMM exited before init ran (see the logs above)"
    exit 1
elif [ "$kernel_seen" -eq 1 ]; then
    echo "FAIL: kernel banner observed, but never reached a running init within ${TIMEOUT_SECS}s -- likely stuck between boot and userspace (root mount, device negotiation, or init itself)"
    exit 1
elif [ -s "$CON" ]; then
    echo "PARTIAL: some console output but no kernel banner matched -- inspect the log above"
    exit 1
else
    echo "FAIL: no console output at all within ${TIMEOUT_SECS}s"
    exit 1
fi
