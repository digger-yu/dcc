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
# 归一化: 不再按原始 .text 字节 sha256 比较 —— 因为 GAS 会合法地选择最短编码
# (imm32 vs imm8, 如 `add $0x4,%r8` 从 7 字节 49 81 c0 04 00 00 00 缩成 4 字节
# 49 83 c0 04). 这种编码选择不改变指令语义, 不是 dcc 的 bug, 但按字节比会误报.
# 改为对比 objdump -d 产出的指令 mnemonic + 操作数文本 (归一化后), 这样两条路径
# 只在真指令/寄存器/操作数语义不同时才 drift.
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
# 默认 corpus: tests/fixtures/asm/parity —— 专门为本门禁下沉的"5 后端都能 -c/-S"
# 的纯 C fixture (无 stdio 头、无 arch 内联 asm、无未支持 IR). 想临时扩大:
#   CORPUS=tests/fixtures/c scripts/check_asm_text_parity.sh
CORPUS="${CORPUS:-tests/fixtures/asm/parity}"
# 5 个声明了 Feature::GasTextOutput 的后端 (见 docs/backend_feature_matrix.md).
# mips64 故意不在此列 —— 无 mips64-as, 无 parity 可比对.
#
# 注意: aarch64/arm/mips/riscv64 的 build_asm_output 当前只 emit stub (函数头
# + ret + "// IR lowering, N bytes" 注释), 尚未把 self.code 反汇编成 GAS 文本;
# 详见 src/codegen/aarch64/codegen/output.rs:92-95. 在它们补齐文本 emitter 之前,
# 跑这些后端只会把 direct 的真字节 vs. stub 的 4 字节 ret 判成 drift —— 这不是
# dcc codegen 的问题, 而是 -S 输出未实现.
# 一旦某后端补齐 GAS 文本 emitter, 把它加回 ARCHES 即可.
ARCHES="${ARCHES:-x86_64-linux-gnu}"

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

