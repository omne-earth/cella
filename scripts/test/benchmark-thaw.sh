#!/usr/bin/env bash
# benchmark-thaw: the thaw warm rate, measured, three regimes.
#
# The warm (crates/cella-vmm/src/warm.rs) touches every guest page
# at thaw; its rate decides what an unavoidable freeze costs the
# world. This benchmark reports the rate -- it asserts nothing and
# always exits 0 with numbers, because the honest number depends
# on the host (page cache, storage, neighbors):
#   hot        freeze then thaw at once: the fault machinery alone
#   cold       the RAM image evicted from the page cache first
#   concurrent N cold machines thawing at the same time -- the
#              neighbor-contention regime a busy harness lives in
#
# The sandbox must live on a real filesystem: on tmpfs the RAM
# image IS page cache and eviction means nothing. The default is a
# scratch dir under $HOME; CELLA_BENCH_DIR overrides.
set -uo pipefail

cd "$(dirname "$0")/../.."
BIN=target/lab/cella
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build-lab"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0

MEM_MB="${BENCH_MEM_MB:-2048}"
N="${BENCH_MACHINES:-4}"
REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME="${CELLA_BENCH_DIR:-$(mktemp -d "$HOME/.cella-bench.XXXXXX")}"
case "$(findmnt -n -o FSTYPE --target "$CELLA_HOME")" in
tmpfs|ramfs) echo "SKIP: $CELLA_HOME is memory-backed -- eviction would lie"; rm -rf "$CELLA_HOME"; exit 0 ;;
esac
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

MACHINES=$(seq -f "bench%.0f" 1 "$N")
teardown() {
    for m in $MACHINES; do
        "$BIN" stop "$m" >/dev/null 2>&1 || true
        "$BIN" destroy "$m" >/dev/null 2>&1 || true
    done
    rm -rf "$CELLA_HOME"
}
trap teardown EXIT

evict() { # <machine>...  drop the RAM images from the page cache
    sync
    for m in "$@"; do
        python3 - "$CELLA_HOME/machines/$m/ram.img" <<'PYEOF'
import os, sys
fd = os.open(sys.argv[1], os.O_RDONLY)
os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
os.close(fd)
PYEOF
    done
}

report() { # <regime> <machine>  parse the newest warm line
    grep -a "warm(stage-2" "$CELLA_HOME/machines/$2/vmm.log" | tail -1 | \
    awk -v r="$1" -v m="$2" '{
        pages=$7; ns=$10;
        mb = pages * 4096 / 1048576;
        printf "benchmark-thaw: %-10s %-8s pages=%s secs=%.3f rate=%.0f MB/s\n", \
            r, m, pages, ns/1e9, mb / (ns/1e9);
    }'
}

echo "==> benchmark-thaw: $N machine(s), ${MEM_MB} MB each, home $CELLA_HOME ($(findmnt -n -o FSTYPE --target "$CELLA_HOME"))"
for m in $MACHINES; do
    "$BIN" create "$m" --mem-mb "$MEM_MB" >/dev/null || { echo "SKIP: create refused"; exit 0; }
    "$BIN" start "$m" >/dev/null || { echo "SKIP: start refused"; exit 0; }
done
sleep 8   # let the guests boot and touch their pages

# hot: freeze writes the pages, the thaw reads them straight back.
"$BIN" freeze bench1 >/dev/null && "$BIN" thaw bench1 >/dev/null
report hot bench1

# cold: the same machine, its image evicted first.
"$BIN" freeze bench1 >/dev/null && evict bench1 && "$BIN" thaw bench1 >/dev/null
report cold bench1

# concurrent: every machine frozen, every image evicted, every
# thaw at once -- the neighbors fight for the same device.
for m in $MACHINES; do "$BIN" freeze "$m" >/dev/null; done
# shellcheck disable=SC2086
evict $MACHINES
T0=$(date +%s.%N)
for m in $MACHINES; do "$BIN" thaw "$m" >/dev/null & done
wait
T1=$(date +%s.%N)
for m in $MACHINES; do report concurrent "$m"; done
awk -v t0="$T0" -v t1="$T1" -v n="$N" -v mb="$MEM_MB" \
    'BEGIN { w=t1-t0; printf "benchmark-thaw: concurrent wall %.3f s for %d machines (%.0f MB/s aggregate)\n", w, n, n*mb/w }'

echo "benchmark-thaw: done (a report, not a verdict -- no PASS, no FAIL)"
