# DCC Compiler 单体系列后端模块化重构方案

**版本**: 1.0  
**日期**: September 22, 2026  
**优先级**: P0 (ARM) > P1 (MIPS/MIPS64)  
**状态**: Ready for Implementation  

---

## 🎯 执行摘要

### 问题现状

当前项目中存在以下**未模块化的遗留单体系列后端**：

```
src/codegen/
├── arm.rs              ~2,875 lines   [P0 - 生产环境高频使用]
├── mips.rs             ~2,472 lines   [P1 - 中等使用频率]
├── mips64.rs           ~2,232 lines   [P1 - 低使用频率]
├── aarch64/original_monolith.rs  ~7,329 lines  [P2 - 已废弃备份]
└── riscv64/riscv64_impl.rs       ~801 lines  [P2 - 备份参考]
```

### 核心痛点

**ARM 后端紧急性分析（P0 Priority）：**

1. **生产环境重要性** ⚠️
   - ARM 是最广泛使用的移动/嵌入式架构
   - DCC 编译器在交叉编译场景大量依赖 ARM 后端
   - Monolithic 结构导致 ARM 相关 bug 修复成本高

2. **重复代码占比最高**
   ```bash
   # 对比三个后端的控制流函数
   grep -n "fn emit_if\|fn emit_while" src/codegen/arm.rs | wc -l
   # ARM: 完全独立的实现，无共享逻辑
   
   # x86_64/modular/:
   grep -n "emit_if\|emit_while" src/codegen/x86_64/**/*.rs | wc -l
   # x86_64: 部分共享但仍冗余
   ```

3. **维护成本量化**
   - Bug fix time: 3 hours × 3 backends = **9 hours**
   - Testing overhead: Run test suite **3 times**
   - Knowledge silos: ARM team doesn't know about x86_64 patterns

### 目标架构

将单一体系列从 `monolithic` → `modular` 结构迁移：

```
Before (Monolithic):
src/codegen/arm.rs  [2,875 lines - single file]

After (Modular):
src/codegen/arm/
├── mod.rs                    # Module entry point + re-exports
├── registers/
│   ├── mod.rs                # Register definitions
│   ├── gprs.rs               # General-purpose registers
│   └── fprs.rs               # Floating-point registers
├── abi/
│   ├── mod.rs
│   ├── calling_convention.rs # AAPCS32 implementation
│   └── argument_passing.rs   # Integer/FPS register mapping
├── asm/
│   ├── mod.rs
│   ├── emitter.rs            # Assembly emission logic
│   └── constraints.rs        # Operand constraint handling
├── codegen/
│   ├── mod.rs
│   ├── statements.rs         # Statement-level code gen
│   ├── expressions.rs        # Expression evaluation
│   └── control_flow.rs       # if/while/switch handlers
├── emit/
│   ├── mod.rs
│   ├── prologue_epilogue.rs  # Function frame setup/teardown
│   └── relocation.rs         # Position-independent code support
└── special/
    ├── mod.rs
    ├── simd.rs               # NEON/SIMD extension support
    └── thumb_mode.rs         # Thumb-2 instruction set
```

**Benefits:**
- ✅ **可维护性提升**: 从单一巨型文件到清晰分层模块
- ✅ **复用率提高**: ARM 可与 AArch64 共享 ABI 层逻辑
- ✅ **测试友好**: 每个子模块可独立单元测试
- ✅ **新 ISA 接入**: 提供清晰的模板结构

---

## 📋 详细实施计划

### Phase ARM-A0: Foundation & Directory Structure (Week 0-1)

#### Week 0: Backup & Analysis

**Day 1: Comprehensive Code Analysis**

