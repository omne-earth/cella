#!/usr/bin/env bash
# smoke-udp: no frame leaves the machine undecided, proven from
# within the guest. The guest itself sends UDP and ICMP toward the
# host. Closed drops everything with no freeze. Open parks every
# frame under its most primitive name: the guest's broadcast ARP
# parks first (the L2 negative -- an unrefined ethertype never
# passes), its release lets the datagram park as an operation with
# proto and port, and a refusal delivers nothing: the host listener
# stays empty, and the guest sees its own send time out in-frame.
# Every assertion reads the guest console, the chronicle, or a host
# capture -- never a claim.
set -uo pipefail

cd "$(dirname "$0")/../.."
BIN=target/smoke/cella
TAP="${CELLA_TEST_TAP:-tap0}"
HOST_IP="${CELLA_TAP_CIDR:-192.168.200.1/24}"; HOST_IP="${HOST_IP%%/*}"
GUEST_IP="${CELLA_TEST_GUEST_IP:-192.168.200.2}"
UDP_PORT=9053

[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0
ip link show "$TAP" >/dev/null 2>&1 || { echo "SKIP: $TAP missing -- run: cella doctor fix"; exit 0; }
command -v nc >/dev/null || { echo "SKIP: nc missing -- run: make install-release"; exit 0; }

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-udp.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

VM=udptest
CAPTURE="$CELLA_HOME/capture"
LISTEN_PID=""
teardown() {
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    [ -n "${VMM_PID:-}" ] && kill -9 "$VMM_PID" 2>/dev/null || true
    [ -n "$LISTEN_PID" ] && kill "$LISTEN_PID" 2>/dev/null || true
    rm -rf "$CELLA_HOME"
}
trap teardown EXIT
say() { echo; echo "==> $1"; }
CON="$CELLA_HOME/machines/$VM/console.log"
STATE="$CELLA_HOME/machines/$VM/state"
# The console is a serial line with a 64-byte input FIFO: every
# typed line stays short, and the session holds until the line has
# echoed before the pipe closes.
type_in() { (printf '%s\n' "$1"; sleep 3) | timeout 15 "$BIN" enter "$VM" >/dev/null 2>&1 || true; }
wait_frozen() {
    # The sidecar lands before the old VMM exits: wait for both, or
    # the next thaw refuses a machine still running.
    local deadline=$((SECONDS + 20))
    until [ -f "$STATE" ]; do
        [ $SECONDS -lt $deadline ] || return 1
        sleep 1
    done
    local pid
    pid=$(cat "$CELLA_HOME/machines/$VM/pid" 2>/dev/null || true)
    while [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; do
        [ $SECONDS -lt $deadline ] || return 1
        sleep 0.2
    done
}
# The ear's live wire: release every held incoming operation --
# mail moves without a thaw.
pump_mail() {
    for id in $("$BIN" gateway "$VM" show incoming | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
        "$BIN" gateway "$VM" release "$id" >/dev/null
    done
}
# Wait for the next egress freeze, moving the mail meanwhile: the
# guest often needs an inbound answer (the ARP reply, a SYN-ACK)
# before its next outbound frame exists.
wait_frozen_mail() {
    local deadline=$((SECONDS + 25))
    until [ -f "$STATE" ]; do
        pump_mail
        [ $SECONDS -lt $deadline ] || return 1
        sleep 0.5
    done
    local pid
    pid=$(cat "$CELLA_HOME/machines/$VM/pid" 2>/dev/null || true)
    while [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; do
        [ $SECONDS -lt $deadline ] || return 1
        sleep 0.2
    done
}
wait_con() {
    local deadline=$((SECONDS + ${2:-20}))
    while [ $SECONDS -lt $deadline ]; do
        grep -aq "$1" "$CON" 2>/dev/null && return 0
        sleep 1
    done
    return 1
}

# The host listener: one process for the whole run. Any datagram that
# reaches the host lands in the capture file.
: > "$CAPTURE"
nc -u -l "$HOST_IP" "$UDP_PORT" > "$CAPTURE" 2>/dev/null &
LISTEN_PID=$!

say "step 1: born closed -- the guest's own datagram dies at the tap (negative)"
"$BIN" create "$VM" --net "$TAP" >/dev/null
[ "$(cat "$CELLA_HOME/machines/$VM/valve")" = "closed" ] || { echo "FAIL: the valve record is not born closed"; exit 1; }
"$BIN" start "$VM" >/dev/null
VMM_PID=$(cat "$CELLA_HOME/machines/$VM/pid")
sleep 4
type_in "echo cp > /dev/udp/$HOST_IP/$UDP_PORT"
type_in "echo sent-close\"d\""
wait_con "sent-closed" || { echo "FAIL: the guest shell did not run the send"; exit 1; }
sleep 2
[ -s "$CAPTURE" ] && { echo "FAIL: a closed machine's datagram reached the host"; exit 1; }
[ -f "$STATE" ] && { echo "FAIL: a closed machine froze on its own egress"; exit 1; }
echo "  the datagram died at the tap: no delivery, no freeze"

say "step 2: open -- the L2 name parks first, then the datagram (negative + positive)"
"$BIN" gateway "$VM" open >/dev/null
sleep 1
type_in "echo op > /dev/udp/$HOST_IP/$UDP_PORT"
wait_frozen || { echo "FAIL: the parked frame did not freeze the machine"; exit 1; }
"$BIN" gateway "$VM" show | sed 's/^/   /'
# The most primitive name first: the guest's broadcast ARP parks as
# an L2 operation -- an unrefined ethertype never passes the
# membrane.
ID_A=$("$BIN" gateway "$VM" show | grep "arp " | awk '{print $1}' | tail -1)
[ -n "$ID_A" ] || { echo "FAIL: show lists no held L2 operation for the ARP"; exit 1; }
[ -s "$CAPTURE" ] && { echo "FAIL: something reached the host before a decision"; exit 1; }
"$BIN" gateway "$VM" release "$ID_A" >/dev/null
"$BIN" thaw "$VM" >/dev/null
# The host's ARP reply is mail: it parks incoming while the
# machine runs, and its live release resolves the neighbor. The
# datagram then parks, refined: address, port, protocol.
wait_frozen_mail || { echo "FAIL: the datagram did not park after the resolution"; exit 1; }
"$BIN" gateway "$VM" show | sed 's/^/   /'
ID_U=$("$BIN" gateway "$VM" show | grep "$HOST_IP:$UDP_PORT" | awk '{print $1}' | tail -1)
[ -n "$ID_U" ] || { echo "FAIL: show lists no held operation for $HOST_IP:$UDP_PORT"; exit 1; }
[ -s "$CAPTURE" ] && { echo "FAIL: the datagram reached the host before a decision"; exit 1; }
echo "  held: arp first, then $HOST_IP:$UDP_PORT -- nothing delivered"

say "step 3: refuse -- the datagram never leaves, and the guest sees the silence"
"$BIN" gateway "$VM" refuse "$ID_U" --why "smoke-udp: the world side carries no datagrams" >/dev/null
"$BIN" thaw "$VM" >/dev/null
VMM_PID=$(cat "$CELLA_HOME/machines/$VM/pid")
sleep 2
[ -s "$CAPTURE" ] && { echo "FAIL: a refused datagram reached the host"; exit 1; }
"$BIN" gateway "$VM" show --all | grep -q "lapsed" || { echo "FAIL: the chronicle records no lapse"; exit 1; }
echo "  refused: the capture is empty, the chronicle says lapsed"

say "step 4: the guest's own ICMP parks and a refusal answers nothing (negative)"
type_in "ping -c1 -W2 $HOST_IP || echo no-repl\"y\""
wait_frozen || { echo "FAIL: the parked echo request did not freeze the machine"; exit 1; }
ID_I=$("$BIN" gateway "$VM" show | grep "$HOST_IP:0" | awk '{print $1}' | tail -1)
[ -n "$ID_I" ] || { echo "FAIL: show lists no held echo request"; exit 1; }
"$BIN" gateway "$VM" refuse "$ID_I" --why "smoke-udp: ICMP dies with UDP" >/dev/null
"$BIN" thaw "$VM" >/dev/null
wait_con "no-reply" || { echo "FAIL: the guest never reported its in-frame timeout"; exit 1; }
echo "  the echo request lapsed; the guest timed out in its own frame"

say "step 5: the ear -- the world's datagram parks incoming, and a refusal drops it unseen"
if [ -f "$STATE" ]; then "$BIN" thaw "$VM" >/dev/null; sleep 1; fi
pump_mail
echo knock | timeout 2 nc -u -w 1 "$GUEST_IP" 9099 2>/dev/null || true
sleep 2
[ -f "$STATE" ] && { echo "FAIL: the machine froze on inbound -- the knock is not its deed"; exit 1; }
ID_IN=$("$BIN" gateway "$VM" show incoming | grep "$HOST_IP:9099\|$HOST_IP" | awk '{print $1}' | tail -1)
[ -n "$ID_IN" ] || { echo "FAIL: show incoming lists no held datagram from the host"; exit 1; }
# The lane pops front first: refuse everything held, oldest
# included, or the datagram's refusal waits behind an undecided
# predecessor -- fail-closed, as ruled.
for id in $("$BIN" gateway "$VM" show incoming | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
    "$BIN" gateway "$VM" refuse "$id" --why "smoke-udp: unsolicited mail" >/dev/null
done
deadline=$((SECONDS + 8))
until "$BIN" gateway "$VM" show --all | grep -q "^${ID_IN:0:16}.*lapsed (smoke-udp: unsolicited mail)"; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: the refused mail did not lapse in the book"; exit 1; }
    sleep 1
done
[ -f "$STATE" ] && { echo "FAIL: refusing mail froze the machine"; exit 1; }
echo "  the knock parked incoming, was refused, and died unseen -- no freeze at any point"

echo
echo "PASS: no frame crosses undecided, either way -- closed drops, open parks both lanes, refusal delivers nothing"
"$BIN" stop "$VM" >/dev/null; "$BIN" destroy "$VM" >/dev/null
