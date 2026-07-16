#!/bin/bash
set -euo pipefail
export PATH="/usr/bin:/bin:$HOME/.cargo/bin:$PATH"
DCC=/home/ds/dcc/target/release/dcc
GCCSRC=/home/ds/gcc-src
REGEX="$GCCSRC/libiberty/regex.c"

$DCC -E -I"$GCCSRC/include" -I"$GCCSRC/libiberty" -DHAVE_CONFIG_H "$REGEX" >/tmp/r.i 2>/dev/null

echo "==== line 6160 area in source ===="
sed -n '6155,6170p' "$REGEX"

echo "==== matching region in preprocessed (anychar case) ===="
grep -n 'case anychar' /tmp/r.i | head -3
awk '/case anychar:/{for(i=0;i<15;i++){getline; print NR":"$0}}' /tmp/r.i | head -20

echo "==== check if PREFETCH macro in table via micro file ===="
cat > /tmp/pf_test.c << 'EOF'
#include "regex.c"
EOF
# Instead, extract just the define + one use from real line numbers
sed -n '5283,5293p;6160,6165p' "$REGEX" > /tmp/pf_extract.c
echo "--- extract ---"
cat /tmp/pf_extract.c
echo "--- preprocess extract ---"
$DCC -E -I"$GCCSRC/include" -I"$GCCSRC/libiberty" -DHAVE_CONFIG_H /tmp/pf_extract.c 2>&1
