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
    # 外层子 shell 也要 `2>/dev/null`：夹具**有意**触发信号时（`builtin_trap.c`
    # 一类），「段错误/非法指令/已中止/追踪或断点陷阱」是 **bash 作业控制自己打的**
    # 消息，不是产物的 stderr——只重定向内层那一条滤不掉它。这些刷屏会让人以为
    # 脚本在报错（它并没有：下面比的正是退出码，gcc 侧同信号同崩才算一致）。
    ( cd "$OUT" && timeout "$timeout_s" "./${tag}_d$L" > "${tag}_d$L.out" 2>/dev/null ) 2>/dev/null
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
  # 进度：全量要十几分钟，不报进度就无法区分「在跑」与「卡死」。并行下每行是短
  # 写入，`>>` 追加一行是原子的；计数偶差一两个无妨，它只用于显示。
  n=$(( $(wc -l < "$OUT/progress.txt" 2>/dev/null || echo 0) + 1 ))
  printf '%s\n' "$f" >> "$OUT/progress.txt"
  if [ $((n % 25)) -eq 0 ]; then
    echo "… 已完成 $n 个夹具" >&2
  fi
  return 0
}
export -f probe norm; export DCC REPO OUT timeout_s

: > "$OUT/summary.txt"
: > "$OUT/progress.txt"

# `ONLY=<子串>` 只跑匹配的夹具：全量要 611 个 × 3 档的「编译+链接+跑」，实测单夹具
# 就要十几秒，整体十几分钟；日常改一处时不需要等全量，全量是 `make test-all` 的活。
# 不开 ONLY 时行为与以前完全一致。
only=${ONLY:-}
list_file="$OUT/corpus.txt"
find "$REPO/tests/fixtures/c" "$REPO/tests/fixtures/opt" -name '*.c' | sort > "$list_file"
if [ -n "$only" ]; then
  grep -F -- "$only" "$list_file" > "$list_file.f" && mv "$list_file.f" "$list_file"
  echo "ONLY=$only → 命中 $(wc -l < "$list_file") 个夹具（不设 ONLY 则跑全量）"
fi
total=$(wc -l < "$list_file")
export total
xargs -a "$list_file" -P "$jobs" -I{} bash -c 'probe "$@"' _ {}
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
  if grep -qE "^error\\[E0302\\]" "$log"; then
    # E0302 不都是一种债。历史上：源里有 IFUNC（target_clones / ifunc 属性）时，
    # dcc 会从「宿主 cc 链接」改道到内置链接器，而内置链接器产出的是**静态**
    # 可执行文件（无 PT_INTERP、无 .dynamic），所以任何 libc 引用都变成未定义符号。
    # 那条改道分支与 elf_has_ifunc_symbols 已删（现 resolver 由 codegen 发在 `.o`
    # 里，宿主 ld 可原生处理，见 src/driver.rs 与 docs/missing_features.md 的 IFUNC 行）；
    # 这个分类因此只是历史成因的标签，正常运行不应再命中——命中即说明改道又回来了。
    if grep -qE "target_clones|__attribute__\\(\\(ifunc|\bifunc\(" "$f"; then
      echo "CF-IFUNC-DYNLINK"; return
    fi
    echo "CF-LIBC-SYM"; return
  fi
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
  # 从 tag 还原源文件路径：tag 是「相对 tests/fixtures 的路径，/ 换成 _」。
  # 只能换**第一个** `_`（它才是目录分隔）；之前写的 `s|_|/|2` 对任何文件名里带
  # 下划线的夹具（本次实测影 9 条 CF）都还原成不存在的路径，于是 `$src` 为空、
  # 下面的 gcc 探针拿到空文件名而失败 —— 把「源文件查不到」误归成「gcc 也拒」。
  src=$(find "$REPO/tests/fixtures/c" "$REPO/tests/fixtures/opt" -name "*.c" \
          | sed "s|$REPO/tests/fixtures/||; s|/|_|g" | grep -x "$tag" \
          | head -1 | sed "s|^|$REPO/tests/fixtures/|; s|_|/|" )
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
      CF-IFUNC-DYNLINK) d="IFUNC 使链接改道内置链接器、产物静默变静态 ⇒ 该改道已删，出现即回归";;
      *)             d="未定性，按新增缺陷处理";;
    esac
    printf '    %-17s %3d  %s\n' "$k" "$n" "$d"
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
