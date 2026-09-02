#!/usr/bin/env bash
# smoke-udp: no datagram leaves the machine undecided, proven from
# within the guest. The guest itself sends UDP and ICMP toward the
# host. Closed drops the datagram with no freeze. Open parks it as an
# operation (proto and port visible in show) and the park is the
# freeze. A refusal delivers nothing: the host listener stays empty,
# and the guest sees its own send time out in-frame. Every assertion
# reads either the guest console or a host capture -- never a claim.
set -uo pipefail

cd "$(dirname "$0")/../.."
BIN=target/smoke/cella
TAP="${CELLA_TEST_TAP:-tap0}"
HOST_IP="${CELLA_TAP_CIDR:-192.168.200.1/24}"; HOST_IP="${HOST_IP%%/*}"
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
    local deadline=$((SECONDS + 20))
    until [ -f "$STATE" ]; do
        [ $SECONDS -lt $deadline ] || return 1
        sleep 1
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

say "step 2: open -- the datagram parks as an operation, and the park is the freeze"
"$BIN" gateway "$VM" open >/dev/null
sleep 1
type_in "echo op > /dev/udp/$HOST_IP/$UDP_PORT"
wait_frozen || { echo "FAIL: the parked datagram did not freeze the machine"; exit 1; }
"$BIN" gateway "$VM" show | sed 's/^/   /'
ID_U=$("$BIN" gateway "$VM" show | grep "$HOST_IP:$UDP_PORT" | awk '{print $1}' | tail -1)
[ -n "$ID_U" ] || { echo "FAIL: show lists no held operation for $HOST_IP:$UDP_PORT"; exit 1; }
[ -s "$CAPTURE" ] && { echo "FAIL: the datagram reached the host before a decision"; exit 1; }
echo "  held: $ID_U ($HOST_IP:$UDP_PORT), nothing delivered"

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

echo
echo "PASS: no datagram leaves undecided -- closed drops, open parks, refusal delivers nothing"
"$BIN" stop "$VM" >/dev/null; "$BIN" destroy "$VM" >/dev/null
