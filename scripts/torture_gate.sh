#!/usr/bin/env bash
# P12-D: torture fail 集合门禁
# 用法: DCC=<bin> bash scripts/torture_gate.sh
# 退出码 0 = fail 集合只收缩或不变；非 0 = 有新增失败
set -uo pipefail

DCC="${DCC:-/root/dcc/target/release/dcc}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BASE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

COMPILE_BASE="$BASE_DIR/tests/skip/torture_known_fail_compile.txt"
EXECUTE_BASE="$BASE_DIR/tests/skip/torture_known_fail_execute.txt"

TMP=$(mktemp -d /tmp/dcc-torture-gate.XXXXXX)
trap 'rm -rf "$TMP"' EXIT

check_set() {
    local label="$1" base="$2" log="$3"
    # 提取当前 FAIL 行: "  FAIL  name" 或 "  FAIL  name (runtime)/(compile)/(timeout)"
    grep '^  FAIL' "$log" | sed 's/^  FAIL  //' | sort > "$TMP/current.txt"

    echo "=== $label: baseline=$(wc -l < "$base") current=$(wc -l < "$TMP/current.txt") ==="

    # 新增失败 = 当前有但基线没有
    comm -23 "$TMP/current.txt" "$base" > "$TMP/added.txt"
    # 已修复 = 基线有但当前没有（好现象，仅提示）
    comm -13 "$TMP/current.txt" "$base" > "$TMP/fixed.txt"

    if [[ -s "$TMP/added.txt" ]]; then
        echo "$label GATE: FAIL (new failures detected)"
        cat "$TMP/added.txt"
        return 1
    fi

    echo "$label GATE: PASS (no new failures)"
    if [[ -s "$TMP/fixed.txt" ]]; then
        local n
        n=$(wc -l < "$TMP/fixed.txt")
        echo "$label: note, $n failures fixed (shrink vs baseline):"
        cat "$TMP/fixed.txt"
    fi
    return 0
}

rc=0

echo "=== compile ==="
DCC="$DCC" bash "$BASE_DIR/scripts/torture_compile_test.sh" > "$TMP/compile.log" 2>&1 || true
tail -3 "$TMP/compile.log"
check_set "COMPILE" "$COMPILE_BASE" "$TMP/compile.log" || rc=1

echo ""
echo "=== execute ==="
DCC="$DCC" bash "$BASE_DIR/scripts/torture_execute_test.sh" > "$TMP/execute.log" 2>&1 || true
tail -3 "$TMP/execute.log"
check_set "EXECUTE" "$EXECUTE_BASE" "$TMP/execute.log" || rc=1

exit $rc
