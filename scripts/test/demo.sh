#!/usr/bin/env bash
# make demo: an end-to-end demonstration of the freeze and thaw. It
# boots an interactive guest, stores a value in the shell, freezes the
# guest, thaws it, and reads the value back from the same shell. The
# demo tears its guest down at the end, pass or fail. It uses its own
# state directory, and it does not touch a guest at the default
# VM_DIR.
set -euo pipefail

cd "$(dirname "$0")/../.."
DIR=demo-vm
SESSION="cella-$DIR"
LOG_GLOB=".logs/console-$DIR-*"

if ! [ -r /dev/kvm ] || ! [ -w /dev/kvm ]; then
    echo "SKIP: no read and write access to /dev/kvm"
    exit 0
fi
command -v tmux >/dev/null || { echo "SKIP: tmux not found -- run: make init"; exit 0; }

teardown() {
    tmux kill-session -t "$SESSION" 2>/dev/null || true
    rm -rf "$DIR"
}
trap teardown EXIT

say() { echo; echo "==> $1"; }
type_in() { tmux send-keys -t "$SESSION" "$1" Enter; sleep 2; }
console() { ls -t $LOG_GLOB 2>/dev/null | head -1; }

teardown
say "step 1: boot a detached guest with a shell on the serial console"
make --no-print-directory boot VM_DIR="$DIR" NET=none >/dev/null
sleep 7

say "step 2: store a value in the shell"
type_in 'MARKER=aurora-$((19*7)); echo "value-set: $MARKER"'
grep -a "value-set: aurora-133" "$(console)" >/dev/null \
    || { echo "FAIL: the shell did not respond before the freeze"; exit 1; }
grep -a "value-set:" "$(console)" | tail -1

say "step 3: freeze the guest (SIGUSR1)"
pkill -USR1 -x cella
sleep 4
echo "the guest is now two files:"
ls -l "$DIR" | tail -n +2

say "step 4: thaw the guest"
make --no-print-directory thaw VM_DIR="$DIR" NET=none >/dev/null
sleep 8

say "step 5: read the value back from the same shell"
type_in 'echo "value-after-thaw: $MARKER"'
if grep -a "value-after-thaw: aurora-133" "$(console)" >/dev/null; then
    grep -a "value-after-thaw:" "$(console)" | tail -1
    echo
    echo "PASS: the shell state survived the freeze and the thaw"
    exit 0
fi
echo "FAIL: the shell did not respond after the thaw"
echo "-- retry after an interrupt, to see whether the session is alive:"
tmux send-keys -t "$SESSION" C-c; sleep 1
type_in 'echo retry-value: $MARKER'
grep -a "retry-value" "$(console)" | tail -2 || true
echo "-- getty generations on the console:"
grep -a "cella-shell:" "$(console)" || true
tail -5 "$(console)" || true
# Post-mortem: freeze the deaf guest again and keep the register dump,
# so that the failure carries its own evidence past the teardown.
pkill -USR1 -x cella || true
sleep 4
DUMP=".logs/demo-fail-dump-$(date +%Y%m%d-%H%M%S).log"
./target/release/cella --dump-state "$DIR" > "$DUMP" 2>&1 || true
echo "(post-mortem sidecar dump: $DUMP)"
grep -a "serial:" "$DUMP" || true
exit 1
