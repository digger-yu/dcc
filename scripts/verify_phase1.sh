#!/bin/bash
# Phase 1 Verification Script
# Checks if all import fixes are properly applied

set -e

echo "=========================================="
echo "Phase 1 Verification Report"
echo "=========================================="
echo ""

ERRORS=$(cargo build 2>&1 | grep -c "^error")
echo "Total Compilation Errors: $ERRORS"
echo "Target: <1500"
echo ""

# ============================================
# Verification Checkpoints
# ============================================

echo "=== Verification Checkpoints ==="
echo ""

# 1. HashMap imports
echo "1. HashMap Import Coverage:"
MISSING_HASHMAP=$(find src/codegen -name "*.rs" -path "*/codegen/*" \
    | xargs grep -L "use std::collections::HashMap;" 2>/dev/null | wc -l || echo "0")
TOTAL_CODEGEN_FILES=$(find src/codegen -name "*.rs" -path "*/codegen/*" | wc -l)
HAS_HASHMAP=$(find src/codegen -name "*.rs" -path "*/codegen/*" \
    | xargs grep -l "HashMap" | wc -l)

echo "   Total codegen files: $TOTAL_CODEGEN_FILES"
echo "   Files using HashMap: $HAS_HASHMAP"  
echo "   Missing HashMap import: $MISSING_HASHMAP"
if [ "$MISSING_HASHMAP" -eq 0 ]; then
    echo "   ✅ All HashMap users have imports"
else
    echo "   ⚠️  Missing in following files:"
    find src/codegen -name "*.rs" -path "*/codegen/*" \
        | xargs grep -L "use std::collections::HashMap;" 2>/dev/null | head -10
fi
echo ""

# 2. AST type imports
echo "2. AST Type Import Coverage:"
FILES_WITH_AST_IMPORT=$(find src/codegen -name "*.rs" -path "*/codegen/*" \
    | xargs grep -l "use crate::ast::" | wc -l)
FILES_WITHOUT_AST=$(find src/codegen -name "*.rs" -path "*/codegen/*" \
    | xargs grep -L "use crate::ast::" | wc -l)

echo "   Files with AST imports: $FILES_WITH_AST_IMPORT"
echo "   Files without AST imports: $FILES_WITHOUT_AST"
if [ "$FILES_WITHOUT_AST" -lt 5 ]; then
    echo "   ✅ Most files have AST imports"
else
    echo "   ⚠️  Some files missing AST imports:"
    find src/codegen -name "*.rs" -path "*/codegen/*" \
        | xargs grep -L "use crate::ast::" 2>/dev/null | head -5
fi
echo ""

# 3. Self-reference errors
echo "3. Self-Reference Fixes:"
SELF_ERRORS=$(cargo build 2>&1 | grep -c "expected value, found module \`self\`" || echo "0")
echo "   Remaining self-reference errors: $SELF_ERRORS"
if [ "$SELF_ERRORS" -eq 0 ]; then
    echo "   ✅ All self-reference issues resolved"
else
    echo "   ⚠️  Self-reference errors still present"
    cargo build 2>&1 | grep "expected value, found module \`self\`" | head -3
fi
echo ""

# 4. Shared module exports
echo "4. Shared Module Exports:"
SHARED_MOD="/root/dcc/src/codegen/shared/mod.rs"
if [ -f "$SHARED_MOD" ]; then
    HAS_EXPR_EXPORT=$(grep -c "pub use crate::ast::{Expr" "$SHARED_MOD" || echo "0")
    HAS_TYPE_EXPORT=$(grep -c "pub use crate::ast::.*Type" "$SHARED_MOD" || echo "0")
    HAS_VALUE_TYPE_EXPORT=$(grep -c "ValueType" "$SHARED_MOD" || echo "0")
    
    echo "   Expr export present: $([ $HAS_EXPR_EXPORT -gt 0 ] && echo '✅' || echo '❌')"
    echo "   Type export present: $([ $HAS_TYPE_EXPORT -gt 0 ] && echo '✅' || echo '❌')"
    echo "   ValueType export present: $([ $HAS_VALUE_TYPE_EXPORT -gt 0 ] && echo '✅' || echo '❌')"
else
    echo "   ❌ shared/mod.rs not found!"
fi
echo ""

# 5. Top remaining errors
echo "5. Error Distribution (Top 10):"
cargo build 2>&1 | grep "error\[" | sort | uniq -c | sort -rn | head -10 | \
    awk '{printf "   %5d errors: %s\n", $1, $2}'
echo ""

# ============================================
# Final Assessment
# ============================================
echo "=== Final Assessment ==="
echo ""

PASS_COUNT=0
TOTAL_CHECKS=5

if [ "$ERRORS" -lt 1500 ]; then
    echo "✅ Error count target met: $ERRORS < 1500"
    PASS_COUNT=$((PASS_COUNT + 1))
else
    echo "❌ Error count target NOT met: $ERRORS >= 1500"
fi

if [ "$MISSING_HASHMAP" -eq 0 ]; then
    echo "✅ HashMap imports complete"
    PASS_COUNT=$((PASS_COUNT + 1))
else
    echo "❌ HashMap imports incomplete: $MISSING_HASHMAP files missing"
fi

if [ "$FILES_WITHOUT_AST" -lt 5 ]; then
    echo "✅ AST imports widespread"
    PASS_COUNT=$((PASS_COUNT + 1))
else
    echo "❌ AST imports incomplete: $FILES_WITHOUT_AST files missing"
fi

if [ "$SELF_ERRORS" -eq 0 ]; then
    echo "✅ Self-reference issues fixed"
    PASS_COUNT=$((PASS_COUNT + 1))
else
    echo "❌ Self-reference issues persist: $SELF_ERRORS errors"
fi

HAS_ALL_EXPORTS=$((HAS_EXPR_EXPORT > 0 && HAS_TYPE_EXPORT > 0 && HAS_VALUE_TYPE_EXPORT > 0))
if [ "$HAS_ALL_EXPORTS" -eq 1 ]; then
    echo "✅ Shared exports complete"
    PASS_COUNT=$((PASS_COUNT + 1))
else
    echo "❌ Shared exports incomplete"
fi

echo ""
echo "Checkpoint Pass Rate: $PASS_COUNT/$TOTAL_CHECKS"
echo ""

if [ $PASS_COUNT -ge 4 ]; then
    echo "🎉 Phase 1 ALMOST COMPLETE - Proceeding to Phase 2 recommended"
elif [ $PASS_COUNT -ge 3 ]; then
    echo "⚠️  Phase 1 PARTIAL - Finish remaining checks before Phase 2"
else
    echo "❌ Phase 1 INCOMPLETE - Complete critical fixes first"
fi

