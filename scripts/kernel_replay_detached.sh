#!/usr/bin/env bash
# 重放**单个**内核 TU 的原始 Kbuild 命令行，但把产物与依赖文件改道到 /tmp。
#
# 为什么需要它（而不是直接用 `kernel_replay_targets.sh`）：那条是批量体检，按原命令行
# 把 `.o` 写回树上；而**正在跑的 `make` 也在写同名 `.o`** —— 重放写进去会留下两种后果：
#   1. make 已把这个 `.o` 记成"本轮已构建"，重放产物与真正的 recipe 产物混在一起；
#   2. 更糟的是重放只跑 `cmd_cc_o_c` 那一段，不跑 objtool，于是树里留下一个没有 ORC
#      数据的 `.o`，后面的 `vmlinux.o` 链接把错的东西吃进去。
# 本脚本把 `-o` 与 `-Wp,-MMD` 都改到 /tmp，树上什么都不写，只读一份 `.<目标>.o.cmd`。
#
# 另一个用途：验证「警告与 gcc 对齐」。整树日志太大不好定位，单 TU 重放能直接给出
# 各诊断类别的条数（W0501/W1015/W0202/unknown -m|-f/W0010/error）与产物里遗留的
# 「只有声明没有定义」的编译期陷阱引用（`__bad_*`/`__field_overflow`/`___once_key`）。
#
# 用法:
#   bash scripts/kernel_replay_detached.sh <BUILD 目录> <目标（相对 BUILD，如 kernel/signal.o）> [dcc 二进制]
# 输出: rc + 各诊断类别计数 + 遗留陷阱引用；退出码就是 dcc 的退出码。
#
# 已知边界:
#   * 只重放编译那一段：`.cmd` 里 `cmd_cc_o_c` 与 objtool 常常写在一行（用 `;` 分隔），
#     本脚本在第一个 `;` 处截断 —— 所以产物**没有** objtool 后处理，不能拿它当最终产物用。
#   * `savedcmd_` 行里的相对路径以 BUILD 目录为基准，故必须 `cd` 过去再 eval。
set -uo pipefail

bdir="${1:?BUILD 目录（O= 的那个）}"
tgt="${2:?目标，相对 BUILD，如 kernel/signal.o}"
dccbin="${3:-$HOME/dcc/target/release/dcc}"

d="$bdir/$(dirname "$tgt")"
b="$(basename "$tgt")"
cmdfile="$d/.$b.cmd"
if [[ ! -f "$cmdfile" ]]; then
    echo "没有 cmd 文件: $cmdfile（该目标还没被构建过，或名字写错）" >&2
    exit 3
fi
if [[ ! -x "$dccbin" ]]; then
    echo "dcc 二进制不可执行: $dccbin" >&2
    exit 3
fi

out="/tmp/dcc-detached-$b.o"
dep="/tmp/dcc-detached-$b.d"
# `-o` 与 `-Wp,-MMD` 的替换都要**前后带空格**，否则 token 会被粘连成
# `/…/dcc-kernel-cc.sh.foo.o.d` 这种 `env: 没有那个文件或目录`(rc=127)。
line="$(grep -m1 '^savedcmd_.* := ' "$cmdfile" \
    | sed -e 's/^savedcmd_.* := //' -e 's/;.*$//' \
          -e "s|-Wp,-MMD,[^ ]*\.d|-Wp,-MMD,$dep|g" \
          -e "s| -o ${tgt} | -o $out |g")"

cd "$bdir" || exit 3
# shellcheck disable=SC2086  # cmd 文件里已按 shell 词法记录，交 shell 解析
eval "set -- $line"
err="/tmp/dcc-detached-$b.err"
env DCC_BIN="$dccbin" DCC_KERNEL_ALLOW_GCC_FALLBACK=0 "$@" >/dev/null 2>"$err"
rc=$?

echo "rc=$rc  目标=$tgt  产物=$out  诊断=$err"
for k in "warning\[W0501\]" "warning\[W1015\]" "warning\[W0010\]" "W0202" \
         "unknown -[mf] option ignored" "GCC-DELEGATE" "^error"; do
    printf '  %-32s %s\n' "$k" "$(grep -cE "$k" "$err")"
done
# 编译期陷阱引用：这些函数在头文件里只有声明（靠「永远不发出引用」保证链得上），
# 产物里只要出现就是链接期的 undefined reference。
if [[ -f "$out" ]]; then
    left="$(nm -u "$out" 2>/dev/null | grep -E "___once_key|__bad_|__field_overflow|____wrong_branch|__compiletime_assert|link_error" | awk '{print $2}' | tr '\n' ' ')"
    printf '  %-32s %s\n' "遗留的编译期陷阱引用" "${left:-（无）}"
fi
exit $rc
