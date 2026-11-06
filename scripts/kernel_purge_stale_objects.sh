#!/usr/bin/env bash
# 清掉「不该再留着」的内核目标文件，让下一次 make 用**当前** dcc 二进制重编它们。
#
# 为什么需要这一步（而不是 make clean，也不是靠 make 自己发现）：
#   1. Kbuild 的 if_changed 只比对记录的命令行串，串里写的是包装脚本路径
#      （scripts/dcc-kernel-cc.sh），dcc 二进制换了一版它也不认 ⇒ 旧二进制编出的 .o
#      会混进 vmlinux，产物既不可复现也说不清楚是哪一版编的。
#   2. make clean 会把宿主工具（scripts/、tools/ 下用 HOSTCC=gcc 编的）与
#      include/generated 一起拆掉，重编成本几十分钟，而缺陷修复只影响 .o。
#   3. 包装脚本在 dcc 失败时会回退 gcc；回退出来的 .o 带 .comment=GCC，
#      目标是「内核 TU 全部由 dcc 编」，这些必须删掉重编才看得见修复。
#
# 判据两条，都是二进制级的（不看日志、不看命令行串）：
#   - .o 的 mtime 早于 dcc 二进制 ⇒ 旧产物，删。
#   - .o 里带 GCC 的 .comment ⇒ gcc 回退产物，删。
#
# 管辖边界：只动内核 TU 的产物。宿主工具目录（scripts/、tools/）整棵不碰。
#
# 「有意交给 gcc」的产物也不能按 dcc 的判据删 —— 它们的 .comment 天然是 GCC、mtime 也可能
# 早于 dcc 二进制，删了只会每轮重编一遍：
#   - arch/x86/boot/ 顶层 *.o      ：-m16 的 setup 代码（gcc 代编）
#   - arch/x86/realmode/rm/*       ：-m16 的 rm 代码（gcc 代编）
#   - arch/x86/realmode/rmpiggy.o  ：objcopy 产物，没有 .comment
# 反过来，arch/x86/boot/startup/、arch/x86/boot/compressed/ 与 arch/x86/realmode/init.o 是
# 普通 64 位 TU、必须由 dcc 编，所以**不能**按目录一刀切排除（那是 dcc 产物，要参与判据）。
#
# 用法: bash kernel_purge_stale_objects.sh [模式] [KERNEL_DIR] [DCC_BIN]
#
#   默认（无模式）       按「早于 dcc 二进制」+「gcc 回退」两条判据清 —— 编译器一换就
#                        全量重编。这不是 make clean: include/generated、宿主工具、
#                        .config 都保留, 因此比 clean 少掉几十分钟的重复工作。
#   --gcc-fallback-only  只删「确实由 gcc 回退编出」的 .o（按 .comment 判定），
#                        用于二进制只改了守卫/不改变字节的路径：不想把整树重编一遍。
#   --deps <子串>        只删「.o.cmd 文本里出现该子串」的 TU（可重复给，OR）。典型用法
#                        是修了一个头文件里的内联汇编/宏：
#                          --deps include/asm/special_insns.h
#                        只重编依赖它的几十个 TU，而不是三千个。
#   --src <子串>         只删「.cmd 里出现该源文件子串」的 TU（可重复给，OR），
#                        例如 --src arch/x86/kernel/head64.c。
#   --insn <ERE>         只删「现有 .o 的反汇编里命中该正则」的 TU（可重复给，OR）。
#                        用于编码层修复 —— 判据是产物里真有一条被修错的指令形状，
#                        不靠头文件依赖推广（否则 asm/processor.h 这类核心头会命中全树）：
#                          --insn 'mov[[:space:]]+%cr[0-9]'   (控制寄存器 mov)
#                        代价是对每个 .o 跑一次 objdump，干把个对象约一分钟。
#   --list               只打印将要删除的 .o 与计数，不删（可与上面任一模式组合）。
#
#   何时**不能**用定向模式：修复在语义层/ABI 层/寄存器分配/内联决策这类「对任何 TU
#   都可能改变产物」的位置 —— 那时只能用默认模式（或 make clean）重编全树。判据：
#   缺陷的触发条件能否用「哪些源文件包含它」表达出来。能用就 --deps/--src，不能就全量。
#
#   KERNEL_DIR 缺省是 **$HOME/linux-7.1-dcc**（dcc 树）。O= 外建时传**构建输出目录**
#   （.o 与 .o.cmd 都在那里）。参照树 $HOME/linux-7.1 绝对不能
#   拿本脚本扫（会把 gcc 基线产物删掉），并且脚本会因缺 dcc .cmd 指纹而直接拒绝执行。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DCC_REPO="$(cd "$SCRIPT_DIR/.." && pwd)"

