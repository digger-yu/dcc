# DCC 编译错误修复快速检查清单

## 🎯 总体目标

从 **3090** 个编译错误减少到 **<100** 个，分三个 phase 执行。

---

## ✅ Phase 1: Import Fixes (2-4 小时)

### 运行脚本
```bash
cd /root/dcc
./scripts/phase1_fix_execution.sh
```

### 关键检查点
| 检查项 | 目标 | 验证命令 |
|-------|------|---------|
| HashMap 导入 | 所有使用 HashMap 的文件 | `./scripts/verify_phase1.sh` |
| AST 类型导入 | 80%+ codegen 文件 | 同上 |
| Self 引用修复 | 0 个 E0424 错误 | 同上 |
| Shared 导出 | Expr/Type/ValueType 已暴露 | 同上 |
| 错误总数 | <1500 | `cargo build 2>&1 \| grep "^error" \| wc -l` |

### 手动修复脚本（如果自动化失败）

#### Fix HashMap imports
```bash
# Add to files using HashMap but missing import
find src/codegen -name "*.rs" -path "*/codegen/*" \
  -exec grep -L "use std::collections::HashMap;" {} \; | \
  xargs -I {} sh -c 'echo "use std::collections::HashMap;" | cat - {} > /tmp/tmp.rs && mv /tmp/tmp.rs {}'
```

#### Fix shared/mod.rs exports
```bash
# Add these lines after "pub use types::CaseInfo;" in shared/mod.rs:
sed -i '/pub use types::CaseInfo;/a\
// Key backend types\
pub use crate::ast::{Expr, Type, ValueType};\
pub use crate::ir::{Module, Function, Global, ValueType};' src/codegen/shared/mod.rs
```

---

## 🔧 Phase 2: Trait Implementation Fixes (6-8 小时)

### 优先级排序
1. **RISC-V64** - Most errors, critical path
2. **AArch64** - Large error count  
3. **MIPS** - Minimal implementation needed
4. **ARM** - Existing, verify signatures

### ExpressionExecutor Implementation Template

For each backend, create/update `expression_impl.rs`:

```rust
// File: src/codegen/{arch}/expression_impl.rs

use crate::ast::{BinOp, UnOp, Type};
use crate::codegen::shared::expression_executor::{ExpressionExecutor, ExprResult, RegisterClass};

pub struct {Arch}ExpressionBackend<'a> {
    pub cg: &'a mut super::Codegen,
}

impl<'a> {Arch}ExpressionBackend<'a> {
    pub fn new(cg: &'a mut super::Codegen) -> Self {
        Self { cg }
    }
}

impl<'a> ExpressionExecutor for {Arch}ExpressionBackend<'a> {
    // Implement all 30+ methods from trait
    fn emit_binary_op(...) -> ExprResult<()> { /* ... */ }
    fn emit_unary_op(...) -> ExprResult<()> { /* ... */ }
    // ... more methods
}
```

### 批量实现步骤

```bash
# 1. Copy template from x86_64 as reference
cp src/codegen/x86_64/expression_impl.rs src/codegen/aarch64/expression_impl.rs

# 2. Replace arch-specific names
sed -i 's/X86_64/AArch64/g' src/codegen/aarch64/expression_impl.rs
sed -i 's/x86_64/aarch64/g' src/codegen/aarch64/expression_impl.rs

# 3. Repeat for mips, riscv64
```

---

## ✨ Phase 3: Polish & Cleanup (4-6 小时)

### 常见错误修复模式

#### E0308 - Type Mismatches
```rust
// Wrong signature:
fn foo(x: Type) -> Result<...> 

// Correct:
fn foo(x: &Type) -> Result<...>  // Often need reference
```

#### E0624 - Method Visibility
```rust
// Make private methods public:
pub(super) fn li_a0(&self, ...) { ... }  // Was just: fn li_a0(&self, ...)
```

#### E0277 - Trait Bounds
```rust
// Add missing bounds:
fn some_method<T: SomeTrait>(&self, value: T) { ... }
```

---

## 📊 状态跟踪

### 每日构建报告
```bash
# Run this daily
cd /root/dcc
cargo build 2>&1 | tee /tmp/build_$(date +%Y%m%d).log
echo "Errors:" $(grep -c "^error" /tmp/build_$(date +%Y%m%d).log)
```

