#!/usr/bin/env bash
# smoke-membrane-memory: the membrane's standing memory (N.F.7,
# docs/NETWORK-MODEL.md "The membrane's memory"). gRPC-only: the
# judge is a rule engine at the seam (the motor stands in), a
# memory rides the Decide stream, and the bridge lands it. One
# criterion per invocation, the device-state pattern:
#   mm1  the live park: a remembered destination waits live -- no
#        freeze, the decision applies on the kick
#   mm2  isolation: an un-remembered destination still freezes
#   mm3  self-expiry: keep_open lapses, the next park freezes
#   mm4  the live refusal: instant lapse, no freeze-thaw churn,
#        the why in the book
#   mm5  the door: the landing is witnessed, the file stands
#   mm6  fail-closed edges: a malformed file is inert, and a thaw
#        does not resurrect an expired memory
set -uo pipefail

MM="${1:-}"
case "$MM" in
mm1|mm2|mm3|mm4|mm5|mm6) ;;
*) echo "usage: membrane-memory.sh <mm1|mm2|mm3|mm4|mm5|mm6>"; exit 2 ;;
esac

cd "$(dirname "$0")/../.."
BIN=target/lab/cella
ENG=target/lab/cella-engine
WORLD_PORT=$(( (RANDOM % 8976) + 1024 ))
DIAL_PORT=$(( (RANDOM % 8976) + 1024 ))
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build-lab"; exit 0; }
[ -f "$ENG" ] || { echo "SKIP: $ENG not built -- run: make build-lab"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0

say() { echo; echo "==> $1"; }
GW=192.168.210.1
OFF=198.51.100.9

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-membrane.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

VM=mindful
M="$CELLA_HOME/machines/$VM"
MOTOR_PID=""; BRIDGE_PID=""
teardown() {
    [ -n "${BRIDGE_PID:-}" ] && kill "$BRIDGE_PID" 2>/dev/null || true
    [ -n "${MOTOR_PID:-}" ] && kill "$MOTOR_PID" 2>/dev/null || true
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    "$BIN" destroy "$VM" >/dev/null 2>&1 || true
    if [ -n "${CELLA_KEEP_SANDBOX:-}" ]; then echo "kept: $CELLA_HOME"; else rm -rf "$CELLA_HOME"; fi
}
trap teardown EXIT
type_in() { (printf '%s\n' "$1"; sleep 2) | timeout 20 "$BIN" enter "$VM" >/dev/null; }
ledger() { "$BIN" --dump-ledger "$M/network/ledger" 2>/dev/null; }
book() { "$BIN" --dump-ledger "$M/audit" 2>/dev/null; }
# Pump: while frozen, thaw (the motor decides; the bridge lands).
pump_until() { # <deadline-secs> <grep-pattern-on-ledger>
    local deadline=$((SECONDS + $1))
    until ledger | grep -q "$2"; do
        [ $SECONDS -lt $deadline ] || return 1
        if [ -f "$M/state" ]; then "$BIN" thaw "$VM" >/dev/null 2>&1 || true; fi
        sleep 1
    done
}
# Watch for a freeze during a window; 0 = froze, 1 = never froze.
froze_within() { # <secs>
    local deadline=$((SECONDS + $1))
    while [ $SECONDS -lt $deadline ]; do
        [ -f "$M/state" ] && return 0
        sleep 0.2
    done
    return 1
}

stand_up() { # <motor extra args...>
    "$BIN" create "$VM" --net world:$WORLD_PORT/udp >/dev/null
    "$BIN" start "$VM" >/dev/null
    MOTOR_LOG="$CELLA_HOME/motor.log"
    "$ENG" motor --listen "127.0.0.1:$DIAL_PORT" "$@" > "$MOTOR_LOG" 2>&1 &
    MOTOR_PID=$!
    sleep 1
    grep -q "motor: listening" "$MOTOR_LOG" || { echo "FAIL: the motor never listened"; exit 1; }
    "$ENG" "$VM" --dial "127.0.0.1:$DIAL_PORT" > "$CELLA_HOME/bridge.log" 2>&1 &
    BRIDGE_PID=$!
    sleep 2
    kill -0 "$BRIDGE_PID" 2>/dev/null || { echo "FAIL: the bridge halted at dial"; exit 1; }
    "$BIN" gateway "$VM" open >/dev/null
    sleep 2
}

# First contact pays the freeze: ping once, pump until the echo
# released, and leave the machine running with the memory standing.
first_contact() {
    type_in "ping -c1 -W4 $GW >/dev/null 2>&1 || true; echo prime\"d\""
    pump_until 40 "released id=" || { echo "FAIL: the first contact never released"; exit 1; }
    if [ -f "$M/state" ]; then "$BIN" thaw "$VM" >/dev/null 2>&1 || true; fi
    sleep 2
    [ -f "$M/state" ] && { echo "FAIL: the machine did not settle running after first contact"; exit 1; }
}

case "$MM" in

mm1)
    say "mm1: a remembered destination waits live"
    stand_up --allow "$GW:*" --remember "arp:600" --remember "$GW:0:600"
    first_contact
    [ -f "$M/membrane-memory" ] || { echo "FAIL: no memory landed at first contact"; exit 1; }
    before=$(ledger | grep -c "released id=" || true)
    say "  the second ping: the park must wait live, no freeze"
    type_in "ping -c1 -W4 $GW >/dev/null 2>&1 && echo live-o\"k\""
    if froze_within 8; then echo "FAIL: a remembered park froze the machine"; exit 1; fi
    after=$(ledger | grep -c "released id=" || true)
    [ "$after" -gt "$before" ] || { echo "FAIL: no new release landed live"; exit 1; }
    grep -aq "live-ok" "$M/console.log" || { echo "FAIL: the guest never saw the live reply"; exit 1; }
    echo "  parked, decided, delivered -- the machine never stopped"
    echo; echo "PASS: mm1 -- the live park"
    ;;

