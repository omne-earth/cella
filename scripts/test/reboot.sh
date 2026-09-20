#!/usr/bin/env bash
# smoke-reboot: a guest-initiated reset ends the VMM (the one-shot
# exit door). With reboot=t on the command line the kernel's reboot
# path triple faults, KVM surfaces KVM_EXIT_SHUTDOWN with no device
# emulation, and the VMM exits 0 with the word on the record. The
# bug this gate pins: under reboot=k the reset wrote to a port this
# VMM never emulated, the guest looped in kb_wait forever, and the
# machine sat running-but-dead -- measured on titanium's sealed
# one-shot trials, and the extractor's "a reset is not a reliable
# exit" caveat was this same hole. The trailer-poll stays as the
# safety net; this gate makes the exit the path.
set -uo pipefail

cd "$(dirname "$0")/../.."
BIN=target/lab/cella
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build-lab"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-reboot.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

VM=phoenix
M="$CELLA_HOME/machines/$VM"
teardown() {
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    "$BIN" destroy "$VM" >/dev/null 2>&1 || true
    if [ -n "${CELLA_KEEP_SANDBOX:-}" ]; then echo "kept: $CELLA_HOME"; else rm -rf "$CELLA_HOME"; fi
}
trap teardown EXIT

echo "==> a machine whose job is to reboot"
"$BIN" create "$VM" >/dev/null
"$BIN" start "$VM" >/dev/null
VMM_PID=$(cat "$M/pid")
sleep 3

(printf 'reboot -f\n'; sleep 2) | timeout 20 "$BIN" enter "$VM" >/dev/null 2>&1 || true

DEADLINE=$((SECONDS + 30))
while kill -0 "$VMM_PID" 2>/dev/null; do
    [ $SECONDS -lt $DEADLINE ] || {
        echo "FAIL: the VMM is still alive 30s after the guest's reboot -- running-but-dead"
        exit 1
    }
    sleep 0.5
done
echo "  the VMM exited within $((SECONDS))s of the reset"

grep -q "cella: guest requested shutdown" "$M/vmm.log" || {
    echo "FAIL: the exit is not on the record -- no 'guest requested shutdown' in vmm.log"
    exit 1
}
echo "  the word is on the record: guest requested shutdown"

# The books recover: the pid on disk names a dead process, so the
# machine is not running, and destroy needs no stop first.
"$BIN" destroy "$VM" >/dev/null || { echo "FAIL: destroy refused the dead machine"; exit 1; }
echo "PASS: a guest reset ends the VMM, exit on the record, destroy recovers"
