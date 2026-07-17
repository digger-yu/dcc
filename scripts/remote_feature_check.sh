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

cat > "$TMP/o2fold.c" <<'EOF'
int main(void) { if (1) return 42; return 0; }
EOF
"$DCC" -O2 -c -o "$TMP/o2fold.o" "$TMP/o2fold.c"
check "O2_branch_fold" bash -c "objdump -d '$TMP/o2fold.o' | grep -qE 'mov.*\\\$0x2a|mov.*\\\$0x42'"

"$DCC" --target aarch64-linux-gnu -c -o "$TMP/a64.o" examples/hello.c
check "aarch64_hello" bash -c "readelf -h '$TMP/a64.o' | grep -q AArch64"

"$DCC" --target riscv64-linux-gnu -c -o "$TMP/rv.o" examples/hello.c
check "riscv64_hello" bash -c "readelf -h '$TMP/rv.o' | grep -q RISC-V"

cat > "$TMP/a64call.c" <<'EOF'
int bump(int x) { return x + 1; }
int main(void) {
    int v = 0;
    if (1) v = bump(v);
    return v;
}
EOF
"$DCC" --target aarch64-linux-gnu -c -o "$TMP/a64call.o" "$TMP/a64call.c"
check "aarch64_call" test -f "$TMP/a64call.o"

cat > "$TMP/rvg.c" <<'EOF'
int g = 5;
int main(void) { return g + 1; }
EOF
"$DCC" --target riscv64-linux-gnu -c -o "$TMP/rvg.o" "$TMP/rvg.c"
check "riscv64_rodata" bash -c "readelf -SW '$TMP/rvg.o' | grep -q '\\.rodata'"

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

cat > "$TMP/san_multi.c" <<'EOF'
int a(void) { return 1; }
int b(void) { return 2; }
EOF
"$DCC" -fsanitize=address,thread -fsanitize-coverage -S -o "$TMP/san_multi.s" "$TMP/san_multi.c"
check "sanitizer_prologue" bash -c "grep -c __asan_init '$TMP/san_multi.s' | grep -qx 1"
check "sanitizer_kcsan_entry" bash -c "grep -c __kcsan_func_entry '$TMP/san_multi.s' | grep -qx 2"
check "sanitizer_cov_entry" bash -c "grep -c __sanitizer_cov_trace_pc '$TMP/san_multi.s' | grep -qx 2"

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

cat > "$TMP/dead.c" <<'EOF'
static int dead_fn(void) { return 99; }
int live_fn(void) { return dead_fn() + 1; }
EOF
"$DCC" -flto -c -o "$TMP/dead.o" "$TMP/dead.c"
check "LTO_function_sections" bash -c "readelf -SW '$TMP/dead.o' | grep -q '\\.text\\.live_fn'"
check "LTO_per_func_section" bash -c "readelf -SW '$TMP/dead.o' | grep -q '\\.text\\.dead_fn'"

cat > "$TMP/parallel.c" <<'EOF'
static int helper(void) { return 3; }
int foo(void) { return helper() + 1; }
int bar(void) { return helper() + 2; }
int main(void) { return foo() + bar(); }
EOF
"$DCC" -O2 -c -o "$TMP/parallel.o" "$TMP/parallel.c"
check "parallel_codegen" bash -c "readelf -s '$TMP/parallel.o' | grep -q ' main'"

"$DCC" -mno-red-zone -c -o "$TMP/nrz.o" "$TMP/san.c"
check "mno_red_zone" test -f "$TMP/nrz.o"

"$DCC" -c -o "$TMP/p4.o" examples/test_p4_va_list.c
check "va_list_gp" test -f "$TMP/p4.o"

cat > "$TMP/fntypedef.c" <<'EOF'
typedef int handler_t (void);
typedef handler_t *handler_ptr;
int main(void) { return 0; }
EOF
check "fn_typedef" "$DCC" -fsyntax-only "$TMP/fntypedef.c"

cat > "$TMP/restrict.c" <<'EOF'
void touch(char * __restrict p) { p[0] = 0; }
int main(void) { return 0; }
EOF
"$DCC" -c -o "$TMP/restrict.o" "$TMP/restrict.c"
check "restrict_param" test -f "$TMP/restrict.o"

