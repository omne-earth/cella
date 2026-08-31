#!/usr/bin/env bash
# Acceptance gates for docs/DEVICE-STATE.md. One argument selects the
# criterion: ac1, ac2, ac3, or ac4. A gate that is not implemented yet
# fails; the FAIL is the specification.
set -ueo pipefail

cd "$(dirname "$0")/../.."
BIN=target/release/cella
ac="${1:-}"

case "$ac" in
ac1 | ac2 | ac3 | ac4) ;;
*)
	echo "usage: $0 ac1|ac2|ac3|ac4" >&2
	exit 2
	;;
esac

[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build"; exit 0; }
if ! [ -r /dev/kvm ] || ! [ -w /dev/kvm ]; then
    echo "SKIP: no read and write access to /dev/kvm"; exit 0
fi
command -v bwrap >/dev/null || { echo "SKIP: bwrap not found -- run: make init"; exit 0; }

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
for f in kernel/canonical/bzImage rootfs/cella/rootfs.ext4; do
    [ -f "$REAL_HOME/$f" ] || { echo "SKIP: golden $f missing -- run: make golden"; exit 0; }
done
export CELLA_HOME=$(mktemp -d /tmp/cella-devstate.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

VM=devstate
teardown() {
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    rm -rf "$CELLA_HOME"
}
trap teardown EXIT
say() { echo; echo "==> $1"; }
type_in() { (printf '%s\n' "$1"; sleep 2) | timeout 20 "$BIN" enter "$VM" >/dev/null; }
CON="$CELLA_HOME/machines/$VM/console.log"

# Wait until a marker shows in the console log, with a deadline.
wait_for() {
    local marker="$1" deadline=$((SECONDS + 15))
    while [ $SECONDS -lt $deadline ]; do
        grep -aq "$marker" "$CON" && return 0
        sleep 1
    done
    return 1
}

case "$ac" in
ac1)
	echo "AC1: the disk survives the thaw (rw root; write, freeze, thaw, read back, sync)."

	say "step 1: create and start a machine on a rw root"
	"$BIN" create "$VM" --net none >/dev/null
	"$BIN" start "$VM" >/dev/null
	sleep 4

	say "step 2: write a file to the disk, and sync it"
	type_in 'echo payload-$((6*7)) > /ac1.txt; sync; echo disk-write-d"one"'
	wait_for "disk-write-done" || { echo "FAIL: the pre-freeze write did not complete"; exit 1; }

	say "step 3: freeze, then thaw"
	"$BIN" freeze "$VM" >/dev/null
	"$BIN" thaw "$VM" >/dev/null
	sleep 2

	say "step 4: read the file back through the thawed disk"
	type_in 'echo "readback: $(cat /ac1.txt)"'
	wait_for "readback: payload-42" || { echo "FAIL: the post-thaw read hung or returned wrong data"; exit 1; }

	say "step 5: write and sync after the thaw (the field-evidence wedge)"
	type_in 'echo post-thaw >> /ac1.txt; sync; echo post-thaw-write-d"one"'
	wait_for "post-thaw-write-done" || { echo "FAIL: the post-thaw write wedged (see docs/DEVICE-STATE.md)"; exit 1; }

	echo
	echo "PASS: AC1 -- the disk survived the thaw"
	"$BIN" stop "$VM" >/dev/null; "$BIN" destroy "$VM" >/dev/null
	;;
ac2)
	echo "AC2: the network survives the thaw (ping, freeze, thaw, ping again)."
	TAP="${CELLA_TEST_TAP:-tap0}"
	HOST_IP="${CELLA_TEST_HOST_IP:-192.168.200.1}"
	GUEST_IP="${CELLA_TEST_GUEST_IP:-192.168.200.2}"
	if ! ip addr show "$TAP" 2>/dev/null | grep -q "$HOST_IP"; then
		echo "SKIP: $TAP is not configured with $HOST_IP -- run: sudo cella setup net"
		exit 0
	fi

	say "step 1: create and start a machine on $TAP"
	"$BIN" create "$VM" --net "$TAP" >/dev/null
	"$BIN" start "$VM" >/dev/null
	sleep 6

	say "step 2: the host pings the guest"
	ping -c 3 -W 2 "$GUEST_IP" >/dev/null || { echo "FAIL: no ICMP reply before the freeze"; exit 1; }
	echo "  $GUEST_IP answers over $TAP"

	say "step 3: freeze, then thaw (the tap claim rides the manifest)"
	"$BIN" freeze "$VM" >/dev/null
	grep -q "$TAP" "$CELLA_HOME/machines/$VM/manifest.json" || { echo "FAIL: the manifest lost the tap claim"; exit 1; }
	"$BIN" thaw "$VM" >/dev/null
	sleep 2

	say "step 4: the host pings the guest again, through the thawed transport"
	ping -c 3 -W 2 "$GUEST_IP" >/dev/null || { echo "FAIL: no ICMP reply after the thaw (see docs/DEVICE-STATE.md)"; exit 1; }
	echo "  $GUEST_IP answers over $TAP"

	echo
	echo "PASS: AC2 -- the network survived the thaw"
	"$BIN" stop "$VM" >/dev/null; "$BIN" destroy "$VM" >/dev/null
	;;
