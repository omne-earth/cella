#!/usr/bin/env bash
# smoke-tls-terminator: the terminated pair (docs/NETWORK-MODEL.md,
# "The terminator"). Both machines boot the terminator golden: the
# appliance serves, and the member borrows the image's probe voice
# and the shared pair trust. One criterion per invocation:
#   t1  the interceptor: every name resolves to the appliance
#   t2  termination: the member's handshake verifies a minted leaf
#       against the baked pair CA (world dead: exit 3, the local
#       proof that interception, minting, and SNI all hold)
#   t3  the nameless splice: a static map carries plain TCP to a
#       host-side world through the resolved name
#   t4  the cache: the second flow asks the upstream nothing
#   t5  the unauthorized middle: a probe trusting a different
#       anchor refuses the handshake
#   t6  the named world: https://example.com end to end -- real
#       DNS, real world-leg TLS against the pinned roots, minted
#       leaf on the member leg (SKIPs without internet)
#   t9  throughput: a bulk transfer through the pair arrives whole
#       and fast -- the verdict path must never be the bottleneck
#       (t7 and t8, the no-VM verifier gates, live in their own
#       scripts)
set -uo pipefail

T="${1:-}"
case "$T" in
t1|t2|t3|t4|t5|t6|t9|t10) ;;
*) echo "usage: tls-terminator.sh <t1|t2|t3|t4|t5|t6|t9>"; exit 2 ;;
esac

