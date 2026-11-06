#!/bin/bash
# 序言帧大小直方图：只统计每个函数**第一条** sub rsp，用来对比 dcc 与参考编译器
# 的帧开销（docs/dcc-kernel-build-verify-report.md §5.12）。
#
# 用法: bash scripts/kernel_frame_hist.sh <构建树> [对象子目录...]
#   bash scripts/kernel_frame_hist.sh /root/pvh-lmc kernel mm lib
# 子目录省略时默认 kernel mm lib，并只取前 120 个对象，保证两侧样本同规模。
set -uo pipefail
tree=${1:?build tree}
shift
dirs=("$@"); [ ${#dirs[@]} -gt 0 ] || dirs=(kernel mm lib)

list=$(mktemp /tmp/frame_objs.XXXXXX)
trap 'rm -f "$list"' EXIT
for d in "${dirs[@]}"; do
    find "$tree/$d" -name '*.o' 2>/dev/null
done | head -120 > "$list"
echo "samples=$(wc -l < "$list") tree=$tree"

xargs -a "$list" -P2 -I{} objdump -d -Mintel "{}" 2>/dev/null |
    awk '
        /^[0-9a-f]+ <.*>:|^[0-9a-f]+ <.*>:$/ { seen=0; next }
        /sub +rsp,0x|sub +\$0x[0-9a-f]+,%rsp/ {
            if (seen) next
            seen=1
            if (match($0, /0x[0-9a-f]+/)) print substr($0, RSTART, RLENGTH)
        }' |
    sort | uniq -c | sort -rn | head -10
