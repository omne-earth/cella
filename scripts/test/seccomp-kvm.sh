#!/usr/bin/env bash
# Runs cella-vmm's KVM-ioctl-filter self-test: install the real
# seccomp filter (with the KVM request table), then issue an ioctl
# whose request is not on that table. Passes iff the kernel kills the
# process with SIGSYS -- proving the argument filter fires on its own,
# not just the outer allowlist that already lets `ioctl` the syscall
# through. Does not need /dev/kvm: the probe ioctl targets /dev/null.
set -uo pipefail  # not -e: we need the non-zero/signal exit code

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="${CELLA_BIN:-$HERE/../../target/release/cella-vmm}"

if [ ! -x "$BIN" ]; then
    echo "FAIL: $BIN not built (run: make build)"
    exit 1
fi

"$BIN" --selftest-seccomp-kvm-ioctl
status=$?

SIGSYS=31
expected=$((128 + SIGSYS))

if [ "$status" -eq "$expected" ]; then
    echo "PASS: the KVM ioctl request filter killed the process with SIGSYS (exit $status) on a request outside the table"
    exit 0
elif [ "$status" -eq 42 ]; then
    echo "FAIL: the disallowed ioctl request was NOT blocked -- the request filter did not fire"
    exit 1
else
    echo "FAIL: unexpected exit status $status (expected $expected for SIGSYS)"
    exit 1
fi
