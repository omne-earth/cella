#!/usr/bin/env bash
# The ledger backend's gate. See docs/NETWORK-MODEL.md, "The control
# plane", and TASKS.md phase 1.
#
# Part A: hold, one fetch parks, the ledger holds one operation with
# an id and both clocks; freeze and thaw leave it still held, not
# delivered; a release by id (the cella gateway verb) completes
# the fetch, and the
# ledger shows the same id parked and released -- never a phantom.
#
# Part B: two operations park in order; a decision for the second
# one first delivers nothing (its predecessor is undecided); a
# decision for the first then delivers both, in park order, and the
# ledger's Released order matches the Parked order.
set -euo pipefail

cd "$(dirname "$0")/../.."
BIN=target/release/cella
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
    rm -rf "$CELLA_HOME" "$WWW"
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

say "step 2: the valve closes, through the verb"
"$BIN" gateway "$VM" close >/dev/null
sleep 1

say "step 3: one fetch parks -- and the park is the freeze (one-shot)"
python3 -m http.server 8080 --bind "$HOST_IP" --directory "$WWW" >/dev/null 2>&1 & SRV1=$!
sleep 1
type_in "H=http://$HOST_IP"
type_in 'wget -q -O /dev/null $H:8080 && echo fetch-a-don"e" &'
STATE="$CELLA_HOME/machines/$VM/state"
wait_frozen() {
    local deadline=$((SECONDS + 20))
    until [ -f "$STATE" ]; do
        [ $SECONDS -lt $deadline ] || return 1
        sleep 1
    done
}
wait_frozen || { echo "FAIL: the machine did not freeze itself on the park"; exit 1; }
[ -s "$LEDGER" ] || { echo "FAIL: no ledger file at $LEDGER"; exit 1; }
echo "  parked; the machine froze itself (one-shot)"

say "step 4: the ledger holds one operation, with an id and both clocks"
DUMP=$("$BIN" --dump-ledger "$LEDGER")
echo "$DUMP" | sed "s/^/  /"
COUNT=$(echo "$DUMP" | grep -c "^parked ")
[ "$COUNT" = "1" ] || { echo "FAIL: expected exactly one parked operation (retransmits should join it), got $COUNT"; exit 1; }
echo "$DUMP" | grep -qE "^parked id=[0-9a-f]{32} " || { echo "FAIL: no well-formed id on the operation"; exit 1; }
echo "$DUMP" | grep -q "ip=$HOST_IP port=8080" || { echo "FAIL: the operation does not name the fetched destination"; exit 1; }
echo "$DUMP" | grep -q "guest_ns=[1-9]" || { echo "FAIL: no guest_ns on the operation"; exit 1; }
echo "$DUMP" | grep -q "host_ns=[1-9]" || { echo "FAIL: no host_ns on the operation"; exit 1; }
ID_A=$(id_of "$DUMP" "port=8080")
[ -n "$ID_A" ] || { echo "FAIL: could not read the operation's id"; exit 1; }

say "step 5: thaw with no decision -- the operation stays held, not delivered"
"$BIN" thaw "$VM" >/dev/null
sleep 2
not_yet "fetch-a-done" || { echo "FAIL: the fetch completed without a decision"; exit 1; }
DUMP=$("$BIN" --dump-ledger "$LEDGER")
echo "$DUMP" | grep -q "^released " && { echo "FAIL: something released without a decision"; exit 1; }
echo "  the ledger still shows only the parked operation; thaw delivered nothing"

say "step 6: release by id -- the fetch completes, and the ledger records it"
"$BIN" gateway "$VM" release "$ID_A" >/dev/null
wait_for "fetch-a-done" || { echo "FAIL: the released fetch did not complete"; exit 1; }
DUMP=$("$BIN" --dump-ledger "$LEDGER")
echo "$DUMP" | sed "s/^/  /"
echo "$DUMP" | grep -q "^released id=$ID_A " || { echo "FAIL: the ledger did not release $ID_A"; exit 1; }
RELEASED_IDS=$(echo "$DUMP" | grep "^released " | sed -n 's/^released id=\([0-9a-f]*\).*/\1/p')
PARKED_IDS=$(echo "$DUMP" | grep "^parked " | sed -n 's/^parked id=\([0-9a-f]*\).*/\1/p')
for rid in $RELEASED_IDS; do
    echo "$PARKED_IDS" | grep -q "^$rid$" || { echo "FAIL: released id $rid never appears as parked -- a phantom"; exit 1; }
done
echo "  released id matches the parked id; no phantom"

say "step 7: the valve persisted -- C parks and freezes; thaw; D parks and freezes"
python3 -m http.server 8081 --bind "$HOST_IP" --directory "$WWW" >/dev/null 2>&1 & SRV2=$!
python3 -m http.server 8082 --bind "$HOST_IP" --directory "$WWW" >/dev/null 2>&1 & SRV3=$!
sleep 1
# No signal anywhere below: the chronicle exists, thus the valve is
# closed across every thaw, and each new destination is one park,
# one self-freeze, one cycle of the ratchet.
type_in 'wget -q -O /dev/null $H:8081 && echo fetch-c-don"e" &'
wait_frozen || { echo "FAIL: C did not park-and-freeze (valve persistence)"; exit 1; }
"$BIN" thaw "$VM" >/dev/null
sleep 2
type_in 'wget -q -O /dev/null $H:8082 && echo fetch-d-don"e" &'
wait_frozen || { echo "FAIL: D did not park-and-freeze"; exit 1; }
"$BIN" thaw "$VM" >/dev/null
sleep 2
DUMP=$("$BIN" --dump-ledger "$LEDGER")
ID_C=$(id_of "$DUMP" "port=8081")
ID_D=$(id_of "$DUMP" "port=8082")
[ -n "$ID_C" ] && [ -n "$ID_D" ] || { echo "FAIL: could not read both operation ids"; exit 1; }
echo "  C=$ID_C D=$ID_D"

say "step 8: decide D first -- nothing delivers, D's predecessor C is undecided"
"$BIN" gateway "$VM" release "$ID_D" >/dev/null
sleep 2
not_yet "fetch-d-done" || { echo "FAIL: D delivered before its predecessor C resolved"; exit 1; }
DUMP=$("$BIN" --dump-ledger "$LEDGER")
echo "$DUMP" | grep -q "^released id=$ID_D " && { echo "FAIL: the ledger released D before C resolved"; exit 1; }
echo "  neither C nor D delivered; D's decision waits"

say "step 9: decide C -- both deliver, in park order"
"$BIN" gateway "$VM" release "$ID_C" >/dev/null
wait_for "fetch-c-done" || { echo "FAIL: C did not deliver once decided"; exit 1; }
wait_for "fetch-d-done" || { echo "FAIL: D did not deliver once its predecessor C resolved"; exit 1; }
DUMP=$("$BIN" --dump-ledger "$LEDGER")
C_LINE=$(echo "$DUMP" | grep -n "^released id=$ID_C " | head -1 | cut -d: -f1)
D_LINE=$(echo "$DUMP" | grep -n "^released id=$ID_D " | head -1 | cut -d: -f1)
[ -n "$C_LINE" ] && [ -n "$D_LINE" ] || { echo "FAIL: the ledger did not release both"; exit 1; }
[ "$C_LINE" -lt "$D_LINE" ] || { echo "FAIL: the Released order does not match the Parked order (C then D)"; exit 1; }
echo "  both delivered; released C before released D, matching park order"

echo
echo "PASS: the ledger names, holds, and releases by id, in park order"
"$BIN" stop "$VM" >/dev/null; "$BIN" destroy "$VM" >/dev/null
