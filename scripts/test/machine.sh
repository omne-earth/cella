#!/usr/bin/env bash
# Exercises the machine registry with the real binary against a
# sandboxed CELLA_HOME: build produces the goldens natively, create
# stages a machine, a second create refuses, destroy removes it. No
# /dev/kvm is needed: no verb here starts a process.
set -euo pipefail

cd "$(dirname "$0")/../.."
BIN=target/release/cella
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build"; exit 0; }
# The native build reuses the repo-level source caches; without them
# the first run downloads and compiles, which is not a unit test.
[ -d target/kernel-build ] && [ -d target/rootfs-build ] \
    || { echo "SKIP: build caches missing -- run: make golden once"; exit 0; }

export CELLA_HOME=$(mktemp -d /tmp/cella-machine-test.XXXXXX)
trap 'rm -rf "$CELLA_HOME"' EXIT

"$BIN" build kernel canonical >/dev/null
"$BIN" build rootfs cella >/dev/null
[ -f "$CELLA_HOME/kernel/canonical/bzImage" ] || { echo "FAIL: golden kernel missing"; exit 1; }

"$BIN" create m1 --net none --mem-mb 128 >/dev/null
[ -f "$CELLA_HOME/machines/m1/disk.img" ] || { echo "FAIL: machine disk missing"; exit 1; }
grep -q '"mem_mb": 128' "$CELLA_HOME/machines/m1/manifest.json" \
    || { echo "FAIL: manifest does not carry the configuration"; exit 1; }

"$BIN" create m1 2>/dev/null && { echo "FAIL: a second create must refuse"; exit 1; }

"$BIN" list | grep -q "m1.*created" || { echo "FAIL: list does not show the machine"; exit 1; }
"$BIN" info m1 | grep -q "state:   created" || { echo "FAIL: info does not show the state"; exit 1; }

"$BIN" destroy m1 >/dev/null
[ ! -d "$CELLA_HOME/machines/m1" ] || { echo "FAIL: destroy left the directory"; exit 1; }

echo "PASS: build, create, list, info, refuse-duplicate, destroy"
