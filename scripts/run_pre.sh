#!/bin/bash
set -euo pipefail
export PATH="/usr/bin:/bin:$HOME/.cargo/bin:$PATH"
DCC=/home/ds/dcc/target/release/dcc
GCCSRC=/home/ds/gcc-src
REGEX="$GCCSRC/libiberty/regex.c"

$DCC -E -I"$GCCSRC/include" -I"$GCCSRC/libiberty" -DHAVE_CONFIG_H "$REGEX" \
  >/tmp/r.i 2>/tmp/r.err || true
echo "lines:" $(wc -l < /tmp/r.i) $(wc -l < /tmp/r.err)
grep 'dcc-debug.*TRANSLATE' /tmp/r.err | head -3 || echo "no TRANSLATE debug"
grep 'dcc-debug.*PREFETCH' /tmp/r.err | head -3 || echo "no PREFETCH debug"
grep -c '\*\*d' /tmp/r.i || echo 0
grep -n 'PREFETCH' /tmp/r.i | head -5
