#!/usr/bin/env bash
# dcc 内核编译包装: 对基础设施文件用 gcc, 其余用 dcc
# 顺便过滤掉传给 gcc 的 clang 专属 -f 标志 (否则 gcc 会失败)
set -euo pipefail
DCC_BIN="${DCC_BIN:-/root/dcc/target/release/dcc}"
# kbuild 会在别的 cwd 里调本包装器（kconfig 的 `scripts/cc-version.sh $(CC)`），
# 带 `/` 的相对 DCC_BIN 在那里不存在，症状是「unknown C compiler」而非法错。
case "$DCC_BIN" in
    */*) DCC_BIN=$(readlink -f "$DCC_BIN") ;;
esac

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
        *.c|*.S|*.s)
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

# （原 `delegate_gcc` 已删除。它是一个"留痕后 exec gcc"的出口，但内核全树验收的判据
# 是"产物里不能出现 GCC"，所以出口只剩：自己答（DCC-PROBE）或硬错（DCC-GAP）。
# 真正需要的 gcc 保底只剩末尾显式的 DCC_KERNEL_ALLOW_GCC_FALLBACK=1，且它也不是给
# 内核构建用的默认行为。）

# realmode / 16-bit 代码: `-m32` / `-m16` 不再委托 gcc。
#
# 汇编输入（`arch/x86/realmode/rm/{header,trampoline_64,stack,reboot}.S`、
# `arch/x86/boot/header.S` 等）：GAS 根本没有 `-m16`，16 位的代码宽度来自源文件里的
# `.code16`/`.code16gcc`，`-m16` 在汇编输入上唯一的作用是让驱动给 `as` 传 `--32`
# （即输出 ELF32）。dcc 已照此实现（`driver.rs::run_assembler_driver`），实测四个
# realmode `.S` 的产物与 gcc `-m16 -c` **逐字节相同**，所以这条路直接交给 dcc。
#
# C 输入（`arch/x86/boot/*.c`、`arch/x86/realmode/rm/*.c`、`vdso32/*.c`）：需要 16/32 位
# x86 代码生成后端，dcc 没有。此时 dcc 会硬错 E0319 —— 这是**如实报缺口**，不是把
# 旗标吐掉后静默生出 64 位代码（旧行为实测就是这种形状）。PVH 验收配置
# （`scripts/pvh-verify.sh`）把 realmode 的 C 部分（`ACPI_SLEEP`）与 `vdso32`
# （`COMPAT_32`）关掉，让这条路在验收链上不出现；`bzImage` 路线仍需要真 16 位后端。
#
# 注意: 这里**不能**按 `*/realmode/*` 路径委托。`arch/x86/realmode/init.c` 是普通 64 位
# TU（`.init.o.cmd` 里没有 -m16），按路径一刀切会把一个 dcc 本可以编的文件白送给 gcc。
for a in "$@"; do
    case "$a" in
        -m32|-m16)
            case "$src" in
                *.c) echo "DCC-GAP reason=m16/m32-no-c-backend src=$src flag=$a" >&2 ;;
            esac
            break
            ;;
    esac
done

# 链接步骤: 交给 dcc（driver 自己直驱 host `ld` + crt*/libc/libgcc）
#
# 旧实现在这里 `exec gcc`。内核里走 `$(CC)` 的链接步骤只有 userprogs
# （`scripts/Makefile.userprogs`：`$(CC) ... -o ucon ucon.c`，单源直接编链）和个别
# 宿主工具；vmlinux 本身是 `$(LD) -T ...`，不经这条。dcc 的 `run_with_host_link`
# 已经能做到「编译 + host ld 直驱」且补了 `-lgcc`，所以这里不再需要 gcc。
#
# 注意 dcc 只认白名单里的 `-Wp,` 子选项（`-MMD`/`-MF`/`-MT`… 都在里面）；kbuild
# 恰好只传这几个，多余的会显式报错而不是静默丢参。
if is_link_step "$@"; then
    rout=()
    replace_dcc_only_flags rout "${args[@]}"
    out=()
    filter_clang_flags out "${rout[@]}"
    echo "DCC-LINK reason=cc-link-step src=${src:-<none>}" >&2
    exec "$DCC_BIN" "${out[@]}"
fi

