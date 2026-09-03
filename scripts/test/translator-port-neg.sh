#!/usr/bin/env bash
# smoke-translator-port-neg: the tether (negative). An incomplete
# teardown -- the machine dir removed without destroy -- must not
# orphan the translator: the process exits on its own when its
# edge.sock is gone, and the knock port frees with it. Before the
# tether, such an orphan held the port until reboot and silently
# swallowed the next machine's knocks.
set -euo pipefail

cd "$(dirname "$0")/../.."
BIN=target/smoke/cella
# The knock port: random per run, so a leaked translator from an
# earlier gate can never poison this one. Four digits, unprivileged.
WORLD_PORT=$(( (RANDOM % 8976) + 1024 ))
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build-smoke"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0

say() { echo; echo "==> $1"; }

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-tether.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

VM=tether
M="$CELLA_HOME/machines/$VM"
EDGE_PID=""
teardown() {
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    "$BIN" destroy "$VM" >/dev/null 2>&1 || true
    [ -n "${EDGE_PID:-}" ] && kill -9 "$EDGE_PID" 2>/dev/null || true
    [ -n "${VMM_PID:-}" ] && kill -9 "$VMM_PID" 2>/dev/null || true
    rm -rf "$CELLA_HOME"
}
trap teardown EXIT

say "step 1: a machine stands; its translator holds the knock port"
"$BIN" create "$VM" --net world:$WORLD_PORT/udp >/dev/null
"$BIN" start "$VM" >/dev/null
sleep 2
EDGE_PID=$(cat "$M/edge.pid" 2>/dev/null || true)
VMM_PID=$(cat "$M/pid" 2>/dev/null || true)
[ -n "$EDGE_PID" ] && kill -0 "$EDGE_PID" 2>/dev/null || { echo "FAIL: no translator stands"; exit 1; }
ss -ulpn 2>/dev/null | grep -q ":$WORLD_PORT " || { echo "FAIL: the translator did not bind port $WORLD_PORT"; exit 1; }
echo "  translator $EDGE_PID holds udp $WORLD_PORT"

say "step 2: the incomplete teardown -- the dir goes, destroy never runs"
[ -n "${VMM_PID:-}" ] && kill -9 "$VMM_PID" 2>/dev/null || true
rm -rf "$M"
echo "  machine dir removed; the pid file and edge.sock are gone"

say "step 3: the tether -- the translator exits on its own; the port frees (negative)"
deadline=$((SECONDS + 15))
while kill -0 "$EDGE_PID" 2>/dev/null; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: the translator outlived its machine dir -- a phantom holds udp $WORLD_PORT until reboot"; exit 1; }
    sleep 1
done
ss -ulpn 2>/dev/null | grep -q ":$WORLD_PORT " && { echo "FAIL: the port is still bound after the translator exited"; exit 1; }
EDGE_PID=""
echo "  no orphan: the translator followed its machine out, and the port freed"

echo
echo "PASS: the tether -- an incomplete teardown orphans nothing; the port dies with the machine"
