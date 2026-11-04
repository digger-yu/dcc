#!/usr/bin/env bash
# 把「陷阱符号残留」清单上的对象用**指定二进制**重放到临时目录，逐条报这些符号还在不在。
#
# 为什么不直接删了 .o 让 make 重建：全量构建正在跑，写回同一路径会与 make 的读写撞车，
# 而且 kbuild 的 `if_changed` 只比命令行 —— 换了二进制它也不会重建，回填后看不出新口径。
# 重放到 /tmp 既不碰构建树，又能用与全树完全一致的命令行（同一套 -I/-include/-D/旗标），
# 是唯一能拿「这 17 个对象在新二进制上到底引用不引用」这种硬结论的口径。
#
# 用法: bash scripts/trap_symbol_replay.sh <二进制路径> [构建树=/root/pvh-lmc]
set -uo pipefail

BIN="${1:?用法: trap_symbol_replay.sh <二进制路径> [构建树]}"
KD="${2:-/root/pvh-lmc}"
OUTD="$(mktemp -d)"
trap 'rm -rf "$OUTD"' EXIT

syms='__pt_no_sw_bit|____wrong_branch_error'

mapfile -t objs < <(cd "$KD" && find . -name '*.o' -size +0 -print0 |
    xargs -0 -P2 -n60 sh -c 'for o in "$@"; do nm -u "$o" 2>/dev/null |
        grep -qE "'"$syms"'" && echo "$o"; done' _ | sort)

echo "== 待验对象 ${#objs[@]} 个，二进制=$BIN =="
"$BIN" --version 2>/dev/null | head -1

bad=0
for o in "${objs[@]}"; do
    d="$(dirname "$o")"; b="$(basename "$o")"
    cmdfile="$KD/$d/.$b.cmd"
    if [ ! -f "$cmdfile" ]; then echo "NOCMD  $o"; bad=$((bad + 1)); continue; fi
    line="$(grep -m1 '^savedcmd_.* := ' "$cmdfile" | sed 's/^savedcmd_.* := //;s/;.*$//')"
    # 输出与依赖文件都改到 /tmp，构建树一个字节都不动
    line="$(printf '%s' "$line" | sed -E "s#-o [^ ]+#-o $OUTD/x.o#; s#-Wp,-MMD,[^ ]+#-Wp,-MMD,$OUTD/x.d#")"
    (
        cd "$KD" || exit 3
        eval "set -- $line"
        env DCC_BIN="$BIN" DCC_KERNEL_ALLOW_GCC_FALLBACK=0 "$@"
    ) >"$OUTD/x.log" 2>&1
    rc=$?
    hits="$(nm -u "$OUTD/x.o" 2>/dev/null | grep -oE "$syms" | sort -u | tr '\n' ' ')"
    if [ "$rc" -ne 0 ]; then
        echo "rc=$rc   $o  （编译失败：$(grep -m1 'error' "$OUTD/x.log" | head -c 120)）"
        bad=$((bad + 1))
    elif [ -n "$hits" ]; then
        echo "残留     $o  $hits"
        bad=$((bad + 1))
    else
        echo "已折     $o"
    fi
done
echo "== 未清 = $bad / ${#objs[@]} =="
[ "$bad" -eq 0 ]
