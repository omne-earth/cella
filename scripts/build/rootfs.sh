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
# life: the serial console is the only channel out of the guest, and
# the heartbeat is the only way the probes can observe the clocks of
# the guest, including across a freeze and a thaw.
while true; do
    # One awk pass produces the whole heartbeat line. Each extra
    # process start adds jitter to the interval between heartbeats,
    # and that jitter is the resolution limit of the freeze and thaw
    # measurement (the probes gate on the spread of these intervals).
    # /proc/timer_list holds CLOCK_MONOTONIC as "now at <N> nsecs" and
    # the base of CLOCK_REALTIME as the .offset line of the clock base
    # with .index 1. /proc/uptime holds the monotonic clock at 10 ms
    # resolution. The epoch field is real_ns/1e9: this busybox date
    # does not support %N, and one awk replaces it. awk uses doubles,
    # thus real_ns has a granularity of ~200 ns at the current epoch.
    awk '
        /now at/ { if (!now) now = $3 }
        /\.index:/ { idx = $2 }
        /\.offset:/ { if (idx == 1) off = $2 }
        FILENAME == "/proc/uptime" { up = $1 }
        END {
            printf "cella-rootfs: wall-clock %d uptime %s mono_ns %.0f real_ns %.0f\n",
                (now + off) / 1e9, up, now, now + off
        }
    ' /proc/timer_list /proc/uptime
    sleep 1
done
