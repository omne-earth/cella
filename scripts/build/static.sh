#!/usr/bin/env bash
# Builds a static cella binary. The nested rootfs needs a static
# binary, because the guest rootfs is busybox and it has no shared
# glibc. The build uses crt-static against glibc-static, because
# Fedora ships no musl std for rust. cella is pure Rust with raw
# syscalls, thus a static glibc binary has no dlopen or NSS problem.
# The build runs inside the cella-build toolbox, thus the host needs
# no rust toolchain of a specific shape. The output is
# target/x86_64-unknown-linux-gnu/release/cella.
set -euo pipefail

cd "$(dirname "$0")/../.."

if [ -z "${INSIDE_CELLA_TOOLBOX:-}" ]; then
    exec toolbox run -c cella-build env INSIDE_CELLA_TOOLBOX=1 "$0"
fi

RUSTFLAGS="-C target-feature=+crt-static" \
    cargo build --release --target x86_64-unknown-linux-gnu
BIN=target/x86_64-unknown-linux-gnu/release/cella
file "$BIN" | grep -qE "statically linked|static-pie" \
    || { echo "cella: FAIL: $BIN is not static" >&2; exit 1; }
echo "cella: static binary -> $BIN"
