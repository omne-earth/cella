#!/usr/bin/env bash
# Multi-net: a machine takes N taps. The gate boots a two-tap
# machine, proves both nics exist in the guest (eth0 configured by
# ip=, eth1 present for the init of a gateway image to configure),
# and pings eth0 from the host. Sandboxed CELLA_HOME.
set -euo pipefail

cd "$(dirname "$0")/../.."
BIN=target/smoke/cella
# The knock port: random per run, so a leaked translator from an
# earlier gate (a stale bind on a fixed port swallows knocks
# silently) can never poison this one. Four digits, unprivileged.
WORLD_PORT=$(( (RANDOM % 8976) + 1024 ))
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-multinet.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

teardown() {
    "$BIN" stop mn >/dev/null 2>&1 || true
    if [ -n "${CELLA_KEEP_SANDBOX:-}" ]; then echo "kept: $CELLA_HOME"; else rm -rf "$CELLA_HOME"; fi
}
trap teardown EXIT
say() { echo; echo "==> $1"; }
CON="$CELLA_HOME/machines/mn/console.log"
M="$CELLA_HOME/machines/mn"
knock() { printf 'knock\n' > /dev/udp/127.0.0.1/$WORLD_PORT 2>/dev/null || true; }
knock_loop() { local end=$((SECONDS + $1)); while [ $SECONDS -lt $end ]; do knock; sleep 1; done; }
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
# The ear's live wire: release every held incoming operation -- mail
# moves without a thaw. Against a frozen machine the release stages
# and the next thaw applies it.
pump_mail() {
    for id in $("$BIN" gateway mn show incoming | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
        "$BIN" gateway mn release "$id" >/dev/null
    done
}
# The stand-in engine: while the given pid runs, release every held
# operation of the frozen machine and thaw it (see ping.sh).
pump_while() { # pid
    local cycles=0
    while kill -0 "$1" 2>/dev/null; do
        pump_mail
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

say "step 1: create and start a machine on two nics: the world, and a wire"
"$BIN" create mn --net world:$WORLD_PORT/tcp+$WORLD_PORT/udp,wire:mn-w >/dev/null
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
B=$("$BIN" --dump-ledger "$M/network/ledger" 2>/dev/null | grep -c "dir=outgoing" || true); knock; knock; sleep 3
[ "$("$BIN" --dump-ledger "$M/network/ledger" 2>/dev/null | grep -c "dir=outgoing" || true)" -gt "$B" ] && { echo "FAIL: an open machine answered without a decision"; exit 1; }
# The knock parks incoming and never freezes the machine (the
# world's knock is not the resident's deed); release it live, and
# the guest's own reply parks in the egress lane -- that park is the
# freeze.
deadline=$((SECONDS + 20))
until [ -f "$M/state" ]; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: the first egress did not park and freeze"; exit 1; }
    pump_mail
    sleep 0.5
done
knock_loop 25 & P3=$!
pump_while "$P3"
wait "$P3" || { echo "FAIL: no reply landed on the first while the engine decided"; exit 1; }
"$BIN" --dump-ledger "$M/network/ledger" | grep -q "released id=.*bytes_out=[1-9]" || { echo "FAIL: no reply crossed on eth0"; exit 1; }
echo "  the knock is answered over eth0, every frame decided"

say "step 4: one valve spans all transports -- egress on eth1 parks too"
# The second nic belongs to the image init in the gateway flavor;
# here the gate configures it by hand. A park on any nic freezes
# the machine: the valve is the machine's, never a nic's.
if [ -f "$M/state" ]; then "$BIN" thaw mn >/dev/null; sleep 1; fi
type_in 'ip addr add 192.168.202.3/24 dev eth1'
type_in 'ip link set eth1 up'
type_in 'ping -c1 -W2 -I eth1 192.168.202.1 &'
deadline=$((SECONDS + 20))
until [ -f "$M/state" ]; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: egress on eth1 did not park and freeze"; exit 1; }
    sleep 1
done
"$BIN" gateway mn show | grep -qE "^[0-9a-f]{32} .*held$" || { echo "FAIL: show lists nothing held for the eth1 egress"; exit 1; }
echo "  the eth1 egress parked, and the one valve froze the machine"
for id in $("$BIN" gateway mn show | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
    "$BIN" gateway mn release "$id" >/dev/null
done
"$BIN" thaw mn >/dev/null
sleep 2

say "step 5: the grammar refuses a host object (negative)"
"$BIN" create mn2 --net tap2 >/dev/null 2>&1 && { echo "FAIL: a tap was granted -- the pool is gone"; exit 1; }
echo "  tap2 refused: there is no pool to claim from"

say "step 6: freeze and thaw with two transports in the sidecar"
if [ -f "$M/state" ]; then "$BIN" thaw mn >/dev/null; sleep 1; fi
"$BIN" freeze mn >/dev/null
"$BIN" thaw mn >/dev/null
sleep 2
# Nothing is inherited: every frame is decided again after the thaw.
knock_loop 25 & P5=$!
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
