#!/usr/bin/env bash
# smoke-extract: evidence leaves a still machine as a faithful tar.
# The gate runs the field binary: extract needs no console, and the
# dark flavor is the verb's home. It proves: a single file extracts
# and matches the golden source byte for byte; numeric uid/gid and
# modes survive; hardlinks ride as links; the whole tree extracts;
# a running machine refuses the verb; and the read is witnessed in
# the machine's audit book.
set -uo pipefail

cd "$(dirname "$0")/../.."
BIN=target/release/cella
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build"; exit 0; }
"$BIN" doctor gate kvm bwrap golden:kernel:canonical golden:rootfs:cella || exit 0

REAL_HOME="${CELLA_HOME:-$HOME/.cella}"
export CELLA_HOME=$(mktemp -d /tmp/cella-extract.XXXXXX)
mkdir -p "$CELLA_HOME/kernel/canonical" "$CELLA_HOME/rootfs/cella"
cp "$REAL_HOME/kernel/canonical/bzImage" "$CELLA_HOME/kernel/canonical/"
cp "$REAL_HOME/rootfs/cella/rootfs.ext4" "$CELLA_HOME/rootfs/cella/"

VM=vault
M="$CELLA_HOME/machines/$VM"
OUT=$(mktemp -d /tmp/cella-extract-out.XXXXXX)
teardown() {
    "$BIN" stop "$VM" >/dev/null 2>&1 || true
    for m in "$VM" "$VM-extractor"; do
        p=$(cat "$CELLA_HOME/machines/$m/pid" 2>/dev/null || true)
        [ -n "$p" ] && kill -9 "$p" 2>/dev/null || true
    done
    rm -rf "$CELLA_HOME" "$OUT"
}
trap teardown EXIT
say() { echo; echo "==> $1"; }

say "step 1: a single file extracts, byte for byte"
"$BIN" create "$VM" >/dev/null || { echo "FAIL: create"; exit 1; }
"$BIN" extract "$VM" /sbin/init > "$OUT/init.tar" \
    || { echo "FAIL: extract /sbin/init returned nonzero"; exit 1; }
tar -tf "$OUT/init.tar" > "$OUT/init.list"
grep -q "sbin/init" "$OUT/init.list" \
    || { echo "FAIL: the tar does not list sbin/init"; exit 1; }
tar -xf "$OUT/init.tar" -C "$OUT" || { echo "FAIL: the tar does not extract"; exit 1; }
# The machine never started: its disk is the golden byte for byte,
# and /sbin/init in the golden is the init script the build baked.
cmp -s "$OUT/sbin/init" scripts/build/rootfs-cella.sh \
    || { echo "FAIL: the extracted init differs from the baked source"; exit 1; }
echo "  /sbin/init left the still disk and matches the baked source"

say "step 2: numeric uid/gid and modes survive"
# The golden is built by mkfs.ext4 -d as the unprivileged builder,
# thus its files carry the builder's uid/gid -- the tar must report
# exactly that, numerically (no name mapping anywhere).
LISTING=$(tar --numeric-owner -tvf "$OUT/init.tar" | grep "sbin/init")
echo "$LISTING" | grep -q " $(id -u)/$(id -g) " \
    || { echo "FAIL: ownership did not survive (want $(id -u)/$(id -g)): $LISTING"; exit 1; }
echo "$LISTING" | grep -q "rwxr-xr-x" \
    || { echo "FAIL: the init's mode did not survive: $LISTING"; exit 1; }
echo "  uid/gid and mode ride the tar"

say "step 3: links ride as links"
# busybox --install makes the applets hardlinks; the tar must record
# them as links to one stored copy, not as hundreds of full copies.
"$BIN" extract "$VM" /bin > "$OUT/bin.tar" \
    || { echo "FAIL: extract /bin returned nonzero"; exit 1; }
tar -tvf "$OUT/bin.tar" > "$OUT/bin.list"
grep -q "link to" "$OUT/bin.list" \
    || { echo "FAIL: no applet hardlink survived as a link"; exit 1; }
echo "  the applet hardlinks survived as links"

say "step 4: the whole tree extracts"
"$BIN" extract "$VM" / > "$OUT/all.tar" \
    || { echo "FAIL: extract / returned nonzero"; exit 1; }
tar -tf "$OUT/all.tar" > "$OUT/all.list"
grep -q "sbin/init" "$OUT/all.list" \
    || { echo "FAIL: the full tree misses sbin/init"; exit 1; }
grep -q "bin/busybox" "$OUT/all.list" \
    || { echo "FAIL: the full tree misses bin/busybox"; exit 1; }
echo "  / left as one tar ($(wc -c < "$OUT/all.tar") bytes)"

say "step 5: a nonexistent path fails loudly, with no tar"
"$BIN" extract "$VM" /no/such/path > "$OUT/none.tar" 2>"$OUT/none.err" \
    && { echo "FAIL: a nonexistent path did not fail"; exit 1; }
[ -s "$OUT/none.tar" ] && { echo "FAIL: a failed extract still wrote bytes"; exit 1; }
grep -q "no such path" "$OUT/none.err" \
    || { echo "FAIL: the failure does not name the reason: $(cat "$OUT/none.err")"; exit 1; }
echo "  no bytes, and the guest's reason on stderr"

say "step 6: a running machine refuses the verb"
"$BIN" start "$VM" >/dev/null || { echo "FAIL: start"; exit 1; }
"$BIN" extract "$VM" /sbin/init >/dev/null 2>&1 \
    && { echo "FAIL: a running machine accepted extract"; exit 1; }
"$BIN" stop "$VM" >/dev/null
echo "  running is the one refusal, held"

say "step 7: the reads are witnessed in the machine's book"
COUNT=$("$BIN" --dump-ledger "$M/audit" 2>/dev/null | grep -c "verb=extract")
[ "$COUNT" -ge 4 ] \
    || { echo "FAIL: expected >=4 extract entries in the audit book, found $COUNT"; exit 1; }
echo "  $COUNT extract entries stand in the book"

echo
echo "PASS: extract -- evidence leaves as a faithful tar, refusals hold, the book records the reads"
