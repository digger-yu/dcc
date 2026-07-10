#!/bin/bash
# DCC 全量回归测试 — 在 Linux 上运行
export PATH=/home/ds/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
cd ~/dcc-debug
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
run_test examples/arith.c 23 arith
run_test examples/hello.c 42 hello
run_test examples/control.c 100 control
run_test examples/call.c 42 call

# 代码生成 + 预处理器测试
run_test examples/codegen_test.c 42 codegen
run_test examples/pp_simple_test.c 42 pp_simple
run_test examples/pp_attr_simple.c 10 pp_attr_simple
run_test examples/pp_attr_test.c 30 pp_attr

# 函数调用 + 求值栈测试
run_test examples/call_assign_test.c 20 call_assign
run_test examples/bar_test.c 20 bar_test

# 指针运算缩放测试
run_test examples/ptr_arith_test.c 64 ptr_arith

echo ""
echo "=== $PASS passed, $FAIL failed ==="
[ $FAIL -eq 0 ]
