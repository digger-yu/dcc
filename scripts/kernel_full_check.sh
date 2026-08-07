#!/usr/bin/env bash
# 内核全树非阻塞验收：modpost / prepare0 / bzImage / QEMU / vmlinux
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
KERNEL_DIR="${KERNEL_DIR:-$HOME/linux-7.1}"
DCC="${DCC:-$HOME/dcc-agent-linux66/target/release/dcc}"
BUILD_BZIMAGE="${BUILD_BZIMAGE:-0}"
BUILD_VMLINUX="${BUILD_VMLINUX:-0}"

echo "== DCC kernel full-tree check =="
echo "kernel: $KERNEL_DIR"
echo "dcc:    $DCC"

bash "$SCRIPT_DIR/modpost_link.sh"
bash "$SCRIPT_DIR/prepare0_full.sh"

if [[ "$BUILD_VMLINUX" == "1" || "$BUILD_BZIMAGE" == "1" ]]; then
    BUILD_VMLINUX="$BUILD_VMLINUX" BUILD_BZIMAGE="$BUILD_BZIMAGE" \
        bash "$SCRIPT_DIR/kernel_build_smoke.sh"
else
    echo "SKIP: vmlinux/bzImage build (set BUILD_VMLINUX=1 or BUILD_BZIMAGE=1)"
fi

KERNEL="$KERNEL_DIR/arch/x86/boot/bzImage" DCC="$DCC" \
    bash "$SCRIPT_DIR/qemu_boot_matrix.sh"
VMLINUX="$KERNEL_DIR/vmlinux" bash "$SCRIPT_DIR/vmlinux_regress.sh"

echo "PASS: kernel full-tree check completed"
