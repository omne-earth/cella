#!/usr/bin/env bash
# The world-engine gates (docs/WORLD-ENGINE.md, "The gates"). One
# rung per invocation: engine.sh w1 | w2 | w3 | w4 | w5. Each rung
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
MOTOR_PID=""; BRIDGE_PID=""
teardown() {
    [ -n "${BRIDGE_PID:-}" ] && kill "$BRIDGE_PID" 2>/dev/null || true
    [ -n "${MOTOR_PID:-}" ] && kill "$MOTOR_PID" 2>/dev/null || true
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    "$BIN" destroy "$VM" >/dev/null 2>&1 || true
    # The rungs' shared assertion: the bridge halted with the run.
    if [ -n "${BRIDGE_PID:-}" ]; then
        sleep 1
        kill -0 "$BRIDGE_PID" 2>/dev/null && { echo "FAIL: the bridge outlived the run"; exit 1; }
    fi
    if [ -n "${CELLA_KEEP_SANDBOX:-}" ]; then echo "kept: $CELLA_HOME"; else rm -rf "$CELLA_HOME"; fi
}
trap teardown EXIT
type_in() { local vm="$1"; shift; (printf '%s\n' "$1"; sleep 2) | timeout 20 "$BIN" enter "$vm" >/dev/null; }

say "$RUNG: stand the machine, the motor, and the bridge"
"$BIN" create "$VM" --net world:$WORLD_PORT/udp >/dev/null
"$BIN" start "$VM" >/dev/null
MOTOR_LOG="$CELLA_HOME/motor.log"
"$ENG" motor --listen "127.0.0.1:$DIAL_PORT" --allow "192.168.210.1:*" > "$MOTOR_LOG" 2>&1 &
MOTOR_PID=$!
sleep 1
grep -q "motor: listening" "$MOTOR_LOG" || { echo "FAIL: the motor never listened"; exit 1; }
"$ENG" "$VM" --dial "127.0.0.1:$DIAL_PORT" > "$CELLA_HOME/bridge.log" 2>&1 &
BRIDGE_PID=$!
sleep 2
kill -0 "$BRIDGE_PID" 2>/dev/null || { echo "FAIL: the bridge halted at dial -- $(cat "$CELLA_HOME/bridge.log")"; exit 1; }
"$BIN" gateway "$VM" open >/dev/null
sleep 2

say "w1: a park reaches the engine as a well-formed Event"
type_in "$VM" "ping -c1 -W2 $GW >/dev/null || true; echo se\"nt\""
deadline=$((SECONDS + 30))
until grep -q "motor: parked id=" "$MOTOR_LOG"; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: no park reached the engine"; tail -3 "$CELLA_HOME/bridge.log"; exit 1; }
    sleep 1
done
grep -q "motor: parked id=[0-9a-f]\{32\}" "$MOTOR_LOG" || { echo "FAIL: the Event carries no well-formed id"; exit 1; }
echo "  the stream stands: the park arrived with its id"
if [ "$RUNG" = w1 ]; then
    echo; echo "PASS: engine-w1 -- the stream stands"; exit 0
fi

say "w2: the decision lands -- the release delivers, the refusal lapses"
# The motor allowed the gateway's address; the park above was the
# guest's ARP or its echo toward the gateway, and the motor's release
# must deliver it: the machine froze on the park (its own egress),
# the bridge's kick staged the decision, and a thaw applies it.
deadline=$((SECONDS + 30))
until grep -q "motor: release id=" "$MOTOR_LOG"; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: the motor released nothing"; exit 1; }
    sleep 1
done
if [ -f "$M/state" ]; then "$BIN" thaw "$VM" >/dev/null; fi
deadline=$((SECONDS + 30))
until "$BIN" --dump-ledger "$M/network/ledger" 2>/dev/null | grep -q "released id="; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: the engine's release never applied"; exit 1; }
    if [ -f "$M/state" ]; then "$BIN" thaw "$VM" >/dev/null 2>&1 || true; fi
    sleep 1
done
echo "  the release landed: parked by the machine, decided by the engine, applied"

# The refusal: a destination off the allowlist. The guest sends a
# datagram to a refused address; the motor refuses; the operation
# lapses by the book.
type_in "$VM" "echo refused > /dev/udp/198.51.100.9/9 || true; echo of"f""
deadline=$((SECONDS + 40))
until grep -q "motor: refuse id=" "$MOTOR_LOG"; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: the refused park never reached the engine"; exit 1; }
    if [ -f "$M/state" ]; then "$BIN" thaw "$VM" >/dev/null 2>&1 || true; fi
    sleep 1
done
deadline=$((SECONDS + 30))
until "$BIN" --dump-ledger "$M/network/ledger" 2>/dev/null | grep -q "lapsed id=.*off the allowlist"; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: the refusal never lapsed with its why"; exit 1; }
    if [ -f "$M/state" ]; then "$BIN" thaw "$VM" >/dev/null 2>&1 || true; fi
    sleep 1
done
echo "  the refusal lapsed, the why in the book"
if [ "$RUNG" = w2 ]; then
    echo; echo "PASS: engine-w2 -- the decision lands"; exit 0
fi

echo "FAIL: rung $RUNG is not built yet"; exit 1
