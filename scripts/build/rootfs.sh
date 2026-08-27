#!/bin/sh
# /sbin/init for the test rootfs -- installed as such by
# scripts/build/assets.sh. There's no login, no shell session, no
# services: cella's virtio-mmio/blk/net + 8250 serial device set needs
# nothing from userspace to prove itself works. Networking in
# particular comes entirely from the kernel's own CONFIG_IP_PNP static
# configuration (see scripts/build/kernel-fragment.config) driven by the
# ip= cmdline parameter -- ICMP replies are handled in-kernel, so this
# script never touches the network itself.
mount -t proc proc /proc
mount -t sysfs sysfs /sys
echo "cella-rootfs: init running (pid $$)"
# A wall-clock heartbeat, once a second, for the rest of this process's
# life: there's no SSH, no shell session, so the serial console is the
# *only* channel out of the guest, and this is the only way anything on
# the host (see probes/wallclock, probes/freeze-thaw-clock) can observe
# what the guest's own clock thinks the time is -- including across a
# freeze/thaw, where /sbin/init itself never runs again (its whole
# state, mid-loop, is what gets restored).
while true; do
    echo "cella-rootfs: wall-clock $(date +%s)"
    sleep 1
done
