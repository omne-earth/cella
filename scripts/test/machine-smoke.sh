#!/usr/bin/env bash
# smoke-machine: the running half of the lifecycle, with a real guest.
# create -> start (readiness) -> the guest reaches its init -> stop
# clears the transients -> start boots fresh -> stop -> destroy. Runs
# in a sandboxed CELLA_HOME, and tears it down, pass or fail.
set -euo pipefail

cd "$(dirname "$0")/../.."
BIN=target/release/cella
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build"; exit 0; }
[ -f dist/bzImage ] && [ -f dist/rootfs-cella.ext4 ] \
    || { echo "SKIP: proof artifacts missing -- run: make dist dist-nested"; exit 0; }
if ! [ -r /dev/kvm ] || ! [ -w /dev/kvm ]; then
    echo "SKIP: no read and write access to /dev/kvm"
    exit 0
fi
command -v bwrap >/dev/null || { echo "SKIP: bwrap not found -- run: make init"; exit 0; }

export CELLA_HOME=$(mktemp -d /tmp/cella-machine-smoke.XXXXXX)
teardown() {
    "$BIN" stop m1 >/dev/null 2>&1 || true
    rm -rf "$CELLA_HOME"
}
trap teardown EXIT

"$BIN" build kernel canonical >/dev/null
"$BIN" build rootfs cella >/dev/null
"$BIN" create m1 --net none >/dev/null

"$BIN" start m1 >/dev/null
DIR="$CELLA_HOME/machines/m1"
[ -f "$DIR/pid" ] || { echo "FAIL: no pid file after start"; exit 1; }

ok=""
for _ in $(seq 20); do
    grep -aq "cella-rootfs: init running" "$DIR/console.log" && { ok=yes; break; }
    sleep 1
done
[ -n "$ok" ] || { echo "FAIL: the guest did not reach its init"; tail -5 "$DIR/vmm.log"; exit 1; }

"$BIN" start m1 2>/dev/null && { echo "FAIL: a second start must refuse"; exit 1; }

"$BIN" freeze m1 >/dev/null
[ -f "$DIR/state" ] || { echo "FAIL: no sidecar after freeze"; exit 1; }
[ ! -f "$DIR/pid" ] || { echo "FAIL: pid file survived the freeze"; exit 1; }
"$BIN" start m1 2>/dev/null && { echo "FAIL: start must refuse a frozen machine"; exit 1; }
"$BIN" stop m1 2>/dev/null && { echo "FAIL: stop must refuse a frozen machine"; exit 1; }

"$BIN" thaw m1 >/dev/null
[ ! -f "$DIR/state" ] || { echo "FAIL: the sidecar survived the thaw"; exit 1; }
grep -aq "cella: thawed" "$DIR/vmm.log" || { echo "FAIL: the VMM did not report the thaw"; exit 1; }
"$BIN" thaw m1 2>/dev/null && { echo "FAIL: thaw must refuse a running machine"; exit 1; }

"$BIN" stop m1 >/dev/null
[ ! -f "$DIR/pid" ] && [ ! -f "$DIR/ram.img" ] || { echo "FAIL: stop left transients"; exit 1; }

"$BIN" start m1 >/dev/null
"$BIN" stop m1 >/dev/null
"$BIN" destroy m1 >/dev/null
[ ! -d "$DIR" ] || { echo "FAIL: destroy left the directory"; exit 1; }

echo "PASS: create, start, freeze, thaw, stop, restart, destroy, with every refusal checked"
