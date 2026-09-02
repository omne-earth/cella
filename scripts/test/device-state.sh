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
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-devstate.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

VM=devstate
teardown() {
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    [ -n "${VMM_PID:-}" ] && kill -9 "$VMM_PID" 2>/dev/null || true
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

	say "step 2: open, prime the reply path, then the host pings the guest"

	# Born closed: open the valve, then prime the reply path -- the
	# first ping parks the guest's reply and freezes the machine;
	# the release makes the pass entry, and pings answer after.
	"$BIN" gateway "$VM" open >/dev/null
	sleep 1
	ping -c 1 -W 3 $GUEST_IP >/dev/null 2>&1 || true
	deadline=$((SECONDS + 20))
	until [ -f "$CELLA_HOME/machines/$VM/state" ]; do
		[ $SECONDS -lt $deadline ] || { echo "FAIL: the reply did not park and freeze"; exit 1; }
		sleep 1
	done
	ID_P=$("$BIN" gateway "$VM" show | grep "$HOST_IP" | awk "{print \$1}")
	[ -n "$ID_P" ] || { echo "FAIL: show lists no parked reply"; exit 1; }
	"$BIN" gateway "$VM" release "$ID_P" >/dev/null
	"$BIN" thaw "$VM" >/dev/null
	sleep 2
	ping -c 3 -W 2 "$GUEST_IP" >/dev/null || { echo "FAIL: no ICMP reply after the release"; exit 1; }
	echo "  $GUEST_IP answers over $TAP"

	say "step 3: freeze, then thaw (the tap claim rides the manifest)"
	"$BIN" freeze "$VM" >/dev/null
	grep -q "$TAP" "$CELLA_HOME/machines/$VM/manifest.json" || { echo "FAIL: the manifest lost the tap claim"; exit 1; }
	"$BIN" thaw "$VM" >/dev/null
	sleep 2

	say "step 4: the host pings the guest again, through the thawed transport"
	# No allow survives an epoch: the post-thaw ping parks again,
	# and the engine decides again -- atomically, every time.
	ping -c 1 -W 3 "$GUEST_IP" >/dev/null 2>&1 || true
	deadline=$((SECONDS + 20))
	until [ -f "$CELLA_HOME/machines/$VM/state" ]; do
		[ $SECONDS -lt $deadline ] || { echo "FAIL: the post-thaw reply did not park"; exit 1; }
		sleep 1
	done
	ID_P2=$("$BIN" gateway "$VM" show | grep "$HOST_IP" | awk "{print \$1}" | tail -1)
	"$BIN" gateway "$VM" release "$ID_P2" >/dev/null
	"$BIN" thaw "$VM" >/dev/null
	sleep 2
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

	# The host answers for reachability: a guest probe cannot run
	# under a born-closed valve.
	curl -s --max-time 8 -o /dev/null http://huggingface.co || {
		echo "SKIP: this host has no route to the www"; exit 0; }

	# The serial RX FIFO holds 64 bytes; every typed line stays short.
	say "step 2: the valve opens -- the membrane, never a free flow"
	"$BIN" gateway "$VM" open >/dev/null
	sleep 1
	type_in 'mkdir -p /etc; echo nameserver 1.1.1.1 >/etc/resolv.conf'
	type_in 'U=http://huggingface.co'

	say "step 3: the fetch -- the park is the freeze"
	type_in 'wget -q -O /dev/null $U && echo held-o"k" &'
	STATE="$CELLA_HOME/machines/$VM/state"
	deadline=$((SECONDS + 20))
	until [ -f "$STATE" ]; do
		[ $SECONDS -lt $deadline ] || { echo "FAIL: the machine did not freeze itself on the park"; exit 1; }
		sleep 1
	done
	grep -aq "held-ok" "$CON" && { echo "FAIL: the request left the machine while held"; exit 1; }
	grep -aq "held egress frame" "$CELLA_HOME/machines/$VM/vmm.log" \
		|| { echo "FAIL: the freeze holds no egress frame"; exit 1; }
	echo "  parked; the machine froze itself (one-shot); frames in the sidecar"

	say "step 4: the engine loop -- decide while it sleeps, thaw, repeat"
	# The fetch resolves a name first: the DNS operation parks and
	# freezes one cycle, its release lets the TCP operation park and
	# freeze the next. The loop is the stand-in engine.
	LEDGER="$CELLA_HOME/machines/$VM/network/ledger"
	VERDICT="$CELLA_HOME/machines/$VM/verdict"
	open_ids() {
		local dump; dump=$("$BIN" --dump-ledger "$LEDGER" 2>/dev/null) || return 0
		comm -23 \
			<(echo "$dump" | sed -n "s/^parked id=\([0-9a-f]*\) .*/\1/p" | sort) \
			<(echo "$dump" | sed -n "s/^\(released\|lapsed\) id=\([0-9a-f]*\).*/\2/p" | sort)
	}
	cycles=0
	until grep -aq "held-ok" "$CON"; do
		cycles=$((cycles + 1))
		[ $cycles -le 12 ] || {
			echo "FAIL: the request did not complete within 12 engine cycles"
			echo "-- ledger:"; "$BIN" --dump-ledger "$LEDGER" | sed "s/^/   /"
			echo "-- console:"; tail -4 "$CON" | sed "s/^/   /"
			exit 1
		}
		deadline=$((SECONDS + 20))
		until [ -f "$STATE" ] || grep -aq "held-ok" "$CON"; do
			[ $SECONDS -lt $deadline ] || { echo "FAIL: neither a freeze nor a completion arrived (cycle $cycles)"; exit 1; }
			sleep 1
		done
		grep -aq "held-ok" "$CON" && break
		for id in $(open_ids); do
			"$BIN" gateway "$VM" release "$id" >/dev/null
		done
		"$BIN" thaw "$VM" >/dev/null
		sleep 2
	done
	echo "  the ratchet cycled $cycles time(s): park, freeze, decide, thaw -- and the request landed"

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
	trap 'stop_srv; "$BIN" stop "$VM" >/dev/null 2>&1 || true
    [ -n "${VMM_PID:-}" ] && kill -9 "$VMM_PID" 2>/dev/null || true; rm -rf "$CELLA_HOME"' EXIT
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

	curl -s -o /dev/null "http://$HOST_IP:8080/" || { echo "FAIL: the stand-in endpoint died after the pre-check"; exit 1; }

	say "step 3: the valve closes; the same request parks, reports, and freezes"
	"$BIN" gateway "$VM" close >/dev/null
	sleep 1
	type_in 'wget -q -O /dev/null $H:8080 && echo rel-o"k" &'
	STATE="$CELLA_HOME/machines/$VM/state"
	deadline=$((SECONDS + 20))
	until [ -f "$STATE" ]; do
		[ $SECONDS -lt $deadline ] || { echo "FAIL: the machine did not freeze itself on the park"; exit 1; }
		sleep 1
	done
	grep -aq "parked egress to $HOST_IP:8080" "$VMM" || { echo "FAIL: no park report for :8080"; exit 1; }
	grep -aq "rel-ok" "$CON" && { echo "FAIL: the request passed without a verdict"; exit 1; }
	echo "  parked, reported; the machine froze itself (one-shot)"

	say "step 4: the engine renders release with allow, by id; the thaw applies"
	LEDGER="$CELLA_HOME/machines/$VM/network/ledger"
	VERDICT="$CELLA_HOME/machines/$VM/verdict"
	ID_REL=$("$BIN" gateway "$VM" show | grep "$HOST_IP:8080" | awk "{print \$1}")
	[ -n "$ID_REL" ] || { echo "FAIL: show lists no held operation for :8080"; exit 1; }
	"$BIN" gateway "$VM" release "$ID_REL" >/dev/null
	"$BIN" thaw "$VM" >/dev/null
	wait_for "rel-ok" || {
		echo "FAIL: the released request did not complete"
		echo "-- vmm.log:"; tail -6 "$VMM" | sed "s/^/   /"
		echo "-- console:"; tail -4 "$CON" | sed "s/^/   /"
		echo "-- listeners:"; ss -ltn | grep -E "8080|9090" | sed "s/^/   /" || true
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

	say "step 6: the machine froze itself on the park; the world grows"
	deadline=$((SECONDS + 20))
	until [ -f "$STATE" ]; do
		[ $SECONDS -lt $deadline ] || { echo "FAIL: the machine did not freeze itself on the park"; exit 1; }
		sleep 1
	done
	grep -aq "held egress frame" "$VMM" || { echo "FAIL: the freeze holds no egress frame"; exit 1; }
	python3 -m http.server 9090 --bind "$HOST_IP" --directory "$WWW" >/dev/null 2>&1 & SRV2=$!
	sleep 1
	echo "  the endpoint at :9090 now exists"

	say "step 7: the engine decides by id; the thaw lands the same request"
	ID_W=$("$BIN" gateway "$VM" show | grep "$HOST_IP:9090" | awk "{print \$1}")
	[ -n "$ID_W" ] || { echo "FAIL: show lists no held operation for :9090"; exit 1; }
	"$BIN" gateway "$VM" release "$ID_W" >/dev/null
	"$BIN" thaw "$VM" >/dev/null
	wait_for "world-ok" || { echo "FAIL: the parked request did not land after the thaw"; exit 1; }
	echo "  the request completed against the new endpoint; the guest saw no failure"

	echo
	echo "PASS: AC4 -- the verdict is external"
	"$BIN" stop "$VM" >/dev/null; "$BIN" destroy "$VM" >/dev/null
	;;
esac
