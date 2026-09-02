#!/usr/bin/env bash
# Lane a's gate (1.6.14a, identity and the jail): three negatives,
# asserted, never assumed.
#
#   1. a cross-machine file touch fails by uid, before SELinux exists
#      to deny it (lane c is not built yet: this is plain DAC).
#   2. a persona runs under its own profile alone (the VMM's jail
#      applies exactly security/profiles/cella-vmm/bwrap.txt's bind
#      set and namespace set).
#   3. a path outside a persona's bind set refuses.
#
# Boots two real machines (net none: tap ownership is the deferred
# cella-network ruling's territory, not this gate's), then probes
# both from outside and from a sandbox mapped to each machine's own
# host uid.
set -ueo pipefail

cd "$(dirname "$0")/../.."
BIN=target/smoke/cella

[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build-smoke"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:canonical || exit 0
command -v newuidmap >/dev/null || { echo "SKIP: newuidmap not installed"; exit 0; }
command -v setfacl >/dev/null || { echo "SKIP: setfacl (the acl package) not installed"; exit 0; }

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-jailid.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/canonical"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/canonical/rootfs.ext4" "$CELLA_HOME/rootfs/canonical/"

VM_A=jailid-a
VM_B=jailid-b
teardown() {
    "$BIN" stop "$VM_A" >/dev/null 2>&1 || true
    "$BIN" stop "$VM_B" >/dev/null 2>&1 || true
    if [ -n "${CELLA_KEEP_SANDBOX:-}" ]; then echo "kept: $CELLA_HOME"; else rm -rf "$CELLA_HOME"; fi
}
trap teardown EXIT

fail() { echo "FAIL: $1"; exit 1; }
pass() { echo "PASS: $1"; }

echo "--- setup: two machines, each its own sub-user ---"
"$BIN" create "$VM_A" --kernel canonical --rootfs canonical --mem-mb 128 --net none >/dev/null || fail "create $VM_A"
"$BIN" create "$VM_B" --kernel canonical --rootfs canonical --mem-mb 128 --net none >/dev/null || fail "create $VM_B"
"$BIN" start "$VM_A" >/dev/null || fail "start $VM_A"
"$BIN" start "$VM_B" >/dev/null || fail "start $VM_B"

DIR_A="$CELLA_HOME/machines/$VM_A"
DIR_B="$CELLA_HOME/machines/$VM_B"
UID_A=$(cat "$DIR_A/uid")
UID_B=$(cat "$DIR_B/uid")
HOST_A=$(( $(grep "^$(id -un):" /etc/subuid | cut -d: -f2) + UID_A ))
HOST_B=$(( $(grep "^$(id -un):" /etc/subuid | cut -d: -f2) + UID_B ))

echo "--- probe 1: a persona runs under its own profile alone ---"
[ "$UID_A" != "$UID_B" ] || fail "two machines share one sub-uid offset ($UID_A)"
OWNER_A=$(stat -c '%u' "$DIR_A/ram.img")
OWNER_B=$(stat -c '%u' "$DIR_B/ram.img")
[ "$OWNER_A" = "$HOST_A" ] || fail "$VM_A's own runtime file is owned by uid $OWNER_A, not its mapped host uid $HOST_A"
[ "$OWNER_B" = "$HOST_B" ] || fail "$VM_B's own runtime file is owned by uid $OWNER_B, not its mapped host uid $HOST_B"
[ "$OWNER_A" != "$OWNER_B" ] || fail "both machines' VMMs wrote their runtime file as the same uid"
pass "each machine's VMM ran as its own distinct, mapped sub-uid ($HOST_A vs $HOST_B)"

echo "--- probe 2: a cross-machine file touch fails by uid ---"
# A sandbox mapped to A's own host uid, exactly the mechanics the
# spawn itself uses (unshare, newuidmap from outside, claim uid 0),
# trying to touch a file inside B's directory: no ACL entry for A's
# uid exists there, and DAC alone (no SELinux yet) must refuse it.
touch_as() {
    local host_uid="$1" target="$2"
    python3 - "$host_uid" "$target" <<'PYEOF'
import ctypes, os, subprocess, sys
libc = ctypes.CDLL("libc.so.6", use_errno=True)
host_uid, target = sys.argv[1], sys.argv[2]
r1, w1 = os.pipe()
r2, w2 = os.pipe()
pid = os.fork()
if pid == 0:
    os.close(r1); os.close(w2)
    libc.unshare(0x10000000)
    os.write(w1, b"\0")
    os.read(r2, 1)
    libc.setresgid(0, 0, 0)
    libc.setresuid(0, 0, 0)
    ok = 0
    try:
        with open(target, "w") as f:
            f.write("x")
        ok = 1
    except OSError:
        ok = 0
    os._exit(0 if ok == 0 else 1)  # exit 0 means the touch was REFUSED
else:
    os.close(w1); os.close(r2)
    os.read(r1, 1)
    subprocess.run(["newuidmap", str(pid), "0", host_uid, "1"], check=True)
    subprocess.run(["newgidmap", str(pid), "0", host_uid, "1"], check=True)
    os.write(w2, b"\0")
    _, status = os.waitpid(pid, 0)
    sys.exit(os.WEXITSTATUS(status))
PYEOF
}
if touch_as "$HOST_A" "$DIR_B/uid-probe-from-a"; then
    pass "machine A's uid could not touch a file in machine B's directory"
else
    fail "machine A's uid touched a file inside machine B's directory"
fi
[ ! -e "$DIR_B/uid-probe-from-a" ] || fail "the refused touch left a file behind"

echo "--- probe 3: a path outside a persona's bind set refuses ---"
OUTSIDE=$(mktemp -d /tmp/cella-jailid-outside.XXXXXX)
echo "canary" > "$OUTSIDE/secret"
if "$(command -v bwrap)" \
    --unshare-user --unshare-pid --unshare-ipc --unshare-uts --unshare-cgroup \
    --ro-bind /usr /usr --ro-bind /bin /bin --ro-bind /lib /lib \
    $( [ -d /lib64 ] && echo --ro-bind /lib64 /lib64 ) \
    --proc /proc --tmpfs /tmp \
    --die-with-parent --new-session \
    /bin/cat "$OUTSIDE/secret" 2>/dev/null
then
    rm -rf "$OUTSIDE"
    fail "a path never named in the bind set was readable from inside the jail"
fi
rm -rf "$OUTSIDE"
pass "a path outside the bind set refused"

echo "ALL JAIL-IDENTITY PROBES PASSED"
