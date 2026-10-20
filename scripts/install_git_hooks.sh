#!/usr/bin/env bash
# scripts/install_git_hooks.sh — 把 scripts/git-hooks/ 下的钩子装进本仓库 .git/hooks/
#
# 用法：bash scripts/install_git_hooks.sh [--force]
#
# 只做「复制 + 加执行位」这一件事，**不碰任何 git 配置**：本仓库禁止改 git config
# （包括 `core.hooksPath`），所以钩子以文件形式落到 .git/hooks/ 而不是指向别处。
# 复制而非符号链接：这样 .git/hooks/pre-commit 自成一体，删掉仓库源码也不会留下
# 断链的钩子。
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC_DIR="$REPO_ROOT/scripts/git-hooks"
HOOKS_DIR="$(git -C "$REPO_ROOT" rev-parse --git-path hooks)"
case "$HOOKS_DIR" in
    /*) ;;                      # 绝对路径直接用
    *)  HOOKS_DIR="$REPO_ROOT/$HOOKS_DIR" ;;   # 相对仓库根展开
esac

FORCE=0
[[ "${1:-}" == "--force" ]] && FORCE=1
mkdir -p "$HOOKS_DIR"

shopt -s nullglob
installed=0
for src in "$SRC_DIR"/*; do
    name=$(basename "$src")
    dst="$HOOKS_DIR/$name"
    if [[ -e "$dst" ]] && ! cmp -s "$src" "$dst"; then
        if (( ! FORCE )); then
            echo "跳过 $name：已有一个内容不同的 $dst（要覆盖用 --force）" >&2
            continue
        fi
        cp -f "$dst" "$dst.orig.$(date +%Y%m%d-%H%M%S)"
        echo "  已备份原文件为 $dst.orig.*"
    fi
    install -m 755 "$src" "$dst"
    echo "已安装 $name -> $dst"
    installed=$((installed+1))
done
shopt -u nullglob

(( installed > 0 )) || { echo "$SRC_DIR 下没有钩子文件" >&2; exit 1; }
echo "完成。卸载：删除 $HOOKS_DIR/pre-commit 即可（git 的 --no-verify 也能单次绕过，但会留在记录里）。"
