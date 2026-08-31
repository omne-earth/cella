#!/usr/bin/env bash
# Acceptance gates for docs/DEVICE-STATE.md. One argument selects the
# criterion: ac1, ac2, ac3, or ac4. Each gate fails until its
# implementation lands; the FAIL is the specification.
set -ueo pipefail

ac="${1:-}"

case "$ac" in
ac1)
	echo "AC1: the disk survives the thaw."
	echo "  Gate: write a file, freeze, thaw, read it back, sync."
	echo "  Pass condition: make demo runs on a rw root."
	;;
ac2)
	echo "AC2: the network survives the thaw."
	echo "  Gate: the net gate, moved past a freeze; the tap claim"
	echo "  persists through the manifest."
	;;
ac3)
	echo "AC3: the in-flight layer is exact."
	echo "  Gate: a parked egress frame is delivered and completed"
	echo "  after the thaw; the same request works, no retransmission."
	;;
ac4)
	echo "AC4: the verdict is external (the world-ratchet gate)."
	echo "  Gate: every egress frame parks; the test, as the stand-in"
	echo "  engine, renders release-with-allow or freeze-grow-thaw."
	;;
*)
	echo "usage: $0 ac1|ac2|ac3|ac4" >&2
	exit 2
	;;
esac

echo "FAIL: $ac is not implemented yet (see docs/DEVICE-STATE.md)"
exit 1
