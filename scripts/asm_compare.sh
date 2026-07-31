#!/usr/bin/env bash
# scripts/asm_compare.sh - dcc vs gcc 汇编对比工具
# 用法: ./scripts/asm_compare.sh <file.c> [dcc-args...]
# 示例: ./scripts/asm_compare.sh examples/basics/arith.c
#       ./scripts/asm_compare.sh tests/fixtures/c/promotion.c -std=gnu23

set -euo pipefail

if [ $# -lt 1 ]; then
    echo "用法: $0 <file.c> [dcc-extra-args...]" >&2
    exit 1
fi

SRC="$1"; shift
BASENAME="$(basename "$SRC" .c)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

DCC="${DCC:-./target/release/dcc}"
GCC="${GCC:-gcc}"

# 编译生成汇编
"$DCC" -S -o "$TMPDIR/dcc.s" "$@" "$SRC" 2>"$TMPDIR/dcc.err" || {
    echo "=== dcc 编译失败 ===" >&2
    cat "$TMPDIR/dcc.err" >&2
    exit 1
}
"$GCC" -S -O0 -o "$TMPDIR/gcc.s" "$SRC" 2>/dev/null || {
    echo "=== gcc 编译失败 ===" >&2
    exit 1
}

# 过滤: 去掉 .cfi / .file / .ident / .section / .align / 空行等噪音
strip_noise() {
    sed '/\.section\s*\.note/,/^\s*$/d' "$1" \
    | grep -vE '^\s*(\.cfi|\.file|\.ident|\.align|\.p2align|\.LFB[0-9]|\.LFE[0-9]|\.long\s+[0-9]f|^[0-9]:|\s*$)'
}

strip_noise "$TMPDIR/gcc.s" > "$TMPDIR/gcc_clean.s"
strip_noise "$TMPDIR/dcc.s" > "$TMPDIR/dcc_clean.s"

# 统计
DCC_LINES=$(wc -l < "$TMPDIR/dcc_clean.s")
GCC_LINES=$(wc -l < "$TMPDIR/gcc_clean.s")
echo "=== $SRC ==="
echo "gcc: $GCC_LINES 行  |  dcc: $DCC_LINES 行"
echo ""

# 并排对比 (width=160, 左=gcc 右=dcc)
if command -v diff &>/dev/null; then
    diff --side-by-side -W 160 --suppress-common-lines \
        --label "gcc -O0" --label "dcc" \
        "$TMPDIR/gcc_clean.s" "$TMPDIR/dcc_clean.s" || true
fi

echo ""
echo "原始汇编: $TMPDIR/gcc.s (gcc)  $TMPDIR/dcc.s (dcc)"
echo "提示: 设置 DCC=/path/to/dcc 或 GCC=/path/to/gcc 可切换编译器"
