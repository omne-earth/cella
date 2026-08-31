#!/usr/bin/env bash
# make demo: the cryogenic chamber, narrated. Boots an interactive
# guest, gives its shell a variable, freezes the guest, thaws it, and
# asks the same shell for the variable. The shell answers, because the
# freeze does not exist for the guest. Tears its guest down at the
# end, pass or fail. Uses its own state directory, and does not touch
# a guest at the default VM_DIR.
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
say "boot: a guest with a shell on its serial console, detached"
make --no-print-directory boot VM_DIR="$DIR" NET=none >/dev/null
sleep 7

say "the shell learns a secret"
type_in 'SECRET=aurora-$((19*7)); echo "the secret is $SECRET"'
grep -a "the secret is aurora-133" "$(console)" >/dev/null \
    || { echo "FAIL: the shell did not answer before the freeze"; exit 1; }
grep -a "the secret is" "$(console)" | tail -1

say "freeze: the guest stops mid-conversation (SIGUSR1)"
pkill -USR1 -x cella
sleep 4
echo "the guest is now two files:"
ls -l "$DIR" | tail -n +2

say "thaw: the same guest, the same instant"
make --no-print-directory thaw VM_DIR="$DIR" NET=none >/dev/null
sleep 8

say "the shell still knows"
type_in 'echo "woke up with $SECRET intact"'
if grep -a "woke up with aurora-133 intact" "$(console)" >/dev/null; then
    grep -a "woke up with" "$(console)" | tail -1
    echo
    echo "PASS: the freeze did not exist for the guest"
    exit 0
fi
echo "FAIL: the shell lost its memory across the thaw"
tail -5 "$(console)" || true
exit 1
