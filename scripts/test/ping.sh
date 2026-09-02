#!/usr/bin/env bash
# smoke-ping: the valve, end to end, under the total membrane. A
# machine is born closed: a host ping fails, and nothing freezes.
# Open turns the tap into the membrane: every egress frame parks --
# the guest's ARP reply first, then its echo reply -- and each park
# is a freeze. The pump (the stand-in engine) releases and thaws,
# one decision per operation, and a reply lands inside the ping's
# own wait window. Close returns the dark; a reopened valve
# remembers nothing. See docs/NETWORK-MODEL.md and
# docs/FREEZE-THAW.md, "The two automata".
set -uo pipefail

cd "$(dirname "$0")/../.."
BIN=target/smoke/cella
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build-smoke"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0
TAP="${CELLA_TEST_TAP:-tap0}"
HOST_IP="${CELLA_TEST_HOST_IP:-192.168.200.1}"
GUEST_IP="${CELLA_TEST_GUEST_IP:-192.168.200.2}"
if ! ip addr show "$TAP" 2>/dev/null | grep -q "$HOST_IP"; then
    echo "SKIP: $TAP is not configured with $HOST_IP -- run: cella doctor fix"
    exit 0
fi

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-ping.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

VM=pingtest
M="$CELLA_HOME/machines/$VM"
teardown() {
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    VMM_PID=$(cat "$M/pid" 2>/dev/null || true)
    [ -n "${VMM_PID:-}" ] && kill -9 "$VMM_PID" 2>/dev/null || true
    rm -rf "$CELLA_HOME"
}
trap teardown EXIT
say() { echo; echo "==> $1"; }
STATE="$M/state"
wait_frozen() {
    local deadline=$((SECONDS + 20))
    until [ -f "$STATE" ]; do
        [ $SECONDS -lt $deadline ] || return 1
        sleep 1
    done
}
# The stand-in engine: while the given pid runs, release every held
# operation of the frozen machine and thaw it -- one decision per
# operation, each park a fresh freeze.
pump_while() { # pid
    # The pump races the host's own ARP timers: a reply released
    # after the host's probe window closes is dropped as
    # unsolicited (arp_accept=0), thus the cadence stays tight.
    local cycles=0
    while kill -0 "$1" 2>/dev/null; do
        if [ -f "$STATE" ]; then
            pid=$(cat "$M/pid" 2>/dev/null || true)
            [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null && { sleep 0.2; continue; }
            for id in $("$BIN" gateway "$VM" show | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
                "$BIN" gateway "$VM" release "$id" >/dev/null
            done
            "$BIN" thaw "$VM" >/dev/null
            cycles=$((cycles + 1))
        fi
        sleep 0.2
    done
    echo "  ($cycles engine cycles)"
}

say "step 1: born closed -- the machine answers nothing"
"$BIN" create "$VM" --net "$TAP" >/dev/null
[ "$(cat "$M/valve")" = "closed" ] || { echo "FAIL: the valve record is not born closed"; exit 1; }
"$BIN" start "$VM" >/dev/null
sleep 4
VMM_PID=$(cat "$M/pid")
ping -c 2 -W 2 "$GUEST_IP" >/dev/null 2>&1 && { echo "FAIL: a closed machine answered a ping"; exit 1; }
[ -f "$STATE" ] && { echo "FAIL: a closed machine froze on inbound traffic"; exit 1; }
[ -f "$M/network/ledger" ] && { echo "FAIL: a closed machine wrote a chronicle"; exit 1; }
echo "  no reply, no freeze, no ledger: dark"

say "step 1b: the never-opened machine stays dark across freeze and thaw (negative)"
# The closed self-loop holds on every edge of the machine
# automaton, not only at birth: create, freeze, thaw -- and the
# machine still answers nothing, parks nothing, freezes on
# nothing.
"$BIN" freeze "$VM" >/dev/null
"$BIN" thaw "$VM" >/dev/null
sleep 2
ping -c 2 -W 2 "$GUEST_IP" >/dev/null 2>&1 && { echo "FAIL: a thawed never-opened machine answered a ping"; exit 1; }
[ -f "$STATE" ] && { echo "FAIL: a thawed never-opened machine froze on traffic"; exit 1; }
[ -f "$M/network/ledger" ] && { echo "FAIL: a thawed never-opened machine wrote a chronicle"; exit 1; }
echo "  dark through the whole life: nothing answered, nothing parked, nothing froze"

say "step 1c: a valve verb works against a frozen machine (positive)"
# The two automata are independent: the gateway verb acts on a
# sleeping machine, the posture holds, and the first post-thaw
# egress parks.
"$BIN" freeze "$VM" >/dev/null
"$BIN" gateway "$VM" open | grep -q "holds across freeze and thaw" || { echo "FAIL: open against a frozen machine did not state its persistence"; exit 1; }
[ "$(cat "$M/valve")" = "open" ] || { echo "FAIL: the valve record did not open against a frozen machine"; exit 1; }
"$BIN" gateway "$VM" close >/dev/null
"$BIN" thaw "$VM" >/dev/null
sleep 2

say "step 2: open -- every egress parks, and the park is the freeze"
"$BIN" gateway "$VM" open >/dev/null
sleep 1
# Three requests, not one: the live kick applies on the next
# run-loop pass, and a request that races it meets the closed
# drain and parks nothing.
ping -c 3 -W 2 "$GUEST_IP" >/dev/null 2>&1 && { echo "FAIL: an open machine answered without a decision"; exit 1; }
wait_frozen || { echo "FAIL: the parked egress did not freeze the machine"; exit 1; }
SHOW=$("$BIN" gateway "$VM" show)
echo "$SHOW" | sed "s/^/  /"
echo "$SHOW" | grep -qE "^[0-9a-f]{32} .*held$" || { echo "FAIL: show lists nothing held"; exit 1; }
echo "  the guest's first egress is held; the machine froze itself"

say "step 3: the engine decides, and a reply lands inside the ping's window"
ping -c 20 -i 1 -W 25 "$GUEST_IP" >/dev/null 2>&1 &
PING_PID=$!
pump_while "$PING_PID"
wait "$PING_PID" || { echo "FAIL: no reply landed while the engine decided"; exit 1; }
echo "  parked, frozen, decided, delivered -- the ratchet turned"

say "step 4: close -- the machine is dark again (negative)"
# The pump left the machine frozen or running; a closed valve needs
# a standing machine to prove its silence.
if [ -f "$STATE" ]; then "$BIN" thaw "$VM" >/dev/null; sleep 1; fi
"$BIN" gateway "$VM" close >/dev/null
sleep 1
ping -c 2 -W 2 "$GUEST_IP" >/dev/null 2>&1 && { echo "FAIL: a closed machine answered a ping"; exit 1; }
[ "$(cat "$M/valve")" = "closed" ] || { echo "FAIL: the valve record did not close"; exit 1; }
echo "  dark: close blocks even the previously decided path"

say "step 5: reopened, the valve remembers nothing (negative)"
"$BIN" gateway "$VM" open >/dev/null
sleep 1
ping -c 1 -W 3 "$GUEST_IP" >/dev/null 2>&1 && { echo "FAIL: a reopened machine answered without a fresh decision"; exit 1; }
wait_frozen || { echo "FAIL: the reopened machine did not park and freeze"; exit 1; }
echo "  reopened: the first egress parked and froze -- nothing was inherited"

echo
echo "PASS: fail, freeze, decide, reply, fail, remember nothing -- the valve holds"
"$BIN" destroy "$VM" >/dev/null 2>&1 || { "$BIN" stop "$VM" >/dev/null 2>&1 || true; "$BIN" destroy "$VM" >/dev/null; }