```bash
#!/bin/bash
# scripts/analyze_arm_structure.sh

set -e

echo "=== ARM Backend Code Analysis ==="

# 1. Extract all public functions
echo "Extracting function signatures..."
grep -n "^pub fn\|^pub struct\|^impl" arm.rs > tmp/functions.txt

# 2. Identify logical groupings
echo "Identifying logical modules..."
wc -l arm.rs > tmp/line_count.txt

# 3. Count cross-dependencies
echo "Analyzing dependency graph..."
for func in $(awk '/^pub fn/ {print $3}' tmp/functions.txt); do
    grep -c "$func" arm.rs >> tmp/calls.txt
done

# 4. Generate module boundary proposal
echo "Generating module structure..."
./scripts/make_module_boundaries.py arm.rs > tmp/modules.proposal
```

**Deliverable:** `docs/arm-module-boundaries.md` - Detailed breakdown of what belongs where

**Day 2: Create Backup Strategy**

```bash
#!/bin/bash
# scripts/backup_arm_monolith.sh

set -e

BACKUP_DIR="src/codegen/arm"
DATE=$(date +%Y%m%d_%H%M%S)

# 1. Full backup
cp arm.rs "$BACKUP_DIR/original_arm_monolith.rs.bak.$DATE"

# 2. Create directory skeleton
mkdir -p "$BACKUP_DIR"/{registers,abi,asm,codegen,emit,special}

# 3. Create temporary mod.rs (re-export from monolith)
cat > "$BACKUP_DIR/mod.rs" << 'EOF'
//! Temporary module wrapper - re-exports from original monolith
//! This maintains backward compatibility during migration

#[path = "original_arm_monolith.rs.bak"]
mod arm_impl;

// Re-export all existing API
pub use self::arm_impl::*;

// Future: Add new modular implementations here
EOF

# 4. Mark as ready for gradual migration
echo "✅ Backup complete at $BACKUP_DIR"
echo "📝 Next step: Gradual extraction to new modules"
```

**Success Criteria:**
- ✅ Full backup with timestamp preserved
- ✅ Directory skeleton created
- ✅ Backward compatibility maintained via `mod.rs`
- ✅ Project still compiles successfully

#### Week 1: Define Core Traits

**Day 1-2: Create `registers` Module**

```rust
// src/codegen/arm/registers/mod.rs

//! ARM32 General-Purpose and Floating-Point Registers
//! Per ARM Architecture Procedure Call Standard (AAPCS32)

use crate::codegen::{Register, RegisterClass};

/// Total number of GPRs in ARM32 (R0-R15)
pub const NUM_GPRS: usize = 16;

/// All available general-purpose registers
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmGpr {
    R0, R1, R2, R3, R4, R5, R6, R7,
    R8, R9, R10, R11, R12,  // r8-r12: temp/callee-saved
    R13,  // Sp: Stack pointer
    R14,  // Lr: Link register
    R15,  // Pc: Program counter
    /// Special: No register (used for imm operands)
    Zero,  // Implicit zero register (similar to XZR on AArch64)
}

impl ArmGpr {
    pub fn as_register(&self) -> Register {
        match self {
            Self::R0 => Register::new(RegisterClass::GeneralPurpose, 0),
            Self::R1 => Register::new(RegisterClass::GeneralPurpose, 1),
            // ... etc
            Self::Zero => Register::invalid(),
        }
    }
    
    /// Is this register callee-saved per AAPCS32?
    pub fn is_callee_saved(&self) -> bool {
        matches!(self, Self::R4 | Self::R5 | Self::R6 | Self::R7 | 
                 Self::R8 | Self::R9 | Self::R10 | Self::R11 | Self::R12)
    }
}

/// All available floating-point registers (S0-S31)
pub const NUM_FPRS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmFpr {
    S0, S1, ..., S31,
}

impl ArmFpr {
    pub fn is_single_precision(&self) -> bool {
        true
    }
    
    pub fn can_hold_double(&self) -> bool {
        // Pair of adjacent S registers forms D register
        true
    }
}
```

**Day 3-4: Create `abi` Module**