# 先抽走开关，剩下的位置参数才是 KERNEL_DIR / DCC_BIN。
GCC_ONLY=0
LIST_ONLY=0
DEPS_PATTERNS=()
SRC_PATTERNS=()
INSNS=()
args=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --gcc-fallback-only) GCC_ONLY=1; shift ;;
        --list) LIST_ONLY=1; shift ;;
        --deps)
            [[ $# -ge 2 ]] || { echo "--deps 需要一个子串参数" >&2; exit 2; }
            DEPS_PATTERNS+=("$2"); shift 2 ;;
        --src)
            [[ $# -ge 2 ]] || { echo "--src 需要一个子串参数" >&2; exit 2; }
            SRC_PATTERNS+=("$2"); shift 2 ;;
        --insn)
            [[ $# -ge 2 ]] || { echo "--insn 需要一个正则参数" >&2; exit 2; }
            INSNS+=("$2"); shift 2 ;;
        *) args+=("$1"); shift ;;
    esac
done
set -- "${args[@]:-}"

# 定向模式: --deps / --src 任一非空即只清「命中判据」的 TU。
TARGETED=0
if [[ ${#DEPS_PATTERNS[@]} -gt 0 || ${#SRC_PATTERNS[@]} -gt 0 || ${#INSNS[@]} -gt 0 ]]; then
    TARGETED=1
    GCC_ONLY=1  # 定向模式只看命中集合，不叠加 mtime 判据
fi

KERNEL_DIR="${1:-${KERNEL_DIR:-$HOME/linux-7.1-dcc}}"
DCC_BIN="${2:-${DCC:-$DCC_REPO/target/release/dcc}}"

[[ -d "$KERNEL_DIR" ]] || { echo "内核目录不存在: $KERNEL_DIR" >&2; exit 1; }
[[ -x "$DCC_BIN" ]] || { echo "dcc 二进制不存在: $DCC_BIN" >&2; exit 1; }

# **防踩 gcc 参照树**（本轮实踩：缺省值曾写的是 `$HOME/linux-7.1`，一次缺省调用把参照树
# 的 1101 个 .o 全删了）。本脚本的语义是「清掉不是当前 dcc 二进制编的 .o」，只对 dcc 树
# 成立；拿它扫参照树会把 gcc 基线拆掉，而两边的 .o 外观完全同构，事后看不出区别。
# 守卫拿二进制级证据：树里必须有 `.cmd` 指纹写着 `dcc-kernel-cc.sh`。全新树（还没编过）
# 没有指纹可查，确认无误后用 DCC_PURGE_ALLOW_NO_DCC_FINGERPRINT=1 显式放行。
if [[ "${DCC_PURGE_ALLOW_NO_DCC_FINGERPRINT:-0}" != "1" ]]; then
    fingerprint=""
    while IFS= read -r cmdf; do
        if grep -qs 'dcc-kernel-cc' "$cmdf"; then
            fingerprint=$cmdf
            break
        fi
    done < <(find "$KERNEL_DIR" -maxdepth 3 -name '.*.o.cmd' 2>/dev/null | head -20)
    if [[ -z "$fingerprint" ]]; then
        echo "拒绝执行：$KERNEL_DIR 里找不到任何写有 dcc-kernel-cc.sh 的 .cmd 指纹。" >&2
        echo "这通常意味着那是 gcc 参照树（不是 dcc 树）—— 本脚本会删它的产物。" >&2
        echo "确认这就是要清的 dcc 树时，导出 DCC_PURGE_ALLOW_NO_DCC_FINGERPRINT=1 重试。" >&2
        exit 1
    fi
fi

# 先把二进制路径固定成绝对路径：find 的 -newer 与 date -r 都要读它，而下面就要
# cd 进内核树 —— 晚换算会把相对路径当成内核目录下的路径来解释，找不到文件。
DCC_BIN_ABS="$(readlink -f "$DCC_BIN")"
[[ -x "$DCC_BIN_ABS" ]] || { echo "dcc 二进制解析失败: $DCC_BIN_ABS" >&2; exit 1; }

# find 的 -path 模式是相对根参数写的 ⇒ 必须先 cd 进内核树再用 '.' 作根，
# 否则传绝对路径时 './scripts/*' 一条也匹配不上，宿主工具会被误删。
cd "$KERNEL_DIR"

# 宿主工具目录：整棵排除，不删它们的产物（它们本来就是 HOSTCC=gcc 编的）。
EXCLUDES=(-not -path './scripts/*' -not -path './tools/*')

# 其余「有意委托 gcc」的产物按路径剔除（不删，也不计入回退清单）。
filter_delegated() {
    grep -vE '^(\./arch/x86/realmode/rm/|\./arch/x86/realmode/rmpiggy\.o|\./arch/x86/boot/[^/]+$)' || true
}

purged=0

# ── 定向模式: 只清「.o.cmd 命中 --deps/--src 子串」的 TU ──────────────────
#
# 判据取 .o.cmd **全文**: `deps_<obj> :=` 行是 kbuild 记下的完整依赖文件清单
# (含 $(wildcard include/config/…) 展开后的绝对路径)，头文件改动的影响面就是它；
# `cmd_<obj> :=` 行则带着源文件路径，--src 靠它命中。比 mtime 判据窄得多。
if [[ "$TARGETED" == 1 ]]; then
    doomed_list=()
    while IFS= read -r obj; do
        [[ -n "$obj" ]] && doomed_list+=("$obj")
    done < <(
        find . "${EXCLUDES[@]}" -name '.*.o.cmd' -not -path '*/.tmp_*' -print |
        while IFS= read -r cmdf; do
            d="${cmdf%/*}"; f="${cmdf##*/}"; f="${f#.}"
            obj="${d}/${f}"; obj="${obj#./}"; obj="${obj%.cmd}"
            [[ -f "$obj" ]] || continue
            hit=0
            for p in ${DEPS_PATTERNS[@]+"${DEPS_PATTERNS[@]}"}; do
                if grep -qF -- "$p" "$cmdf"; then hit=1; break; fi
            done
            if [[ $hit == 0 ]]; then
                for p in ${SRC_PATTERNS[@]+"${SRC_PATTERNS[@]}"}; do
                    if grep -qF -- "$p" "$cmdf"; then hit=1; break; fi
                done
            fi
            if [[ $hit == 0 ]]; then
                for p in ${INSNS[@]+"${INSNS[@]}"}; do
                    # 不能用 grep -q: 它匹配一条就退出关闭管道, objdump 吃 SIGPIPE 以
                    # 141 退出, 而本脚本开着 pipefail ⇒ 整条管道判假 —— 大对象
                    # (输出超管道缓冲区) 全部漏判。逐字读完的 grep 不会提前退出。
                    if objdump -d "$obj" 2>/dev/null | grep -E -- "$p" >/dev/null 2>&1; then
                        hit=1
                        break
                    fi
                done
            fi
            [[ $hit == 1 ]] && echo "$obj"
        done | sort
    )
    echo "== purge summary (targeted) =="
    for p in ${DEPS_PATTERNS[@]+"${DEPS_PATTERNS[@]}"}; do echo "  --deps $p"; done
    for p in ${SRC_PATTERNS[@]+"${SRC_PATTERNS[@]}"}; do echo "  --src  $p"; done
    for p in ${INSNS[@]+"${INSNS[@]}"}; do echo "  --insn $p"; done
    echo "命中 TU  : ${#doomed_list[@]}"
    if [[ "$LIST_ONLY" == 1 ]]; then
        printf '%s\n' ${doomed_list[@]+"${doomed_list[@]}"}
        echo "(--list: 未删除)"
        exit 0
    fi
    for obj in ${doomed_list[@]+"${doomed_list[@]}"}; do
        rm -f "$obj"
        purged=$((purged + 1))
    done
    echo "已删除   : $purged"
    echo "下一步   : make -C <源码树> O=$KERNEL_DIR vmlinux（只重编被删的 TU）"
    exit 0
fi

# 两条准则按模式合成一份清单：默认「早于二进制 或 属 gcc 回退」，
# --gcc-fallback-only 只取后者。
list_gc_fallback=()
while IFS= read -r obj; do
    if readelf -p .comment "$obj" 2>/dev/null | grep -q 'GCC:'; then
        list_gc_fallback+=("$obj")
    fi
done < <(find . "${EXCLUDES[@]}" -name '*.o' -not -path '*/.tmp_*' -print | filter_delegated)

list_stale=()
if [[ "$GCC_ONLY" == 0 ]]; then
    mapfile -t list_stale < <(find . "${EXCLUDES[@]}" -name '*.o' -not -path '*/.tmp_*' \
        ! -newer "$DCC_BIN_ABS" -print | filter_delegated)
fi

if [[ "$LIST_ONLY" == 1 ]]; then
    echo "== purge summary (list only) =="
    echo "早于 dcc 二进制的 .o  : ${#list_stale[@]}"
    echo "gcc 回退产物          : ${#list_gc_fallback[@]}"
    printf '%s\n' ${list_stale[@]+"${list_stale[@]}"} ${list_gc_fallback[@]+"${list_gc_fallback[@]}"} \
        | sort -u | sed '/^$/d'
    echo "(--list: 未删除)"
    exit 0
fi

declare -A doomed=()
for obj in "${list_stale[@]:-}" "${list_gc_fallback[@]:-}"; do
    [[ -n "$obj" ]] || continue
    doomed["$obj"]=1
done

gcc_count=${#list_gc_fallback[@]}

for obj in "${!doomed[@]}"; do
    rm -f "$obj"
    purged=$((purged + 1))
done

echo "== purge summary =="
echo "模式                  : $([[ $GCC_ONLY == 1 ]] && echo gcc-fallback-only || echo stale+gcc-fallback)"
echo "dcc 二进制            : $DCC_BIN_ABS ($(date -r "$DCC_BIN_ABS" '+%F %T'))"
echo "早于该二进制的 .o      : ${#list_stale[@]}"
echo "gcc 回退产物          : $gcc_count"
echo "已删除 (两准则去重后)  : $purged"

# 还留在树上的 gcc 回退产物（mtime 比二进制新，即这次刚编但仍非 dcc 所编）单独点名，
# 免得「全树无回退」的结论被旧 mtime 掩盖。
remain=0
while IFS= read -r obj; do
    if readelf -p .comment "$obj" 2>/dev/null | grep -q 'GCC:'; then
        remain=$((remain + 1))
        echo "仍是 gcc 回退产物: $obj"
    fi
done < <(find . "${EXCLUDES[@]}" -name '*.o' -newer "$DCC_BIN_ABS" -print)
echo "新于二进制且为 gcc 产物: $remain"
