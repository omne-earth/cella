#!/usr/bin/env bash
# The ledger backend's gate. See docs/NETWORK-MODEL.md, "The control
# plane", and tasks/PHASE1.md phase 1.
#
# Part A: under the total membrane a fetch's first egress is the
# guest's ARP -- it parks as an L2 operation with an id and both
# clocks; its release resolves the neighbor, and the SYN parks
# next, refined to address and port. A thaw with no decision keeps
# everything held; a release by id records in the chronicle, and
# every released id was first parked -- never a phantom.
#
# Part B: two operations park in one batch, in order; a decision
# for the second one first applies nothing (its predecessor is
# undecided); a decision for the first then applies both, in park
# order, and the Released order matches the Parked order.
set -euo pipefail

cd "$(dirname "$0")/../.."
BIN=target/smoke/cella
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0
TAP="${CELLA_TEST_TAP:-tap0}"
HOST_IP="${CELLA_TEST_HOST_IP:-192.168.200.1}"
if ! ip addr show "$TAP" 2>/dev/null | grep -q "$HOST_IP"; then
    echo "SKIP: $TAP is not configured with $HOST_IP -- run: cella doctor fix"
    exit 0
fi
command -v python3 >/dev/null || { echo "SKIP: python3 not found (the stand-in endpoints)"; exit 0; }

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-ledger.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

VM=ledgertest
WWW=$(mktemp -d); echo world > "$WWW/index.html"
SRV1=""; SRV2=""; SRV3=""
# A stand-in endpoint leaked by an interrupted run squats its port.
pkill -f "http.server (8080|8081|8082) --bind $HOST_IP" 2>/dev/null || true
teardown() {
    kill $SRV1 $SRV2 $SRV3 2>/dev/null || true
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    [ -n "${VMM_PID:-}" ] && kill -9 "$VMM_PID" 2>/dev/null || true
    if [ -n "${CELLA_KEEP_SANDBOX:-}" ]; then
        echo "kept: $CELLA_HOME"
        rm -rf "$WWW"
    else
        rm -rf "$CELLA_HOME" "$WWW"
    fi
}
trap teardown EXIT
say() { echo; echo "==> $1"; }
CON="$CELLA_HOME/machines/$VM/console.log"
type_in() { (printf '%s\n' "$1"; sleep 2) | timeout 20 "$BIN" enter "$VM" >/dev/null; }
wait_for() {
    local marker="$1" deadline=$((SECONDS + 15))
    while [ $SECONDS -lt $deadline ]; do
        grep -aq "$marker" "$CON" && return 0
        sleep 1
    done
    return 1
}
not_yet() { # marker -- true if the marker has NOT appeared
    ! grep -aq "$1" "$CON"
}
LEDGER="$CELLA_HOME/machines/$VM/network/ledger"
VERDICT="$CELLA_HOME/machines/$VM/verdict"
id_of() { # dump destination-substring -- the last matching parked id
    echo "$1" | grep "^parked .*$2" | tail -1 | sed -n 's/^parked id=\([0-9a-f]*\) .*/\1/p'
}

say "step 1: create and start a machine on $TAP"
"$BIN" create "$VM" --net "$TAP" >/dev/null
"$BIN" start "$VM" >/dev/null
sleep 6
VMM_PID=$(cat "$CELLA_HOME/machines/$VM/pid")

say "step 2: the valve opens, through the verb -- the membrane arms"
"$BIN" gateway "$VM" open >/dev/null
sleep 1