mm2)
    say "mm2: an un-remembered destination still freezes"
    stand_up --allow "$GW:*" --allow "$OFF:*" --remember "arp:600" --remember "$GW:0:600"
    first_contact
    say "  a datagram to an un-remembered (allowed) destination"
    type_in "echo x > /dev/udp/$OFF/7 || true; echo se\"nt\""
    froze_within 15 || { echo "FAIL: an un-remembered park did not freeze"; exit 1; }
    echo "  memory never leaks across destinations: the park froze"
    pump_until 30 "released id=" || true
    echo; echo "PASS: mm2 -- isolation holds"
    ;;

mm3)
    say "mm3: keep_open lapses and the cryogenic default resumes"
    stand_up --allow "$GW:*" --remember "arp:12" --remember "$GW:0:12"
    first_contact
    type_in "ping -c1 -W4 $GW >/dev/null 2>&1 || true; echo aga\"in\""
    if froze_within 8; then echo "FAIL: the memory did not hold while standing"; exit 1; fi
    say "  waiting out the window (12s)"
    sleep 14
    type_in "ping -c1 -W4 $GW >/dev/null 2>&1 || true; echo la\"te\""
    froze_within 15 || { echo "FAIL: an expired memory still skipped the freeze"; exit 1; }
    echo "  the window closed on its own: the park froze again"
    echo; echo "PASS: mm3 -- self-expiry"
    ;;

