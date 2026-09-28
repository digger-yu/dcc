#!/bin/bash
# C++ Itanium ABI 交叉验证: 逐夹具分别用 g++ (真值) 与 dcc (原生后端)
# 编译运行, 对比 stdout 与退出码。
#
# 覆盖 tests/fixtures/cpp_abi_* 下的全部阶段验证域:
# - cpp_abi_virtual   (Stage 5/6: vtable / 多继承 thunk / 虚继承 chain / 对象尺寸)
# - cpp_abi_lifecycle (Stage 4: 构造析构顺序 / 委托构造 / 拷贝构造 / 全局对象初始化)
# - cpp_abi_overload  (Stage 2/4: 重载成员/构造的链接名与派发)
# - cpp_abi_rtti      (Stage 7: typeinfo / typeid / dynamic_cast)
# - cpp_abi_eh        (Stage 9: zero-cost EH 抛出-栈展开-捕获与清理顺序)
# - cpp_abi_template  (Stage 8: 模板实例的编译期常量与依赖名折叠)
#
# 新增分组目录会被自动纳入 (find cpp_abi_*/*.cpp); tests/cpp_abi_gcc_cross_check.rs
# 里的 cpp_abi_groups_are_all_covered 会强制新分组同时登记成 cargo 测试。
#
# 夹具约定: 自己 printf 出全部观察值, 不依赖返回值编码, 这样一处不一致就能
# 在 diff 里直接定位到具体偏移/槽。只看退出码会放过「算错但未触发断言」。
#
# 用法:
#   scripts/cpp_abi_gcc_cross_check.sh                    # 全量
#   scripts/cpp_abi_gcc_cross_check.sh --filter=eh_       # 子串过滤
#   DCC_BIN=./target/release/dcc scripts/cpp_abi_gcc_cross_check.sh
set -u

ROOT=$(cd "$(dirname "$0")/.." && pwd)
FIXTURE_ROOT="$ROOT/tests/fixtures"
DCC_BIN=${DCC_BIN:-"$ROOT/target/debug/dcc"}
WORK=${WORK_DIR:-"$ROOT/tmp_cpp_abi_check"}
FILTER=""

for arg in "$@"; do
  case "$arg" in
    --filter=*) FILTER=${arg#--filter=} ;;
    -h|--help)
      sed -n '1,20p' "$0"
      exit 0 ;;
    *)
      echo "未知参数: $arg" >&2
      exit 2 ;;
  esac
done

if [ ! -x "$DCC_BIN" ]; then
  echo "找不到 dcc 可执行文件: $DCC_BIN (先 cargo build, 或设 DCC_BIN)" >&2
  exit 2
fi

rm -rf "$WORK"
mkdir -p "$WORK"

CXX=${CXX:-g++}
CXXSTD=${CXXSTD:-c++17}

pass=0
fail=0
failed_names=()

# 用 find 而不是 `dir/*.cpp` 形式展开: 后者在部分 bash 配置下会把
# 未命中的目录名原样留在列表里, 把目录当夹具跑。
SRCS=()
while IFS= read -r line; do
  SRCS+=("$line")
done < <(find "$FIXTURE_ROOT" -maxdepth 2 -type f -name '*.cpp' -path '*/cpp_abi_*/*' | sort)

for src in "${SRCS[@]}"; do
  [ -e "$src" ] || continue
  name=$(basename "$src" .cpp)
  if [ -n "$FILTER" ] && [[ "$name" != *"$FILTER"* ]]; then
    continue
  fi

  # --- 真值: g++ 直接编译运行 ---
  if ! "$CXX" -std="$CXXSTD" -O0 -no-pie -o "$WORK/$name.gxx" "$src" \
      > "$WORK/$name.gxx.log" 2>&1; then
    echo "SKIP  $name  (g++ 无法编译该夹具)"
    continue
  fi
  "$WORK/$name.gxx" > "$WORK/$name.gxx.out" 2>&1
  gxx_rc=$?

  # --- 被测: dcc 出 .o, 宿主链接器链 libc++ 运行 ---
  if ! "$DCC_BIN" -c -o "$WORK/$name.dcc.o" "$src" > "$WORK/$name.dcc.log" 2>&1; then
    echo "FAIL  $name  dcc 编译失败"
    sed -e 's/^/      /' "$WORK/$name.dcc.log" | head -15
    fail=$((fail + 1))
    failed_names+=("$name(compile)")
    continue
  fi
  if ! "$CXX" -no-pie -o "$WORK/$name.dcc" "$WORK/$name.dcc.o" \
      >> "$WORK/$name.dcc.log" 2>&1; then
    echo "FAIL  $name  链接失败"
    sed -e 's/^/      /' "$WORK/$name.dcc.log" | grep -v "GNU-stack" | head -15
    fail=$((fail + 1))
    failed_names+=("$name(link)")
    continue
  fi
  "$WORK/$name.dcc" > "$WORK/$name.dcc.out" 2>&1
  dcc_rc=$?

  if [ "$gxx_rc" != "$dcc_rc" ]; then
    echo "FAIL  $name  退出码不一致 gcc=$gxx_rc dcc=$dcc_rc"
    diff <(sed -e "s/^/gcc  /" "$WORK/$name.gxx.out") \
         <(sed -e "s/^/dcc  /" "$WORK/$name.dcc.out") | sed -e 's/^/      /' | head -20
    fail=$((fail + 1))
    failed_names+=("$name(exit)")
    continue
  fi
  if ! diff -q "$WORK/$name.gxx.out" "$WORK/$name.dcc.out" > /dev/null; then
    echo "FAIL  $name  stdout 不一致 (exit=$gxx_rc)"
    echo "      --- gcc | +++ dcc"
    diff -y "$WORK/$name.gxx.out" "$WORK/$name.dcc.out" | sed -e 's/^/      /' | head -40
    fail=$((fail + 1))
    failed_names+=("$name(stdout)")
    continue
  fi
  echo "ok    $name  (exit=$gxx_rc)"
  pass=$((pass + 1))
done

echo
echo "cpp_abi gcc 交叉验证: pass=$pass fail=$fail"
if [ "$fail" -ne 0 ]; then
  printf '  失败: %s\n' "${failed_names[*]}"
  echo "  中间产物保留在 $WORK"
  exit 1
fi
rm -rf "$WORK"
exit 0
