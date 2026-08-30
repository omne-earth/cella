#!/bin/sh
# /sbin/init for the nested test rootfs (dist/rootfs-nested.ext4).
# This guest is a host: it starts the inner cella with the canonical
# assets from /opt. The inner guest prints the standard heartbeat
# ("cella-rootfs: ..."), and this init prints "cella-nested: ..."
# lines only. The test script tells the two layers apart by that
# prefix.
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t tmpfs tmpfs /tmp
echo "cella-nested: outer init running (pid $$)"
if [ ! -c /dev/kvm ]; then
    echo "cella-nested: FAIL: no /dev/kvm in the outer guest"
    while true; do sleep 1; done
fi
mkdir -p /tmp/state
# The inner guest gets the block device only: no TAP exists in here.
# The command line therefore names one virtio_mmio device.
CMDLINE="$(/opt/cella --print-default-cmdline) root=/dev/vda rw virtio_mmio.device=4K@0xd0000000:5"
echo "cella-nested: starting the inner cella"
/opt/cella --state-dir /tmp/state \
    --kernel /opt/bzImage --disk /opt/rootfs.ext4 \
    --mem-mb 64 --cmdline "$CMDLINE"
echo "cella-nested: inner cella exited with code $?"
# Keep the guest alive, so that the serial output stays readable.
while true; do sleep 1; done