mm4)
    say "mm4: a standing refusal answers live -- no churn"
    stand_up --allow "$GW:*" --remember "arp:600" --remember "$GW:0:600" --remember "$OFF:9:600"
    first_contact
    say "  first refused datagram: pays the freeze, plants the memory"
    type_in "echo x > /dev/udp/$OFF/9 || true; echo one\"-done\""
    pump_until 40 "lapsed id=.*off the allowlist" || { echo "FAIL: the first refusal never lapsed"; exit 1; }
    if [ -f "$M/state" ]; then "$BIN" thaw "$VM" >/dev/null 2>&1 || true; fi
    sleep 2
    lapses=$(ledger | grep -c "lapsed id=" || true)
    say "  second refused datagram: must lapse live"
    type_in "echo x > /dev/udp/$OFF/9 || true; echo two\"-done\""
    if froze_within 8; then echo "FAIL: a remembered refusal still froze the machine"; exit 1; fi
    now=$(ledger | grep -c "lapsed id=" || true)
    [ "$now" -gt "$lapses" ] || { echo "FAIL: no live lapse landed"; exit 1; }
    ledger | grep -q "lapsed id=.*off the allowlist" || { echo "FAIL: the why is not in the book"; exit 1; }
    echo "  refused instantly, the why on the record, the machine never stopped"
    echo; echo "PASS: mm4 -- the live refusal"
    ;;

mm5)
    say "mm5: the landing is witnessed and the file stands"
    stand_up --allow "$GW:*" --remember "arp:600" --remember "$GW:0:600"
    first_contact
    [ -s "$M/membrane-memory" ] || { echo "FAIL: no membrane-memory file"; exit 1; }
    book | grep -q "verb=membrane-memory" || { echo "FAIL: the landing is not witnessed"; exit 1; }
    book | grep "verb=membrane-memory" | grep -q "keep_open=600s" || { echo "FAIL: the witness names no window"; exit 1; }
    entries=$(book | grep -c "verb=membrane-memory" || true)
    echo "  $entries landing(s) in the audit book, the file on disk ($(wc -c < "$M/membrane-memory") bytes)"
    echo; echo "PASS: mm5 -- the engine-seam door, witnessed"
    ;;

mm6)
    say "mm6a: a malformed memory file is inert"
    "$BIN" create "$VM" --net world:$WORLD_PORT/udp >/dev/null
    printf 'not a frame at all' > "$M/membrane-memory"
    "$BIN" start "$VM" >/dev/null
    MOTOR_LOG="$CELLA_HOME/motor.log"
    "$ENG" motor --listen "127.0.0.1:$DIAL_PORT" --allow "$GW:*" > "$MOTOR_LOG" 2>&1 &
    MOTOR_PID=$!
    sleep 1
    "$ENG" "$VM" --dial "127.0.0.1:$DIAL_PORT" > "$CELLA_HOME/bridge.log" 2>&1 &
    BRIDGE_PID=$!
    sleep 2
    "$BIN" gateway "$VM" open >/dev/null
    sleep 2
    type_in "ping -c1 -W4 $GW >/dev/null 2>&1 || true; echo gar\"bage\""
    froze_within 15 || { echo "FAIL: a malformed file skipped a freeze"; exit 1; }
    echo "  garbage decodes to nothing: the park froze (fail-closed)"
    pump_until 40 "released id=" || { echo "FAIL: the pump never released"; exit 1; }
    if [ -f "$M/state" ]; then "$BIN" thaw "$VM" >/dev/null 2>&1 || true; fi
    kill "$BRIDGE_PID" 2>/dev/null || true; BRIDGE_PID=""
    kill "$MOTOR_PID" 2>/dev/null || true; MOTOR_PID=""
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    "$BIN" destroy "$VM" >/dev/null 2>&1 || true

    say "mm6b: a thaw does not resurrect an expired memory"
    stand_up --allow "$GW:*" --remember "arp:10" --remember "$GW:0:10"
    first_contact
    say "  freeze by the operator's hand, wait out the window, thaw"
    "$BIN" freeze "$VM" >/dev/null
    sleep 12
    "$BIN" thaw "$VM" >/dev/null
    sleep 2
    type_in "ping -c1 -W4 $GW >/dev/null 2>&1 || true; echo af\"ter\""
    froze_within 15 || { echo "FAIL: the thaw resurrected an expired memory"; exit 1; }
    echo "  expired stays expired: the fresh membrane read the same arithmetic"
    echo; echo "PASS: mm6 -- the fail-closed edges"
    ;;
esac
