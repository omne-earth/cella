#!/usr/bin/env bash
# smoke-collide: the matcher never guesses. A machine cannot mint two
# open operations under one key (a same-key frame joins), thus the
# gate plants the collision by surgery: it copies the chronicle's own
# Parked frame and patches the id, so the thaw finds two open
# operations under one key. The rebind then re-mints a fresh id and
# every ambiguous frame stays held -- none delivers. The engine
# refuses the stale ids, and the bookkeeping lapse closes the book at
# the next thaw edge: refused, and held by nothing. A collision costs
# a decision, never a leak.
set -uo pipefail

cd "$(dirname "$0")/../.."
BIN=target/smoke/cella
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build-smoke"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0
HOST_IP=$(ip -4 route get 1.1.1.1 2>/dev/null | grep -oP 'src \K[0-9.]+' | head -1); [ -n "$HOST_IP" ] || HOST_IP=127.0.0.1
command -v python3 >/dev/null || { echo "SKIP: python3 not found (the surgeon)"; exit 0; }

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-collide.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

VM=collide
M="$CELLA_HOME/machines/$VM"
LEDGER="$M/network/ledger"
teardown() {
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    p=$(cat "$M/pid" 2>/dev/null || true)
    [ -n "$p" ] && kill -9 "$p" 2>/dev/null || true
    if [ -n "${CELLA_KEEP_SANDBOX:-}" ]; then
        echo "kept: $CELLA_HOME"
    else
        rm -rf "$CELLA_HOME"
    fi
}
trap teardown EXIT
say() { echo; echo "==> $1"; }
wait_frozen() {
    local deadline=$((SECONDS + 20))
    until [ -f "$M/state" ]; do
        [ $SECONDS -lt $deadline ] || return 1
        sleep 1
    done
    local pid
    pid=$(cat "$M/pid" 2>/dev/null || true)
    while [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; do
        [ $SECONDS -lt $deadline ] || return 1
        sleep 0.2
    done
}
# The ear's live wire: release every held incoming operation -- mail
# moves without a thaw.
pump_mail() {
    for id in $("$BIN" gateway "$VM" show incoming | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
        "$BIN" gateway "$VM" release "$id" >/dev/null
    done
}

say "step 1: one real operation parks and freezes"
"$BIN" create "$VM" --net world:1709/tcp+1709/udp >/dev/null
"$BIN" start "$VM" >/dev/null
sleep 4
"$BIN" gateway "$VM" open >/dev/null
sleep 1
# A host ping no longer freezes the machine: it parks incoming (the
# world's knock is not the resident's deed). Release it live, and
# the guest's own reply parks in the egress lane -- that park is the
# freeze.
for _ in 1 2 3; do printf 'knock\n' > /dev/udp/127.0.0.1/1709 2>/dev/null || true; sleep 1; done
pump_mail
wait_frozen || { echo "FAIL: nothing parked and froze"; exit 1; }
ID_REAL=$("$BIN" --dump-ledger "$LEDGER" | grep '^parked .*dir=outgoing' | sed -n 's/^parked id=\([0-9a-f]*\) .*/\1/p' | tail -1)
[ -n "$ID_REAL" ] || { echo "FAIL: no parked operation in the chronicle"; exit 1; }
echo "  held: $ID_REAL"

say "step 2: the surgery -- a second open operation under the same key"
python3 - "$LEDGER" "$ID_REAL" <<'EOF'
import sys
path, rid = sys.argv[1], sys.argv[2]
raw = open(path, "rb").read()
old = bytes.fromhex(rid)
# The ear's mail parks and resolves before the real operation does
# now, thus its Parked frame is not first in the file: scan every
# varint-length-prefixed frame and take the one carrying this id.
pos, frame = 0, None
while pos < len(raw):
    i, shift, length = pos, 0, 0
    while True:
        b = raw[i]; length |= (b & 0x7F) << shift; i += 1; shift += 7
        if not b & 0x80: break
    candidate = raw[pos : i + length]
    if old in candidate:
        frame = candidate
        break
    pos = i + length
assert frame is not None, "the id bytes are not in any frame"
twin = bytearray(old); twin[-1] ^= 0xFF
open(path, "ab").write(frame.replace(old, bytes(twin)))
print("  planted twin id:", bytes(twin).hex())
EOF
ID_TWIN=$(printf '%s' "$ID_REAL" | python3 -c 'import sys; s=bytearray(bytes.fromhex(sys.stdin.read())); s[-1]^=0xFF; print(s.hex())')
COUNT=$("$BIN" --dump-ledger "$LEDGER" | grep -c "^parked .*dir=outgoing")
[ "$COUNT" = "2" ] || { echo "FAIL: the surgery did not take (parked=$COUNT)"; exit 1; }

say "step 3: the thaw over the collision -- the matcher never guesses"
"$BIN" thaw "$VM" >/dev/null
sleep 2
grep -aq "no unambiguous open operation at thaw" "$M/vmm.log" || { echo "FAIL: the rebind matched through an ambiguity"; exit 1; }
ID_FRESH=$("$BIN" --dump-ledger "$LEDGER" | grep '^parked .*dir=outgoing' | sed -n 's/^parked id=\([0-9a-f]*\) .*/\1/p' | tail -1)
[ "$ID_FRESH" != "$ID_REAL" ] && [ "$ID_FRESH" != "$ID_TWIN" ] || { echo "FAIL: no fresh id was minted"; exit 1; }
"$BIN" --dump-ledger "$LEDGER" | grep -qE "^released id=($ID_REAL|$ID_TWIN) " && { echo "FAIL: an ambiguous frame delivered"; exit 1; }
"$BIN" gateway "$VM" show | grep -q "^${ID_FRESH:0:16}.*held" || { echo "FAIL: the re-minted operation is not held"; exit 1; }
echo "  re-minted $ID_FRESH; every ambiguous frame stays held; none delivered"

say "step 4: the engine refuses the stale ids; the book closes them"
"$BIN" gateway "$VM" refuse "$ID_REAL" --why "collision melt: held by nothing" >/dev/null
"$BIN" gateway "$VM" refuse "$ID_TWIN" --why "collision melt: held by nothing" >/dev/null
"$BIN" freeze "$VM" >/dev/null 2>&1 || true
"$BIN" thaw "$VM" >/dev/null
sleep 2
D=$("$BIN" --dump-ledger "$LEDGER")
echo "$D" | grep -q "^lapsed id=$ID_REAL " || { echo "FAIL: the chronicle did not close $ID_REAL"; exit 1; }
echo "$D" | grep -q "^lapsed id=$ID_TWIN " || { echo "FAIL: the chronicle did not close $ID_TWIN"; exit 1; }
echo "$D" | grep -qE "^released id=($ID_REAL|$ID_TWIN) " && { echo "FAIL: something delivered during the bookkeeping"; exit 1; }
echo "  both stale ids lapsed by the book; nothing delivered"

echo
echo "PASS: a collision costs a decision, never a leak"
"$BIN" stop "$VM" >/dev/null 2>&1 || true
"$BIN" destroy "$VM" >/dev/null
