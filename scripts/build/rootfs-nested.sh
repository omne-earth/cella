#!/bin/sh
# /sbin/init for the nested test rootfs (dist/rootfs-nested.ext4).
# This guest is a host: it starts the inner cella with the canonical
# assets from the golden layout of the guest (/root/.cella). The inner guest prints the standard heartbeat
# ("cella-rootfs: ..."), and this init prints "cella-nested: ..."
# lines only. The test script tells the two layers apart by that
# prefix.
#
# Modes, selected by the kernel command line of the outer guest:
#   (default)               the inner guest runs with the block device
#                           only. The airgapped and hybrid tests use
#                           this mode.
#   cella_nested_mode=www   the inner cella also gets a TAP. cella
#                           creates the interface when it opens
#                           /dev/net/tun, and this init then gives the
#                           interface an address. The init pings the
#                           inner guest and reports the result.
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t tmpfs tmpfs /tmp
echo "cella-nested: outer init running (pid $$)"
if [ ! -c /dev/kvm ]; then
    echo "cella-nested: FAIL: no /dev/kvm in the outer guest"
    while true; do sleep 1; done
fi
mkdir -p /tmp/state
BASE="$(/bin/cella --print-default-cmdline)"

if grep -q cella_nested_mode=www /proc/cmdline; then
    echo "cella-nested: starting the inner cella (www: block + net)"
    /bin/cella --state-dir /tmp/state \
        --kernel /root/.cella/kernel/canonical/bzImage --disk /root/.cella/rootfs/canonical/rootfs.ext4 --tap tap0 \
        --mem-mb 64 --cmdline "$BASE root=/dev/vda rw \
virtio_mmio.device=4K@0xd0000000:5 virtio_mmio.device=4K@0xd0001000:6 \
ip=192.168.201.2::192.168.201.1:255.255.255.0::eth0:off" &
    INNER=$!
    # The interface exists after the inner cella opens /dev/net/tun.
    n=0
    while [ $n -lt 30 ]; do
        ip link show tap0 >/dev/null 2>&1 && break
        n=$((n+1)); sleep 1
    done
    ip addr add 192.168.201.1/24 dev tap0
    ip link set tap0 up
    echo 1 > /proc/sys/net/ipv4/ip_forward
    n=0
    while [ $n -lt 60 ]; do
        if ping -c 1 -W 1 192.168.201.2 >/dev/null 2>&1; then
            echo "cella-nested: inner answered ICMP"
            break
        fi
        n=$((n+1)); sleep 1
    done
    [ $n -ge 60 ] && echo "cella-nested: FAIL: no ICMP reply from the inner guest"
    wait $INNER
else
    # The inner guest gets the block device only: no TAP in this mode.
    echo "cella-nested: starting the inner cella (block only)"
    /bin/cella --state-dir /tmp/state \
        --kernel /root/.cella/kernel/canonical/bzImage --disk /root/.cella/rootfs/canonical/rootfs.ext4 \
        --mem-mb 64 --cmdline "$BASE root=/dev/vda rw virtio_mmio.device=4K@0xd0000000:5"
fi
echo "cella-nested: inner cella exited with code $?"
# Keep the guest alive, so that the serial output stays readable.
while true; do sleep 1; done
