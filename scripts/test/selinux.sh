#!/usr/bin/env bash
# smoke-selinux (lane c's own gate, 1.6.14c): the enforced policy
# denies lateral movement between machine directories, and the
# denial stands in the audit log. Needs root (semodule,
# setenforce, runcon under a transitioned MCS category) -- if this
# script is not root, it names the exact privileged steps and exits
# without asserting anything (a permissive or unloaded host is a
# FAIL of this gate, never silently accepted as a variant, so this
# script does not claim PASS in that case -- it states what remains).
set -uo pipefail

cd "$(dirname "$0")/../.."
BIN=target/smoke/cella
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build-smoke"; exit 0; }

if ! command -v semodule >/dev/null || ! command -v runcon >/dev/null; then
    echo "SKIP: semodule or runcon not on PATH -- install policycoreutils"
    exit 0
fi

if [ "$(id -u)" -ne 0 ]; then
    cat <<EOF
NEEDS PRIVILEGE: this gate loads and enforces a real SELinux policy
and transitions a process's MCS category with runcon -- it cannot
prove anything as a normal user. Run:

    sudo scripts/test/selinux.sh

What this proves once run as root:
  1. scripts/selinux-build.sh loads security/selinux/cella-base.cil
     and every security/profiles/*/selinux.cil, then forces
     enforcing (setenforce 1).
  2. Two machine directories get distinct MCS categories via
     scripts/selinux-machine-label.sh.
  3. A process transitioned into cella_vmm_t at one machine's
     category is denied touching a path inside the other machine's
     directory -- the AVC is asserted, not assumed.
  4. ausearch shows that denial, naming cella_vmm_t against
     cella_machine_data_t (the harvest verb is retired; the gate
     reads the audit log itself, as root).
EOF
    exit 1
fi

echo "--- selinux gate: load and enforce the policy ---"
scripts/selinux-build.sh || { echo "FAIL: policy failed to build/load"; exit 1; }
[ "$(getenforce)" = "Enforcing" ] || { echo "FAIL: host is not Enforcing -- a permissive host is a FAIL of this gate, not a variant"; exit 1; }
echo "PASS: policy loaded, host is Enforcing"

echo "--- selinux gate: two machines, two categories ---"
# This gate needs root, so $HOME is root's unless the invoking user's
# home is resolved explicitly -- the goldens live under the invoking
# user's ~/.cella, not root's.
INVOKING_HOME="$(getent passwd "${SUDO_USER:-$USER}" | cut -d: -f6)"
REAL_HOME="${CELLA_HOME:-$INVOKING_HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-selinux.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/" 2>/dev/null || true
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/" 2>/dev/null || true
teardown() {
    rm -rf "$CELLA_HOME"
    semodule -r cella-profile-cella cella-profile-cella-build cella-profile-cella-doctor \
        cella-profile-cella-gateway cella-profile-cella-machine cella-profile-cella-network \
        cella-profile-cella-probe cella-profile-cella-universe cella-profile-cella-vmm \
        cella-base >/dev/null 2>&1 || true
}
trap teardown EXIT

"$BIN" create alpha --net none >/dev/null 2>&1
"$BIN" create beta --net none >/dev/null 2>&1
A="$CELLA_HOME/machines/alpha"
B="$CELLA_HOME/machines/beta"
[ -d "$A" ] && [ -d "$B" ] || { echo "FAIL: create did not produce both machine directories"; exit 1; }

# The gate's own CELLA_HOME stands in for ~/.cella: label the whole
# tree cella_home_t first (search-only, matching a real install), so
# a machine-scoped domain can path-traverse down to its own
# directory; the per-machine chcon below then overrides just the two
# machine subtrees to cella_machine_data_t, each at its own category.
chcon -R -t cella_home_t "$CELLA_HOME"
scripts/selinux-machine-label.sh "$A" 0 >/dev/null
scripts/selinux-machine-label.sh "$B" 1 >/dev/null
echo "PASS: alpha at c0, beta at c1"

echo "--- selinux gate: the negative -- a cross-machine touch denies ---"
# runcon needs something cella_vmm_t may legitimately enter -- a
# stand-in for the real cella-vmm binary, since the policy grants
# cella_vmm_t entrypoint on cella_vmm_exec_t alone, on purpose (the
# run loop's shrunk allowlist, not an open door onto coreutils).
PROBE="$CELLA_HOME/probe-touch"
cp /usr/bin/touch "$PROBE"
chcon -t cella_vmm_exec_t "$PROBE"
TARGET="$B/pwned-from-alpha"
# cella_vmm_t has no write permission on any type outside its own
# machine's category, so stdout/stderr redirected to plain files
# would themselves be denied and muddy the result -- send them to
# /dev/null (a well-known device every domain already reaches) and
# assert on exit status and the target's absence instead.
if runcon unconfined_u:system_r:cella_vmm_t:s0:c0 "$PROBE" "$TARGET" >/dev/null 2>&1; then
    echo "FAIL: a process at alpha's category (c0) touched beta's directory (c1)"
    exit 1
fi
[ ! -e "$TARGET" ] || { echo "FAIL: the file exists despite the denied touch"; exit 1; }
echo "PASS: the AVC denied cella_vmm_t:c0 -> beta's cella_machine_data_t:c1 (verified below via the audit book)"

echo "--- selinux gate: the audit log holds the denial ---"
command -v ausearch >/dev/null || { echo "FAIL: ausearch not on PATH -- install audit"; exit 1; }
AVC=$(ausearch -m avc -ts recent 2>/dev/null | grep "avc:.*denied.*cella_vmm_t.*cella_machine_data_t" || true)
[ -n "$AVC" ] || { echo "FAIL: ausearch shows no denial of cella_vmm_t against cella_machine_data_t"; exit 1; }
echo "  $(echo "$AVC" | tail -1)"
echo "PASS: the audit log names cella_vmm_t against cella_machine_data_t"

echo
echo "ALL SELINUX GATE PROBES PASSED"
