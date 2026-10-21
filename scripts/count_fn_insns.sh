#!/bin/bash
# count_fn_insns.sh <obj> <sym> — 函数体指令口径统计（报告 §6.6 的那一套）
#
# 口径（与 docs/dcc-lua-sqlite-build-verify-report.md §6.6 一致）：
#   insn   = 函数体内的指令行数
#   movrr  = mnemonic 为 mov* 且两个操作数都是裸寄存器（含 32 位形态与 movsxd/movzx 的 dst,dst）
#   r13    = 含 `r13` 的行数（栈引用）
#   imul/movsxd/cmp+test = 对应 mnemonic 的出现次数
#
# Intel 语法 + --no-show-raw-insn：一行一条指令，避免字节列干扰计数。
# 用法: bash scripts/count_fn_insns.sh lvm.o luaV_execute
set -u
obj="$1"; sym="$2"
objdump -d -M intel --no-show-raw-insn "$obj" > /tmp/count_fn_insns.dis
awk -v sym="$sym" '
  $0 ~ ("<" sym ">:") {inside=1; next}
  inside && /^[0-9a-f]+ <.*>:/ {exit}
  inside && /^[[:space:]]+[0-9a-f]+:/ {print}
' /tmp/count_fn_insns.dis > /tmp/count_fn_insns.body
n=$(wc -l < /tmp/count_fn_insns.body)
movrr=$(grep -cE '^[[:space:]]+[0-9a-f]+:[[:space:]]+mov[a-z]*[[:space:]]+[a-z0-9]+,[a-z0-9]+$' /tmp/count_fn_insns.body)
r13=$(grep -c 'r13' /tmp/count_fn_insns.body)
imul=$(grep -cE '\bimul' /tmp/count_fn_insns.body)
movsxd=$(grep -cE '\bmovsxd' /tmp/count_fn_insns.body)
cmp=$(grep -cE '\b(cmp|test)[a-z]*\b' /tmp/count_fn_insns.body)
echo "sym=$sym insn=$n movrr=$movrr r13=$r13 imul=$imul movsxd=$movsxd cmp=$cmp"
