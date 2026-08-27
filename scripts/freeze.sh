#!/usr/bin/env bash
# Trigger a cryogenic freeze of a running cella process. The process
# writes its frozen state and exits; re-running the same cella command
# line against the same --state-dir thaws instead of booting.
#
# Usage: scripts/freeze.sh <pid>
set -euo pipefail
PID="${1:?usage: freeze.sh <cella-pid>}"
kill -USR1 "$PID"
echo "cella: sent freeze signal to pid $PID (it will exit once the state file is written)"
