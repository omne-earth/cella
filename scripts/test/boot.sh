#!/usr/bin/env bash
# Boots cella against a real kernel under real KVM and waits for signs
# of life on the serial console. This is the one test in this repo that
# actually exercises the boot path (GDT/page tables/bzImage load) end to
# end -- everything else in tests/ and scripts/test-*.sh deliberately
# avoids needing /dev/kvm.
#
# Honesty note (see README "What to check first"): the boot path has
# never been run against real hardware by us. This script is what you'd
# run to find out; a clean pass here is the strongest evidence available
# that the loader is correct.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$HERE/../.."
BIN="${CELLA_BIN:-$ROOT/target/release/cella}"
KERNEL="${CELLA_TEST_KERNEL:-$ROOT/dist/bzImage}"
DISK="${CELLA_TEST_DISK:-$ROOT/dist/rootfs.ext4}"
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
    echo "SKIP: test assets not found -- run: make dist"
    exit 0
fi
if ! ip link show "$TAP" &>/dev/null; then
    echo "SKIP: $TAP does not exist -- run: sudo scripts/setup/tap.sh $TAP"
    exit 0
fi

TMP="$(mktemp -d)"
STATE_DIR="$TMP/state"
LOG="$TMP/serial.log"
PID=""
trap 'kill "$PID" 2>/dev/null; wait 2>/dev/null; rm -rf "$TMP"' EXIT
mkdir -p "$STATE_DIR"
cp "$DISK" "$TMP/disk.img" # don't mutate the shared test asset

echo "cella: booting (log: $LOG, timeout ${TIMEOUT_SECS}s)"
"$BIN" \
    --state-dir "$STATE_DIR" \
    --kernel "$KERNEL" \
    --disk "$TMP/disk.img" \
    --tap "$TAP" \
    --mem-mb 128 \
    --cmdline "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw virtio_mmio.device=4K@0xd0000000:5 virtio_mmio.device=4K@0xd0001000:6" \
    >"$LOG" 2>"$TMP/stderr.log" &
PID=$!

deadline=$((SECONDS + TIMEOUT_SECS))
found=0
while [ $SECONDS -lt $deadline ]; do
    if ! kill -0 "$PID" 2>/dev/null; then
        echo "FAIL: process exited before producing kernel output"
        break
    fi
    if grep -q "Linux version" "$LOG" 2>/dev/null; then
        found=1
        break
    fi
    sleep 0.5
done

kill "$PID" 2>/dev/null
wait "$PID" 2>/dev/null

echo "--- stderr ---"
cat "$TMP/stderr.log" 2>/dev/null || true
echo "--- last 20 lines of serial output ---"
tail -20 "$LOG" 2>/dev/null || true
echo "---"

if [ "$found" -eq 1 ]; then
    echo "PASS: kernel banner observed on serial console"
    exit 0
elif [ -s "$LOG" ]; then
    echo "PARTIAL: some serial output but no kernel banner matched -- inspect the log above"
    exit 1
else
    echo "FAIL: no serial output at all within ${TIMEOUT_SECS}s"
    exit 1
fi
