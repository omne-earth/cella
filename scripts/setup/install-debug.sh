#!/usr/bin/env bash
# The lab install, deliberate and named: the smoke-profile binary
# (release-sized, debug-assertions on) under the -debug suffix, thus
# the flavor is visible in every invocation and the two flavors never
# shadow each other on PATH. The console exists in this flavor alone:
# console.log, console.sock, and enter are lab instruments. No shared
# code with install-release.sh, thus no accidental install of the
# wrong flavor. The field install owns the packages, the capability
# binary, and the boot unit; this script installs binaries only.
#
# Usage: scripts/setup/install-debug.sh
set -euo pipefail

cd "$(dirname "$0")/../.."

cargo build --profile smoke
# Every persona is its own binary since the split (1.6.13), and the
# lab flavor carries the -debug suffix on each: a -debug shim execs
# -debug personas, and the two flavors never shadow each other.
for name in cella cella-machine cella-vmm cella-gateway cella-universe cella-build cella-doctor cella-probe; do
    install -D -m 0755 "target/smoke/$name" "$HOME/.local/bin/$name-debug"
done
echo "cella: lab flavor installed -> ~/.local/bin/cella-debug (and its -debug personas)"
echo "cella: the console exists in this flavor alone; the field flavor stays dark"
