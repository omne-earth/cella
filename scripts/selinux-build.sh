#!/usr/bin/env bash
# Builds and loads the enforced cella SELinux policy: the base
# module plus every persona's selinux.cil, installed as one
# transaction so cross-file type references (cella_vmm_t named from
# cella-machine's profile, etc.) resolve. Then forces enforcing.
#
# Privileged: semodule and setenforce both need root. Run with sudo.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$HERE/.."

if [ "$(id -u)" -ne 0 ]; then
    echo "selinux-build: needs root (semodule, setenforce) -- run: sudo $0" >&2
    exit 1
fi

if ! command -v semodule >/dev/null; then
    echo "selinux-build: semodule not found -- install policycoreutils" >&2
    exit 1
fi

# semodule names each module after its file's basename (minus
# extension) -- every persona's source is literally selinux.cil, so
# installing them as-is would collide, each overwriting the last.
# Stage them under unique names first.
STAGE="$(mktemp -d /tmp/cella-selinux-build.XXXXXX)"
trap 'rm -rf "$STAGE"' EXIT
cp "$ROOT/security/selinux/cella-base.cil" "$STAGE/cella-base.cil"
MODULES=("$STAGE/cella-base.cil")
for d in "$ROOT"/security/profiles/*/; do
    f="$d/selinux.cil"
    [ -f "$f" ] || continue
    persona="$(basename "$d")"
    cp "$f" "$STAGE/cella-profile-$persona.cil"
    MODULES+=("$STAGE/cella-profile-$persona.cil")
done

echo "selinux-build: installing ${#MODULES[@]} module(s)"
semodule -i "${MODULES[@]}"

echo "selinux-build: forcing enforcing (no permissive-forever dev exception)"
setenforce 1
getenforce