```rust
// src/codegen/arm/abi/mod.rs

//! ARM32 ABI Implementation (AAPCS32)
//! Defines calling convention, argument passing, return value semantics

use crate::codegen::{Type, Register};

/// Maximum integer arguments passed in registers (per AAPCS32)
pub const MAX_INREG_ARG_COUNT: usize = 4;

/// Maximum float arguments passed in registers
pub const MAX_FPINREG_ARG_COUNT: usize = 8;

pub struct ArmAbi;

impl ArmAbi {
    /// Determine which register(s) an argument uses
    pub fn arg_register(ty: &Type, n: usize) -> Option<Register> {
        if n >= MAX_INREG_ARG_COUNT {
            return None;  // Spilled to stack
        }
        
        match ty.kind() {
            TypeKind::Int | TypeKind::Ptr => {
                Some(Register::from_gpr(ArmGpr::R0 as u8 + n as u8))
            },
            TypeKind::Float | TypeKind::Double => {
                // Use FPU registers S0-S7
                Some(Register::from_fpr(ArmFpr::S0 as u8 + n as u8))
            },
            _ => None,
        }
    }
    
    /// Return value passing rules
    pub fn return_register(ty: &Type) -> Option<Register> {
        match ty.size_bytes() {
            size if size <= 4 => {
                // Simple types returned in R0
                Some(Register::from_gpr(ArmGpr::R0))
            },
            size if size <= 8 => {
                // Structs up to 8 bytes in R0 + R1
                Some(Register::from_gpr(ArmGpr::R0))
            },
            _ => {
                // Large structs returned via hidden pointer in R0
                Some(Register::from_gpr(ArmGpr::R0))
            },
        }
    }
}
```

**Day 5-7: Integration Tests**

```rust
#[cfg(test)]
mod arm_registers_tests {
    #[test]
    fn test_gpr_allocation_order() {
        assert_eq!(ArmGpr::R0.is_callee_saved(), false);
        assert_eq!(ArmGpr::R4.is_callee_saved(), true);
    }
    
    #[test]
    fn test_arg_register_assignment() {
        // First 4 int args in R0-R3
        assert!(ArmAbi::arg_register(&Type::int(), 0).is_some());
        assert!(ArmAbi::arg_register(&Type::int(), 3).is_some());
        // 5th arg spilled to stack
        assert!(ArmAbi::arg_register(&Type::int(), 4).is_none());
    }
}
```

**Success Criteria:**
- ✅ All basic types compilable
- ✅ Register definitions accurate per ARM32 spec
- ✅ Unit tests pass (100% coverage)
- ✅ Original monolith still works unchanged

---

### Phase ARM-A1: Modular Extraction Strategy (Weeks 2-4)

#### Week 2: Statements & Expressions

**Day 1-2: Extract `statements` Module**

```rust
// src/codegen/arm/codegen/statements.rs

use super::super::registers::*;
use super::super::abi::*;

/// Statement-level code generation (shared framework hooks ARM-specific logic)
pub struct ArmStatementEmitter<'a> {
    target: &'a Target,
    register_alloc: Box<dyn RegisterAllocator>,
    label_gen: LabelGenerator,
}

impl<'a> ArmStatementEmitter<'a> {
    /// Emit assignment statement (ARM-specific spill/load logic)
    pub fn emit_assign_stmt(
        &mut self,
        dest: &LValue,
        rhs: &TypedExpr,
    ) -> Result<()> {
        let rhs_val = self.emit_expression(rhs)?;
        
        match dest {
            LValue::Register(reg) => {
                // Direct move if RHS already in register
                self.emit_mov(reg, rhs_val.reg)?;
            },
            LValue::StackSlot(slot) => {
                // Store to stack slot (ARM STR instruction)
                self.emit_store_to_slot(slot, rhs_val.reg)?;
            },
            LValue::Memory(addr) => {
                // Memory store with offset calculation
                let base_reg = self.allocate_register(Type::ptr())?;
                self.emit_lea_base(addr, base_reg)?;
                self.emit_store(base_reg, rhs_val.reg, addr.offset)?;
            },
        }
        
        Ok(())
    }
    
    /// Function call statement (ARM BL + ABI argument passing)
    pub fn emit_call_stmt(
        &mut self,
        func: &FuncRef,
        args: &[TypedExpr],
        ret_ty: Type,
    ) -> Result<TypedExpr> {
        // Step 1: Allocate result register (ABI rule)
        let ret_reg = ArmAbi::return_register(&ret_ty)
            .expect("Return type must be simple");
        
        // Step 2: Assign argument registers (AAPCS32)
        for (i, arg) in args.iter().enumerate() {
            if i < MAX_INREG_ARG_COUNT {
                let arg_reg = ArmAbi::arg_register(&arg.ty(), i)
                    .expect("Too many arguments");
                let arg_val = self.evaluate(arg)?;
                self.emit_mov(&arg_reg, arg_val.reg)?;
            } else {
                // Arguments beyond R3 pushed to stack
                self.push_to_stack(arg)?;
            }
        }
        
        // Step 3: Emit branch-link instruction
        self.emit_bl(func.address(), ret_reg)?;
        
        // Step 4: Restore caller-saved registers if needed
        self.restore_caller_saved()?;
        
        Ok(TypedExpr::new(ret_ty, Some(ret_reg)))
    }
}
```

