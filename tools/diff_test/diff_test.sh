#!/bin/bash
# dcc 差分测试工具 - 主入口
# 比较 dcc 和 gcc 编译输出的符号表和重定位表
#
# 用法：
#   ./diff_test.sh                    # 运行所有测试
#   ./diff_test.sh simple_add         # 运行指定测试
#   ./diff_test.sh --verbose          # 详细输出
#   ./diff_test.sh --list             # 列出所有测试

set -e

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

# 路径配置
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DCC_BIN="$PROJECT_ROOT/target/release/dcc"
FIXTURES_DIR="$SCRIPT_DIR/fixtures"
REPORTS_DIR="$SCRIPT_DIR/reports"
WORK_DIR="/tmp/dcc-diff-test-$$"

# 加载库
source "$SCRIPT_DIR/lib/elf_dump.sh"
source "$SCRIPT_DIR/lib/compare.sh"

# 默认配置
VERBOSE=0
SHOW_REPORT=1

# 日志函数
log_info() {
    echo -e "${BLUE}[INFO]${NC} $*"
}

log_success() {
    echo -e "${GREEN}[OK]${NC} $*"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $*"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $*" >&2
}

log_verbose() {
    if [[ $VERBOSE -eq 1 ]]; then
        echo -e "${CYAN}[DEBUG]${NC} $*"
    fi
}

