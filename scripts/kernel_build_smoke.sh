#!/usr/bin/env bash
# 可选：用 CC=dcc 构建 vmlinux / bzImage（smoke 门禁前置）
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DCC_REPO="${DCC_REPO:-$(cd "$SCRIPT_DIR/.." && pwd)}"
KERNEL_DIR="${KERNEL_DIR:-$HOME/linux-7.1}"
DCC="${DCC:-$DCC_REPO/target/release/dcc}"
WRAPPER="${WRAPPER:-$SCRIPT_DIR/dcc-host-wrapper.sh}"
JOBS="${JOBS:-$(nproc 2>/dev/null || echo 4)}"
BUILD_VMLINUX="${BUILD_VMLINUX:-0}"
BUILD_BZIMAGE="${BUILD_BZIMAGE:-0}"

echo "== DCC kernel smoke build =="
echo "kernel: $KERNEL_DIR"
echo "dcc:    $DCC"
echo "jobs:   $JOBS"

if [[ ! -d "$KERNEL_DIR" ]]; then
    echo "SKIP: kernel tree missing"
    exit 0
fi

cd "$KERNEL_DIR"

if [[ ! -f .config ]]; then
    echo "Using allnoconfig..."
    make allnoconfig
fi

# 避免 syncconfig 交互（如 CONFIG_CFI_CLANG NEW）
if [[ -f scripts/config ]]; then
    scripts/config --set-val CONFIG_CFI_CLANG n 2>/dev/null || true
    make olddefconfig
fi

if [[ ! -f include/generated/autoconf.h ]]; then
    make olddefconfig
    make -s include/generated/autoconf.h
fi

export DCC_BIN="$DCC"
export HOST_LD="${HOST_LD:-gcc}"
export DCC_REPO="$DCC_REPO"
HOSTCC="${HOSTCC:-gcc}"

# 安装 Rust fixdep（Kbuild 宿主 kconfig 等仍默认 gcc）
DCC_REPO="$DCC_REPO" bash "$SCRIPT_DIR/build_fixdep_dcc.sh"

if [[ "$BUILD_VMLINUX" == "1" ]]; then
    echo "-- make CC=dcc HOSTCC=$HOSTCC vmlinux (-j$JOBS)"
    make CC="$DCC" HOSTCC="$HOSTCC" vmlinux -j"$JOBS" 2>&1 | tail -30
    if [[ -f vmlinux ]]; then
        echo "PASS: vmlinux built"
    else
        echo "FAIL: vmlinux not produced"
        exit 1
    fi
else
    echo "SKIP: vmlinux (set BUILD_VMLINUX=1)"
fi

if [[ "$BUILD_BZIMAGE" == "1" ]]; then
    echo "-- make CC=dcc HOSTCC=$HOSTCC bzImage (-j$JOBS)"
    make CC="$DCC" HOSTCC="$HOSTCC" bzImage -j"$JOBS" 2>&1 | tail -30
    if [[ -f arch/x86/boot/bzImage ]]; then
        echo "PASS: bzImage built"
    else
        echo "FAIL: bzImage not produced"
        exit 1
    fi
else
    echo "SKIP: bzImage (set BUILD_BZIMAGE=1)"
fi

echo "PASS: kernel smoke build step done"
