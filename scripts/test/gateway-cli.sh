#!/usr/bin/env bash
# 1.3's gate: the cella gateway verbs drive the membrane -- close
# shuts the valve, show lists the held operation, release completes
# the fetch, refuse lapses one, and open states the one-way rule.
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
    rm -rf "$CELLA_HOME" "$WWW"
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
    local deadline=$((SECONDS + 20))
    until [ -f "$STATE" ]; do
        [ $SECONDS -lt $deadline ] || return 1
        sleep 1
    done
}

say "step 1: create (born closed), start, open the valve through the verb"
"$BIN" create "$VM" --net "$TAP" >/dev/null
grep -q '"valve": "closed"' "$CELLA_HOME/machines/$VM/manifest.json" || { echo "FAIL: not born closed"; exit 1; }
"$BIN" start "$VM" >/dev/null
sleep 5
VMM_PID=$(cat "$CELLA_HOME/machines/$VM/pid")
"$BIN" gateway "$VM" open >/dev/null || { echo "FAIL: open failed"; exit 1; }
sleep 1

say "step 2: a fetch parks and self-freezes; show lists the held operation"
python3 -m http.server 8080 --bind "$HOST_IP" --directory "$WWW" >/dev/null 2>&1 & SRV1=$!
sleep 1
type_in "H=http://$HOST_IP"
type_in 'wget -q -O /dev/null $H:8080 && echo fetch-a-don"e" &'
wait_frozen || { echo "FAIL: the machine did not freeze itself on the park"; exit 1; }
SHOW=$("$BIN" gateway "$VM" show)
echo "$SHOW" | sed "s/^/  /"
echo "$SHOW" | grep -q "$HOST_IP:8080.*held" || { echo "FAIL: show does not list the held operation"; exit 1; }
ID_A=$(echo "$SHOW" | grep "$HOST_IP:8080" | awk "{print \$1}")

say "step 3: release by id prefix; the thaw applies it"
"$BIN" gateway "$VM" release "${ID_A:0:16}" | grep -q "applies at thaw" || { echo "FAIL: release against the sleeping machine did not defer to the thaw"; exit 1; }
"$BIN" thaw "$VM" >/dev/null
wait_for "fetch-a-done" || { echo "FAIL: the released fetch did not complete"; exit 1; }
"$BIN" gateway "$VM" show --all | grep -q "released" || { echo "FAIL: show --all does not record the release"; exit 1; }
echo "  released by prefix; the fetch landed; the book records it"

say "step 4: a second operation, refused -- it lapses, and never delivers"
python3 -m http.server 8081 --bind "$HOST_IP" --directory "$WWW" >/dev/null 2>&1 & SRV2=$!
sleep 1
type_in 'wget -q -O /dev/null $H:8081 && echo fetch-b-don"e" &'
wait_frozen || { echo "FAIL: the second park did not freeze (valve persistence)"; exit 1; }
SHOW=$("$BIN" gateway "$VM" show)
ID_B=$(echo "$SHOW" | grep "$HOST_IP:8081" | awk "{print \$1}")
[ -n "$ID_B" ] || { echo "FAIL: show does not list the second operation"; exit 1; }
"$BIN" gateway "$VM" refuse "${ID_B:0:16}" --why "not part of this world" >/dev/null
"$BIN" thaw "$VM" >/dev/null
sleep 2
grep -aq "fetch-b-done" "$CON" && { echo "FAIL: a refused operation delivered"; exit 1; }
"$BIN" gateway "$VM" show --all | grep -q "lapsed (not part of this world)" || { echo "FAIL: the book does not record the lapse and its why"; exit 1; }
echo "  refused; nothing delivered; the book records the lapse"

say "step 5: close -- the machine blocks even the previously allowed flow"
"$BIN" gateway "$VM" close >/dev/null
sleep 1
type_in 'wget -q -T 4 -O /dev/null $H:8080 && echo fetch-c-don"e" &'
sleep 6
grep -aq "fetch-c-done" "$CON" && { echo "FAIL: a closed machine reached an allowed destination"; exit 1; }
[ -f "$STATE" ] && { echo "FAIL: a closed machine froze on egress"; exit 1; }
"$BIN" gateway "$VM" open >/dev/null
sleep 1
# No allow survives an epoch: the reopened fetch parks again, and
# the engine decides again -- atomically, every time.
type_in 'wget -q -O /dev/null $H:8080 && echo fetch-d-don"e" &'
wait_frozen || { echo "FAIL: the reopened fetch did not park and freeze"; exit 1; }
ID_D=$("$BIN" gateway "$VM" show | grep "$HOST_IP:8080" | awk "{print \$1}")
"$BIN" gateway "$VM" release "$ID_D" >/dev/null
"$BIN" thaw "$VM" >/dev/null
wait_for "fetch-d-done" || { echo "FAIL: the re-decided fetch did not complete"; exit 1; }
echo "  closed: dark even to the once-allowed; reopened: parked and decided afresh"

echo
echo "PASS: the gateway verbs drive the membrane"
"$BIN" stop "$VM" >/dev/null; "$BIN" destroy "$VM" >/dev/null
