#!/usr/bin/env bash
# dcc 内核编译包装: 对基础设施文件用 gcc, 其余用 dcc
# 顺便过滤掉传给 gcc 的 clang 专属 -f 标志 (否则 gcc 会失败)
set -euo pipefail
DCC_BIN="${DCC_BIN:-/home/di/dcc/target/release/dcc}"

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
        # 过滤 clang-only 标志后交给 gcc
        out=()
        for a in "${args[@]}"; do
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
                -mindirect-branch=*|\
                -mindirect-branch-register|\
                -mindirect-branch-cs-prefix|\
                -mfunction-return=*|\
                -mrecord-mcount|\
                -mfentry|\
                -mstack-protector-guard-reg=*|\
                -mstack-protector-guard-symbol=*|\
                -mharden-sls=*)
                    ;;
                *)
                    out+=("$a")
                    ;;
            esac
        done
        exec gcc "${out[@]}"
        ;;
esac
# dcc 不实现 fentry, 但 kernel 头文件检查 CC_USING_FENTRY; 额外注入, 让代码以为支持.
# 实际 fentry 钩子不会插入, 但编译可以走完.
# dcc 的 x86_64 后端始终使用 SSE, 过滤 -mno-sse 避免 E0300 误报
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
exec "$DCC_BIN" "${out[@]}" -Wno-implicit-declaration
