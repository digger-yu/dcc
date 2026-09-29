#!/usr/bin/env bash
# 用 dcc 批量编译 GCC 源码树中的 C++ 翻译单元，暴露 dcc C++ 前端缺陷。
#
# 这是"缺陷发现仪器"而不是构建系统：它按组件收集 (源文件, 编译参数) 对，
# 用 dcc -c 逐个编译，把 stderr 落盘，再按错误消息聚合出频次排行，从而决定
# 下一轮该修哪个前端缺口。同一文件列表可以交给 g++ (--compiler=g++) 做真值
# 对照，用来区分"dcc 缺陷"与"该 TU 本身就编不过"。
#
# 组件 (--component):
#   libcpp      GCC 预处理器库 (libcpp/*.cc)，cc1 的硬依赖
#   generator   gcc 主机生成器 (gcc/gen*.cc)
#   gcc-cpp     gcc middle-end 主体 (gcc/*.cc)，规模最大
#   gcc-cp      gcc/cp C++ 前端 (gcc/cp/*.cc)
#   gcc-c       gcc/c C 前端 (gcc/c/*.cc)
#
# 用法:
#   scripts/gcc_tree_cpp_probe.sh --component=libcpp
#   scripts/gcc_tree_cpp_probe.sh --component=generator --filter=genmodes
#   scripts/gcc_tree_cpp_probe.sh --component=gcc-cpp --top=20
#   scripts/gcc_tree_cpp_probe.sh --component=libcpp --compiler=g++
#
# 环境变量:
#   GCC_SRC   GCC 源码根 (默认 /root/gcc-15.3.0)
#   DCC_BIN   dcc 二进制 (默认 <repo>/target/release/dcc)
#   JOBS      并行度 (默认 nproc)
#   WORK_DIR  日志目录 (默认 <repo>/.work_tmp/gcc-cpp-probe)
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SCRIPT_DIR/.." && pwd)"
GCC_SRC="${GCC_SRC:-/root/gcc-15.3.0}"
DCC_BIN="${DCC_BIN:-$REPO/target/release/dcc}"
JOBS="${JOBS:-$(nproc 2>/dev/null || echo 4)}"
WORK_DIR="${WORK_DIR:-$REPO/.work_tmp/gcc-cpp-probe}"
BUILDDIR="$GCC_SRC/build"

COMPONENT=""
FILTER=""
TOP=15
COMPILER="dcc"
LIST_ONLY=0

for arg in "$@"; do
    case "$arg" in
        --component=*) COMPONENT="${arg#*=}" ;;
        --filter=*)    FILTER="${arg#*=}" ;;
        --top=*)       TOP="${arg#*=}" ;;
        --compiler=*)  COMPILER="${arg#*=}" ;;
        --list)        LIST_ONLY=1 ;;
        -h|--help)     sed -n '1,29p' "$0"; exit 0 ;;
        *) echo "未知参数: $arg" >&2; exit 2 ;;
    esac
done

# 各组件的编译参数取自 build/ 下 Makefile 的实际展开（去掉 -W* 噪声与 -g）。
gcc_host_flags() {
    printf '%s\0' \
        -DIN_GCC -fno-exceptions -fno-rtti -DHAVE_CONFIG_H \
        -I"$BUILDDIR/gcc" -I"$GCC_SRC/gcc" -I"$GCC_SRC/include" \
        -I"$GCC_SRC/libcpp/include"
}
component_flags() {
    case "$COMPONENT" in
        libcpp)
            printf '%s\0' -DHAVE_CONFIG_H \
                -I"$BUILDDIR/libcpp" -I"$GCC_SRC/libcpp" -I"$GCC_SRC/include" \
                -I"$GCC_SRC/libcpp/include" ;;
        generator)
            gcc_host_flags; printf '%s\0' -DGENERATOR_FILE ;;
        gcc-cpp|gcc-cp|gcc-c) gcc_host_flags ;;
        *) echo "未知组件: $COMPONENT" >&2; exit 2 ;;
    esac
}

