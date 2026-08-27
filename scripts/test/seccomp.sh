#!/usr/bin/env bash
# Runs cella's seccomp self-test hook: install the real filter, then
# deliberately make a disallowed syscall. Passes iff the kernel kills the
# process with SIGSYS -- not a simulation, the actual filter, actually
# installed, actually tripped. Does not need /dev/kvm.
set -uo pipefail  # not -e: we need the non-zero/signal exit code

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="${CELLA_BIN:-$HERE/../../target/release/cella}"

if [ ! -x "$BIN" ]; then
    echo "FAIL: $BIN not built (run: make build)"
    exit 1
fi

"$BIN" --selftest-seccomp
status=$?

# Bash reports a signal-killed child as exit code 128+signum.
SIGSYS=31
expected=$((128 + SIGSYS))

if [ "$status" -eq "$expected" ]; then
    echo "PASS: seccomp filter killed the process with SIGSYS (exit $status) on a disallowed syscall"
    exit 0
elif [ "$status" -eq 42 ]; then
    echo "FAIL: the disallowed syscall was NOT blocked -- the filter did not fire"
    exit 1
else
    echo "FAIL: unexpected exit status $status (expected $expected for SIGSYS)"
    exit 1
fi