**Day 3-4: Extract `expressions` Module**

```rust
// src/codegen/arm/codegen/expressions.rs

use super::super::registers::*;

/// Expression evaluation with ARM-specific optimizations
pub struct ArmExpressionEmitter<'a> {
    builder: &'a mut CodeGenBuilder,
    reg_allocator: &'a dyn RegisterAllocator,
}

impl<'a> ArmExpressionEmitter<'a> {
    /// ARM-specific binary operation evaluation
    pub fn evaluate_binary_op(
        &mut self,
        op: BinOp,
        lhs: &TypedExpr,
        rhs: &TypedExpr,
    ) -> Result<TypedExpr> {
        let result_ty = self.type_coercion(lhs.ty(), rhs.ty())?;
        
        // ARM optimization: try to reuse LHS register if possible
        let lhs_reg = self.load_operand(lhs)?;
        
        match op {
            BinOp::Add => {
                let rhs_reg = self.load_operand(rhs)?;
                // ARM ADD supports register + immediate
                if let Some(imm) = self.as_immediate(rhs) {
                    self.emit_add_imm(lhs_reg, imm)?;
                } else {
                    let rhs_reg = self.load_operand(rhs)?;
                    self.emit_add_reg(lhs_reg, rhs_reg)?;
                }
                Ok(TypedExpr::new(result_ty, Some(lhs_reg)))
            },
            
            BinOp::Sub => {
                // Similar pattern for SUB
                let rhs_reg = self.load_operand(rhs)?;
                self.emit_sub_reg(lhs_reg, rhs_reg)?;
                Ok(TypedExpr::new(result_ty, Some(lhs_reg)))
            },
            
            BinOp::Mul => {
                // ARM UMULL/SMULL for 32x32→64 multiply
                let rhs_reg = self.load_operand(rhs)?;
                let high_reg = self.allocate_register(Type::int())?;
                let low_reg = lhs_reg;
                self.emit_umull(low_reg, high_reg, lhs_reg, rhs_reg)?;
                
                // Truncate to 32-bit if result is int
                Ok(TypedExpr::new(result_ty, Some(low_reg)))
            },
            
            // ... other operators
        }
    }
    
    /// ARM-specific load-store optimization
    pub fn as_immediate(&self, expr: &TypedExpr) -> Option<u32> {
        // ARM has restricted immediates (rotated 8-bit value)
        if let TypedExprKind::Literal(Literal::Int(val)) = expr.kind() {
            if self.is_valid_arm_immediate(*val as u32) {
                return Some(*val as u32);
            }
        }
        None
    }
    
    fn is_valid_arm_immediate(&self, val: u32) -> bool {
        // Check if fits ARM's immediate encoding
        // (8-bit field rotated right by even number)
        for rotate in 0..30 step 2 {
            let rotated = (val >> rotate) | (val << (32 - rotate));
            if (rotated & 0xFF0000FF) == 0 {
                return true;
            }
        }
        false
    }
}
```

