#!/bin/bash
# ELF 信息提取模块 - 从 ELF 文件中提取符号表和重定位表
# 输出格式：结构化、易于解析

set -e

# 提取符号表（结构化格式）
# 输出：bind|type|name|value|size|section
extract_symbols() {
    local elf_file="$1"
    local output_file="$2"
    
    if [[ ! -f "$elf_file" ]]; then
        echo "ERROR: ELF file not found: $elf_file" >&2
        return 1
    fi
    
    # 使用 readelf 提取符号表，过滤掉空行和标题
    readelf -s "$elf_file" 2>/dev/null | \
        awk '/^ *[0-9]+:/ {
            # 解析 readelf 输出格式
            # Num:    Value          Size Type    Bind   Vis      Ndx Name
            num = $1
            value = $2
            size = $3
            type = $4
            bind = $5
            vis = $6
            ndx = $7
            name = $8
            
            # 跳过空符号和节符号
            if (name != "" && type != "SECTION") {
                printf "%s|%s|%s|%s|%s|%s\n", bind, type, name, value, size, ndx
            }
        }' | sort -t'|' -k3,3 > "$output_file"
}

# 提取重定位表（结构化格式）
# 输出：offset|type|symbol|addend|section
extract_relocations() {
    local elf_file="$1"
    local output_file="$2"
    
    if [[ ! -f "$elf_file" ]]; then
        echo "ERROR: ELF file not found: $elf_file" >&2
        return 1
    fi
    
    # 提取所有重定位节
    # readelf -r 输出格式（可能是中文或英文）：
    # 偏移量          信息           类型           符号值        符号名称 + 加数
    # 00000000004f  000600000002 R_X86_64_PC32     0000000000000000 global_var - 4
    readelf -r "$elf_file" 2>/dev/null | \
        awk '
        /^(Relocation section|重定位节)/ {
            # 提取节名称（在单引号之间）
            section = ""
            for (i = 1; i <= NF; i++) {
                if ($i ~ /^'\''\./) {
                    section = $i
                    gsub(/'\''/, "", section)
                    break
                }
            }
            next
        }
        /^(Offset|偏移量)/ { next }  # 跳过标题行
        /^$/ { next }       # 跳过空行
        /^[0-9a-f]+/ {
            # 解析重定位条目
            offset = $1
            type = $3
            
            # 符号名和加数的处理
            # 格式可能是：sym_name + addend 或 sym_name - addend
            sym_name = $5
            
            # 查找 +/- 符号和加数值
            # readelf 输出格式：... sym_name - 4 或 ... sym_name + 0
            addend = "0"
            for (i = 6; i <= NF; i++) {
                if ($i == "-" || $i == "+") {
                    sign = $i
                    addend = $(i+1)
                    if (sign == "-") {
                        addend = "-" addend
                    }
                    break
                }
            }
            
            printf "%s|%s|%s|%s|%s\n", offset, type, sym_name, addend, section
        }
        ' | sort -t'|' -k1,1 -k3,3 > "$output_file"
}

# 提取节信息
extract_sections() {
    local elf_file="$1"
    local output_file="$2"
    
    if [[ ! -f "$elf_file" ]]; then
        echo "ERROR: ELF file not found: $elf_file" >&2
        return 1
    fi
    
    readelf -S "$elf_file" 2>/dev/null | \
        awk '/\[[ 0-9]+\]/ {
            # 匹配节头行
            if ($1 ~ /^\[[ 0-9]+\]$/) {
                name = $2
                type = $3
                addr = $4
                offset = $5
                size = $6
                printf "%s|%s|%s|%s|%s\n", name, type, addr, offset, size
            }
        }' > "$output_file"
}

# 主函数：提取所有信息
extract_all() {
    local elf_file="$1"
    local output_dir="$2"
    local base_name=$(basename "$elf_file" .o)
    
    mkdir -p "$output_dir"
    
    extract_symbols "$elf_file" "$output_dir/${base_name}.symbols"
    extract_relocations "$elf_file" "$output_dir/${base_name}.relocs"
    extract_sections "$elf_file" "$output_dir/${base_name}.sections"
    
    echo "Extracted: $output_dir/${base_name}.{symbols,relocs,sections}"
}

# 如果直接执行脚本
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    if [[ $# -lt 2 ]]; then
        echo "Usage: $0 <elf_file> <output_dir>"
        exit 1
    fi
    extract_all "$1" "$2"
fi