component_dir_glob() {
    case "$COMPONENT" in
        libcpp)    printf '%s\n%s\n' "$GCC_SRC/libcpp" '*.cc' ;;
        generator) printf '%s\n%s\n' "$GCC_SRC/gcc" 'gen*.cc' ;;
        gcc-cpp)   printf '%s\n%s\n' "$GCC_SRC/gcc" '*.cc' ;;
        gcc-cp)    printf '%s\n%s\n' "$GCC_SRC/gcc/cp" '*.cc' ;;
        gcc-c)     printf '%s\n%s\n' "$GCC_SRC/gcc/c" '*.cc' ;;
    esac
}

BIN="$DCC_BIN"
if [[ "$COMPILER" == "g++" ]]; then
    BIN="g++"
elif [[ ! -x "$DCC_BIN" ]]; then
    echo "找不到 dcc: $DCC_BIN (先 cargo build --release)" >&2
    exit 2
fi

mapfile -t _dirglob < <(component_dir_glob)
SRC_DIR="${_dirglob[0]}"
SRC_GLOB="${_dirglob[1]}"

SOURCES=()
while IFS= read -r f; do
    [[ -n "$FILTER" && "$f" != *"$FILTER"* ]] && continue
    SOURCES+=("$f")
done < <(find "$SRC_DIR" -maxdepth 1 -name "$SRC_GLOB" | sort)

total=${#SOURCES[@]}
if (( total == 0 )); then
    echo "没有匹配到源文件 (component=$COMPONENT filter=${FILTER:-<none>})"
    exit 1
fi
if [[ "$LIST_ONLY" == 1 ]]; then
    printf '%s\n' "${SOURCES[@]}"
    exit 0
fi

OUT="$WORK_DIR/$COMPONENT${FILTER:+_$FILTER}"
rm -rf "$OUT"; mkdir -p "$OUT"

FLAGS=()
while IFS= read -r -d '' a; do FLAGS+=("$a"); done < <(component_flags)
printf '%s\n' "${FLAGS[@]}" > "$OUT/flags.txt"
: > "$OUT/pass.txt"; : > "$OUT/fail.txt"

run_one() {
    local src="$1" obj log rc
    obj="$(basename "$src" .cc)"
    log="$OUT/$obj.log"
    # shellcheck disable=SC2206  # PROBE_FLAGS 是 printf %q 生成的, 此处必须做词法还原
    local flags=( $PROBE_FLAGS )
    "$BIN" -c "${flags[@]}" -o "$OUT/$obj.o" "$src" > "$log" 2>&1
    rc=$?
    if (( rc == 0 )) && [[ -s "$OUT/$obj.o" ]]; then
        echo "$obj" >> "$OUT/pass.txt"
    else
        echo "$obj" >> "$OUT/fail.txt"
    fi
    rm -f "$OUT/$obj.o"
}
export -f run_one
# 注意: FLAGS 在本脚本里是数组, bash 不允许把同名变量再当标量导出 (导出会静默
# 丢值), 所以并行子进程只能收到一个 %q 编码过的字符串副本。
export BIN OUT
export PROBE_FLAGS="$(printf '%q ' "${FLAGS[@]}")"

printf '%s\n' "${SOURCES[@]}" | xargs -P "$JOBS" -I{} \
    bash -c 'run_one "$@"' _ {}

pass_count=$(wc -l < "$OUT/pass.txt")
fail_count=$(wc -l < "$OUT/fail.txt")

echo "== gcc_tree_cpp_probe: component=$COMPONENT compiler=$BIN =="
echo "总数: $total  通过: $pass_count  失败: $fail_count"
echo "日志: $OUT"

if (( fail_count > 0 )); then
    echo "--- 失败文件 ---"
    sort -u "$OUT/fail.txt" | tr '\n' ' '; echo
    echo "--- 每个失败 TU 的首条错误 ---"
    : > "$OUT/first-errors.txt"
    while IFS= read -r f; do
        msg=$(grep -m1 -E '^error|^warning: .*not supported|panic' "$OUT/$f.log" 2>/dev/null)
        echo "$f: ${msg:-<无 error 行, 见日志>}" >> "$OUT/first-errors.txt"
    done < <(sort -u "$OUT/fail.txt")
    cat "$OUT/first-errors.txt"
    echo "--- 错误消息排行 (top $TOP) ---"
    sed -E 's/^[^:]+: //' "$OUT/first-errors.txt" | sort | uniq -c | sort -rn | head -"$TOP"
fi
exit 0
