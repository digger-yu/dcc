#!/bin/bash
# GCC 纯 C 子集探测：libiberty + fixincludes + libdecnumber（链接 libiberty）
set -e
DCC="${DCC:-/home/ds/dcc/target/release/dcc}"
GCCSRC="${GCCSRC:-/home/ds/gcc-src}"
OUT="${OUT:-/tmp/dcc-gcc-build}"
LIBOUT="${LIBOUT:-/tmp/libiberty.o}"

echo "==== libiberty (per-file -c) ===="
DCC="$DCC" GCCSRC="$GCCSRC" OUT="$OUT" bash "$(dirname "$0")/remote_libiberty_probe.sh" | tail -5

echo "==== libiberty link (-r) ===="
if "$DCC" -r -o "$LIBOUT" "$OUT"/*.o 2>"$OUT/link.err"; then
  echo "OK link $LIBOUT ($(stat -c%s "$LIBOUT" 2>/dev/null || wc -c <"$LIBOUT") bytes)"
else
  echo "FAIL link:"
  sed 's/\x1b\[[0-9;]*m//g' "$OUT/link.err" | head -15
fi

INC=(-I"$GCCSRC/include" -I"$GCCSRC/libiberty" -I"$GCCSRC/fixincludes" -DHAVE_CONFIG_H)

echo "==== fixincludes ===="
FI_OK=0 FI_FAIL=0 FI_SKIP=0
for src in "$GCCSRC"/fixincludes/*.c; do
  f=$(basename "$src" .c)
  case "$f" in fixtests) FI_SKIP=$((FI_SKIP+1)); echo "SKIP $f"; continue;; esac
  if "$DCC" -c "${INC[@]}" -o "$OUT/${f}.o" "$src" 2>"$OUT/${f}.err"; then
    FI_OK=$((FI_OK+1)); echo "OK $f"
  else
    FI_FAIL=$((FI_FAIL+1)); echo "FAIL $f:"
    sed 's/\x1b\[[0-9;]*m//g' "$OUT/${f}.err" | head -6
  fi
done
echo "fixincludes ok=$FI_OK fail=$FI_FAIL skip=$FI_SKIP"

echo "==== libdecnumber (single combined TU, decSingle+decDouble+decQuad) ===="
# libdecnumber 在 GCC 仓库里由 11 个互相 #include 的源文件组成, decCommon.c
# / decBasic.c / decSingle.c / decDouble.c / decQuad.c 共享 DECPMAX 宏,
# 三个 wrapper 通过不同 DECPMAX 反复 include 同名代码生成三套符号.
# dcc 把三种格式的 wrapper 收敛到一个 .c 里 (tools/libdecnumber_combined.c),
# 编译为单 .o 后直接参与 libiberty 链接, 避免 GCC configure 阶段的 11
# 个 SKIP 文件. dpd/ 下的 decimal32/64/128 wrapper 继续走独立编译链路.
DN_INC=(-I"$GCCSRC/libdecnumber" -I"$GCCSRC/libdecnumber/dpd" \
        -I"$(dirname "$0")/../tools/libdecnumber_stub")
if "$DCC" -c "${DN_INC[@]}" -DHAVE_CONFIG_H \
     -o "$OUT/libdecnumber_combined.o" \
     "$(dirname "$0")/../tools/libdecnumber_combined.c" 2>"$OUT/libdecnumber_combined.err"; then
  DN_OK=1; DN_SKIP=0; DN_FAIL=0
  echo "OK libdecnumber_combined ($(stat -c%s "$OUT/libdecnumber_combined.o" 2>/dev/null || wc -c <"$OUT/libdecnumber_combined.o") bytes)"
else
  DN_OK=0; DN_SKIP=0; DN_FAIL=1
  echo "FAIL libdecnumber_combined:"
  sed 's/\x1b\[[0-9;]*m//g' "$OUT/libdecnumber_combined.err" | head -10
fi
echo "libdecnumber ok=$DN_OK fail=$DN_FAIL skip=$DN_SKIP (combined TU)"
