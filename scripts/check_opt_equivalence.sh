#!/bin/bash
# ===========================================================================
# scripts/check_opt_equivalence.sh — 优化档位执行等价不变式门禁
#
# 判据: 同一份源码用 dcc 自己编 -O0 / -O2 / -O3, 三档的 **stdout 与退出码必须完全
# 一致**。为什么以 -O0 为基准而不是 gcc: 部分夹具的绝对期望值本就与 gcc 不同
# (需要特定运行时支持 / 故意 abort / 多 TU 链接), 那些是"运行库覆盖缺口"而不是
# 优化器缺陷; 而优化器的正确性定义恰好就是「不改行为」。
#
# 为什么必须有这一条: `make test` 的 Tier 0 不跑 -O 档位 (`tests/integration/opt.rs`
# 里的 m16_* 「优化测试」全部零参数 = -O0), 因此一批静默误编译 (LICM 把 while 体
# 掏空成死循环、GVN 无 kill 机制、并行发射标号撞名、假 hint 造成 SEGV) 能在门禁
# 全绿的情况下长期存在。本轮就是靠这个扫描发现并修完了它们。
#
# 输出: 只报 OPT-DIFF / COMPILE-FAIL; COMPILE-FAIL 先作为存量债报告不置红
# (根因是夹具自身的运行时依赖, 需逐个定性), OPT-DIFF 非空即 exit 1。
# 用法: DCC=... DCC_REPO=... JOBS=2 bash scripts/check_opt_equivalence.sh
# ===========================================================================
set -o pipefail
DCC=${DCC:-/root/dcc/target/release/dcc}
REPO=${DCC_REPO:-/root/dcc}
OUT=${OUT:-/tmp/opteq2}
jobs=${JOBS:-2}
timeout_s=${TIMEOUT_S:-20}
mkdir -p "$OUT"

# 抹掉 ASLR 决定的地址：夹具里打印指针值的输出每次运行都不同，直接比 stdout 会把
# 「本来就不可比」报成 OPT-DIFF（同一手法已用在 gcc 差分 harness 上）。只抹带 0x 前缀
# 的长十六进制串与裸的 8+ 位 hex，不抹普通数值，以免掩盖真的算错的值。
# 归一化后相等而原文不等时仍会记一行 ADDR-NORM，让「被抹掉的部分」在报告里可见。
norm() { sed -E 's/0x[0-9a-fA-F]{4,}/0xADDR/g; s/\b[0-9a-fA-F]{8,}\b/HEX/g' "$1"; }
probe() {
  local f=$1
  local tag
  tag=$(echo "$f" | sed "s|$REPO/tests/fixtures/||; s|/|_|g")
  local base_rc="" 
  for L in 0 2 3; do
    if ! "$DCC" -O$L -std=gnu2x -I "$REPO/tests/fixtures/c" -I "$REPO" "$f" \
         -o "$OUT/${tag}_d$L" >"$OUT/${tag}_d$L.build" 2>&1; then
      echo "COMPILE-FAIL $tag -O$L" >> "$OUT/summary.txt"
      return 0
    fi
    ( cd "$OUT" && timeout "$timeout_s" "./${tag}_d$L" > "${tag}_d$L.out" 2>/dev/null )
    rc=$?
    if [ "$L" = 0 ]; then
      base_rc=$rc
      mv "$OUT/${tag}_d0.out" "$OUT/${tag}_base.out" 2>/dev/null
    else
      if [ "$rc" -ne "$base_rc" ] || ! cmp -s <(norm "$OUT/${tag}_base.out") <(norm "$OUT/${tag}_d$L.out"); then
        echo "OPT-DIFF $tag -O$L base_rc=$base_rc opt_rc=$rc" >> "$OUT/summary.txt"
      elif ! cmp -s "$OUT/${tag}_base.out" "$OUT/${tag}_d$L.out"; then
        echo "ADDR-NORM $tag -O$L (仅地址不同，已归一化)" >> "$OUT/summary.txt"
      fi
    fi
  done
  return 0
}
export -f probe norm; export DCC REPO OUT timeout_s

: > "$OUT/summary.txt"
find "$REPO/tests/fixtures/c" "$REPO/tests/fixtures/opt" -name '*.c' | sort \
  | xargs -P "$jobs" -I{} bash -c 'probe "$@"' _ {}
