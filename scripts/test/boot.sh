#!/usr/bin/env bash
# Boots cella against a real kernel under real KVM and waits for a
# *complete* boot -- root filesystem mounted, /sbin/init actually
# running (rootfs-init.sh prints "cella-rootfs: init running" on
# success) -- not just an early kernel banner. This is the one test in
# this repo that actually exercises the boot path (GDT/page tables/
# bzImage load, virtio-mmio device negotiation, root mount) end to end
# -- everything else in tests/ and scripts/test-*.sh deliberately
# avoids needing /dev/kvm.
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
BIN="${CELLA_BIN:-$ROOT/target/release/cella}"
CH="${CELLA_HOME:-$HOME/.cella}"
KERNEL="${CELLA_TEST_KERNEL:-$CH/kernel/canonical/bzImage}"
DISK="${CELLA_TEST_DISK:-$CH/rootfs/canonical/rootfs.ext4}"
TAP="${CELLA_TEST_TAP:-tap0}"
TIMEOUT_SECS="${CELLA_BOOT_TIMEOUT:-20}"

if [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
    echo "SKIP: no rw access to /dev/kvm on this machine"
    exit 0
fi
if [ ! -x "$BIN" ]; then
    echo "FAIL: $BIN not built (run: make build)"
    exit 1
fi
if [ ! -f "$KERNEL" ] || [ ! -f "$DISK" ]; then
    echo "SKIP: test assets not found -- run: make golden"
    exit 0
fi
if ! ip link show "$TAP" &>/dev/null; then
    echo "SKIP: $TAP does not exist -- run: sudo scripts/setup/tap.sh $TAP"
    exit 0
fi

# The default command line comes from the binary, so that these
# values are defined once. See src/config.rs.
CELLA_DEFAULT_CMDLINE="$("$BIN" --print-default-cmdline)"

TMP="$(mktemp -d)"
STATE_DIR="$TMP/state"
LOG="$TMP/serial.log"
PID=""
trap 'kill "$PID" 2>/dev/null; wait 2>/dev/null; rm -rf "$TMP"' EXIT
mkdir -p "$STATE_DIR"
cp "$DISK" "$TMP/disk.img" # don't mutate the shared test asset

echo "cella: booting (log: $LOG, timeout ${TIMEOUT_SECS}s)"
boot_start=$(date +%s.%N)
"$BIN" \
    --state-dir "$STATE_DIR" \
    --kernel "$KERNEL" \
    --disk "$TMP/disk.img" \
    --tap "$TAP" \
    --mem-mb 128 \
    --cmdline "${CELLA_DEFAULT_CMDLINE} root=/dev/vda rw virtio_mmio.device=4K@0xd0000000:5 virtio_mmio.device=4K@0xd0001000:6" \
    >"$LOG" 2>"$TMP/stderr.log" &
PID=$!

deadline=$((SECONDS + TIMEOUT_SECS))
kernel_seen=0
init_seen=0
exited_early=0
boot_elapsed=""
while [ $SECONDS -lt $deadline ]; do
    if grep -q "cella-rootfs: init running" "$LOG" 2>/dev/null; then
        init_seen=1
        boot_elapsed=$(awk -v s="$boot_start" -v e="$(date +%s.%N)" 'BEGIN { printf "%.2f", e - s }')
        break
    fi
    if ! kill -0 "$PID" 2>/dev/null; then
        exited_early=1
        break
    fi
    if grep -q "Linux version" "$LOG" 2>/dev/null; then
        kernel_seen=1
    fi
    sleep 0.1
done

kill "$PID" 2>/dev/null
wait "$PID" 2>/dev/null

echo "--- stderr ---"
cat "$TMP/stderr.log" 2>/dev/null || true
echo "--- last 20 lines of serial output ---"
tail -20 "$LOG" 2>/dev/null || true
echo "---"

if [ "$init_seen" -eq 1 ]; then
    echo "PASS: kernel booted, mounted root, and reached a running init in ${boot_elapsed}s (full boot confirmed)"
    exit 0
elif [ "$exited_early" -eq 1 ]; then
    echo "FAIL: process exited before reaching init (see stderr/serial log above)"
    exit 1
elif [ "$kernel_seen" -eq 1 ]; then
    echo "FAIL: kernel banner observed, but never reached a running init within ${TIMEOUT_SECS}s -- likely stuck between boot and userspace (root mount, device negotiation, or init itself)"
    exit 1
elif [ -s "$LOG" ]; then
    echo "PARTIAL: some serial output but no kernel banner matched -- inspect the log above"
    exit 1
else
    echo "FAIL: no serial output at all within ${TIMEOUT_SECS}s"
    exit 1
fi
