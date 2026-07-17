#!/bin/bash
set -euo pipefail
export PATH="/usr/bin:/bin:$HOME/.cargo/bin:$PATH"
DCC=/home/ds/dcc/target/release/dcc
GCCSRC=/home/ds/gcc-src
REGEX="$GCCSRC/libiberty/regex.c"

echo "==== #define d / #undef TRANSLATE / #undef PREFETCH ===="
grep -n '^#.*define d\b\|^#.*undef TRANSLATE\|^#.*undef PREFETCH\|^#.*define PREFETCH' "$REGEX" | head -20

echo "==== preprocess with debug stderr ===="
$DCC -E -I"$GCCSRC/include" -I"$GCCSRC/libiberty" -DHAVE_CONFIG_H "$REGEX" \
  >/tmp/regex_full.i 2>/tmp/regex_full.err || true

echo "==== TRANSLATE define in debug ===="
grep 'dcc-debug.*TRANSLATE' /tmp/regex_full.err | head -5

echo "==== PREFETCH define in debug ===="
grep 'dcc-debug.*PREFETCH' /tmp/regex_full.err | head -5

echo "==== **d in full ===="
grep -n '\*\*d' /tmp/regex_full.i | head -5

echo "==== gcc **d count ===="
gcc -E -I"$GCCSRC/include" -I"$GCCSRC/libiberty" -DHAVE_CONFIG_H "$REGEX" \
  2>/dev/null >/tmp/regex_gcc.i
grep -c '\*\*d' /tmp/regex_gcc.i || echo 0

echo "==== source line 6162 context in dcc output ===="
grep -n "RE_DOT_NEWLINE\|anychar\|PREFETCH" /tmp/regex_full.i | grep -A2 -B2 'anychar' | head -20
