#!/bin/bash
# 用内核构建树里现成的 .cmd 旗标编译一个临时 C 文件（不进 kbuild）：
# 借 net/mac80211/.agg-rx.o.cmd 的完整 include/宏/选项，把输入源换成用户给的
# 文件、输出换成 -o 指定的位置。用于把内核现场缩成几行夹具。
#
# 用法: dcc_kernel_tu.sh <源码.c> <输出.o> [构建树]
set -uo pipefail

src=${1:?source c file}
out=${2:?output object}
build=${3:-/root/pvh-lmc}
cmd_file="$build/net/mac80211/.agg-rx.o.cmd"

cmd=$(grep -m1 '^savedcmd' "$cmd_file" | sed -E 's/^savedcmd[^=]*:=[[:space:]]*//')
cmd=${cmd%%;*}
# 换源文件、换输出；依赖文件也挪到 /dev/null，避免污染 kbuild 的 .d
cmd=$(printf '%s' "$cmd" | sed -E "s# -o [^ ]+ # -o $out #; s#/root/linux-pvh-src/net/mac80211/agg-rx\.c#$src#; s#-Wp,-MMD,[^ ]+#-Wp,-MMD,/dev/null#")

cd "$build" || exit 2
eval "$cmd"
rc=$?
echo "rc=$rc out=$out"
exit $rc