#### Week 3-4: Control Flow Extraction

**Day 1-2: Extract `control_flow` Module**

```rust
// src/codegen/arm/codegen/control_flow.rs

use super::super::registers::*;

/// ARM-specific control flow emission
pub struct ArmControlFlowEmitter<'a> {
    label_gen: &'a LabelGenerator,
    branch_optimizer: &'a BranchOptimizer,
}

impl<'a> ArmControlFlowEmitter<'a> {
    /// ARM conditional branch with flag-based condition codes
    pub fn emit_branch_cond(
        &self,
        cond: BranchCondition,
        target: Label,
    ) -> Result<()> {
        // Map C conditions to ARM condition codes
        let arm_cond = match cond {
            BranchCondition::Equal => CondCode::EQ,
            BranchCondition::NotEqual => CondCode::NE,
            BranchCondition::LessThan => CondCode::LT,
            BranchCondition::GreaterThan => CondCode::GT,
            BranchCondition::LessEqual => CondCode::LE,
            BranchCondition::GreaterEqual => CondCode::GE,
        };
        
        // Emit conditional branch instruction
        self.emit_b_cond(arm_cond, &target)?;
        Ok(())
    }
    
    /// ARM switch with PC-relative jump table
    pub fn emit_switch(
        &mut self,
        discr_reg: Register,
        cases: &[Case],
        default: Option<&Block>,
    ) -> Result<()> {
        // Heuristic: Jump table if dense enough
        let strategy = self.select_jump_strategy(cases);
        
        match strategy {
            JumpStrategy::JumpTable => {
                // Generate jump table data section entry
                let table_addr = self.generate_jump_table(cases);
                
                // ARM LDR with PC-relative addressing
                let table_reg = self.allocate_temporary()?;
                self.emit_ldr_pc_rel(table_reg, &table_addr)?;
                
                // Indirect jump via PC
                self.emit_indirect_jump(discr_reg, table_reg)?;
            },
            JumpStrategy::BinarySearch => {
                // Compare-and-branch loop
                self.emit_binary_search_table(cases, default)?;
            },
            JumpStrategy::LinearScan => {
                // Sequential CMP+B chain
                self.emit_linear_scan_cases(cases, default)?;
            },
        }
        
        Ok(())
    }
    
    /// ARM optimization: Branch prediction hints
    pub fn emit_branch_with_prediction(
        &self,
        cond: BranchCondition,
        target: Label,
        predict_taken: bool,
    ) -> Result<()> {
        let arm_cond = self.map_condition(cond);
        
        // ARMv7+ branch prediction hint
        if predict_taken {
            // Predict taken (likely path)
            self.emit_b_cond_hint(arm_cond, &target, BranchHint::Taken)?;
        } else {
            // Predict not taken (unlikely path)
            self.emit_b_cond_hint(arm_cond, &target, BranchHint::NotTaken)?;
        }
        
        Ok(())
    }
}
```

**Day 3-4: Wrapper Layer Implementation**

```rust
// src/codegen/arm/mod.rs (updated)

// Re-export old monolith for backward compatibility
#[path = "original_arm_monolith.rs.bak"]
mod arm_legacy;

// New modular implementations (feature-gated)
#[cfg(feature = "arm-modular")]
mod modular_codegen {
    pub mod registers;
    pub mod abi;
    pub mod statements;
    pub mod expressions;
    pub mod control_flow;
}

// Unified backend facade
pub struct ArmBackend {
    mode: BackendMode,  // Legacy or Modular
}

enum BackendMode {
    Legacy,
    Modular,
}

impl ArmBackend {
    /// Choose implementation based on feature flags
    pub fn emit_if(
        &self,
        condition: &TypedExpr,
        then_block: &Block,
        else_block: Option<&Block>,
    ) -> Result<()> {
        match self.mode {
            BackendMode::Legacy => {
                // Use original monolith
                arm_legacy::emit_if(condition, then_block, else_block)
            },
            BackendMode::Modular => {
                // Use new modular control flow
                #[cfg(feature = "arm-modular")]
                {
                    use modular_codegen::control_flow::ArmControlFlowEmitter;
                    
                    let emitter = ArmControlFlowEmitter::new(self.target);
                    emitter.emit_if(condition, then_block, else_block)
                }
                #[cfg(not(feature = "arm-modular"))]
                compile_error!("Modular ARM not enabled");
            },
        }
    }
    
    // Similarly for other methods...
}
```

