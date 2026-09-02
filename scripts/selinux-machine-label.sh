#!/usr/bin/env bash
# Labels one machine directory with its MCS category, and states the
# category-assignment contract this lane assumes lane a's spawn
# will honor.
#
# The category is a deterministic function of the machine's
# per-machine sub-uid, not of the machine name: two directories must
# never share a category, and the same machine must always get the
# same category across a stop/start cycle, so the mapping has to key
# off the same identity the sub-user binding already uses.
#
#   category = (sub_uid - CELLA_SUBUID_BASE) mod 1024
#
# ASSUMPTION (stated for the reviewer at the lane a/c merge): lane a
# allocates a stable, small per-machine sub-uid offset from a known
# base (subuid range or direct uid_map write -- lane a's report names
# which). This script does not know that offset; the caller passes it
# as $2, already computed by whatever lane a's spawn used to pick the
# sub-uid. Until lane a lands, this script is exercised directly with
# an explicit category for lane c's own gate.
#
# Usage: scripts/selinux-machine-label.sh <machine-dir> <sub-uid-offset>
set -euo pipefail

DIR="${1:?usage: selinux-machine-label.sh <machine-dir> [sub-uid-offset]}"
# Lane a landed the mechanism this lane assumed: the spawn persists
# the machine's sub-uid offset in <machine-dir>/uid. When the second
# argument is omitted, the offset comes from there -- the category
# derives from the real allocation, not from a caller's claim.
OFFSET="${2:-$(cat "$DIR/uid" 2>/dev/null || true)}"
[ -n "$OFFSET" ] || { echo "selinux-machine-label: no offset given and no $DIR/uid" >&2; exit 1; }
CATEGORY=$(( OFFSET % 1024 ))

if [ ! -d "$DIR" ]; then
    echo "selinux-machine-label: $DIR is not a directory" >&2
    exit 1
fi

chcon -R -l "s0:c${CATEGORY}" "$DIR"
chcon -R -t cella_machine_data_t "$DIR"
echo "selinux-machine-label: $DIR -> cella_machine_data_t:s0:c${CATEGORY}"
