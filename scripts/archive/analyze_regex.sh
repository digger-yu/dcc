#!/bin/bash
set -euo pipefail
export PATH="/usr/bin:/bin:$HOME/.cargo/bin:$PATH"
DCC=/home/ds/dcc/target/release/dcc
GCCSRC=/home/ds/gcc-src
$DCC -E -I"$GCCSRC/include" -I"$GCCSRC/libiberty" -DHAVE_CONFIG_H \
  "$GCCSRC/libiberty/regex.c" >/tmp/r.i 2>/dev/null

echo "==== (*d) count ===="
grep -o '(unsigned char) (\*d)' /tmp/r.i | wc -l
echo "==== (**d) count ===="
grep -o '(unsigned char) (\*\*d)' /tmp/r.i | wc -l

echo "==== sample **d line ===="
grep -n '\*\*d' /tmp/r.i | head -3

echo "==== sample *d line (good) ===="
grep -n 'translate\[(unsigned char) (\*d)\]' /tmp/r.i | head -3

echo "==== search #define d in regex.c only ===="
grep -n '#define d\b\|#define d ' "$GCCSRC/libiberty/regex.c" || echo none

echo "==== TRANSLATE defs processed (grep source ifndef) ===="
grep -n 'TRANSLATE' "$GCCSRC/libiberty/regex.c" | head -15
