#!/usr/bin/env bash
# The delta battery: rerun only the targets that errored in a prior
# `make -k smoke` log. Usage: make smoke-delta SMOKE_LOG=<smoke log>
# (default: the newest *smoke*.log under .logs). Each target runs in
# order, serially, and the tally is the verdict.
set -uo pipefail
cd "$(dirname "$0")/../.."
LOG="${1:-$(ls -t .logs/*smoke*.log 2>/dev/null | head -1)}"
[ -f "$LOG" ] || { echo "smoke-delta: no smoke log (pass SMOKE_LOG=<file>)"; exit 2; }
targets=$(grep -aoE 'make: \*\*\* \[Makefile:[0-9]+: [a-z0-9-]+\]' "$LOG" | sed -E 's/.*: ([a-z0-9-]+)\]/\1/' | grep -v '^smoke$' | sort -u)
[ -n "$targets" ] || { echo "smoke-delta: nothing errored in $LOG"; exit 0; }
echo "smoke-delta: rerunning: $(echo $targets | tr '\n' ' ')"
fail=0
for t in $targets; do
    make "$t" > ".logs/delta-$t.log" 2>&1; rc=$?
    echo "$t rc=$rc"; [ $rc -eq 0 ] || fail=1
done
[ $fail -eq 0 ] && echo "PASS: the delta is green" || { echo "FAIL: see .logs/delta-*.log"; exit 1; }