cd "$(dirname "$0")/../.."
BIN=target/lab/cella
ENG=target/lab/cella-engine
WORLD_PORT=$(( (RANDOM % 8976) + 1024 ))
DIAL_PORT=$(( (RANDOM % 8976) + 1024 ))
DNS_PORT=$(( (RANDOM % 8976) + 1024 ))
HTTP_PORT=$(( (RANDOM % 8976) + 1024 ))
# The world services live on the host's LAN address: from a guest,
# 127.0.0.1 is the guest's own loopback and never leaves the wire.
HOST_IP=$(ip -4 route get 1.1.1.1 2>/dev/null | grep -oP 'src \K[0-9.]+' | head -1); [ -n "$HOST_IP" ] || HOST_IP=127.0.0.1
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build-lab"; exit 0; }
[ -f "$ENG" ] || { echo "SKIP: $ENG not built -- run: make build-lab"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:terminator || exit 0
if [ "$T" = t6 ]; then
    timeout 5 bash -c 'exec 3<>/dev/tcp/example.com/443' 2>/dev/null \
        || { echo "SKIP: no route to example.com -- the named world needs the internet"; exit 0; }
fi

say() { echo; echo "==> $1"; }
GW=10.77.0.1        # the appliance, member side (pair 0 convention)
MEMBER_IP=10.77.0.2

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-tlsterm.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/terminator"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/terminator/rootfs.ext4" "$CELLA_HOME/rootfs/terminator/"

TERM_VM=appliance
MEM_VM=member
WIRE="pair$RANDOM"
DNS_COUNT="$CELLA_HOME/dns-count"
MOTOR_PID=""; BT_PID=""; BM_PID=""; DNS_PID=""; HTTP_PID=""
evidence() {
    echo "-- member console:"; tail -25 "$CELLA_HOME/machines/$MEM_VM/console.log" 2>/dev/null | cat -v
    echo "-- appliance console:"; tail -25 "$CELLA_HOME/machines/$TERM_VM/console.log" 2>/dev/null | cat -v
    echo "-- motor (full):"; cat "$MOTOR_LOG" 2>/dev/null; echo "-- member probe lines:"; grep -a "probe" "$CELLA_HOME/machines/$MEM_VM/console.log" 2>/dev/null | cat -v
}
teardown() {
    for p in "$BT_PID" "$BM_PID" "$MOTOR_PID" "$DNS_PID" "$HTTP_PID"; do
        [ -n "$p" ] && kill "$p" 2>/dev/null || true
    done
    if [ -n "${CELLA_KEEP_SANDBOX:-}" ]; then
        for m in "$TERM_VM" "$MEM_VM"; do "$BIN" stop "$m" >/dev/null 2>&1 || true; done
        echo "kept: $CELLA_HOME"
        return
    fi
    for m in "$TERM_VM" "$MEM_VM"; do
        "$BIN" stop "$m" >/dev/null 2>&1 || true
        "$BIN" destroy "$m" >/dev/null 2>&1 || true
    done
    rm -rf "$CELLA_HOME"
}
trap teardown EXIT
type_term() { (printf '%s\n' "$1"; sleep 2) | timeout 25 "$BIN" enter "$TERM_VM" >/dev/null; }
type_mem() { (printf '%s\n' "$1"; sleep 2) | timeout 25 "$BIN" enter "$MEM_VM" >/dev/null; }
mem_log() { grep -a "$1" "$CELLA_HOME/machines/$MEM_VM/console.log"; }
thaw_all() {
    for m in "$TERM_VM" "$MEM_VM"; do
        [ -f "$CELLA_HOME/machines/$m/state" ] && "$BIN" thaw "$m" >/dev/null 2>&1 || true
    done
}
wait_console() { # <vm> <marker> <secs>
    local deadline=$((SECONDS + $3))
    until grep -aq "$2" "$CELLA_HOME/machines/$1/console.log" 2>/dev/null; do
        [ $SECONDS -lt $deadline ] || return 1
        thaw_all
        sleep 1
    done
}

# The host-side world: an upstream resolver that answers 127.0.0.1
# for every name and counts its questions, and a plain HTTP page.
python3 - "$DNS_PORT" "$DNS_COUNT" "$HOST_IP" <<'PYEOF' &
import socket, sys
port, cnt, host = int(sys.argv[1]), sys.argv[2], sys.argv[3]
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind((host, port))
n = 0
while True:
    d, a = s.recvfrom(512)
    n += 1
    open(cnt, "w").write(str(n))
    ans = d[:2] + b"\x81\x80\x00\x01\x00\x01\x00\x00\x00\x00" + d[12:]
    ans += b"\xc0\x0c\x00\x01\x00\x01\x00\x00\x00\x3c\x00\x04" + bytes(int(o) for o in host.split("."))
    s.sendto(ans, a)
PYEOF
DNS_PID=$!
mkdir -p "$CELLA_HOME/www" && echo "the-world-answers" > "$CELLA_HOME/www/index.html"
[ "$T" = t9 ] && dd if=/dev/zero of="$CELLA_HOME/www/bulk.bin" bs=1M count=16 status=none
(cd "$CELLA_HOME/www" && exec python3 -m http.server "$HTTP_PORT" --protocol HTTP/1.1 --bind "$HOST_IP" >/dev/null 2>&1) &
HTTP_PID=$!

say "$T: stand the pair, the judge, and the host world"
"$BIN" create "$TERM_VM" --rootfs terminator --net "world:$WORLD_PORT/udp,wire:$WIRE" >/dev/null
"$BIN" create "$MEM_VM" --rootfs terminator --net "wire:$WIRE" >/dev/null
"$BIN" start "$TERM_VM" >/dev/null
"$BIN" start "$MEM_VM" >/dev/null
MOTOR_LOG="$CELLA_HOME/motor.log"
# Destination grants, standing from stream-open. The terminator
# image pins its reply ports to 50000-50007 (the consistent reply
# port), so the appliance's replies to the member are eight exact
# destinations -- enumerable at policy time, no ephemeral naming.
GRANTS="--grant arp:600 \
    --grant $GW:443/tcp:600 \
    --grant $GW:53/udp:600 \
    --grant $GW:8080/tcp:600 \
    --grant $HOST_IP:$DNS_PORT/udp:600 \
    --grant $HOST_IP:$HTTP_PORT/tcp:600 \
    --grant $HOST_IP:443/tcp:600 \
    --grant 9.9.9.9:53/udp:600"
for p in $(seq 50000 50007); do
    GRANTS="$GRANTS --grant $MEMBER_IP:$p/tcp:600 --grant $MEMBER_IP:$p/udp:600"
done
# The named world (t6): the world-leg 443 destination is unknowable
# at policy time, so a released 443 park plants its own exact
# memory -- the remember rule, port-wildcarded.
# shellcheck disable=SC2086
"$ENG" motor --listen "127.0.0.1:$DIAL_PORT" --allow "*:*" $GRANTS \
    --remember "*:443:600" \
    > "$MOTOR_LOG" 2>&1 &
MOTOR_PID=$!
sleep 1
grep -q "motor: listening" "$MOTOR_LOG" || { echo "FAIL: the motor never listened"; exit 1; }
"$ENG" "$TERM_VM" --dial "127.0.0.1:$DIAL_PORT" > "$CELLA_HOME/bridge-term.log" 2>&1 &
BT_PID=$!
"$ENG" "$MEM_VM" --dial "127.0.0.1:$DIAL_PORT" > "$CELLA_HOME/bridge-mem.log" 2>&1 &
BM_PID=$!
sleep 2
"$BIN" gateway "$TERM_VM" open >/dev/null
"$BIN" gateway "$MEM_VM" open >/dev/null
sleep 1

wait_console "$TERM_VM" "cella-shell: getty" 30 || { echo "FAIL: the appliance never offered a console"; exit 1; }
wait_console "$MEM_VM" "cella-shell: getty" 30 || { echo "FAIL: the member never offered a console"; exit 1; }

say "  configure the pair (the gates' console hand; the field uses cmdline knobs)"
# The appliance: gate-local upstream and the t3 map -- except t6,
# which faces the real world: the true upstream, no map.
if [ "$T" = t6 ]; then
    APPLIANCE_CONF="wire_ip=$GW\nupstream_dns=9.9.9.9\nlisten=443,80\n"
else
    APPLIANCE_CONF="wire_ip=$GW\nupstream_dns=$HOST_IP:$DNS_PORT\nlisten=443,80\nmap=8080:w.test:$HTTP_PORT\n"
fi
type_term "printf '$APPLIANCE_CONF' > /etc/cella-terminator.conf; pkill cella-terminator; echo conf-o\"k\""
wait_console "$TERM_VM" "conf-ok" 30 || { echo "FAIL: the appliance took no configuration"; exit 1; }
# The member: its wire address and its resolver.
type_mem "ip link set lo up; ip addr add $MEMBER_IP/24 dev eth0; ip link set eth0 up; echo nameserver $GW > /etc/resolv.conf; printf 'wire_ip=127.0.0.1\nupstream_dns=9.9.9.9\n' > /etc/cella-terminator.conf; pkill cella-terminator; echo net-o\"k\""
wait_console "$MEM_VM" "net-ok" 30 || { echo "FAIL: the member took no address"; exit 1; }

case "$T" in

t1)
    say "t1: every name resolves to the appliance"
    type_mem "nslookup w.test $GW 2>&1; echo ns-don\"e\""
    wait_console "$MEM_VM" "ns-done" 40 || { echo "FAIL: the lookup never returned"; exit 1; }
    mem_log "$GW" | grep -qv "nameserver" || true
    grep -a -A1 "Name:" "$CELLA_HOME/machines/$MEM_VM/console.log" | grep -q "$GW" \
        || mem_log "Address.*$GW" >/dev/null \
        || { echo "FAIL: the answer was not the appliance"; echo "-- the member's last words:"; tail -15 "$CELLA_HOME/machines/$MEM_VM/console.log" | cat -v; exit 1; }
    echo "  the interceptor answered home"
    echo; echo "PASS: t1 -- the resolver is the interceptor"
    ;;