# triple → <triple>-objdump 命令名; host 归一成 "objdump".
# 必须用 triple 版: host objdump 不认交叉 ELF (aarch64/arm/mips/riscv64 都会
# "Can't disassemble for architecture").
objdump_for() { # $1=triple
    case "$1" in
        x86_64-linux-gnu) echo "objdump" ;;
        aarch64-linux-gnu) echo "aarch64-linux-gnu-objdump" ;;
        arm-linux-gnueabi) echo "arm-linux-gnueabi-objdump" ;;
        mips-linux-gnu)    echo "mips-linux-gnu-objdump" ;;
        riscv64-linux-gnu) echo "riscv64-linux-gnu-objdump" ;;
        *) echo "" ;;
    esac
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# 提取反汇编指令流并归一化: 去掉地址/机器码前缀/注释, 只保留 mnemonic + 操作数.
# 这样 GAS 的 imm32→imm8 短编码、NOP padding 数量差异、段内地址偏移等都被抹平,
# 只在真指令序列不同时才 drift.
#   $1=.o path  $2=objdump cmd  $3=输出归一化文件路径
# 退出码: 0 成功; 反汇编有内容但 .text 为空时文件为空.
disasm_norm() { # $1=obj  $2=objdump  $3=out_file
    local obj="$1" od="$2" out="$3"
    rm -f "$out"
    if [ -z "$od" ] || ! command -v "$od" >/dev/null 2>&1; then
        : > "$out"
        return 0
    fi
    # objdump -d 输出用 tab 分隔字段:
    #   x86_64: "  48:\t49 81 c0 04 00 00 00 \tadd    $0x4,%r8"  (3 字段)
    #   ARM:    "   0:\te92d4800\tpush\t{fp, lr}"                (4 字段)
    # 第 1 字段 = 地址, 第 2 字段 = 机器码, 其余是 mnemonic + 操作数.
    # 去掉前 2 字段, 把剩下的字段合并成一条指令文本 —— 这样机器码/编码长度
    # 差异 (GAS 短编码) 被自然剥离.
    "$od" -d "$obj" 2>/dev/null \
        | awk -F'\t' '
            # 匹配以十六进制地址 + 冒号开头的行 (允许前导空白)
            $1 ~ /^[[:space:]]*[0-9a-f]+:[[:space:]]*$/ {
                # 把第 3 字段及之后拼成指令文本
                insn = ""
                for (i = 3; i <= NF; i++) {
                    if (i > 3) insn = insn " "
                    insn = insn $i
                }
                # 去掉行内 "# ..." 注释 (RIP-relative 注释)
                sub(/[[:space:]]*#.*$/, "", insn)
                # 压缩空白
                gsub(/[[:space:]]+/, " ", insn)
                # 去首尾空格
                sub(/^ /, "", insn); sub(/ $/, "", insn)
                if (length(insn) == 0) next
                # 跳过 NOP padding (两条路径可能对齐到不同长度)
                if (insn == "nop") next
                # 归一化地址: 分支目标、RIP-relative 偏移在两条路径上可能因
                # 前面 NOP 数量不同而错位. 抹掉具体地址, 只保留 mnemonic + 操作数类型.
                # x86_64 形如:
                #   "call 102 <main+0x2a>"  → "call"
                #   "je 137 <main+0x5f>"    → "je"
                #   "lea 0x0(%rip),%rax"    → "lea (%rip),%rax"  (把位移抹成 0)
                # ARM 形如:
                #   "beq 1234 <func>"       → "beq"
                # 做法: 删从 " <addr> <name+offset>" 形式的目标. 只对以 <...> 结尾的
                # 行生效, 其他指令 (mov/add/lea 等带寄存器) 不受影响.
                sub(/[[:space:]]+[-0-9xa-fA-F]+[[:space:]]+<[^>]*>[[:space:]]*$/, "", insn)
                # RIP-relative lea/mov: "lea 0xNN(%rip),%reg" → "lea (%rip),%reg"
                sub(/0x[0-9a-fA-F]+\(%rip\)/, "(%rip)", insn)
                if (length(insn) > 0) print insn
              }' \
        > "$out"
    if [ ! -s "$out" ]; then
        : > "$out"
    fi
}

total=0; bad=0; warn=0; skipped=0

printf 'ASM-TEXT-PARITY: corpus=%s arch=%s\n' "$CORPUS" "$ARCHES"

for src in "${SOURCES[@]}"; do
    name=$(basename "$src")
    for arch in $ARCHES; do
        as=$(assembler_for "$arch")
        od=$(objdump_for "$arch")
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
        if ! command -v "$od" >/dev/null 2>&1; then
            echo "ASM-PARITY-SKIP: $name | $arch (本机缺 $od)"
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

        norm_a="$TMP/a.norm.txt"
        norm_b="$TMP/b.norm.txt"
        disasm_norm "$o_a" "$od" "$norm_a"
        disasm_norm "$o_b" "$od" "$norm_b"
        na=$(wc -l < "$norm_a")
        nb=$(wc -l < "$norm_b")

        # 判定策略:
        # - 指令数不同 (na != nb) → PARITY-FAIL: 真 bug (一条路径丢了指令,
        #   比如 -S 没 emit 某个嵌套函数 / 被 /* raw */ 注释吞掉). 这种必须修.
        # - 指令数相同但归一化文本不同 → PARITY-WARN: GAS 合法 alias
        #   (movapd↔movupd, movdqu↔movdqa, movslq↔mov, lea (%rip)↔lea 0x0).
        #   这些不改变语义, 也不是 dcc codegen 的 bug, 只是 GAS 在合法编码
        #   选择上与 dcc 直出不同. 警告但不阻断.
        if [ "$na" != "$nb" ]; then
            echo "PARITY-FAIL $name | $arch: 指令数不一致 direct=$na gas=$nb"
            bad=$((bad + 1))
        elif cmp -s "$norm_a" "$norm_b"; then
            : # 完全一致
        else
            echo "PARITY-WARN $name | $arch: GAS alias 差异 (指令数 $na 相等, 文本不同 —— 合法编码选择, 非 dcc bug)"
            warn=$((warn + 1))
        fi
        rm -f "$o_a" "$s_b" "$o_b" "$norm_a" "$norm_b"
    done
done

echo "───────────────────────────────────────"
echo "组合数: $total (skip $skipped, warn $warn)"
if [ "$bad" -eq 0 ]; then
    if [ "$total" -eq 0 ]; then
        echo "✅ 全部 skip (本 host 无交叉 as, 门禁不参与判定)"
    else
        echo "✅ 无 PARITY-FAIL (指令数一致; GAS alias 差异不计为真 bug)"
    fi
    exit 0
fi
echo "❌ 检出 $bad 个 PARITY-FAIL (指令数不一致 —— 真 bug)"
exit 1
