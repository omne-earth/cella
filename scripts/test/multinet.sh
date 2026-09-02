#!/usr/bin/env bash
# Multi-net: a machine takes N taps. The gate boots a two-tap
# machine, proves both nics exist in the guest (eth0 configured by
# ip=, eth1 present for the init of a gateway image to configure),
# and pings eth0 from the host. Sandboxed CELLA_HOME.
set -euo pipefail

cd "$(dirname "$0")/../.."
BIN=target/smoke/cella
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0
for t in tap1 tap2; do
    ip link show $t >/dev/null 2>&1 || { echo "SKIP: $t missing -- run: cella doctor fix"; exit 0; }
done

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-multinet.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

teardown() { "$BIN" stop mn >/dev/null 2>&1 || true; rm -rf "$CELLA_HOME"; }
trap teardown EXIT
say() { echo; echo "==> $1"; }
CON="$CELLA_HOME/machines/mn/console.log"
type_in() { (printf '%s\n' "$1"; sleep 2) | timeout 20 "$BIN" enter mn >/dev/null; }
wait_for() {
    local marker="$1" deadline=$((SECONDS + 15))
    while [ $SECONDS -lt $deadline ]; do
        grep -aq "$marker" "$CON" && return 0
        sleep 1
    done
    return 1
}
M="$CELLA_HOME/machines/mn"
# The stand-in engine: while the given pid runs, release every held
# operation of the frozen machine and thaw it (see ping.sh).
pump_while() { # pid
    local cycles=0
    while kill -0 "$1" 2>/dev/null; do
        if [ -f "$M/state" ]; then
            pid=$(cat "$M/pid" 2>/dev/null || true)
            [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null && { sleep 0.2; continue; }
            for id in $("$BIN" gateway mn show | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
                "$BIN" gateway mn release "$id" >/dev/null
            done
            "$BIN" thaw mn >/dev/null
            cycles=$((cycles + 1))
        fi
        sleep 0.2
    done
    echo "  ($cycles engine cycles)"
}

say "step 1: create and start a machine on two taps"
"$BIN" create mn --net tap1,tap2 >/dev/null
"$BIN" start mn >/dev/null
sleep 5

say "step 2: both nics exist in the guest"
type_in 'ls /sys/class/net | tr "\n" " "; echo nics-liste"d"'
wait_for "nics-listed" || { echo "FAIL: the guest did not answer"; exit 1; }
grep -a "eth0" "$CON" | grep -aq "eth1" || { echo "FAIL: eth1 is absent in the guest"; exit 1; }
echo "  eth0 and eth1 present"

say "step 3: open -- every egress parks, and the engine lands a reply on eth0"
"$BIN" gateway mn open >/dev/null
sleep 1
ping -c 1 -W 3 192.168.201.2 >/dev/null 2>&1 && { echo "FAIL: an open machine answered without a decision"; exit 1; }
deadline=$((SECONDS + 20))
until [ -f "$M/state" ]; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: the first egress did not park and freeze"; exit 1; }
    sleep 1
done
ping -c 20 -i 1 -W 25 192.168.201.2 >/dev/null 2>&1 & P3=$!
pump_while "$P3"
wait "$P3" || { echo "FAIL: no reply landed on the first tap while the engine decided"; exit 1; }
echo "  192.168.201.2 answers over tap1, every frame decided"

say "step 4: the tap claims are exclusive per tap"
"$BIN" create mn2 --net tap2 >/dev/null 2>&1 && { echo "FAIL: a claimed tap was granted again"; exit 1; }
echo "  tap2 refused: the list claims each tap"

say "step 5: freeze and thaw with two transports in the sidecar"
if [ -f "$M/state" ]; then "$BIN" thaw mn >/dev/null; sleep 1; fi
"$BIN" freeze mn >/dev/null
"$BIN" thaw mn >/dev/null
sleep 2
# Nothing is inherited: every frame is decided again after the thaw.
ping -c 20 -i 1 -W 25 192.168.201.2 >/dev/null 2>&1 & P5=$!
pump_while "$P5"
wait "$P5" || { echo "FAIL: no reply landed after the thaw while the engine decided"; exit 1; }
if [ -f "$M/state" ]; then "$BIN" thaw mn >/dev/null; sleep 1; fi
type_in 'ls /sys/class/net | grep -ac eth; echo nets-aliv"e"'
wait_for "nets-alive" || { echo "FAIL: the guest did not answer after the thaw"; exit 1; }
echo "  both transports rode the sidecar; eth0 answers after the thaw, re-decided"

echo
echo "PASS: multi-net -- a machine takes N taps"
"$BIN" stop mn >/dev/null 2>&1 || true
"$BIN" destroy mn >/dev/null
