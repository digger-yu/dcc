#!/usr/bin/env bash
# 构建最小 initramfs：busybox (static) + /init shell 脚本
# 产物：$DCC_REPO/initramfs/initramfs.cpio.gz
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DCC_REPO="${DCC_REPO:-$(cd "$SCRIPT_DIR/.." && pwd)}"
BUSYBOX="${BUSYBOX:-/usr/bin/busybox}"
OUTPUT_DIR="$DCC_REPO/initramfs"
ROOTFS="$OUTPUT_DIR/rootfs"
OUTPUT="$OUTPUT_DIR/initramfs.cpio.gz"

echo "== DCC initramfs builder =="
echo "busybox: $BUSYBOX"
echo "output:  $OUTPUT"

# --- 前置检查 -----------------------------------------------------------
if [[ ! -x "$BUSYBOX" ]]; then
    echo "FAIL: busybox not found at $BUSYBOX"
    exit 1
fi

if ! file "$BUSYBOX" | grep -q 'statically linked'; then
    echo "WARN: busybox is not statically linked — init may fail"
fi

for tool in cpio gzip; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "FAIL: $tool not found"
        exit 1
    fi
done

# --- 清理并创建目录结构 --------------------------------------------------
rm -rf "$ROOTFS"
mkdir -p "$ROOTFS"/{bin,dev,proc,sys,etc}

# --- 复制 busybox --------------------------------------------------------
cp "$BUSYBOX" "$ROOTFS/bin/busybox"
chmod 755 "$ROOTFS/bin/busybox"

# --- 创建符号链接 --------------------------------------------------------
ln -sf busybox "$ROOTFS/bin/sh"

# --- 生成 /init 脚本 -----------------------------------------------------
cat > "$ROOTFS/init" << 'INITEOF'
#!/bin/sh
# DCC minimal initramfs init — 验证内核成功到达用户空间
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev 2>/dev/null || true

echo "=== DCC kernel: init reached successfully ==="
echo "=== Boot to init complete ==="

# 关闭系统（QEMU -no-reboot 会将 poweroff 转为退出）
poweroff -f
INITEOF
chmod 755 "$ROOTFS/init"

# --- 打包为 cpio.gz ------------------------------------------------------
cd "$ROOTFS"
find . | cpio -H newc -o --quiet | gzip -9 > "$OUTPUT"

# --- 验证产物 ------------------------------------------------------------
size=$(stat -c%s "$OUTPUT" 2>/dev/null || stat -f%z "$OUTPUT" 2>/dev/null)
echo "PASS: initramfs built ($size bytes): $OUTPUT"

# 列出内容供调试
echo "-- initramfs contents:"
cd "$ROOTFS"
find . -print | sort | sed 's|^|  |'
