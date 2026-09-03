#!/bin/sh
# /sbin/init for the gateway image: a small appliance between an
# agent and the world. eth0 is the world side, configured by the ip=
# kernel parameter (a pool tap). eth1 is the agent side: the pair id
# arrives as cella_pair=<n> on the command line, and the convention
# gives the gateway 10.77.<n>.1/24. Forwarding is plain routing --
# the canonical kernel carries no netfilter, and the host NAT
# masquerades the agent subnet on its way out.
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t tmpfs tmpfs /tmp
echo "cella-gateway: init running (pid $$)"

PAIR=$(sed -n 's/.*cella_pair=\([0-9]*\).*/\1/p' /proc/cmdline)
# A wire carries no convention on the command line (1.6.14e): an
# eth1 with no cella_pair is the agent side of wire 0.
[ -z "$PAIR" ] && [ -e /sys/class/net/eth1 ] && PAIR=0
if [ -n "$PAIR" ]; then
    ip addr add "10.77.$PAIR.1/24" dev eth1
    ip link set eth1 up
    echo 1 > /proc/sys/net/ipv4/ip_forward
    echo "cella-gateway: agent side 10.77.$PAIR.1/24, forwarding on"
else
    echo "cella-gateway: no cella_pair on the command line, forwarding stays off"
fi

# A quiet heartbeat, and a shell for diagnosis.
(
    while true; do
        echo "cella-gateway: alive $(cat /proc/uptime | cut -d' ' -f1)"
        sleep 30
    done
) &
N=0
while true; do
    N=$((N+1))
    echo "cella-shell: getty generation $N starting"
    /bin/getty -n -l /bin/sh 115200 ttyS0
    echo "cella-shell: getty generation $N exited with $?"
done
