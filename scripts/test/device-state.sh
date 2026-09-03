#!/usr/bin/env bash
# Acceptance gates for docs/DEVICE-STATE.md. One argument selects the
# criterion: ac1, ac2, ac3, or ac4. A gate that is not implemented yet
# fails; the FAIL is the specification.
set -ueo pipefail

cd "$(dirname "$0")/../.."
BIN=target/smoke/cella
# The knock port: random per run, so a leaked translator from an
# earlier gate (a stale bind on a fixed port swallows knocks
# silently) can never poison this one. Four digits, unprivileged.
WORLD_PORT=$(( (RANDOM % 8976) + 1024 ))
ac="${1:-}"

case "$ac" in
ac1 | ac2 | ac3 | ac4 | ac5) ;;
*)
	echo "usage: $0 ac1|ac2|ac3|ac4|ac5" >&2
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
    if [ -n "${CELLA_KEEP_SANDBOX:-}" ]; then echo "kept: $CELLA_HOME"; else rm -rf "$CELLA_HOME"; fi
}
trap teardown EXIT
say() { echo; echo "==> $1"; }
knock() { printf 'knock\n' > /dev/udp/127.0.0.1/$WORLD_PORT 2>/dev/null || true; }
knock_loop() { local end=$((SECONDS + $1)); while [ $SECONDS -lt $end ]; do knock; sleep 1; done; }

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
STATE="$CELLA_HOME/machines/$VM/state"
held_ids() { "$BIN" gateway "$VM" show | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'; }
# The stand-in engine: while the given pid runs, release every held
# operation of the frozen machine and thaw it -- every frame is a
# fresh decision under the total membrane.
pump_mail() {
    for id in $("$BIN" gateway "$VM" show incoming | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
        "$BIN" gateway "$VM" release "$id" >/dev/null
    done
}
pump_while() { # pid
    local cycles=0
    while kill -0 "$1" 2>/dev/null; do
        pump_mail
        if [ -f "$STATE" ]; then
            local vp
            vp=$(cat "$CELLA_HOME/machines/$VM/pid" 2>/dev/null || true)
            [ -n "$vp" ] && kill -0 "$vp" 2>/dev/null && { sleep 0.2; continue; }
            for id in $(held_ids); do
                "$BIN" gateway "$VM" release "$id" >/dev/null
            done
            "$BIN" thaw "$VM" >/dev/null
            cycles=$((cycles + 1))
        fi
        sleep 0.2
    done
    echo "  ($cycles engine cycles)"
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
	HOST_IP=$(ip -4 route get 1.1.1.1 2>/dev/null | grep -oP 'src \K[0-9.]+' | head -1); [ -n "$HOST_IP" ] || HOST_IP=127.0.0.1

	say "step 1: create and start a machine on --net world"
	"$BIN" create "$VM" --net world:$WORLD_PORT/tcp+$WORLD_PORT/udp >/dev/null
	"$BIN" start "$VM" >/dev/null
	sleep 6

	say "step 2: open -- every egress parks, and the engine lands a reply"
	"$BIN" gateway "$VM" open >/dev/null
	sleep 1
	knock_loop 25 & P2=$!
	pump_while "$P2"
	wait "$P2" || { echo "FAIL: no reply landed while the engine decided"; exit 1; }
	"$BIN" --dump-ledger "$CELLA_HOME/machines/$VM/network/ledger" | grep -q "released id=.*bytes_out=[1-9]" || { echo "FAIL: no reply crossed while the engine decided"; exit 1; }
	echo "  the knock was answered through the membrane, every frame decided"

	say "step 3: freeze, then thaw (the claim rides the manifest)"
	if [ -f "$STATE" ]; then "$BIN" thaw "$VM" >/dev/null; sleep 1; fi
	"$BIN" freeze "$VM" >/dev/null
	grep -q "world" "$CELLA_HOME/machines/$VM/manifest.json" || { echo "FAIL: the manifest lost its nic"; exit 1; }
	"$BIN" thaw "$VM" >/dev/null
	sleep 2

	say "step 4: the host pings again through the thawed transport, re-decided"
	knock_loop 25 & P4=$!
	pump_while "$P4"
	wait "$P4" || { echo "FAIL: no reply landed after the thaw (see docs/DEVICE-STATE.md)"; exit 1; }
	echo "  the knock is answered again after the thaw"

	echo
	echo "PASS: AC2 -- the network survived the thaw"
	"$BIN" stop "$VM" >/dev/null 2>&1 || true
	"$BIN" destroy "$VM" >/dev/null
	;;
ac3)
	echo "AC3: the in-flight layer is exact (a parked egress request is delivered"
	echo "and completed after the thaw; the same request works, no retransmission)."
	HOST_IP=$(ip -4 route get 1.1.1.1 2>/dev/null | grep -oP 'src \K[0-9.]+' | head -1); [ -n "$HOST_IP" ] || HOST_IP=127.0.0.1

	command -v python3 >/dev/null || { echo "SKIP: python3 not found (the stand-in endpoint)"; exit 0; }
	pkill -f "http.server 8080 --bind $HOST_IP" 2>/dev/null || true
	WWW=$(mktemp -d); echo world > "$WWW/index.html"
	SRV1=""
	trap 'kill $SRV1 2>/dev/null; rm -rf "$WWW"; "$BIN" stop "$VM" >/dev/null 2>&1 || true
    [ -n "${VMM_PID:-}" ] && kill -9 "$VMM_PID" 2>/dev/null || true; rm -rf "$CELLA_HOME"' EXIT

	say "step 1: create and start a machine on --net world"
	"$BIN" create "$VM" --net world:$WORLD_PORT/tcp+$WORLD_PORT/udp >/dev/null
	"$BIN" start "$VM" >/dev/null
	sleep 6
	VMM_PID=$(cat "$CELLA_HOME/machines/$VM/pid")

	# The serial RX FIFO holds 64 bytes; every typed line stays short.
	# The endpoint is the host's stand-in: a freeze loses in-flight
	# server segments (no ingress hold until the appliance), and a
	# remote TLS burst depends on the sender's retransmission
	# patience -- the single-segment stand-in keeps the exactness
	# claim deterministic.
	say "step 2: the valve opens -- the membrane, never a free flow"
	python3 -m http.server 8080 --bind "$HOST_IP" --directory "$WWW" >/dev/null 2>&1 & SRV1=$!
	sleep 1
	"$BIN" gateway "$VM" open >/dev/null
	sleep 1
	type_in "U=http://$HOST_IP:8080"

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
	# Under the total membrane every frame is a fresh decision: the
	# ARP, the DNS, and each TCP batch park and freeze in turn. The
	# loop is the stand-in engine, and the budget bounds the churn
	# (let it churn).
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
		[ $cycles -le 120 ] || {
			echo "FAIL: the request did not complete within 120 engine cycles"
			echo "-- ledger:"; "$BIN" --dump-ledger "$LEDGER" | sed "s/^/   /"
			echo "-- console:"; tail -4 "$CON" | sed "s/^/   /"
			exit 1
		}
		deadline=$((SECONDS + 20))
		until [ -f "$STATE" ] || grep -aq "held-ok" "$CON"; do
			[ $SECONDS -lt $deadline ] || { echo "FAIL: neither a freeze nor a completion arrived (cycle $cycles)"; exit 1; }
			# The HTTP response's mail (the ARP reply, the SYN-ACK,
			# the response bytes) arrives here: pump it every half
			# second, or the guest's own real time in this window
			# outruns the pump and it retries instead of landing.
			for id in $("$BIN" gateway "$VM" show incoming | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
				"$BIN" gateway "$VM" release "$id" >/dev/null
			done
			sleep 0.5
		done
		grep -aq "held-ok" "$CON" && break
		# The sidecar lands before the old VMM exits: wait it out.
		vp=$(cat "$CELLA_HOME/machines/$VM/pid" 2>/dev/null || true)
		while [ -n "$vp" ] && kill -0 "$vp" 2>/dev/null; do sleep 0.2; done
		for id in $(open_ids); do
			"$BIN" gateway "$VM" release "$id" >/dev/null
		done
		"$BIN" thaw "$VM" >/dev/null
		sleep 2
	done
	echo "  the ratchet cycled $cycles time(s): park, freeze, decide, thaw -- and the request landed"

	echo
	echo "PASS: AC3 -- the in-flight layer is exact"
	"$BIN" stop "$VM" >/dev/null 2>&1 || true
	"$BIN" destroy "$VM" >/dev/null
	;;
ac4)
	echo "AC4: the verdict is external (the world-ratchet gate). Every egress"
	echo "frame parks; the test, as the stand-in engine, renders the verdicts."
	HOST_IP=$(ip -4 route get 1.1.1.1 2>/dev/null | grep -oP 'src \K[0-9.]+' | head -1); [ -n "$HOST_IP" ] || HOST_IP=127.0.0.1
	command -v python3 >/dev/null || { echo "SKIP: python3 not found (the stand-in endpoints)"; exit 0; }
	# A stand-in endpoint leaked by an interrupted run squats its port
	# and serves a deleted directory; sweep them first.
	pkill -f "http.server 9090 --bind $HOST_IP" 2>/dev/null || true
	WWW=$(mktemp -d); echo world > "$WWW/index.html"
	SRV1=""; SRV2=""
	stop_srv() { kill $SRV1 $SRV2 2>/dev/null; rm -rf "$WWW"; }
	trap 'stop_srv; "$BIN" stop "$VM" >/dev/null 2>&1 || true
    [ -n "${VMM_PID:-}" ] && kill -9 "$VMM_PID" 2>/dev/null || true; rm -rf "$CELLA_HOME"' EXIT
	VMM="$CELLA_HOME/machines/$VM/vmm.log"

	say "step 1: create and start a machine on --net world"
	"$BIN" create "$VM" --net world:$WORLD_PORT/tcp+$WORLD_PORT/udp >/dev/null
	"$BIN" start "$VM" >/dev/null
	sleep 6
	VMM_PID=$(cat "$CELLA_HOME/machines/$VM/pid")

	say "step 2: the request toward a world that does not exist -- it parks, and reports"
	"$BIN" gateway "$VM" open >/dev/null
	sleep 1
	type_in "H=http://$HOST_IP"
	type_in 'wget -q -T 90 -O /dev/null $H:9090 && echo world-o"k" &'
	# The engine releases everything except the :9090 operation --
	# the ARP resolves, and the SYN toward the missing endpoint
	# parks and stays held. Nothing may reach a world that does not
	# exist.
	LEDGER="$CELLA_HOME/machines/$VM/network/ledger"
	cycles=0
	until "$BIN" gateway "$VM" show | grep -q ":9090.*held"; do
		cycles=$((cycles + 1))
		[ $cycles -le 8 ] || { echo "FAIL: the :9090 operation never parked"; exit 1; }
		if [ -f "$STATE" ]; then
			vp=$(cat "$CELLA_HOME/machines/$VM/pid" 2>/dev/null || true)
			[ -n "$vp" ] && kill -0 "$vp" 2>/dev/null && { sleep 0.5; continue; }
			for id in $("$BIN" gateway "$VM" show | grep -v ":9090" | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
				"$BIN" gateway "$VM" release "$id" >/dev/null
			done
			if [ -n "$("$BIN" gateway "$VM" show | grep ":9090.*held")" ]; then break; fi
			"$BIN" thaw "$VM" >/dev/null
		fi
		sleep 1
	done
	grep -aq "parked egress to $HOST_IP:9090" "$VMM" || { echo "FAIL: no park report for :9090"; exit 1; }
	grep -aq "world-ok" "$CON" && { echo "FAIL: the request passed without a verdict"; exit 1; }
	[ -f "$STATE" ] || { echo "FAIL: the machine did not freeze itself on the park"; exit 1; }
	grep -aq "held egress frame" "$VMM" || { echo "FAIL: the freeze holds no egress frame"; exit 1; }
	echo "  parked, reported, frozen; the frames ride the sidecar"

	say "step 3: the world grows while the machine sleeps"
	python3 -m http.server 9090 --bind "$HOST_IP" --directory "$WWW" >/dev/null 2>&1 & SRV2=$!
	sleep 1
	echo "  the endpoint at :9090 now exists"

	say "step 4: the engine decides by id; the ratchet lands the same request"
	ID_W=$("$BIN" gateway "$VM" show | grep ":9090" | awk "{print \$1}" | head -1)
	[ -n "$ID_W" ] || { echo "FAIL: show lists no held operation for :9090"; exit 1; }
	"$BIN" gateway "$VM" release "$ID_W" >/dev/null
	"$BIN" thaw "$VM" >/dev/null
	# Every further frame of the flow is its own decision: the pump
	# carries the request to completion.
	( until grep -aq "world-ok" "$CON"; do sleep 1; done ) & WAITER=$!
	( sleep 90; kill $WAITER 2>/dev/null ) & KILLER=$!
	pump_while "$WAITER"
	kill "$KILLER" 2>/dev/null || true
	grep -aq "world-ok" "$CON" || { echo "FAIL: the parked request did not land after the thaw"; exit 1; }
	echo "  the request completed against the endpoint that now exists; the guest saw no failure"

	echo
	echo "PASS: AC4 -- the verdict is external"
	"$BIN" stop "$VM" >/dev/null 2>&1 || true
	"$BIN" destroy "$VM" >/dev/null
	;;
ac5)
	echo "AC5: the true world. AC3 proves exactness against the stand-in;"
	echo "this leg walks a real internet fetch through the total membrane,"
	echo "one decision per frame. It skips when the host is offline, and it"
	echo "rides the peer-patience bound: the far end must retransmit into"
	echo "the aperture."
	# A real internet endpoint that answers small, over plain HTTP:
	# a TLS burst spans many segments, and a segment that lands on
	# a frozen is lost -- the peer-patience bound. The claim
	# here is the true world, not the biggest possible page.
	AC5_URL="${CELLA_AC5_URL:-http://example.com}"
	curl -s --max-time 8 -o /dev/null "$AC5_URL" || {
		echo "SKIP: this host has no route to the www"; exit 0; }

	say "step 1: create and start a machine on --net world (no tap, no host address)"
	"$BIN" create "$VM" --net world >/dev/null
	"$BIN" start "$VM" >/dev/null
	sleep 6
	VMM_PID=$(cat "$CELLA_HOME/machines/$VM/pid")

	# The serial RX FIFO holds 64 bytes; every typed line stays short.
	say "step 2: the valve opens, and the fetch begins"
	"$BIN" gateway "$VM" open >/dev/null
	sleep 1
	type_in 'mkdir -p /etc; echo nameserver 1.1.1.1 >/etc/resolv.conf'
	type_in "U=$AC5_URL"
	type_in 'wget -q -O /dev/null $U && echo world-rea"l" &'

	say "step 3: the engine loop, against the true world"
	LEDGER="$CELLA_HOME/machines/$VM/network/ledger"
	open_ids() {
		local dump; dump=$("$BIN" --dump-ledger "$LEDGER" 2>/dev/null) || return 0
		comm -23 \
			<(echo "$dump" | sed -n "s/^parked id=\([0-9a-f]*\) .*/\1/p" | sort) \
			<(echo "$dump" | sed -n "s/^\(released\|lapsed\) id=\([0-9a-f]*\).*/\2/p" | sort)
	}
	STATE="$CELLA_HOME/machines/$VM/state"
	cycles=0
	until grep -aq "world-real" "$CON"; do
		cycles=$((cycles + 1))
		[ $cycles -le 150 ] || {
			echo "FAIL: the real fetch did not land within 150 engine cycles"
			echo "-- ledger tail:"; "$BIN" --dump-ledger "$LEDGER" | tail -6 | sed "s/^/   /"
			echo "-- console:"; tail -4 "$CON" | sed "s/^/   /"
			exit 1
		}
		deadline=$((SECONDS + 25))
		until [ -f "$STATE" ] || grep -aq "world-real" "$CON"; do
			[ $SECONDS -lt $deadline ] || { echo "FAIL: neither a freeze nor a completion arrived (cycle $cycles)"; exit 1; }
			# Same as AC3: pump the response's mail continuously, or
			# the real world's reply misses the window and the guest
			# retries instead of landing.
			for id in $("$BIN" gateway "$VM" show incoming | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
				"$BIN" gateway "$VM" release "$id" >/dev/null
			done
			sleep 0.5
		done
		grep -aq "world-real" "$CON" && break
		vp=$(cat "$CELLA_HOME/machines/$VM/pid" 2>/dev/null || true)
		while [ -n "$vp" ] && kill -0 "$vp" 2>/dev/null; do sleep 0.2; done
		for id in $(open_ids); do
			"$BIN" gateway "$VM" release "$id" >/dev/null
		done
		"$BIN" thaw "$VM" >/dev/null
		sleep 1
	done
	echo "  the ratchet cycled $cycles time(s), and the true world answered"

	echo
	echo "PASS: AC5 -- a real internet fetch crossed the total membrane"
	"$BIN" stop "$VM" >/dev/null 2>&1 || true
	"$BIN" destroy "$VM" >/dev/null
	;;
esac
