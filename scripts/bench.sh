#!/usr/bin/env bash
# scripts/bench.sh - 本地快速基准测试
# 第一性原理: 一次脚本跑完 "dcc 编 hello" + "dcc 编 fib" + "汇编对照"
# suckless 准则: < 50 行,无任何依赖

set -e
cd "$(dirname "$0")/.."

DCC=./target/release/dcc
GCC=gcc
TMP=$(mktemp -d)
trap "rm -rf $TMP" EXIT

echo "=== DCC local bench ==="

# 1. dcc 编 hello.c
echo "[1/4] dcc -> hello.c"
time $DCC -O2 examples/hello.c -o $TMP/hello-dcc
chmod +x $TMP/hello-dcc
$TMP/hello-dcc; echo "exit code: $?  (期望 42)"

# 2. gcc 编 hello.c (对比)
echo "[2/4] gcc -> hello.c (baseline)"
$GCC -O2 examples/hello.c -o $TMP/hello-gcc
$TMP/hello-gcc; echo "exit code: $?  (期望 42)"

# 3. dcc 编 fib.c
echo "[3/4] dcc -> fib.c"
time $DCC -O2 examples/fib.c -o $TMP/fib-dcc 2>&1 || echo "fib not yet supported"
if [ -x $TMP/fib-dcc ]; then
    $TMP/fib-dcc; echo "exit code: $?  (期望 55)"
fi

# 4. 体积对比
echo "[4/4] binary size"
ls -la $TMP/hello-dcc $TMP/hello-gcc 2>/dev/null | awk '{print $5, $9}'

echo "=== done ==="
