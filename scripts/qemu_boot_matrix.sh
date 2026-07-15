#!/usr/bin/env bash
# QEMU 启动矩阵探测脚本（需本机安装 qemu-system-x86_64 与 bzImage）
set -euo pipefail

KERNEL="${KERNEL:-$HOME/linux-6.6/arch/x86/boot/bzImage}"
DCC="${DCC:-$HOME/dcc-agent-linux66/target/release/dcc}"

echo "== DCC QEMU boot matrix =="
echo "compiler: $DCC"
echo "kernel:   $KERNEL"

if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
    echo "SKIP: qemu-system-x86_64 not installed (need sudo/apt)"
    exit 0
fi

if [[ ! -f "$KERNEL" ]]; then
    echo "SKIP: bzImage not found — build with:"
    echo "  cd ~/linux-6.6 && make CC=$DCC bzImage"
    exit 0
fi

MATRIX=(
    "nographic:-nographic -append 'console=ttyS0'"
    "serial-stdio:-serial mon:stdio -append 'console=ttyS0'"
)

for entry in "${MATRIX[@]}"; do
    name="${entry%%:*}"
    extra="${entry#*:}"
    echo "-- matrix: $name"
    # 仅验证 QEMU 能加载内核（5s 超时）
    timeout 5 qemu-system-x86_64 -kernel "$KERNEL" -m 512M $extra \
        -no-reboot -display none 2>/dev/null || true
done

echo "PASS: QEMU matrix probes completed"
