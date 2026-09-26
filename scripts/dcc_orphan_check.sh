#!/bin/bash
# scripts/dcc_orphan_check.sh —— 检测从未参与编译的孤儿模块 / 影子模块
#
# 架构原则：`src/` 下任何 .rs 都必须被某个父模块显式 `mod xxx;` 声明，
# 否则它根本不参与编译（历史搬迁遗留的影子副本）。死副本会让 ABI 语义
# 出现两个真源，改一处漏一处。
#
# 判定规则（复刻 rustc 模块解析，适配本仓库 mod.rs 约定）：
#   <dir>/<name>.rs 存活 <=> 存在祖先目录 D（含 <dir> 自身），使以下任一文件
#   含 `mod <name>;`：  D/mod.rs  D/lib.rs  D/main.rs  D.rs
#
# 用法: scripts/dcc_orphan_check.sh [目录...]   默认 src/
# 退出码: 0 = 无孤儿; 1 = 发现孤儿
set -u

DIRS=("$@")
[ ${#DIRS[@]} -eq 0 ] && DIRS=(src)

declared_in() { # <file> <modname>
    [ -f "$1" ] || return 1
    grep -qE "^[[:space:]]*(pub([[:space:]]*\([^)]*\))?[[:space:]]*)*mod[[:space:]]+$2[[:space:]]*;" "$1"
}

is_live() { # <file>
    local name dir
    name=$(basename "$1" .rs)
    case "$name" in mod|lib|main|build) return 0 ;; esac
    dir=$(dirname "$1")
    while :; do
        declared_in "$dir/mod.rs" "$name" && return 0
        declared_in "$dir/lib.rs" "$name" && return 0
        declared_in "$dir/main.rs" "$name" && return 0
        declared_in "$dir.rs" "$name" && return 0
        local parent
        parent=$(dirname "$dir")
        [ "$parent" = "$dir" ] && return 1
        dir="$parent"
    done
}

orphans=0
while IFS= read -r f; do
    if grep -rq "include!(.*$(basename "$f")" --include='*.rs' "${DIRS[@]}" 2>/dev/null; then
        continue # include! 注入的文件不算孤儿
    fi
    if ! is_live "$f"; then
        printf 'ORPHAN  %s\n' "$f"
        orphans=$((orphans + 1))
    fi
done < <(find "${DIRS[@]}" -name '*.rs' | sort)

echo "----------------------------------------"
if [ "$orphans" -eq 0 ]; then
    echo "OK: 无孤儿模块"
    exit 0
fi
echo "FAIL: $orphans 个孤儿模块"
exit 1
