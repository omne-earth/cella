#!/usr/bin/env bash
# tls-terminator-t8: the field verifier. The strict stack that
# actually tripped in the field was Python's ssl module under
# VERIFY_X509_STRICT (default-on since 3.13): it raised
# X509_V_ERR_MISSING_AUTHORITY_KEY_IDENTIFIER against a bare
# minted leaf while curl and default OpenSSL forgave it. This gate
# replays that exact stack against the proxy's served chain --
# hostname verification included, the member's true view.
set -uo pipefail

cd "$(dirname "$0")/../.."
BIN=target/lab/cella-terminator
[ -f "$BIN" ] || { echo "SKIP: $BIN not built -- run: make build-lab"; exit 0; }
command -v python3 >/dev/null || { echo "SKIP: no python3 on this host"; exit 0; }

say() { echo; echo "==> $1"; }
D=$(mktemp -d /tmp/cella-tlspy.XXXXXX)
PORT=$(( (RANDOM % 8976) + 1024 ))
DNS_PORT=$(( (RANDOM % 8976) + 1024 ))
PROXY_PID=""
teardown() {
    [ -n "$PROXY_PID" ] && kill "$PROXY_PID" 2>/dev/null
    rm -rf "$D"
}
trap teardown EXIT

say "t8: mint a pair and stand the proxy on loopback"
"$BIN" --mint-pair-ca strict-field "$D" || { echo "FAIL: the mint door refused"; exit 1; }
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

say "t8: python ssl, VERIFY_X509_STRICT, hostname on"
timeout 20 python3 - "$PORT" "$D/ca.pem" <<'PYEOF' \
    || { echo "FAIL: the field verifier refused the minted leaf"; exit 1; }
import socket, ssl, sys
port, ca = int(sys.argv[1]), sys.argv[2]
ctx = ssl.create_default_context(cafile=ca)
ctx.verify_flags |= ssl.VERIFY_X509_STRICT
with socket.create_connection(("127.0.0.1", port), timeout=10) as s:
    with ctx.wrap_socket(s, server_hostname="strict.test") as t:
        print("strict-ok", t.version())
PYEOF

echo "  the stack that tripped in the field now verifies the mint"
echo; echo "PASS: t8 -- the field verifier"
