#!/usr/bin/env bash
# Kbuild HOSTCC 包装：dcc 编译、gcc 链接；翻译 -Wp,-MMD 为 -MD -MF
set -euo pipefail

DCC_BIN="${DCC_BIN:-dcc}"
HOST_LD="${HOST_LD:-gcc}"

filter_args() {
    local -n _in=$1
    local -n _out=$2
    _out=()
    local skip=false
    for a in "${_in[@]}"; do
        if $skip; then
            skip=false
            continue
        fi
        case "$a" in
            -Wp,-MMD,*)
                _out+=(-MD -MF "${a#-Wp,-MMD,}")
                ;;
            -Wp,-MD,*)
                _out+=(-MD -MF "${a#-Wp,-MD,}")
                ;;
            -Wp,*)
                ;;
            *)
                _out+=("$a")
                ;;
        esac
    done
}

args=("$@")
has_c=false
for a in "${args[@]}"; do
    [[ "$a" == "-c" ]] && has_c=true
done

if $has_c; then
    filtered=()
    filter_args args filtered
    exec "$DCC_BIN" "${filtered[@]}"
fi

out=""
src=""
for ((i = 0; i < ${#args[@]}; i++)); do
    if [[ "${args[i]}" == "-o" && $((i + 1)) -lt ${#args[@]} ]]; then
        out="${args[i + 1]}"
    fi
done
src="${args[-1]}"
if [[ -n "$out" && "$src" == *.c ]]; then
    # fixdep：dcc -c 生成 .d，二进制用 Rust fixdep
    if [[ "$(basename "$src")" == "fixdep.c" ]]; then
        fixdep_bin="${DCC_REPO:-$HOME/dcc-agent-linux66}/target/release/fixdep"
        if [[ ! -x "$fixdep_bin" && -f "$HOME/.cargo/env" ]]; then
            # shellcheck disable=SC1091
            source "$HOME/.cargo/env"
            (cd "${DCC_REPO:-$HOME/dcc-agent-linux66}" && cargo build --release --bin fixdep --quiet) || true
        fi
        compile=()
        depfile=""
        skip=false
        for ((i = 0; i < ${#args[@]}; i++)); do
            if $skip; then skip=false; continue; fi
            a="${args[i]}"
            if [[ "$a" == "-o" ]]; then skip=true; continue; fi
            case "$a" in
                -Wp,-MMD,*)
                    depfile="${a#-Wp,-MMD,}"
                    compile+=(-MD -MF "$depfile" -c -o /dev/null)
                    ;;
                -Wp,-MD,*)
                    depfile="${a#-Wp,-MD,}"
                    compile+=(-MD -MF "$depfile" -c -o /dev/null)
                    ;;
                -Wp,*)
                    ;;
                "$out")
                    ;;
                *)
                    compile+=("$a")
                    ;;
            esac
        done
        # 仅生成依赖文件（src 已在 compile 末尾）
        if ! "$DCC_BIN" "${compile[@]}"; then
            echo "WARN: dcc -MD for fixdep.c failed; writing stub .fixdep.d" >&2
            if [[ -n "$depfile" ]]; then
                printf '%s: %s\n' "$out" "$src" >"$depfile"
            fi
        fi
        if [[ -n "$depfile" && -f "$depfile" ]]; then
            sed -i 's| /dev/null| '"$out"'|g; s|^/dev/null|'"$out"'|' "$depfile" 2>/dev/null || true
        fi
        if [[ -x "$fixdep_bin" ]]; then
            install -m 755 "$fixdep_bin" "$out"
            exit 0
        fi
    fi
    tmp_o="$(mktemp --suffix=.o)"
    compile=()
    depfile=""
    skip=false
    for ((i = 0; i < ${#args[@]}; i++)); do
        if $skip; then
            skip=false
            continue
        fi
        a="${args[i]}"
        if [[ "$a" == "-o" ]]; then
            compile+=(-o "$tmp_o" -c)
            skip=true
            continue
        fi
        case "$a" in
            -Wp,-MMD,*)
                depfile="${a#-Wp,-MMD,}"
                compile+=(-MD -MF "$depfile")
                ;;
            -Wp,-MD,*)
                depfile="${a#-Wp,-MD,}"
                compile+=(-MD -MF "$depfile")
                ;;
            -Wp,*)
                ;;
            "$out")
                ;;
            *)
                compile+=("$a")
                ;;
        esac
    done
    "$DCC_BIN" "${compile[@]}"
    "$HOST_LD" -o "$out" "$tmp_o"
    # 将 .d 文件中的临时 .o 目标名替换为最终输出名，供 fixdep 解析
    if [[ -n "${depfile:-}" && -f "$depfile" ]]; then
        tmp_base="$(basename "$tmp_o")"
        out_base="$(basename "$out")"
        sed -i "s|^${tmp_base}:|${out_base}:|; s| ${tmp_base} | ${out_base} |g" "$depfile" 2>/dev/null || true
    fi
    rm -f "$tmp_o"
    exit 0
fi

exec "$HOST_LD" "$@"
