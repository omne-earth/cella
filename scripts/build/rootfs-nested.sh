#!/bin/sh
# /sbin/init for the nested test rootfs (dist era: /opt; now the
# golden layout). This guest is a host, and it hosts the way the host
# does: through the verbs, jailed. cella and bwrap are static
# binaries on the path, the inner goldens live at /root/.cella, and
# the inner machine runs inside its own bwrap jail with its own
# seccomp filter, one level down. The init prints "cella-nested:"
# lines only; a "cella-rootfs:" line can come from the inner guest
# alone, relayed from its console log.
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t tmpfs tmpfs /tmp
echo "cella-nested: outer init running (pid $$)"
if [ ! -c /dev/kvm ]; then
    echo "cella-nested: FAIL: no /dev/kvm in the outer guest"
    while true; do sleep 1; done
fi
# The machine home lives on the tmpfs: the RAM file of the inner
# machine is larger than the root disk of this guest. The goldens
# stay on the disk, reachable through symlinks.
mkdir -p /tmp/cella
ln -s /root/.cella/kernel /tmp/cella/kernel
ln -s /root/.cella/rootfs /tmp/cella/rootfs
export CELLA_HOME=/tmp/cella

if grep -q cella_nested_mode=www /proc/cmdline || [ -f /etc/cella-www ]; then
    # tap1, not tap0: the pool convention gives tap1 the 192.168.201
    # subnet, and the outer guest's own eth0 already uses 192.168.200.
    # tap1 must exist before the jailed start: the inner VMM runs in a
    # user namespace with no CAP_NET_ADMIN, and it can only open a
    # persistent tap. setup net creates and addresses it (this init is
    # root in the guest, thus no sudo).
    cella setup net --taps 1 --from 1 || echo "cella-nested: FAIL: setup net failed"
    echo 1 > /proc/sys/net/ipv4/ip_forward
    echo "cella-nested: creating the inner machine (www: block + net)"
    cella create inner --kernel canonical --rootfs canonical \
        --mem-mb 64 --net tap1 || echo "cella-nested: FAIL: create failed"
    echo "cella-nested: starting the inner machine (jailed)"
    cella start inner || echo "cella-nested: FAIL: start failed"
    # Relay the inner console now, in the background: the ear's ratchet
    # below can run many cycles against a nested VM, and the observer
    # must see the inner guest's own boot line without waiting on the
    # ICMP engine to finish first.
    tail -f /tmp/cella/machines/inner/console.log &
    # Born closed: this init is the inner machine's engine. Open the
    # valve, ping (the inner reply parks and the inner machine
    # freezes itself), release every held operation, thaw, and ping
    # again -- one decision at a time, one level down. The ping runs
    # in the background with its own patience: a nested freeze/thaw
    # round trip can run longer than any single ICMP timeout, and a
    # ping that gives up before its own reply lands never sees it --
    # the next ping is then one cycle behind forever. The pump below
    # decides without regard to any one attempt.
    cella gateway inner open
    ping -c 20 -i 1 -W 10 192.168.201.2 >/dev/null 2>&1 &
    PPID_ICMP=$!
    n=0
    while kill -0 "$PPID_ICMP" 2>/dev/null; do
        n=$((n+1))
        if [ $n -gt 60 ]; then
            kill "$PPID_ICMP" 2>/dev/null
            break
        fi
        # The ear's live wire: the ping is a knock at the inner
        # machine's own inbound lane -- it parks and does not
        # freeze the inner machine. Release it live while inner
        # runs, and its own reply parks egress and freezes.
        for id in $(cella gateway inner show incoming | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
            cella gateway inner release "$id"
        done
        ipid=$(cat /tmp/cella/machines/inner/pid 2>/dev/null || true)
        if [ -n "$ipid" ] && kill -0 "$ipid" 2>/dev/null; then
            : # the sidecar lands before the old VMM exits; try again next pass
        elif [ -f /tmp/cella/machines/inner/state ]; then
            echo "cella-nested: the inner machine froze on its reply -- deciding"
            for id in $(cella gateway inner show | sed -n 's/^\([0-9a-f]\{32\}\) .*held$/\1/p'); do
                cella gateway inner release "$id"
            done
            cella thaw inner
        fi
        sleep 1
    done
    if wait "$PPID_ICMP"; then
        echo "cella-nested: inner answered ICMP"
    else
        echo "cella-nested: FAIL: no ICMP reply from the inner guest"
    fi
else
    echo "cella-nested: creating the inner machine (block only)"
    cella create inner --kernel canonical --rootfs canonical \
        --mem-mb 64 --net none || echo "cella-nested: FAIL: create failed"
    echo "cella-nested: starting the inner machine (jailed)"
    cella start inner || echo "cella-nested: FAIL: start failed"
    # Relay the inner console, for the outer observer.
    tail -f /tmp/cella/machines/inner/console.log &
fi
# The relay above runs in the background; keep this init alive as
# pid 1 for as long as the outer guest lives.
while true; do sleep 1; done
