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
check "O1_fold" bash -c "objdump -d '$TMP/fold.o' | grep -q 'mov.*\\\$0x5'"

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
check "sanitizer" bash -c "readelf -SW '$TMP/san.o' | grep -q '\\.dcc\\.san_info'"

"$DCC" -g -fno-omit-frame-pointer -c -o "$TMP/fp.o" "$TMP/san.c"
check "eh_frame" bash -c "readelf -SW '$TMP/fp.o' | grep -q '\\.eh_frame'"

"$DCC" -flto -c -o "$TMP/lto.o" "$TMP/san.c"
check "lto" bash -c "readelf -SW '$TMP/lto.o' | grep -q '\\.llvm\\.lto'"

cat > "$TMP/gasm.c" <<'EOF'
asm(".globl global_marker\nglobal_marker:\n");
int main(void) { return 0; }
EOF
"$DCC" -c -o "$TMP/gasm.o" "$TMP/gasm.c"
check "global_asm" bash -c "strings '$TMP/gasm.o' | grep -q 'global_marker'"

cat > "$TMP/custom_sec.c" <<'EOF'
int custom_function(void) __attribute__((section(".init.text"))) {
    return 7;
}
int main(void) { return custom_function(); }
EOF
"$DCC" -S -o "$TMP/custom_sec.s" "$TMP/custom_sec.c"
check "S_custom_section" grep -q '\.section \.init\.text' "$TMP/custom_sec.s"

cat > "$TMP/lock.c" <<'EOF'
int value = 1;
int main(void) {
    int expected = 1;
    int desired = 2;
    __atomic_compare_exchange_n(
        &value, &expected, desired, 0,
        __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
    return value == 2 ? 0 : 1;
}
EOF
"$DCC" -S -o "$TMP/lock.s" "$TMP/lock.c"
check "S_lock_cmpxchg" grep -q 'lock cmpxchg' "$TMP/lock.s"
check "S_no_lock_byte" bash -c "! grep -q '.byte 0xf0' '$TMP/lock.s'"

cat > "$TMP/sys.h" <<'EOF'
#define SYS_MARKER 1
EOF
cat > "$TMP/mmd.c" <<'EOF'
#include "sys.h"
int main(void) { return SYS_MARKER; }
EOF
"$DCC" -MMD -MF "$TMP/mmd.d" -isystem "$TMP" -c -o "$TMP/mmd.o" "$TMP/mmd.c"
check "MMD_skips_system" bash -c "! grep -q 'sys.h' '$TMP/mmd.d'"
check "MMD_keeps_source" grep -q 'mmd.c' "$TMP/mmd.d"

"$DCC" -MD -MP -MF "$TMP/phony.d" -c -o "$TMP/main.o" "$TMP/main.c"
check "MP_phony" bash -c "grep -q '^$TMP/hdr.h:' '$TMP/phony.d'"

"$DCC" -flto -c -o "$TMP/lto2.o" "$TMP/san.c"
check "LTO_size_hint" bash -c "strings '$TMP/lto2.o' | grep -q ':.*:1'"

"$DCC" -mno-red-zone -c -o "$TMP/nrz.o" "$TMP/san.c"
check "mno_red_zone" test -f "$TMP/nrz.o"

"$DCC" -c -o "$TMP/p4.o" examples/test_p4_va_list.c
check "va_list_gp" test -f "$TMP/p4.o"

echo "=== $pass passed, $fail failed ==="
test "$fail" -eq 0