t2)
    say "t2: the minted leaf verifies against the baked pair CA"
    type_mem "/bin/cella-terminator --probe w.test 443 $GW /etc/cella/pair-ca.pem; echo probe-r\"c\"=\$?"
    wait_console "$MEM_VM" "probe-rc=" 60 || { echo "FAIL: the probe never returned"; exit 1; }
    mem_log "probe: verified w.test" >/dev/null \
        || { echo "FAIL: the handshake did not verify"; evidence; exit 1; }
    mem_log "probe-rc=3" >/dev/null \
        || { echo "FAIL: expected exit 3 (verified, world dead) -- $(mem_log 'probe-rc=' | tail -1)"; exit 1; }
    echo "  interception, SNI, minting, and the pair trust all hold"
    echo; echo "PASS: t2 -- termination verified"
    ;;

t3)
    say "t3: the static map splices plain TCP to the world"
    type_mem "wget -q -O- http://$GW:8080/ ; echo wget-r\"c\"=\$?"
    wait_console "$MEM_VM" "wget-rc=" 60 || { echo "FAIL: the fetch never returned"; exit 1; }
    mem_log "the-world-answers" >/dev/null \
        || { echo "FAIL: the world's page never arrived"; evidence; exit 1; }
    # The name ratchet: the appliance's world-leg park carries the
    # resolved name as testimony (proto Destination.host).
    "$BIN" --dump "$CELLA_HOME/machines/$TERM_VM/network/ledger" | grep -q "host=w.test" \
        || { echo "FAIL: no park carries the resolved name"; evidence; exit 1; }
    # The ratchet is durable: a freeze must not erase the name that
    # would stamp the next park (the names file survives the thaw).
    "$BIN" freeze "$TERM_VM" >/dev/null || { echo "FAIL: the appliance would not freeze"; exit 1; }
    "$BIN" thaw "$TERM_VM" >/dev/null || { echo "FAIL: the appliance would not thaw"; exit 1; }
    type_mem "wget -q -O- http://$GW:8080/ >/dev/null; echo re-don\"e\""
    wait_console "$MEM_VM" "re-done" 60 || { echo "FAIL: the post-thaw fetch never returned"; evidence; exit 1; }
    named=$("$BIN" --dump "$CELLA_HOME/machines/$TERM_VM/network/ledger" | grep -c "host=w.test")
    [ "$named" -ge 2 ] \
        || { echo "FAIL: the ratchet forgot at the thaw ($named named park)"; evidence; exit 1; }
    echo "  member -> appliance map -> resolved name -> host world, spliced"
    echo "  and the parks testify host=w.test through a freeze ($named of them)"
    echo; echo "PASS: t3 -- the nameless splice"
    ;;

