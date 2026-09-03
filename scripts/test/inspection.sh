#!/usr/bin/env bash
# smoke-inspection: judgment requires sight, and sight requires
# stillness. The gate is CLI operations alone -- park, inspect,
# show, refuse, thaw -- nothing else. A held operation's plaintext
# renders; a high-entropy payload renders as the sealed envelope it
# is; the look lands in the chronicle as an Inspected event and
# resolves nothing; a running machine refuses the look; a resolved
# id has nothing to see; and the machine thaws unharmed after every
# look.
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
export CELLA_HOME=$(mktemp -d /tmp/cella-inspection.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

VM=looked
M="$CELLA_HOME/machines/$VM"
LEDGER="$M/network/ledger"
teardown() {
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    p=$(cat "$M/pid" 2>/dev/null || true)
    [ -n "$p" ] && kill -9 "$p" 2>/dev/null || true
    rm -rf "$CELLA_HOME"
}
trap teardown EXIT
say() { echo; echo "==> $1"; }
wait_frozen() {
    local deadline=$((SECONDS + 25))
    until [ -f "$M/state" ]; do
        # Mail moves while we wait: the world's answers park
        # incoming and never freeze; release them live.
        for id in $("$BIN" gateway "$VM" show incoming | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
            "$BIN" gateway "$VM" release "$id" >/dev/null
        done
        [ $SECONDS -lt $deadline ] || return 1
        sleep 0.5
    done
    local pid
    pid=$(cat "$M/pid" 2>/dev/null || true)
    while [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; do
        [ $SECONDS -lt $deadline ] || return 1
        sleep 0.2
    done
}
type_in() { (printf '%s\n' "$1"; sleep 3) | timeout 15 "$BIN" enter "$VM" >/dev/null 2>&1 || true; }

say "step 1: a plaintext datagram parks, and the ARP before it is decided"
"$BIN" create "$VM" --net world:$WORLD_PORT/tcp+$WORLD_PORT/udp >/dev/null
"$BIN" start "$VM" >/dev/null
sleep 4
"$BIN" gateway "$VM" open >/dev/null
sleep 1
type_in "echo the-crown-jewels-42 > /dev/udp/$HOST_IP/9053"
wait_frozen || { echo "FAIL: the first egress did not park and freeze"; exit 1; }
ID_ARP=$("$BIN" gateway "$VM" show outgoing | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p' | head -1)
"$BIN" gateway "$VM" release "$ID_ARP" >/dev/null
"$BIN" thaw "$VM" >/dev/null
wait_frozen || { echo "FAIL: the datagram did not park after the resolution"; exit 1; }
ID_U=$("$BIN" gateway "$VM" show outgoing | grep ":9053" | awk '{print $1}' | head -1)
[ -n "$ID_U" ] || { echo "FAIL: show lists no held datagram"; exit 1; }
echo "  held: $ID_U"

say "step 2: inspect renders the plaintext, and the look is witnessed"
LOOK=$("$BIN" gateway "$VM" inspect "$ID_U")
echo "$LOOK" | sed 's/^/  /'
# The ascii gutter wraps at sixteen bytes: one of the two
# disjoint fragments survives any single line boundary.
echo "$LOOK" | grep -q -e "crown" -e "jewels" || { echo "FAIL: the payload did not render"; exit 1; }
"$BIN" --dump-ledger "$LEDGER" | grep -q "^inspected id=$ID_U" || { echo "FAIL: no Inspected event in the chronicle"; exit 1; }
"$BIN" gateway "$VM" show outgoing | grep -q "^$ID_U.*held" || { echo "FAIL: the look resolved the operation"; exit 1; }
echo "  the payload rendered; the chronicle records the look; the hold stands"

say "step 3: a running machine refuses the look, and the judge freezes first"
"$BIN" thaw "$VM" >/dev/null || { echo "FAIL: the machine did not thaw after the look"; exit 1; }
sleep 1
"$BIN" gateway "$VM" inspect "$ID_U" >/dev/null 2>&1 && { echo "FAIL: a running machine accepted the look"; exit 1; }
MSG=$("$BIN" gateway "$VM" inspect "$ID_U" 2>&1 || true)
echo "$MSG" | grep -q "stillness" || { echo "FAIL: the refusal does not state the stillness rule"; exit 1; }
# The judge who wants sight freezes first: one machine verb, and
# the look works again against the same hold.
"$BIN" freeze "$VM" >/dev/null
"$BIN" gateway "$VM" inspect "$ID_U" | grep -q -e "crown" -e "jewels" || { echo "FAIL: the look did not work after the judge froze"; exit 1; }
echo "  running refuses; frozen by the judge's own verb, the same hold renders"

say "step 4: a sealed envelope renders as what it is"
# The held operation is decided before the machine dwells running
# again: an undecided egress across a long running stretch wedges
# the guest's TX queue (the seam's documented time bomb).
"$BIN" gateway "$VM" refuse "$ID_U" --why "seen enough" >/dev/null
"$BIN" thaw "$VM" >/dev/null
sleep 1
type_in "head -c 300 /dev/urandom > /dev/udp/$HOST_IP/9053"
wait_frozen || { echo "FAIL: the sealed datagram did not park and freeze"; exit 1; }
ID_S=$("$BIN" gateway "$VM" show outgoing | grep ":9053.*held" | awk '{print $1}' | head -1)
[ -n "$ID_S" ] || { echo "FAIL: show lists no held sealed datagram"; exit 1; }
LOOK=$("$BIN" gateway "$VM" inspect "$ID_S")
echo "$LOOK" | grep -q "sealed envelope" || { echo "FAIL: a high-entropy payload rendered as sight"; exit 1; }
echo "  the envelope stays sealed: entropy named, bytes withheld"

say "step 5: a resolved id has nothing to see (negative)"
"$BIN" gateway "$VM" inspect "$ID_U" >/dev/null 2>&1 && { echo "FAIL: a resolved operation accepted the look"; exit 1; }
echo "  refused, lapsed, and no longer visible: nothing held under the id"

echo
echo "PASS: sight requires stillness, the look is witnessed, and the envelope stays sealed"
"$BIN" stop "$VM" >/dev/null 2>&1 || true
"$BIN" destroy "$VM" >/dev/null
