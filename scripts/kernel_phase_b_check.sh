#!/usr/bin/env bash
# 阶段 B 内核质量门禁：modpost / prepare0 / vmlinux / QEMU
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
KERNEL_DIR="${KERNEL_DIR:-$HOME/linux-6.6}"
DCC="${DCC:-$HOME/dcc-agent-linux66/target/release/dcc}"
BUILD_VMLINUX="${BUILD_VMLINUX:-0}"
BUILD_BZIMAGE="${BUILD_BZIMAGE:-0}"

echo "== DCC kernel phase-B quality gate =="
echo "kernel: $KERNEL_DIR"
echo "dcc:    $DCC"

bash "$SCRIPT_DIR/modpost_link.sh"
bash "$SCRIPT_DIR/prepare0_full.sh"

if [[ "$BUILD_VMLINUX" == "1" || "$BUILD_BZIMAGE" == "1" ]]; then
    bash "$SCRIPT_DIR/kernel_build_smoke.sh"
fi

KERNEL="$KERNEL_DIR/arch/x86/boot/bzImage" DCC="$DCC" \
    bash "$SCRIPT_DIR/qemu_boot_matrix.sh"
VMLINUX="$KERNEL_DIR/vmlinux" bash "$SCRIPT_DIR/vmlinux_regress.sh"

echo "PASS: phase-B quality gate completed"
