#!/bin/bash
# scripts/check_fixtures.sh - 验证 tests/*.rs 中所有 fixtures 引用对应的文件存在
#
# 用法: scripts/check_fixtures.sh
# 退出码: 0 (全部存在), 1 (有缺失)

set -euo pipefail

errors=0
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
FIXTURES_DIR="$REPO_ROOT/tests/fixtures"
ERRORS_DIR="$FIXTURES_DIR/errors"

# 已知故意缺失的文件（如 does_not_exist.c 用于 E0100 负面测试）
declare -A INTENTIONALLY_MISSING
INTENTIONALLY_MISSING["tests/fixtures/errors/does_not_exist.c"]=1

check_file() {
    local rel_path="$1"
    local source_ref="$2"

    if [[ -v INTENTIONALLY_MISSING["$rel_path"] ]]; then
        return 0
    fi

    local full_path="$REPO_ROOT/$rel_path"
    if [[ ! -f "$full_path" ]]; then
        echo "❌ MISSING: $rel_path (referenced in $source_ref)"
        return 1
    fi
    return 0
}

# 提取 .join("...") 中的 .c/.s 文件名
extract_join() {
    local line="$1"
    # 匹配 .join("anything.c") 或 .join("anything.s")
    [[ "$line" =~ \.join\(\"([^\"]+\.(c|s))\"\) ]] && echo "${BASH_REMATCH[1]}" || true
}

echo "=== 扫描 tests/*.rs 中的 fixture 引用 ==="

for test_file in "$REPO_ROOT"/tests/*.rs; do
    [[ -f "$test_file" ]] || continue
    basename="$(basename "$test_file")"

    line_num=0
    while IFS= read -r line; do
        line_num=$((line_num + 1))

        # ── Pattern 1: include_str!("fixtures/...") ──
        if [[ "$line" =~ include_str!\(\"fixtures/([^\"]+)\"\) ]]; then
            rel="${BASH_REMATCH[1]}"
            check_file "tests/fixtures/$rel" "$basename:$line_num" || errors=$((errors + 1))
        fi

        # ── Pattern 2: ErrorCase CASES 数组中的动态构造 ──
        # name: "xxx", ..., fixture_ext: "y"
        if [[ "$line" =~ name:\ \"([^\"]+)\" ]]; then
            name="${BASH_REMATCH[1]}"
        fi
        if [[ "$line" =~ fixture_ext:\ \"([^\"]+)\" ]]; then
            ext="${BASH_REMATCH[1]}"
            if [[ -n "${name:-}" ]]; then
                check_file "tests/fixtures/errors/${name}.${ext}" "$basename:$line_num" || errors=$((errors + 1))
                name=""
            fi
        fi

        # ── Pattern 3: .join("filename.c") / .join("filename.s") ──
        if [[ "$line" =~ \.join\(\"([^\"]+\.(c|s))\"\) ]]; then
            filename="${BASH_REMATCH[1]}"
            # 检查两个可能的 fixture 位置
            found=false
            for prefix in "tests/fixtures" "tests/fixtures/errors"; do
                if [[ -f "$REPO_ROOT/$prefix/$filename" ]]; then
                    found=true
                    break
                fi
            done
            if ! $found; then
                # 仅当是 fixture 上下文时才处理（避免误报非 fixture 的 .join）
                if echo "$line" | grep -qP '(fixture|profile_src)'; then
                    # 检查是否故意缺失（如 does_not_exist.c）
                    any_intentional=false
                    for prefix in "tests/fixtures" "tests/fixtures/errors"; do
                        if [[ -v INTENTIONALLY_MISSING["$prefix/$filename"] ]]; then
                            any_intentional=true
                            break
                        fi
                    done
                    if ! $any_intentional; then
                        echo "❌ MISSING: $filename (in tests/fixtures/ or tests/fixtures/errors/), referenced in $basename:$line_num"
                        errors=$((errors + 1))
                    fi
                fi
            fi
        fi
    done < "$test_file"
done

echo ""
if [[ $errors -gt 0 ]]; then
    echo "❌ 共 $errors 个缺失的 fixture 文件！"
    exit 1
else
    echo "✅ 所有 fixture 文件均存在。"
    exit 0
fi
