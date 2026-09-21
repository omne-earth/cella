#!/bin/sh
# /sbin/init for dist/rootfs-cella.ext4, the interactive cella image
# (the -cella suffix tracks the latest cella mvp image). The heartbeat
# of the canonical image runs in the background, and the console gets
# a shell. `make enter` attaches to that shell; a freeze and a thaw
# resume it exactly where it stopped.
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t tmpfs tmpfs /tmp
# The inspect verb attaches a machine's disk as /dev/vdb, read-only
# at the device. The mount adds the execution deny, and norecovery
# keeps a dirty journal (a frozen source) from any replay attempt:
# the view is the crash-consistent instant.
if [ -b /dev/vdb ]; then
    mkdir -p /rock
    mount -o ro,noexec,nosuid,nodev,norecovery /dev/vdb /rock \
        && echo "cella-rootfs: evidence mounted at /rock (ro, noexec)" \
        || echo "cella-rootfs: /dev/vdb present but the mount failed"
fi
# The extract verb attaches a blank scratch disk as /dev/vdc and
# names a guest path on the kernel command line. The job runs with
# no console and no shell: tar the evidence at the path onto the raw
# scratch (offset 512), write the trailer to sector 0 last, and
# halt. The canonical kernel has no power-off device, and a reset
# may boot the kernel again instead of ending the VMM -- thus the
# job halts, and the host, polling for the trailer, stops the
# appliance itself. The trailer names the byte length and the
# sha256; a
# missing or wrong trailer tells the host the job died -- a crash
# here can never pass off a truncated tar as evidence. The source
# is read-only, thus the three tar passes see the same bytes.
if [ -b /dev/vdc ] && grep -q 'cella_extract=' /proc/cmdline; then
    P=$(sed -n 's/.*cella_extract=\([^ ]*\).*/\1/p' /proc/cmdline)
    T='cella-extract-0 the job died mid-tar'
    if [ -n "$P" ] && [ -e "/rock$P" ]; then
        # GNU tar walks holes (SEEK_HOLE) so a sparse twin costs its
        # allocated bytes, not its apparent size; busybox tar is the
        # fallback and reads everything. Same command all three
        # passes: the sparse map of a read-only source is stable.
        if [ -x /bin/gtar ]; then evtar() { /bin/gtar --sparse -cf - -C /rock ".$P" 2>/dev/null; }
        else evtar() { tar -cf - -C /rock ".$P" 2>/dev/null; }; fi
        LEN=$(evtar | wc -c)
        SUM=$(evtar | sha256sum | cut -d' ' -f1)
        if [ "$LEN" -gt 0 ] \
            && evtar | dd of=/dev/vdc bs=512 seek=1 conv=notrunc 2>/dev/null; then
            T="cella-extract-1 $LEN $SUM"
        fi
    else
        T="cella-extract-0 no such path under /rock: $P"
    fi
    printf '%s\n' "$T" | dd of=/dev/vdc bs=512 count=1 conv=sync,notrunc 2>/dev/null
    sync
    poweroff -f
fi
echo "cella-rootfs: init running (pid $$)"
# The serial console is also the shell of the user. The heartbeat and
# the diagnostic listings therefore print only when the kernel command
# line carries cella_diag: the demo boots with it, and an interactive
# `make boot` stays quiet.
if grep -q cella_diag /proc/cmdline; then
(
    while true; do
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
) &
# A respawning shell on the serial line, through getty: getty makes
# ttyS0 the controlling terminal and sets the line discipline up, and
# -n -l /bin/sh gives a root shell with no login prompt. A plain
# /bin/sh on the console of PID 1 gets no controlling tty, and it is
# not interactive. The loop brings the shell back when it exits;
# poweroff still stops the guest through the kernel.
# A process listing every 10 s, for the diagnosis of a shell that
# stops reading. The state letter (S, R, T, Z) tells a sleeping reader
# apart from a stopped or a dead one, and wchan names the kernel
# function that a sleeping process waits in. The serial line of
# /proc/interrupts shows whether RX interrupts arrive at all.
(
    while true; do
        for p in /proc/[0-9]*; do
            [ -f "$p/stat" ] || continue
            read -r pid comm state _ < "$p/stat" 2>/dev/null || continue
            echo "cella-ps: $pid $comm $state wchan=$(cat "$p/wchan" 2>/dev/null)"
        done
        echo "cella-irq: $(grep -E '^ *4:' /proc/interrupts | tr -s ' ')"
        sleep 10
    done
) &
fi
N=0
while true; do
    N=$((N+1))
    echo "cella-shell: getty generation $N starting"
    /bin/getty -n -l /bin/bash 115200 ttyS0
    echo "cella-shell: getty generation $N exited with $?"
done
