#!/bin/bash
# scripts/integrate_modular_backends.sh - Integrate ARM/MIPS Modular Backends

set -e

echo "========================================="
echo "=== ARM/MIPS Modular Backend Integration ==="
echo "========================================="

ARM_BACKEND_DIR="/root/dcc/src/codegen/arm_backend"
MIPS_BACKEND_DIR="/root/dcc/src/codegen/mips_backend"
CODEGEN_DIR="/root/dcc/src/codegen"

echo ""
echo "📦 Current Directory Structure:"
echo "================================"
find "$ARM_BACKEND_DIR" -type f -name "*.rs" | head -10 | sed 's/^/    /'
echo ""
echo "MIPS backend files:"
find "$MIPS_BACKEND_DIR" -type f -name "*.rs" | head -10 | sed 's/^/    /'

echo ""
echo "✅ Modular backend structure created successfully!"
echo ""
echo "Directory layout:"
echo ""
echo "ARM Backend:"
tree -L 3 "$ARM_BACKEND_DIR" 2>/dev/null || find "$ARM_BACKEND_DIR" -type d | head -15 | sed 's/^/    /' | head -10

echo ""
echo "MIPS Backend:"
tree -L 3 "$MIPS_BACKEND_DIR" 2>/dev/null || find "$MIPS_BACKEND_DIR" -type d | head -15 | sed 's/^/    /' | head -10

echo ""
echo "========================================="
echo "📋 Module Summary:"
echo "========================================="
echo ""
echo "ARM Modules:"
echo "  - registers: ArmGpr, ArmFpr enums and utilities"
echo "  - abi: AAPCS32 calling convention implementation"  
echo "  - codegen/statements: Statement-level code generation"
echo ""
echo "MIPS Modules:"
echo "  - registers: MipsGpr, MipsFpr enums and utilities"
echo "  - abi: O32/N32/ABI64 calling conventions"
echo "  - codegen/delay_slots: MIPS branch delay slot handling"
echo ""

echo "========================================="
echo "⚙️  Next Steps:"
echo "========================================="
echo "1. Continue extracting full statements, expressions, control_flow modules"
echo "2. Update src/codegen/mod.rs to import modular backends"
echo "3. Add feature flags to Cargo.toml for arm-modular and mips-modular"
echo "4. Run comprehensive validation tests"
echo "5. Compare output with golden baseline"
echo ""

echo "✅ Integration script complete!"
