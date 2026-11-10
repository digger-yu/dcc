#!/bin/bash
# 帧带漏扫探针（docs/dcc-kernel-build-verify-report.md §5.12）
#
# 帧底那几条带（complex / 位域 RMW / 向量临时槽 / trampoline）现在是**按需**给的：
# 尺寸来自一次穷举静态扫描。扫漏一种形态的后果不是「帧小了一点」，而是发射期算出
# 一个压在别人身上的地址 —— 静默误编译。所以每条带都配了 fail-closed 断言
# （`LayoutMap::cx_rvalue_off` / `bf_rmw_slot` / `vec_temp`、`gen_trampoline`），
# 本脚本就是把这批断言在全语料上跑一遍：漏扫的站点会以 panic / E1128 / E1136 现形。
#
# 为什么必须用 **debug** 二进制：debug profile 打开溢出检查，无符号直减、移位越宽这类
# 「release 里回绕成一个天文数字、把后续初始化元素全吃进本字段」的缺陷在 debug 下是
# 一次 panic。release 二进制扫语料会整批漏掉这一族。
#
# 用法: bash scripts/frame_band_probe.sh [文件...]        # 默认全量 gcc.c-torture
#       DCC=target/debug/dcc LEVELS="-O0 -O2" bash scripts/frame_band_probe.sh
# 退出码：0 = 无命中；非 0 = 命中的文件数（每个命中打印一行 文件名 档位 诊断）。
#
# 与 HEAD 对照才算结论：本语料有一批**既存**的 debug-only 崩溃（见报告 §7），
# 所以判据是「命中集合与 HEAD 一致」，不是「命中数为 0」。做法：
#   git worktree add /tmp/dcc_head HEAD
#   cargo build --manifest-path /tmp/dcc_head/Cargo.toml --target-dir /tmp/head_t
#   DCC=/tmp/head_t/debug/dcc bash scripts/frame_band_probe.sh > /tmp/head.txt
#   DCC=target/debug/dcc       bash scripts/frame_band_probe.sh > /tmp/now.txt
#   diff <(awk '{print $1}' /tmp/head.txt|sort -u) <(awk '{print $1}' /tmp/now.txt|sort -u)
set -uo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DCC=${DCC:-$REPO_ROOT/target/debug/dcc}
LEVELS=${LEVELS:--O0 -O2}
TORTURE_DIR=${TORTURE_DIR:-$REPO_ROOT/tests/gcc-torture/gcc/testsuite/gcc.c-torture}

[ -x "$DCC" ] || { echo "DCC 不可执行: $DCC（按需带断言只在 debug profile 下才会因溢出而停住）" >&2; exit 2; }

if [ "$#" -gt 0 ]; then
    files=("$@")
else
    mapfile -t files < <(find "$TORTURE_DIR" -name '*.c' | sort)
fi
[ "${#files[@]}" -gt 0 ] || { echo "没有输入文件" >&2; exit 2; }

out=$(mktemp -d /tmp/frame_band_probe.XXXXXX)
trap 'rm -rf "$out"' EXIT

# 只认「后端自己叫停」的形态：内部断言与帧预算/绑定类硬错。普通前端诊断不算命中。
HITS='panicked at|error\[E11(28|33|34|36)\]|error\[E0321\]'
n=0
for f in "${files[@]}"; do
    for lv in $LEVELS; do
        diag=$("$DCC" "$lv" -w -c -o "$out/x.o" "$f" 2>&1 | grep -oE "$HITS" | sort -u | tr '\n' ' ')
        if [ -n "$diag" ]; then
            printf '%s %s => %s\n' "$(basename "$f")" "$lv" "$diag"
            n=$((n + 1))
        fi
    done
done
echo "flagged=$n files=${#files[@]} dcc=$DCC levels='$LEVELS'"
[ "$n" -eq 0 ] && exit 0
exit 1
