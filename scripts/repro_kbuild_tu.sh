#!/bin/bash
# 复现单个 kbuild TU：读取构建树里的 .<obj>.cmd savedcmd，改写 -o 到指定输出，
# 在构建树目录下重放该编译命令。用于把链接期缺陷缩小到单个 TU。
#
# 用法: repro_kbuild_tu.sh <构建树> <对象相对路径> [输出对象]
set -uo pipefail

build=${1:?build dir}
rel=${2:?object relative path}
out=${3:-/tmp/$(basename "$rel")}

cmd_file="$build/$(dirname "$rel")/.$(basename "$rel").cmd"
[ -f "$cmd_file" ] || { echo "missing $cmd_file" >&2; exit 2; }

cmd=$(grep -m1 '^savedcmd' "$cmd_file" | sed -E 's/^savedcmd[^=]*:=[[:space:]]*//')
[ -n "$cmd" ] || { echo "no savedcmd in $cmd_file" >&2; exit 2; }

# savedcmd 形如 `cc … -o <obj> ; ./tools/objtool/objtool … <obj>`：
# 只保留编译段（objtool 会就地改写对象，复现时不需要）
cmd=${cmd%%;*}

# 把 "-o <原对象>" 换成新输出，避免覆盖构建树产物
cmd=$(printf '%s' "$cmd" | sed -E "s# -o [^ ]+ # -o $out #")

cd "$build" || exit 2
eval "$cmd"
rc=$?
echo "rc=$rc out=$out"
exit $rc
