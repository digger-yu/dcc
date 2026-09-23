#!/bin/bash
# scripts/analyze_arm_structure.sh - ARM/MIPS 后端结构分析脚本

set -e

echo "========================================="
echo "=== ARM/MIPS Backend Modularization Analysis ==="
echo "========================================="

# Create analysis directory
mkdir -p /tmp/arm_mips_analysis

ARM_FILE="/root/dcc/src/codegen/arm.rs"
MIPS_FILE="/root/dcc/src/codegen/mips.rs"

echo ""
echo "📊 ARM Backend Analysis:"
echo "------------------------"
if [ ! -f "$ARM_FILE" ]; then
    echo "❌ ARM file not found: $ARM_FILE"
    exit 1
fi

arm_lines=$(wc -l < "$ARM_FILE")
echo "Total lines: $arm_lines"

# Count functions
grep -c "^pub fn\|^fn " "$ARM_FILE" > /tmp/arm_mips_analysis/arm_func_count.txt
func_count=$(cat /tmp/arm_mips_analysis/arm_func_count.txt)
echo "Functions: $func_count"

# Count structs
grep -c "^pub struct\|^struct " "$ARM_FILE" > /tmp/arm_mips_analysis/arm_struct_count.txt
struct_count=$(cat /tmp/arm_mips_analysis/arm_struct_count.txt)
echo "Structs: $struct_count"

# Count impl blocks
grep -c "^impl" "$ARM_FILE" > /tmp/arm_mips_analysis/arm_impl_count.txt
impl_count=$(cat /tmp/arm_mips_analysis/arm_impl_count.txt)
echo "Impl blocks: $impl_count"

# Extract function names and their line ranges
echo ""
echo "Function breakdown (first 20):"
awk '/^[[:space:]]*(pub )?fn / {
    match($0, /\(([^)]*)\)/);
    func_name = substr($0, RSTART, RLENGTH);
    gsub(/[()]/, "", func_name);
    if (NR > start) print start": "line;
    start = NR;
    line = $0;
}' "$ARM_FILE" | head -20 > /tmp/arm_mips_analysis/arm_functions.txt
cat /tmp/arm_mips_analysis/arm_functions.txt

# Identify logical sections by line number patterns
echo ""
echo "Identifying logical groupings..."
awk '
/^impl ArmCodegen/ { impl_start = NR; impl_type = "ArmCodegen" }
/^impl/ && !/^impl ArmCodegen/ { 
    if (impl_start) print "Impl " impl_type ": lines " impl_start "-" (NR-1);
    impl_start = NR;
    impl_type = $0;
}
END { if (impl_start) print "Impl " impl_type ": lines " impl_start "-END" }
' "$ARM_FILE" > /tmp/arm_mips_analysis/arm_imps.txt

echo "Impl blocks by line range:"
cat /tmp/arm_mips_analysis/arm_imps.txt

echo ""
echo "✅ ARM analysis complete at /tmp/arm_mips_analysis/"

echo ""
echo "📊 MIPS Backend Analysis:"
echo "------------------------"
if [ ! -f "$MIPS_FILE" ]; then
    echo "❌ MIPS file not found: $MIPS_FILE"
    exit 1
fi

mips_lines=$(wc -l < "$MIPS_FILE")
echo "Total lines: $mips_lines"

# Count functions
grep -c "^pub fn\|^fn " "$MIPS_FILE" > /tmp/arm_mips_analysis/mips_func_count.txt
func_count=$(cat /tmp/arm_mips_analysis/mips_func_count.txt)
echo "Functions: $func_count"

# Count structs
grep -c "^pub struct\|^struct " "$MIPS_FILE" > /tmp/arm_mips_analysis/mips_struct_count.txt
struct_count=$(cat /tmp/arm_mips_analysis/mips_struct_count.txt)
echo "Structs: $struct_count"

# Count impl blocks
grep -c "^impl" "$MIPS_FILE" > /tmp/arm_mips_analysis/mips_impl_count.txt
impl_count=$(cat /tmp/arm_mips_analysis/mips_impl_count.txt)
echo "Impl blocks: $impl_count"

# Extract function names
echo ""
echo "Function breakdown (first 20):"
awk '/^[[:space:]]*(pub )?fn / {
    match($0, /\(([^)]*)\)/);
    func_name = substr($0, RSTART, RLENGTH);
    gsub(/[()]/, "", func_name);
    if (NR > start) print start": "line;
    start = NR;
    line = $0;
}' "$MIPS_FILE" | head -20 > /tmp/arm_mips_analysis/mips_functions.txt
cat /tmp/arm_mips_analysis/mips_functions.txt

# Identify logical sections
echo ""
echo "Identifying logical groupings..."
awk '
/^impl MipsCodegen/ { impl_start = NR; impl_type = "MipsCodegen" }
/^impl/ && !/^impl MipsCodegen/ { 
    if (impl_start) print "Impl " impl_type ": lines " impl_start "-" (NR-1);
    impl_start = NR;
    impl_type = $0;
}
END { if (impl_start) print "Impl " impl_type ": lines " impl_start "-END" }
' "$MIPS_FILE" > /tmp/arm_mips_analysis/mips_imps.txt

echo "Impl blocks by line range:"
cat /tmp/arm_mips_analysis/mips_imps.txt

echo ""
echo "✅ MIPS analysis complete at /tmp/arm_mips_analysis/"

echo ""
echo "========================================="
echo "Analysis Summary:"
echo "========================================="
echo "ARM backend: $arm_lines lines, $func_count functions, $struct_count structs"
echo "MIPS backend: $mips_lines lines, $func_count functions, $struct_count structs"
echo ""
echo "✅ Analysis reports saved to /tmp/arm_mips_analysis/"
