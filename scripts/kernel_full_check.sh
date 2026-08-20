#!/usr/bin/env bash
# 内核全树验收：modpost / prepare0 / bzImage / QEMU / vmlinux
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DCC_REPO="${DCC_REPO:-$(cd "$SCRIPT_DIR/.." && pwd)}"
KERNEL_DIR="${KERNEL_DIR:-$HOME/linux-7.1}"
DCC="${DCC:-$DCC_REPO/target/release/dcc}"
STRICT_FULL_BUILD="${STRICT_FULL_BUILD:-0}"

if [[ "$STRICT_FULL_BUILD" == "1" ]]; then
    BUILD_BZIMAGE="1"
    BUILD_VMLINUX="1"
    REQUIRE_BZIMAGE="1"
    REQUIRE_VMLINUX="1"
    REQUIRE_KERNEL_DIR="1"
    REQUIRE_PAHOLE="1"
    REQUIRE_OBJTOOL="1"
    REQUIRE_QEMU_BOOT="1"
    STRICT_MODPOST="1"
    export BUILD_BZIMAGE BUILD_VMLINUX REQUIRE_BZIMAGE REQUIRE_VMLINUX
    export REQUIRE_KERNEL_DIR REQUIRE_PAHOLE REQUIRE_OBJTOOL REQUIRE_QEMU_BOOT STRICT_MODPOST
else
    BUILD_BZIMAGE="${BUILD_BZIMAGE:-0}"
    BUILD_VMLINUX="${BUILD_VMLINUX:-0}"
fi

export DCC_REPO KERNEL_DIR DCC

echo "== DCC kernel full-tree check =="
echo "kernel: $KERNEL_DIR"
echo "dcc:    $DCC"
echo "strict: $STRICT_FULL_BUILD"

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
