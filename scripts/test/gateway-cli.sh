#!/usr/bin/env bash
# 1.3's gate: the cella gateway verbs drive the membrane -- show
# lists the held operations, release and refuse decide by id prefix
# and stage for the thaw, close returns the dark, and a reopened
# valve remembers nothing. Under the total membrane a fetch is many
# cycles (AC3 proves the full pump); this gate proves the verbs.
# See docs/NETWORK-MODEL.md, "The control plane".
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
export CELLA_HOME=$(mktemp -d /tmp/cella-gwcli.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

VM=gwcli
WWW=$(mktemp -d); echo world > "$WWW/index.html"
SRV1=""; SRV2=""
pkill -f "http.server (8080|8081) --bind $HOST_IP" 2>/dev/null || true
teardown() {
    kill $SRV1 $SRV2 2>/dev/null || true
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
STATE="$CELLA_HOME/machines/$VM/state"
type_in() { (printf '%s\n' "$1"; sleep 2) | timeout 20 "$BIN" enter "$VM" >/dev/null; }
wait_for() {
    local marker="$1" deadline=$((SECONDS + 15))
    while [ $SECONDS -lt $deadline ]; do
        grep -aq "$marker" "$CON" && return 0
        sleep 1
    done
    return 1
}
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
first_held() { "$BIN" gateway "$VM" show outgoing | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p' | head -1; }
# The ear's live wire: release every held incoming operation -- mail
# moves without a thaw. The guest's own ARP reply is mail now, and
# without it the guest never gets far enough to send its next frame.
pump_mail() {
    for id in $("$BIN" gateway "$VM" show incoming | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
        "$BIN" gateway "$VM" release "$id" >/dev/null || true
    done
}
wait_frozen_mail() {
    local deadline=$((SECONDS + 25))
    until [ -f "$STATE" ]; do
        pump_mail
        [ $SECONDS -lt $deadline ] || return 1
        sleep 0.5
    done
    local pid
    pid=$(cat "$CELLA_HOME/machines/$VM/pid" 2>/dev/null || true)
    while [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; do
        [ $SECONDS -lt $deadline ] || return 1
        sleep 0.2
    done
}

say "step 1: create (born closed), start, open the valve through the verb"
"$BIN" create "$VM" --net "$TAP" >/dev/null
[ "$(cat "$CELLA_HOME/machines/$VM/valve")" = "closed" ] || { echo "FAIL: the valve record is not born closed"; exit 1; }
"$BIN" start "$VM" >/dev/null
sleep 5
VMM_PID=$(cat "$CELLA_HOME/machines/$VM/pid")
"$BIN" gateway "$VM" open >/dev/null || { echo "FAIL: open failed"; exit 1; }
sleep 1

say "step 2: a fetch parks and self-freezes; show lists the held operation"
python3 -m http.server 8080 --bind "$HOST_IP" --directory "$WWW" >/dev/null 2>&1 & SRV1=$!
sleep 1
type_in "H=http://$HOST_IP"
type_in 'wget -q -T 60 -O /dev/null $H:8080 && echo fetch-a-don"e" &'
wait_frozen || { echo "FAIL: the machine did not freeze itself on the park"; exit 1; }
"$BIN" gateway "$VM" show | sed "s/^/  /"
ID_A=$(first_held)
[ -n "$ID_A" ] || { echo "FAIL: show lists nothing held"; exit 1; }

say "step 3: release by id prefix; the decision stages for the thaw"
"$BIN" gateway "$VM" release "${ID_A:0:16}" | grep -q "applies at the thaw" || { echo "FAIL: the release did not state its staging"; exit 1; }
"$BIN" thaw "$VM" >/dev/null
sleep 2
"$BIN" gateway "$VM" show --all | grep -q "^${ID_A:0:16}.*released" || { echo "FAIL: show --all does not record the release"; exit 1; }
echo "  released by prefix, staged, applied at the thaw; the book records it"

say "step 4: the next operation, refused -- it lapses, and never delivers"
wait_frozen_mail || { echo "FAIL: the fetch's next frame did not park and freeze (valve persistence)"; exit 1; }
ID_B=$(first_held)
[ -n "$ID_B" ] || { echo "FAIL: show lists no second operation"; exit 1; }
"$BIN" gateway "$VM" refuse "${ID_B:0:16}" --why "not part of this world" >/dev/null
"$BIN" thaw "$VM" >/dev/null
sleep 2
grep -aq "fetch-a-done" "$CON" && { echo "FAIL: a refused flow completed its fetch"; exit 1; }
"$BIN" gateway "$VM" show --all | grep -q "lapsed (not part of this world)" || { echo "FAIL: the book does not record the lapse and its why"; exit 1; }
echo "  refused; nothing delivered; the book records the lapse"

say "step 5: close -- the machine is dark, and a reopened valve remembers nothing"
if [ -f "$STATE" ]; then "$BIN" thaw "$VM" >/dev/null; sleep 1; fi
"$BIN" gateway "$VM" close >/dev/null
[ "$(cat "$CELLA_HOME/machines/$VM/valve")" = "closed" ] || { echo "FAIL: the valve record did not close"; exit 1; }
sleep 1
type_in 'wget -q -T 4 -O /dev/null $H:8080 && echo fetch-c-don"e" &'
sleep 6
grep -aq "fetch-c-done" "$CON" && { echo "FAIL: a closed machine reached a past destination"; exit 1; }
[ -f "$STATE" ] && { echo "FAIL: a closed machine froze on egress"; exit 1; }
"$BIN" gateway "$VM" open >/dev/null
sleep 1
# Nothing is inherited: the reopened machine's next egress parks
# and freezes, and the engine decides again -- atomically, every
# time.
type_in 'wget -q -T 30 -O /dev/null $H:8080 && echo fetch-d-don"e" &'
wait_frozen || { echo "FAIL: the reopened fetch did not park and freeze"; exit 1; }
echo "  closed: dark even to the once-decided; reopened: parked afresh"

echo
echo "PASS: the gateway verbs drive the membrane"
"$BIN" stop "$VM" >/dev/null 2>&1 || true
"$BIN" destroy "$VM" >/dev/null
