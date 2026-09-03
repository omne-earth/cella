#!/usr/bin/env bash
# Generic per-persona seccomp negative gate (1.6.14b): install that
# persona's own filter, then deliberately trip it with socket(2) --
# the shared canary every persona's table leaves out on purpose (see
# cella-libs/src/seccomp.rs). Passes iff the kernel kills the process
# with SIGSYS. Does not need /dev/kvm.
#
# Usage: scripts/test/seccomp-persona.sh <persona-binary-name>
set -uo pipefail  # not -e: we need the non-zero/signal exit code

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NAME="${1:?usage: seccomp-persona.sh <persona-binary-name>}"
BIN="${CELLA_BIN:-$HERE/../../target/release/$NAME}"

if [ ! -x "$BIN" ]; then
    echo "FAIL: $BIN not built (run: make build)"
    exit 1
fi

"$BIN" --selftest-seccomp
status=$?

SIGSYS=31
expected=$((128 + SIGSYS))

if [ "$status" -eq "$expected" ]; then
    echo "PASS: $NAME's seccomp filter killed the process with SIGSYS (exit $status) on a disallowed syscall"
    exit 0
elif [ "$status" -eq 42 ]; then
    echo "FAIL: $NAME's disallowed syscall was NOT blocked -- the filter did not fire"
    exit 1
else
    echo "FAIL: $NAME: unexpected exit status $status (expected $expected for SIGSYS)"
    exit 1
fi
