#!/usr/bin/env bash
# 清理 dcc 调试与内核编译产生的临时文件、缓存与中间产物。
# 所有清理项均为可自动再生的数据，删除后无任何不良影响。
#
# 用法:
#   bash scripts/cleanup_disk.sh          # 交互式（默认清理 1-5）
#   bash scripts/cleanup_disk.sh --all    # 全量清理（含 Rust target/）
#   bash scripts/cleanup_disk.sh --dry    # 仅显示占用，不删除
#
# 清理项:
#   1. ~/.dcc/cache/          — dcc 预处理缓存（可达 40+ GB）
#   2. /tmp/dcc-*             — dcc driver 与脚本的临时文件/工作目录
#   3. /tmp 杂项              — 测试二进制、内核编译日志、PCH 等
#   4. 工作区 profile/coverage — .profraw / .profdata / .gcda / .gcno
#   5. 工作区 PCH 文件        — *.pch / *.gch
#   6. Rust target/           — cargo 构建缓存（可选，清理后需重新编译）

set -euo pipefail

DCC_ROOT="${DCC_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"

# ── 颜色 ──────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

# ── 工具函数 ──────────────────────────────────────────
human_size() {
    local bytes=$1
    if (( bytes >= 1073741824 )); then
        local whole=$((bytes / 1073741824))
        local frac=$(( (bytes % 1073741824) * 10 / 1073741824 ))
        printf "%d.%d GB" "$whole" "$frac"
    elif (( bytes >= 1048576 )); then
        local whole=$((bytes / 1048576))
        local frac=$(( (bytes % 1048576) * 10 / 1048576 ))
        printf "%d.%d MB" "$whole" "$frac"
    elif (( bytes >= 1024 )); then
        local whole=$((bytes / 1024))
        local frac=$(( (bytes % 1024) * 10 / 1024 ))
        printf "%d.%d KB" "$whole" "$frac"
    else
        printf "%d B" "$bytes"
    fi
}

dir_size() {
    if [[ -e "$1" ]]; then
        du -sb "$1" 2>/dev/null | awk '{print $1}'
    else
        echo 0
    fi
}

