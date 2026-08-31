#!/usr/bin/env bash
# Orchestrates the full cryogenic freeze/thaw lifecycle against a real
# VM under real KVM: boot -> freeze (SIGUSR1) -> verify the crash-safe
# sidecar -> thaw (same command line, same --state-dir) -> verify
# one-shot enforcement. `make smoke-thaw` is this script; nothing in the
# Makefile duplicates this logic.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$HERE/../.."
BIN="${CELLA_BIN:-$ROOT/target/release/cella}"
CH="${CELLA_HOME:-$HOME/.cella}"
KERNEL="${CELLA_TEST_KERNEL:-$CH/kernel/canonical/bzImage}"
DISK="${CELLA_TEST_DISK:-$CH/rootfs/canonical/rootfs.ext4}"
TAP="${CELLA_TEST_TAP:-tap0}"
BOOT_WAIT_SECS="${CELLA_BOOT_WAIT:-8}"
FREEZE_TIMEOUT_SECS="${CELLA_FREEZE_TIMEOUT:-10}"

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
DISK_COPY="$TMP/disk.img"
mkdir -p "$STATE_DIR"
cp "$DISK" "$DISK_COPY"
# Two processes get backgrounded over this script's life (boot, then
# thaw) -- track both explicitly rather than relying on `kill %1`,
# which can point at the wrong job once the first one's slot is reused.
PID=""
PID2=""
trap 'kill "$PID" ${PID2:+"$PID2"} 2>/dev/null; wait 2>/dev/null; rm -rf "$TMP"' EXIT

RUN_ARGS=(
    --state-dir "$STATE_DIR"
    --disk "$DISK_COPY"
    --tap "$TAP"
    --mem-mb 128
    --cmdline "${CELLA_DEFAULT_CMDLINE} root=/dev/vda rw virtio_mmio.device=4K@0xd0000000:5 virtio_mmio.device=4K@0xd0001000:6"
)

fail() {
    echo "FAIL: $1"
    exit 1
}

echo "--- step 1: boot ---"
"$BIN" --kernel "$KERNEL" "${RUN_ARGS[@]}" >"$TMP/boot.log" 2>"$TMP/boot.err" &
PID=$!
sleep "$BOOT_WAIT_SECS"
kill -0 "$PID" 2>/dev/null || fail "process exited during boot (see $TMP/boot.err)"
echo "PASS: process is running after ${BOOT_WAIT_SECS}s"

echo "--- step 2: freeze ---"
# Cryogenic freeze: SIGUSR1 tells cella to write its frozen state and
# exit; re-running the same command line against the same --state-dir
# then thaws instead of booting (step 3, below).
kill -USR1 "$PID"
echo "cella: sent freeze signal to pid $PID (it will exit once the state file is written)"
deadline=$((SECONDS + FREEZE_TIMEOUT_SECS))
while kill -0 "$PID" 2>/dev/null; do
    [ $SECONDS -lt $deadline ] || fail "process did not exit within ${FREEZE_TIMEOUT_SECS}s of SIGUSR1"
    sleep 0.2
done
wait "$PID" 2>/dev/null
[ -f "$STATE_DIR/state" ] || fail "no state file after freeze -- write_state did not complete or did not rename into place"
[ -f "$STATE_DIR/ram.img" ] || fail "no ram.img after freeze"
[ ! -f "$STATE_DIR/state.tmp" ] || fail "state.tmp left behind -- rename step did not happen"
echo "PASS: process exited cleanly, state + ram.img present, no leftover .tmp"

echo "--- step 3: thaw ---"
# Same command line, same --state-dir: cella must detect the frozen
# state and thaw instead of re-booting. --kernel is deliberately omitted
# here to prove it isn't needed on thaw.
"$BIN" "${RUN_ARGS[@]}" >"$TMP/thaw.log" 2>"$TMP/thaw.err" &
PID2=$!
sleep 2
kill -0 "$PID2" 2>/dev/null || fail "process exited immediately on thaw (see $TMP/thaw.err)"
grep -q "thawed" "$TMP/thaw.err" "$TMP/thaw.log" 2>/dev/null || fail "no 'thawed' message observed"
echo "PASS: thawed and running"

echo "--- step 4: one-shot enforcement ---"
[ ! -f "$STATE_DIR/state" ] || fail "state file still present after a successful thaw -- one-shot enforcement did not fire"
echo "PASS: state file consumed by finalize_thaw"

kill "$PID2" 2>/dev/null
wait "$PID2" 2>/dev/null

echo "ALL FREEZE/THAW STEPS PASSED"
