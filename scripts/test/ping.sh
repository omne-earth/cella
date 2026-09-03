#!/usr/bin/env bash
# smoke-ping: the valve, end to end, under the total membrane with
# the ear's customs. A machine is born closed: a host ping fails,
# and nothing freezes. Open arms both directions: the host's echo
# request parks in the inbound lane -- and the machine keeps
# running, because the world's knock is not the machine's own action.
# Its release moves live (the ear's wire needs no thaw); the
# guest's reply then parks in the egress lane, and that park is the
# freeze. The pump decides both lanes, and a reply lands inside the
# ping's own wait window. Close returns the dark; a reopened valve
# remembers nothing, in either direction. See docs/NETWORK-MODEL.md
# and docs/FREEZE-THAW.md, "The two automata".
set -uo pipefail

cd "$(dirname "$0")/../.."
BIN=target/smoke/cella
# The knock port: random per run, so a leaked translator from an
# earlier gate (a stale bind on a fixed port swallows knocks
# silently) can never poison this one. Four digits, unprivileged.
WORLD_PORT=$(( (RANDOM % 8976) + 1024 ))
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build-smoke"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0
HOST_IP=$(ip -4 route get 1.1.1.1 2>/dev/null | grep -oP 'src \K[0-9.]+' | head -1); [ -n "$HOST_IP" ] || HOST_IP=127.0.0.1

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
    # destroy before rm: the translator is machine-lifetime, and an
    # rm alone orphans it -- a dead process holding the knock port
    # swallows the next gate's knocks.
    "$BIN" destroy "$VM" >/dev/null 2>&1 || true
    rm -rf "$CELLA_HOME"
}
trap teardown EXIT
say() { echo; echo "==> $1"; }
STATE="$M/state"
# The knock (1.6.14e): a datagram on the machine's mapped port
# $WORLD_PORT, from the host. The guest has no listener there, so an
# answered knock is the guest's ICMP port-unreachable reply -- an
# egress frame that parks outgoing. "answered" therefore means an
# outgoing park appeared after the knock; a closed valve shows none.
knock() { printf 'knock\n' > /dev/udp/127.0.0.1/$WORLD_PORT 2>/dev/null || true; }
outgoing_parks() { "$BIN" --dump-ledger "$M/network/ledger" 2>/dev/null | grep -c "dir=outgoing" || true; }
answered() { # <parks-before>
    sleep 3
    [ "$(outgoing_parks)" -gt "$1" ]
}
knock_loop() { # seconds -- keep knocking while the pump decides
    local end=$((SECONDS + $1))
    while [ $SECONDS -lt $end ]; do knock; sleep 1; done
}

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
        # The ear's live wire: mail moves without a thaw. Against a
        # frozen machine the release stages and the next thaw
        # applies it.
        for id in $("$BIN" gateway "$VM" show incoming | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
            "$BIN" gateway "$VM" release "$id" >/dev/null
        done
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
"$BIN" create "$VM" --net world:$WORLD_PORT/tcp+$WORLD_PORT/udp >/dev/null
[ "$(cat "$M/valve")" = "closed" ] || { echo "FAIL: the valve record is not born closed"; exit 1; }
"$BIN" start "$VM" >/dev/null
sleep 4
VMM_PID=$(cat "$M/pid")
B=$(outgoing_parks); knock; knock
answered "$B" && { echo "FAIL: a closed machine answered a knock"; exit 1; }
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
B=$(outgoing_parks); knock; knock
answered "$B" && { echo "FAIL: a thawed never-opened machine answered a knock"; exit 1; }
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

say "step 2: open -- the knock parks in the inbound lane, and the machine keeps running"
"$BIN" gateway "$VM" open >/dev/null
sleep 1
# Three requests, not one: the live kick applies on the next
# run-loop pass, and a request that races it meets the closed
# drain and parks nothing.
B=$(outgoing_parks); knock; knock
answered "$B" && { echo "FAIL: an open machine answered without a decision"; exit 1; }
[ -f "$STATE" ] && { echo "FAIL: the machine froze on inbound -- the world's knock is not the machine's own action"; exit 1; }
SHOW=$("$BIN" gateway "$VM" show incoming)
echo "$SHOW" | sed "s/^/  /"
echo "$SHOW" | grep -qE "^[0-9a-f]{32} .*held$" || { echo "FAIL: show incoming lists no held knock"; exit 1; }
"$BIN" gateway "$VM" show outgoing | grep -qE "^[0-9a-f]{32} .*held$" && { echo "FAIL: the knock leaked into the egress lane"; exit 1; }
echo "  the knock is held incoming; no freeze, and the lanes are separate"

say "step 2b: the released knock reaches the guest; the reply parks, and that park is the freeze"
ID_K=$("$BIN" gateway "$VM" show incoming | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p' | head -1)
"$BIN" gateway "$VM" release "$ID_K" | grep -q "applies now" || { echo "FAIL: the incoming release did not apply live"; exit 1; }
wait_frozen || { echo "FAIL: the guest's reply did not park and freeze"; exit 1; }
"$BIN" gateway "$VM" show outgoing | grep -qE "^[0-9a-f]{32} .*held$" || { echo "FAIL: show outgoing lists no held reply"; exit 1; }
echo "  mail moved live; the machine's own egress froze it"

say "step 3: the engine decides, and a reply lands inside the ping's window"
knock_loop 25 &
PING_PID=$!
pump_while "$PING_PID"
wait "$PING_PID" || true
"$BIN" --dump-ledger "$M/network/ledger" | grep -q "released id=.*bytes_out=[1-9]" || { echo "FAIL: no reply landed while the engine decided"; exit 1; }
echo "  parked, frozen, decided, delivered -- the ratchet turned"

say "step 4: close -- the machine is dark again (negative)"
# The pump left the machine frozen or running; a closed valve needs
# a standing machine to prove its silence.
if [ -f "$STATE" ]; then "$BIN" thaw "$VM" >/dev/null; sleep 1; fi
"$BIN" gateway "$VM" close >/dev/null
sleep 1
B=$(outgoing_parks); knock; knock
answered "$B" && { echo "FAIL: a closed machine answered a knock"; exit 1; }
[ "$(cat "$M/valve")" = "closed" ] || { echo "FAIL: the valve record did not close"; exit 1; }
echo "  dark: close blocks even the previously decided path"

say "step 5: reopened, the valve remembers nothing, in either direction (negative)"
"$BIN" gateway "$VM" open >/dev/null
sleep 1
B=$(outgoing_parks); knock; knock
answered "$B" && { echo "FAIL: a reopened machine answered without a fresh decision"; exit 1; }
[ -f "$STATE" ] && { echo "FAIL: a reopened machine froze on inbound"; exit 1; }
"$BIN" gateway "$VM" show incoming | grep -qE "^[0-9a-f]{32} .*held$" || { echo "FAIL: the reopened knock did not park afresh"; exit 1; }
echo "  reopened: the knock parked afresh, undelivered -- nothing was inherited"

echo
echo "PASS: fail, freeze, decide, reply, fail, remember nothing -- the valve holds"
"$BIN" destroy "$VM" >/dev/null 2>&1 || { "$BIN" stop "$VM" >/dev/null 2>&1 || true; "$BIN" destroy "$VM" >/dev/null; }
