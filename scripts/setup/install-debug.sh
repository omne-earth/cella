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
install -D -m 0755 target/smoke/cella "$HOME/.local/bin/cella-debug"
install -D -m 0755 target/smoke/cella-probe "$HOME/.local/bin/cella-probe-debug"
# The multi-call names, suffixed: persona dispatch strips -debug.
for name in cella-machine cella-build cella-doctor cella-vmm cella-universe cella-gateway; do
    ln -sf cella-debug "$HOME/.local/bin/$name-debug"
done
echo "cella: lab flavor installed -> ~/.local/bin/cella-debug (and its -debug personas)"
echo "cella: the console exists in this flavor alone; the field flavor stays dark"
