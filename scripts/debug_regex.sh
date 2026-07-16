#!/bin/bash
set -euo pipefail
export PATH="/usr/bin:/bin:$HOME/.cargo/bin:$PATH"
DCC=/home/ds/dcc/target/release/dcc
GCCSRC=/home/ds/gcc-src
REGEX="$GCCSRC/libiberty/regex.c"

echo "==== macro defs in regex.c ===="
grep -n 'define TRANSLATE\|define PREFETCH' "$REGEX" | head -20

echo "==== dcc-debug TRANSLATE/PREFETCH ===="
$DCC -E -I"$GCCSRC/include" -I"$GCCSRC/libiberty" -DHAVE_CONFIG_H "$REGEX" 2>&1 \
  | grep 'dcc-debug.*TRANSLATE\|dcc-debug.*PREFETCH' | head -30

echo "==== grep TRANSLATE(*d) in source ===="
grep -n 'TRANSLATE (\*d)\|TRANSLATE(\*d)' "$REGEX" | head -10
