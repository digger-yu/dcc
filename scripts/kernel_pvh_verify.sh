#!/usr/bin/env bash
# 内核「全程 dcc」的 PVH 验收装置。
#
# 为什么走 PVH：`arch/x86/boot/**` 与 realmode 的 C 部分（`wakemain.c`、
# `video-mode.c` …）是 `-m16` 的 16 位 C 语料，dcc 没有 16/32 位代码生成
# 后端（C 输入直接硬错 E0319）。PVH 路线用 `CONFIG_PVH=y` + QEMU
# `-kernel vmlinux` 直接进 64 位入口，绕开 setup/实模式那一段，于是验收链上
# 只剩**汇编**形式的 16 位语料 —— 那批由 dcc 预处理 + `as --32` 处理，
# 实测与 gcc `-m16 -c` 的代码/重定位/符号逐字节相同。
#
# 四个阶段（可单独跑，默认只跑 --partial）：
#   --partial  快闸：realmode 六个 `.S` 的产物与 gcc 对拍 + `-m16` C 输入硬错
#   --config   O= 外建目录里按 KCONFIG 落配置（默认 localmodconfig）后关掉两条
#              16/32 位 C 路径；验收规模以 localmodconfig 为准（allmodconfig 太大，留作
#              压力项：KCONFIG=allmodconfig 可切回去）
#   --build    `make vmlinux`，全程 CC/HOSTCC 都是 dcc，PATH 上摆 gcc 陷阱
#   --boot     QEMU `-kernel vmlinux` + initramfs，验到 "Run /init as init process"
#
# 环境变量：KERNEL_TREE（默认 /root/linux-7.1-dcc）、BUILD（默认
# $KERNEL_TREE/../pvh-build）、DCC_BIN、JOBS、QEMU_TIMEOUT、
# KCONFIG（默认 localmodconfig；要压力项时传 allmodconfig，快速通道 x86_64_defconfig）、
# BUILD_TARGET（默认 vmlinux；模块验收用 `all`）。
set -euo pipefail

DCC_REPO="$(cd "$(dirname "$0")/.." && pwd)"
KERNEL_TREE="${KERNEL_TREE:-/root/linux-7.1-dcc}"
BUILD="${BUILD:-$(cd "$KERNEL_TREE/.." && pwd)/pvh-build}"
DCC_BIN="${DCC_BIN:-$DCC_REPO/target/release/dcc}"
JOBS="${JOBS:-2}"
KCONFIG="${KCONFIG:-localmodconfig}"
BUILD_TARGET="${BUILD_TARGET:-vmlinux}"
QEMU_TIMEOUT="${QEMU_TIMEOUT:-45}"
CC_WRAP="$DCC_REPO/scripts/dcc-kernel-cc.sh"
HOST_WRAP="$DCC_REPO/scripts/dcc-host-wrapper.sh"

MODE="${1:---partial}"
pass=0
fail=0
ok()  { pass=$((pass + 1)); echo "PASS: $*"; }
bad() { fail=$((fail + 1)); echo "FAIL: $*"; }

