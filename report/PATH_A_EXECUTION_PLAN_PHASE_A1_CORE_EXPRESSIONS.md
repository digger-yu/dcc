# Path A Execution Plan - Phase A1 Core Expressions

**Date**: September 20, 2026  
**Turn Range**: 21-28 (8 turns total)  
**Focus**: Complete gen_expr() extraction (~2,400 lines expected)  
**Option Selected**: A2 - Production-Ready Complete  

---

## Executive Summary

This document outlines the detailed execution plan for **Path A: Full Extraction Commitment**, following Option A2 (Production-Ready Complete). The goal is to extract approximately ~2,400 lines of actual implementation code from the monolithic `gen_expr()` function in x86_64.rs into well-organized modular handlers under `src/codegen/x86_64/emit/expr/`.

### Key Objectives

1. **Complete Extraction**: Move ~2,400 lines from x86_64.rs line 9887+ into modular handlers
2. **Quality Maintenance**: Zero compilation errors maintained throughout
3. **Golden Baselines**: Create SHA256 hashes for regression detection
4. **Comprehensive Testing**: Integration tests passing with no regressions
5. **Knowledge Capture**: Document lessons learned during extraction

---

## Detailed Turn Breakdown

### Turn 21: Literals Implementation (~300 lines) ✅ IN PROGRESS
**Files to Modify**:
- `src/codegen/x86_64/emit/expr/literal.rs` - Extract integer/char constant logic

**Extraction Source**: x86_64.rs lines 9913-9925 (~12 lines) + related helper functions (~50-100 lines for mask_imm_to_type, float/string handling)

**Expected Output**: 
- Integer/char constant evaluation fully functional through modular handler
- Type-aware masking/sign-extension implemented correctly
- Floating point/string placeholders documented

### Turn 22: L-value Address Calculation (~500 lines)
**Files to Modify**:
- `src/codegen/x86_64/emit/expr/lvalue.rs` - Extract identifier/member/index handling

**Extraction Source**: x86_64.rs lines 9924-10100 (~176 lines) + address calculation helpers (~100-150 lines)

**Expected Output**: All l-value addressing working through modular handlers

### Turn 23: Unary Operators Implementation (~200 lines)
**Files to Modify**:
- `src/codegen/x86_64/emit/expr/unary.rs` - Extract negate/deref operations

**Extraction Source**: x86_64.rs relevant unary operator branches (~100 lines) + instruction emission helpers (~100 lines)

**Expected Output**: Unary operators (-,!~,*,&) fully functional

### Turns 24-25: Binary Operators (~1,000 lines)
**Files to Modify**:
- `src/codegen/x86_64/emit/expr/binary.rs` - Complete binop implementations

**Extraction Source**: x86_64.rs lines 10077-10800 (~723 lines) + short-circuit evaluation logic (~150 lines) + floating point conversions (~100 lines)

**Expected Output**: Complete arithmetic/logic operator set with short-circuit evaluation

### Turns 26-27: Integration & Refinement (~300 lines)
**Activities**:
- Run full test suite ensuring zero regressions
- Create golden hash baselines
- Optimize instruction sequences
- Final documentation updates

**Expected Output**: Confirmed working system maintaining perfect quality record

### Turn 28: Documentation & Handoff
**Activities**:
- Comprehensive achievement report
- Lessons learned summary
- Next steps recommendation
- Team enablement materials

---

## Quality Gates Maintained

Throughout all 8 turns, maintain these non-negotiable gates:

```bash
# Before starting new file:
cargo check --package dcc              # Must pass with zero errors

# After each modification:
cargo build --tests                    # Verify no new warnings

# Before integration:
cargo test --test integration          # Ensure no regressions
git diff --stat                        # Confirm alignment with goals
```

---

## Success Criteria for Phase A1

By end of Turn 28, achieve:
- [ ] ~2,400 lines extracted and integrated successfully
- [ ] Zero compilation errors maintained throughout entire period
- [ ] Multiple expression type families fully functional
- [ ] Integration verification completed with golden baselines
- [ ] Comprehensive handoff documentation created
- [ ] Knowledge capture enabling team continuation

---

## Risk Mitigation Strategies

| Risk | Probability | Impact | Mitigation Strategy |
|------|-------------|--------|-------------------|
| Unexpected complexity during extraction | Medium | High | Maintain quality gates; adjust scope if needed |
| Time pressure leading to rushed work | Medium-High | High | Refuse to compromise standards regardless of schedule |
| Breaking existing functionality | Low | High | Golden hash baselines; incremental verification |

---

## Resource Requirements

**Current Assets**:
- ✅ Modular structure established (31 files, ~3,953 lines production + framework)
- ✅ Expression framework ready (9 files, ~884 lines structure with partial implementations)
- ✅ Methodology validated (framework-first approach proven effective)
- ✅ Quality standards proven (zero errors maintained for 20 turns)

**Additional Resources Needed**:
- ⏳ Time allocation for focused extraction work (8 turns dedicated)
- ⏳ Verification infrastructure (cargo test access maintained)

All required assets currently available ✅

---

## Execution Principles

1. **Quality Over Speed**: Never compromise on cargo check/test gates regardless of schedule pressure
2. **Incremental Verification**: Test after every single file creation
3. **Documentation Discipline**: Document every extraction decision and lesson learned
4. **Golden Baselines**: Create hashes early for regression detection
5. **Knowledge Transfer**: Enable team continuation through comprehensive docs

---

*Document serves as detailed execution guide for Phase A1 Core Expressions.*  
*Based on Principle: Thorough solutions over simplified alternatives*  
*Guiding Standard: docs/backend_refactor_plan.md v1.2 requirements*  
*Status Date: September 20, 2026 (Phase A1 Start)*
