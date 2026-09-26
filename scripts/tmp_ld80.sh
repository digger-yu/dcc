#!/bin/bash
# 临时：ld80_arith 夹具判据（stdout 逐字符 + 退出码 双判据）。收尾时删除。
set -u
D=${DCC:-/root/dcc/target/debug/dcc}
F=${1:-/root/dcc/tests/fixtures/c/ld80_arith.c}
O=/tmp/ld80run
rm -f "$O.gcc" "$O.dcc"
gcc -std=gnu17 -O0 -no-pie -o "$O.gcc" "$F" 2>/tmp/ld80_gcc.err
grc=$?
if [ $grc -ne 0 ] || [ ! -x "$O.gcc" ]; then
    echo "GCC-BUILD-FAIL rc=$grc"; cat /tmp/ld80_gcc.err; exit 90
fi
"$O.gcc" > /tmp/ld80_gcc.out 2>&1; gout=$?
"$D" -std=gnu23 -o "$O.dcc" "$F" 2>/tmp/ld80_dcc.err
drc=$?
if [ $drc -ne 0 ] || [ ! -x "$O.dcc" ]; then
    echo "DCC-BUILD-FAIL rc=$drc"; cat /tmp/ld80_dcc.err; exit 91
fi
"$O.dcc" > /tmp/ld80_dcc.out 2>&1; dout=$?
echo "gcc: rc=$gout dcc: rc=$dout"
if diff -u /tmp/ld80_gcc.out /tmp/ld80_dcc.out > /tmp/ld80_diff.txt; then
    echo "STDOUT: IDENT"
else
    echo "STDOUT: DIFF"; cat /tmp/ld80_diff.txt
fi
echo "--- gcc out ---"; cat /tmp/ld80_gcc.out
echo "--- dcc out ---"; cat /tmp/ld80_dcc.out
[ "$gout" = "$dout" ] && [ ! -s /tmp/ld80_diff.txt ] && { echo "RESULT: PASS"; exit 0; }
echo "RESULT: FAIL"; exit 1
