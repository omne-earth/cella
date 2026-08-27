#!/usr/bin/env bash
# Verifies the rootless bwrap jail actually confines filesystem access --
# not a boot test, just "does the sandbox sandbox." Runs anywhere bwrap
# is installed; does not need /dev/kvm.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="${CELLA_BIN:-$HERE/../../target/release/cella}"

if ! command -v bwrap >/dev/null; then
    echo "SKIP: bubblewrap not installed"
    exit 0
fi
if [ ! -x "$BIN" ]; then
    echo "FAIL: $BIN not built (run: make build)"
    exit 1
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
STATE_DIR="$TMP/state"
DISK="$TMP/disk.img"
mkdir -p "$STATE_DIR"
: > "$DISK"

# A secret file outside anything jail.sh binds in. If the jail is doing
# its job, cella (or anything jail.sh launches) must not be able to
# read it, even though the *host* process running this script can.
SECRET="$TMP/outside/secret"
mkdir -p "$TMP/outside"
echo "sandbox-escape-canary" > "$SECRET"

echo "--- probe 1: bwrap denies a path never bound in ---"
if bwrap \
    --unshare-user --unshare-pid --unshare-ipc --unshare-uts --unshare-cgroup \
    --ro-bind "$BIN" /cella \
    --ro-bind /usr /usr --ro-bind /bin /bin --ro-bind /lib /lib \
    $( [ -d /lib64 ] && echo --ro-bind /lib64 /lib64 ) \
    --proc /proc --tmpfs /tmp \
    --die-with-parent --new-session \
    /bin/cat "$SECRET" 2>/dev/null
then
    echo "FAIL: the secret file outside the jail was readable from inside it"
    exit 1
fi
echo "PASS: secret file outside the jail bind set was not readable"

echo "--- probe 2: jail.sh's own bind set matches --state-dir/--disk, nothing else ---"
# jail.sh should refuse to run without cella's required args (fails
# fast, before ever calling bwrap) -- this is jail.sh's own input
# validation, exercised for real.
if "$HERE/../jail.sh" --tap tap0 >/tmp/jail_missing_args.$$ 2>&1; then
    echo "FAIL: jail.sh should have refused to run without --state-dir/--disk"
    cat /tmp/jail_missing_args.$$
    rm -f /tmp/jail_missing_args.$$
    exit 1
fi
rm -f /tmp/jail_missing_args.$$
echo "PASS: jail.sh refuses to launch without required arguments"

echo "--- probe 3: no ambient CAP_NET_ADMIN inside the jail ---"
# The jail should not be able to create network devices -- that
# capability was deliberately spent once, out of band, in scripts/setup/tap.sh.
if bwrap \
    --unshare-user --unshare-pid --unshare-ipc --unshare-uts --unshare-cgroup \
    --ro-bind /usr /usr --ro-bind /bin /bin --ro-bind /lib /lib \
    $( [ -d /lib64 ] && echo --ro-bind /lib64 /lib64 ) \
    --proc /proc --tmpfs /tmp \
    --die-with-parent --new-session \
    /bin/sh -c 'ip tuntap add mode tap probe0 2>/dev/null' 2>/dev/null
then
    echo "FAIL: created a TAP device from inside the jail (unexpected CAP_NET_ADMIN)"
    exit 1
fi
echo "PASS: no CAP_NET_ADMIN inside the jail"

echo "ALL JAIL PROBES PASSED"