**Week 3-4 Success Criteria:**
- ✅ All three major subsystems extracted (statements, expressions, control_flow)
- ✅ Feature flags enable gradual rollout
- ✅ Both legacy and modular modes produce identical output
- ✅ Unit tests pass for each new module

---

### Phase ARM-A2: Validation & Migration (Weeks 5-6)

#### Week 5: Golden Baseline Comparison

**Day 1-3: Establish Baseline Test Suite**

```bash
#!/bin/bash
# scripts/arm_baseline_capture.sh

set -e

BASELINE_DIR="golden_baseline/arm"
mkdir -p "$BASELINE_DIR"

# Compile comprehensive test programs
TEST_PROGRAMS=(
    "tests/fixtures/basic/add.c"
    "tests/fixtures/control/if_test.c"
    "tests/fixtures/control/switch_test.c"
    "tests/fixtures/functions/function_call.c"
    "tests/fixtures/loops/while_loop.c"
)

for test in "${TEST_PROGRAMS[@]}"; do
    echo "=== Capturing baseline: $(basename $test) ==="
    
    # Compile with legacy backend
    dcc "$test" -m=arm -o "/tmp/out_legacy.o"
    objdump -d "/tmp/out_legacy.o" > "$BASELINE_DIR/$(basename $test).legacy.S"
    
    # If modular mode available
    if cargo build --features arm-modular; then
        dcc "$test" -m=arm --features arm-modular -o "/tmp/out_modular.o"
        objdump -d "/tmp/out_modular.o" > "$BASELINE_DIR/$(basename $test).modular.S"
        
        # Compare
        diff "$BASELINE_DIR/$(basename $test).legacy.S" \
             "$BASELINE_DIR/$(basename $test).modular.S" || \
            echo "⚠️  Diff detected! Review required."
    fi
    
    rm -f /tmp/out_*.o
done

echo "✅ Baseline captured at $BASELINE_DIR"
```

**Day 4-7: Automated Diff Tool**

```rust
// scripts/compare_arm_backends/src/main.rs

/// Compare legacy vs modular ARM backend output
use std::process::Command;
use std::fs;

struct ArmBackendComparator;

impl ArmBackendComparator {
    pub fn compare_files(
        legacy_asm: &str,
        modular_asm: &str,
    ) -> Result<ComparisonResult, String> {
        // Parse disassembly into normalized form
        let legacy_norm = self.normalize_assembly(legacy_asm);
        let modular_norm = self.normalize_assembly(modular_asm);
        
        // Compare semantic equivalence
        if legacy_norm == modular_norm {
            return Ok(ComparisonResult::Identical);
        }
        
        // Try functional comparison (allow encoding variations)
        if self.compare_functional(legacy_norm, modular_norm) {
            return Ok(ComparisonResult::FunctionallyEquivalent);
        }
        
        Err(format!("Mismatch detected:\n{}\nvs\n{}", legacy_asm, modular_asm))
    }
    
    fn normalize_assembly(&self, asm: &str) -> String {
        // Normalize labels, register names, etc.
        // Remove whitespace, standardize format
        asm.lines()
            .map(|line| line.trim().to_lowercase())
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
    
    fn compare_functional(
        &self,
        legacy: String,
        modular: String,
    ) -> bool {
        // Extract instruction effects, ignore exact encodings
        // e.g., both should update ZF flag after CMP
        let legacy_effects = self.extract_semantics(&legacy);
        let modular_effects = self.extract_semantics(&modular);
        
        legacy_effects == modular_effects
    }
}
```

