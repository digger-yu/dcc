#!/usr/bin/env bash
# 可复现的 cc1/cc1plus bootstrap 门禁 (M26.6).
#
# 用 dcc 作为 CC/CXX 配置并构建 GCC 的 cc1/cc1plus 目标, 输出进度
# 报告 (对象数 / 错误数 / 首批失败文件), 用于跟踪 dcc 对 GCC 主体
# (C/C++) 的 bootstrap 收敛程度. 无 GCC 源码树时 SKIP, 不全量构建
# 时按 xfail 记录差距, 门禁本身不因 dcc 能力不足而红.
#
# 环境变量:
#   GCC_SRC    GCC 源码树 (必须有 gcc/ 子目录才会执行)
#   DCC       dcc 二进制 (默认 $REPO/target/release/dcc)
#   JOBS       并行度 (默认 nproc)
#   BUILD_DIR  构建目录 (默认 $GCC_SRC/../dcc-bootstrap-build)
#   TIMEOUT    make 超时秒数 (默认 1800)
#
# 退出码:
#   0  SKIP (缺源码树/dcc) 或 cc1 与 cc1plus 都完整构建 (收敛)
#   2  xfail: configure/make 部分或全部失败, 但已产出进度报告
#   1  环境/脚本错误 (不可复现, 视为门禁失败)
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$SCRIPT_DIR/.." && pwd)"
GCC_SRC="${GCC_SRC:-$HOME/gcc-src}"
DCC="${DCC:-$REPO/target/release/dcc}"
JOBS="${JOBS:-$(nproc 2>/dev/null || echo 4)}"
TIMEOUT="${TIMEOUT:-1800}"
BUILD_DIR="${BUILD_DIR:-$GCC_SRC/../dcc-bootstrap-build}"

echo "== dcc cc1/cc1plus bootstrap gate =="
echo "gcc_src: $GCC_SRC"
echo "dcc:     $DCC"
echo "jobs:    $JOBS"
echo "build:   $BUILD_DIR"

# SKIP: 无 GCC 源码树或 dcc 未构建
if [[ ! -d "$GCC_SRC/gcc" ]]; then
    echo "SKIP: GCC source tree not found at $GCC_SRC"
    exit 0
fi
if [[ ! -x "$DCC" ]]; then
    echo "SKIP: dcc binary not found at $DCC (run 'cargo build --release')"
    exit 0
fi

mkdir -p "$BUILD_DIR"
OUT="$BUILD_DIR/cc1plus_progress.txt"
FIRST_FAIL="$BUILD_DIR/cc1plus_first_fail.txt"
: > "$FIRST_FAIL"

# 1. configure (CC/CXX = dcc), 只在未配置过时执行
if [[ ! -f "$BUILD_DIR/Makefile" ]]; then
    echo "== configure (CC=CXX=$DCC) =="
    CFG_LOG="$BUILD_DIR/configure.log"
    if ! (cd "$BUILD_DIR" && timeout 600 "$GCC_SRC/configure" \
            --disable-bootstrap \
            --enable-languages=c,c++ \
            --disable-multilib \
            --disable-nls \
            --disable-werror \
            CC="$DCC" CXX="$DCC" > "$CFG_LOG" 2>&1); then
        echo "configure FAILED (xfail) - log: $CFG_LOG"
        {
            echo "cc1plus bootstrap gate: xfail"
            echo "stage: configure"
            echo "result: configure failed"
            echo "log: configure.log"
        } > "$OUT"
        exit 2
    fi
fi

# 2. 构建 cc1 / cc1plus (含编译器前端依赖), 失败不中断 (-k)
echo "== make -j$JOBS -k all-gcc (timeout ${TIMEOUT}s) =="
MK_LOG="$BUILD_DIR/make-gcc.log"
timeout "$TIMEOUT" make -C "$BUILD_DIR" -j "$JOBS" -k all-gcc > "$MK_LOG" 2>&1
mk_status=$?

# 3. 汇总进度: 对象文件数, 错误数, 首批失败文件
objs=$(find "$BUILD_DIR/gcc" -name '*.o' 2>/dev/null | wc -l)
errs=$(grep -cE 'error:|Error [0-9]+' "$MK_LOG" 2>/dev/null || echo 0)
grep -oE '[A-Za-z0-9_./+-]+\.(c|cc|cpp):[0-9]+:[0-9]+: (fatal error|error):' \
    "$MK_LOG" 2>/dev/null \
    | sed -E 's/:[0-9]+:[0-9]+:.*//' | sort -u | head -10 > "$FIRST_FAIL"

CC1="$BUILD_DIR/gcc/cc1"
CC1PLUS="$BUILD_DIR/gcc/cc1plus"

# 4. 判定结果
if [[ -x "$CC1" && -x "$CC1PLUS" ]]; then
    echo "cc1/cc1plus BUILT: $CC1 $CC1PLUS"
    {
        echo "cc1plus bootstrap gate: pass"
        echo "stage: build"
        echo "result: cc1 built, cc1plus built"
        echo "objects: $objs errors: $errs"
    } > "$OUT"
    exit 0
fi

if [[ "$mk_status" -eq 124 ]]; then
    echo "build TIMED OUT after ${TIMEOUT}s (xfail) - log: $MK_LOG"
    stage="build (timeout)"
else
    echo "cc1/cc1plus NOT fully built (xfail) - log: $MK_LOG"
    stage="build"
fi
cc1_ok=missing; [[ -x "$CC1" ]] && cc1_ok=ok
cc1plus_ok=missing; [[ -x "$CC1PLUS" ]] && cc1plus_ok=ok
echo "cc1=$cc1_ok cc1plus=$cc1plus_ok objects=$objs errors=$errs"
{
    echo "cc1plus bootstrap gate: xfail"
    echo "stage: $stage"
    echo "result: cc1=$cc1_ok cc1plus=$cc1plus_ok"
    echo "objects: $objs errors: $errs"
    echo "first failures:"
    cat "$FIRST_FAIL"
} > "$OUT"
exit 2
