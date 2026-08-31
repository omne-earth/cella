#!/bin/sh
# /sbin/init for the inception test rootfs (dist/rootfs-inception.ext4).
# This guest runs the freeze and thaw clock probe against an inner
# cella. The probe boots, freezes, and thaws an inner guest, and it
# prints its verdict on this console. The init prints
# "cella-inception:" lines only, thus the probe and the two guest
# layers stay tellable apart.
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t tmpfs tmpfs /tmp
echo "cella-inception: init running (pid $$)"
if [ ! -c /dev/kvm ]; then
    echo "cella-inception: FAIL: no /dev/kvm in the outer guest"
    while true; do sleep 1; done
fi
export CELLA_BIN=/bin/cella
export CELLA_TEST_KERNEL=/root/.cella/kernel/canonical/bzImage
export CELLA_TEST_DISK=/root/.cella/rootfs/canonical/rootfs.ext4
export CELLA_TEST_TAP=none
export CELLA_POST_THAW_SECS=0
export TMPDIR=/tmp
echo "cella-inception: starting the probe"
/bin/freeze-thaw-clock-probe
echo "cella-inception: probe exited with code $?"
# Keep the guest alive, so that the serial output stays readable.
while true; do sleep 1; done
