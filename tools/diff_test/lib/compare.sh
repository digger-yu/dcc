#!/bin/bash
# 结构化对比模块 - 比较两个 ELF 文件的符号表和重定位表
# 输出：差异报告（不是文本 diff，而是语义对比）

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 对比符号表
# 参数：dcc_symbols gcc_symbols
compare_symbols() {
    local dcc_file="$1"
    local gcc_file="$2"
    
    local dcc_only gcc_only mismatch_count=0
    
    echo "========================================"
    echo "         SYMBOL TABLE COMPARISON"
    echo "========================================"
    echo ""
    
    # 找出只在 dcc 中存在的符号
    dcc_only=$(comm -23 <(awk -F'|' '{print $3}' "$dcc_file" | sort) \
                       <(awk -F'|' '{print $3}' "$gcc_file" | sort))
    
    if [[ -n "$dcc_only" ]]; then
        echo -e "${YELLOW}[!] Symbols only in DCC:${NC}"
        echo "$dcc_only" | while read -r sym; do
            echo "  - $sym"
        done
        echo ""
    fi
    
    # 找出只在 gcc 中存在的符号
    gcc_only=$(comm -13 <(awk -F'|' '{print $3}' "$dcc_file" | sort) \
                       <(awk -F'|' '{print $3}' "$gcc_file" | sort))
    
    if [[ -n "$gcc_only" ]]; then
        echo -e "${BLUE}[i] Symbols only in GCC:${NC}"
        echo "$gcc_only" | while read -r sym; do
            echo "  - $sym"
        done
        echo ""
    fi
    
    # 对比共同符号的属性
    echo -e "${GREEN}[✓] Common symbols attribute comparison:${NC}"
    local common_syms=$(comm -12 <(awk -F'|' '{print $3}' "$dcc_file" | sort) \
                                <(awk -F'|' '{print $3}' "$gcc_file" | sort))
    
    echo "$common_syms" | while read -r sym; do
        [[ -z "$sym" ]] && continue
        
        local dcc_line=$(grep "|${sym}|" "$dcc_file" | head -1)
        local gcc_line=$(grep "|${sym}|" "$gcc_file" | head -1)
        
        local dcc_bind=$(echo "$dcc_line" | cut -d'|' -f1)
        local dcc_type=$(echo "$dcc_line" | cut -d'|' -f2)
        local dcc_size=$(echo "$dcc_line" | cut -d'|' -f5)
        
        local gcc_bind=$(echo "$gcc_line" | cut -d'|' -f1)
        local gcc_type=$(echo "$gcc_line" | cut -d'|' -f2)
        local gcc_size=$(echo "$gcc_line" | cut -d'|' -f5)
        
        local issues=""
        
        # 对比类型
        if [[ "$dcc_type" != "$gcc_type" ]]; then
            issues="${issues} TYPE(dcc=$dcc_type vs gcc=$gcc_type)"
        fi
        
        # 对比绑定
        if [[ "$dcc_bind" != "$gcc_bind" ]]; then
            issues="${issues} BIND(dcc=$dcc_bind vs gcc=$gcc_bind)"
        fi
        
        # 对比大小（允许一定差异，因为对齐策略可能不同）
        if [[ "$dcc_size" != "$gcc_size" && "$dcc_size" != "0" && "$gcc_size" != "0" ]]; then
            issues="${issues} SIZE(dcc=$dcc_size vs gcc=$gcc_size)"
        fi
        
        if [[ -n "$issues" ]]; then
            echo -e "  ${RED}✗ $sym:${issues}${NC}"
            ((mismatch_count++)) || true
        fi
    done
    
    if [[ $mismatch_count -eq 0 ]]; then
        echo -e "  ${GREEN}All common symbols match!${NC}"
    fi
    
    echo ""
    echo "Summary:"
    local dcc_only_count=$(echo "$dcc_only" | grep -c . 2>/dev/null || echo "0")
    local gcc_only_count=$(echo "$gcc_only" | grep -c . 2>/dev/null || echo "0")
    # 确保是单个数字
    dcc_only_count=$(echo "$dcc_only_count" | tail -1 | tr -d '[:space:]')
    gcc_only_count=$(echo "$gcc_only_count" | tail -1 | tr -d '[:space:]')
    [[ -z "$dcc_only_count" ]] && dcc_only_count=0
    [[ -z "$gcc_only_count" ]] && gcc_only_count=0
    echo "  DCC-only symbols: $dcc_only_count"
    echo "  GCC-only symbols: $gcc_only_count"
    echo "  Attribute mismatches: $mismatch_count"
    echo ""
}

