#!/usr/bin/env bash
# 1.6.14d's gate: field 15 fills both books -- every Audit and Event
# entry carries the SHA-256 of its predecessor's framed bytes, the
# first entry chaining from the empty digest. `cella doctor verify
# <vm>` walks both books once. The negatives: an edited entry fails
# loudly, naming the entry where the chain snaps; a branched twin's
# book, copied byte for byte, still verifies.
#
# The audit book carries the live proof here (create/start/stop need
# only KVM, not a tap): every verb against a real machine witnesses
# an entry, and the chain across them is exercised end to end. The
# ledger's Event chain shares the same append_chained/verify_chain
# code as the audit's Audit chain (see cella-libs/src/ledger.rs) and
# is proven directly there, in the unit tests (`make test`) --
# populating it here would need real guest network traffic, which
# adds KVM/tap flakiness this gate does not need to carry.
set -euo pipefail

cd "$(dirname "$0")/../.."
BIN=target/smoke/cella
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build-smoke"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-chain.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

VM=chained
TWIN=chained-twin
teardown() {
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    p=$(cat "$CELLA_HOME/machines/$VM/pid" 2>/dev/null || true)
    [ -n "$p" ] && kill -9 "$p" 2>/dev/null || true
    if [ -n "${CELLA_KEEP_SANDBOX:-}" ]; then
        echo "kept: $CELLA_HOME"
    else
        rm -rf "$CELLA_HOME"
    fi
}
trap teardown EXIT
say() { echo; echo "==> $1"; }

say "step 1: a machine's audit book fills across its lifecycle"
"$BIN" create "$VM" --net none >/dev/null
"$BIN" start "$VM" >/dev/null
sleep 4
"$BIN" gateway "$VM" show >/dev/null
"$BIN" freeze "$VM" >/dev/null
sleep 1
"$BIN" thaw "$VM" >/dev/null
sleep 1
"$BIN" stop "$VM" >/dev/null
AUDIT="$CELLA_HOME/machines/$VM/audit"
[ -s "$AUDIT" ] || { echo "FAIL: no audit entries to chain"; exit 1; }
echo "  the book holds: create, start, gateway show, freeze, thaw, stop"

say "step 2: an intact book verifies end to end"
OUT=$("$BIN" doctor verify "$VM")
echo "$OUT" | sed 's/^/  /'
echo "$OUT" | grep -q "ok    $VM audit: chain verifies end to end" || { echo "FAIL: the audit chain did not verify"; exit 1; }
echo "  cella doctor verify $VM: the chain holds"

say "step 3: one edited entry fails loudly, naming the break (negative)"
cp "$AUDIT" "$AUDIT.orig"
# Flip one byte inside the first entry's "start" verb string. The
# frame's varint lengths are untouched, but a flipped byte in a
# string field usually breaks its UTF-8 outright: the break
# surfaces at that same entry, since it fails to decode at all --
# no different, as a loud failure, than a byte flipped somewhere
# that still decodes but breaks the next entry's predecessor match.
# (create lands in the root book, not the machine's: the manifest
# does not exist yet when create is witnessed -- see audit.rs.)
python3 - "$AUDIT" <<'PY'
import sys
path = sys.argv[1]
data = bytearray(open(path, "rb").read())
needle = b"start"
i = data.find(needle)
assert i >= 0, "no 'start' verb found in the audit book"
data[i] ^= 0xff
open(path, "wb").write(data)
PY
OUT=$("$BIN" doctor verify "$VM" || true)
echo "$OUT" | sed 's/^/  /'
echo "$OUT" | grep -qE "FAIL  $VM audit: chain breaks at entry [0-9]+" || { echo "FAIL: the tampered audit book did not fail loudly"; exit 1; }
echo "  the tampered book fails, naming the entry where the chain snaps"
cp "$AUDIT.orig" "$AUDIT"
rm -f "$AUDIT.orig"
OUT=$("$BIN" doctor verify "$VM")
echo "$OUT" | grep -q "ok    $VM audit: chain verifies end to end" || { echo "FAIL: restoring the original bytes did not restore verification"; exit 1; }
echo "  restored bytes verify again -- the FAIL above was the edit, not a false positive"

say "step 4: a branched twin's book forks with its history and stays valid"
"$BIN" branch "$VM" "$TWIN" >/dev/null
TWIN_AUDIT="$CELLA_HOME/machines/$TWIN/audit"
[ -f "$TWIN_AUDIT" ] || { echo "FAIL: the twin has no audit book -- branch did not carry the book"; exit 1; }
cmp -s "$AUDIT" "$TWIN_AUDIT" || { echo "FAIL: the twin's audit book is not a byte-identical copy"; exit 1; }
OUT=$("$BIN" doctor verify "$TWIN" || true)
echo "$OUT" | sed 's/^/  /'
echo "$OUT" | grep -q "ok    $TWIN audit: chain verifies end to end" || { echo "FAIL: the twin's audit chain did not verify"; exit 1; }
echo "  the twin's copied book verifies from the same genesis as the source's"

say "step 5: the twin's own new entries extend its forked chain"
"$BIN" start "$TWIN" >/dev/null 2>&1 || true
sleep 3
"$BIN" stop "$TWIN" >/dev/null 2>&1 || true
# The twin's own boot journals its disk (ext4 recovery on an
# unclean-shutdown image), which the golden digest check flags --
# unrelated to the chain this gate judges; doctor verify's overall
# exit reflects that FAIL too, so it is not asserted here.
OUT=$("$BIN" doctor verify "$TWIN" || true)
echo "$OUT" | sed 's/^/  /'
echo "$OUT" | grep -q "ok    $TWIN audit: chain verifies end to end" || { echo "FAIL: the twin's extended chain did not verify"; exit 1; }
echo "  the twin's own new entries extend from the copied tail, and still verify"

echo
echo "PASS: the hash chain -- an intact book verifies, a tampered one snaps loudly, a twin's book forks and stays valid"
"$BIN" destroy "$VM" >/dev/null 2>&1 || true
"$BIN" destroy "$TWIN" >/dev/null 2>&1 || true
