#!/bin/bash
# M3 全局类型注册表: 头文件缓存功能测试
cd /tmp
DCC=${DCC:-./target/release/dcc}
PASS=0
FAIL=0

echo "=== M3 Global Type Registry Cache Test ==="
echo ""

# 清理之前的缓存
rm -rf ~/.dcc/cache 2>/dev/null

# Test 1: 缓存未命中 (首次编译)
echo "--- Test 1: cache miss (first compile) ---"
cat > /tmp/main.c << 'EOF'
#include "header.h"
int main(void) { return VALUE; }
EOF
cat > /tmp/header.h << 'EOF'
#define VALUE 42
EOF
/usr/bin/timeout 10 $DCC /tmp/main.c -I/tmp -o /tmp/main.o 2>&1
rc=$?
echo "COMPILE_EXIT=$rc"
if [ -f /tmp/main.o ]; then
    /tmp/main.o; echo "RUN_EXIT=$?"
fi
ls ~/.dcc/cache/ 2>/dev/null && echo "CACHE_FILES=$(ls ~/.dcc/cache/ | wc -l)" || echo "NO_CACHE_DIR"
echo ""

# Test 2: 缓存命中 (重复编译)
echo "--- Test 2: cache hit (recompile same file) ---"
/usr/bin/timeout 10 $DCC /tmp/main.c -I/tmp -o /tmp/main2.o 2>&1
rc=$?
echo "COMPILE_EXIT=$rc"
if [ -f /tmp/main2.o ]; then
    /tmp/main2.o; echo "RUN_EXIT=$?"
fi
echo ""

# Test 3: 缓存自动失效 (修改头文件内容)
echo "--- Test 3: cache invalidation (modified header) ---"
cat > /tmp/header.h << 'EOF'
#define VALUE 100
EOF
/usr/bin/timeout 10 $DCC /tmp/main.c -I/tmp -o /tmp/main3.o 2>&1
rc=$?
echo "COMPILE_EXIT=$rc"
if [ -f /tmp/main3.o ]; then
    /tmp/main3.o; echo "RUN_EXIT=$?"
fi
echo ""

# Test 4: 宏状态隔离 - 头文件不污染父文件的宏
echo "--- Test 4: macro isolation ---"
cat > /tmp/test_isolation.c << 'EOF'
#include "iso.h"
#ifndef ISO_MACRO
int x = 1;
#endif
int main(void) { return x; }
EOF
cat > /tmp/iso.h << 'EOF'
#define ISO_MACRO 42
EOF
/usr/bin/timeout 10 $DCC /tmp/test_isolation.c -I/tmp -fsyntax-only 2>&1
echo "COMPILE_EXIT=$?"
echo ""

# Test 5: 多文件 include 链
echo "--- Test 5: multi-level include chain ---"
cat > /tmp/level1.h << 'EOF'
#include "level2.h"
#define LEVEL1_VAL 10
EOF
cat > /tmp/level2.h << 'EOF'
#define LEVEL2_VAL 20
EOF
cat > /tmp/chain.c << 'EOF'
#include "level1.h"
int main(void) { return LEVEL1_VAL + LEVEL2_VAL; }
EOF
rm -rf ~/.dcc/cache 2>/dev/null
/usr/bin/timeout 10 $DCC /tmp/chain.c -I/tmp -o /tmp/chain.o 2>&1
rc=$?
echo "COMPILE_EXIT=$rc"
if [ -f /tmp/chain.o ]; then
    /tmp/chain.o; echo "RUN_EXIT=$?"
fi
echo "CACHE_FILES=$(ls ~/.dcc/cache/ 2>/dev/null | wc -l)"
echo ""

echo "=== All M3 cache tests done ==="
