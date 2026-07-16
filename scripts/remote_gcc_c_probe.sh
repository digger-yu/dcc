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

echo "==== libdecnumber (standalone; include-only 模块 SKIP) ===="
DN_OK=0 DN_FAIL=0 DN_SKIP=0
for src in "$GCCSRC"/libdecnumber/*.c; do
  f=$(basename "$src" .c)
  case "$f" in
    *test*|decBasic|decCommon|decContext|decDouble|decExcept|decLibrary|decNumber|decPacked|decQuad|decRound|decSingle)
      DN_SKIP=$((DN_SKIP+1)); echo "SKIP $f (include/unit)"
      continue
      ;;
  esac
  if "$DCC" -c "${INC[@]}" -o "$OUT/${f}.o" "$src" 2>"$OUT/${f}.err"; then
    DN_OK=$((DN_OK+1)); echo "OK $f"
  else
    DN_FAIL=$((DN_FAIL+1)); echo "FAIL $f:"
    sed 's/\x1b\[[0-9;]*m//g' "$OUT/${f}.err" | head -6
  fi
done
echo "libdecnumber ok=$DN_OK fail=$DN_FAIL skip=$DN_SKIP"
