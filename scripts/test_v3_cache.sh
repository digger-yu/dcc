#!/bin/bash
# 验证：旧 V2 缓存存在时，V3 缓存键仍能得到正确 regex.c 展开
set -euo pipefail
export PATH="/usr/bin:/bin:$HOME/.cargo/bin:$PATH"
DCC="${DCC:-/home/ds/dcc-agent-linux66/target/release/dcc}"
GCCSRC=/home/ds/gcc-src
REGEX="$GCCSRC/libiberty/regex.c"

echo "==== V2 cache files (may exist):" $(ls ~/.dcc/cache/v2_* 2>/dev/null | wc -l)

echo "==== preprocess (must not use stale V2 output) ===="
$DCC -E -I"$GCCSRC/include" -I"$GCCSRC/libiberty" -DHAVE_CONFIG_H "$REGEX" \
  >/tmp/regex_v3.i 2>/dev/null

echo "**d:" $(grep -c '\*\*d' /tmp/regex_v3.i || echo 0)
echo "PREFETCH:" $(grep -c '\bPREFETCH\b' /tmp/regex_v3.i || echo 0)

echo "==== compile regex.c ===="
$DCC -c -I"$GCCSRC/include" -I"$GCCSRC/libiberty" -DHAVE_CONFIG_H "$REGEX" -o /tmp/regex.o 2>&1 | tail -3

echo "==== libiberty ===="
echo "(已迁移到 cargo test --test integration libiberty_combined_*)"
