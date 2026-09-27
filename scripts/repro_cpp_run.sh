#!/bin/bash
# 手工复现/对照脚本: 用 dcc 编译夹具 -> g++ 链接运行 -> 打印退出码;
# 可选用 g++ -std=c++20 直接编译同一定义片段作为真值对照.
# 用法: ./scripts/repro_cpp_run.sh tests/fixtures/cpp/flat_map.cpp [extra dcc args...]
set -u
SRC="$1"; shift || true
BIN=${DCC_BIN:-./target/release/dcc}
NAME=$(basename "$SRC" .cpp)
TMP=/root/dcc/tmp_m52
mkdir -p "$TMP"
echo "=== dcc -c $SRC ==="
"$BIN" -c -o "$TMP/$NAME.dcc.o" "$SRC" "$@"
rc=$?
if [ $rc -ne 0 ]; then echo "dcc compile FAILED rc=$rc"; exit $rc; fi
echo "=== g++ link+run (dcc object) ==="
g++ -no-pie -o "$TMP/$NAME.dcc.bin" "$TMP/$NAME.dcc.o" 2>&1 | head -20
if [ ! -x "$TMP/$NAME.dcc.bin" ]; then echo "link failed"; exit 1; fi
"$TMP/$NAME.dcc.bin"; DCCCODE=$?
echo "dcc output exit code = $DCCCODE"
echo "=== g++ -std=c++20 truth (fixture alone, host headers) ==="
g++ -std=c++20 -no-pie -o "$TMP/$NAME.gxx.bin" "$SRC" 2>&1 | head -20
if [ -x "$TMP/$NAME.gxx.bin" ]; then
  "$TMP/$NAME.gxx.bin"; echo "g++ exit code = $?"
else
  echo "g++ could not build (fixture may use dcc-only headers)"
fi