# 对比重定位表
# 参数：dcc_relocs gcc_relocs
compare_relocations() {
    local dcc_file="$1"
    local gcc_file="$2"
    
    echo "========================================"
    echo "       RELOCATION TABLE COMPARISON"
    echo "========================================"
    echo ""
    
    # 按符号分组对比
    echo "Relocations by symbol:"
    echo ""
    
    # 获取所有唯一的符号
    local all_syms=$(cat <(awk -F'|' '{print $3}' "$dcc_file" 2>/dev/null) \
                         <(awk -F'|' '{print $3}' "$gcc_file" 2>/dev/null) | \
                     sort -u)
    
    local total_mismatches=0
    local type_mismatches=0
    
    echo "$all_syms" | while read -r sym; do
        [[ -z "$sym" ]] && continue
        
        local dcc_rels=$(grep "|${sym}|" "$dcc_file" 2>/dev/null || true)
        local gcc_rels=$(grep "|${sym}|" "$gcc_file" 2>/dev/null || true)
        
        # 计算行数（过滤空行）
        local dcc_count=0 gcc_count=0
        if [[ -n "$dcc_rels" ]]; then
            dcc_count=$(echo "$dcc_rels" | wc -l | tr -d '[:space:]')
        fi
        if [[ -n "$gcc_rels" ]]; then
            gcc_count=$(echo "$gcc_rels" | wc -l | tr -d '[:space:]')
        fi
        
        # 对比数量
        if [[ "$dcc_count" != "$gcc_count" ]]; then
            echo -e "  ${RED}✗ $sym: count mismatch (dcc=$dcc_count vs gcc=$gcc_count)${NC}"
            ((total_mismatches++)) || true
            continue
        fi
        
        # 对比类型
        if [[ -n "$dcc_rels" && -n "$gcc_rels" ]]; then
            local dcc_types=$(echo "$dcc_rels" | cut -d'|' -f2 | sort | tr '\n' ',' | sed 's/,$//')
            local gcc_types=$(echo "$gcc_rels" | cut -d'|' -f2 | sort | tr '\n' ',' | sed 's/,$//')
            
            if [[ "$dcc_types" != "$gcc_types" ]]; then
                echo -e "  ${RED}✗ $sym: type mismatch${NC}"
                echo -e "    ${YELLOW}DCC: $dcc_types${NC}"
                echo -e "    ${BLUE}GCC: $gcc_types${NC}"
                ((type_mismatches++)) || true
            fi
        fi
    done
    
    echo ""
    echo "Detailed relocation type analysis:"
    echo ""
    
    # 统计重定位类型分布
    echo "Relocation type distribution:"
    echo "  DCC:"
    awk -F'|' '{print $2}' "$dcc_file" 2>/dev/null | sort | uniq -c | sort -rn | while read -r count type; do
        echo "    $count x $type"
    done
    
    echo "  GCC:"
    awk -F'|' '{print $2}' "$gcc_file" 2>/dev/null | sort | uniq -c | sort -rn | while read -r count type; do
        echo "    $count x $type"
    done
    
    echo ""
    echo "Summary:"
    echo "  Total DCC relocations: $(wc -l < "$dcc_file" 2>/dev/null || echo 0)"
    echo "  Total GCC relocations: $(wc -l < "$gcc_file" 2>/dev/null || echo 0)"
    echo ""
}

# 生成综合对比报告
generate_report() {
    local dcc_prefix="$1"
    local gcc_prefix="$2"
    local report_file="$3"
    local test_name="$4"
    
    {
        echo "========================================"
        echo "   DIFFERENTIAL TEST REPORT"
        echo "========================================"
        echo ""
        echo "Test: $test_name"
        echo "Date: $(date -Iseconds)"
        echo ""
        
        # 对比符号表
        compare_symbols "${dcc_prefix}.symbols" "${gcc_prefix}.symbols"
        
        # 对比重定位表
        compare_relocations "${dcc_prefix}.relocs" "${gcc_prefix}.relocs"
        
    } > "$report_file"
    
    echo "Report generated: $report_file"
}

# 如果直接执行脚本
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    if [[ $# -lt 4 ]]; then
        echo "Usage: $0 <dcc_prefix> <gcc_prefix> <report_file> <test_name>"
        echo "  dcc_prefix/gcc_prefix: path prefix (files should be prefix.symbols and prefix.relocs)"
        exit 1
    fi
    generate_report "$1" "$2" "$3" "$4"
fi
