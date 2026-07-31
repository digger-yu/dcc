#!/usr/bin/env bash
# 探测各错误码的实际 stderr 输出（用于生成 snapshot）
set -euo pipefail
D="${1:-./target/release/dcc}"
strip() { perl -pe 's/\e\[[0-9;]*m//g'; }

run() {
    local name="$1"
    shift
    echo "===== $name ====="
    "$@" 2>&1 | strip || true
    echo
}

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# E0001: expect
echo 'int main() { return }' > "$TMP/e0001.c"
run E0001 "$D" -std=gnu23 -fsyntax-only "$TMP/e0001.c"

# E0002
echo 'int x' > "$TMP/e0002.c"
run E0002 "$D" -std=gnu23 -fsyntax-only "$TMP/e0002.c"

# E0003
echo 'struct;' > "$TMP/e0003.c"
run E0003 "$D" -std=gnu23 -fsyntax-only "$TMP/e0003.c"

# E0004
echo 'int 123;' > "$TMP/e0004.c"
run E0004 "$D" -std=gnu23 -fsyntax-only "$TMP/e0004.c"

# E0006
echo 'int main(){ int x = ; return 0; }' > "$TMP/e0006.c"
run E0006 "$D" -std=gnu23 -fsyntax-only "$TMP/e0006.c"

# E0009
echo 'missing_t x; int main(){return 0;}' > "$TMP/e0009.c"
run E0009 "$D" -std=gnu23 -fsyntax-only "$TMP/e0009.c"

# E0010
echo '_Static_assert(0, "fail"); int main(){return 0;}' > "$TMP/e0010.c"
run E0010 "$D" -std=gnu23 -fsyntax-only "$TMP/e0010.c"

# E0011
echo '_Static_assert(1/0, "x"); int main(){return 0;}' > "$TMP/e0011.c"
run E0011 "$D" -std=gnu23 -fsyntax-only "$TMP/e0011.c"

# E0012
echo '_Static_assert(1, 123); int main(){return 0;}' > "$TMP/e0012.c"
run E0012 "$D" -std=gnu23 -fsyntax-only "$TMP/e0012.c"

# E0013
echo 'int x = __builtin_choose_expr(main(), 1, 2); int main(){return 0;}' > "$TMP/e0013.c"
run E0013 "$D" -std=gnu23 -fsyntax-only "$TMP/e0013.c"

# E0100
run E0100 "$D"

# E0202
echo '#endif' > "$TMP/e0202.c"
run E0202 "$D" -std=gnu23 -fsyntax-only "$TMP/e0202.c"

# E0203
echo '#error stop here' > "$TMP/e0203.c"
run E0203 "$D" -std=gnu23 -fsyntax-only "$TMP/e0203.c"

# E0200
printf '#if 1\n#else\n#elif 0\n#endif\n' > "$TMP/e0200.c"
run E0200 "$D" -std=gnu23 -fsyntax-only "$TMP/e0200.c"

# E0201
printf '#if 0\n#else\n#else\n#endif\n' > "$TMP/e0201.c"
run E0201 "$D" -std=gnu23 -fsyntax-only "$TMP/e0201.c"

# E0204
echo '#include BOGUS' > "$TMP/e0204.c"
run E0204 "$D" -std=gnu23 -fsyntax-only "$TMP/e0204.c"

# E0207 comment
echo '/* unclosed' > "$TMP/e0207.c"
run E0207 "$D" -std=gnu23 -fsyntax-only "$TMP/e0207.c"

# E1001 (needs -c to reach IR)
echo 'missing_t x; int main(){return 0;}' > "$TMP/e1001.c"
run E1001 "$D" -std=gnu23 -c -o "$TMP/out.o" "$TMP/e1001.c"

# E1004
echo 'int main(){ int x=1; return *x; }' > "$TMP/e1004.c"
run E1004 "$D" -std=gnu23 -c -o "$TMP/out.o" "$TMP/e1004.c"

# E1006
echo 'int g = main(); int main(){return 0;}' > "$TMP/e1006.c"
run E1006 "$D" -std=gnu23 -c -o "$TMP/out.o" "$TMP/e1006.c"

# E1003
echo 'int main(){ float a=1,b=2; return (int)(a % b); }' > "$TMP/e1003.c"
run E1003 "$D" -std=gnu23 -c -o "$TMP/out.o" "$TMP/e1003.c"

# E1007
echo 'typedef float v4f __attribute__((vector_size(16))); int main(){ v4f a,b; return (int)(a | b)[0]; }' > "$TMP/e1007.c"
run E1007 "$D" -std=gnu23 -c -o "$TMP/out.o" "$TMP/e1007.c"

# E1008
echo 'int main(){ _Complex float a=1,b=2; return (int)(a / b); }' > "$TMP/e1008.c"
run E1008 "$D" -std=gnu23 -c -o "$TMP/out.o" "$TMP/e1008.c"

# E0206
echo '#include "no_such_header_xyz.h"' > "$TMP/e0206.c"
run E0206 "$D" -std=gnu23 -fsyntax-only "$TMP/e0206.c"

# E0001 alignof
echo 'int x = _Alignof int;' > "$TMP/e0001_alignof.c"
run E0001_alignof "$D" -std=gnu23 -fsyntax-only "$TMP/e0001_alignof.c"

# examples/advanced/bad_syntax.c
if [ -f examples/advanced/bad_syntax.c ]; then
  run bad.c "$D" -std=gnu23 -fsyntax-only examples/advanced/bad_syntax.c
fi
