#!/bin/bash
# ===========================================================================
# scripts/check_rex_hygiene.sh — x86_64 产物 REX 卫生不变式
#
# 两条判据都以 **objdump 为权威**（不是自家解码器 —— 自家解码器看不见「该发 REX 却没发」，
# `Rex.present` 补上之前它就把 `80 c7 10` 读成 `add $0x10,%dil`，正好假绿）：
#
#   A. 产物里出现 `rex` / `rex.WRB` 这类**标注** ⇒ 该指令带了不扩任何字段的 REX 位。
#      标定过：GAS 对同样的助记符从不发这种前缀，所以这不是读数口径差异而是多发。
#      多发不改变语义，但它是「rex() 助手无法表达『一个都不需要』」的化石层 —— 留着，
#      下一处就会有人照抄；而且多余的 REX.W 恰好是 `movsd`↔`movq`、`movsb`↔`movsq`
#      这类宽度歧义的开始。
#   B. 产物里出现 `%ah/%ch/%dh/%bh` ⇒ 8 位操作数没带存在性 REX（本仓库的寄存器名表把
#      那四个折叠到 0..3，所以后端**从不**意图使用它们；真机上它们是字段 4..7 无 REX 的
#      读数，即「值静默落进别的寄存器的 8..15 位」）。
#
# 语料：tests/fixtures/c 与 tests/fixtures/opt（纯 C 构造）。tests/fixtures/asm 里的
# 内核汇编大块会把**字面量池**留在 .text，反汇编必然读出无意义助记符 —— 那不是编码缺陷，
# 所以不在本门禁的语料里（已由 tests/integration/c_asm.rs 的产物字节断言单独覆盖）。
#
# 输出: 每条违规打印「夹具 - 档位 - 助记符 + 字节」；有任何一条即 exit 1。
# 用法: DCC=... DCC_REPO=... JOBS=4 bash scripts/check_rex_hygiene.sh
# ===========================================================================
set -o pipefail
DCC=${DCC:-/root/dcc/target/release/dcc}
REPO=${DCC_REPO:-/root/dcc}
OUT=${OUT:-/tmp/rexh}
mkdir -p "$OUT"
: > "$OUT/violations.txt"

corpus=$(ls "$REPO"/tests/fixtures/c/*.c "$REPO"/tests/fixtures/opt/*.c 2>/dev/null)
count=0
bad=0
for f in $corpus; do
  for L in 0 2; do
    obj="$OUT/$(basename "$f" .c).d$L.o"
    if ! "$DCC" -c -O$L -std=gnu2x -I "$REPO/tests/fixtures/c" -I "$REPO" "$f" -o "$obj" \
         > "$OUT/build.log" 2>&1; then
      rm -f "$obj"; continue   # 编译不过的夹具由 opt-eq / COMPILE-FAIL 那侧报告
    fi
    count=$((count + 1))
    objdump -d -M att "$obj" 2>/dev/null \
      | grep -E "\brex(\.[A-Z]{1,3})?[ \t]|%(ah|ch|dh|bh)\b" \
      | sed -E "s|^ +|$(basename "$f") -O$L: |" >> "$OUT/violations.txt"
    rm -f "$obj"
  done
done

viol=$(wc -l < "$OUT/violations.txt" | tr -d ' ')
echo "=== REX 卫生门禁：检查 $count 个「夹具 × 档位」组合 ==="
if [ "$viol" -ne 0 ]; then
  echo "违规 $viol 条，按助记符分布："
  awk '{ $1=""; $2=""; $3=""; print }' "$OUT/violations.txt" \
    | sed -E 's/^ +//' | awk '{print $1, $2, $3}' | sort | uniq -c | sort -rn | head -12
  echo "--- 前 10 条原文（全部在 $OUT/violations.txt）---"
  head -10 "$OUT/violations.txt"
  echo "REX-HYGIENE FAIL: $viol 条（判据 A=多余 REX 位, B=8 位操作数缺存在性 REX）"
  exit 1
fi
echo "REX-HYGIENE OK: 零条（无多余 REX 位，无 %ah/%ch/%dh/%bh）"
