#!/usr/bin/env bash
# The wire plane (1.6.14e rung 2): two machines, one wire, no host
# network object anywhere. The translators stand between the VMMs;
# every crossing parks and is decided on both membranes; a frozen
# peer's mail is discarded at the edge and counted, never buffered
# across the gap.
set -euo pipefail

cd "$(dirname "$0")/../.."
BIN=target/smoke/cella
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build-smoke"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0

say() { echo; echo "==> $1"; }

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-wire.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

teardown() {
    for m in wa wb; do
        "$BIN" stop $m >/dev/null 2>&1 || true
        # A kept sandbox keeps its machine dirs: destroy removes
        # them (and kills the translators), so skip it under the
        # knob and kill the translators directly.
        if [ -n "${CELLA_KEEP_SANDBOX:-}" ]; then
            p=$(cat "$CELLA_HOME/machines/$m/edge.pid" 2>/dev/null || true)
            [ -n "$p" ] && kill -9 "$p" 2>/dev/null || true
        else
            "$BIN" destroy $m >/dev/null 2>&1 || true
        fi
    done
    if [ -n "${CELLA_KEEP_SANDBOX:-}" ]; then echo "kept: $CELLA_HOME"; else rm -rf "$CELLA_HOME"; fi
}
trap teardown EXIT
type_in() { local vm="$1"; shift; (printf '%s\n' "$1"; sleep 2) | timeout 20 "$BIN" enter "$vm" >/dev/null; }
wait_con() {
    local con="$CELLA_HOME/machines/$1/console.log" deadline=$((SECONDS + 20))
    while [ $SECONDS -lt $deadline ]; do
        grep -aq "$2" "$con" && return 0
        sleep 1
    done
    return 1
}
# The decision pump: release the incoming mail of a running member,
# and every held operation of a frozen one, then thaw it.
pump() { # marker budget
    local marker="$1" budget="$2" cycles=0
    until grep -aq "$marker" "$CELLA_HOME/machines/wa/console.log"; do
        cycles=$((cycles + 1))
        [ $cycles -le "$budget" ] || return 1
        for m in wa wb; do
            for id in $("$BIN" gateway $m show incoming | sed -n "s/^\([0-9a-f]\{32\}\) .*held\$/\1/p"); do
                "$BIN" gateway $m release "$id" >/dev/null
            done
            [ -f "$CELLA_HOME/machines/$m/state" ] || continue
            pid=$(cat "$CELLA_HOME/machines/$m/pid" 2>/dev/null || true)
            [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null && continue
            for id in $("$BIN" gateway $m show | sed -n "s/^\([0-9a-f]\{32\}\) .*held\$/\1/p"); do
                "$BIN" gateway $m release "$id" >/dev/null
            done
            "$BIN" thaw $m >/dev/null
        done
        sleep 0.5
    done
}

say "step 1: two machines, one wire, no host object"
"$BIN" create wa --net wire:w0 >/dev/null
"$BIN" create wb --net wire:w0 >/dev/null
ip link show 2>/dev/null | grep -q "w0" && { echo "FAIL: a host interface named the wire"; exit 1; }
"$BIN" start wa >/dev/null
"$BIN" start wb >/dev/null
[ -f "$CELLA_HOME/machines/wa/edge.pid" ] || { echo "FAIL: wa has no translator"; exit 1; }
[ -f "$CELLA_HOME/machines/wb/edge.pid" ] || { echo "FAIL: wb has no translator"; exit 1; }
sleep 4
grep -aq "wire \"w0\"" "$CELLA_HOME/machines/wa/edge.log" || { echo "FAIL: wa's translator never touched wire w0"; exit 1; }
echo "  two translators stand; the wire is theirs alone"

say "step 2: the guests address the wire themselves (no host convention)"
type_in wa 'ip addr add 10.88.0.1/24 dev eth0 && ip link set eth0 up && echo wa-addresse"d"'
wait_con wa "wa-addressed" || { echo "FAIL: wa did not address eth0"; exit 1; }
type_in wb 'ip addr add 10.88.0.2/24 dev eth0 && ip link set eth0 up && echo wb-addresse"d"'
wait_con wb "wb-addressed" || { echo "FAIL: wb did not address eth0"; exit 1; }

say "step 3: both open -- a ping crosses, every hop decided on both membranes"
"$BIN" gateway wa open >/dev/null
"$BIN" gateway wb open >/dev/null
sleep 1
type_in wa 'ping -c20 -W60 10.88.0.2 >/dev/null && echo wire-o"k"'
pump "wire-ok" 180 || { echo "FAIL: the ping did not cross the wire"; exit 1; }
echo "  wa -> wb over the wire, one decision at a time"

say "step 4: the frozen peer -- mail is discarded at the edge and counted (negative)"
# Freeze wb by its own park if pending, else by the verb; then send
# from wa and release it: the frame reaches wb's translator, which
# has no VMM to give it to.
"$BIN" freeze wb >/dev/null 2>&1 || true
for _ in 1 2 3 4 5; do [ -f "$CELLA_HOME/machines/wb/state" ] && break; sleep 1; done
[ -f "$CELLA_HOME/machines/wb/state" ] || { echo "FAIL: wb did not freeze"; exit 1; }
# The send parks wa's egress and freezes wa mid-command: the shell
# suspends inside the ping, and the marker cannot print until the
# thaw. Type, wait for the freeze, release, thaw -- then the ping
# completes (unanswered) and the marker lands.
type_in wa 'ping -c2 -W1 10.88.0.2 >/dev/null; echo sent-into-the-ga"p"'
# Each unanswered echo parks and freezes wa in its own epoch (-c2
# means two freezes); release and thaw until the shell completes.
cycles=0
until grep -aq "sent-into-the-gap" "$CELLA_HOME/machines/wa/console.log"; do
    cycles=$((cycles + 1))
    [ $cycles -le 30 ] || { echo "FAIL: wa's send did not complete across its freezes"; exit 1; }
    if [ -f "$CELLA_HOME/machines/wa/state" ]; then
        pid=$(cat "$CELLA_HOME/machines/wa/pid" 2>/dev/null || true)
        if [ -z "$pid" ] || ! kill -0 "$pid" 2>/dev/null; then
            for id in $("$BIN" gateway wa show | sed -n "s/^\([0-9a-f]\{32\}\) .*held\$/\1/p"); do
                "$BIN" gateway wa release "$id" >/dev/null
            done
            "$BIN" thaw wa >/dev/null
        fi
    fi
    sleep 1
done
deadline=$((SECONDS + 20))
until grep -aq "discarded" "$CELLA_HOME/machines/wb/edge.log"; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: wb's translator discarded nothing -- the gap buffered"; exit 1; }
    sleep 1
done
echo "  the frozen peer's mail died at the edge, counted, never buffered"

say "step 5: thaw the peer -- the wire carries a fresh crossing, re-decided"
for id in $("$BIN" gateway wb show | sed -n "s/^\([0-9a-f]\{32\}\) .*held\$/\1/p"); do
    "$BIN" gateway wb release "$id" >/dev/null
done
"$BIN" thaw wb >/dev/null
sleep 2
type_in wa 'ping -c20 -W60 10.88.0.2 >/dev/null && echo wire-agai"n"'
pump "wire-again" 180 || { echo "FAIL: the wire did not survive the peer's freeze"; exit 1; }
echo "  the wire held across the freeze; the new epoch decided its own crossings"

say "step 6: destroy kills the translators"
PA=$(cat "$CELLA_HOME/machines/wa/edge.pid"); PB=$(cat "$CELLA_HOME/machines/wb/edge.pid")
for m in wa wb; do "$BIN" stop $m >/dev/null 2>&1 || true; "$BIN" destroy $m >/dev/null; done
sleep 1
kill -0 "$PA" 2>/dev/null && { echo "FAIL: wa's translator outlived destroy"; exit 1; }
kill -0 "$PB" 2>/dev/null && { echo "FAIL: wb's translator outlived destroy"; exit 1; }
echo "  machine-lifetime means machine-lifetime"

echo
echo "PASS: the wire plane -- no host object, both membranes judge, the gap discards, destroy ends it"
