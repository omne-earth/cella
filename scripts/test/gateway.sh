#!/usr/bin/env bash
# The time-gateway ladder, first rungs: pair wiring, the gateway
# appliance forwards, and the pair freezes and thaws together. The
# agent reaches the world only through the gateway; the pair-freeze
# order is agent first, thaw order gateway first (the world side
# stands before the agent wakes).
set -euo pipefail

cd "$(dirname "$0")/../.."
BIN=target/smoke/cella
NET_BIN="$HOME/.local/bin/cella-network"
[ -x "$NET_BIN" ] || NET_BIN=target/release/cella-network
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella golden:rootfs:gateway || exit 0
ip link show tap1 >/dev/null 2>&1 || { echo "SKIP: tap1 missing -- run: cella doctor fix"; exit 0; }

say() { echo; echo "==> $1"; }

say "step 1: wire the pair (bridge, two taps, the route to the agent subnet)"
if ! "$NET_BIN" pair --id 0 --via tap1; then
    echo "SKIP: pair wiring failed (cap_net_admin -- run: make install)"; exit 0
fi

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-gateway.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella" "$CELLA_HOME/rootfs/gateway"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"
cp "$REAL_HOME/rootfs/gateway/rootfs.ext4" "$CELLA_HOME/rootfs/gateway/"

teardown() {
    for m in gw ag; do "$BIN" stop $m >/dev/null 2>&1 || true; done
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

say "step 2: the gateway appliance, world side tap1, agent side pair0g"
"$BIN" create gw --net tap1,pair0g --rootfs gateway >/dev/null
"$BIN" start gw >/dev/null
wait_con gw "forwarding on" || { echo "FAIL: the gateway did not bring its agent side up"; exit 1; }
echo "  gateway up: 10.77.0.1 on the agent side, forwarding on"

say "step 3: the agent, behind the gateway only -- both machines open, the pump decides"
"$BIN" create ag --net pair0a >/dev/null
"$BIN" start ag >/dev/null
sleep 4
"$BIN" gateway gw open >/dev/null
"$BIN" gateway ag open >/dev/null
sleep 1
# The decision pump: the stand-in engine. While the marker is
# absent, release every held operation of any frozen machine of the
# pair, and thaw it.
pump() { # marker cycles
    local marker="$1" budget="$2" cycles=0
    until grep -aq "$marker" "$CELLA_HOME/machines/ag/console.log"; do
        cycles=$((cycles + 1))
        [ $cycles -le "$budget" ] || return 1
        for m in ag gw; do
            # The ear's live wire: release incoming mail while the
            # member runs -- mail moves without a thaw, and each hop
            # needs its own replies released before it forwards on.
            for id in $("$BIN" gateway $m show incoming | sed -n "s/^\([0-9a-f]\{32\}\) .*held\$/\1/p"); do
                "$BIN" gateway $m release "$id" >/dev/null
            done
            [ -f "$CELLA_HOME/machines/$m/state" ] || continue
            # The sidecar lands before the old VMM exits: wait out
            # the gap, or the thaw refuses a machine still running.
            pid=$(cat "$CELLA_HOME/machines/$m/pid" 2>/dev/null || true)
            [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null && continue
            for id in $("$BIN" gateway $m show | sed -n "s/^\([0-9a-f]\{32\}\) .*held\$/\1/p"); do
                "$BIN" gateway $m release "$id" >/dev/null
            done
            "$BIN" thaw $m >/dev/null
        done
        # A stride of half a second: one echo's round trip crosses
        # two machines -- three freezes and four mail moves -- and
        # the reply must land inside the ping's own patience.
        sleep 0.5
    done
}
type_in ag 'ping -c20 -W60 10.77.0.1 >/dev/null && echo pair-o"k"'
pump "pair-ok" 180 || { echo "FAIL: the agent cannot reach the gateway over the pair"; exit 1; }
echo "  agent -> gateway over the L2 pair, one decision at a time"

say "step 4: the agent reaches the host, through the gateway"
type_in ag 'ping -c20 -W60 192.168.201.1 >/dev/null && echo world-o"k"'
pump "world-ok" 180 || { echo "FAIL: the gateway does not forward to the world"; exit 1; }
echo "  agent -> gateway -> host: the appliance forwards, every hop decided"

say "step 5: the pair freezes together (agent first), thaws together (gateway first)"
# A member may already stand frozen on its own park, or be caught
# mid-transition (its park's sidecar landing, its VMM exiting):
# the pair freeze takes each machine as it finds it, agent first,
# and waits out a transition instead of racing it.
for m in ag gw; do
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        [ -f "$CELLA_HOME/machines/$m/state" ] && break
        if "$BIN" freeze $m >/dev/null 2>&1; then break; fi
        sleep 1
    done
    [ -f "$CELLA_HOME/machines/$m/state" ] || { echo "FAIL: $m neither froze nor stood"; exit 1; }
done
"$BIN" thaw gw >/dev/null
wait_con gw "forwarding on" >/dev/null 2>&1 || true
"$BIN" thaw ag >/dev/null
sleep 2
# A thaw starts a fresh epoch on both machines: nothing is
# inherited, and every hop parks and is decided again. The pump
# stands in for the engine here too.
# ICMP never retransmits on its own, and a reply that lands while
# a member is frozen is lost at the tap: the repeating ping gives
# the chain fresh echoes until one crossing survives every hop.
type_in ag 'ping -c20 -W60 192.168.201.1 >/dev/null && echo world-agai"n"'
pump "world-again" 180 || { echo "FAIL: the pair did not survive the freeze"; exit 1; }
echo "  the pair froze and thawed; the agent reaches the world again, re-decided"

echo
echo "PASS: the gateway ladder -- pair wiring, forwarding, pair freeze"
# Take each machine as the ladder left it: a member may stand
# re-frozen on a post-PASS park (stop refuses frozen; destroy
# takes any still machine).
for m in ag gw; do
    "$BIN" stop $m >/dev/null 2>&1 || true
    "$BIN" destroy $m >/dev/null
done