t4)
    say "t4: the cache asks the upstream once"
    type_mem "wget -q -O- http://$GW:8080/ >/dev/null; echo one-don\"e\""
    wait_console "$MEM_VM" "one-done" 60 || { echo "FAIL: the first fetch never returned"; exit 1; }
    first=$(cat "$DNS_COUNT" 2>/dev/null || echo 0)
    [ "$first" -ge 1 ] || { echo "FAIL: the upstream was never asked"; evidence; exit 1; }
    type_mem "wget -q -O- http://$GW:8080/ >/dev/null; echo two-don\"e\""
    wait_console "$MEM_VM" "two-done" 60 || { echo "FAIL: the second fetch never returned"; exit 1; }
    second=$(cat "$DNS_COUNT")
    [ "$second" -eq "$first" ] \
        || { echo "FAIL: the cache leaked a question ($first -> $second)"; exit 1; }
    echo "  $first upstream question(s) total; the second flow asked nothing"
    echo; echo "PASS: t4 -- TTL-honest caching"
    ;;

t5)
    say "t5: an anchor that is not the pair's refuses the middle"
    # The config file is not a certificate: an empty trust store,
    # and the handshake must fail -- the member's protection.
    type_mem "/bin/cella-terminator --probe w.test 443 $GW /etc/cella-terminator.conf; echo probe-r\"c\"=\$?"
    wait_console "$MEM_VM" "probe-rc=" 60 || { echo "FAIL: the probe never returned"; exit 1; }
    if mem_log "probe: verified" >/dev/null; then
        echo "FAIL: a foreign anchor verified the middle"; exit 1
    fi
    mem_log "probe-rc=1" >/dev/null \
        || { echo "FAIL: expected exit 1 -- $(mem_log 'probe-rc=' | tail -1)"; exit 1; }
    echo "  no pair trust, no middle: the handshake refused"
    echo; echo "PASS: t5 -- the unauthorized middle is refused"
    ;;

