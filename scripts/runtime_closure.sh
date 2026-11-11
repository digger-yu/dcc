#!/usr/bin/env bash
# 最小运行时闭环 + 模块加载闭环 的编排脚本。
#
# 运行时闭环判据：同一份 initramfs（/t 测试程序）分别在 dcc 内核与
# gcc 内核上启动，日志里所有 RT| / MOD| 行**逐字节相同**（与 SQLite
# 轮「结果行逐字节相同」同一判据）。覆盖 fork/exec/wait、基本系统
# 调用、initramfs + ext4 挂载、setitimer/nanosleep 与 LOC 中断计数。
#
# 模块闭环判据：dcc 编译 → 链接 → modpost → QEMU 内 insmod →
# /proc/hello 功能读出 → rmmod 干净卸载，且全日志无 panic。
#
# 用法：
#   runtime_closure.sh --modules      构建 configs.ko + hello.ko
#   runtime_closure.sh --ext4         生成 8 MiB ext4 镜像
#   runtime_closure.sh --initramfs    打包 runtime.cpio.gz
#   runtime_closure.sh --prepare      = 以上三步
#   runtime_closure.sh --boot KERNEL LOG    QEMU 跑一次并做面板判定
#   runtime_closure.sh --compare A.log B.log  RT|/MOD| 行 cmp
#   runtime_closure.sh --gcc-build    用同一 .config 建 /root/pvh-gcc-rt
#   runtime_closure.sh --all          dcc boot + gcc build/boot + compare
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DCC_REPO="${DCC_REPO:-$(cd "$SCRIPT_DIR/.." && pwd)}"
KERNEL_TREE="${KERNEL_TREE:-/root/linux-pvh-src}"
BUILD="${BUILD:-/root/pvh-lmc3}"
DCC_BIN="${DCC_BIN:-$DCC_REPO/target/release/dcc}"
CC_WRAP="$DCC_REPO/scripts/dcc-kernel-cc.sh"
HOST_WRAP="$DCC_REPO/scripts/dcc-host-wrapper.sh"
JOBS="${JOBS:-4}"
QEMU_TIMEOUT="${QEMU_TIMEOUT:-120}"
OUTPUT_DIR="$DCC_REPO/initramfs"
INITRD="$OUTPUT_DIR/runtime.cpio.gz"
EXT4_IMG="$OUTPUT_DIR/rt-ext4.img"
HELLO_DIR="$DCC_REPO/tests/kmod/hello"
HELLO_KO="$HELLO_DIR/hello.ko"
GCC_BUILD="${GCC_BUILD:-/root/pvh-gcc-rt}"

pass=0
fail=0
ok()  { echo "PASS: $*"; pass=$((pass + 1)); }
bad() { echo "FAIL: $*"; fail=$((fail + 1)); }

kbuild() { # 追加 CC/HOSTCC/HOSTLD 的 make 调用
    env DCC_BIN="$DCC_BIN" make -C "$KERNEL_TREE" O="$BUILD" -j"$JOBS" \
        CC="$CC_WRAP" HOSTCC="$HOST_WRAP" HOSTLD=ld "$@"
}

modules() {
    echo "-- 树内模块（CONFIG_IKCONFIG=m → kernel/configs.ko）"
    kbuild modules
    [[ -f "$BUILD/kernel/configs.ko" ]] \
        && ok "configs.ko: $(ls -lh "$BUILD/kernel/configs.ko" | awk '{print $5}')" \
        || bad "configs.ko 未产出"
    echo "-- out-of-tree 模块 hello（dcc 编译 + 链接 + modpost）"
    env DCC_BIN="$DCC_BIN" make -C "$KERNEL_TREE" O="$BUILD" -j"$JOBS" \
        M="$HELLO_DIR" CC="$CC_WRAP" HOSTCC="$HOST_WRAP" HOSTLD=ld modules
    if [[ -f "$HELLO_KO" ]]; then
        ok "hello.ko: $(ls -lh "$HELLO_KO" | awk '{print $5}')"
        # vermagic 与内核一致是 insmod 成功的前提，先静态核对一遍。
        local kv mv
        kv=$(strings "$BUILD/vmlinux" | grep -m1 -E '^[0-9]+\.[0-9]+\.[0-9]+.*SMP' || true)
        mv=$(modinfo -F vermagic "$HELLO_KO" 2>/dev/null || true)
        echo "  kernel:   ${kv:-<n/a>}"
        echo "  vermagic: ${mv:-<n/a>}"
    else
        bad "hello.ko 未产出"
    fi
}

ext4_image() {
    mkdir -p "$OUTPUT_DIR"
    dd if=/dev/zero of="$EXT4_IMG" bs=1M count=8 status=none
    mke2fs -q -t ext4 -F "$EXT4_IMG"
    ok "ext4 镜像: $(ls -lh "$EXT4_IMG" | awk '{print $5}')"
}

