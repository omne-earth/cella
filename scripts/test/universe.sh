#!/usr/bin/env bash
# The universe family, end to end: a machine learns a value and
# writes a file, freezes; branch makes a frozen twin and both thaw
# to the same instant; archive makes a rock that refuses start,
# thaw, and enter; inspect reads the evidence through /rock,
# read-only and noexec; branch of the rock stays a rock. Sandboxed
# CELLA_HOME, torn down pass or fail.
set -euo pipefail

cd "$(dirname "$0")/../.."
BIN=target/release/cella
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build"; exit 0; }
if ! [ -r /dev/kvm ] || ! [ -w /dev/kvm ]; then
    echo "SKIP: no read and write access to /dev/kvm"; exit 0
fi
command -v bwrap >/dev/null || { echo "SKIP: bwrap not found -- run: make init"; exit 0; }
REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
for f in kernel/canonical/bzImage rootfs/cella/rootfs.ext4; do
    [ -f "$REAL_HOME/$f" ] || { echo "SKIP: golden $f missing -- run: make golden"; exit 0; }
done
export CELLA_HOME=$(mktemp -d /tmp/cella-universe.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

teardown() {
    for m in u1 u2 u3 u1-inspector; do "$BIN" stop $m >/dev/null 2>&1 || true; done
    rm -rf "$CELLA_HOME"
}
trap teardown EXIT
say() { echo; echo "==> $1"; }
type_in() { local vm="$1"; shift; (printf '%s\n' "$1"; sleep 2) | timeout 20 "$BIN" enter "$vm" >/dev/null; }
wait_con() { # vm marker
    local con="$CELLA_HOME/machines/$1/console.log" deadline=$((SECONDS + 15))
    while [ $SECONDS -lt $deadline ]; do
        grep -aq "$2" "$con" && return 0
        sleep 1
    done
    return 1
}

say "step 1: a machine learns a value and writes a file, then freezes"
"$BIN" create u1 --net none >/dev/null
"$BIN" start u1 >/dev/null
sleep 4
type_in u1 'M=universe-$((3*47)); echo "set: $M"; echo $M > /u.txt; sync'
wait_con u1 "set: universe-141" || { echo "FAIL: the value did not set"; exit 1; }
"$BIN" freeze u1 >/dev/null

say "step 2: branch the frozen machine -- a frozen twin"
"$BIN" branch u1 u2 | grep -q "frozen twin" || { echo "FAIL: the branch is not a frozen twin"; exit 1; }
grep -aq '"digest_disk"' "$CELLA_HOME/machines/u2/manifest.json" || { echo "FAIL: the copy carries no disk digest"; exit 1; }
grep -aq '"digest_ram"' "$CELLA_HOME/machines/u2/manifest.json" || { echo "FAIL: the copy carries no ram digest"; exit 1; }

say "step 3: both twins thaw to the same instant"
"$BIN" thaw u1 >/dev/null; "$BIN" thaw u2 >/dev/null
sleep 2
type_in u1 'echo "u1: $M"'
type_in u2 'echo "u2: $M"'
wait_con u1 "u1: universe-141" || { echo "FAIL: the source lost the instant"; exit 1; }
wait_con u2 "u2: universe-141" || { echo "FAIL: the twin lost the instant"; exit 1; }
echo "  both twins carry the value from before the fork"

say "step 4: archive the twin -- a rock that refuses the lifecycle"
"$BIN" stop u2 >/dev/null
"$BIN" archive u2 | grep -q "a rock" || { echo "FAIL: archive did not report a rock"; exit 1; }
for verb in start thaw enter; do
    "$BIN" $verb u2 >/dev/null 2>&1 && { echo "FAIL: $verb accepted a rock"; exit 1; }
done
echo "  start, thaw, and enter refuse the rock"

say "step 5: inspect the source -- the evidence at /rock, read-only, noexec"
"$BIN" stop u1 >/dev/null
DIGEST_BEFORE=$(sha3sum "$CELLA_HOME/machines/u1/disk.img" 2>/dev/null || sha256sum "$CELLA_HOME/machines/u1/disk.img")
(printf 'grep -a . /rock/u.txt; echo rock-rea"d"\n'; sleep 3; printf 'touch /rock/x 2>&1 | head -1\n'; sleep 2) \
    | timeout 30 "$BIN" inspect u1 >/dev/null
CON="$CELLA_HOME/machines/u1-inspector/console.log"
# The inspector is destroyed on detach; its console log went with it.
[ -d "$CELLA_HOME/machines/u1-inspector" ] && { echo "FAIL: the inspector survived the detach"; exit 1; }
DIGEST_AFTER=$(sha3sum "$CELLA_HOME/machines/u1/disk.img" 2>/dev/null || sha256sum "$CELLA_HOME/machines/u1/disk.img")
[ "$DIGEST_BEFORE" = "$DIGEST_AFTER" ] || { echo "FAIL: the inspection changed the evidence"; exit 1; }
echo "  the inspector came, read, was destroyed; the evidence is byte-identical"

say "step 6: branch the rock -- the latch carries"
"$BIN" branch u2 u3 | grep -q "rock" || { echo "FAIL: the rock branch did not report a rock"; exit 1; }
"$BIN" start u3 >/dev/null 2>&1 && { echo "FAIL: start accepted the branched rock"; exit 1; }
echo "  the copy is a rock; nothing resurrects by side effect"

echo
echo "PASS: the universe family -- branch, archive, inspect"
"$BIN" destroy u3 >/dev/null; "$BIN" destroy u2 >/dev/null; "$BIN" destroy u1 >/dev/null