### 错误趋势图表建议
Create `reports/error_trends.csv`:
```csv
date,total_errors,e0599_count,e0425_count
2026-09-23,3090,757,800
2026-09-24,<target>,<target>,<target>
```

---

## 🚀 立即开始

### Option A: Automated (Recommended)
```bash
cd /root/dcc
./scripts/phase1_fix_execution.sh
```

### Option B: Manual Step-by-Step

**Step 1**: Add HashMap imports
```bash
# Identify files needing HashMap
grep -r "HashMap" src/codegen --include="*.rs" -l | \
  while read file; do
    if ! grep -q "use std::collections::HashMap" "$file"; then
      echo "Adding HashMap to $file"
      sed -i '1i use std::collections::HashMap;' "$file"
    fi
done
```

**Step 2**: Fix self-references
```bash
# RISC-V64 expansion.rs
sed -i 's/pub fn expand_memory_address(/pub fn expand_memory_address(\&self, /' \
  src/codegen/riscv64/asm/expansion.rs

# RISC-V64 blocks.rs  
sed -i 's/pub fn build_from_statements(statements:/pub fn build_from_statements(\&self, statements:/g' \
  src/codegen/riscv64/codegen/blocks.rs
```

**Step 3**: Update shared exports
```bash
# Add to src/codegen/shared/mod.rs
echo '// Backend type exports
pub use crate::ast::{Expr, Type, ValueType};
pub use crate::ir::{Module, Function, Global, ValueType};' >> src/codegen/shared/mod.rs.tmp && \
cat src/codegen/shared/mod.rs.tmp src/codegen/shared/mod.rs > merged && \
rm src/codegen/shared/mod.rs src/codegen/shared/mod.rs.tmp
```

---

## 💡 Pro Tips

1. **Don't chase all errors at once** - Focus on workstream groups
2. **Verify after each phase** - Use verification script before proceeding
3. **Document as you go** - Update this checklist with observations
4. **Test incrementally** - Build frequently, catch issues early
5. **Use git strategically** - Commit after each phase completes

---

## 📝 所需工具

### Scripts Created
- `/root/dcc/scripts/phase1_fix_execution.sh` - Automation runner
- `/root/dcc/scripts/verify_phase1.sh` - Verification tool
- `/root/dcc/COMPILATION_ERROR_FIX_PLAN.md` - This detailed plan
- `/root/dcc/QUICK_FIX_CHECKLIST.md` - This quick reference

### Key Files Modified
- `src/codegen/shared/mod.rs` - Enhanced exports
- `src/codegen/mod.rs` - Convenience re-exports (optional)
- `src/codegen/riscv64/asm/expansion.rs` - Self fix #1
- `src/codegen/riscv64/codegen/blocks.rs` - Self fix #2
- All `src/codegen/*/codegen/*.rs` - Import additions

---

## 🆘 遇到问题？

### Common Issues

**Issue**: Script fails with "Permission denied"
```bash
chmod +x scripts/*.sh
```

**Issue**: Sed commands break file formatting
```bash
# Use backup-first approach
cp file.rs file.rs.backup
sed -i.bak 's/pattern/replacement/' file.rs
```

**Issue**: Changes cause new compile errors
```bash
# Check what changed
git diff src/codegen/shared/mod.rs
# Revert if breaking
git checkout src/codegen/shared/mod.rs
```

**Issue**: Unclear which backend method to implement
```bash
# Look at existing implementation
cat src/codegen/x86_64/expression_impl.rs | head -200
# Use as template
```

---

## 📈 Success Metrics

| Metric | Initial | Target | Status |
|--------|---------|--------|--------|
| Total Errors | 3090 | <100 | ⏳ In Progress |
| E0599 Count | 757 | <50 | ⏳ Pending |
| E0425 Count | ~800 | <50 | ⏳ Pending |
| Phase 1 Complete | No | Yes | ⏳ Not Started |
| Auto Tests Pass | ? | ✓ | ⏳ TBD |

---

**Document Version**: 1.0  
**Created**: September 23, 2026  
**Last Updated**: September 23, 2026  
**Status**: Ready for Execution  

---

## 🎬 Next Steps

1. Review this checklist with team
2. Execute Phase 1 automation
3. Verify results with checklists above
4. Proceed to Phase 2 if Phase 1 succeeds
5. Document learnings and adjust plan as needed

Good luck! 🚀
