#!/usr/bin/env bash
# smoke-witness: every verb is an event, no exception -- show and
# inspect included. One of each verb class runs against a sandbox
# machine, and every one lands in the right book: machine-scoped in
# machines/<vm>/audit, placeless in the audit file at the CELLA_HOME
# root, each entry with uid, gid, and persona. The negatives: a verb
# that only reads still writes its entry (show twice makes two
# entries), and the AVC harvest on a host with no matching denials
# files an empty set and says so.
set -uo pipefail

cd "$(dirname "$0")/../.."
BIN=target/smoke/cella
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build-smoke"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-witness.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

VM=seen
M="$CELLA_HOME/machines/$VM"
ROOT_BOOK="$CELLA_HOME/audit"
teardown() {
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    p=$(cat "$M/pid" 2>/dev/null || true)
    [ -n "$p" ] && kill -9 "$p" 2>/dev/null || true
    rm -rf "$CELLA_HOME"
}
trap teardown EXIT
say() { echo; echo "==> $1"; }
book() { "$BIN" --dump-ledger "$1" 2>/dev/null | grep "^audit "; }
entries() { book "$1" | grep -c "verb=$2" || true; }

say "step 1: the placeless verbs land in the root book"
"$BIN" list >/dev/null
[ -f "$ROOT_BOOK" ] || { echo "FAIL: list wrote no root book"; exit 1; }
book "$ROOT_BOOK" | grep -q "verb=list" || { echo "FAIL: list is not witnessed"; exit 1; }
book "$ROOT_BOOK" | grep "verb=list" | grep -q "uid=$(id -u) gid=$(id -g) persona=" \
    || { echo "FAIL: the list entry misses uid, gid, or persona"; exit 1; }
echo "  list landed in the root book, with uid, gid, and persona"

say "step 2: the machine-scoped verbs land in the machine's book"
"$BIN" create "$VM" --net none >/dev/null
"$BIN" start "$VM" >/dev/null
sleep 2
"$BIN" gateway "$VM" show >/dev/null
"$BIN" freeze "$VM" >/dev/null
"$BIN" thaw "$VM" >/dev/null
sleep 1
[ -f "$M/audit" ] || { echo "FAIL: the machine has no book"; exit 1; }
for v in start gateway freeze thaw; do
    book "$M/audit" | grep -q "verb=$v" || { echo "FAIL: $v is not in the machine's book"; exit 1; }
done
book "$ROOT_BOOK" | grep -q 'verb=create.*args=\["seen"' || { echo "FAIL: create is not witnessed (the root book carries the birth)"; exit 1; }
echo "  start, gateway show, freeze, thaw in the machine's book; the birth in the root book"

say "step 3: a verb that only reads still writes its entry (negative)"
BEFORE=$(entries "$M/audit" gateway)
"$BIN" gateway "$VM" show >/dev/null
"$BIN" gateway "$VM" show >/dev/null
AFTER=$(entries "$M/audit" gateway)
[ "$AFTER" -eq $((BEFORE + 2)) ] || { echo "FAIL: two shows made $((AFTER - BEFORE)) entries, not 2"; exit 1; }
echo "  show twice makes two entries: reading is an act"

say "step 4: the harvest files the denial set and says so"
OUT=$("$BIN" doctor harvest 2>&1 || true)
echo "  $OUT"
if echo "$OUT" | grep -q "ausearch not found\|privileged"; then
    echo "  (no ausearch or no privilege here -- the harvest stated it honestly)"
elif echo "$OUT" | grep -q "harvested"; then
    [ -f "$CELLA_HOME/avc" ] || { echo "FAIL: the harvest reported but filed nothing"; exit 1; }
    echo "  the denial set is filed beside the book"
else
    echo "FAIL: the harvest neither filed nor explained"
    exit 1
fi

echo
echo "PASS: every verb is an event -- both books carry the border's human side"
"$BIN" stop "$VM" >/dev/null 2>&1 || true
"$BIN" destroy "$VM" >/dev/null
