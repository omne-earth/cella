#!/usr/bin/env bash
# smoke-ping: the valve, end to end. A machine is born closed (the
# closed): a host ping fails. Open turns the tap into the membrane:
# the guest's echo reply parks, and the park is the freeze. A release
# and a thaw let the reply flow, and the next ping answers. Close
# closes the machine: ping fails again. Fail, freeze, release,
# reply, fail -> PASS. See docs/NETWORK-MODEL.md.
set -euo pipefail

cd "$(dirname "$0")/../.."
BIN=target/release/cella
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0
TAP="${CELLA_TEST_TAP:-tap0}"
HOST_IP="${CELLA_TEST_HOST_IP:-192.168.200.1}"
GUEST_IP="${CELLA_TEST_GUEST_IP:-192.168.200.2}"
if ! ip addr show "$TAP" 2>/dev/null | grep -q "$HOST_IP"; then
    echo "SKIP: $TAP is not configured with $HOST_IP -- run: cella doctor fix"
    exit 0
fi

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-ping.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

VM=pingtest
teardown() {
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    [ -n "${VMM_PID:-}" ] && kill -9 "$VMM_PID" 2>/dev/null || true
    rm -rf "$CELLA_HOME"
}
trap teardown EXIT
say() { echo; echo "==> $1"; }
STATE="$CELLA_HOME/machines/$VM/state"
wait_frozen() {
    local deadline=$((SECONDS + 20))
    until [ -f "$STATE" ]; do
        [ $SECONDS -lt $deadline ] || return 1
        sleep 1
    done
}

say "step 1: born closed -- the machine answers nothing"
"$BIN" create "$VM" --net "$TAP" >/dev/null
grep -q '"valve": "closed"' "$CELLA_HOME/machines/$VM/manifest.json" || { echo "FAIL: the manifest is not born closed"; exit 1; }
"$BIN" start "$VM" >/dev/null
sleep 4
VMM_PID=$(cat "$CELLA_HOME/machines/$VM/pid")
ping -c 2 -W 2 "$GUEST_IP" >/dev/null 2>&1 && { echo "FAIL: a closed machine answered a ping"; exit 1; }
[ -f "$STATE" ] && { echo "FAIL: a closed machine froze on inbound traffic"; exit 1; }
echo "  no reply, no freeze, no ledger: dark"

say "step 2: open -- the membrane; the reply parks, and the park is the freeze"
"$BIN" gateway "$VM" open >/dev/null
sleep 1
ping -c 1 -W 3 "$GUEST_IP" >/dev/null 2>&1 && { echo "FAIL: an open machine answered without a decision"; exit 1; }
wait_frozen || { echo "FAIL: the parked reply did not freeze the machine"; exit 1; }
SHOW=$("$BIN" gateway "$VM" show)
echo "$SHOW" | sed "s/^/  /"
echo "$SHOW" | grep -q "$HOST_IP.*held" || { echo "FAIL: show does not list the parked reply"; exit 1; }
echo "  the guest's reply is held; the machine froze itself"

say "step 3: release the reply; the next ping answers"
ID_R=$(echo "$SHOW" | grep "$HOST_IP" | awk "{print \$1}")
"$BIN" gateway "$VM" release "$ID_R" >/dev/null
"$BIN" thaw "$VM" >/dev/null
sleep 2
ping -c 2 -W 3 "$GUEST_IP" >/dev/null || { echo "FAIL: no reply after the release"; exit 1; }
echo "  released: the pass entry stands, and pings answer"

say "step 4: close -- the machine is closed again"
"$BIN" gateway "$VM" close >/dev/null
sleep 1
ping -c 2 -W 2 "$GUEST_IP" >/dev/null 2>&1 && { echo "FAIL: a closed machine answered a ping"; exit 1; }
echo "  dark again: close kills even the allowed flow"

echo
echo "PASS: fail, freeze, release, reply, fail -- the valve holds"
"$BIN" stop "$VM" >/dev/null; "$BIN" destroy "$VM" >/dev/null
