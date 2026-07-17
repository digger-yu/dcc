#!/bin/bash
# 远程验证 TRANSLATE(*d) 宏展开：期望 (*d)，禁止 (**d)
set -euo pipefail
export PATH="/usr/bin:/bin:$HOME/.cargo/bin:$PATH"
cd "$(dirname "$0")/.."
DCC="${DCC:-target/release/dcc}"

cargo build --release >/dev/null
echo "==== t_tr2.c -E ===="
"$DCC" -E scripts/t_tr2.c 2>&1 | tee /tmp/t_tr2.i

echo "==== **d check ===="
if grep -q '\*\*d' /tmp/t_tr2.i; then
  echo "FAIL: found **d"
  exit 1
fi
echo "OK: no **d"

echo "==== *d check ===="
grep -q 'translate\[(unsigned char) (\*d)\]' /tmp/t_tr2.i
echo "OK: (*d) preserved"

echo "==== gcc compare ===="
gcc -E scripts/t_tr2.c 2>/dev/null | grep -q 'translate\[(unsigned char) (\*d)\]'
echo "OK: matches gcc -E"