[[ -x "$DCC_BIN" ]] || { echo "FAIL: dcc binary not found: $DCC_BIN"; exit 1; }
# DCC_BIN 可以写成相对路径（本文§3 的 recipe 就是 `DCC_BIN=target/release/dcc`），
# 但 kbuild 会在**别的 cwd** 里调包装器（kconfig 的 `$(shell scripts/cc-version.sh
# $(CC))`），相对路径在那里解析不到，症状是「Sorry, this C compiler is not
# supported.」而非法错。带 `/` 的写法一次性定成绝对路径；裸命令名（走 PATH）不绕。
case "$DCC_BIN" in
    */*) DCC_BIN=$(readlink -f "$DCC_BIN") ;;
esac
[[ -x "$DCC_BIN" ]] || { echo "FAIL: dcc binary not found: $DCC_BIN"; exit 1; }
[[ -d "$KERNEL_TREE" ]] || { echo "FAIL: kernel tree not found: $KERNEL_TREE"; exit 1; }

# ---------------------------------------------------------------- helper ----
# gcc 陷阱：任何 gcc/cc/clang 调用都会留一行并退出 99。`ld` 本身不拦（dcc 直驱
# 用它），只拦 `ld.bfd` —— 那是 gcc 驱动才会直接点的名字。
make_trap_dir() {
    local trap_dir="$1"
    mkdir -p "$trap_dir"
    : > "$trap_dir/log"
    local n
    for n in gcc cc g++ c++ clang ld.bfd; do
        cat > "$trap_dir/$n" <<EOF
#!/bin/sh
echo "$n \$*" >> "$trap_dir/log"
exit 99
EOF
        chmod +x "$trap_dir/$n"
    done
    echo "$trap_dir"
}

elf_class() { # -> ELF32 / ELF64 / other
    local f="$1"
    local class
    class=$(od -An -tu1 -j4 -N1 "$f" | tr -d ' ')
    case "$class" in
        1) echo ELF32 ;;
        2) echo ELF64 ;;
        *) echo other ;;
    esac
}

# ------------------------------------------------------------ --partial -----
# realmode 的六个 `.S` 是 vmlinux 链上仅剩的 16 位语料（rmpiggy.o →
# realmode.bin）。对比口径：剔除 `.debug_*`（行号表本来就随预处理器的行结构
# 变，objcopy -O binary 也会丢掉），代码/重定位/符号必须逐字节相同。
partial() {
    local rm="$KERNEL_TREE/arch/x86/realmode/rm"
    local tmp
    tmp=$(mktemp -d)
    local cflags=(
        -nostdinc
        # O= 构建时 `generated/` 在 $BUILD 下，不在源码树里；顺序与 kbuild 一致：
        # 源码树 include 在前，构建目录 generated 紧跟其后。
        -I"$KERNEL_TREE/arch/x86/include" -I"$BUILD/arch/x86/include/generated"
        -I"$KERNEL_TREE/include" -I"$BUILD/include"
        -I"$KERNEL_TREE/arch/x86/include/uapi" -I"$BUILD/arch/x86/include/generated/uapi"
        -I"$KERNEL_TREE/include/uapi" -I"$BUILD/include/generated/uapi"
        -include "$KERNEL_TREE/include/linux/compiler-version.h"
        -include "$KERNEL_TREE/include/linux/kconfig.h"
        -D__KERNEL__ -D__ASSEMBLY__ -D_SETUP -D_WAKEUP
        -I"$KERNEL_TREE/arch/x86/boot"
        -m16 -g -Os -march=i386 -fno-pic -fcf-protection=none
    )
    local can_compare=1
    command -v gcc >/dev/null 2>&1 || can_compare=0

    local f
    for f in header trampoline_64 stack reboot bioscall copy; do
        local src="$rm/$f.S"
        [[ -f "$src" ]] || { bad "missing source $src"; continue; }
        if ! "$DCC_BIN" "${cflags[@]}" -c -o "$tmp/$f.o" "$src" 2>"$tmp/$f.err"; then
            bad "dcc -m16 -c $f.S: $(head -c 300 "$tmp/$f.err")"
            continue
        fi
        if [[ "$(elf_class "$tmp/$f.o")" != "ELF32" ]]; then
            bad "$f.o 不是 ELF32（-m16 必须走 as --32）"
            continue
        fi
        ok "dcc -m16 -c $f.S → ELF32"
        if [[ "$can_compare" == 1 ]]; then
            gcc "${cflags[@]}" -c -o "$tmp/$f.gcc.o" "$src" 2>/dev/null || {
                bad "gcc -m16 -c $f.S（对拍基线）失败"
                continue
            }
            objcopy --strip-debug "$tmp/$f.o" "$tmp/$f.nodbg.o"
            objcopy --strip-debug "$tmp/$f.gcc.o" "$tmp/$f.gcc.nodbg.o"
            if cmp -s "$tmp/$f.nodbg.o" "$tmp/$f.gcc.nodbg.o"; then
                ok "$f.o 与 gcc -m16 产物同（代码/重定位/符号）"
            else
                bad "$f.o 与 gcc -m16 产物不同（代码/重定位/符号）"
            fi
        fi
    done

    # `-m16` 的 C 输入必须硬错 E0319，且**不许**回头找 gcc 代工。
    local trap_dir
    trap_dir=$(make_trap_dir "$tmp/trap")
    local csrc="$rm/wakemain.c"
    if [[ -f "$csrc" ]]; then
        local out
        out=$(PATH="$trap_dir:$PATH" DCC_BIN="$DCC_BIN" "$CC_WRAP" \
            -m16 -D__KERNEL__ -D__ASSEMBLY__ -c -o "$tmp/wakemain.o" "$csrc" 2>&1 || true)
        if grep -q "DCC-GAP reason=m16/m32-no-c-backend" <<<"$out" \
            && grep -q "E0319" <<<"$out"; then
            ok "-m16 的 C 输入留痕 DCC-GAP + E0319"
        else
            bad "-m16 的 C 输入没有如实报缺口：$(head -c 300 <<<"$out")"
        fi
        if [[ -s "$trap_dir/log" ]]; then
            bad "-m16 C 输入仍起了 gcc：$(cat "$trap_dir/log")"
        else
            ok "-m16 C 输入没有起 gcc（陷阱 0 次）"
        fi
    fi

    rm -rf "$tmp"
}

# ------------------------------------------------------------- --config -----
config() {
    mkdir -p "$BUILD"
    echo "-- $KCONFIG in $BUILD (source tree untouched, O= out-of-tree)"
    # **配置期与构建期必须是同一个 CC**。kbuild 把 `$(CC) --version` 的结果写进
    # `CONFIG_CC_VERSION_TEXT`，并据此派生 `CONFIG_CC_IS_CLANG`/`CC_IS_GCC` 与一大票
    # `-f…`/`-m…` 旗标。旧写法这里用宿主 gcc、`--build` 才换 dcc：syncconfig 把配置
    # 改写成 `dcc 0.1.0` + `CONFIG_CC_IS_CLANG=y`（dcc 预定义 `__clang__`），带进
    # `-fms-anonymous-structs`/`-mstack-alignment=8` 这类 clang 专属旗标 —— dcc 只报
    # warning 忽略，产物看不出差别，但「同一份配置、同一个编译器」这条前提就断了。
    # 实测 `vclock_gettime.o` 的 `.cmd` 里就混着这些旗标。
    local kcc=(-C "$KERNEL_TREE" O="$BUILD" CC="$CC_WRAP" HOSTCC="$HOST_WRAP" HOSTLD=ld)
    # localmodconfig 以**已存在的 .config** 为起点；没有种子时它会把整棵树当交互式
    # oldconfig 问一遍（脚本里不能有人问），所以先落一份 defconfig 再 streamline。
    # 它只裁「本机没加载的模块」，built-in 的启动必需项（8250 console/initramfs）不受影响。
    if [[ "$KCONFIG" == localmodconfig ]]; then
        DCC_BIN="$DCC_BIN" make -s "${kcc[@]}" defconfig
        # `< <(yes "")`：make 退完后 `yes` 拿 SIGPIPE 是正常现象，不能落进 pipefail。
        DCC_BIN="$DCC_BIN" make -s "${kcc[@]}" localmodconfig < <(yes "")
    else
        DCC_BIN="$DCC_BIN" make -s "${kcc[@]}" "$KCONFIG"
    fi
    # 关掉两条 16/32 位 **C** 路径（汇编路径保留，由 as --32 承担）：
    #   ACPI_SLEEP=n   → realmode 的 wakeup C 语料不进链（-m16 的 C）
    #   COMPAT_32=n    → vdso32 不进链（-m32 的 C + fake_32bit_build.h）
    # PVH=y 保证 QEMU 能用 `-kernel vmlinux` 直接进 64 位入口。
    #
    # ACPI_SLEEP 自己**没有 prompt**（`drivers/acpi/Kconfig`：`bool` + `default y`，
    # 依赖 `SUSPEND || HIBERNATION`），对它是 `-d` 不生效的 —— olddefconfig 会按
    # default 把它又拉回 y（实测第一轮就是这样：sed 写进去了，olddefconfig 又开回来）。
    # 想要它 n，必须从上游两个可见选项下手。
    #
    # 用 `scripts/config` 而不是 sed：defconfig 里这些项可能本来就是 "is not set"
    # （缺行），sed 替换不到；脚本会自己按依赖关系把新值写进去。
    "$KERNEL_TREE/scripts/config" --file "$BUILD/.config" \
        -e PVH -d SUSPEND -d HIBERNATION -d ACPI_SLEEP \
        -d IA32_EMULATION -d COMPAT
    DCC_BIN="$DCC_BIN" make -s "${kcc[@]}" olddefconfig
    local n
    for n in CONFIG_PVH CONFIG_ACPI_SLEEP CONFIG_IA32_EMULATION CONFIG_COMPAT; do
        printf '  %-28s %s\n' "$n" "$(grep -E "^(# )?${n}[ =]" "$BUILD/.config" || echo '<absent>')"
    done
    # 配置期所用的编译器必须与构建期一致；不一致时 kbuild 会在构建开始时重写这两行。
    for n in CONFIG_CC_VERSION_TEXT CONFIG_CC_IS_CLANG CONFIG_CC_IS_GCC; do
        printf '  %-28s %s\n' "$n" "$(grep -E "^(# )?${n}[ =]" "$BUILD/.config" || echo '<absent>')"
    done
    grep -q '^CONFIG_PVH=y' "$BUILD/.config" || { bad "CONFIG_PVH 未生效"; return; }
    if grep -q '^CONFIG_ACPI_SLEEP=y' "$BUILD/.config"; then
        bad "CONFIG_ACPI_SLEEP 仍为 y：realmode 的 -m16 C 语料会进链（dcc 无 16 位 C 后端）"
        return
    fi
    ok "PVH 验收配置就绪：$BUILD/.config"
}

# -------------------------------------------------------------- --build -----
build() {
    local trap_dir log
    trap_dir=$(mktemp -d)
    trap_dir=$(make_trap_dir "$trap_dir")
    log="$BUILD/vmlinux-build.log"
    mkdir -p "$BUILD"
    echo "-- make -j$JOBS $BUILD_TARGET (CC=$CC_WRAP, HOSTCC=$HOST_WRAP)"
    local rc=0
    PATH="$trap_dir:$PATH" DCC_BIN="$DCC_BIN" \
        make -C "$KERNEL_TREE" O="$BUILD" -j"$JOBS" \
        CC="$CC_WRAP" HOSTCC="$HOST_WRAP" HOSTLD=ld \
        "$BUILD_TARGET" >"$log" 2>&1 || rc=$?
    local traps delegates
    traps=$(wc -l < "$trap_dir/log")
    delegates=$(grep -c "GCC-DELEGATE" "$log" || true)
    echo "  log:        $log"
    echo "  gcc traps:  $traps"
    echo "  delegates:  $delegates"
    if [[ "$traps" != 0 ]]; then
        bad "构建过程起了 $traps 次 gcc/cc/clang：$(head -3 "$trap_dir/log")"
    else
        ok "构建过程没有起 gcc（陷阱 0 次）"
    fi
    if [[ "$delegates" != 0 ]]; then
        bad "包装器仍委托 gcc $delegates 次：$(grep -m3 "GCC-DELEGATE" "$log")"
    else
        ok "包装器没有 GCC-DELEGATE 留痕"
    fi
    if [[ "$rc" == 0 && -f "$BUILD/vmlinux" ]]; then
        ok "vmlinux 产出：$(ls -lh "$BUILD/vmlinux" | awk '{print $5}')"
    else
        bad "make vmlinux rc=$rc（尾部：$(tail -3 "$log")）"
    fi
    rm -rf "$trap_dir"
}

# --------------------------------------------------------------- --boot -----
boot() {
    if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
        echo "SKIP: qemu-system-x86_64 未安装"
        return
    fi
    [[ -f "$BUILD/vmlinux" ]] || { bad "缺少 $BUILD/vmlinux（先跑 --build）"; return; }
    local initrd="$DCC_REPO/initramfs/initramfs.cpio.gz"
    if [[ ! -f "$initrd" ]]; then
        bash "$DCC_REPO/scripts/build_initramfs.sh" >/dev/null
    fi
    local log="$BUILD/qemu-pvh.log"
    echo "-- QEMU PVH: -kernel vmlinux（不经 setup/实模式）"
    timeout "$QEMU_TIMEOUT" qemu-system-x86_64 \
        -kernel "$BUILD/vmlinux" \
        -initrd "$initrd" \
        -append "console=ttyS0 earlyprintk=serial init=/init panic=-1" \
        -m 1024M -no-reboot -nographic >"$log" 2>&1 || true
    if grep -q "DCC kernel: init reached successfully" "$log"; then
        ok "PVH 启动到达用户空间（log: $log）"
    elif grep -q "Run /init as init process" "$log"; then
        ok "内核执行到 /init（log: $log）"
    else
        bad "PVH 启动未到用户空间（尾部：$(tail -5 "$log")）"
    fi
}

case "$MODE" in
    --partial) partial ;;
    --config)  config ;;
    --build)   build ;;
    --boot)    boot ;;
    --all)     partial; config; build; boot ;;
    *) echo "usage: $0 [--partial|--config|--build|--boot|--all]"; exit 2 ;;
esac

echo "== summary: $pass passed, $fail failed (mode=$MODE)"
[[ "$fail" == 0 ]]
