#!/bin/bash
# scripts/arm_mips_golden_baseline.sh - ARM/MIPS Golden Baseline Capture and Validation Script

set -e

echo "========================================="
echo "=== ARM/MIPS Golden Baseline ==="
echo "========================================="

# Configuration
BASELINE_DIR="/root/dcc/golden_baseline/arm_mips_modular"
mkdir -p "$BASELINE_DIR"

TESTS_DIR="/root/dcc/tests/fixtures"
TEMP_DIR="/tmp/arm_mips_test_output"
rm -rf "$TEMP_DIR"
mkdir -p "$TEMP_DIR"

# Test programs to compile
declare -a TEST_PROGRAMS=(
    "basic/test_add.c"
    "control/if_else.c"
    "functions/function_call.c"
)

# Architecture modes
ARCH_MODES=("arm" "mips")

echo ""
echo "📋 Testing with architectures: ${ARCH_MODES[*]}"
echo ""

# Track results
declare -A RESULTS
TOTAL_TESTS=0
PASSED_TESTS=0

for arch in "${ARCH_MODES[@]}"; do
    echo "========================================="
    echo "🔧 Processing: $arch"
    echo "========================================="
    
    mkdir -p "$BASELINE_DIR/$arch"
    
    for test_file in "${TEST_PROGRAMS[@]}"; do
        TOTAL_TESTS=$((TOTAL_TESTS + 1))
        
        full_path="$TESTS_DIR/${test_file}"
        
        if [ ! -f "$full_path" ]; then
            echo "⚠️  Skipping missing test: $test_file"
            continue
        fi
        
        basename=$(basename "$test_file" .c)
        output_obj="$TEMP_DIR/${arch}_${basename}.o"
        output_asm="$BASELINE_DIR/${arch}/${basename}.asm"
        
        echo "Testing: $test_file ($arch)"
        
        # Compile with dcc
        if dcc "$full_path" -m="$arch" -o "$output_obj" 2>/dev/null; then
            echo "   ✅ Compilation successful"
            
            # Extract assembly listing
            if objdump -d "$output_obj" > "$output_asm" 2>/dev/null; then
                echo "   ✅ Assembly extracted"
                
                # Create normalized version
                normalize_asm "$output_asm" > "${output_asm%.asm}_normalized.asm"
                
                RESULTS["${arch}_${basename}"]="PASS"
                PASSED_TESTS=$((PASSED_TESTS + 1))
            else
                echo "   ❌ Failed to extract assembly"
                RESULTS["${arch}_${basename}"]="ASM_EXTRACTION_FAILED"
            fi
        else
            echo "   ❌ Compilation failed"
            RESULTS["${arch}_${basename}"]="COMPILATION_FAILED"
        fi
        
        echo ""
    done
done

echo "========================================="
echo "📊 Test Summary:"
echo "========================================="
echo "Total tests: $TOTAL_TESTS"
echo "Passed: $PASSED_TESTS"
echo "Failed: $((TOTAL_TESTS - PASSED_TESTS))"
echo ""

if [ $TOTAL_TESTS -gt 0 ]; then
    success_rate=$((PASSED_TESTS * 100 / TOTAL_TESTS))
    echo "Success rate: ${success_rate}%"
fi

echo ""
echo "========================================="
echo "💾 Baseline Storage:"
echo "========================================="
echo "Baseline location: $BASELINE_DIR"
ls -la "$BASELINE_DIR" | head -5

echo ""
echo "✅ Golden baseline capture complete!"
echo ""

# Function to normalize assembly output
normalize_asm() {
    local input_file="$1"
    
    # Convert register names, remove addresses, standardize formatting
    sed -E '
        s/\[.*\]/\[RELOC\]/g          # Remove relocation info
        s/0x[0-9a-f]+/ADDR/g          # Replace hex addresses
        /^$/d                          # Remove empty lines
        s/^[[:space:]]+//             # Trim leading whitespace
    ' "$input_file" | \
    tr '[:upper:]' '[:lower:]' | \
    uniq
}

# Exit with error if any tests failed
if [ $PASSED_TESTS -ne $TOTAL_TESTS ]; then
    echo "⚠️  Some tests failed. Check output for details."
    exit 1
fi

exit 0
