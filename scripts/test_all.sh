#!/bin/bash
# DCC 全量回归测试 — 在 Linux 上运行
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"
cargo build 2>/dev/null || { echo "构建失败"; exit 1; }

PASS=0; FAIL=0
run_test() {
    local src=$1 expected=$2 name=$3
    cargo run -- "$src" -o "/tmp/t_$name" 2>/dev/null
    "/tmp/t_$name"
    local actual=$?
    if [ $actual -eq $expected ]; then
        echo "PASS $name: exit=$actual"
        PASS=$((PASS+1))
    else
        echo "FAIL $name: exit=$actual (expect $expected)"
        FAIL=$((FAIL+1))
    fi
}

# 基础测试
run_test examples/basics/arith.c 23 arith
run_test examples/basics/hello.c 42 hello
run_test examples/basics/control_flow.c 100 control
run_test examples/pointers/function_call.c 42 call

# 代码生成 + 预处理器测试
run_test examples/basics/compound_assign.c 42 codegen
run_test examples/preprocessor/const_expr.c 42 pp_simple
run_test examples/preprocessor/attributes_simple.c 10 pp_attr_simple
run_test examples/preprocessor/attributes.c 30 pp_attr

# 函数调用 + 求值栈测试
run_test examples/pointers/call_by_assign.c 20 call_assign
run_test examples/pointers/bar_call.c 20 bar_test

# 指针运算缩放测试
run_test examples/pointers/pointer_arith.c 64 ptr_arith

echo ""
echo "=== $PASS passed, $FAIL failed ==="
[ $FAIL -eq 0 ]
