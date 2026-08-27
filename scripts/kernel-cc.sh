#!/usr/bin/env bash
# kernel-cc.sh - Kernel build compiler wrapper
# Uses dcc for C files, gcc for assembly (.S) files.
# This works around dcc's limitations with complex GAS macro
# expansions in assembly files while keeping dcc for C codegen.
set -euo pipefail

DCC="${DCC:-/root/dcc/target/release/dcc}"
GCC="${GCC:-gcc}"

# Determine input file from arguments
is_asm=false
for arg in "$@"; do
    case "$arg" in
        *.S|*.s) is_asm=true ;;
    esac
done

if $is_asm; then
    # Filter out dcc-specific options that gcc doesn't understand
    filtered_args=()
    skip_next=false
    for arg in "$@"; do
        if $skip_next; then
            skip_next=false
            continue
        fi
        case "$arg" in
            -fms-anonymous-structs|\
            -fzero-init-padding-bits=*|\
            -fdiagnostics-show-context=*|\
            -fdiagnostics-show-inlining-chain|\
            -fmin-function-alignment=*)
                continue
                ;;
            -mstack-alignment=*)
                # gcc uses -mstackrealign or -mincoming-stack-boundary
                continue
                ;;
            -mretpoline-external-thunk)
                # gcc uses -mindirect-branch=thunk-extern
                continue
                ;;
        esac
        filtered_args+=("$arg")
    done
    exec "$GCC" "${filtered_args[@]}"
else
    exec "$DCC" "$@"
fi
