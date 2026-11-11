#!/usr/bin/env bash
# 构建运行时/模块闭环用的 initramfs：
#   busybox(static) + /init(tests/runtime/init.sh) + /t /t2(gcc -static)
#   + hello.ko(dcc, out-of-tree) + configs.ko(树内 CONFIG_IKCONFIG=m)
# 产物：$DCC_REPO/initramfs/runtime.cpio.gz
#
# 用户态测试程序用宿主 gcc 编译 —— 闭环判据是"同一份 initramfs 在
# dcc 内核与 gcc 内核上输出逐字节相同"，编译器变量只允许出现在内核侧。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DCC_REPO="${DCC_REPO:-$(cd "$SCRIPT_DIR/.." && pwd)}"
BUSYBOX="${BUSYBOX:-/usr/bin/busybox}"
BUILD="${BUILD:-/root/pvh-lmc3}"
OUTPUT_DIR="$DCC_REPO/initramfs"
ROOTFS="$OUTPUT_DIR/runtime-rootfs"
OUTPUT="$OUTPUT_DIR/runtime.cpio.gz"

HELLO_KO="${HELLO_KO:-$DCC_REPO/tests/kmod/hello/hello.ko}"
CONFIGS_KO="${CONFIGS_KO:-$BUILD/kernel/configs.ko}"

echo "== DCC runtime initramfs builder =="
echo "busybox: $BUSYBOX"
echo "hello:   $HELLO_KO"
echo "configs: $CONFIGS_KO"
echo "output:  $OUTPUT"

for f in "$BUSYBOX" "$HELLO_KO" "$CONFIGS_KO" \
         "$DCC_REPO/tests/runtime/init.sh" \
         "$DCC_REPO/tests/runtime/init.c" \
         "$DCC_REPO/tests/runtime/t2.c"; do
    [[ -f "$f" ]] || { echo "FAIL: missing $f"; exit 1; }
done
for tool in cpio gzip gcc file; do
    command -v "$tool" >/dev/null 2>&1 || { echo "FAIL: $tool not found"; exit 1; }
done

rm -rf "$ROOTFS"
mkdir -p "$ROOTFS"/{bin,dev,proc,sys,mnt}

# --- 用户态测试程序（宿主 gcc，静态链接） --------------------------------
gcc -static -O2 -pthread -o "$ROOTFS/t" "$DCC_REPO/tests/runtime/init.c"
gcc -static -O2 -o "$ROOTFS/t2" "$DCC_REPO/tests/runtime/t2.c"
for f in t t2; do
    file "$ROOTFS/$f" | grep -q 'statically linked' \
        || { echo "FAIL: $f 不是静态链接"; exit 1; }
done

# --- /init 脚本 + busybox + 模块 ------------------------------------------
cp "$DCC_REPO/tests/runtime/init.sh" "$ROOTFS/init"
chmod 755 "$ROOTFS/init"
cp "$BUSYBOX" "$ROOTFS/bin/busybox"
chmod 755 "$ROOTFS/bin/busybox"
ln -sf busybox "$ROOTFS/bin/sh"
# 全部 applet 的符号链接。缺了它们时 ash 的命令解析依赖 PATH 与环境
# 的偶然组合：实测同一 busybox 在 chroot 下 mount/cat 可用、insmod 不可用，
# 在 QEMU 里却反过来（mount/cat/wc/grep 报 not found，insmod/rmmod 可用）。
# /init 依赖 mount 挂 proc/sys/devtmpfs，缺链接会让整条闭环假失败。
mkdir -p "$ROOTFS"/{sbin,usr/bin,usr/sbin}
chroot "$ROOTFS" /bin/busybox --install -s
cp "$HELLO_KO" "$ROOTFS/hello.ko"
cp "$CONFIGS_KO" "$ROOTFS/configs.ko"
chmod 644 "$ROOTFS/hello.ko" "$ROOTFS/configs.ko"

# --- 打包 ----------------------------------------------------------------
cd "$ROOTFS"
find . | cpio -H newc -o --quiet | gzip -9 > "$OUTPUT"

size=$(stat -c%s "$OUTPUT")
echo "PASS: runtime initramfs built ($size bytes): $OUTPUT"
echo "-- contents:"
find . -print | sort | sed 's|^|  |'
