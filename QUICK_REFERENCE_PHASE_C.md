# DCC Phase C Extension - Quick Reference Guide

## 🚀 Getting Started

### 1. Validate Instruction Specifications

```bash
cd /root/dcc/scripts/codegen-gen
cargo run -- \
  --input-dir ../../src/codegen/data \
  --validate-only
```

### 2. Generate Code for Specific ISA

```bash
cargo run -- \
  --input-dir ../../src/codegen/data/x86_instructions \
  --output-dir ../../src/codegen/generated \
  --isa x86_64
```

### 3. Add New Instruction

1. Create JSON file in `src/codegen/data/{arch}_instructions/`
2. Follow schema: `docs/instruction_spec_schema.json`
3. Run codegen-gen to generate Rust code
4. Implement actual encoding logic (not just stubs)

---

## 📁 File Locations

| Component | Path | Purpose |
|-----------|------|---------|
| **JSON Schema** | `docs/instruction_spec_schema.json` | Spec format definition |
| **Codegen Tool** | `scripts/codegen-gen/` | Auto-generation utility |
| **x86_64 Specs** | `src/codegen/data/x86_instructions/*.json` | Current instruction definitions |
| **Generated Code** | `src/codegen/generated/x86_instructions.rs` | Output from codegen-gen |
| **Building System** | `src/codegen/build.rs` | Integration with Cargo |
| **Main Guide** | `docs/adding_new_isa.md` | Complete implementation tutorial |

---

## 📦 Current Status

### ✅ Completed (Foundation Layer)
- [x] JSON specification format
- [x] Code generator tool
- [x] x86_64 instruction specs (6 instructions)
- [x] Pre-generated output templates
- [x] "Adding New ISA" comprehensive guide

### ⏳ Pending (Expansion Work)
- [ ] Backend refactoring (use generated code)
- [ ] AArch64 instruction specs
- [ ] RISC-V instruction specs  
- [ ] Full test suite integration

---

## 💡 Key Principles

1. **Specify, Don't Hardcode**: Use JSON for all instruction definitions
2. **Generate, Don't Repeat**: Let codegen-create boilerplate
3. **Document Everything**: Semantics, flags, side effects
4. **Validate Early**: Schema checks catch errors before build

---

## 🎯 Next Steps

1. Implement byte emission for current instructions
2. Add MOV and CMP instructions (critical for basic compilation)
3. Update x86_64 backend to use generated encoders
4. Begin AArch64 spec creation

---

## 🔗 References

- **Full Progress Report**: `PHASE_C_EXTENSION_PROGRESS.md`
- **Final Completion Report**: `PHASE_C_EXTENSION_FINAL_COMPLETE.md`
- **Implementation Guide**: `docs/adding_new_isa.md`

---

*Quick reference compiled September 22, 2026*
