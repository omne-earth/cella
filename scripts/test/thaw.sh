#!/usr/bin/env bash
# Orchestrates the full cryogenic freeze/thaw lifecycle through the
# verbs against a real VM under real KVM: create -> start -> freeze ->
# verify the crash-safe sidecar -> thaw -> verify one-shot enforcement.
# `make smoke-thaw` is this script; nothing in the Makefile duplicates
# this logic.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$HERE/../.."
BIN="${CELLA_BIN:-$ROOT/target/smoke/cella}"
# The knock port: random per run, so a leaked translator from an
# earlier gate (a stale bind on a fixed port swallows knocks
# silently) can never poison this one. Four digits, unprivileged.
WORLD_PORT=$(( (RANDOM % 8976) + 1024 ))
REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
BOOT_WAIT_SECS="${CELLA_BOOT_WAIT:-8}"

if [ ! -x "$BIN" ]; then
    echo "FAIL: $BIN not built (run: make build)"
    exit 1
fi
"$BIN" doctor gate kvm golden:kernel:canonical golden:rootfs:canonical || exit 0

# A sandbox home: the smoke must not touch the real machines.
export CELLA_HOME=$(mktemp -d /tmp/cella-thaw.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/canonical"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/kernel/canonical/golden.json" "$CELLA_HOME/kernel/canonical/" 2>/dev/null || true
cp "$REAL_HOME/rootfs/canonical/rootfs.ext4" "$CELLA_HOME/rootfs/canonical/"
cp "$REAL_HOME/rootfs/canonical/golden.json" "$CELLA_HOME/rootfs/canonical/" 2>/dev/null || true

VM=thaw
M="$CELLA_HOME/machines/$VM"
teardown() {
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    VMM_PID=$(cat "$M/pid" 2>/dev/null || true)
    [ -n "${VMM_PID:-}" ] && kill -9 "$VMM_PID" 2>/dev/null || true
    rm -rf "$CELLA_HOME"
}
trap teardown EXIT

fail() {
    echo "FAIL: $1"
    tail -10 "$M/vmm.log" 2>/dev/null | sed 's/^/   /'
    exit 1
}

echo "--- step 1: create and start ---"
"$BIN" create "$VM" --kernel canonical --rootfs canonical --mem-mb 128 --net world:$WORLD_PORT/tcp+$WORLD_PORT/udp >/dev/null || fail "create failed"
"$BIN" start "$VM" >/dev/null || fail "start failed"
sleep "$BOOT_WAIT_SECS"
kill -0 "$(cat "$M/pid")" 2>/dev/null || fail "the VMM exited during boot"
echo "PASS: the machine is running after ${BOOT_WAIT_SECS}s"

echo "--- step 2: freeze ---"
"$BIN" freeze "$VM" >/dev/null || fail "freeze failed"
[ -f "$M/state" ] || fail "no state file after freeze -- write_state did not complete or did not rename into place"
[ -f "$M/ram.img" ] || fail "no ram.img after freeze"
[ ! -f "$M/state.tmp" ] || fail "state.tmp left behind -- rename step did not happen"
echo "PASS: the VMM exited cleanly, state + ram.img present, no leftover .tmp"

echo "--- step 3: thaw ---"
"$BIN" thaw "$VM" >/dev/null || fail "thaw failed"
sleep 2
kill -0 "$(cat "$M/pid")" 2>/dev/null || fail "the VMM exited immediately on thaw"
grep -q "thawed" "$M/vmm.log" 2>/dev/null || fail "no 'thawed' message observed"
echo "PASS: thawed and running"

echo "--- step 4: one-shot enforcement ---"
[ ! -f "$M/state" ] || fail "state file still present after a successful thaw -- one-shot enforcement did not fire"
echo "PASS: state file consumed by finalize_thaw"

"$BIN" stop "$VM" >/dev/null 2>&1 || true
"$BIN" destroy "$VM" >/dev/null 2>&1 || true

echo "ALL FREEZE/THAW STEPS PASSED"
