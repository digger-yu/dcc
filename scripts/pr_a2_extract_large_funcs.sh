#!/bin/bash
# PR-A2: Extract ASM Operand Map and Helpers using bash/sed
set -e

echo "=== Starting ASM Module Extraction (Phase 3) ==="

MOD_FILE="src/codegen/x86_64/mod.rs"
OUTPUT_DIR="src/codegen/x86_64/asm"

echo "1. Checking source file..."
wc -l "$MOD_FILE"

echo "2. Extracting build_asm_operand_map() function (line 30416-31060)..."
sed -n '30416,31060p' "$MOD_FILE" > /tmp/asm_operand_map_extract.txt
echo "   Extracted: $(wc -l < /tmp/asm_operand_map_extract.txt) lines to temp file"

echo "3. Extracting helper functions..."

# gp_reg_name (line 31062-31083)
sed -n '31062,31083p' "$MOD_FILE" > /tmp/gp_reg_name_extract.txt

# writeback_asm_operand (line 31092-31239)  
sed -n '31092,31239p' "$MOD_FILE" > /tmp/writeback_asm_operand_extract.txt

# store_xmm32_stack (line 31241-31243)
sed -n '31241,31243p' "$MOD_FILE" > /tmp/store_xmm32_extract.txt

# store_xmm64_stack (line 31245-31247)
sed -n '31245,31247p' "$MOD_FILE" > /tmp/store_xmm64_extract.txt

# prepare_* binding series
sed -n '31249,31251p' "$MOD_FILE" > /tmp/prepare_xmm_extract.txt    # Start of prepare_asm_xmm_binding
sed -n '31272,31297p' "$MOD_FILE" > /tmp/prepare_binding_extract.txt  # prepare_asm_binding  
sed -n '31299,31327p' "$MOD_FILE" > /tmp/prepare_x87_extract.txt      # prepare_asm_x87_binding
sed -n '31329,31356p' "$MOD_FILE" > /tmp/prepare_mmx_extract.txt      # prepare_asm_mmx_binding
sed -n '31358,31381p' "$MOD_FILE" > /tmp/prepare_seg_extract.txt      # prepare_asm_seg_binding

echo "All extraction commands executed!"
echo "Temp files created in /tmp/"
