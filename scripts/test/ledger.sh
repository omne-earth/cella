#!/usr/bin/env bash
# 1.2's gate: hold, one fetch parks, the ledger holds one operation
# with an id and both clocks. See docs/NETWORK-MODEL.md, "The
# control plane", and TASKS.md phase 1.
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
command -v python3 >/dev/null || { echo "SKIP: python3 not found (the stand-in endpoint)"; exit 0; }

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-ledger.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

VM=ledgertest
WWW=$(mktemp -d); echo world > "$WWW/index.html"
SRV=""
# A stand-in endpoint leaked by an interrupted run squats its port.
pkill -f "http.server 8080 --bind $HOST_IP" 2>/dev/null || true
teardown() {
    kill $SRV 2>/dev/null || true
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
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
LEDGER="$CELLA_HOME/machines/$VM/network/ledger"

say "step 1: create and start a machine on $TAP"
"$BIN" create "$VM" --net "$TAP" >/dev/null
"$BIN" start "$VM" >/dev/null
sleep 6
VMM_PID=$(cat "$CELLA_HOME/machines/$VM/pid")

say "step 2: egress hold on"
kill -USR2 "$VMM_PID"
sleep 1

say "step 3: one fetch parks (its SYN retransmits join the same operation)"
python3 -m http.server 8080 --bind "$HOST_IP" --directory "$WWW" >/dev/null 2>&1 & SRV=$!
sleep 1
type_in "H=http://$HOST_IP"
type_in 'wget -q -O /dev/null $H:8080 & echo park-triggere"d"'
wait_for "park-triggered" || { echo "FAIL: could not trigger the fetch"; exit 1; }
deadline=$((SECONDS + 15))
while [ ! -s "$LEDGER" ] && [ $SECONDS -lt $deadline ]; do sleep 1; done
[ -s "$LEDGER" ] || { echo "FAIL: no ledger file at $LEDGER"; exit 1; }

say "step 4: the ledger holds one operation, with an id and both clocks"
DUMP=$("$BIN" --dump-ledger "$LEDGER")
echo "$DUMP" | sed "s/^/  /"
COUNT=$(echo "$DUMP" | grep -c "^parked ")
[ "$COUNT" = "1" ] || { echo "FAIL: expected exactly one parked operation (retransmits should join it), got $COUNT"; exit 1; }
echo "$DUMP" | grep -qE "^parked id=[0-9a-f]{32} " || { echo "FAIL: no well-formed id on the operation"; exit 1; }
echo "$DUMP" | grep -q "ip=$HOST_IP port=8080" || { echo "FAIL: the operation does not name the fetched destination"; exit 1; }
echo "$DUMP" | grep -q "guest_ns=[1-9]" || { echo "FAIL: no guest_ns on the operation"; exit 1; }
echo "$DUMP" | grep -q "host_ns=[1-9]" || { echo "FAIL: no host_ns on the operation"; exit 1; }

echo
echo "PASS: the ledger holds one operation, with an id and both clocks"
"$BIN" stop "$VM" >/dev/null; "$BIN" destroy "$VM" >/dev/null