cat > "$TMP/vecinit.c" <<'EOF'
typedef int v4si __attribute__((vector_size(16)));
int main(void) {
    v4si a = {1, 2, 3, 4};
    v4si b = a + (v4si){10, 20, 30, 40};
    int *p = (int *)&b;
    return p[0] + p[1] + p[2] + p[3] == 110 ? 0 : 1;
}
EOF
"$DCC" -c -o "$TMP/vecinit.o" "$TMP/vecinit.c"
check "vector_init_rvalue" test -f "$TMP/vecinit.o"

cat > "$TMP/irfold.c" <<'EOF'
int main(void) { return 2 + 3; }
EOF
"$DCC" -O1 -c -o "$TMP/irfold.o" "$TMP/irfold.c"
check "ir_resync_o1" bash -c "objdump -d '$TMP/irfold.o' | grep -q 'mov.*\\\$0x5'"

cat > "$TMP/compat_td.c" <<'EOF'
typedef unsigned long sz;
typedef unsigned long sz;
int main(void) { sz x = 0; return (int)x; }
EOF
"$DCC" -c -o "$TMP/compat_td.o" "$TMP/compat_td.c"
check "compat_typedef" test -f "$TMP/compat_td.o"

cat > "$TMP/shadow.c" <<'EOF'
char *index(const char *s, int c) { (void)s; (void)c; return (char *)0; }
int f(void) { unsigned int index = 3; return (int)index; }
int main(void) { return f(); }
EOF
"$DCC" -c -o "$TMP/shadow.o" "$TMP/shadow.c"
check "local_shadows_libc" test -f "$TMP/shadow.o"

cat > "$TMP/asm_goto_hash.c" <<'EOF'
#define asm_volatile_goto(x...) asm goto(x)
int main(void) {
	asm_volatile_goto("1:" "jmp %l[l_yes] # objtool NOPs this \n\t"
		: : "i" (0) : : l_yes);
l_yes: return 0;
}
EOF
"$DCC" -o "$TMP/asm_goto_hash" "$TMP/asm_goto_hash.c" && "$TMP/asm_goto_hash"
check "asm_goto_hash_comment" test $? -eq 0

cat > "$TMP/simd.c" <<'EOF'
typedef int v4si __attribute__((vector_size(16)));
int main(void) {
    v4si a = {1, 2, 3, 4};
    v4si b = {10, 20, 30, 40};
    v4si c = a + b;
    v4si e = a;
    e = e + b;
    int *p = (int *)&c;
    if (p[0] != 11 || p[1] != 22 || p[2] != 33 || p[3] != 44) return 1;
    int *q = (int *)&e;
    if (q[0] != 11 || q[3] != 44) return 2;
    return 0;
}
EOF
"$DCC" -o "$TMP/simd" "$TMP/simd.c" && "$TMP/simd"
check "vector_simd_scalar" test $? -eq 0

cat > "$TMP/int128_abi.c" <<'EOF'
static __int128 passthrough(__int128 v) { return v; }
int main(void) {
    __int128 a = ((__int128)10 << 64) | 3;
    return passthrough(a) == a ? 0 : 1;
}
EOF
"$DCC" -o "$TMP/int128_abi" "$TMP/int128_abi.c" && "$TMP/int128_abi"
check "int128_sysv_call" test $? -eq 0

cat > "$TMP/vecfloat.c" <<'EOF'
typedef float v4f __attribute__((vector_size(16)));
int main(void) {
    v4f a = {1,2,3,4}, b = {10,20,30,40}, c = a + b;
    float *p = (float *)&c;
    return p[0] == 11.0f && p[3] == 44.0f ? 0 : 1;
}
EOF
"$DCC" -o "$TMP/vecfloat" "$TMP/vecfloat.c" && "$TMP/vecfloat"
check "vector_float_scalar" test $? -eq 0

cat > "$TMP/asm_ext.c" <<'EOF'
int g[4] = {1, 2, 3, 4};
__asm__(".pushsection .t,\"a\"\n.balign 4\n.long 1\n.popsection\n");
unsigned long f(unsigned long x) {
	asm volatile("btsq $5, %0\n\tpopcntq %0, %0\n\tbswapq %0" : "+r"(x) :: "cc");
	return x;
}
int main(void) { return (int)(f(0x100) & 0xff); }
EOF
"$DCC" -c -o "$TMP/asm_ext.o" "$TMP/asm_ext.c" 2>"$TMP/asm_ext.err"
check "asm_extended_no_nop" bash -c "! grep -q 'emitted as nop' '$TMP/asm_ext.err' && test -f '$TMP/asm_ext.o'"

