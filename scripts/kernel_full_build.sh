#!/usr/bin/env bash
# 内核全树 make CC=dcc 首跑（-k 继续至收集全部失败）
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DCC_REPO="${DCC_REPO:-$(cd "$SCRIPT_DIR/.." && pwd)}"
KERNEL_DIR="${KERNEL_DIR:-$HOME/linux-7.1}"
DCC="${DCC:-$DCC_REPO/target/release/dcc}"
LOG="${LOG:-$DCC_REPO/build.log}"
JOBS="${JOBS:-$(nproc 2>/dev/null || echo 8)}"

cd "$KERNEL_DIR"

# 避免 syncconfig 交互阻塞（CONFIG_CFI_CLANG 等新选项）
if [[ -f scripts/config ]]; then
    scripts/config --disable CFI_CLANG 2>/dev/null || true
    scripts/config --disable CFI 2>/dev/null || true
    yes '' | make CC="$DCC" HOSTCC=gcc oldconfig >/dev/null || true
    make CC="$DCC" HOSTCC=gcc olddefconfig
    make CC="$DCC" HOSTCC=gcc include/generated/autoconf.h
fi

# 恢复 syncconfig 误删的 autoconf（若存在 .config）
if [[ -f .config && ! -f include/generated/autoconf.h ]]; then
    echo "Restoring autoconf.h via make olddefconfig..."
    make olddefconfig
    make include/generated/autoconf.h
fi

echo "== kernel full build: CC=$DCC HOSTCC=gcc -j$JOBS =="
make CC="$DCC" HOSTCC=gcc -k V=1 -j"$JOBS" 2>&1 | tee "$LOG"
echo "EXIT_CODE=${PIPESTATUS[0]}"