# 检查依赖
check_dependencies() {
    local missing=()
    
    if [[ ! -f "$DCC_BIN" ]]; then
        missing+=("dcc binary (run 'make build' first)")
    fi
    
    for cmd in gcc readelf objdump; do
        if ! command -v "$cmd" &>/dev/null; then
            missing+=("$cmd")
        fi
    done
    
    if [[ ${#missing[@]} -gt 0 ]]; then
        log_error "Missing dependencies: ${missing[*]}"
        exit 1
    fi
    
    log_verbose "All dependencies satisfied"
}

# 列出所有可用测试
list_tests() {
    echo "Available test fixtures:"
    echo ""
    for f in "$FIXTURES_DIR"/*.c; do
        if [[ -f "$f" ]]; then
            local name=$(basename "$f" .c)
            echo "  - $name"
        fi
    done
}

# 编译单个测试用例
compile_test() {
    local source_file="$1"
    local test_name=$(basename "$source_file" .c)
    
    log_info "Testing: $test_name"
    
    # 创建工作目录
    local test_work_dir="$WORK_DIR/$test_name"
    mkdir -p "$test_work_dir"
    
    # 复制源文件
    cp "$source_file" "$test_work_dir/"
    
    # 编译 - dcc
    local dcc_obj="$test_work_dir/${test_name}_dcc.o"
    log_verbose "Compiling with dcc: $dcc_obj"
    if ! "$DCC_BIN" -c -o "$dcc_obj" "$source_file" 2>"$test_work_dir/dcc_stderr.log"; then
        log_error "dcc compilation failed for $test_name"
        cat "$test_work_dir/dcc_stderr.log" >&2
        return 1
    fi
    
    # 编译 - gcc
    local gcc_obj="$test_work_dir/${test_name}_gcc.o"
    log_verbose "Compiling with gcc: $gcc_obj"
    if ! gcc -c -O0 -o "$gcc_obj" "$source_file" 2>"$test_work_dir/gcc_stderr.log"; then
        log_error "gcc compilation failed for $test_name"
        cat "$test_work_dir/gcc_stderr.log" >&2
        return 1
    fi
    
    # 提取 ELF 信息
    log_verbose "Extracting ELF information..."
    extract_all "$dcc_obj" "$test_work_dir"
    extract_all "$gcc_obj" "$test_work_dir"
    
    # 生成对比报告
    local report_file="$REPORTS_DIR/${test_name}_report.txt"
    generate_report \
        "$test_work_dir/${test_name}_dcc" \
        "$test_work_dir/${test_name}_gcc" \
        "$report_file" \
        "$test_name"
    
    # 显示摘要
    show_summary "$report_file"
    
    return 0
}

# 显示测试摘要
show_summary() {
    local report_file="$1"
    
    # 提取关键统计
    local mismatches=$(grep -c "type mismatch" "$report_file" 2>/dev/null || echo "0")
    # 确保是单个数字
    mismatches=$(echo "$mismatches" | tail -1 | tr -d '[:space:]')
    [[ -z "$mismatches" ]] && mismatches=0
    
    echo ""
    echo "┌─────────────────────────────────────────┐"
    echo "│           TEST SUMMARY                  │"
    echo "├─────────────────────────────────────────┤"
    
    if [[ "$mismatches" -eq 0 ]]; then
        echo -e "│  ${GREEN}✓ All relocations match!${NC}            │"
    else
        echo -e "│  ${RED}✗ Found $mismatches relocation type mismatches${NC}  │"
    fi
    
    echo "│                                         │"
    echo "│  Full report: $report_file"
    echo "└─────────────────────────────────────────┘"
    echo ""
}

# 运行单个测试
run_test() {
    local test_name="$1"
    local source_file="$FIXTURES_DIR/${test_name}.c"
    
    if [[ ! -f "$source_file" ]]; then
        log_error "Test not found: $test_name"
        log_info "Available tests:"
        list_tests
        return 1
    fi
    
    compile_test "$source_file"
}

# 运行所有测试
run_all_tests() {
    local failed=0
    local passed=0
    
    echo ""
    echo "╔═══════════════════════════════════════════╗"
    echo "║     DCC DIFFERENTIAL TEST SUITE           ║"
    echo "╚═══════════════════════════════════════════╝"
    echo ""
    
    for source_file in "$FIXTURES_DIR"/*.c; do
        if [[ -f "$source_file" ]]; then
            if compile_test "$source_file"; then
                ((passed++))
            else
                ((failed++))
            fi
            echo ""
        fi
    done
    
    echo "╔═══════════════════════════════════════════╗"
    echo "║           FINAL RESULTS                   ║"
    echo "╠═══════════════════════════════════════════╣"
    echo -e "║  ${GREEN}Passed: $passed${NC}                              ║"
    if [[ $failed -gt 0 ]]; then
        echo -e "║  ${RED}Failed: $failed${NC}                              ║"
    else
        echo "║  Failed: 0                                ║"
    fi
    echo "╚═══════════════════════════════════════════╝"
    echo ""
    echo "Reports saved to: $REPORTS_DIR"
    
    return $failed
}

# 清理
cleanup() {
    if [[ -d "$WORK_DIR" ]]; then
        rm -rf "$WORK_DIR"
        log_verbose "Cleaned up work directory"
    fi
}

# 主函数
main() {
    trap cleanup EXIT
    
    # 解析参数
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -v|--verbose)
                VERBOSE=1
                shift
                ;;
            -l|--list)
                list_tests
                exit 0
                ;;
            -h|--help)
                echo "Usage: $0 [OPTIONS] [TEST_NAME]"
                echo ""
                echo "Options:"
                echo "  -v, --verbose    Enable verbose output"
                echo "  -l, --list       List available tests"
                echo "  -h, --help       Show this help"
                echo ""
                echo "Examples:"
                echo "  $0                 Run all tests"
                echo "  $0 simple_add      Run specific test"
                echo "  $0 --verbose       Run with debug output"
                exit 0
                ;;
            -*)
                log_error "Unknown option: $1"
                exit 1
                ;;
            *)
                # 测试名称
                check_dependencies
                mkdir -p "$REPORTS_DIR"
                run_test "$1"
                exit $?
                ;;
        esac
    done
    
    # 默认：运行所有测试
    check_dependencies
    mkdir -p "$REPORTS_DIR"
    run_all_tests
}

main "$@"