cat > "$TMP/builtin_os.c" <<'EOF'
struct S { char a[8]; int b; } s;
char buf[16];
int main(void) {
	if (__builtin_object_size(buf, 1) != 1) return 1;
	if (__builtin_object_size(s.a, 0) != 8) return 2;
	return 0;
}
EOF
"$DCC" -o "$TMP/builtin_os" "$TMP/builtin_os.c" && "$TMP/builtin_os"
check "builtin_object_size_fortify" test $? -eq 0

cat > "$TMP/builtin_apply.c" <<'EOF'
static int callee(void *d) { return *(int *)d + 1; }
static int t(void) {
	int x = 41;
	void *b = __builtin_apply((void (*)(void))callee, &x, 8);
	if (!b) return 1;
	__builtin_return(b);
	return 99;
}
int main(void) { return t() == 42 ? 0 : 2; }
EOF
"$DCC" -o "$TMP/builtin_apply" "$TMP/builtin_apply.c" && "$TMP/builtin_apply"
check "builtin_apply_return" test $? -eq 0

cat > "$TMP/constp.c" <<'EOF'
int main(void) {
	int x = 1;
	if (__builtin_constant_p(42) != 1) return 1;
	if (__builtin_constant_p(x) != 0) return 2;
	return 0;
}
EOF
"$DCC" -o "$TMP/constp" "$TMP/constp.c" && "$TMP/constp"
check "builtin_constant_p" test $? -eq 0

cat > "$TMP/inline.c" <<'EOF'
static int __attribute__((always_inline)) sq(int x) { return x * x; }
static int __attribute__((noinline)) bump(int x) { return x + 1; }
int main(void) {
	return sq(4) == 16 && bump(9) == 10 ? 0 : 1;
}
EOF
"$DCC" -O1 -o "$TMP/inline" "$TMP/inline.c" && "$TMP/inline"
check "inline_attrs_o1" test $? -eq 0

cat > "$TMP/unk.c" <<'EOF'
int f(void) { return __builtin_dcc_unknown_xyz(1); }
void g(void) { __builtin_dcc_unknown_void(); }
int main(void) { return f(); }
EOF
"$DCC" -c -o "$TMP/unk.o" "$TMP/unk.c" 2>"$TMP/unk.err"
check "unknown_builtin_warn" bash -c "grep -q 'unsupported __builtin_' '$TMP/unk.err' && test -f '$TMP/unk.o'"
check "unknown_builtin_ud2" bash -c "objdump -d '$TMP/unk.o' | grep -q ud2"

cat > "$TMP/gvn.c" <<'EOF'
int f(int x, int y) {
	int a = x + y, b = x + y;
	return a + b;
}
int main(void) { return f(3, 4) == 14 ? 0 : 1; }
EOF
"$DCC" -O2 -o "$TMP/gvn" "$TMP/gvn.c" && "$TMP/gvn"
check "opt_gvn_o2" test $? -eq 0

cat > "$TMP/unroll.c" <<'EOF'
int sum4(const int *p) {
	int s = 0;
	for (int i = 0; i < 4; i++) s += p[i];
	return s;
}
int main(void) { int a[4] = {1,2,3,4}; return sum4(a) == 10 ? 0 : 1; }
EOF
"$DCC" -O3 -o "$TMP/unroll" "$TMP/unroll.c" && "$TMP/unroll"
check "opt_loop_unroll_o3" test $? -eq 0

cat > "$TMP/vecloop.c" <<'EOF'
typedef int v4si __attribute__((vector_size(16)));
int main(void) {
	v4si a = {1,2,3,4}, b = {5,6,7,8}, c = {0,0,0,0};
	for (int i = 0; i < 4; i++) c[i] = a[i] + b[i];
	return c[0] == 6 && c[3] == 12 ? 0 : 1;
}
EOF
"$DCC" -O3 -o "$TMP/vecloop" "$TMP/vecloop.c" && "$TMP/vecloop"
check "opt_loop_vectorize_o3" test $? -eq 0

cat > "$TMP/ailo0.c" <<'EOF'
static int __attribute__((always_inline)) dbl(int x) { return x + x; }
int main(void) { return dbl(21) == 42 ? 0 : 1; }
EOF
"$DCC" -O0 -o "$TMP/ailo0" "$TMP/ailo0.c" && "$TMP/ailo0"
check "always_inline_o0" test $? -eq 0

echo "=== $pass passed, $fail failed ==="
test "$fail" -eq 0
