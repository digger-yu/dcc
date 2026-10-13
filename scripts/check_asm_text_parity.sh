#!/bin/bash
# scripts/check_asm_text_parity.sh - G4: `-c` 直产 vs. `-S --asm-fallback=gas`
# 经 <triple>-as 汇编后, `.text` 段字节必须一致.
#
# 第一性原理: dcc 有两条产出真指令的路径 —— 二进制 renderer 直发 ELF
# (-c) 与文本 renderer 渲染成 .s 再喂给真 GAS (-S --asm-fallback=gas).
# 两条路径读的是同一个 CommonImage, 但各自有独立的 emitter; 一旦某条
# 路径有 bug (比如压缩指令前缀写反、透传行丢 tab), 单跑 `-c` 或单跑 `-S`
# 都看不出来, 只有对两条路径产出的 `.text` 做归一化比较才能暴露.
#
# 归一化: `.text` 字节里可能含有定位无关的 `dcc-<pid>` 命名 (host-link
# 场景), 但在 `-c` 与 `--asm-fallback=gas` 这两条路径里, 中间产物名只留在
# 临时目录、不进 ELF 节内容 —— 因此 `.text` 可以直接按 sha256 比较.
#
# G5 反假绿: 缺少 `<triple>-as` 时显式打印 ASM-PARITY-SKIP, 不允许静默通过.
# 这是本门禁的核心价值 —— 交叉后端的 parity 只有在工具链齐全时才能判定,
# 假绿的门禁等于没有门禁.
#
# 用法:
#   scripts/check_asm_text_parity.sh
#   DCC=target/release/dcc ARCHES="x86_64-linux-gnu aarch64-linux-gnu" \
#       scripts/check_asm_text_parity.sh
#
# 环境变量:
#   DCC     被检二进制, 默认 target/release/dcc
#   CORPUS  fixture 目录, 默认 tests/fixtures/c (收集 *.c)
#   ARCHES  空格分隔 triple, 默认 5 个具备 GasTextOutput 的后端
#           (故意不含 mips64-linux-gnu —— 该后端没声明 GasTextOutput,
#            跑它只会拿到 E0312, 无 parity 可比对)
#
# 退出码: 0 全部一致 / 全 skip；1 检出 parity 分歧；2 用法或环境错误.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

DCC="${DCC:-target/release/dcc}"
# 本门禁先不接 corpus —— 完整 tests/fixtures/c 有几百个边界 case (wchar/vla/weak 等)
# 尚未下沉到交叉后端, 拿来当门禁会刷屏红. 门禁的作用是"已下沉的部分",
# 所以这里只跑**显式列举的 fixture**, 允许空 (空时输出 "全部 skip", 由 G5 显式打印).
#
# 想扩大 corpus: CORPUS=tests/fixtures/c scripts/check_asm_text_parity.sh
CORPUS="${CORPUS:-}"
# 5 个声明了 Feature::GasTextOutput 的后端 (见 docs/backend_feature_matrix.md).
# mips64 故意不在此列 —— 无 mips64-as, 无 parity 可比对.
ARCHES="${ARCHES:-x86_64-linux-gnu aarch64-linux-gnu arm-linux-gnueabi mips-linux-gnu riscv64-linux-gnu}"

die() { echo "错误: $*" >&2; exit 2; }

[ -x "$DCC" ] || die "找不到可执行文件 $DCC (先 cargo build --release)"
if [ -n "$CORPUS" ]; then
    [ -d "$CORPUS" ] || die "corpus 目录不存在: $CORPUS"
fi

