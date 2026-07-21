#!/bin/bash
set -euo pipefail
export PATH="/usr/bin:/bin:$HOME/.cargo/bin:$PATH"
DCC=/home/ds/dcc/target/release/dcc
GCCSRC=/home/ds/gcc-src
REGEX="$GCCSRC/libiberty/regex.c"

echo "==== clear preprocessor cache ===="
rm -rf ~/.dcc/cache/* 2>/dev/null || true
ls -la ~/.dcc/cache 2>/dev/null || echo "cache dir empty/missing"

echo "==== preprocess regex.c (cache cleared) ===="
$DCC -E -I"$GCCSRC/include" -I"$GCCSRC/libiberty" -DHAVE_CONFIG_H "$REGEX" \
  >/tmp/regex_fresh.i 2>/tmp/regex_fresh.err

echo "**d count:" $(grep -c '\*\*d' /tmp/regex_fresh.i || echo 0)
echo "*d count:" $(grep -c 'translate\[(unsigned char) (\*d)\]' /tmp/regex_fresh.i || echo 0)
echo "PREFETCH leftover:" $(grep -c '\bPREFETCH\b' /tmp/regex_fresh.i || echo 0)
echo "while (d == dend) from PREFETCH:" $(grep -c 'while (d == dend)' /tmp/regex_fresh.i || echo 0)

echo "==== compile regex.c ===="
$DCC -c -I"$GCCSRC/include" -I"$GCCSRC/libiberty" -DHAVE_CONFIG_H "$REGEX" -o /tmp/regex.o 2>&1 | tail -5
echo "compile exit: $?"

echo "==== libiberty probe ===="
echo "(已迁移到 cargo test --test integration libiberty_combined_*)"