ac3)
	echo "AC3: the in-flight layer is exact (a parked egress request is delivered"
	echo "and completed after the thaw; the same request works, no retransmission)."
	TAP="${CELLA_TEST_TAP:-tap0}"
	HOST_IP="${CELLA_TEST_HOST_IP:-192.168.200.1}"
	if ! ip addr show "$TAP" 2>/dev/null | grep -q "$HOST_IP"; then
		echo "SKIP: $TAP is not configured with $HOST_IP -- run: sudo cella setup net"
		exit 0
	fi

	say "step 1: create and start a machine on $TAP"
	"$BIN" create "$VM" --net "$TAP" >/dev/null
	"$BIN" start "$VM" >/dev/null
	sleep 6
	VMM_PID=$(cat "$CELLA_HOME/machines/$VM/pid")

	# The serial RX FIFO holds 64 bytes; every typed line stays short.
	say "step 2: prove real egress -- the guest fetches the hugging face page"
	type_in 'mkdir -p /etc; echo nameserver 1.1.1.1 >/etc/resolv.conf'
	type_in 'U=http://huggingface.co'
	type_in 'wget -q -O /dev/null $U && echo www-o"k"'
	if ! wait_for "www-ok"; then
		echo "SKIP: the guest has no route to the www (host forwarding/NAT -- see install.sh)"
		exit 0
	fi

	say "step 3: egress hold on, then the same fetch -- its frames park"
	kill -USR2 "$VMM_PID"
	sleep 1
	type_in 'wget -q -O /dev/null $U && echo held-o"k" &'
	sleep 1
	grep -aq "held-ok" "$CON" && { echo "FAIL: the request left the machine while held"; exit 1; }
	echo "  the request is in flight, and parked"

	say "step 4: freeze -- the parked frames ride the sidecar"
	"$BIN" freeze "$VM" >/dev/null
	grep -aq "held egress frame" "$CELLA_HOME/machines/$VM/vmm.log" \
		|| { echo "FAIL: the freeze holds no egress frame"; exit 1; }
	grep -a "held egress frame" "$CELLA_HOME/machines/$VM/vmm.log" | tail -1 | sed "s/^/  /"

	say "step 5: thaw -- the held frames are delivered and completed"
	"$BIN" thaw "$VM" >/dev/null
	wait_for "held-ok" || { echo "FAIL: the parked request did not complete after the thaw"; exit 1; }
	echo "  the same request landed, and the page came back"

	echo
	echo "PASS: AC3 -- the in-flight layer is exact"
	"$BIN" stop "$VM" >/dev/null; "$BIN" destroy "$VM" >/dev/null
	;;
