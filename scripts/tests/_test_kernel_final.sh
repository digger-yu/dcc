#!/bin/bash
# DCC Linux Kernel Compilation Status Report
cd /home/ds/linux-6.1
DCC=/home/ds/dcc-debug/target/release/dcc
GCC_INC=$(gcc -print-file-name=include)
COMMON_FLAGS="-nostdinc -isystem $GCC_INC -I./arch/x86/include -I./arch/x86/include/generated -I./include -I./arch/x86/include/uapi -I./arch/x86/include/generated/uapi -I./include/uapi -I./include/generated/uapi -include ./include/linux/compiler-version.h -include ./include/linux/kconfig.h -D__KERNEL__"

echo "=========================================="
echo "DCC Linux Kernel Compilation Test"
echo "=========================================="
echo ""

# Test 1: Compiler detection
echo "--- Test 1: Compiler detection ---"
RESULT=$(bash scripts/cc-version.sh $DCC 2>&1)
echo "  cc-version: $RESULT"
RESULT=$(bash scripts/as-version.sh $DCC 2>&1)
echo "  as-version: $RESULT"

# Test 2: Preprocessor (-E mode)
echo ""
echo "--- Test 2: Preprocessor -E mode ---"
OUT=$(echo '__GNUC__' | $DCC -E -P -x c - 2>&1)
echo "  __GNUC__ expands to: $OUT"

# Test 3: CONFIG_HZ from autoconf.h
echo ""
echo "--- Test 3: -include kconfig.h (CONFIG values) ---"
OUT=$(echo 'CONFIG_HZ' | $DCC -nostdinc -isystem "$GCC_INC" -I./include -I./arch/x86/include -I./arch/x86/include/generated -include ./include/linux/kconfig.h -D__KERNEL__ -E -P -x c - 2>&1)
echo "  CONFIG_HZ = $OUT"

# Test 4: Simple kernel file compilation
echo ""
echo "--- Test 4: Compile simple C files ---"
for f in kernel/sys_ni.c kernel/panic.c init/main.c; do
    OUT=$($DCC $COMMON_FLAGS -c $f -o /tmp/test_$$.o 2>&1)
    if echo "$OUT" | grep -q "error"; then
        ERRORS=$(echo "$OUT" | grep "error:" | wc -l)
        FIRST=$(echo "$OUT" | grep "error:" | head -1)
        echo "  $f: FAIL ($ERRORS errors) - $FIRST"
    elif echo "$OUT" | grep -q "wrote"; then
        echo "  $f: OK"
    else
        echo "  $f: UNKNOWN - $OUT"
    fi
done

# Test 5: Preprocess-only check
echo ""
echo "--- Test 5: Preprocessing completeness ---"
for f in kernel/panic.c init/main.c; do
    LINES=$($DCC $COMMON_FLAGS -E $f 2>/dev/null | wc -l)
    GCC_LINES=$(gcc $COMMON_FLAGS -E $f 2>/dev/null | wc -l)
    echo "  $f: DCC=$LINES lines, GCC=$GCC_LINES lines"
done

# Test 6: Try make with DCC (capture first errors)
echo ""
echo "--- Test 6: make with DCC (first 20 lines of output) ---"
rm -rf ~/.dcc/cache/
timeout 30 make CC=$DCC V=1 2>&1 | head -20

echo ""
echo "=========================================="
echo "Test complete."
