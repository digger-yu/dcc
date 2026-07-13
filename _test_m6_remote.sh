#!/bin/bash
# M6 milestone remote test script
export PATH=$HOME/.cargo/bin:$PATH
cd $HOME/dcc

KERNEL=/home/ds/linux-6.1
INCLUDES="-I $KERNEL/include -I $KERNEL/arch/x86/include -I $KERNEL/arch/x86/include/generated"
INCLUDES="$INCLUDES -I $KERNEL/arch/x86/include/uapi -I $KERNEL/arch/x86/include/generated/uapi"
INCLUDES="$INCLUDES -I $KERNEL/include/uapi -I $KERNEL/include/generated/uapi"
DEFINES="-D__KERNEL__"
FORCE_INCLUDE="-include $KERNEL/include/linux/kconfig.h -include $KERNEL/include/linux/compiler_types.h"
DCC=./target/release/dcc

echo "=== M6 Test Suite ==="
echo ""

# 1. init/main.c parse + codegen
echo "--- Test 1: init/main.c parse + codegen ---"
/usr/bin/timeout 120 $DCC $KERNEL/init/main.c $INCLUDES $DEFINES $FORCE_INCLUDE -c -o /tmp/main.o 2>&1
echo "EXIT: $?"
echo ""

# 2. -MD/-MF dependency file generation
echo "--- Test 2: -MD/-MF dependency file ---"
/usr/bin/timeout 120 $DCC $KERNEL/init/main.c $INCLUDES $DEFINES $FORCE_INCLUDE -c -o /tmp/main.o -MD -MF /tmp/main.d 2>&1
echo "EXIT: $?"
if [ -f /tmp/main.d ]; then
    echo "DEP_FILE_EXISTS: yes"
    head -3 /tmp/main.d
else
    echo "DEP_FILE_NOT_FOUND"
fi
echo ""

# 3. Asm volatile with constraints
echo "--- Test 3: asm volatile (scripts/mod/devicetable-offsets.c) ---"
/usr/bin/timeout 30 $DCC $KERNEL/scripts/mod/devicetable-offsets.c $INCLUDES $DEFINES $FORCE_INCLUDE -fsyntax-only 2>&1
echo "EXIT: $?"
echo ""

# 4. sys_ni.c (COND_SYSCALL/weak)
echo "--- Test 4: sys_ni.c (COND_SYSCALL/weak) ---"
/usr/bin/timeout 120 $DCC $KERNEL/kernel/sys_ni.c $INCLUDES $DEFINES $FORCE_INCLUDE -fsyntax-only 2>&1
echo "EXIT: $?"
echo ""

# 5. Unknown typedef fallback
echo "--- Test 5: unknown typedef fallback ---"
cat > /tmp/unknown_typedef.c << 'EOF'
void f() { unknown_type x = 1; }
int g() { unknown_type *p; return 0; }
EOF
/usr/bin/timeout 10 $DCC /tmp/unknown_typedef.c -fsyntax-only 2>&1
echo "EXIT: $?"
echo ""

# 6. Preprocessor comment/blank line stripping
echo "--- Test 6: preprocessor -E output check ---"
cat > /tmp/pp_test.c << 'EOF'
/* block comment */
int x;
// line comment
int y;
EOF
echo "Input:"
cat /tmp/pp_test.c
echo ""
echo "Output (-E):"
/usr/bin/timeout 10 $DCC /tmp/pp_test.c -E 2>&1
echo "EXIT: $?"
echo ""

# 7. Regression: existing examples
echo "--- Test 7: Regression (hello.c) ---"
/usr/bin/timeout 10 $DCC $HOME/dcc/examples/hello.c -o /tmp/hello_test 2>&1
echo "COMPILE_EXIT: $?"
if [ -f /tmp/hello_test ]; then
    /tmp/hello_test
    echo "RUNTIME_EXIT: $?"
fi
echo ""

echo "=== All tests done ==="
