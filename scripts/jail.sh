#!/usr/bin/env bash
# Rootless jail for cella, using bubblewrap instead of Firecracker's
# jailer (which requires starting as root). Namespaces cost nothing here
# -- CAP_NET_ADMIN was already spent once, out of band, in `sudo cella setup net`.
#
# Usage:
#   scripts/jail.sh --state-dir ./vm1 --disk ./rootfs.img --tap tap0 \
#       --kernel ./vmlinux --mem-mb 256
#
# Everything after the script name is forwarded to cella unchanged.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="${CELLA_BIN:-$HERE/../target/release/cella}"

if [ ! -x "$BIN" ]; then
    echo "cella binary not found at $BIN (build with: cargo build --release)" >&2
    exit 1
fi
if [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
    echo "no rw access to /dev/kvm -- on Fedora this is usually 0666 already;" \
         "otherwise: sudo setfacl -m u:\$USER:rw /dev/kvm" >&2
    exit 1
fi

# Extract --state-dir/--disk/--kernel so we can bind-mount exactly those
# paths and nothing else on the host filesystem. A crude scan rather than
# a real arg parser: good enough for a wrapper script whose whole job is
# picking bind-mount targets before handing off to cella.
STATE_DIR=""
DISK=""
KERNEL=""
args=("$@")
for i in "${!args[@]}"; do
    case "${args[$i]}" in
        --state-dir) STATE_DIR="${args[$((i+1))]}" ;;
        --disk) DISK="${args[$((i+1))]}" ;;
        --kernel) KERNEL="${args[$((i+1))]}" ;;
    esac
done
[ -n "$STATE_DIR" ] || { echo "missing --state-dir" >&2; exit 1; }
[ -n "$DISK" ] || { echo "missing --disk" >&2; exit 1; }
mkdir -p "$STATE_DIR"

BIND_ARGS=(
    --ro-bind "$BIN" "/cella-vmm"
    --ro-bind /lib /lib
    --ro-bind /usr/lib /usr/lib
    --dev-bind /dev/kvm /dev/kvm
    --bind "$STATE_DIR" "$STATE_DIR"
    --bind "$(dirname "$DISK")" "$(dirname "$DISK")"
)
if [ -d /lib64 ]; then
    BIND_ARGS+=(--ro-bind /lib64 /lib64)
fi
if [ -n "$KERNEL" ]; then
    BIND_ARGS+=(--ro-bind "$(dirname "$KERNEL")" "$(dirname "$KERNEL")")
fi

exec bwrap \
    --unshare-user --unshare-pid --unshare-ipc --unshare-uts --unshare-cgroup \
    "${BIND_ARGS[@]}" \
    --proc /proc \
    --tmpfs /tmp \
    --die-with-parent \
    --new-session \
    /cella-vmm "$@"
