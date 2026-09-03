#!/usr/bin/env bash
# The rootless sweep (1.6.14e, the load-bearing claim): after make
# install, cella holds no capability, and no tap, bridge, nft
# table, or boot unit of cella's exists on the host. The network is
# processes and file descriptors the user owns, nothing more.
set -euo pipefail

cd "$(dirname "$0")/../.."

fail=0
say() { echo "==> $1"; }

say "no file capability on any cella binary"
for b in target/release/cella* target/smoke/cella* "$HOME"/.local/bin/cella*; do
    [ -f "$b" ] || continue
    if caps=$(getcap "$b" 2>/dev/null) && [ -n "$caps" ]; then
        echo "FAIL: $caps"; fail=1
    fi
done

say "no tap, pair tap, or bridge of cella's on the host"
if ip -o link show 2>/dev/null | awk -F': ' '{print $2}' | grep -qE '^(tap[0-9]+|pair[0-9]+[ag]|brp[0-9]+)$'; then
    echo "FAIL: a pool interface still exists:"; ip -br link | grep -E '^(tap[0-9]+|pair[0-9]+[ag]|brp[0-9]+)\b' || true; fail=1
fi

say "no nft table of cella's"
if sudo -n nft list table inet cella_nat >/dev/null 2>&1; then
    echo "FAIL: the cella_nat table still exists"; fail=1
fi

say "no boot unit"
if systemctl --user is-enabled cella-network.service >/dev/null 2>&1 || systemctl is-enabled cella-network.service >/dev/null 2>&1; then
    echo "FAIL: a cella-network.service unit is still enabled"; fail=1
fi

say "no privileged cella process"
if pgrep -x cella-network >/dev/null; then
    for p in $(pgrep -x cella-network); do
        if grep -qE '^CapEff:\s+0{16}$' "/proc/$p/status" 2>/dev/null; then :; else
            echo "FAIL: cella-network pid $p holds capabilities"; fail=1
        fi
    done
fi

[ "$fail" -eq 0 ] || exit 1
echo
echo "PASS: rootless -- no capability, no host object, no unit; the network is the user's own processes and fds"
