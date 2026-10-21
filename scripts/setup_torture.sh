#!/usr/bin/env bash
# scripts/setup_torture.sh — 下载 GCC C Torture Suite 到 tests/gcc-torture
#
# 使用 git sparse-checkout 仅拉取 gcc.c-torture 目录（~5000 个 C 正确性测试），
# 再按源码里的 #include 路径补齐 gcc.c-torture 之外的少量兄弟文件。
# 幂等: 已存在则跳过，传 --force 强制重新拉取。
#
# 用法:
#   scripts/setup_torture.sh            # 首次下载
#   scripts/setup_torture.sh --force    # 删除并重新拉取
#   scripts/setup_torture.sh --update   # 拉取最新 (re-sparse-checkout)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TORTURE_DIR="$REPO_ROOT/tests/gcc-torture"
GCC_URL="https://github.com/gcc-mirror/gcc.git"

# 若仓库根目录的 tests/gcc-dg 存在, 将其软链进 suite 的
# gcc/testsuite/gcc.dg, 使 #include "../../gcc.dg/..." 的包装测试
# (pr88423 及 execute/ 下若干) 能被 dcc 编译/运行, 从而减少 skip。
link_gcc_dg() {
    local src="$REPO_ROOT/tests/gcc-dg"
    local link="$TORTURE_DIR/gcc/testsuite/gcc.dg"
    [[ -d "$src" ]] || return 0
    [[ -e "$link" || -L "$link" ]] && return 0
    ln -s "$src" "$link"
    echo "   已链接 gcc.dg -> $src (减少 gcc.dg 相关 skip)"
}

# ── 补齐 gcc.c-torture 目录之外的兄弟 include ──
# compile/execute 里有一批包装测试会 #include "../../gcc.dg/..." 或
# "../../gcc.target/..."; 这些路径不在稀疏检出的 gcc.c-torture 内, 缺文件
# 时 harness 只能整例跳过 —— CI 与本地因此跑出不同的 skip 数。这里按源码
# 中实际出现的相对路径逐个从同一 clone 取 blob (blob:none 部分克隆只在
# git show 时按需下载这几个文件)。
# 若 gcc.dg 已软链到 tests/gcc-dg(独立 clone), 该子树归外部所有, 不碰。
fetch_sibling_includes() {
    [[ -d "$TORTURE_DIR/.git" ]] || return 0
    local rel path top target
    while IFS= read -r rel; do
        [[ "$rel" == ../../* ]] || continue
        path="${rel#../../}"
        top="${path%%/*}"
        [[ -L "$TORTURE_DIR/gcc/testsuite/$top" ]] && continue
        target="$TORTURE_DIR/gcc/testsuite/$path"
        mkdir -p "$(dirname "$target")"
        if git -C "$TORTURE_DIR" show "HEAD:gcc/testsuite/$path" >"$target" 2>/dev/null; then
            echo "   同步兄弟文件: $path"
        else
            rm -f "$target"
        fi
    done < <(grep -rhoP '#\s*include\s+"\K\.\./\.\.[^"]+' \
                 "$TORTURE_DIR/gcc/testsuite/gcc.c-torture/compile" \
                 "$TORTURE_DIR/gcc/testsuite/gcc.c-torture/execute" 2>/dev/null | sort -u)
}

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
        rm -rf "$TORTURE_DIR"
    elif $update; then
        echo "== 更新 GCC C Torture Suite =="
        cd "$TORTURE_DIR"
        git fetch --depth 1 origin
        git reset --hard FETCH_HEAD
        echo "   版本: $(git rev-parse --short HEAD)"
        link_gcc_dg
        fetch_sibling_includes
        echo "✅ Torture suite 已更新"
        exit 0
    else
        count=$(find "$SUITE_DIR" -maxdepth 2 -name '*.c' | wc -l)
        echo "✅ GCC C Torture Suite 已存在 ($count 个 .c 文件)"
        echo "   路径: $TORTURE_DIR"
        echo "   版本: $(git -C "$TORTURE_DIR" rev-parse --short HEAD 2>/dev/null || echo unknown)"
        echo "   使用 --force 重新拉取，--update 拉取最新"
        link_gcc_dg
        fetch_sibling_includes
        exit 0
    fi
fi

# ── 清理残留 ──
rm -rf "$TORTURE_DIR"
mkdir -p "$(dirname "$TORTURE_DIR")"

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
echo "   版本: $(git -C "$TORTURE_DIR" rev-parse --short HEAD 2>/dev/null || echo unknown)"
link_gcc_dg
fetch_sibling_includes