ac4)
	echo "AC4: the verdict is external (the world-ratchet gate). Every egress"
	echo "frame parks; the test, as the stand-in engine, renders the verdicts."
	TAP="${CELLA_TEST_TAP:-tap0}"
	HOST_IP="${CELLA_TEST_HOST_IP:-192.168.200.1}"
	if ! ip addr show "$TAP" 2>/dev/null | grep -q "$HOST_IP"; then
		echo "SKIP: $TAP is not configured with $HOST_IP -- run: sudo cella setup net"
		exit 0
	fi
	command -v python3 >/dev/null || { echo "SKIP: python3 not found (the stand-in endpoints)"; exit 0; }
	# A stand-in endpoint leaked by an interrupted run squats its port
	# and serves a deleted directory; sweep them first.
	pkill -f "http.server (8080|9090) --bind $HOST_IP" 2>/dev/null || true
	WWW=$(mktemp -d); echo world > "$WWW/index.html"
	SRV1=""; SRV2=""
	stop_srv() { kill $SRV1 $SRV2 2>/dev/null; rm -rf "$WWW"; }
	trap 'stop_srv; "$BIN" stop "$VM" >/dev/null 2>&1 || true; rm -rf "$CELLA_HOME"' EXIT
	VMM="$CELLA_HOME/machines/$VM/vmm.log"

	say "step 1: create and start a machine on $TAP"
	"$BIN" create "$VM" --net "$TAP" >/dev/null
	"$BIN" start "$VM" >/dev/null
	sleep 6
	VMM_PID=$(cat "$CELLA_HOME/machines/$VM/pid")

	say "step 2: prove the path to the host endpoint, before any hold"
	python3 -m http.server 8080 --bind "$HOST_IP" --directory "$WWW" >/dev/null 2>&1 & SRV1=$!
	sleep 1
	type_in "H=http://$HOST_IP"
	type_in 'wget -q -O /dev/null $H:8080 && echo pre-o"k"'
	if ! wait_for "pre-ok"; then
		# ICMP passes through most zones; unsolicited TCP to the host
		# does not. The taps must sit in the trusted zone.
		echo "SKIP: the guest cannot reach $HOST_IP:8080 -- rerun: sudo cella setup net"
		exit 0
	fi

	say "step 3: egress hold on; the same request parks, and reports"
	kill -USR2 "$VMM_PID"
	sleep 1
	type_in 'wget -q -O /dev/null $H:8080 && echo rel-o"k" &'
	deadline=$((SECONDS + 15))
	until grep -aq "parked egress to $HOST_IP:8080" "$VMM"; do
		[ $SECONDS -lt $deadline ] || { echo "FAIL: no park report for :8080"; exit 1; }
		sleep 1
	done
	grep -aq "rel-ok" "$CON" && { echo "FAIL: the request passed without a verdict"; exit 1; }
	echo "  parked, and reported"

	say "step 4: the engine renders release with allow -- the flow completes"
	echo "allow $HOST_IP:8080" > "$CELLA_HOME/machines/$VM/verdict"
	kill -WINCH "$VMM_PID"
	wait_for "rel-ok" || {
		echo "FAIL: the released request did not complete"
		echo "-- vmm.log:"; tail -6 "$VMM" | sed "s/^/   /"
		echo "-- console:"; tail -4 "$CON" | sed "s/^/   /"
		exit 1
	}
	PARKS=$(grep -ac "parked egress to $HOST_IP:8080" "$VMM")
	type_in 'wget -q -O /dev/null $H:8080 && echo rel2-o"k"'
	wait_for "rel2-ok" || { echo "FAIL: the allowed flow did not run at full speed"; exit 1; }
	[ "$(grep -ac "parked egress to $HOST_IP:8080" "$VMM")" = "$PARKS" ] \
		|| { echo "FAIL: the allow entry did not pass the second request"; exit 1; }
	echo "  one park and one verdict per destination; later frames match inline"

	say "step 5: a request to a part of the world that does not exist -- it parks"
	type_in 'wget -q -O /dev/null $H:9090 && echo world-o"k" &'
	deadline=$((SECONDS + 15))
	until grep -aq "parked egress to $HOST_IP:9090" "$VMM"; do
		[ $SECONDS -lt $deadline ] || { echo "FAIL: no park report for :9090"; exit 1; }
		sleep 1
	done
	echo "  parked, and reported"

	say "step 6: the engine freezes the machine, and grows the world"
	"$BIN" freeze "$VM" >/dev/null
	grep -aq "held egress frame" "$VMM" || { echo "FAIL: the freeze holds no egress frame"; exit 1; }
	python3 -m http.server 9090 --bind "$HOST_IP" --directory "$WWW" >/dev/null 2>&1 & SRV2=$!
	sleep 1
	echo "  the endpoint at :9090 now exists"

	say "step 7: thaw -- the same request lands on the endpoint that now exists"
	"$BIN" thaw "$VM" >/dev/null
	wait_for "world-ok" || {
		echo "FAIL: the parked request did not land after the thaw"
		echo "-- vmm.log:"; tail -6 "$VMM" | sed "s/^/   /"
		echo "-- console:"; tail -4 "$CON" | sed "s/^/   /"
		exit 1
	}
	echo "  the request completed against the new endpoint; the guest saw no failure"

	echo
	echo "PASS: AC4 -- the verdict is external"
	"$BIN" stop "$VM" >/dev/null; "$BIN" destroy "$VM" >/dev/null
	;;
esac
