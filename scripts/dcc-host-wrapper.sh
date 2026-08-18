#!/usr/bin/env bash
# Kbuild HOSTCC 包装：dcc 编译、gcc 链接；翻译白名单 -Wp 子选项
set -euo pipefail

DCC_BIN="${DCC_BIN:-dcc}"
DCC_REPO="${DCC_REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
HOST_LD="${HOST_LD:-gcc}"
DCC_HOST_WRAPPER_DEPFILE=""
DCC_HOST_WRAPPER_GEN_DEP=false

wp_error() {
    echo "dcc-host-wrapper: error: $1" >&2
    return 2
}

translate_wp_arg() {
    local a=$1
    local -n out_ref=$2
    local payload="${a#-Wp,}"
    if [[ -z "$payload" || "$payload" == ,* || "$payload" == *, || "$payload" == *,,* ]]; then
        wp_error "empty -Wp option"
        return $?
    fi

    local parts=()
    IFS=',' read -r -a parts <<<"$payload"
    local i=0
    while ((i < ${#parts[@]})); do
        local opt="${parts[i]}"
        case "$opt" in
            -MD|-MMD)
                DCC_HOST_WRAPPER_GEN_DEP=true
                out_ref+=(-MD)
                if ((i + 1 < ${#parts[@]})) && [[ "${parts[i + 1]}" != -* ]]; then
                    i=$((i + 1))
                    DCC_HOST_WRAPPER_DEPFILE="${parts[i]}"
                    out_ref+=(-MF "$DCC_HOST_WRAPPER_DEPFILE")
                fi
                ;;
            -MF)
                i=$((i + 1))
                if ((i >= ${#parts[@]})); then
                    wp_error "missing arg after -Wp,-MF"
                    return $?
                fi
                DCC_HOST_WRAPPER_DEPFILE="${parts[i]}"
                out_ref+=(-MF "$DCC_HOST_WRAPPER_DEPFILE")
                ;;
            -MT|-MQ|-D|-I|-include|-isystem|-U)
                i=$((i + 1))
                if ((i >= ${#parts[@]})); then
                    wp_error "missing arg after -Wp,$opt"
                    return $?
                fi
                out_ref+=("$opt" "${parts[i]}")
                ;;
            -MP|-P|-C|-nostdinc)
                out_ref+=("$opt")
                ;;
            -D?*|-I?*|-include?*|-isystem?*|-U?*)
                out_ref+=("$opt")
                ;;
            *)
                wp_error "unsupported preprocessor option in -Wp: $opt"
                return $?
                ;;
        esac
        i=$((i + 1))
    done
}

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
            -Wp,*)
                translate_wp_arg "$a" _out
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
    case "$a" in
        -Wp)
            wp_error "empty -Wp option"
            exit $?
            ;;
        -Wp,*)
            wp_validate=()
            translate_wp_arg "$a" wp_validate
            ;;
    esac
    [[ "$a" == "-c" ]] && has_c=true
done
DCC_HOST_WRAPPER_DEPFILE=""
DCC_HOST_WRAPPER_GEN_DEP=false

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
        fixdep_bin="$DCC_REPO/target/release/fixdep"
        if [[ ! -x "$fixdep_bin" && -f "$HOME/.cargo/env" ]]; then
            # shellcheck disable=SC1091
            source "$HOME/.cargo/env"
            (cd "$DCC_REPO" && cargo build --release --bin fixdep --quiet) || true
        fi
        compile=()
        DCC_HOST_WRAPPER_DEPFILE=""
        DCC_HOST_WRAPPER_GEN_DEP=false
        skip=false
        for ((i = 0; i < ${#args[@]}; i++)); do
            if $skip; then skip=false; continue; fi
            a="${args[i]}"
            if [[ "$a" == "-o" ]]; then skip=true; continue; fi
            case "$a" in
                -Wp,*)
                    translate_wp_arg "$a" compile
                    ;;
                "$out")
                    ;;
                *)
                    compile+=("$a")
                    ;;
            esac
        done
        if $DCC_HOST_WRAPPER_GEN_DEP; then
            compile+=(-c -o /dev/null)
        fi
        depfile="$DCC_HOST_WRAPPER_DEPFILE"
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
    DCC_HOST_WRAPPER_DEPFILE=""
    DCC_HOST_WRAPPER_GEN_DEP=false
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
            -Wp,*)
                translate_wp_arg "$a" compile
                ;;
            "$out")
                ;;
            *)
                compile+=("$a")
                ;;
        esac
    done
    depfile="$DCC_HOST_WRAPPER_DEPFILE"
    "$DCC_BIN" "${compile[@]}"
    # 用 -no-pie 跳过 ld 默认的 PIE 检查 (dcc .o 含绝对 32 位重定位,
    # 与 ld --pie 不兼容, 常见于 kernel 宿主工具 relocs_32/genksyms 等).
    "$HOST_LD" -no-pie -o "$out" "$tmp_o"
    # 将 .d 文件中的临时 .o 目标名替换为最终输出名，供 fixdep 解析
    if [[ -n "${depfile:-}" && -f "$depfile" ]]; then
        tmp_base="$(basename "$tmp_o")"
        out_base="$(basename "$out")"
        sed -i "s|^${tmp_base}:|${out_base}:|; s| ${tmp_base} | ${out_base} |g" "$depfile" 2>/dev/null || true
    fi
    rm -f "$tmp_o"
    exit 0
fi

exec "$HOST_LD" -no-pie "$@"
