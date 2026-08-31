#!/bin/sh
# /sbin/init for dist/rootfs-cella.ext4, the interactive cella image
# (the -cella suffix tracks the latest cella mvp image). The heartbeat
# of the canonical image runs in the background, and the console gets
# a shell. `make enter` attaches to that shell; a freeze and a thaw
# resume it exactly where it stopped.
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t tmpfs tmpfs /tmp
echo "cella-rootfs: init running (pid $$)"
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
N=0
while true; do
    N=$((N+1))
    echo "cella-shell: getty generation $N starting"
    /bin/getty -n -l /bin/sh 115200 ttyS0
    echo "cella-shell: getty generation $N exited with $?"
done
