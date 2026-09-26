#!/usr/bin/env bash
# smoke-himem: a machine larger than the hole. Guest RAM used to be
# one flat region, so past 3328 MiB it shadowed the virtio windows
# at 0xd0000000 and the kernel panicked unable to find vda -- the
# titanium 4096 report. Now RAM splits around the hole (low bank at
# zero, remainder at 4 GiB, the PC layout) and this gate pins the
# contract: a 4096 MiB machine boots, sees its memory, keeps every
# byte across a freeze and thaw, and ends by its own reboot.
set -uo pipefail

cd "$(dirname "$0")/../.."
BIN=target/lab/cella
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build-lab"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-himem.XXXXXX)
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

echo "==> a 4096 MiB machine boots and sees its memory"
"$BIN" create "$VM" --mem-mb 4096 >/dev/null || { echo "FAIL: create at 4096"; exit 1; }
"$BIN" start "$VM" >/dev/null || { echo "FAIL: start"; exit 1; }
sleep 10
type_in "grep MemTotal /proc/meminfo; echo mem-see\"n\""
grep -aq "mem-seen" "$M/console.log" || { echo "FAIL: the console never answered"; exit 1; }
KB=$(grep -a "MemTotal" "$M/console.log" | tail -1 | tr -dc 0-9)
# 4096 MiB minus kernel reservations lands near 4026852 kB; the
# floor at 3.5 GiB separates the split layout from a clamped or
# shadowed one with a wide margin.
[ "${KB:-0}" -ge $((3584 * 1024)) ] \
    || { echo "FAIL: MemTotal ${KB:-0} kB -- the high bank is missing"; exit 1; }
echo "  MemTotal ${KB} kB: both banks present"

echo "==> the high bank survives the freeze"
type_in "dd if=/dev/urandom of=/tmp/high bs=1M count=64 2>/dev/null; sha256sum /tmp/high > /tmp/sum; echo mark-don\"e\""
grep -aq "mark-done" "$M/console.log" || { echo "FAIL: the canary never wrote"; exit 1; }
"$BIN" freeze "$VM" >/dev/null || { echo "FAIL: freeze"; exit 1; }
"$BIN" thaw "$VM" >/dev/null || { echo "FAIL: thaw"; exit 1; }
sleep 3
type_in "sha256sum -c /tmp/sum && echo canary-hel\"d\""
grep -aq "canary-held" "$M/console.log" \
    || { echo "FAIL: the canary changed across the freeze"; exit 1; }
echo "  64 MiB of RAM-backed bytes, byte-exact across freeze and thaw"

echo "==> the machine ends by its own reboot"
P=$(cat "$M/pid")
type_in "reboot -f"
DEADLINE=$((SECONDS + 30))
while kill -0 "$P" 2>/dev/null; do
    [ $SECONDS -lt $DEADLINE ] || { echo "FAIL: the VMM outlived the reboot"; exit 1; }
    sleep 0.5
done
grep -q "guest exit: shutdown exit=reset" "$M/vmm.log" \
    || { echo "FAIL: the exit is not on the record"; exit 1; }
echo "PASS: himem -- the split layout boots, remembers, and ends"
