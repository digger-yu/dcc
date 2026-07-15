#!/usr/bin/env bash
set -euo pipefail
source ~/.cargo/env
DCC="${DCC:-./target/release/dcc}"
TMP="${TMPDIR:-/tmp}/dcc-feature-$$"
mkdir -p "$TMP"
trap 'rm -rf "$TMP"' EXIT

pass=0
fail=0
check() {
    local name="$1"
    shift
    if "$@"; then
        echo "PASS $name"
        pass=$((pass + 1))
    else
        echo "FAIL $name"
        fail=$((fail + 1))
    fi
}

cat > "$TMP/fold.c" <<'EOF'
int main(void) { return 2 + 3; }
EOF
"$DCC" -O1 -c -o "$TMP/fold.o" "$TMP/fold.c"
check "O1_fold" objdump -d "$TMP/fold.o" | grep -q 'mov.*\$0x5'

cat > "$TMP/hdr.h" <<'EOF'
int hdr_var;
EOF
cat > "$TMP/main.c" <<'EOF'
#include "hdr.h"
int main(void) { return hdr_var; }
EOF
"$DCC" -MD -MF "$TMP/main.d" -c -o "$TMP/main.o" "$TMP/main.c"
check "MD_dep" grep -q 'hdr.h' "$TMP/main.d"

cat > "$TMP/san.c" <<'EOF'
int f(void) { return 0; }
EOF
"$DCC" -fsanitize=address -c -o "$TMP/san.o" "$TMP/san.c"
check "sanitizer" readelf -SW "$TMP/san.o" | grep -q '\.dcc\.san_info'

"$DCC" -g -fno-omit-frame-pointer -c -o "$TMP/fp.o" "$TMP/san.c"
check "eh_frame" readelf -SW "$TMP/fp.o" | grep -q '\.eh_frame'

"$DCC" -flto -c -o "$TMP/lto.o" "$TMP/san.c"
check "lto" readelf -SW "$TMP/lto.o" | grep -q '\.llvm\.lto'

cat > "$TMP/gasm.c" <<'EOF'
asm(".globl global_marker\nglobal_marker:\n");
int main(void) { return 0; }
EOF
"$DCC" -c -o "$TMP/gasm.o" "$TMP/gasm.c"
check "global_asm" strings "$TMP/gasm.o" | grep -q 'global_marker'

echo "=== $pass passed, $fail failed ==="
test "$fail" -eq 0
