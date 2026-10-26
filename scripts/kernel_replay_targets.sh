#!/usr/bin/env bash
# 回放一批内核目标的原始 Kbuild 命令行，用当前 dcc 二进制逐个重编并按错误签名汇总。
#
# 为什么要有它：全树 `make` 一轮要跑一两个小时，而修一个缺陷只需要知道「哪些写法还过不去」。
# Kbuild 把每个目标的完整命令行记在 `.<目标>.o.cmd` 里，照原样重放就是与全树完全一致的口径
# （同一套 -I/-include/-flags、同一个 dcc-kernel-cc.sh 包装器、失败同样不回退 gcc），
# 但只跑被点名的几十个目标，几分钟就能拿到红绿与错误原文。
#
# 用法:
#   bash scripts/kernel_replay_targets.sh <目标清单文件> [并发数]
#     目标清单每行一个内核目标（相对内核树根，如 `init/main.o`），`#` 开头与空行忽略。
#     并发数默认 2（本机 7 GB 内存，再高会被 OOM 杀，产生 rc=137 的假失败）。
# 输出: 每目标一行 `OK|FAIL rc=<n> <目标>`，末尾按错误签名汇总；有 FAIL 时退出码为 1。
set -uo pipefail

KDIR="${KDIR:-/root/linux-7.1-dcc}"
WRAPPER="${WRAPPER:-/root/dcc/scripts/dcc-kernel-cc.sh}"
DCC_BIN="${DCC_BIN:-/root/dcc/target/release/dcc}"

list="${1:?用法: kernel_replay_targets.sh <目标清单文件> [并发数]}"
jobs="${2:-2}"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

replay_one() {
    local tgt="$1" kdir="$2" wrapper="$3" dccbin="$4" out="$5"
    local d b cmdfile line
    d="$kdir/$(dirname "$tgt")"
    b="$(basename "$tgt")"
    cmdfile="$d/.$b.cmd"
    if [ ! -f "$cmdfile" ]; then
        echo "NOCMD $tgt" >"$out"
        return
    fi
    line="$(grep -m1 '^savedcmd_.* := ' "$cmdfile" | sed 's/^savedcmd_.* := //')"
    (
        cd "$kdir" || exit 3
        # shellcheck disable=SC2086  # cmd 文件里已用引号分词，需交给 shell 解析
        eval "set -- $line"
        DCC_BIN="$dccbin" DCC_KERNEL_ALLOW_GCC_FALLBACK=0 "$wrapper" "$@"
    ) >"$out" 2>&1
    local rc=$?
    if [ "$rc" -eq 0 ]; then
        echo "OK $tgt" >"$out.tag"
    else
        echo "FAIL rc=$rc $tgt" >"$out.tag"
    fi
}

# 清单可能带重复与不存在的目标：先去重，再逐条判定 cmd 记录是否存在
mapfile -t targets < <(grep -vE '^\s*(#|$)' "$list" | sed 's/\s*$//' | sort -u)
echo "== 回放 ${#targets[@]} 个目标（并发 $jobs，树=$KDIR）=="

for tgt in "${targets[@]}"; do
    printf '%s\n' "$tgt" >"$tmpdir/$(echo "$tgt" | tr '/' '_').list"
done

# 并发跑：每个目标一个日志文件 + 一个 tag 文件
find "$tmpdir" -name '*.list' -print0 |
    xargs -0 -P "$jobs" -I{} bash -c '
        tgt=$(cat "{}")
        replay_one() { :; }
      ' 2>/dev/null || true

# 上面的 xargs 无法带函数，改为显式循环 + 作业控制（保持并发上限）
rm -f "$tmpdir"/*.tag
running=0
for tgt in "${targets[@]}"; do
    key="$(echo "$tgt" | tr '/' '_')"
    replay_one "$tgt" "$KDIR" "$WRAPPER" "$DCC_BIN" "$tmpdir/$key.log" &
    running=$((running + 1))
    if [ "$running" -ge "$jobs" ]; then
        wait -n
        running=$((running - 1))
    fi
done
wait

ok=0
fail=0
nocmd=0
for tgt in "${targets[@]}"; do
    key="$(echo "$tgt" | tr '/' '_')"
    if [ ! -f "$tmpdir/$key.tag" ]; then
        nocmd=$((nocmd + 1))
        echo "NOCMD $tgt"
        continue
    fi
    tag="$(cat "$tmpdir/$key.tag")"
    if [ "${tag%% *}" = "OK" ]; then
        ok=$((ok + 1))
    else
        fail=$((fail + 1))
        echo "$tag"
    fi
done

echo
echo "== 汇总: OK=$ok FAIL=$fail 无 cmd 记录=$nocmd / 共 ${#targets[@]} =="
if [ "$fail" -gt 0 ] || [ "$nocmd" -gt 0 ]; then
    echo
    echo "== 错误签名（按出现次数）=="
    cat "$tmpdir"/*.log 2>/dev/null |
        grep -hoE "^error\[[^]]+\]: .*|^- [a-z].*: not encodable by dcc|error: .*" |
        sed -e 's/[0-9]\+/N/g' |
        sort | uniq -c | sort -rn | head -30
fi
[ "$fail" -eq 0 ] && [ "$nocmd" -eq 0 ]
