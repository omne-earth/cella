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

say "w3: stillness on engine halt -- the operation waits, nothing defaults"
kill "$MOTOR_PID" 2>/dev/null; wait "$MOTOR_PID" 2>/dev/null || true; MOTOR_PID=""
sleep 1
B_REL=$("$BIN" --dump-ledger "$M/network/ledger" 2>/dev/null | grep -c "released id=\|lapsed id=" || true)
if [ -f "$M/state" ]; then "$BIN" thaw "$VM" >/dev/null 2>&1 || true; sleep 1; fi
type_in "$VM" "echo held > /dev/udp/192.168.210.1/$WORLD_PORT || true; echo se\"nt2\""
sleep 8
A_REL=$("$BIN" --dump-ledger "$M/network/ledger" 2>/dev/null | grep -c "released id=\|lapsed id=" || true)
[ "$A_REL" -gt "$B_REL" ] && { echo "FAIL: something decided while the engine was halted"; exit 1; }
"$BIN" gateway "$VM" show | grep -qE "^[0-9a-f]{32} .*held$" || { echo "FAIL: no held operation stands -- where did it go?"; exit 1; }
echo "  the engine halted; the operation waits; nothing defaulted"

say "w3: a restarted engine resumes judging the same hold"
"$ENG" motor --listen "127.0.0.1:$DIAL_PORT" --allow "192.168.210.1:*" > "$MOTOR_LOG" 2>&1 &
MOTOR_PID=$!
sleep 1
# The old bridge halted with its stream; a new one carries on.
"$ENG" "$VM" --dial "127.0.0.1:$DIAL_PORT" > "$CELLA_HOME/bridge2.log" 2>&1 &
BRIDGE_PID=$!
deadline=$((SECONDS + 40))
until [ "$("$BIN" --dump-ledger "$M/network/ledger" 2>/dev/null | grep -c "released id=\|lapsed id=" || true)" -gt "$B_REL" ]; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: the restarted engine never judged the waiting hold"; exit 1; }
    if [ -f "$M/state" ]; then "$BIN" thaw "$VM" >/dev/null 2>&1 || true; fi
    sleep 1
done
echo "  the hold outlived the halt: judged by the engine's second life"
if [ "$RUNG" = w3 ]; then
    echo; echo "PASS: engine-w3 -- stillness on engine halt"; exit 0
fi

say "w4: the frozen machine -- decisions stage, and the thaw applies them"
# A fresh park: the guest's own egress freezes it (the park is the
# freeze), so the machine has no pid when the engine's decision
# arrives -- the bridge's kick must stage, not error.
if [ -f "$M/state" ]; then "$BIN" thaw "$VM" >/dev/null 2>&1 || true; sleep 1; fi
B_REL=$("$BIN" --dump-ledger "$M/network/ledger" 2>/dev/null | grep -c "released id=" || true)
V_SIZE=$(stat -c %s "$M/verdict" 2>/dev/null || echo 0)
type_in "$VM" "echo staged > /dev/udp/192.168.210.1/$WORLD_PORT || true; echo se\"nt3\""
deadline=$((SECONDS + 30))
until [ -f "$M/state" ]; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: the park never froze the machine"; exit 1; }
    sleep 1
done
deadline=$((SECONDS + 30))
until [ "$(stat -c %s "$M/verdict" 2>/dev/null || echo 0)" -gt "$V_SIZE" ]; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: no decision staged against the frozen machine"; exit 1; }
    sleep 1
done
kill -0 "$BRIDGE_PID" 2>/dev/null || { echo "FAIL: the bridge halted on the pidless kick"; exit 1; }
[ "$("$BIN" --dump-ledger "$M/network/ledger" 2>/dev/null | grep -c "released id=" || true)" -gt "$B_REL" ] \
    && { echo "FAIL: a decision applied against a frozen machine"; exit 1; }
echo "  the decision staged in the verdict file; the frozen machine holds"
"$BIN" thaw "$VM" >/dev/null
deadline=$((SECONDS + 30))
until [ "$("$BIN" --dump-ledger "$M/network/ledger" 2>/dev/null | grep -c "released id=" || true)" -gt "$B_REL" ]; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: the thaw never applied the staged decision"; exit 1; }
    sleep 1
done
echo "  the thaw applied the staged decision, in park order"
if [ "$RUNG" = w4 ]; then
    echo; echo "PASS: engine-w4 -- the frozen machine"; exit 0
fi

echo "FAIL: rung $RUNG is not built yet"; exit 1
