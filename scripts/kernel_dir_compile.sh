#!/usr/bin/env bash
# 批量编译 kernel/ 与 kernel/sched/ 下的 C 文件 (复用 core.o 的命令模板)
# 用法: bash scripts/kernel_dir_compile.sh [kernel_dir]
set -u
KERNEL_DIR="${1:-/home/di/linux-7.1}"
DCC="${DCC:-/home/di/dcc/target/release/dcc}"
CC_WRAP="${CC_WRAP:-/home/di/dcc/scripts/dcc-kernel-cc.sh}"
OUT_LOG="${OUT_LOG:-/tmp/kernel_dir_compile.log}"
JOBS="${JOBS:-$(nproc 2>/dev/null || echo 8)}"

cd "$KERNEL_DIR" || exit 1
CMD_TEMPLATE=$(sed 's/^savedcmd_//' kernel/sched/.core.o.cmd 2>/dev/null | head -1 | sed 's/^kernel\/sched\/core.o := //')
if [ -z "$CMD_TEMPLATE" ]; then
    echo "cannot load command template" >&2
    exit 1
fi

# 提取公共前缀 (到 -c 之前), 并去掉开头的 CC_WRAP 自身路径
# (模板首词是 wrapper 脚本, 直接传入会让 dcc 误判为输入文件)
PREFIX="${CMD_TEMPLATE% -c *}"
PREFIX="${PREFIX#"$CC_WRAP" }"

: > "$OUT_LOG"
compile_one() {
    local src="$1" base out
    base=$(basename "$src" .c)
    out="kernel/${src#kernel/}"
    out="${out%.c}.o"
    local modfile="kernel/${src#kernel/}"; modfile="${modfile%.c}"
    local cmd=()
    local skip=false
    for a in $PREFIX; do
        if $skip; then skip=false; continue; fi
        case "$a" in
            # 注意: 参数必须以字面双引号包裹值 (内核 make 在 shell 层剥单引号后
            # 传给编译器的是 -DNAME="value"); 若用 '\"$var\"' 会把单引号留在
            # 宏体里, 展开后产生 char literal, 导致 dcc 解析报 E0001.
            # KBUILD_MODFILE 不带 .c 后缀 (与真实 .cmd 一致)
            -DKBUILD_MODFILE=*)
                cmd+=("-DKBUILD_MODFILE=\"$modfile\"")
                ;;
            -DKBUILD_BASENAME=*)
                cmd+=("-DKBUILD_BASENAME=\"$base\"")
                ;;
            -DKBUILD_MODNAME=*)
                cmd+=("-DKBUILD_MODNAME=\"$base\"")
                ;;
            -D__KBUILD_MODNAME=*)
                cmd+=("-D__KBUILD_MODNAME=$base")
                ;;
            # -Wp 依赖文件路径按当前文件替换, 避免所有文件写同一个 .d
            -Wp,-MMD,*)
                cmd+=("-Wp,-MMD,${out%/*}/.${out##*/}.d")
                ;;
            -Wp,-MT,*)
                cmd+=("-Wp,-MT,$out")
                ;;
            *)
                cmd+=("$a")
                ;;
        esac
    done
    cmd+=("-c" "-o" "$out" "$src")
    if ! "$CC_WRAP" "${cmd[@]}" >> "$OUT_LOG" 2>&1; then
        echo "FAIL $src" >> "$OUT_LOG"
        return 1
    fi
    return 0
}

export -f compile_one
export CC_WRAP PREFIX OUT_LOG

files=""
for f in kernel/*.c kernel/sched/*.c; do
    case "$f" in
        kernel/bounds.c|kernel/sched/rq-offsets.c) continue ;;
    esac
    o="${f%.c}.o"
    if [ "${FORCE:-0}" = "1" ] || [ ! -f "$o" ]; then
        files="$files $f"
    fi
done

echo "== compiling $(echo $files | wc -w) files with $JOBS jobs =="
echo "$files" | tr ' ' '\n' | grep -v '^$' | xargs -P "$JOBS" -I{} bash -c 'compile_one "$1"' _ {} 
echo "== done; failures: =="
grep -c "^FAIL " "$OUT_LOG" || true
grep "^FAIL " "$OUT_LOG" | head -20
