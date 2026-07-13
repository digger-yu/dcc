#!/bin/bash
# M6 综合测试脚本
cd /tmp
DCC=${DCC:-./target/release/dcc}
PASS=0
FAIL=0

test_case() {
    local name="$1"
    local file="$2"
    local expect_rc="$3"
    local extra_args="$4"
    echo -n "Test $((PASS+FAIL+1)): $name ... "
    /usr/bin/timeout 10 $DCC $file -fsyntax-only $extra_args > /tmp/_dcc_out.txt 2>&1
    rc=$?
    if [ "$rc" = "$expect_rc" ]; then
        echo "PASS (rc=$rc)"
        PASS=$((PASS+1))
    else
        echo "FAIL (rc=$rc, expected $expect_rc)"
        cat /tmp/_dcc_out.txt
        FAIL=$((FAIL+1))
    fi
}

# === Test 1: init/main.c (raw) ===
test_case "init/main.c (raw)" "/home/ds/linux-6.1/init/main.c" 1

# === Test 2: unknown typedef fallback ===
cat > /tmp/t_typedef.c << 'EOF'
void f() {
    unknown_type x = 1;
}
typedef int myint;
myint y = 42;
EOF
test_case "unknown typedef fallback" "/tmp/t_typedef.c" 0

# === Test 3: asm volatile (function-level) ===
cat > /tmp/t_asm_func.c << 'EOF'
void f(void) {
    __asm__ volatile("nop" : : : "memory");
    asm("cli");
}
EOF
test_case "asm volatile (function-level)" "/tmp/t_asm_func.c" 0

# === Test 4: file-level asm ===
cat > /tmp/t_asm_file.c << 'EOF'
__asm__(".globl my_func\nmy_func:\n\tret");
EOF
test_case "file-level asm" "/tmp/t_asm_file.c" 0

# === Test 5: function pointers ===
cat > /tmp/t_fptr.c << 'EOF'
void (*fptr)(int);
void (*fptr2)(int) = 0;
int (*get_handler(void))(int) {
    return 0;
}
EOF
test_case "function pointers" "/tmp/t_fptr.c" 0

# === Test 6: asm label on variable ===
cat > /tmp/t_asm_label.c << 'EOF'
int var __asm__("myvar") = 42;
int arr[4] __asm__("myarr");
EOF
test_case "asm label on variable" "/tmp/t_asm_label.c" 0

# === Test 7: attributes ===
cat > /tmp/t_attr.c << 'EOF'
int x __attribute__((unused)) = 1;
void __attribute__((noreturn)) panic(void) { for(;;); }
EOF
test_case "attributes" "/tmp/t_attr.c" 0

# === Test 8: weak symbols ===
cat > /tmp/t_weak.c << 'EOF'
int __weak x = 0;
__attribute__((weak)) int weak_func(void) { return 0; }
EOF
test_case "weak symbols" "/tmp/t_weak.c" 0

# === Test 9: -MD/-MF dependency generation ===
echo -n "Test $((PASS+FAIL+1)): -MD/-MF generation ... "
rm -f /tmp/t_md.d /tmp/t_md.o
/usr/bin/timeout 10 $DCC /tmp/t_fptr.c -o /tmp/t_md.o -MD -MF /tmp/t_md.d 2>/dev/null
rc=$?
if [ -f /tmp/t_md.d ] && [ -s /tmp/t_md.d ]; then
    echo "PASS (rc=$rc, file exists)"
    cat /tmp/t_md.d
    PASS=$((PASS+1))
else
    echo "FAIL (no .d file generated, rc=$rc)"
    FAIL=$((FAIL+1))
fi

# === Test 10: COND_SYSCALL (weak alias) ===
cat > /tmp/t_cond.c << 'EOF'
void __sys_open(void) {}
__attribute__((weak, alias("__sys_open"))) void sys_open(void);
EOF
test_case "COND_SYSCALL pattern" "/tmp/t_cond.c" 0

# === Test 11: preprocessor -E comment stripping ===
cat > /tmp/t_pp.c << 'EOF'
/* comment 1 */
int x;
/* multi-line
   comment */
int z;
EOF
echo -n "Test $((PASS+FAIL+1)): preprocessor -E comment stripping ... "
/usr/bin/timeout 10 $DCC -E /tmp/t_pp.c > /tmp/_pp_out.txt 2>&1
rc=$?
if grep -q "comment" /tmp/_pp_out.txt 2>/dev/null; then
    echo "FAIL (comments leftover in output)"
    cat /tmp/_pp_out.txt
    FAIL=$((FAIL+1))
elif [ "$rc" = "0" ]; then
    echo "PASS (rc=$rc, no comments in output)"
    cat /tmp/_pp_out.txt
    PASS=$((PASS+1))
else
    echo "FAIL (rc=$rc)"
    FAIL=$((FAIL+1))
fi

# === Test 12: codegen (produce valid ELF, needs main) ===
cat > /tmp/t_codegen.c << 'EOF'
int main(void) { return 0; }
EOF
echo -n "Test $((PASS+FAIL+1)): codegen (ELF output) ... "
rm -f /tmp/t_codegen.o
/usr/bin/timeout 10 $DCC /tmp/t_codegen.c -o /tmp/t_codegen.o 2>/dev/null
rc=$?
if [ "$rc" = "0" ] && [ -f /tmp/t_codegen.o ] && [ -s /tmp/t_codegen.o ]; then
    file /tmp/t_codegen.o
    echo "PASS (rc=$rc, ELF generated)"
    PASS=$((PASS+1))
else
    echo "FAIL (rc=$rc)"
    FAIL=$((FAIL+1))
fi

# === Summary ===
echo "========================================="
echo "M6 Test Summary: $PASS passed, $FAIL failed"
echo "========================================="
exit $FAIL
