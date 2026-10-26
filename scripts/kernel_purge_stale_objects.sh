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
# 管辖边界：只动内核 TU 的产物。宿主工具目录（scripts/、tools/）
# 与被有意委托给 gcc 的 realmode（-m16/-m32，dcc 只支持 x86_64）一律不碰。
#
# 用法: bash kernel_purge_stale_objects.sh [--gcc-fallback-only] [KERNEL_DIR] [DCC_BIN]
#   --gcc-fallback-only 只删「确实由 gcc 回退编出」的 .o（按 .comment 判定），
#   用于二进制只改了守卫/不改变字节的路径：不想把整树重编一遍。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DCC_REPO="$(cd "$SCRIPT_DIR/.." && pwd)"

# 先抽走开关，剩下的位置参数才是 KERNEL_DIR / DCC_BIN。
GCC_ONLY=0
args=()
for a in "$@"; do
    case "$a" in
        --gcc-fallback-only) GCC_ONLY=1 ;;
        *) args+=("$a") ;;
    esac
done
set -- "${args[@]:-}"

KERNEL_DIR="${1:-${KERNEL_DIR:-$HOME/linux-7.1}}"
DCC_BIN="${2:-${DCC:-$DCC_REPO/target/release/dcc}}"

[[ -d "$KERNEL_DIR" ]] || { echo "内核目录不存在: $KERNEL_DIR" >&2; exit 1; }
[[ -x "$DCC_BIN" ]] || { echo "dcc 二进制不存在: $DCC_BIN" >&2; exit 1; }

# 先把二进制路径固定成绝对路径：find 的 -newer 与 date -r 都要读它，而下面就要
# cd 进内核树 —— 晚换算会把相对路径当成内核目录下的路径来解释，找不到文件。
DCC_BIN_ABS="$(readlink -f "$DCC_BIN")"
[[ -x "$DCC_BIN_ABS" ]] || { echo "dcc 二进制解析失败: $DCC_BIN_ABS" >&2; exit 1; }

# find 的 -path 模式是相对根参数写的 ⇒ 必须先 cd 进内核树再用 '.' 作根，
# 否则传绝对路径时 './scripts/*' 一条也匹配不上，宿主工具会被误删。
cd "$KERNEL_DIR"

# 宿主工具与有意交给 gcc 的目录：整棵排除，不删它们的产物。
EXCLUDES=(-not -path './scripts/*' -not -path './tools/*' -not -path './arch/x86/realmode/*')

purged=0

# 两条准则按模式合成一份清单：默认「早于二进制 或 属 gcc 回退」，
# --gcc-fallback-only 只取后者。
list_gc_fallback=()
while IFS= read -r obj; do
    if readelf -p .comment "$obj" 2>/dev/null | grep -q 'GCC:'; then
        list_gc_fallback+=("$obj")
    fi
done < <(find . "${EXCLUDES[@]}" -name '*.o' -not -path '*/.tmp_*' -print)

list_stale=()
if [[ "$GCC_ONLY" == 0 ]]; then
    mapfile -t list_stale < <(find . "${EXCLUDES[@]}" -name '*.o' -not -path '*/.tmp_*' \
        ! -newer "$DCC_BIN_ABS" -print)
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
