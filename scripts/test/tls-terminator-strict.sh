#!/usr/bin/env bash
# tls-terminator-t7: the strict verifier. rustls tolerates a bare
# leaf; a member's stricter stack is entitled not to (RFC 5280).
# This gate lets openssl -- a verifier cella does not ship -- judge
# the chain the proxy actually serves: mint a pair, run the proxy
# on loopback as an ordinary user, fetch the served chain with
# s_client, and hold it to `openssl verify -x509_strict` plus the
# extensions a TLS server leaf must carry (AKI, KeyUsage, EKU).
# No VMs: the subject is the minter's bytes on a real wire.
set -uo pipefail

cd "$(dirname "$0")/../.."
BIN=target/lab/cella-terminator
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build-lab"; exit 0; }
command -v openssl >/dev/null || { echo "SKIP: no openssl on this host"; exit 0; }

say() { echo; echo "==> $1"; }
D=$(mktemp -d /tmp/cella-tlsstrict.XXXXXX)
PORT=$(( (RANDOM % 8976) + 1024 ))
DNS_PORT=$(( (RANDOM % 8976) + 1024 ))
PROXY_PID=""
teardown() {
    [ -n "$PROXY_PID" ] && kill "$PROXY_PID" 2>/dev/null
    rm -rf "$D"
}
trap teardown EXIT

say "t7: mint a pair and stand the proxy on loopback"
"$BIN" --mint-pair-ca strict-gate "$D" || { echo "FAIL: the mint door refused"; exit 1; }
printf 'wire_ip=127.0.0.1\ndns_port=%s\nupstream_dns=127.0.0.1:9\nlisten=%s\nca_cert=%s/ca.pem\nca_key=%s/ca.key\n' \
    "$DNS_PORT" "$PORT" "$D" "$D" > "$D/conf"
"$BIN" "$D/conf" > "$D/proxy.log" 2>&1 &
PROXY_PID=$!
for _ in $(seq 1 50); do
    grep -q "serving" "$D/proxy.log" 2>/dev/null && break
    kill -0 "$PROXY_PID" 2>/dev/null || { echo "FAIL: the proxy died -- $(cat "$D/proxy.log")"; exit 1; }
    sleep 0.1
done
grep -q "serving" "$D/proxy.log" || { echo "FAIL: the proxy never served"; exit 1; }

say "t7: the served chain, judged by a verifier cella does not ship"
echo | timeout 15 openssl s_client -connect "127.0.0.1:$PORT" \
    -servername strict.test -showcerts 2>/dev/null > "$D/handshake" \
    || { echo "FAIL: the handshake never completed"; exit 1; }
# Split the served chain: the first certificate is the leaf.
awk '/BEGIN CERT/{n++} n==1' "$D/handshake" > "$D/leaf.pem"
[ -s "$D/leaf.pem" ] || { echo "FAIL: no leaf in the served chain"; exit 1; }

# The strict verification itself: -x509_strict enforces the RFC 5280
# conformance a lenient stack forgives.
timeout 15 openssl verify -x509_strict -CAfile "$D/ca.pem" "$D/leaf.pem" \
    || { echo "FAIL: the strict verifier refused the minted leaf"; exit 1; }

# The extensions, by name: the AKI that chains, the usages that
# say exactly what a TLS server leaf is for.
openssl x509 -in "$D/leaf.pem" -noout -text > "$D/leaf.txt"
for want in "Authority Key Identifier" "Digital Signature" "TLS Web Server Authentication"; do
    grep -q "$want" "$D/leaf.txt" \
        || { echo "FAIL: the leaf carries no \"$want\""; exit 1; }
done
# And the SAN carries the SNI -- hostname verification's anchor.
grep -q "DNS:strict.test" "$D/leaf.txt" \
    || { echo "FAIL: the leaf's SAN does not carry the SNI"; exit 1; }

echo "  openssl -x509_strict accepts the pair's mint; AKI, KU, EKU, SAN all present"
echo; echo "PASS: t7 -- the strict verifier"
