#!/bin/sh
# /sbin/init for the terminator image (docs/NETWORK-MODEL.md, "The
# terminator"): the one network appliance. eth0 is the world side,
# configured by the ip= kernel parameter; eth1 is the member side:
# cella_pair=<n> gives the terminator 10.77.<n>.1/24, the gateway
# convention. No forwarding exists and none is wanted: the resolver
# answers every member query with this address, the proxy
# terminates and splices, and the canonical kernel stays quiet.
#
# The knobs, all on the kernel command line, all optional:
#   cella_pair=<n>          the member wire (default 0 when eth1 exists)
#   cella_dns=<ip>          the upstream provider (default 9.9.9.9)
#   cella_listen=<p,p,...>  the named ports (default 443,80)
#   cella_map=<lp:host:rp+lp:host:rp+...>  static maps for nameless
#                           flows ('+'-separated; ':' is taken)
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t tmpfs tmpfs /tmp
echo "cella-terminator: init running (pid $$)"

PAIR=$(sed -n 's/.*cella_pair=\([0-9]*\).*/\1/p' /proc/cmdline)
[ -z "$PAIR" ] && [ -e /sys/class/net/eth1 ] && PAIR=0
WIRE_IP="10.77.${PAIR:-0}.1"
if [ -e /sys/class/net/eth1 ]; then
    ip addr add "$WIRE_IP/24" dev eth1
    ip link set eth1 up
    echo "cella-terminator: member side $WIRE_IP/24"
else
    echo "cella-terminator: no member wire (eth1 absent)"
fi

DNS=$(sed -n "s/.*cella_dns=\([0-9.:]*\).*/\1/p" /proc/cmdline)
LISTEN=$(sed -n 's/.*cella_listen=\([0-9,]*\).*/\1/p' /proc/cmdline)
MAPS=$(sed -n 's/.*cella_map=\([^ ]*\).*/\1/p' /proc/cmdline)
{
    echo "wire_ip=$WIRE_IP"
    echo "upstream_dns=${DNS:-9.9.9.9}"
    echo "listen=${LISTEN:-443,80}"
    if [ -n "$MAPS" ]; then
        echo "$MAPS" | tr '+' '\n' | while read -r m; do
            [ -n "$m" ] && echo "map=$m"
        done
    fi
} > /etc/cella-terminator.conf
echo "cella-terminator: configured (dns ${DNS:-9.9.9.9}, listen ${LISTEN:-443,80})"

# The one service, under the house respawn loop, in the
# background. The pair CA sits at /etc/cella/pair-ca.{pem,key},
# baked at image build; the key never leaves this image.
(
    N=0
    while true; do
        N=$((N+1))
        echo "cella-terminator: generation $N starting"
        /bin/cella-terminator /etc/cella-terminator.conf
        echo "cella-terminator: generation $N exited with $?"
        sleep 1
    done
) &
# The console shell, the house pattern: the lab drives the gates
# through it, and the field discards the bytes.
N=0
while true; do
    N=$((N+1))
    echo "cella-shell: getty generation $N starting"
    /bin/getty -n -l /bin/sh 115200 ttyS0
    echo "cella-shell: getty generation $N exited with $?"
done
