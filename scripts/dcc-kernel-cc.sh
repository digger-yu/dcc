#!/usr/bin/env bash
# dcc 内核编译包装: 对基础设施文件用 gcc, 其余用 dcc
# 顺便过滤掉传给 gcc 的 clang 专属 -f 标志 (否则 gcc 会失败)
set -euo pipefail
DCC_BIN="${DCC_BIN:-/root/dcc/target/release/dcc}"

# 过滤 clang-only 标志 (gcc 和 dcc 都不认识)
filter_clang_flags() {
    local -n _out=$1
    shift
    for a in "$@"; do
        # 注意：函数对齐的两个写法 (`-falign-functions=N` / `-fmin-function-alignment=N`)
        # 必须原样透传。内核 `Makefile` 先拿 `cc-option` 问过本包装器才决定用哪个，包装器
        # 把它吐掉会让探测报「支持」而真编译时被丢弃 ⇒ 所有函数都不对齐，`vmlinux` 的
        # 函数边界与 gcc 产物完全不同（而 `CONFIG_FUNCTION_ALIGNMENT=16` 本该是 16）。
        case "$a" in
            -fms-anonymous-structs|\
            -fzero-init-padding-bits=*|\
            -fdiagnostics-show-context=*|\
            -fdiagnostics-show-inlining-chain|\
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
            -mharden-sls=*|\
            -mstack-alignment=*|\
            -mretpoline-external-thunk)
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
# 基础设施文件 (asm-offsets.c, bounds.c 等) 也用 dcc 编译,
# 因为内核头文件依赖 -fms-anonymous-structs 等 GCC 不支持的扩展
# (旧方案: 交给 gcc 但过滤 -fms-anonymous-structs 导致匿名结构体字段丢失)

# gcc 不支持 -fms-anonymous-structs, 但 -fplan9-extensions 提供等价功能
# (匿名结构体/联合体成员). 在委托给 gcc 时进行替换.
#
# `-fmin-function-alignment=N` 是自 GCC 14 才有的写法，本机 gcc 13 认不得；它完全等价于
# 旧的 `-falign-functions=N`，所以委托给 gcc 时重写而不是丢掉。丢掉会令那些 gcc 接手的
# 步骤（`.s` 汇编、链接、-m16 realmode、OOM 回退）编出不对齐的函数，与 dcc 部分
# 拼进 `vmlinux.o` 时函数边界不一致。
replace_dcc_only_flags() {
    local -n _rout=$1
    shift
    for a in "$@"; do
        case "$a" in
            -fms-anonymous-structs)
                _rout+=("-fplan9-extensions")
                ;;
            -fmin-function-alignment=*)
                _rout+=("-falign-functions=${a#*=}")
                ;;
            *)
                _rout+=("$a")
                ;;
        esac
    done
}

# 把一次编译显式交给 gcc：每条都必须留一行可被日志 grep 的判据。
# 「全程用 dcc 编内核」是当前目标，所以剩下这些 gcc 通路必须点名，不能再藏在
# 包装器里：靠 `.comment=GCC` 只能知道「有代工件」，说不出是哪一条路为什么走的。
delegate_gcc() {
    echo "GCC-DELEGATE reason=$1 src=${src:-<none>}" >&2
    exec gcc "${@:2}"
}

# realmode / 16-bit 代码: 需要 -m32 或 -m16, dcc 只支持 x86_64,
# 将这些文件委托给 gcc.  同时过滤 dcc 专属标志 (gcc 不认识).
for a in "$@"; do
    case "$a" in
        -m32|-m16)
            rout=()
            replace_dcc_only_flags rout "${args[@]}"
            out=()
            filter_clang_flags out "${rout[@]}"
            delegate_gcc "m16/m32-no-dcc-backend" "${out[@]}"
            ;;
    esac
done
# Also delegate realmode directory sources
case "$src" in
    */realmode/*)
            rout=()
            replace_dcc_only_flags rout "${args[@]}"
            out=()
            filter_clang_flags out "${rout[@]}"
            delegate_gcc "realmode-dir" "${out[@]}"
            ;;
esac

# 链接步骤: 委托给 gcc (dcc 内置链接器不能解析内核外部符号)
if is_link_step "$@"; then
    rout=()
    replace_dcc_only_flags rout "${args[@]}"
    out=()
    filter_clang_flags out "${rout[@]}"
    # 链接步骤：dcc 没有自己的链接器（vmlinux 的脚本链接语义只能交 GNU ld 驱动），
    # 这不是「用 gcc 代编 C 代码」，但依然留痕。
    delegate_gcc "link-step" "${out[@]}"
fi

# 无源文件 (可能是其他非编译调用): 直接委托 gcc
if [ -z "$src" ]; then
    rout=()
    replace_dcc_only_flags rout "${args[@]}"
    out=()
    filter_clang_flags out "${rout[@]}"
    delegate_gcc "no-source-file" "${out[@]}"
fi

# 以下是 dcc 编译路径
# dcc 不实现 fentry, 但 kernel 头文件检查 CC_USING_FENTRY; 额外注入
if ! printf '%s\n' "$@" | grep -q -- '-DCC_USING_FENTRY'; then
    set -- -DCC_USING_FENTRY "$@"
fi
# 过滤 -mno-sse 系列: dcc 后端始终使用 SSE
# 过滤 -Werror=implicit-function-declaration: dcc 不识别部分内核函数声明,
# 降级为 warning 以避免编译失败
out=()
for a in "$@"; do
    case "$a" in
        -mno-sse*|-mno-mmx|-mno-3dnow*|-mno-avx*|-mno-sse4*) ;;
        -Werror=implicit-function-declaration) ;;
        *) out+=("$a") ;;
    esac
done

# 确定输出文件路径
out_file=""
for ((i = 0; i < ${#out[@]}; i++)); do
    case "${out[$i]}" in
        -o) out_file="${out[$((i+1))]}" ;;
        -o*) out_file="${out[$i]#-o}" ;;
    esac
done
if [ -z "$out_file" ]; then
    # 默认: source.c -> source.o
    out_file="${src%.c}.o"
fi

# 运行 dcc。默认**不允许**回退 gcc：目标要求内核全树由 dcc 编出来，dcc 编不出就是
# 要补的 dcc 缺陷，交 gcc 代工只会把缺陷藏进产物里（实测：`xsave64` 被静默丢弃时，
# gcc 接手前编出的 `.o` 已经在 `.altinstructions` 里留下 0 长条目，要到 `ld -r` 后的
# objtool 才暴露）。需要旧行为（OOM 时让 gcc 兼一下）时显式导出
# DCC_KERNEL_ALLOW_GCC_FALLBACK=1。
dcc_rc=0
"$DCC_BIN" "${out[@]}" -Wno-implicit-declaration -fpermissive -fno-builtin-linker || dcc_rc=$?

if [ $dcc_rc -ne 0 ] || [ ! -f "$out_file" ]; then
    if [ "${DCC_KERNEL_ALLOW_GCC_FALLBACK:-0}" = "1" ]; then
        echo "dcc failed (rc=$dcc_rc) for $src, falling back to gcc (opt-in)" >&2
        rout=()
        replace_dcc_only_flags rout "${out[@]}"
        gout=()
        filter_clang_flags gout "${rout[@]}"
        exec gcc "${gout[@]}"
    fi
    echo "dcc failed (rc=$dcc_rc) for $src: 不再回退 gcc，这是要修的 dcc 缺陷" >&2
    # 删掉半截产物，让 Kbuild 下一轮能重跑这一步而不是把残缺文件当成已编好。
    rm -f "$out_file"
    exit "$dcc_rc"
fi