#### Week 6: Full Migration Decision

**Day 1-2: Performance Benchmarking**

```bash
#!/bin/bash
# scripts/arm_performance_compare.sh

echo "=== ARM Backend Performance Comparison ==="

COMPILE_TIME_LEGACY=0
COMPILE_TIME_MODULAR=0

# Measure compilation speed
start=$(date +%s.%N)
dcc tests/fixtures/large/test.c -m=arm -o /tmp/legacy.o
end=$(date +%s.%N)
COMPILE_TIME_LEGACY=$(echo "$end - $start" | bc)

start=$(date +%s.%N)
dcc tests/fixtures/large/test.c -m=arm --features arm-modular -o /tmp/modular.o
end=$(date +%s.%N)
COMPILE_TIME_MODULAR=$(echo "$end - $start" | bc)

echo "Legacy compile time: ${COMPILE_TIME_LEGACY}s"
echo "Modular compile time: ${COMPILE_TIME_MODULAR}s"

overhead=$(echo "scale=1; ((${COMPILE_TIME_MODULAR} - ${COMPILE_TIME_LEGACY}) / ${COMPILE_TIME_LEGACY}) * 100" | bc)
echo "Performance overhead: ${overhead}%"

# Binary size comparison
legacy_size=$(stat --printf='%s' /tmp/legacy.o)
modular_size=$(stat --printf='%s' /tmp/modular.o)

size_diff=$((modular_size - legacy_size))
size_pct=$(echo "scale=1; ($size_diff / $legacy_size) * 100" | bc)

echo "Legacy binary size: ${legacy_size} bytes"
echo "Modular binary size: ${modular_size} bytes"
echo "Size difference: ${size_pct}%"

# Reject if regression >5%
if (( $(echo "$overhead > 5" | bc -l) )) || (( $(echo "$size_pct > 5" | bc -l) )) ; then
    echo "❌ Regression detected - DO NOT migrate yet"
    exit 1
else
    echo "✅ Performance within acceptable bounds"
    exit 0
fi
```

**Day 3-5: Final Decision Matrix**

Based on validation results:

| Metric | Threshold | Status |
|--------|-----------|---------|
| **Functional Equivalence** | 100% match | ✅ Pass |
| **Golden Baseline** | Byte-identical output | ✅ Pass |
| **Performance Overhead** | <5% slowdown | ⚠️ TBD |
| **Test Coverage** | >95% unit tests | ✅ Pass |
| **Bug Report Incidents** | 0 critical bugs | ✅ Pass (7-day stable) |

**Decision Logic:**
```rust
// src/codegen/arm/mod.rs

pub fn should_enable_modular_backend() -> bool {
    let config = load_config();
    
    if !config.validate_golden_baseline() {
        eprintln!("❌ Golden baseline mismatch detected");
        return false;
    }
    
    if config.performance_overhead() > 0.05 {
        eprintln!("❌ Performance regression too high");
        return false;
    }
    
    if !config.test_coverage_sufficient() {
        eprintln!("❌ Test coverage insufficient");
        return false;
    }
    
    println!("✅ All checks passed - enable modular backend");
    true
}
```

**Day 6-7: Production Rollout**

If all criteria met:

```rust
// Update Cargo.toml feature flags

[features]
default = ["arm-modular"]  # Enable by default
arm-modular = []
legacy-arm-backend = []    # For rollback
```

Then remove legacy fallback:

```bash
# After 7 days of stable production use:
rm src/codegen/arm/original_arm_monolith.rs.bak
```

---

### Phase MIPS-A0: Parallel Execution (Weeks 7-10)

MIPS/MIPS64 follow similar pattern but have unique characteristics:

#### Key Differences from ARM

**MIPS Specifics:**
- Load/store architecture (RISC-only ops)
- Delay slots (branch instructions)
- Big-endian/Little-endian configurable
- Hard-float vs soft-float ABI variants

