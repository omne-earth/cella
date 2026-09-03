#!/usr/bin/env bash
# The world plane, stateless half (1.6.14e rung 3): a machine with
# --net world reaches the world through its translator's sockets --
# no tap, no NAT, no capability anywhere. ARP and the gateway's
# echo answer at the edge; ICMP and UDP cross through real sockets;
# every crossing parks and is decided, both directions.
set -euo pipefail

cd "$(dirname "$0")/../.."
BIN=target/smoke/cella
# The knock port: random per run, so a leaked translator from an
# earlier gate (a stale bind on a fixed port swallows knocks
# silently) can never poison this one. Four digits, unprivileged.
WORLD_PORT=$(( (RANDOM % 8976) + 1024 ))
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build-smoke"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0

say() { echo; echo "==> $1"; }
GW=192.168.210.1

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-world.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

NC_PID=""
teardown() {
    for p in "${NC_PID:-}" "${TCP_PID:-}" "${KNOCK_PID:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done
    "$BIN" stop wo >/dev/null 2>&1 || true
    if [ -n "${CELLA_KEEP_SANDBOX:-}" ]; then
        p=$(cat "$CELLA_HOME/machines/wo/edge.pid" 2>/dev/null || true)
        [ -n "$p" ] && kill -9 "$p" 2>/dev/null || true
        echo "kept: $CELLA_HOME"
    else
        "$BIN" destroy wo >/dev/null 2>&1 || true
        rm -rf "$CELLA_HOME"
    fi
}
trap teardown EXIT
type_in() { local vm="$1"; shift; (printf '%s\n' "$1"; sleep 2) | timeout 20 "$BIN" enter "$vm" >/dev/null; }
# The decision pump: release incoming mail live; release a frozen
# machine's holds and thaw it.
pump() { # marker budget
    local marker="$1" budget="$2" cycles=0
    until grep -aq "$marker" "$CELLA_HOME/machines/wo/console.log"; do
        cycles=$((cycles + 1))
        [ $cycles -le "$budget" ] || return 1
        for id in $("$BIN" gateway wo show incoming | sed -n "s/^\([0-9a-f]\{32\}\) .*held\$/\1/p"); do
            "$BIN" gateway wo release "$id" >/dev/null
        done
        if [ -f "$CELLA_HOME/machines/wo/state" ]; then
            pid=$(cat "$CELLA_HOME/machines/wo/pid" 2>/dev/null || true)
            if [ -z "$pid" ] || ! kill -0 "$pid" 2>/dev/null; then
                for id in $("$BIN" gateway wo show | sed -n "s/^\([0-9a-f]\{32\}\) .*held\$/\1/p"); do
                    "$BIN" gateway wo release "$id" >/dev/null
                done
                "$BIN" thaw wo >/dev/null
            fi
        fi
        sleep 0.5
    done
}

say "step 1: a machine on --net world -- no tap, no host interface, a translator"
"$BIN" create wo --net world:$WORLD_PORT/tcp >/dev/null
"$BIN" start wo >/dev/null
[ -f "$CELLA_HOME/machines/wo/edge.pid" ] || { echo "FAIL: no translator"; exit 1; }
ip -br link | grep -qE "tap.*wo|wo.*tap" && { echo "FAIL: a host interface appeared"; exit 1; }
sleep 4

say "step 2: born closed -- the gateway answers nothing (negative)"
type_in wo "ping -c1 -W2 $GW >/dev/null && echo gw-aliv\"e\" || echo gw-dar\"k\""
deadline=$((SECONDS + 20))
until grep -aq "gw-dark" "$CELLA_HOME/machines/wo/console.log"; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: no report from the closed machine"; exit 1; }
    sleep 1
done
grep -aq "gw-alive" "$CELLA_HOME/machines/wo/console.log" && { echo "FAIL: a closed machine reached its gateway"; exit 1; }
[ -f "$CELLA_HOME/machines/wo/state" ] && { echo "FAIL: a closed machine froze on its own egress"; exit 1; }
echo "  closed drops at the membrane; the translator never heard a thing"

say "step 3: open -- ARP parks first, the gateway's echo crosses decided"
"$BIN" gateway wo open >/dev/null
sleep 1
type_in wo "ping -c3 -W30 $GW >/dev/null && echo gw-o\"k\""
pump "gw-ok" 120 || { echo "FAIL: the gateway echo did not cross"; exit 1; }
echo "  ARP answered at the edge, the echo answered at the edge, every hop decided"

say "step 4: UDP to the real host through a socket, the reply parks incoming"
UDP_PORT=$WORLD_PORT
LISTENER_OUT="$CELLA_HOME/listener.out"
# A python listener, not nc: it binds fresh (no stale-port flake),
# records the knock to a file, and repeats the reply -- the judged
# path may take seconds and UDP owes nobody a retransmit.
python3 - "$UDP_PORT" "$LISTENER_OUT" <<'PY' &
import socket, sys, time
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("0.0.0.0", int(sys.argv[1])))
s.settimeout(120)
try:
    data, peer = s.recvfrom(2048)
    with open(sys.argv[2], "w") as f:
        f.write(data.decode(errors="replace").strip())
    for _ in range(30):
        s.sendto(b"world-answer\n", peer)
        time.sleep(1)
