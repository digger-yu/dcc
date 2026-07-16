#!/usr/bin/env bash
# modpost 完整链接：HOSTCC=dcc，bootstrap 工具（fixdep 等）仍用 gcc
set -euo pipefail

KERNEL_DIR="${KERNEL_DIR:-$HOME/linux-6.6}"
DCC="${DCC:-$HOME/dcc-agent-linux66/target/release/dcc}"
HOSTGCC="${HOSTGCC:-gcc}"

echo "== DCC modpost full link =="
echo "kernel: $KERNEL_DIR"
echo "dcc:    $DCC"
echo "bootstrap HOSTCC: $HOSTGCC"

if [[ ! -x "$DCC" ]]; then
    echo "FAIL: dcc not found at $DCC"
    exit 1
fi

if [[ ! -d "$KERNEL_DIR/scripts/mod" ]]; then
    echo "SKIP: kernel tree not found at $KERNEL_DIR"
    exit 0
fi

cd "$KERNEL_DIR"

if [[ ! -f include/generated/autoconf.h ]]; then
    echo "Generating include/generated/autoconf.h..."
    make -s include/generated/autoconf.h
fi

echo "-- bootstrap: scripts/basic/fixdep (HOSTCC=$HOSTGCC)"
make -C scripts/basic HOSTCC="$HOSTGCC" fixdep

echo "-- build: scripts/mod/modpost (HOSTCC=dcc)"
make -C scripts/mod HOSTCC="$DCC" HOSTLD=ld modpost

MODPOST="$KERNEL_DIR/scripts/mod/modpost"
if [[ ! -x "$MODPOST" ]]; then
    echo "FAIL: modpost binary not produced at $MODPOST"
    exit 1
fi

echo "-- smoke: modpost --version"
"$MODPOST" --version >/dev/null 2>&1 || true

echo "PASS: modpost linked with dcc HOSTCC"