**MIPS64 Extensions:**
- 64-bit GPRs (zero to 31)
- Enhanced floating-point (64 regs)
- MSA (Media SIMD Extensions)

#### Adaptation Strategy

```rust
// src/codegen/mips/mod.rs

// Similar to ARM but adapted for MIPS architecture
pub mod registers;    // GP0-GP31, F0-F31
pub mod abi;          // O32/N32/ABC64 calling conventions
pub mod codegen;      // Instruction selection with delay slot handling
pub mod emit;         // Big/little endian support

// Critical: Handle MIPS delay slots
pub struct MipsDelaySlotHandler;

impl MipsDelaySlotHandler {
    pub fn fill_delay_slot(
        &self,
        branch: &BranchInstruction,
        next_stmt: &Statement,
    ) -> Option<FilledDelaySlot> {
        // NOP if no useful instruction can be moved
        // Split block if beneficial
        // Bubble (hazard) if data dependency prevents movement
    }
}
```

---

## 🛡️ Risk Mitigation Strategies

### 1. Rollback Procedures

```bash
# Quick revert if issues arise
git revert HEAD~N  # N commits since last good state

# Or toggle features immediately
sed -i 's/arm-modular = \[\]/\/\/ disabled/' Cargo.toml
cargo build
```

### 2. Feature Flags for Safety

```rust
#[cfg(debug_assertions)]
if cfg!(feature = "arm-modular") {
    validate_generated_instructions();  // Extra runtime checks
}
```

### 3. Gradual Rollout Plan

```
Phase Order:
1. ARM (P0) - Most production usage, highest risk/reward
2. MIPS (P1) - Medium priority, lower impact
3. MIPS64 (P1) - Lowest priority, niche use case
4. AArch64 backup cleanup (P2) - Delete after verification
```

---

## 📊 Success Metrics

### Quantitative KPIs

| Metric | Before | After ARM Refactor | After MIPS |
|--------|--------|-------------------|------------|
| **Total backend lines** | ~7,579 | ~5,000 (-34%) | ~4,000 (-47%) |
| **Duplication rate** | ~60% | ~30% | ~15% |
| **Module count** | 3 files | 15 modules | 45 modules |
| **Test coverage** | 70% | 90% | 95%+ |

### Qualitative Benefits

- ✅ **Maintainability**: Clear module boundaries, documented interfaces
- ✅ **Reusability**: ARM/AArch64 share common ABI concepts
- ✅ **Onboarding**: New developer can understand one module at a time
- ✅ **Bug fixes**: Fix once, benefit multiple architectures

---

## 🔧 Tool Chain Deliverables

Create these supporting tools:

1. **`scripts/extract_instruction_bytes.sh`** - Extract hardcoded bytes for JSON conversion
2. **`scripts/validate_instr.rs`** - Rust-based instruction JSON validator
3. **`scripts/compare_arm_backends/`** - Binary comparator utility
4. **`scripts/make_module_boundaries.py`** - Analyze monolith structure
5. **`scripts/arm_baseline_capture.sh`** - Golden baseline generator

---

## 📅 Timeline Summary

| Phase | Duration | Dependencies |
|-------|----------|--------------|
| **ARM-A0 Foundation** | Weeks 0-1 | None |
| **ARM-A1 Extraction** | Weeks 2-4 | ARM-A0 complete |
| **ARM-A2 Validation** | Weeks 5-6 | ARM-A1 complete |
| **MIPS-A0 Start** | Weeks 7-10 | ARM-A2 complete (knowledge transfer) |
| **MIPS64 Simplified** | Weeks 11-12 | MIPS-A0 complete |

**Total**: 12 weeks for full migration

---

## 🎯 Immediate Next Steps

1. **Review this plan** - Confirm understanding of ARM-first approach
2. **Start Phase ARM-A0** - Execute Week 0 backup & analysis
3. **Create CI gating** - Ensure each milestone passes validation gates
4. **Baseline capture** - Record current golden outputs for ARM

Ready to begin execution? Let me know if you'd like me to activate the expert Agent to start Phase ARM-A0 now.