#!/usr/bin/env bash
# smoke-membrane-memory: the membrane's standing memory (N.F.7,
# docs/NETWORK-MODEL.md "The membrane's memory"). One MembraneMemory
# entry per destination, written at the engine seam alone, read on
# the kick. A memory affects freezing, never crossing.
#
# One criterion per invocation, the device-state pattern:
#   mm1  the live park: skip_freeze holds the machine running
#        through an egress park, and the decision applies live
#   mm2  isolation: an un-remembered destination still freezes
#   mm3  self-expiry: keep_open lapses, the next park freezes
#   mm4  the live refusal: instant error, no churn, the reason
#        lands in the Lapsed record
#   mm5  the door: the memory file lands from the engine seam on
#        the kick, and the write is witnessed
#   mm6  fail-closed edges: a zero or malformed entry is inert,
#        and a thaw re-reads without resurrecting an expired memory
#
# Each gate fails until its implementation lands (tasks/
# PHASE2-security.md, 2.6).
set -uo pipefail

MM="${1:-}"
case "$MM" in
mm1|mm2|mm3|mm4|mm5|mm6) ;;
*) echo "usage: membrane-memory.sh <mm1|mm2|mm3|mm4|mm5|mm6>"; exit 2 ;;
esac

echo "FAIL ($MM): NOT IMPLEMENTED -- the membrane's memory has no implementation yet (tasks/PHASE2-security.md, 2.6)"
exit 1