say "step 3: one fetch parks its ARP first -- and the park is the freeze (one-shot)"
python3 -m http.server 8080 --bind "$HOST_IP" --directory "$WWW" >/dev/null 2>&1 & SRV1=$!
sleep 1
type_in "H=http://$HOST_IP"
type_in 'wget -q -O /dev/null $H:8080 && echo fetch-a-don"e" &'
STATE="$CELLA_HOME/machines/$VM/state"
wait_frozen() {
    # The sidecar lands before the old VMM exits: wait for both.
    local deadline=$((SECONDS + 20))
    until [ -f "$STATE" ]; do
        [ $SECONDS -lt $deadline ] || return 1
        sleep 1
    done
    local pid
    pid=$(cat "$CELLA_HOME/machines/$VM/pid" 2>/dev/null || true)
    while [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; do
        [ $SECONDS -lt $deadline ] || return 1
        sleep 0.2
    done
}
wait_frozen || { echo "FAIL: the machine did not freeze itself on the park"; exit 1; }
# The ear's live wire: release every held incoming operation -- mail
# moves without a thaw.
pump_mail() {
    for id in $("$BIN" gateway "$VM" show incoming | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
        "$BIN" gateway "$VM" release "$id" >/dev/null || true
    done
}
# Pump mail every half second until the machine self-freezes on its
# own next egress park, or the window closes. A single end-of-window
# pump can miss the reply by up to its own sleep: the guest's real
# clock runs while this window is open (freezes alone are timeless),
# and a miss here reads to the guest as a dead peer worth retrying --
# the retry, not a stall, is what a coarse pump produces.
pump_while_running() { # seconds
    local deadline=$((SECONDS + $1))
    until [ -f "$STATE" ] || [ $SECONDS -ge $deadline ]; do
        pump_mail
        sleep 0.5
    done
}
[ -s "$LEDGER" ] || { echo "FAIL: no ledger file at $LEDGER"; exit 1; }
echo "  parked; the machine froze itself (one-shot)"

say "step 4: the ledger holds one L2 operation, with an id and both clocks"
DUMP=$("$BIN" --dump-ledger "$LEDGER")
echo "$DUMP" | sed "s/^/  /"
COUNT=$(echo "$DUMP" | grep -c "^parked .*dir=outgoing")
[ "$COUNT" = "1" ] || { echo "FAIL: expected exactly one parked operation (the ARP; retransmits join), got $COUNT"; exit 1; }
echo "$DUMP" | grep -qE "^parked id=[0-9a-f]{32} " || { echo "FAIL: no well-formed id on the operation"; exit 1; }
echo "$DUMP" | grep -q "l2=arp" || { echo "FAIL: the first operation is not the ARP at its primitive name"; exit 1; }
echo "$DUMP" | grep -q "guest_ns=[1-9]" || { echo "FAIL: no guest_ns on the operation"; exit 1; }
echo "$DUMP" | grep -q "host_ns=[1-9]" || { echo "FAIL: no host_ns on the operation"; exit 1; }

say "step 5: thaw with no decision -- the operation stays held, not delivered"
"$BIN" thaw "$VM" >/dev/null
sleep 1
not_yet "fetch-a-done" || { echo "FAIL: the fetch completed without a decision"; exit 1; }
DUMP=$("$BIN" --dump-ledger "$LEDGER")
echo "$DUMP" | grep -q "^released " && { echo "FAIL: something released without a decision"; exit 1; }
echo "  the ledger still shows only held operations; the thaw delivered nothing"

say "step 6: release the ARP; the engine cycles until the SYN parks, refined -- no phantom"
ID_ARP=$(echo "$DUMP" | grep "^parked .*l2=arp" | tail -1 | sed -n 's/^parked id=\([0-9a-f]*\) .*/\1/p')
[ -n "$ID_ARP" ] || { echo "FAIL: could not read the ARP operation's id"; exit 1; }
# Each cycle: decide everything held, thaw, and watch for the
# fetch to land -- the SYN parks refined on the way (the budget
# bounds the ratchet, AC3-style).
cycles=0
until grep -aq "fetch-a-done" "$CON"; do
    cycles=$((cycles + 1))
    [ $cycles -le 10 ] || { echo "FAIL: the fetch did not land within 10 engine cycles"; exit 1; }
    "$BIN" freeze "$VM" >/dev/null 2>&1 || true
    for id in $("$BIN" gateway "$VM" show | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
        "$BIN" gateway "$VM" release "$id" >/dev/null
    done
    "$BIN" thaw "$VM" >/dev/null
    # The ARP's reply (and any other mail) is not a decision this
    # loop staged for a thaw -- it is the ear's own live wire, and
    # the guest needs it moved before its next frame can exist.
    pump_while_running 6
done
"$BIN" --dump-ledger "$LEDGER" | grep -q "^parked .*ip=$HOST_IP port=8080" || { echo "FAIL: the SYN never parked refined to its destination"; exit 1; }
DUMP=$("$BIN" --dump-ledger "$LEDGER")
echo "$DUMP" | sed "s/^/  /"
echo "$DUMP" | grep -q "^released id=$ID_ARP " || { echo "FAIL: the ledger did not release $ID_ARP"; exit 1; }
echo "  ($cycles engine cycles)"
ID_A=$(id_of "$DUMP" "port=8080")
RELEASED_IDS=$(echo "$DUMP" | grep "^released " | sed -n 's/^released id=\([0-9a-f]*\).*/\1/p')
PARKED_IDS=$(echo "$DUMP" | grep "^parked " | sed -n 's/^parked id=\([0-9a-f]*\).*/\1/p')
for rid in $RELEASED_IDS; do
    echo "$PARKED_IDS" | grep -q "^$rid$" || { echo "FAIL: released id $rid never appears as parked -- a phantom"; exit 1; }
done
echo "  released id matches the parked id; no phantom"

say "step 7: the valve persisted -- C and D park in one batch, in order"
# Settle flow A's tail (its FIN and teardown park too): decide
# everything until the machine stands quiet with nothing held.
for _ in 1 2 3 4; do
    "$BIN" freeze "$VM" >/dev/null 2>&1 || true
    sleep 1
    for id in $("$BIN" gateway "$VM" show | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
        # Best-effort settling: an id can lapse between the show and
        # the release (the tail is still moving), and that is not a
        # failure of the settle.
        "$BIN" gateway "$VM" release "$id" >/dev/null 2>&1 || true
    done
    "$BIN" thaw "$VM" >/dev/null 2>&1 || true
    pump_while_running 3
done
# Both new fetches park in one typed line: two operations, one
# batch, one self-freeze. The chronicle exists, thus the valve is
# open across every thaw until the closing verb.
type_in 'for p in 8081 8082; do wget -q $H:$p & done'
wait_frozen || { echo "FAIL: C and D did not park-and-freeze (valve persistence)"; exit 1; }
# The guest may freeze on C's batch before D's sender ran: thaw
# (deciding nothing) until both operations stand held, in order.
tries=0
until "$BIN" --dump-ledger "$LEDGER" | grep -q "port=8082"; do
    tries=$((tries + 1))
    [ $tries -le 4 ] || { echo "FAIL: D never parked"; exit 1; }
    "$BIN" thaw "$VM" >/dev/null
    wait_frozen || { echo "FAIL: the machine did not refreeze while D was due"; exit 1; }
done
DUMP=$("$BIN" --dump-ledger "$LEDGER")
ID_C=$(id_of "$DUMP" "port=8081")
ID_D=$(id_of "$DUMP" "port=8082")
[ -n "$ID_C" ] && [ -n "$ID_D" ] || { echo "FAIL: could not read both operation ids"; exit 1; }
echo "  C=$ID_C D=$ID_D"

say "step 8: decide D first -- nothing applies, D's predecessor C is undecided"
"$BIN" gateway "$VM" release "$ID_D" >/dev/null
"$BIN" thaw "$VM" >/dev/null
sleep 2
DUMP=$("$BIN" --dump-ledger "$LEDGER")
echo "$DUMP" | grep -q "^released id=$ID_D " && { echo "FAIL: the ledger released D before C resolved"; exit 1; }
echo "  D's decision waits behind the undecided C"

say "step 9: decide C -- both apply, in park order"
"$BIN" gateway "$VM" release "$ID_C" >/dev/null
"$BIN" freeze "$VM" >/dev/null 2>&1 || true
"$BIN" thaw "$VM" >/dev/null
sleep 2
DUMP=$("$BIN" --dump-ledger "$LEDGER")
echo "$DUMP" | grep -q "^released id=$ID_C " || { echo "FAIL: the ledger did not release C"; exit 1; }
echo "$DUMP" | grep -q "^released id=$ID_D " || { echo "FAIL: the ledger did not release D once its predecessor resolved"; exit 1; }
C_LINE=$(echo "$DUMP" | grep -n "^released id=$ID_C " | head -1 | cut -d: -f1)
D_LINE=$(echo "$DUMP" | grep -n "^released id=$ID_D " | head -1 | cut -d: -f1)
[ -n "$C_LINE" ] && [ -n "$D_LINE" ] || { echo "FAIL: the ledger did not release both"; exit 1; }
[ "$C_LINE" -lt "$D_LINE" ] || { echo "FAIL: the Released order does not match the Parked order (C then D)"; exit 1; }
echo "  both applied; released C before released D, matching park order"

echo
echo "PASS: the ledger names, holds, and releases by id, in park order"
"$BIN" stop "$VM" >/dev/null 2>&1 || true
"$BIN" destroy "$VM" >/dev/null
