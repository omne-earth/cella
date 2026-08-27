#!/bin/sh
# /sbin/init for the test rootfs -- installed as such by
# scripts/build-assets.sh. There's no login, no shell session, no
# services: cella's virtio-mmio/blk/net + 8250 serial device set needs
# nothing from userspace to prove itself works. Networking in
# particular comes entirely from the kernel's own CONFIG_IP_PNP static
# configuration (see scripts/kernel-fragment.config) driven by the
# ip= cmdline parameter -- ICMP replies are handled in-kernel, so this
# script never touches the network itself.
mount -t proc proc /proc
mount -t sysfs sysfs /sys
echo "cella-rootfs: init running (pid $$)"
while true; do
    sleep 3600
done