initramfs() {
    bash "$DCC_REPO/scripts/build_runtime_initramfs.sh"
    [[ -f "$INITRD" ]] && ok "initramfs: $INITRD" || bad "initramfs 未产出"
}

boot_one() { # $1=vmlinux  $2=log
    local vmlinux="$1" log="$2"
    [[ -f "$vmlinux" ]] || { bad "缺少 $vmlinux"; return; }
    [[ -f "$INITRD" ]] || initramfs
    echo "-- QEMU: $vmlinux → $log"
    timeout "$QEMU_TIMEOUT" qemu-system-x86_64 \
        -kernel "$vmlinux" \
        -initrd "$INITRD" \
        -drive file="$EXT4_IMG",format=raw,if=virtio \
        -append "console=ttyS0 earlyprintk=serial init=/init panic=-1" \
        -m 1024M -no-reboot -nographic -smp 1 >"$log" 2>&1 || true

    grep -q "MOD|done" "$log" && ok "模块闭环跑完（MOD|done）" \
        || bad "模块闭环没跑完（尾部：$(tail -3 "$log" | tr '\n' ' ')）"
    grep -q "^RT|exit|0$" <(sed 's/\r$//' "$log") && ok "运行时测试全过（RT|exit|0）" \
        || bad "运行时测试有失败（$(sed 's/\r$//' "$log" | grep -a '^RT|' | tr '\n' ' ')）"
    if grep -qE "Kernel panic|general protection fault|BUG: unable to handle" "$log"; then
        bad "日志出现 panic/oops（$(grep -m1 -E 'Kernel panic|general protection fault|BUG: unable' "$log")）"
    else
        ok "无 panic / oops"
    fi
    echo "  RT|/MOD| 行:"
    sed 's/\r$//' "$log" | grep -aE '^(RT|MOD)\|' | sed 's/^/    /'
}

compare() { # $1=dcc.log  $2=gcc.log
    local a="$1" b="$2" la="$OUTPUT_DIR/rt-dcc.lines" lb="$OUTPUT_DIR/rt-gcc.lines"
    sed 's/\r$//' "$a" | grep -aE '^(RT|MOD)\|' >"$la"
    sed 's/\r$//' "$b" | grep -aE '^(RT|MOD)\|' >"$lb"
    echo "-- dcc 行数: $(wc -l <"$la") / gcc 行数: $(wc -l <"$lb")"
    if diff -u "$la" "$lb" >"$OUTPUT_DIR/rt-compare.diff"; then
        ok "dcc 内核与 gcc 内核输出逐字节相同（$(wc -l <"$la") 行）"
    else
        bad "输出不一致：$(head -20 "$OUTPUT_DIR/rt-compare.diff")"
    fi
}

gcc_build() {
    echo "-- gcc 基线内核：$GCC_BUILD（同一 .config，O= 独立）"
    mkdir -p "$GCC_BUILD"
    cp "$BUILD/.config" "$GCC_BUILD/.config"
    make -C "$KERNEL_TREE" O="$GCC_BUILD" olddefconfig >/dev/null
    local log="$GCC_BUILD/gcc-build.log"
    make -C "$KERNEL_TREE" O="$GCC_BUILD" -j"$JOBS" \
        CC=gcc HOSTCC=gcc HOSTLD=ld vmlinux >"$log" 2>&1 \
        && ok "gcc vmlinux: $(ls -lh "$GCC_BUILD/vmlinux" | awk '{print $5}')" \
        || bad "gcc 构建失败（尾部：$(tail -3 "$log" | tr '\n' ' ')）"
}

case "${1:-}" in
    --modules)   modules ;;
    --ext4)      ext4_image ;;
    --initramfs) initramfs ;;
    --prepare)   modules; ext4_image; initramfs ;;
    --boot)      boot_one "${2:?vmlinux}" "${3:?log}" ;;
    --compare)   compare "${2:?a.log}" "${3:?b.log}" ;;
    --gcc-build) gcc_build ;;
    --all)
        modules; ext4_image; initramfs
        boot_one "$BUILD/vmlinux" "$OUTPUT_DIR/rt-dcc.log"
        if [[ ! -f "$GCC_BUILD/vmlinux" ]]; then
            gcc_build
        fi
        boot_one "$GCC_BUILD/vmlinux" "$OUTPUT_DIR/rt-gcc.log"
        compare "$OUTPUT_DIR/rt-dcc.log" "$OUTPUT_DIR/rt-gcc.log"
        ;;
    *) echo "usage: $0 [--modules|--ext4|--initramfs|--prepare|--boot KERNEL LOG|--compare A B|--gcc-build|--all]"; exit 2 ;;
esac

echo "== summary: $pass passed, $fail failed"
[[ "$fail" == 0 ]]
