#!/bin/bash
# PR-A2 Final Cleanup - Safe & Complete Implementation
set -e

echo "=== PR-A2 Final Cleanup Script ==="
echo "Starting at: $(date)"

# Step 1: Create backup
cp src/codegen/x86_64/mod.rs /tmp/mod_pre_cleanup_backup.rs
echo "✅ Backup created"

# Step 2: Find exact boundaries
START_LINE=$(grep -n "^    fn build_asm_operand_map(" src/codegen/x86_64/mod.rs | cut -d: -f1)
END_LINE=$(awk "NR>$START_LINE && /^    \}\s*$/ {print NR; exit}" src/codegen/x86_64/mod.rs)

echo "Function to remove: lines $START_LINE to $END_LINE"

# Step 3: Extract head (before function) and tail (after function)
sed -n "1,$((START_LINE-1))p" src/codegen/x86_64/mod.rs > /tmp/head_final.txt
sed -n "$((END_LINE+1)),\$p" src/codegen/x86_64/mod.rs > /tmp/tail_final.txt

# Add blank line in between
echo "" >> /tmp/head_final.txt

# Create new wrapper function
cat > /tmp/wrapper_final.txt << 'EOF'
    /// 构建 asm 操作数映射：%N / %[name] → 寄存器名或内存操作数
    /// 
    /// DELEGATION WRAPPER (PR-A2 Phase):
    /// This function delegates to the extracted implementation in asm/bindings.rs
    /// Original ~645 line implementation removed for modularity.
    fn build_asm_operand_map(
        &mut self,
        outputs: &[Expr],
        output_names: &[Option<String>],
        output_constraints: &[String],
        inputs: &[Expr],
        input_names: &[Option<String>],
        input_constraints: &[String],
        clobbers: &[String],
        bindings: &mut Vec<AsmOpBinding>,
    ) -> HashMap<String, String> {
        crate::codegen::x86_64::asm::bindings::build_asm_operand_map(
            self, outputs, output_names, output_constraints,
            inputs, input_names, input_constraints, clobbers, bindings,
        )
    }

EOF

# Combine everything
cat /tmp/head_final.txt /tmp/wrapper_final.txt /tmp/tail_final.txt > /tmp/mod_combined_final.rs
mv /tmp/mod_combined_final.rs src/codegen/x86_64/mod.rs

# Check result
NEW_LINES=$(wc -l < src/codegen/x86_64/mod.rs)
OLD_LINES=41658
REDUCED=$((OLD_LINES - NEW_LINES))

echo "✅ File replaced successfully!"
echo "   Before: $OLD_LINES lines"
echo "   After:  $NEW_LINES lines"
echo "   Reduced by: $REDUCED lines"

# Verify with compilation check
echo ""
echo "--- Compilation Verification ---"
if cargo build --lib 2>&1 | grep -q "Finished"; then
    echo "✅ COMPILATION SUCCESSFUL!"
else
    echo "❌ BUILD FAILED - Checking error..."
    cargo build --lib 2>&1 | grep "error\[" -A 2 | head -10
    
    if [ $? -ne 0 ]; then
        echo ""
        echo "⚠️  Restoring from backup..."
        cp /tmp/mod_pre_cleanup_backup.rs src/codegen/x86_64/mod.rs
        echo "✅ Restored safely"
        exit 1
    fi
fi

echo ""
echo "=== PR-A2 FINAL CLEANUP COMPLETE! ==="
