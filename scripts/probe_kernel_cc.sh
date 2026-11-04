#!/usr/bin/env bash
# 用真树头文件编译一个探针 .c（沿用 8250_platform.o 的完整命令行），只报陷阱符号引用数。
# 用法: probe_kernel_cc.sh <探针.c> [符号正则=wrong_branch|__pt_no_sw_bit]
set -uo pipefail
src="${1:?用法: probe_kernel_cc.sh <探针.c> [符号正则]}"
sym="${2:-wrong_branch|__pt_no_sw_bit}"
KD="${KD:-/root/pvh-lmc}"
CMD="${CMD:-drivers/tty/serial/8250/.8250_platform.o.cmd}"
BIN="${DCC_BIN:-/root/dcc/target/debug/dcc}"
out="/tmp/$(basename "$src" .c).o"
# 末尾的源文件路径要摘掉（本脚本自己把探针 .c 拼回命令行尾部）。分两段 sed：前一段要展开
# `$out` 因此必须用双引号，后一段的行尾锚点 `s#...$##` 在双引号里会被 bash 把 `$#` 当位置参数
# 展开（写成 `\$` 又会让 sed 把 `$` 当字面量），所以单独用单引号那一刀；savedcmd 行尾带多个
# 空格，`${line% *}` 只会删掉一个空格，不可靠。
line="$(grep -m1 '^savedcmd_.* := ' "$KD/$CMD" | sed 's/^savedcmd_.* := //;s/;.*$//' |
    sed -E "s#-o [^ ]+#-o $out#; s#-Wp,-MMD,[^ ]+#-Wp,-MMD,/tmp/probe.d#" |
    sed -E 's#[^[:space:]]*\.c[[:space:]]*$##')"
eval "set -- $line"
# 行尾留个空格再拼源文件，保证与 savedcmd 的相对路径解析一致
(cd "$KD" && env DCC_BIN="$BIN" DCC_KERNEL_ALLOW_GCC_FALLBACK=0 "$@" "$src") >/tmp/probe_cc.log 2>&1
rc=$?
if [ $rc -ne 0 ]; then
    echo "rc=$rc 编译失败: $(grep -m1 -E '^error|error:' /tmp/probe_cc.log | head -c 160)"
    exit $rc
fi
n="$(nm -u "$out" 2>/dev/null | grep -cE "$sym")"
echo "$(basename "$src") [$BIN] 引用=$n"
[ "$n" -eq 0 ]
