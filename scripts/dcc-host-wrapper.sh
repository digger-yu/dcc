#!/usr/bin/env bash
# Kbuild HOSTCC 包装：编译与链接都交给 dcc（链接由 dcc 自己驱动 `ld`，全程不起 gcc）。
# 翻译白名单 -Wp 子选项；依赖文件的目标名由 `-MT` 直接定成最终输出。
#
# 2026-11-02：crt1/crti/crtn 发现 + `-lc` + `-dynamic-linker` 已搬进 dcc 自己
# （`src/link/host_ld.rs`）。这一层不再拼 ld 命令行，只做 `-Wp` 翻译与参数归一；
# 缺 crt/ld/解释器时由 dcc 自报并退回 host cc（`DCC_LINK_VIA=cc` 可强制对照）。
set -euo pipefail

DCC_REPO="${DCC_REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
# 默认值必须能从**脚本位置**推出来：以前这里是裸 `dcc`，依赖 PATH 里有它。内核
# 子 make（`tools/objtool/`）会重建环境，PATH 里没有 `target/release` 时就是
# `exec: dcc: 未找到`（实测：objtool/libsubcmd 整目录 127 失败）。
DCC_BIN="${DCC_BIN:-$DCC_REPO/target/release/dcc}"
DCC_HOST_WRAPPER_DEPFILE=""
DCC_HOST_WRAPPER_GEN_DEP=false

wp_error() {
    echo "dcc-host-wrapper: error: $1" >&2
    return 2
}

# 链接统一交给 dcc 自己驱动 `ld`：crt1/crti/crtn 的发现、`-lc`、`-dynamic-linker`
# 都在 `src/link/host_ld.rs` 里（一套实现，不再与 bash 重复）。本函数只做参数归一：
#   - 去掉原命令行里的 `-o <out>`，由 dcc 统一给；
#   - 其余参数（`-l`/`-L`/`-Wl,`/`-I`…）原样交给 dcc —— 链接期用不到的编译旗标
#     对 dcc 是无害输入；带 .c 的纯链接调用 dcc 会自己编。
#
# 环境不满足直驱时（缺 crt/ld/解释器、sanitizer、交叉目标）dcc 会自报并退回 host cc；
# `DCC_LINK_VIA=cc` 可强制走 cc 做对照，`DCC_LD` / `DCC_HOST_CRT_DIR` 可覆盖工具链位置。
host_link() {
    local out=$1; shift
    local -a pass=()
    local skip=false a
    for a in "$@"; do
        if $skip; then
            skip=false
            if [ "$a" = "$out" ]; then continue; fi
            pass+=(-o "$a")
            continue
        fi
        case "$a" in
            -o) skip=true ;;
            "$out") ;;
            *) pass+=("$a") ;;
        esac
    done
    "$DCC_BIN" -o "$out" "${pass[@]}"
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

# 预处理探针（`$(CC) -dM -E -x c /dev/null`、`gcc -E -x c -`）：kbuild 拿它取内建宏表，
# tools/build 拿它做特性探测。以前这类调用因「没有 .c 输入」落到链接分支，于是要么
# 起 gcc（新目标下不允许，本轮用 PATH 陷阱证实了），要么静默失败让探测答错。
# dcc 自己有 `-E`/`-dM`，直接转过去。
for _a in "${args[@]}"; do
    if [[ "$_a" == "-E" ]]; then
        exec "$DCC_BIN" "${args[@]}"
    fi
done

out=""
src=""
for ((i = 0; i < ${#args[@]}; i++)); do
    if [[ "${args[i]}" == "-o" && $((i + 1)) -lt ${#args[@]} ]]; then
        out="${args[i + 1]}"
    fi
done
src="${args[-1]}"
if [[ -n "$out" && "$src" == *.c ]]; then
    # fixdep：默认走「dcc -c + dcc 链接（内部驱动 ld）」这条与其余宿主程序完全同一条路。
    # 实测三份二进制对同一份真 .d 的输出逐字节相同（dcc 编 + ld 链 vs gcc 编链 vs Rust 版）。
    #
    # 仓里的 Rust fixdep（`scripts/build_fixdep_dcc.sh`）只作为**显式**退路，要
    # `DCC_HOST_USE_RUST_FIXDEP=1` 才启用。它以前是默认，两个代价：「fixdep 到底由谁编的」
    # 在验收口径里说不清；而且那条路只为了给 kbuild 交一份 .d 就要跑
    # `dcc -c -o /dev/null`，而 dcc 对非文本产物一律 chmod 0755 —— 那正是把 /dev/null
    # 改写成 0755 普通权限的元凶。本文件因此不再出现 `-o /dev/null`，退路也改写成临时 .o。
    if [[ "$(basename "$src")" == "fixdep.c" && "${DCC_HOST_USE_RUST_FIXDEP:-0}" == "1" ]]; then
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
        discard_o="$(mktemp --suffix=.o)"
        if $DCC_HOST_WRAPPER_GEN_DEP; then
            compile+=(-c -o "$discard_o" -MT "$out")
        fi
        # 仅生成依赖文件（src 已在 compile 末尾）
        # 依赖生成失败必须让 host 构建失败，避免伪造不完整的依赖文件。
        if ! "$DCC_BIN" "${compile[@]}"; then
            echo "ERROR: dcc -MD for fixdep.c failed" >&2
            rm -f "$discard_o"
            exit 1
        fi
        rm -f "$discard_o"
        if [[ -x "$fixdep_bin" ]]; then
            install -m 755 "$fixdep_bin" "$out"
            exit 0
        fi
        echo "dcc-host-wrapper: DCC_HOST_USE_RUST_FIXDEP=1 但 $fixdep_bin 不可执行" >&2
        exit 1
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
    # 目标名同样交给 `-MT`（与 host_link 里那一份同口径）：编译用的是临时 .o，
    # 而 .d 里该出现的是最终输出；编完再 sed 在绝对路径下不匹配。
    if $DCC_HOST_WRAPPER_GEN_DEP; then
        compile+=(-MT "$out")
    fi
    "$DCC_BIN" "${compile[@]}"
    # 启动文件与 libc 由 dcc 内部统一拼（`src/link/host_ld.rs`，不经过 gcc）。
    host_link "$out" "$tmp_o"
    rm -f "$tmp_o"
    exit 0
fi

# 纯链接调用（没有源文件）：从参数里取 `-o` 的目标，其余交给同一份 host_link。
link_target=""
lt_skip=false
for _a in "$@"; do
    if $lt_skip; then link_target="$_a"; break; fi
    case "$_a" in
        -o) lt_skip=true ;;
        -o*) link_target="${_a#-o}"; break ;;
    esac
done
if [ -z "$link_target" ]; then
    echo "dcc-host-wrapper: 链接调用里没有 -o 目标: $*" >&2
    exit 1
fi
host_link "$link_target" "$@"
