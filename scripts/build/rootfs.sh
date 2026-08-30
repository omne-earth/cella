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
    # Two clocks, not one. `date` gives CLOCK_REALTIME at a resolution of
    # 1 second. /proc/uptime gives the monotonic clock at a resolution of
    # 10 ms. The freeze must not advance either one, and the second value
    # is what shows this at a resolution better than the 1 second tick of
    # this loop.
    # /proc/timer_list holds CLOCK_MONOTONIC in nanoseconds, as
    # "now at <N> nsecs". It also holds the base of CLOCK_REALTIME: the
    # ".offset" line of the clock base with ".index: 1". The sum
    # of the two values is CLOCK_REALTIME in nanoseconds. This busybox
    # `date` does not support %N, thus /proc/timer_list is the only
    # nanosecond source in this guest. awk uses doubles, thus real_ns
    # has a granularity of ~200 ns at the current epoch. The 1 second
    # period of this loop and the start of two programs each cycle
    # remain the limit of any measurement across one interval.
    echo "cella-rootfs: wall-clock $(date +%s) uptime $(cut -d' ' -f1 /proc/uptime) mono_ns $(awk '/now at/ { print $3; exit }' /proc/timer_list 2>/dev/null) real_ns $(awk '/now at/ { now = $3 } /\.index:/ { idx = $2 } /\.offset:/ { if (idx == 1) off = $2 } END { printf "%.0f", now + off }' /proc/timer_list 2>/dev/null)"
    sleep 1
done
