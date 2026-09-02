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

say "step 1: create and start a machine on two taps"
"$BIN" create mn --net tap1,tap2 >/dev/null
"$BIN" start mn >/dev/null
sleep 5

say "step 2: both nics exist in the guest"
type_in 'ls /sys/class/net | tr "\n" " "; echo nics-liste"d"'
wait_for "nics-listed" || { echo "FAIL: the guest did not answer"; exit 1; }
grep -a "eth0" "$CON" | grep -aq "eth1" || { echo "FAIL: eth1 is absent in the guest"; exit 1; }
echo "  eth0 and eth1 present"

say "step 3: open, prime the reply, then the host pings eth0"
"$BIN" gateway mn open >/dev/null
sleep 1
ping -c 1 -W 3 192.168.201.2 >/dev/null 2>&1 || true
deadline=$((SECONDS + 20))
until [ -f "$CELLA_HOME/machines/mn/state" ]; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: the reply did not park and freeze"; exit 1; }
    sleep 1
done
ID_P=$("$BIN" gateway mn show | grep "192.168.201.1" | awk "{print \$1}")
[ -n "$ID_P" ] || { echo "FAIL: show lists no parked reply"; exit 1; }
"$BIN" gateway mn release "$ID_P" >/dev/null
"$BIN" thaw mn >/dev/null
sleep 2
ping -c 3 -W 2 192.168.201.2 >/dev/null || { echo "FAIL: no ICMP reply on the first tap"; exit 1; }
echo "  192.168.201.2 answers over tap1"

say "step 4: the tap claims are exclusive per tap"
"$BIN" create mn2 --net tap2 >/dev/null 2>&1 && { echo "FAIL: a claimed tap was granted again"; exit 1; }
echo "  tap2 refused: the list claims each tap"

say "step 5: freeze and thaw with two transports in the sidecar"
"$BIN" freeze mn >/dev/null
"$BIN" thaw mn >/dev/null
sleep 2
# No allow survives an epoch: prime the reply path again.
ping -c 1 -W 3 192.168.201.2 >/dev/null 2>&1 || true
deadline=$((SECONDS + 20))
until [ -f "$CELLA_HOME/machines/mn/state" ]; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: the post-thaw reply did not park"; exit 1; }
    sleep 1
done
ID_P2=$("$BIN" gateway mn show | grep "192.168.201.1" | awk "{print \$1}" | tail -1)
"$BIN" gateway mn release "$ID_P2" >/dev/null
"$BIN" thaw mn >/dev/null
sleep 2
ping -c 3 -W 2 192.168.201.2 >/dev/null || { echo "FAIL: no ICMP reply after the thaw"; exit 1; }
type_in 'ls /sys/class/net | grep -ac eth; echo nets-aliv"e"'
wait_for "nets-alive" || { echo "FAIL: the guest did not answer after the thaw"; exit 1; }
echo "  both transports rode the sidecar; eth0 answers after the thaw"

echo
echo "PASS: multi-net -- a machine takes N taps"
"$BIN" stop mn >/dev/null; "$BIN" destroy mn >/dev/null
