#!/usr/bin/env bash
# 内核全树非阻塞验收：modpost 链接 / bzImage / QEMU 矩阵 / vmlinux 工具链
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
KERNEL_DIR="${KERNEL_DIR:-$HOME/linux-6.6}"
DCC="${DCC:-$HOME/dcc-agent-linux66/target/release/dcc}"
BUILD_BZIMAGE="${BUILD_BZIMAGE:-0}"

echo "== DCC kernel full-tree check (non-blocking) =="
echo "kernel: $KERNEL_DIR"
echo "dcc:    $DCC"

bash "$SCRIPT_DIR/modpost_link.sh"

if [[ "$BUILD_BZIMAGE" == "1" ]]; then
    echo "-- optional: make CC=dcc bzImage"
    if [[ -d "$KERNEL_DIR" ]]; then
        cd "$KERNEL_DIR"
        make CC="$DCC" bzImage 2>&1 || echo "WARN: bzImage build failed (non-blocking)"
    fi
else
    echo "SKIP: bzImage build (set BUILD_BZIMAGE=1 to enable)"
fi

KERNEL="$KERNEL_DIR/arch/x86/boot/bzImage" DCC="$DCC" bash "$SCRIPT_DIR/qemu_boot_matrix.sh"
VMLINUX="$KERNEL_DIR/vmlinux" bash "$SCRIPT_DIR/vmlinux_regress.sh"

echo "PASS: kernel full-tree check completed"
