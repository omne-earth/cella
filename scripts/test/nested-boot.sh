#!/usr/bin/env bash
# Smoke test: cella hosts cella, through the verbs at both layers.
# Three variants, selected by $1:
#
#   airgapped  no network on either layer.
#   hybrid     the outer machine is networked; the inner stays
#              airgapped. PASS needs the inner boot and an outer
#              ICMP reply -- decided through the membrane.
#   www        both layers networked. The outer init is the inner
#              machine's engine (see rootfs-nested.sh). PASS needs
#              the inner boot, the outer ICMP reply, and the inner
#              ICMP reply.
#
# The outer machine is born closed, like every machine. The gate
# proves the negatives first: a closed machine answers nothing and
# does not freeze on inbound; an open machine answers nothing
# before a decision. The outer init prints "cella-nested:" lines
# only, thus a "cella-rootfs:" line can come from the inner guest
# alone.
set -euo pipefail

MODE="${1:-airgapped}"
case "$MODE" in airgapped|hybrid|www) ;; *) echo "usage: $0 airgapped|hybrid|www" >&2; exit 2;; esac

cd "$(dirname "$0")/../.."
BIN=target/smoke/cella
TAP="${CELLA_TEST_TAP:-tap0}"
HOST_IP="${CELLA_TAP_CIDR:-192.168.200.1/24}"; HOST_IP="${HOST_IP%%/*}"
OUTER_IP="${CELLA_TEST_GUEST_IP:-192.168.200.2}"

