#!/usr/bin/env bash
# scripts/setup_torture.sh — 下载 GCC C Torture Suite 到 third_party/gcc-torture
#
# 使用 git sparse-checkout 仅拉取 gcc.c-torture 目录（~5000 个 C 正确性测试）。
# 幂等: 已存在则跳过，传 --force 强制重新拉取。
#
# 用法:
#   scripts/setup_torture.sh            # 首次下载
#   scripts/setup_torture.sh --force    # 删除并重新拉取
#   scripts/setup_torture.sh --update   # 拉取最新 (re-sparse-checkout)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TORTURE_DIR="$REPO_ROOT/third_party/gcc-torture"
GCC_URL="https://gcc.gnu.org/git/gcc.git"

# ── 参数处理 ──
force=false
update=false
for arg in "$@"; do
    case "$arg" in
        --force)  force=true ;;
        --update) update=true ;;
        *)        echo "未知参数: $arg"; exit 1 ;;
    esac
done

# ── 已存在检查 ──
SUITE_DIR="$TORTURE_DIR/gcc/testsuite/gcc.c-torture"
if [[ -d "$SUITE_DIR/execute" && -n "$(ls -A "$SUITE_DIR/execute" 2>/dev/null)" ]]; then
    if $force; then
        echo "== 强制重新拉取 GCC C Torture Suite =="
        rm -rf "$REPO_ROOT/third_party"
    elif $update; then
        echo "== 更新 GCC C Torture Suite =="
        cd "$TORTURE_DIR"
        git fetch --depth 1 origin
        git reset --hard FETCH_HEAD
        echo "✅ Torture suite 已更新"
        exit 0
    else
        count=$(find "$SUITE_DIR" -maxdepth 2 -name '*.c' | wc -l)
        echo "✅ GCC C Torture Suite 已存在 ($count 个 .c 文件)"
        echo "   路径: $TORTURE_DIR"
        echo "   使用 --force 重新拉取，--update 拉取最新"
        exit 0
    fi
fi

# ── 清理残留 ──
rm -rf "$REPO_ROOT/third_party"
mkdir -p "$REPO_ROOT/third_party"

echo "== 下载 GCC C Torture Suite (sparse clone) =="
echo "   来源: $GCC_URL"

# ── Sparse shallow clone ──
git clone --depth 1 --filter=blob:none --sparse \
    "$GCC_URL" "$TORTURE_DIR" 2>&1

cd "$TORTURE_DIR"
git sparse-checkout set \
    gcc/testsuite/gcc.c-torture/execute \
    gcc/testsuite/gcc.c-torture/compile

# ── 统计 ──
execute=$(find gcc/testsuite/gcc.c-torture/execute -maxdepth 1 -name '*.c' | wc -l)
compile=$(find gcc/testsuite/gcc.c-torture/compile  -maxdepth 1 -name '*.c' | wc -l)

echo ""
echo "✅ GCC C Torture Suite 下载完成"
echo "   execute: $execute 个测试"
echo "   compile: $compile 个测试"
echo "   路径: $TORTURE_DIR"
