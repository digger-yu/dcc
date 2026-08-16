#!/usr/bin/env bash
# dcc 内核编译包装: 对基础设施文件用 gcc, 其余用 dcc
# 顺便过滤掉传给 gcc 的 clang 专属 -f 标志 (否则 gcc 会失败)
set -euo pipefail
DCC_BIN="${DCC_BIN:-/home/di/dcc/target/release/dcc}"

# 过滤 clang-only 标志 (gcc 和 dcc 都不认识)
filter_clang_flags() {
    local -n _out=$1
    shift
    for a in "$@"; do
        case "$a" in
            -fms-anonymous-structs|\
            -fzero-init-padding-bits=*|\
            -fdiagnostics-show-context=*|\
            -fdiagnostics-show-inlining-chain|\
            -fmin-function-alignment=*|\
            -fstack-clash-protection|\
            -fpatchable-function-entry=*|\
            -fharden-sls=*|\
            -ftrivial-auto-var-init=*|\
            -fzero-call-used-regs=*|\
            -fconserve-stack|\
            -fstrict-flex-arrays=*|\
            -fno-builtin-wcslen|\
            -falign-loops=*|\
            -falign-jumps=*|\
            -mrecord-mcount|\
            -mfentry|\
            -mstack-protector-guard-reg=*|\
            -mstack-protector-guard-symbol=*|\
            -mharden-sls=*)
                ;;
            *)
                _out+=("$a")
                ;;
        esac
    done
}

# 判断是否是链接步骤 (应委托给 gcc)
# 链接步骤特征: 无 -c/-E/-S/-fsyntax-only, 且有 -pie/-shared/-Wl 或多个 .o 输入
is_link_step() {
    local has_c=false has_e=false has_s=false has_syntax=false
    local has_pie=false has_shared=false has_wl=false
    local obj_count=0 has_src=false

    for a in "$@"; do
        case "$a" in
            -c) has_c=true ;;
            -E) has_e=true ;;
            -S) has_s=true ;;
            -fsyntax-only) has_syntax=true ;;
            -pie|-fpie|-fPIE) has_pie=true ;;
            -shared|-fpic|-fPIC) has_shared=true ;;
            -Wl,*) has_wl=true ;;
            *.o) ((obj_count++)) || true ;;
            *.c|*.S|*.cpp|*.cc) has_src=true ;;
        esac
    done

    # 编译/预处理/汇编步骤 → 不是链接
    if $has_c || $has_e || $has_s || $has_syntax; then
        return 1
    fi
    # 有链接标志或多个 .o 输入 → 是链接
    if $has_pie || $has_shared || $has_wl; then
        return 0
    fi
    if [ "$obj_count" -ge 2 ]; then
        return 0
    fi
    # 单个 .o 输入无 -c → 链接
    if [ "$obj_count" -ge 1 ] && ! $has_src; then
        return 0
    fi
    return 1
}

# 找到最后一个源文件
args=("$@")
n=${#args[@]}
src=""
for ((i = n - 1; i >= 0; i--)); do
    a="${args[i]}"
    case "$a" in
        *.c|*.S)
            src="$a"
            break
            ;;
    esac
done

base="$(basename "$src" 2>/dev/null || true)"
case "$base" in
    asm-offsets.c|bounds.c|*-offsets.c)
        # 基础设施文件: 直接交给 gcc
        out=()
        filter_clang_flags out "${args[@]}"
        exec gcc "${out[@]}"
        ;;
esac

# 链接步骤: 委托给 gcc (dcc 内置链接器不能解析内核外部符号)
if is_link_step "$@"; then
    out=()
    filter_clang_flags out "${args[@]}"
    exec gcc "${out[@]}"
fi

# 以下是 dcc 编译路径
# dcc 不实现 fentry, 但 kernel 头文件检查 CC_USING_FENTRY; 额外注入
if ! printf '%s\n' "$@" | grep -q -- '-DCC_USING_FENTRY'; then
    set -- -DCC_USING_FENTRY "$@"
fi
# 过滤 -mno-sse 系列: dcc 后端始终使用 SSE
out=()
for a in "$@"; do
    case "$a" in
        -mno-sse*|-mno-mmx|-mno-3dnow*|-mno-avx*|-mno-sse4*) ;;
        *) out+=("$a") ;;
    esac
done
exec "$DCC_BIN" "${out[@]}" -Wno-implicit-declaration -fno-builtin-linker