[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build"; exit 0; }
"$BIN" doctor gate kvm golden:kernel:nested golden:rootfs:nested || exit 0
if [ "$MODE" != airgapped ] && [ ! -e "/sys/class/net/$TAP" ]; then
    echo "SKIP: $TAP does not exist -- run: cella doctor fix"
    exit 0
fi

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-nested.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/nested" "$CELLA_HOME/rootfs/nested"
cp "$REAL_HOME/kernel/nested/bzImage" "$CELLA_HOME/kernel/nested/"
cp "$REAL_HOME/kernel/nested/golden.json" "$CELLA_HOME/kernel/nested/" 2>/dev/null || true
cp "$REAL_HOME/rootfs/nested/rootfs.ext4" "$CELLA_HOME/rootfs/nested/"
cp "$REAL_HOME/rootfs/nested/golden.json" "$CELLA_HOME/rootfs/nested/" 2>/dev/null || true

VM=outer
teardown() {
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    [ -n "${VMM_PID:-}" ] && kill -9 "$VMM_PID" 2>/dev/null || true
    if [ -n "${CELLA_KEEP_SANDBOX:-}" ]; then echo "kept: $CELLA_HOME"; else rm -rf "$CELLA_HOME"; fi
}
trap teardown EXIT
say() { echo; echo "==> $1"; }
CON="$CELLA_HOME/machines/$VM/console.log"
STATE="$CELLA_HOME/machines/$VM/state"
wait_con() {
    local marker="$1" deadline=$((SECONDS + ${2:-60}))
    while [ $SECONDS -lt $deadline ]; do
        grep -aq "$marker" "$CON" 2>/dev/null && return 0
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
# The stand-in engine: while the given pid runs, release every held
# operation of the frozen outer machine and thaw it.
pump_mail() {
    for id in $("$BIN" gateway "$VM" show incoming | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
        "$BIN" gateway "$VM" release "$id" >/dev/null
    done
}
pump_while() { # pid
    local cycles=0
    while kill -0 "$1" 2>/dev/null; do
        pump_mail
        if [ -f "$STATE" ]; then
            pid=$(cat "$CELLA_HOME/machines/$VM/pid" 2>/dev/null || true)
            [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null && { sleep 0.2; continue; }
            for id in $("$BIN" gateway "$VM" show | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
                "$BIN" gateway "$VM" release "$id" >/dev/null
            done
            "$BIN" thaw "$VM" >/dev/null
            cycles=$((cycles + 1))
        fi
        sleep 0.2
    done
    echo "  ($cycles engine cycles)"
}

say "step 1: create the outer machine through the verbs"
NET_FLAG="--net none"; [ "$MODE" != airgapped ] && NET_FLAG="--net $TAP"
"$BIN" create "$VM" --kernel nested --rootfs nested --mem-mb 768 $NET_FLAG >/dev/null
[ "$(cat "$CELLA_HOME/machines/$VM/valve")" = "closed" ] || { echo "FAIL: the valve record is not born closed"; exit 1; }
if [ "$MODE" = www ]; then
    # The mode marker rides the disk: a verb machine cannot extend
    # the kernel command line.
    echo www > "$CELLA_HOME/mode"
    # The golden rootfs has no /etc; make it before the write. debugfs
    # exits 0 on a failed command, thus the verification is the read
    # back of the marker.
    debugfs -w -R "mkdir /etc" "$CELLA_HOME/machines/$VM/disk.img" >/dev/null 2>&1 || true
    debugfs -w -R "write $CELLA_HOME/mode /etc/cella-www" \
        "$CELLA_HOME/machines/$VM/disk.img" >/dev/null 2>&1 || true
    debugfs -R "cat /etc/cella-www" "$CELLA_HOME/machines/$VM/disk.img" 2>/dev/null \
        | grep -q www || { echo "SKIP: debugfs cannot write the mode marker"; exit 0; }
fi
"$BIN" start "$VM" >/dev/null
VMM_PID=$(cat "$CELLA_HOME/machines/$VM/pid")

say "step 2: the outer guest boots, and hosts the inner machine"
wait_con "cella-nested: outer init running" 30 || { echo "FAIL: the outer init never ran"; exit 1; }
if grep -aq "cella-nested: FAIL: no /dev/kvm" "$CON"; then
    echo "SKIP: the outer guest has no /dev/kvm -- no nested virtualization one layer deeper"
    exit 0
fi
wait_con "cella-rootfs: init running" 90 || {
    echo "FAIL: the inner guest never booted"
    tail -20 "$CON" | sed "s/^/   /"; exit 1; }
echo "  the inner guest booted, one level down"

if [ "$MODE" != airgapped ]; then
    say "step 3: born closed -- the outer machine answers nothing (negative)"
    ping -c 2 -W 2 "$OUTER_IP" >/dev/null 2>&1 && { echo "FAIL: a closed machine answered a ping"; exit 1; }
    [ -f "$STATE" ] && { echo "FAIL: a closed machine froze on inbound traffic"; exit 1; }
    echo "  no reply, no freeze: dark"

    say "step 4: open -- the knock parks in the inbound lane, and the outer machine keeps running"
    "$BIN" gateway "$VM" open >/dev/null
    sleep 1
    ping -c 1 -W 3 "$OUTER_IP" >/dev/null 2>&1 && { echo "FAIL: an open machine answered without a decision"; exit 1; }
    [ -f "$STATE" ] && { echo "FAIL: the outer machine froze on inbound -- the world's knock is not the resident's deed"; exit 1; }
    "$BIN" gateway "$VM" show incoming | grep -qE "^[0-9a-f]{32} .*held$" || { echo "FAIL: show incoming lists no held knock"; exit 1; }
    echo "  the knock is held incoming; no freeze"

    say "step 4b: the released knock reaches the guest; the reply parks, and that park is the freeze"
    ID_K=$("$BIN" gateway "$VM" show incoming | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p' | head -1)
    "$BIN" gateway "$VM" release "$ID_K" | grep -q "applies now" || { echo "FAIL: the incoming release did not apply live"; exit 1; }
    wait_frozen || { echo "FAIL: the guest's reply did not park and freeze"; exit 1; }
    "$BIN" gateway "$VM" show outgoing | grep -qE "^[0-9a-f]{32} .*held$" || { echo "FAIL: show outgoing lists no held reply"; exit 1; }
    echo "  mail moved live; the resident's own deed froze the machine"
    ping -c 20 -i 1 -W 30 "$OUTER_IP" >/dev/null 2>&1 & P4=$!
    pump_while "$P4"
    wait "$P4" || { echo "FAIL: no ICMP reply landed while the engine decided"; exit 1; }
    if [ -f "$STATE" ]; then "$BIN" thaw "$VM" >/dev/null; sleep 1; fi
    echo "  parked, frozen, decided: the outer machine answers"
fi

if [ "$MODE" = www ]; then
    say "step 5: the inner ICMP, decided by the outer init as the engine"
    wait_con "cella-nested: inner answered ICMP" 90 || {
        echo "FAIL: the inner guest never answered inside the outer guest"
        grep -a "cella-nested:" "$CON" | tail -8 | sed "s/^/   /"; exit 1; }
    echo "  the ratchet turned one level down"
fi

echo
echo "PASS ($MODE): all layers reported"
grep -a "cella-nested:" "$CON" | head -6
"$BIN" stop "$VM" >/dev/null; "$BIN" destroy "$VM" >/dev/null
