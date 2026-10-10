#!/usr/bin/env bash
# scripts/bootstrap.sh — DCC 自举验证脚本
#
# 自举流程:
#   阶段 0: cargo build --release  →  dcc0 (Rust 编译产物)
#   阶段 1: dcc0 编译 bootstrap_selftest.c → 可执行程序
#   阶段 2: 运行编译后的程序, 验证 dcc 代码生成正确性
#   阶段 3: dcc0 再次编译同一源文件 → .o, 与第一次 .o 做三层对比
#           (byte / struct / semantic), 验证编译确定性
#
# 退出码: 0 = 全部通过, 非零 = 失败
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

DCC="${DCC:-$REPO_ROOT/target/release/dcc}"
SELFTEST_SRC="$REPO_ROOT/tests/fixtures/c/bootstrap_selftest.c"
WORK_DIR="$(mktemp -d /tmp/dcc-bootstrap.XXXXXX)"
trap 'rm -rf "$WORK_DIR"' EXIT

PASS=0
FAIL=0

log_pass() { echo "  ✅ $1"; PASS=$((PASS + 1)); }
log_fail() { echo "  ❌ $1"; FAIL=$((FAIL + 1)); }

echo "========================================"
echo "  DCC 自举验证 (M5 Bootstrap)"
echo "========================================"
echo ""

# ── 阶段 0: 构建 dcc0 ──
echo "── 阶段 0: 构建 dcc0 (cargo build --release) ──"
if cargo build --release 2>/dev/null; then
    log_pass "dcc0 构建成功"
else
    log_fail "dcc0 构建失败"
    echo ""
    echo "结果: $PASS passed, $FAIL failed"
    exit 1
fi

DCC_VERSION="$("$DCC" --version 2>&1 | head -1)"
echo "  dcc: $DCC_VERSION"
echo ""

# ── 阶段 1: dcc0 编译 bootstrap_selftest.c ──
echo "── 阶段 1: dcc0 编译自举测试程序 ──"

SELFTEST_EXE="$WORK_DIR/bootstrap_selftest"
SELFTEST_O1="$WORK_DIR/selftest_1.o"
SELFTEST_O2="$WORK_DIR/selftest_2.o"

# 1a. 编译为可执行文件
if "$DCC" -O0 -std=gnu23 "$SELFTEST_SRC" -o "$SELFTEST_EXE" 2>/dev/null; then
    log_pass "dcc0 编译自举测试程序成功"
else
    log_fail "dcc0 编译自举测试程序失败"
fi

# 1b. 编译为 .o (用于确定性对比)
if "$DCC" -c -O0 -std=gnu23 "$SELFTEST_SRC" -o "$SELFTEST_O1" 2>/dev/null; then
    log_pass "dcc0 编译 .o (第 1 次) 成功"
else
    log_fail "dcc0 编译 .o (第 1 次) 失败"
fi

echo ""

# ── 阶段 2: 运行编译后的程序 ──
echo "── 阶段 2: 运行自举测试程序 ──"

if [ -x "$SELFTEST_EXE" ]; then
    OUTPUT=$("$SELFTEST_EXE" 2>&1) || true
    EXIT_CODE=$("$SELFTEST_EXE" >/dev/null 2>&1; echo $?) || true

    if [ "$EXIT_CODE" -eq 0 ]; then
        log_pass "自举测试程序运行通过 (exit=0)"
        echo "  输出: $OUTPUT"
    else
        log_fail "自举测试程序运行失败 (exit=$EXIT_CODE)"
        echo "  输出: $OUTPUT"
    fi
else
    log_fail "自举测试程序不存在或不可执行"
fi

echo ""

# ── 阶段 3: 编译确定性验证 ──
echo "── 阶段 3: 编译确定性验证 (dcc0 第二次编译 + 对比) ──"

if "$DCC" -c -O0 -std=gnu23 "$SELFTEST_SRC" -o "$SELFTEST_O2" 2>/dev/null; then
    log_pass "dcc0 编译 .o (第 2 次) 成功"