except Exception as e:
    with open(sys.argv[2] + ".err", "w") as f:
        f.write(str(e))
PY
NC_PID=$!
sleep 1
HOST_IP=$(ip -4 route get 1.1.1.1 2>/dev/null | grep -oP 'src \K[0-9.]+' | head -1)
[ -n "$HOST_IP" ] || HOST_IP=127.0.0.1
type_in wo "echo world-knock > /dev/udp/$HOST_IP/$UDP_PORT && echo udp-sen\"t\""
pump "udp-sent" 120 || { echo "FAIL: the datagram did not leave"; exit 1; }
deadline=$((SECONDS + 30))
until [ -s "$LISTENER_OUT" ]; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: the datagram never reached the host socket"; exit 1; }
    sleep 1
done
echo "  the datagram crossed a plain socket: $(cat "$LISTENER_OUT")"

say "step 5: the reply crosses back and parks in the ingress lane"
# The chronicle is the proof: the reply must appear as an incoming
# park from the host -- whether a later release followed or not.
LEDGER="$CELLA_HOME/machines/wo/network/ledger"
deadline=$((SECONDS + 30))
until "$BIN" --dump-ledger "$LEDGER" 2>/dev/null | grep -q "dir=incoming ip=$HOST_IP port=$UDP_PORT"; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: the reply never parked incoming"; exit 1; }
    sleep 1
done
echo "  the world's answer parked in the ingress lane -- the chronicle carries it"

say "step 6: TCP out -- the guest's SYN becomes a connect, bytes cross, every segment decided"
TCP_PORT=$((WORLD_PORT + 1))
TCP_OUT="$CELLA_HOME/tcp.out"
python3 - "$TCP_PORT" "$TCP_OUT" <<'PY' &
import socket, sys
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("0.0.0.0", int(sys.argv[1]))); s.listen(4); s.settimeout(180)
try:
    c, peer = s.accept(); c.settimeout(60)
    data = c.recv(4096)
    with open(sys.argv[2], "w") as f:
        f.write(data.decode(errors="replace").strip())
    c.sendall(b"tcp-answer\n"); c.close()
except Exception as e:
    with open(sys.argv[2] + ".err", "w") as f:
        f.write(str(e))
PY
TCP_PID=$!
sleep 1
type_in wo "printf 'tcp-hello\\n' | nc $HOST_IP $TCP_PORT; echo tcp-don\"e\""
pump "tcp-done" 200 || { echo "FAIL: the TCP exchange did not complete"; exit 1; }
deadline=$((SECONDS + 30))
until [ -s "$TCP_OUT" ]; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: the host socket never received the guest's bytes"; exit 1; }
    sleep 1
done
echo "  the guest's bytes crossed a real TCP socket: $(cat "$TCP_OUT")"
grep -aq "tcp-answer" "$CELLA_HOME/machines/wo/console.log" || { echo "FAIL: the host's answer never reached the guest"; exit 1; }
echo "  the answer crossed back, parked, released, and landed in the guest"
kill "$TCP_PID" 2>/dev/null || true

say "step 7: the knock -- a connection on the mapped port parks incoming, no freeze (negative)"
KNOCK_OUT="$CELLA_HOME/knock.out"
python3 - "$HOST_IP" "$KNOCK_OUT" "$WORLD_PORT" <<'PY' &
import socket, sys, time
try:
    c = socket.create_connection((sys.argv[1], int(sys.argv[3])), timeout=60)
    c.sendall(b"knock\n")
    with open(sys.argv[2], "w") as f:
        f.write("connected")
    time.sleep(20); c.close()
except Exception as e:
    with open(sys.argv[2], "w") as f:
        f.write("error: " + str(e))
PY
KNOCK_PID=$!
deadline=$((SECONDS + 30))
until "$BIN" --dump-ledger "$LEDGER" 2>/dev/null | grep -q "dir=incoming ip=$HOST_IP port=$WORLD_PORT\|dir=incoming.*port=$WORLD_PORT"; do
    [ $SECONDS -lt $deadline ] || { echo "FAIL: the knock never parked incoming"; exit 1; }
    sleep 1
done
[ -f "$CELLA_HOME/machines/wo/state" ] && { echo "FAIL: the machine froze on a knock -- the world's knock is not the resident's deed"; exit 1; }
echo "  the knock stands in the ingress lane; the machine keeps running"
kill "$KNOCK_PID" 2>/dev/null || true

echo
echo "PASS: the world plane -- sockets instead of taps, ICMP/UDP/TCP decided both ways, the knock parks"
