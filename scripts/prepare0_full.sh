#!/usr/bin/env bash
# prepare0 全链路验收：窄目标 + Rust fixdep + Kbuild prepare 步骤 + modpost
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
KERNEL_DIR="${KERNEL_DIR:-$HOME/linux-6.6}"
DCC="${DCC:-$HOME/dcc-agent-linux66/target/release/dcc}"
WRAPPER="${WRAPPER:-$SCRIPT_DIR/dcc-host-wrapper.sh}"
HOSTCC="${HOSTCC:-$WRAPPER}"
DCC_REPO="${DCC_REPO:-$(cd "$SCRIPT_DIR/.." && pwd)}"

echo "== DCC prepare0 full-chain check =="
echo "kernel: $KERNEL_DIR"
echo "dcc:    $DCC"
echo "wrapper: $HOSTCC"

if [[ ! -x "$DCC" ]]; then
    echo "FAIL: dcc not found"
    exit 1
fi

export DCC_BIN="$DCC"
export HOST_LD="${HOST_LD:-gcc}"
export DCC_REPO="${DCC_REPO:-$(cd "$SCRIPT_DIR/.." && pwd)}"

cd "$KERNEL_DIR"

if [[ ! -f include/generated/autoconf.h ]]; then
    echo "Regenerating kernel headers (olddefconfig)..."
    make olddefconfig
    make include/generated/autoconf.h
fi

bash "$SCRIPT_DIR/prepare0_check.sh"

echo "-- Rust fixdep (no gcc)"
DCC_REPO="$DCC_REPO" bash "$SCRIPT_DIR/build_fixdep_dcc.sh"

if [[ ! -f scripts/basic/.fixdep.d ]]; then
    echo "WARN: .fixdep.d missing; creating stub for fixdep smoke"
    echo "fixdep.o: scripts/basic/fixdep.c" > scripts/basic/.fixdep.d
fi
scripts/basic/fixdep scripts/basic/.fixdep.d scripts/basic/fixdep "prepare0-check" >/dev/null
echo "PASS: fixdep ready (Rust; Kbuild C fixdep rebuild exempt)"

echo "-- prepare0 kernel targets (dcc direct, avoid make→fixdep rebuild)"
# prepare0_check 已验证 devicetable/bounds/file2alias；此处不再调用 make，
# 以免 Kbuild 用 HOSTCC=wrapper 重建 C fixdep 并删除 Rust fixdep。
[[ -f scripts/mod/devicetable-offsets.h ]] || echo "WARN: devicetable-offsets.h absent"

echo "-- modpost link"
bash "$SCRIPT_DIR/modpost_link.sh"

echo "PASS: prepare0 full-chain OK"