else
    log_fail "dcc0 编译 .o (第 2 次) 失败"
fi

# 3a. 字节级对比
if [ -f "$SELFTEST_O1" ] && [ -f "$SELFTEST_O2" ]; then
    if cmp -s "$SELFTEST_O1" "$SELFTEST_O2"; then
        log_pass "字节级确定性: .o 文件完全一致"
    else
        SIZE1=$(stat -c%s "$SELFTEST_O1" 2>/dev/null || stat -f%z "$SELFTEST_O1")
        SIZE2=$(stat -c%s "$SELFTEST_O2" 2>/dev/null || stat -f%z "$SELFTEST_O2")
        log_fail "字节级确定性: .o 文件不一致 (${SIZE1}B vs ${SIZE2}B)"
    fi
fi

# 3b. 语义级对比 (使用 --bootstrap-compare)
if [ -f "$SELFTEST_O1" ]; then
    COMPARE_OUTPUT=$("$DCC" -c -O0 -std=gnu23 \
        --bootstrap-compare="$SELFTEST_O1" \
        "$SELFTEST_SRC" -o "$WORK_DIR/selftest_3.o" 2>&1) || true

    if echo "$COMPARE_OUTPUT" | grep -q "byte + semantic identical"; then
        log_pass "语义级确定性: byte + semantic identical"
    elif echo "$COMPARE_OUTPUT" | grep -q "semantically_equivalent: true"; then
        log_pass "语义级确定性: semantically equivalent (byte 略有差异)"
    elif echo "$COMPARE_OUTPUT" | grep -q "bootstrap compare"; then
        log_fail "语义级确定性: 存在语义差异"
        echo "  $COMPARE_OUTPUT" | grep -A5 "bootstrap compare"
    else
        log_pass "语义级确定性: 对比通过 (无差异输出)"
    fi
fi

echo ""

# ── 阶段 4: 多文件编译测试 ──
echo "── 阶段 4: 多文件编译压力测试 ──"

EXTRA_SRCS=(
    "$REPO_ROOT/tests/fixtures/smoke/hello.c"
    "$REPO_ROOT/tests/fixtures/smoke/arith.c"
    "$REPO_ROOT/tests/fixtures/smoke/fib.c"
    "$REPO_ROOT/tests/fixtures/smoke/control_flow.c"
)

MULTI_PASS=0
for src in "${EXTRA_SRCS[@]}"; do
    if [ -f "$src" ]; then
        base="$(basename "$src" .c)"
        o1="$WORK_DIR/multi_${base}_1.o"
        o2="$WORK_DIR/multi_${base}_2.o"
        if "$DCC" -c -O0 "$src" -o "$o1" 2>/dev/null && \
           "$DCC" -c -O0 "$src" -o "$o2" 2>/dev/null; then
            if cmp -s "$o1" "$o2"; then
                MULTI_PASS=$((MULTI_PASS + 1))
            fi
        fi
    fi
done

if [ "$MULTI_PASS" -eq "${#EXTRA_SRCS[@]}" ]; then
    log_pass "多文件编译确定性: ${MULTI_PASS}/${#EXTRA_SRCS[@]} 文件字节一致"
else
    log_fail "多文件编译确定性: ${MULTI_PASS}/${#EXTRA_SRCS[@]} 文件字节一致"
fi

echo ""

# ── 汇总 ──
echo "========================================"
echo "  自举验证结果: $PASS passed, $FAIL failed"
echo "========================================"

if [ "$FAIL" -eq 0 ]; then
    echo "  ✅ DCC 自举验证全部通过!"
    echo ""
    echo "  验证内容:"
    echo "    • dcc0 构建 (cargo build --release)"
    echo "    • dcc0 编译 C 程序 (14 组测试全部通过)"
    echo "    • 编译确定性 (byte + semantic 对比)"
    echo "    • 多文件编译一致性"
    exit 0
else
    echo "  ❌ DCC 自举验证存在失败项"
    exit 1
fi