total=$(find "$REPO/tests/fixtures/c" "$REPO/tests/fixtures/opt" -name '*.c' | wc -l)
diffs=$(grep -c '^OPT-DIFF' "$OUT/summary.txt" || true)
fails=$(grep -c '^COMPILE-FAIL' "$OUT/summary.txt" || true)
# COMPILE-FAIL 不是一条债，而是几种不同的东西混在一起（本轮逐个定性出来的）：平铺成
# 一个数字会让人以为「还有 60 个缺陷」，也会让**新的**真缺陷藏在里面。所以按日志归类，
# 只有 CF-UNKNOWN 才置红。
#
# 关键一条：判「前端错误算不算 dcc 的债」不靠人列文件清单，而是**用同样的 flags 问 gcc**。
# 例：`c89_trigraphs.c` 的 `??=` 在 -std=gnu2x 下 gcc 也拒绝（夹具自己的注释就写着需要
# -trigraphs），它属于"语料与 flags 不匹配"，不是缺陷；靠人记就会把它错登记成债。
classify_cf() {
  local f=$1 tag=$2 L=$3 log="$OUT/${tag}_d${L}.build"
  [ -f "$log" ] || { echo "CF-UNKNOWN"; return; }
  if grep -qE "undefined reference to .main|crt1\\.o.*_start" "$log"; then echo "CF-NOMAIN"; return; fi
  if grep -qE "^error\\[E0302\\]" "$log"; then echo "CF-LIBC-SYM"; return; fi
  if grep -qE "undefined reference to" "$log"; then echo "CF-CROSS-TU"; return; fi
  # 到这里都是前端/汇编侧错误：gcc 也拒绝 ⇒ 语料本身的问题，不是编译器的债
  if gcc -fsyntax-only -std=gnu2x -w "$f" >/dev/null 2>&1; then
    echo "CF-UNKNOWN"
  else
    echo "CF-SRC-REJECT"
  fi
}
: > "$OUT/cf_classified.txt"
grep '^COMPILE-FAIL' "$OUT/summary.txt" | while read -r _ tag lvl _rest; do
  L=${lvl#-O}
  # 从 tag 还原源文件路径：tag 是「相对 tests/fixtures 的路径，/ 换成 _」
  src=$(find "$REPO/tests/fixtures/c" "$REPO/tests/fixtures/opt" -name "*.c" \
          | sed "s|$REPO/tests/fixtures/||; s|/|_|g" | grep -x "$tag" \
          | head -1 | sed "s|^|$REPO/tests/fixtures/|; s|_|/|2" )
  [ -f "$src" ] || src=""
  printf '%s %s\n' "$(classify_cf "$src" "$tag" "$L")" "$tag" >> "$OUT/cf_classified.txt"
done
unknown=$(grep -c '^CF-UNKNOWN' "$OUT/cf_classified.txt" || true)
norms=$(grep -c '^ADDR-NORM' "$OUT/summary.txt" || true)
echo "=== 语料总数: $total"
echo "=== OPT-DIFF: $diffs   COMPILE-FAIL: $fails   ADDR-NORM(已归一化): $norms"
if [ -s "$OUT/cf_classified.txt" ]; then
  echo "=== COMPILE-FAIL 定性（只有 CF-UNKNOWN 计为新增缺陷）:"
  awk '{print $1}' "$OUT/cf_classified.txt" | sort | uniq -c | sort -rn | while read -r n k; do
    case "$k" in
      CF-NOMAIN)     d="夹具无 main，只做编译期检查（门禁不该链接它）";;
      CF-CROSS-TU)   d="符号由另一 TU 提供，单文件链接必失败";;
      CF-SRC-REJECT) d="同样 flags 下 gcc 也拒绝（语料/flags 不匹配，非编译器债）";;
      CF-LIBC-SYM)   d="缺 libc 符号支持（运行库覆盖缺口）";;
      *)             d="未定性，按新增缺陷处理";;
    esac
    printf '    %-14s %3d  %s\n' "$k" "$n" "$d"
  done
fi

if [ "${unknown:-0}" -gt 0 ]; then
  echo "CF-UNKNOWN 非空 → FAIL（有未定性的编译失败，先归类再决定是否算债）:"
  grep '^CF-UNKNOWN' "$OUT/cf_classified.txt" | sed 's/^/  /'
  exit 1
fi
if [ "$diffs" -gt 0 ]; then
  files=$(grep '^OPT-DIFF' "$OUT/summary.txt" | awk '{print $2}' | sort -u | wc -l)
  # 必须全量打印：以前这里带 `| head -40`，而调用方（以及文档里的度量）习惯用
  # `grep -c '^OPT-DIFF'` 统计 **stdout**，于是「显示截断」被误当成「名单总量」，
  # 实际 103 条被记成 40 条。门禁的数自己说谎比它红更糟。
  echo "=== TOTAL OPT-DIFF=$diffs 条 / $files 个文件（以下为全部条目，未截断；原始汇总: $OUT/summary.txt）"
  grep '^OPT-DIFF' "$OUT/summary.txt" | sort
  echo "OPT-DIFF 非空 → FAIL (优化器改变了程序行为)"
  exit 1
fi
if [ "$fails" -gt 0 ]; then
  grep '^COMPILE-FAIL' "$OUT/summary.txt" | sort | head -10
  echo "注: COMPILE-FAIL 不置红 (夹具自身依赖), 但不得增长"
fi
echo "check_opt_equivalence: OK (0 OPT-DIFF)"
exit 0
