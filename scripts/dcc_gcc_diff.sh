#!/usr/bin/env bash
# scripts/dcc_gcc_diff.sh —— gcc 与 dcc 编译同一份 C 源码后的运行时差分 oracle。
#
# 为什么需要它：仓库既有的 harness 全部是"dcc 自证返回 0"，对 REX/ModRM 位错、
# 基址寄存器选错、槽推进量算错这类 ABI 缺陷结构性失明（实测 817 个 integration
# 用例全绿的同时，va_arg 与 long double 存在 5 类真实缺陷）。唯一有鉴别力的
# 判据是"与真实编译器跑同样的输入得到同样的结果"。
#
# 用法:
#   scripts/dcc_gcc_diff.sh <file.c> [dcc 额外参数...]      单文件
#   scripts/dcc_gcc_diff.sh --mix <callee.c> <caller.c>     跨编译器混合链接
#
# 退出码: 0 一致 / 1 不一致 / 90 gcc 编译失败 / 91 dcc 编译失败
set -u
DCC=${DCC:-/root/dcc/target/release/dcc}
GCC=${GCC:-gcc}
WORK=${TMPDIR:-/tmp}/dcc-gcc-diff.$$
mkdir -p "$WORK"
trap 'rm -rf "$WORK"' EXIT

run_case() {
  local f="$1"; shift
  local b; b=$(basename "$f" .c)
  if ! "$GCC" -std=gnu17 -no-pie -O0 -o "$WORK/$b.gcc" "$f" 2>"$WORK/g.log"; then
    echo "[GCC BUILD FAIL] $f"; sed -n 1,20p "$WORK/g.log"; return 90
  fi
  if ! "$DCC" -std=gnu23 -o "$WORK/$b.dcc" "$f" "$@" 2>"$WORK/d.log"; then
    echo "[DCC BUILD FAIL] $f"; sed -n 1,20p "$WORK/d.log"; return 91
  fi
  local g d
  ( cd "$WORK" && timeout 10 "./$b.gcc" >g.out 2>&1 ); g=$?
  ( cd "$WORK" && timeout 10 "./$b.dcc" >d.out 2>&1 ); d=$?
  if [ "$g" = "$d" ] && cmp -s "$WORK/g.out" "$WORK/d.out"; then
    echo "MATCH    $f (exit=$g)"
    return 0
  fi
  echo "MISMATCH $f (gcc exit=$g, dcc exit=$d)"
  if ! cmp -s "$WORK/g.out" "$WORK/d.out"; then
    echo "  --- gcc stdout ---"; sed -n 1,10p "$WORK/g.out"
    echo "  --- dcc stdout ---"; sed -n 1,10p "$WORK/d.out"
  fi
  return 1
}

# 跨编译器混合链接: callee 由其中一个编译器编译, caller(含 main) 由另一个编译。
# 这是 ABI 边界最直接的判据 —— 只有一侧错时结果必然偏离"两侧同编译器"的参照。
run_mix() {
  local callee="$1" caller="$2" b; b=$(basename "$caller" .c)
  local cc_obj cu_obj gcc_ref mixed
  "$GCC" -std=gnu17 -no-pie -c -o "$WORK/callee.gcc.o" "$callee" 2>"$WORK/m.log" || { echo "[GCC BUILD FAIL] $callee"; return 90; }
  "$GCC" -std=gnu17 -no-pie -c -o "$WORK/caller.gcc.o" "$caller" 2>>"$WORK/m.log" || { echo "[GCC BUILD FAIL] $caller"; return 90; }
  "$DCC" -std=gnu23 -c -o "$WORK/callee.dcc.o" "$callee" 2>>"$WORK/m.log" || { echo "[DCC BUILD FAIL] $callee"; return 91; }
  "$DCC" -std=gnu23 -c -o "$WORK/caller.dcc.o" "$caller" 2>>"$WORK/m.log" || { echo "[DCC BUILD FAIL] $caller"; return 91; }
  "$GCC" -no-pie -o "$WORK/ref" "$WORK/callee.gcc.o" "$WORK/caller.gcc.o" 2>>"$WORK/m.log" || { echo "[LINK FAIL]"; return 90; }
  "$GCC" -no-pie -o "$WORK/mix" "$WORK/callee.dcc.o" "$WORK/caller.gcc.o" 2>>"$WORK/m.log" || { echo "[LINK FAIL]"; return 90; }
  # 必须同时比退出码与 stdout: 这类 callee 把期望值与实际值打成文本后让 main
  # 恒返回 0, 只比退出码会把错判成 MATCH (与夹具头里批评 assert_eq!(code,0) 同一类失明)。
  ( cd "$WORK" && timeout 10 ./ref >ref.out 2>&1 ); gcc_ref=$?
  ( cd "$WORK" && timeout 10 ./mix  >mix.out 2>&1 ); mixed=$?
  if [ "$gcc_ref" = "$mixed" ] && cmp -s "$WORK/ref.out" "$WORK/mix.out"; then
    echo "MATCH    mix($b: dcc callee + gcc caller) exit=$mixed  [ref=$gcc_ref]"
    return 0
  fi
  echo "MISMATCH mix($b: dcc callee + gcc caller) exit=$mixed  [all-gcc ref=$gcc_ref]"
  if ! cmp -s "$WORK/ref.out" "$WORK/mix.out"; then
    echo "  --- all-gcc ref stdout ---"; sed -n 1,12p "$WORK/ref.out"
    echo "  --- mixed (dcc callee) stdout ---"; sed -n 1,12p "$WORK/mix.out"
  fi
  return 1
}

if [ "${1:-}" = "--mix" ]; then shift; run_mix "$@"; exit $?; fi
rc=0
for f in "$@"; do
  case "$f" in -*) continue;; esac
  run_case "$f" || rc=1
done
exit $rc
