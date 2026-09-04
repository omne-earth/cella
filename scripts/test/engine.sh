#!/usr/bin/env bash
# The world-engine gates (docs/WORLD-ENGINE.md, "The gates"). One
# rung per invocation: engine.sh w1 | w2 | w3 | w4 | e5. Each rung
# assumes the ones before it; a red rung names its layer.
set -euo pipefail

RUNG="${1:-w1}"
cd "$(dirname "$0")/../.."
BIN=target/smoke/cella
ENG=target/smoke/cella-engine
# The knock port: random per run, so a leaked translator from an
# earlier gate can never poison this one. Four digits, unprivileged.
WORLD_PORT=$(( (RANDOM % 8976) + 1024 ))
# The engine's own listener: distinct from the knock port.
DIAL_PORT=$(( (RANDOM % 8976) + 1024 ))
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build-smoke"; exit 0; }
[ -f "$ENG" ] || { echo "SKIP: $ENG not built -- run: make build-smoke"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0

say() { echo; echo "==> $1"; }
GW=192.168.210.1

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-engine.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

VM=judged
M="$CELLA_HOME/machines/$VM"
TOY_PID=""; BRIDGE_PID=""
teardown() {
    [ -n "${BRIDGE_PID:-}" ] && kill "$BRIDGE_PID" 2>/dev/null || true
    [ -n "${TOY_PID:-}" ] && kill "$TOY_PID" 2>/dev/null || true
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    "$BIN" destroy "$VM" >/dev/null 2>&1 || true
    # The rungs' shared assertion: the bridge died with the run.
    if [ -n "${BRIDGE_PID:-}" ]; then
        sleep 1
        kill -0 "$BRIDGE_PID" 2>/dev/null && { echo "FAIL: the bridge outlived the run"; exit 1; }
    fi
    if [ -n "${CELLA_KEEP_SANDBOX:-}" ]; then echo "kept: $CELLA_HOME"; else rm -rf "$CELLA_HOME"; fi
}
trap teardown EXIT
type_in() { local vm="$1"; shift; (printf '%s\n' "$1"; sleep 2) | timeout 20 "$BIN" enter "$vm" >/dev/null; }

say "$RUNG: stand the machine, the toy engine, and the bridge"
"$BIN" create "$VM" --net world:$WORLD_PORT/udp >/dev/null
"$BIN" start "$VM" >/dev/null
TOY_LOG="$CELLA_HOME/toy.log"
"$ENG" toy --listen "127.0.0.1:$DIAL_PORT" > "$TOY_LOG" 2>&1 &
TOY_PID=$!
sleep 1
grep -q "toy: listening" "$TOY_LOG" || { echo "FAIL: the toy engine never listened"; exit 1; }
"$ENG" "$VM" --dial "127.0.0.1:$DIAL_PORT" > "$CELLA_HOME/bridge.log" 2>&1 &
BRIDGE_PID=$!
sleep 2
kill -0 "$BRIDGE_PID" 2>/dev/null || { echo "FAIL: the bridge died at dial -- $(cat "$CELLA_HOME/bridge.log")"; exit 1; }
"$BIN" gateway "$VM" open >/dev/null
sleep 2

say "w1: a park reaches the engine as a well-formed Event"
type_in "$VM" "ping -c1 -W2 $GW >/dev/null || true; echo se\"nt\""
deadline=$((SECONDS + 30))
until grep -q "toy: parked id=" "$TOY_LOG"; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: no park reached the engine"; tail -3 "$CELLA_HOME/bridge.log"; exit 1; }
    sleep 1
done
grep -q "toy: parked id=[0-9a-f]\{32\}" "$TOY_LOG" || { echo "FAIL: the Event carries no well-formed id"; exit 1; }
echo "  the stream stands: the park arrived with its id"
if [ "$RUNG" = w1 ]; then
    echo; echo "PASS: engine-w1 -- the stream stands"; exit 0
fi

echo "FAIL: rung $RUNG is not built yet"; exit 1
