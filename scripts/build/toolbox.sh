#!/usr/bin/env bash
# Creates and provisions the 'cella-build' toolbox: a Fedora container
# that holds the kernel/rootfs build toolchain (gcc, bison, flex,
# e2fsprogs, ...). The host itself stays free of build dependencies --
# `toolbox` (a thin podman wrapper) is the only thing
# scripts/setup/bootstrap.sh installs for this on the host.
# scripts/build/assets.sh re-execs itself inside this container to
# actually build the kernel and rootfs.
#
# Idempotent: safe to re-run. Called by `make .toolbox` (see Makefile),
# which only invokes this when the .toolbox sentinel file is missing.
set -euo pipefail

TOOLBOX=cella-build

if ! command -v toolbox &>/dev/null; then
    echo "cella: 'toolbox' not found -- run: make init" >&2
    exit 1
fi

if toolbox list -c 2>/dev/null | grep -qw "$TOOLBOX"; then
    echo "cella: toolbox '$TOOLBOX' already exists"
else
    echo "cella: creating toolbox '$TOOLBOX'"
    toolbox create -y "$TOOLBOX"
fi

echo "cella: installing the kernel/rootfs build toolchain inside '$TOOLBOX'"
toolbox run -c "$TOOLBOX" sudo dnf install -y \
    gcc make bc bison flex elfutils-libelf-devel openssl-devel \
    perl-interpreter perl-generators xz bzip2 e2fsprogs glibc-static

echo "cella: '$TOOLBOX' ready"