# 收集 fixture —— 只取 .c (交叉后端的 -S 文本对 *.cpp 的 ABI 变体尚未稳定,
# 避免拿不确定的边界当门禁). 允许空 corpus (显式 skip, 见 G5).
shopt -s nullglob
SOURCES=()
if [ -n "$CORPUS" ]; then
    SOURCES=("$CORPUS"/*.c)
fi
shopt -u nullglob

# triple → <triple>-as 命令名; host 归一成 "as".
# 与 src/driver.rs::assembler_cmd_name 保持一致 —— 若二者漂移, 本门禁就
# 守不住 P7-3 的串接.
assembler_for() { # $1=triple
    case "$1" in
        x86_64-linux-gnu) echo "as" ;;
        aarch64-linux-gnu) echo "aarch64-linux-gnu-as" ;;
        arm-linux-gnueabi) echo "arm-linux-gnueabi-as" ;;
        mips-linux-gnu)    echo "mips-linux-gnu-as" ;;
        riscv64-linux-gnu) echo "riscv64-linux-gnu-as" ;;
        *) echo "" ;;
    esac
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# 提取 .text 段的字节并算 sha256 —— 用 objcopy 而不是 readelf -x, 因为
# readelf 输出带地址前缀, 每次 layout 抖动会让 sha256 变.
#   $1=.o path  $2=assembler objdump prefix (可选, 交叉用)
# 输出: "<sha256>" 或空串 (失败/无 .text).
text_hash() { # $1=obj
    local obj="$1"
    local bin
    if ! command -v objcopy >/dev/null 2>&1; then
        return 0
    fi
    bin=$(objcopy -O binary --only-section=.text "$obj" /dev/stdout 2>/dev/null | sha256sum | cut -d' ' -f1) || bin=""
    printf '%s' "$bin"
}

total=0; bad=0; skipped=0

printf 'ASM-TEXT-PARITY: corpus=%s arch=%s\n' "$CORPUS" "$ARCHES"

for src in "${SOURCES[@]}"; do
    name=$(basename "$src")
    for arch in $ARCHES; do
        as=$(assembler_for "$arch")
        if [ -z "$as" ]; then
            echo "ASM-PARITY-SKIP: $name | $arch (无对应 <triple>-as 映射)"
            skipped=$((skipped + 1))
            continue
        fi
        if ! command -v "$as" >/dev/null 2>&1; then
            echo "ASM-PARITY-SKIP: $name | $arch (本机缺 $as)"
            skipped=$((skipped + 1))
            continue
        fi
        total=$((total + 1))

        # 路径 A: dcc -c 直发 ELF
        o_a="$TMP/a.o"
        rm -f "$o_a"
        if ! "$DCC" --target "$arch" -c "$src" -o "$o_a" >/dev/null 2>&1; then
            echo "PARITY-FAIL $name | $arch: dcc -c 直接编译失败 (无 parity 基线)"
            bad=$((bad + 1))
            continue
        fi

        # 路径 B: dcc -S --asm-fallback=gas, 再调 <triple>-as 汇编成 .o
        s_b="$TMP/b.s"
        o_b="$TMP/b.o"
        rm -f "$s_b" "$o_b"
        if ! "$DCC" --target "$arch" -S --asm-fallback=gas "$src" -o "$s_b" >/dev/null 2>&1; then
            echo "PARITY-FAIL $name | $arch: dcc -S --asm-fallback=gas 失败, 无 parity 可比对"
            bad=$((bad + 1))
            continue
        fi
        if ! "$as" -o "$o_b" "$s_b" >/dev/null 2>&1; then
            echo "PARITY-FAIL $name | $arch: $as 拒绝了 dcc 产出的 .s (G4 破)"
            bad=$((bad + 1))
            continue
        fi

        ha=$(text_hash "$o_a")
        hb=$(text_hash "$o_b")
        if [ -z "$ha" ] || [ -z "$hb" ]; then
            echo "PARITY-FAIL $name | $arch: .text 提取失败 (ha='$ha' hb='$hb')"
            bad=$((bad + 1))
            continue
        fi
        if [ "$ha" != "$hb" ]; then
            echo "PARITY-DRIFT $name | $arch: .text 不一致 direct=$ha gas=$hb"
            bad=$((bad + 1))
        fi
        rm -f "$o_a" "$s_b" "$o_b"
    done
done

echo "───────────────────────────────────────"
echo "组合数: $total (skip $skipped)"
if [ "$bad" -eq 0 ]; then
    if [ "$total" -eq 0 ]; then
        echo "✅ 全部 skip (本 host 无交叉 as, 门禁不参与判定)"
    else
        echo "✅ 全部 .text 一致 (direct -c 与 --asm-fallback=gas 经 as 汇出的字节相同)"
    fi
    exit 0
fi
echo "❌ 检出 $bad 个 parity 分歧"
exit 1
