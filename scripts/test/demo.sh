#!/usr/bin/env bash
# make demo: the freeze and the thaw, end to end, through the verbs.
# A shell stores a value, the machine freezes to files, thaws, and the
# same shell returns the value. Runs in a sandboxed CELLA_HOME with
# the goldens copied in, and tears down, pass or fail.
set -euo pipefail

cd "$(dirname "$0")/../.."
BIN=target/release/cella
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-demo.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

teardown() {
    "$BIN" stop demo >/dev/null 2>&1 || true
    rm -rf "$CELLA_HOME"
}
trap teardown EXIT
say() { echo; echo "==> $1"; }
type_in() { (printf '%s\n' "$1"; sleep 2) | timeout 10 "$BIN" enter demo >/dev/null; }
CON="$CELLA_HOME/machines/demo/console.log"

say "step 1: create and start a machine with a shell (rw root)"
"$BIN" create demo --net none --diag >/dev/null
"$BIN" start demo >/dev/null
sleep 4

say "step 2: store a value in the shell"
type_in 'MARKER=aurora-$((19*7)); echo "value-set: $MARKER"'
grep -aq "value-set: aurora-133" "$CON" || { echo "FAIL: the shell did not respond before the freeze"; exit 1; }
grep -a "value-set:" "$CON" | tail -1

say "step 3: freeze -- the machine is files"
"$BIN" freeze demo >/dev/null
ls -l "$CELLA_HOME/machines/demo" | grep -E "ram.img|state" | awk '{print "  " $9 "  " $5 " bytes"}'

say "step 4: thaw -- the same machine, the same instant"
"$BIN" thaw demo >/dev/null
sleep 2

say "step 5: read the value back from the same shell"
type_in 'echo "value-after-thaw: $MARKER"'
if grep -aq "value-after-thaw: aurora-133" "$CON"; then
    grep -a "value-after-thaw:" "$CON" | tail -1
    echo
    echo "PASS: the shell state survived the freeze and the thaw"
    "$BIN" stop demo >/dev/null; "$BIN" destroy demo >/dev/null
    exit 0
fi
echo "FAIL: the shell did not respond after the thaw"
echo "-- the processes of the guest (diag):"
grep -a "cella-ps:" "$CON" | tail -8 || true
exit 1
