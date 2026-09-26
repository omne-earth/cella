#!/usr/bin/env bash
# smoke-memory: the guest memory line, one size per invocation --
# each case pins a wall this VMM once hit:
#   m3328  the flat boundary: the largest single-bank guest (RAM
#          ends exactly where the virtio windows begin)
#   m3400  the first split: a 72 MiB high bank at 4 GiB -- the
#          smallest guest that crosses the hole
#   m4096  the titanium repro: RAM used to shadow the windows and
#          the kernel panicked unable to find vda
#   m4608  the volve repro, the page-table wall: boot tables used
#          to scale with guest size and refused past 4 GiB
#   m6144  a deep high bank, and the freeze: 64 MiB of RAM-backed
#          bytes must survive freeze and thaw byte-exact
# Every case boots, proves MemTotal sees both banks, and ends by
# the guest's own reboot (the reboot=t contract).
set -uo pipefail

CASE="${1:-}"
case "$CASE" in
m3328|m3400|m4096|m4608|m6144) ;;
*) echo "usage: memory.sh <m3328|m3400|m4096|m4608|m6144>"; exit 2 ;;
esac
MB="${CASE#m}"

cd "$(dirname "$0")/../.."
BIN=target/lab/cella
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build-lab"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0
# The deep cases map guests the host must back; a small host skips
# rather than swaps.
FREE_KB=$(awk '/MemAvailable/{print $2}' /proc/meminfo)
[ "$FREE_KB" -ge $((MB * 1024 + 1024 * 1024)) ] \
    || { echo "SKIP: host has ${FREE_KB} kB available -- ${MB} MiB guest needs headroom"; exit 0; }

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-memory.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

VM=tall
M="$CELLA_HOME/machines/$VM"
teardown() {
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    "$BIN" destroy "$VM" >/dev/null 2>&1 || true
    if [ -n "${CELLA_KEEP_SANDBOX:-}" ]; then echo "kept: $CELLA_HOME"; else rm -rf "$CELLA_HOME"; fi
}
trap teardown EXIT
type_in() { (printf '%s\n' "$1"; sleep 3) | timeout 30 "$BIN" enter "$VM" >/dev/null 2>&1 || true; }
# A typed command keeps running after enter detaches; wait for its
# marker instead of glancing for it.
wait_log() { # <marker> <secs>
    local deadline=$((SECONDS + $2))
    until grep -aq "$1" "$M/console.log"; do
        [ $SECONDS -lt $deadline ] || return 1
        sleep 1
    done
}

echo "==> $CASE: a ${MB} MiB machine boots and sees its memory"
"$BIN" create "$VM" --mem-mb "$MB" >/dev/null || { echo "FAIL: create at $MB"; exit 1; }
"$BIN" start "$VM" >/dev/null || { echo "FAIL: start"; exit 1; }
sleep 10
type_in "grep MemTotal /proc/meminfo; echo mem-see\"n\""
wait_log "mem-seen" 30 || { echo "FAIL: the console never answered"; exit 1; }
KB=$(grep -a "MemTotal" "$M/console.log" | tail -1 | tr -dc 0-9)
# Kernel reservations cost well under 7%; the floor separates a
# whole guest from a clamped, shadowed, or unmapped bank.
FLOOR=$((MB * 1024 * 93 / 100))
[ "${KB:-0}" -ge "$FLOOR" ] \
    || { echo "FAIL: MemTotal ${KB:-0} kB < floor ${FLOOR} kB -- a bank is missing"; exit 1; }
echo "  MemTotal ${KB} kB: the whole guest is present"

if [ "$CASE" = m6144 ]; then
    echo "==> the high bank survives the freeze"
    type_in "dd if=/dev/urandom of=/tmp/high bs=1M count=64 2>/dev/null; sha256sum /tmp/high > /tmp/sum; echo mark-don\"e\""
    wait_log "mark-done" 90 || { echo "FAIL: the canary never wrote"; exit 1; }
    "$BIN" freeze "$VM" >/dev/null || { echo "FAIL: freeze"; exit 1; }
    "$BIN" thaw "$VM" >/dev/null || { echo "FAIL: thaw"; exit 1; }
    sleep 3
    type_in "sha256sum -c /tmp/sum && echo canary-hel\"d\""
    wait_log "canary-held" 90 \
        || { echo "FAIL: the canary changed across the freeze"; exit 1; }
    echo "  64 MiB of RAM-backed bytes, byte-exact across freeze and thaw"
fi

P=$(cat "$M/pid")
type_in "reboot -f"
DEADLINE=$((SECONDS + 30))
while kill -0 "$P" 2>/dev/null; do
    [ $SECONDS -lt $DEADLINE ] || { echo "FAIL: the VMM outlived the reboot"; exit 1; }
    sleep 0.5
done
grep -q "guest exit: shutdown exit=reset" "$M/vmm.log" \
    || { echo "FAIL: the exit is not on the record"; exit 1; }
echo "PASS: $CASE -- ${MB} MiB boots whole and ends clean"