glob_size() {
    # 计算匹配路径的总大小（参数为 find 的 path + 选项）
    # 先用 find 收集匹配项，再用 du -sbc 一次性计算
    local items=()
    while IFS= read -r -d '' entry; do
        items+=("$entry")
    done < <(find "$@" -mindepth 1 -maxdepth 1 -print0 2>/dev/null)
    if (( ${#items[@]} == 0 )); then
        echo 0
        return
    fi
    du -sbc "${items[@]}" 2>/dev/null | tail -1 | awk '{print $1}'
}

recursive_size() {
    # 递归查找匹配文件的总大小（参数为 find 的 path + -name 选项）
    find "$@" -type f -printf '%s\n' 2>/dev/null | awk '{s+=$1} END {print s+0}'
}

recursive_count() {
    find "$@" -type f 2>/dev/null | wc -l
}

count_files() {
    find "$@" 2>/dev/null | wc -l
}

# ── 统计各项大小 ──────────────────────────────────────
declare -a SIZES=()
declare -a LABELS=()
declare -a DETAILS=()

# 1) ~/.dcc/cache/
DCC_CACHE="${HOME}/.dcc"
s=$(dir_size "$DCC_CACHE")
SIZES+=("$s")
LABELS+=("${CYAN}~/.dcc/${NC}  (预处理缓存 + 全局类型缓存)")
DETAILS+=("路径: $DCC_CACHE")

# 2) /tmp/dcc-*
s=$(glob_size /tmp -maxdepth 1 -name 'dcc-*')
n=$(count_files /tmp -maxdepth 1 -name 'dcc-*')
SIZES+=("$s")
LABELS+=("${CYAN}/tmp/dcc-*${NC}  (driver 临时 asm / cpp / link + 脚本工作目录)")
DETAILS+=("$n 项: dcc-asm-*.s / dcc-cpp-*.c / dcc-builtlink-*.o / dcc-hostlink-*.o / dcc-{prepare0,pahole,objtool,modpost}-* / dcc-kernel-features-*")

# 3) /tmp 杂项（测试二进制、内核编译日志、PCH、旧探针文件）
tmp_misc=0
for pat in 't_*' 'kernel_dcc_files.txt' 'kernel_dir_compile.log' 'dcc-pch-*' 'regex_fresh.*' 't_*.c' 't_*.o'; do
    s=$(glob_size /tmp -maxdepth 1 -name "$pat")
    tmp_misc=$((tmp_misc + s))
done
SIZES+=("$tmp_misc")
LABELS+=("${CYAN}/tmp 杂项${NC}  (测试二进制 / 内核编译日志 / PCH / 旧探针)")
DETAILS+=("t_* / kernel_dcc_files.txt / kernel_dir_compile.log / dcc-pch-* / regex_fresh.*")

# 4) 工作区 profile / coverage 文件
prof_size=0
for ext in '*.profraw' '*.profdata' '*.dcc.profdata' '*.gcda' '*.gcno'; do
    s=$(recursive_size "$DCC_ROOT" -name "$ext")
    prof_size=$((prof_size + s))
done
SIZES+=("$prof_size")
LABELS+=("${CYAN}工作区 profile/coverage${NC}  (.profraw / .profdata / .gcda / .gcno)")
DETAILS+=("路径: $DCC_ROOT 下递归查找")

# 5) 工作区 PCH 文件
pch_size=0
for ext in '*.pch' '*.gch'; do
    s=$(recursive_size "$DCC_ROOT" -name "$ext")
    pch_size=$((pch_size + s))
done
SIZES+=("$pch_size")
LABELS+=("${CYAN}工作区 PCH${NC}  (*.pch / *.gch)")
DETAILS+=("路径: $DCC_ROOT 下递归查找")

# 6) Rust target/
TARGET_DIR="$DCC_ROOT/target"
s=$(dir_size "$TARGET_DIR")
SIZES+=("$s")
LABELS+=("${CYAN}target/${NC}  (Rust 构建缓存，清理后需 cargo build 重新编译)")
DETAILS+=("路径: $TARGET_DIR")

# ── 显示摘要 ──────────────────────────────────────────
total=0
echo ""
echo -e "${BOLD}═══ dcc 磁盘占用分析 ═══${NC}"
echo ""
for i in "${!SIZES[@]}"; do
    sz=${SIZES[$i]}
    total=$((total + sz))
    printf "  ${YELLOW}%d)${NC} %-52s %s\n" \
        "$((i + 1))" \
        "$(echo -e "${LABELS[$i]}")" \
        "$(human_size "$sz")"
    echo -e "     ${NC}${DETAILS[$i]}"
done
echo ""
echo -e "  ${BOLD}合计${NC}  $(human_size "$total")"
echo ""

# ── 模式判断 ──────────────────────────────────────────
MODE="${1:-interactive}"

if [[ "$MODE" == "--dry" ]]; then
    echo -e "${GREEN}--dry 模式：仅显示占用，不执行删除。${NC}"
    exit 0
fi

# ── 清理函数 ──────────────────────────────────────────
clean_dcc_cache() {
    echo -e "${YELLOW}[1/6] 清理 ~/.dcc/ ...${NC}"
    if [[ -d "$DCC_CACHE" ]]; then
        # 先尝试直接删除；若因权限/不可变标志等失败，回退到逐层清理
        if rm -rf "$DCC_CACHE" 2>/dev/null; then
            echo -e "  ${GREEN}已删除${NC} $DCC_CACHE"
        else
            echo "  直接删除失败，尝试逐层清理..."
            # 优先处理不可写目录：先恢复写权限
            chmod -R u+w "$DCC_CACHE" 2>/dev/null || true
            rm -rf "$DCC_CACHE" 2>/dev/null || {
                # 最后手段：清空内容但保留目录壳
                find "$DCC_CACHE" -mindepth 1 -delete 2>/dev/null || true
                if [[ "$(ls -A "$DCC_CACHE" 2>/dev/null)" ]]; then
                    echo -e "  ${RED}警告: 部分文件无法删除（可能被其他进程占用或设置了不可变标志）${NC}"
                    echo -e "  ${RED}       请检查: lsattr $DCC_CACHE 或 lsof +D $DCC_CACHE${NC}"
                else
                    echo -e "  ${GREEN}已清空内容${NC} $DCC_CACHE"
                fi
                return
            }
            echo -e "  ${GREEN}已删除${NC} $DCC_CACHE"
        fi
    else
        echo "  跳过（目录不存在）"
    fi
}

clean_tmp_dcc() {
    echo -e "${YELLOW}[2/6] 清理 /tmp/dcc-* ...${NC}"
    local n=0
    for f in /tmp/dcc-*; do
        [[ -e "$f" ]] || continue
        rm -rf "$f"
        n=$((n + 1))
    done
    echo -e "  ${GREEN}已删除${NC} $n 项"
}

clean_tmp_misc() {
    echo -e "${YELLOW}[3/6] 清理 /tmp 杂项 ...${NC}"
    local n=0
    for pat in '/tmp/t_[0-9a-zA-Z_-]*' '/tmp/kernel_dcc_files.txt' \
               '/tmp/kernel_dir_compile.log' '/tmp/dcc-pch-*' \
               '/tmp/regex_fresh.*'; do
        for f in $pat; do
            [[ -e "$f" ]] || continue
            rm -rf "$f"
            n=$((n + 1))
        done
    done
    echo -e "  ${GREEN}已删除${NC} $n 项"
}

clean_profile() {
    echo -e "${YELLOW}[4/6] 清理工作区 profile/coverage ...${NC}"
    local n=0
    for ext in '*.profraw' '*.profdata' '*.dcc.profdata' '*.gcda' '*.gcno'; do
        while IFS= read -r -d '' f; do
            rm -f "$f"
            n=$((n + 1))
        done < <(find "$DCC_ROOT" -name "$ext" -print0 2>/dev/null)
    done
    echo -e "  ${GREEN}已删除${NC} $n 个文件"
}

clean_pch() {
    echo -e "${YELLOW}[5/6] 清理工作区 PCH ...${NC}"
    local n=0
    for ext in '*.pch' '*.gch'; do
        while IFS= read -r -d '' f; do
            rm -f "$f"
            n=$((n + 1))
        done < <(find "$DCC_ROOT" -name "$ext" -print0 2>/dev/null)
    done
    echo -e "  ${GREEN}已删除${NC} $n 个文件"
}

clean_target() {
    echo -e "${YELLOW}[6/6] 清理 Rust target/ ...${NC}"
    if [[ -d "$TARGET_DIR" ]]; then
        rm -rf "$TARGET_DIR"
        echo -e "  ${GREEN}已删除${NC} $TARGET_DIR"
        echo -e "  ${RED}注意: 下次构建需完整重新编译${NC}"
    else
        echo "  跳过（目录不存在）"
    fi
}

# ── 执行 ──────────────────────────────────────────────
if [[ "$MODE" == "--all" ]]; then
    echo -e "${RED}${BOLD}═══ 全量清理（含 Rust target/）═══${NC}"
    echo ""
    clean_dcc_cache
    clean_tmp_dcc
    clean_tmp_misc
    clean_profile
    clean_pch
    clean_target
else
    # 交互式：默认清理 1-5（不含 target/）
    echo -e "${GREEN}${BOLD}═══ 安全清理（不含 Rust target/）═══${NC}"
    echo -e "  使用 ${CYAN}--all${NC} 可额外清理 target/（需重新 cargo build）"
    echo ""
    clean_dcc_cache
    clean_tmp_dcc
    clean_tmp_misc
    clean_profile
    clean_pch
fi

echo ""
echo -e "${GREEN}${BOLD}═══ 清理完成 ═══${NC}"
echo ""
