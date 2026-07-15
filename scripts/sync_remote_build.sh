#!/usr/bin/env bash
# 将本地 dcc 源码同步到远程 Linux 并构建测试
set -euo pipefail

REMOTE="${REMOTE:-ds@192.168.148.128}"
REMOTE_DIR="${REMOTE_DIR:-/home/ds/dcc-agent-linux66}"
KERNEL_DIR="${KERNEL_DIR:-/home/ds/linux-6.6}"
LOCAL_DIR="$(cd "$(dirname "$0")/.." && pwd)"

echo "Syncing $LOCAL_DIR -> $REMOTE:$REMOTE_DIR"
rsync -avz --delete \
    --exclude target \
    --exclude .git \
    "$LOCAL_DIR/" "$REMOTE:$REMOTE_DIR/"

echo "Building on remote..."
ssh "$REMOTE" "source ~/.cargo/env && cd $REMOTE_DIR && cargo build --release 2>&1"

echo "Running integration tests..."
ssh "$REMOTE" "source ~/.cargo/env && cd $REMOTE_DIR && cargo test --release 2>&1"

echo "Running remote feature checks..."
ssh "$REMOTE" "source ~/.cargo/env && cd $REMOTE_DIR && bash scripts/remote_feature_check.sh 2>&1"

echo "Running kernel feature examples..."
ssh "$REMOTE" "source ~/.cargo/env && cd $REMOTE_DIR && bash scripts/test_kernel_features.sh 2>&1"

echo "Testing devicetable-offsets.s and kernel/bounds.s..."
ssh "$REMOTE" bash -s <<REMOTE_SCRIPT
set -euo pipefail
KERNEL_DIR="${KERNEL_DIR}"
REMOTE_DIR="${REMOTE_DIR}"
cd "\$KERNEL_DIR"
DCC="\$REMOTE_DIR/target/release/dcc"

if [ ! -f include/generated/autoconf.h ]; then
    echo "include/generated/autoconf.h missing; running make olddefconfig + autoconf.h (gcc)..."
    if [ ! -f .config ]; then
        make olddefconfig
    fi
    make include/generated/autoconf.h
fi

\$DCC -nostdinc \\
    -I./arch/x86/include -I./arch/x86/include/generated \\
    -I./include -I./arch/x86/include/uapi \\
    -I./arch/x86/include/generated/uapi \\
    -I./include/uapi -I./include/generated/uapi \\
    -include ./include/linux/compiler-version.h \\
    -include ./include/linux/kconfig.h \\
    -include ./include/linux/compiler_types.h \\
    -D__KERNEL__ -std=gnu11 -S \\
    -o /tmp/devicetable-offsets.s scripts/mod/devicetable-offsets.c
grep -c 'SIZE_' /tmp/devicetable-offsets.s
echo "devicetable-offsets.s OK"

\$DCC -nostdinc \\
    -I./arch/x86/include -I./arch/x86/include/generated \\
    -I./include -I./arch/x86/include/uapi \\
    -I./arch/x86/include/generated/uapi \\
    -I./include/uapi -I./include/generated/uapi \\
    -include ./include/linux/compiler-version.h \\
    -include ./include/linux/kconfig.h \\
    -include ./include/linux/compiler_types.h \\
    -D__KERNEL__ -std=gnu11 -S \\
    -o /tmp/bounds.s kernel/bounds.c
echo "bounds.s OK"
REMOTE_SCRIPT

echo "All remote checks passed."
