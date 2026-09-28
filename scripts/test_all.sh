#!/bin/bash
# DCC examples 冒烟回归 — 在 Linux 上运行（逐个 examples/*.c 编译并核对退出码）
#
# 权威分层清单: docs/tests_quality_system_plan.md §9「门禁与命令（权威 Tier 表）」。
#   Tier 0（PR 必绿, 本地等价 `make test`）:
#     cargo build --tests / cargo test --release
#     bash scripts/check_fixtures.sh / bash scripts/check_fixtures_selftest.sh
#   本脚本 **只做薄编排**, 不复制任何断言逻辑: 单元/集成断言一律走 cargo(target 由
#   Cargo.toml 发现), fixture 引用完整性与命名/目录规则一律由 scripts/check_fixtures.sh
#   负责(元门禁: check_fixtures_selftest.sh)。此处仅保留 examples 端到端冒烟这一件事,
#   不要再往这里加 "check_*" 式断言 —— 那是越界, 请加去对应的权威入口。
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"
cargo build 2>/dev/null || { echo "构建失败"; exit 1; }

PASS=0; FAIL=0
# 产物暂存目录：默认 `/tmp`，可用 `SMOKE_TMP=<dir>` 指向别的可写位置（无 /tmp 写权限的环境）。
SMOKE_TMP="${SMOKE_TMP:-/tmp}"
run_test() {
    local src=$1 expected=$2 name=$3
    local out="$SMOKE_TMP/t_$name"
    # 编译失败与运行退出码不符都是 FAIL，但都不能让脚本在 set -e 下直接退出：
    # 本脚本逐条跑完才能给出汇总（旧写法把 `"$out"` 直接放在 set -e 下，而每个
    # 用例都故意返回非 0 退出码 —— 第一条就会把脚本打断，永不产出汇总）。
    if ! cargo run -q --bin dcc -- "$src" -o "$out" > /dev/null 2>&1; then
        echo "FAIL $name: 编译失败"
        FAIL=$((FAIL + 1))
        return 0
    fi
    local actual=0
    "$out" || actual=$?
    if [ "$actual" -eq "$expected" ]; then
        echo "PASS $name: exit=$actual"
        PASS=$((PASS + 1))
    else
        echo "FAIL $name: exit=$actual (expect $expected)"
        FAIL=$((FAIL + 1))
    fi
    return 0
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