t9)
    say "t9: a bulk transfer through the pair -- whole, and fast"
    T0=$SECONDS
    type_mem "wget -q -O /tmp/bulk http://$GW:8080/bulk.bin && wc -c /tmp/bulk; echo bulk-don\"e\""
    wait_console "$MEM_VM" "bulk-done" 120 || { echo "FAIL: the transfer never finished"; evidence; exit 1; }
    WALL=$((SECONDS - T0))
    # Whole: the exact byte count, or the stream truncated.
    mem_log "16777216 /tmp/bulk" >/dev/null \
        || { echo "FAIL: the transfer arrived torn -- $(mem_log '/tmp/bulk' | tail -1)"; evidence; exit 1; }
    # Fast: 16 MiB inside 32 s of wall (typing overhead included)
    # is >= 0.5 MiB/s end to end. The pre-ear bridge carried
    # ~27 kB/s -- ten minutes for this file; the floor separates
    # the regimes with a wide margin, not a benchmark's precision.
    [ "$WALL" -le 32 ] \
        || { echo "FAIL: 16 MiB took ${WALL}s -- the verdict path is the bottleneck again"; evidence; exit 1; }
    echo "  16 MiB in ${WALL}s, byte-exact: the pair carries bulk"
    echo; echo "PASS: t9 -- throughput through the pair"
    ;;

t6)
    say "t6: the named world -- https://example.com through the pair"
    type_mem "/bin/cella-terminator --probe example.com 443 $GW /etc/cella/pair-ca.pem; echo probe-r\"c\"=\$?"
    wait_console "$MEM_VM" "probe-rc=" 120 || { echo "FAIL: the probe never returned"; evidence; exit 1; }
    mem_log "probe: verified example.com" >/dev/null \
        || { echo "FAIL: the member-leg handshake did not verify"; evidence; exit 1; }
    mem_log "probe-rc=0" >/dev/null \
        || { echo "FAIL: expected exit 0 (the world answered) -- $(mem_log 'probe-rc=' | tail -1)"; evidence; exit 1; }
    echo "  real name, real roots, minted leaf: the whole seam against the world"
    echo; echo "PASS: t6 -- the named world"
    ;;

t10)
    say "t10: the retry storm -- rapid crossings all answer"
    # The reproduction of the ekdh4mm lockout: a client retrying
    # briskly is the most ordinary traffic there is, and every
    # crossing rides a world leg drawn from the appliance's
    # consistent reply window (8 ports). The contract: twelve
    # rapid sequential requests all answer. Under 60 s TIME_WAIT
    # the window is a graveyard after ~8 and connect() dies with
    # EADDRINUSE -- the member sees empty replies, this gate sees
    # fewer than twelve, and it FAILS until the drain is fixed.
    type_mem "for i in \$(seq 1 12); do (wget -q -O- -T 5 http://$GW:8080/ >/dev/null 2>&1 && echo hit >> /tmp/hits) & done; wait; echo burst-o\"k\"=\$(wc -l < /tmp/hits)"
    wait_console "$MEM_VM" "burst-ok=" 120 || { echo "FAIL: the burst never finished"; evidence; exit 1; }
    GOT=$(mem_log 'burst-ok=' | tail -1 | sed 's/.*burst-ok=//' | tr -dc 0-9)
    [ "${GOT:-0}" -eq 12 ] \
        || { echo "FAIL: only ${GOT:-0}/12 rapid crossings answered -- the reply window locked out"; evidence; exit 1; }
    echo "  12/12 rapid crossings answered: the window survives a retry storm"
    echo; echo "PASS: t10 -- the retry storm"
    ;;
esac