# 无源文件 (探针 / 版本询问 / 纯链接)：一律不许再问 gcc，三条各自留痕。
#
#   * `-E`/`-dM`/`-x c -` 这类**预处理探针**交 dcc 自己做（同 `dcc-host-wrapper.sh` 那侧的
#     同一条规则）。内核的 `scripts/cc-version.sh` 就是靠 `__clang__`/`__GNUC__` 宏认人的，
#     而 dcc 本来就预定义 `__clang__ 1 / __clang_major__ 18` 这一族兼容宏，所以这是**如实**
#     回答而不是伪装成别人。
#   * `-Wa,--version`（`scripts/as-version.sh` 要的第一行）：报 binutils `as` 的版本行。
#     内核只用它做 `CONFIG_AS_*` 的能力闸门；本构建的汇编实际由 dcc 自己完成。
#   * `-print-file-name=X`（`scripts/gcc-plugins/Kconfig`）：给一个**不存在**的路径，于是
#     GCC_PLUGINS 自动关掉（dcc 没有 gcc 插件这套东西）。
if [ -z "$src" ]; then
    for a in "${args[@]}"; do
        case "$a" in
            -E|-dM|-dD|-E*|*-cpp-output)
                rout=()
                replace_dcc_only_flags rout "${args[@]}"
                out=()
                filter_clang_flags out "${rout[@]}"
                echo "DCC-PROBE reason=preprocess-only src=${src:-<none>}" >&2
                exec "$DCC_BIN" "${out[@]}"
                ;;
        esac
    done

    if printf '%s\n' "${args[@]}" | grep -q -- '-Wa,--version'; then
        echo "DCC-PROBE reason=as-version" >&2
        as --version 2>/dev/null | head -n 1
        exit 0
    fi

    for a in "${args[@]}"; do
        case "$a" in
            -print-file-name=*)
                # 不存在的路径：`test -d` 会失败，相关 config（GCC_PLUGINS）自己关。
                echo "DCC-PROBE reason=print-file-name arg=$a" >&2
                echo "/nonexistent-dcc-toolchain/${a#-print-file-name=}"
                exit 0
                ;;
            --version|--version=*)
                echo "DCC-PROBE reason=cc-version" >&2
                exec "$DCC_BIN" --version
                ;;
        esac
    done

    rout=()
    replace_dcc_only_flags rout "${args[@]}"
    out=()
    filter_clang_flags out "${rout[@]}"
    # `/dev/null` 与 `-` 也是合法输入：Kconfig 的 `cc-option` 就是
    #   $(CC) -Werror $(1) -c -x c /dev/null -o .tmp_$$/tmp.o
    # 把它当「没有源文件」交给 gcc，探针就在替 dcc 扯谎：探到的能力是 gcc 的，真编译时
    # dcc 又做不到（实测：`-fsanitize=bounds-strict` 探测因这一条回答「支持」，于是
    # CONFIG_UBSAN_BOUNDS_STRICT 一直是 y，旗标递到真编译后 dcc 直接错）。
    for a in "${args[@]}"; do
        case "$a" in
            /dev/null|-)
                echo "DCC-PROBE reason=empty-input src=$a" >&2
                exec "$DCC_BIN" "${out[@]}"
                ;;
        esac
    done
    # 走到这里说明是一条我们没归类的「无源文件」调用。以前这里 `exec gcc`，那等于给
    # 「全程 dcc」开了个后门：gcc 进程一起，产物里的 `.comment` 已经是 GCC，判据就废了。
    # 现在硬错并把整条 argv 打出来，下一次命中时能直接看出是哪条探针缺了归类。
    echo "DCC-GAP reason=no-source-file argv=$*" >&2
    echo "dcc-wrapper: refusing to spawn gcc for a source-less invocation; add an explicit DCC-PROBE branch" >&2
    exit 1
fi

# 以下是 dcc 编译路径
# dcc 不实现 fentry, 但 kernel 头文件检查 CC_USING_FENTRY; 额外注入
if ! printf '%s\n' "$@" | grep -q -- '-DCC_USING_FENTRY'; then
    set -- -DCC_USING_FENTRY "$@"
fi
# `-mno-sse` 系列必须**原样透传**：dcc 后端以前无条件用 SSE，所以这里曾把它们吞掉。
# 但内核是所有 C TU 都带 `-mno-sse -mno-mmx -mno-sse2 -mno-3dnow -mno-avx -mno-sse4a`
# 编的，目标是 `CR4.OSFXSR=0` 的启动早期也要能跑：变参序言里那 8 条 `movups %xmmN`
# 一旦发出来就是 #UD（实测 PVH 启动在 `early_printk` 的第一条 movups 上死循环）。
# dcc 的 `target.sse` 已经把 va save area / va_arg XMM 槽 / float 参数登记 / x87 选择
# 都按它分叉，透传后序言只存 48B 的 GP 槽，与 gcc 的 `-mno-sse` 产物同形。
#
# 过滤 -Werror=implicit-function-declaration: dcc 不识别部分内核函数声明,
# 降级为 warning 以避免编译失败
out=()
for a in "$@"; do
    case "$a" in
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
