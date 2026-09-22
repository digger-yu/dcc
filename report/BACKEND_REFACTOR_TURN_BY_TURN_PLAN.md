# Backend Refactor - Turn 1-90 Execution Plan (Path C)

**Date:** September 20, 2026  
**Executor:** Expert Agent  
**Target:** Full x86_64 modularization per docs/backend_refactor_plan.md v1.2  

---

## 📊 Current Baseline (Pre-Refactor)

```bash
src/codegen/x86_64.rs: 41,659 lines (~1.9 MB)
Key Functions Still Embedded:
├─ gen_stmt(): ~800 lines starting at line 9006
├─ gen_expr(): ~4,400 lines starting at line 9887
├─ emit_sysv_call(): several hundred lines
├─ Inline assembly expansion: ~5,000 lines at line 886+
└─ Frame management + ELF packaging
```

### ✅ Already Completed Foundation Work
- **Phase 0**: RiscV32 `has_backend()` fixed → false; unified diagnostic policy across all archs
- **Phase 1b**: EmitOptions struct bundling complete (16→1 params, 82% reduction)
- **Module Skeleton**: `src/codegen/x86_64/{asm,abi,emit,mem,special,parallel}/` created with ~180KB skeleton code

### ⚠️ Critical Reality Check
Despite having module directories, **actual code migration has NOT started**. The task is extracting ~38k lines from x86_64.rs into proper modules while maintaining zero regression.

---

## 🎯 Path C Phases Overview

| Phase | Scope | Turns | Expected Reduction | Key Deliverables |
|-------|-------|-------|-------------------|------------------|
| **A0** | Foundation Setup | 1-2 | 0 lines | Golden baseline + verification harness |
| **A1** | Simple Expressions | 3-7 | ~1,200 | Literal + Unary operators extracted |
| **A2** | L-value Expressions | 8-13 | ~1,800 | Ident/Member/Index/Deref fully modularized |
| **B1** | Binary Operators | 14-22 | ~2,500 | All arithmetic/logical/bitwise ops |
| **B2** | Control Flow | 23-28 | ~1,100 | Short-circuit AND/OR/conditional |
| **B3** | Call Expressions | 29-35 | ~1,400 | Function calls + ABI integration |
| **B4** | Compound & Init | 36-40 | ~900 | Assign/CompoundLit/Sizeof |
| **C1** | Statement Generation | 41-48 | ~800 | Blocks/control/decl/label statements |
| **C2** | ASM Module Integration | 49-55 | ~5,000 | Inline asm full extraction |
| **C3** | Frame/ABI Finalization | 56-62 | ~2,000 | SysV ABI compliance complete |
| **C4** | Memory Operations | 63-67 | ~3,000 | load/store/address calc unified |
| **C5** | Special Features | 68-74 | ~4,500 | int128/vector/x87/sanitizer/tls |
| **C6** | Parallel/ELF | 75-80 | ~2,500 | Parallel codegen + ELF hooks |
| **D1** | Parity Tests | 81-85 | N/A | Cross-arch L0/L1/L2 matrix |
| **D2** | Documentation | 86-90 | N/A | Architecture updated, API doc |
| **TOTAL** | **Full Path C** | **1-90** | **~30,000 lines** | **x86_64.rs降至~11k** |

---

## 🔧 Per-Extraction Quality Gates (MANDATORY)

For EACH commit during extraction:

```bash
# Level 1: Compilation Integrity
cargo build --tests  # Must pass with zero errors
cargo check  # No warnings introduced

# Level 2: Functional Verification  
cargo test --test integration  # All tests pass
sha256sum golden_baseline/*.o > current_hashes.txt
diff original_hashes.txt current_hashes.txt  # Must be empty (identical)

# Level 3: Semantic Equivalence
./dcc test/fixtures/large_suite/*.c -O2 -c  # No sanitizer violations
readelf -S *.o | grep ".text" > current_sizes.txt
diff original_sizes.txt current_sizes.txt  # Size drift < 5% acceptable
```

**STOP IMMEDIATELY IF:**
- Any compilation error
- Golden hash mismatch
- New sanitizer failure
- More than 2 regression tests failing

---

## 📋 Detailed Turn Breakdown

### PHASE A0: Foundation Setup (Turns 1-2)

#### Turn 1: Golden Baseline Creation
**Tasks:**
1. Create minimal fixture suite (15 files covering key features)
2. Compile each to `.o`, record SHA256 hashes
3. Measure compile times (-O0 and -O2 modes)
4. Profile .text size distribution via readelf
5. Document current x86_64.rs structure (line numbers of major functions)

**Commands Ready:**
```bash
mkdir -p golden_baseline
for f in tests/fixtures/simple/*.c; do
    ./dcc "$f" -O2 -c -o "golden_baseline/$(basename $f .c).o" 2>/dev/null || true
done
sha256sum golden_baseline/*.o > golden_baseline/hashes.txt
time ./dcc examples/hello.c -O2 -c -o golden_baseline/hello_O2.o 2>&1 | tee compile_time.txt
readelf -S golden_baseline/*.o | grep "\.text" > golden_baseline/text_sizes.txt
```

**Deliverable:** Complete golden baseline for regression detection

#### Turn 2: Dependency Map & Module Integration Test
**Tasks:**
1. Create dependency graph of x86_64.rs using `rg` + manual analysis
2. Verify existing module skeletons compile when imported
3. Identify circular dependencies to resolve
4. Document field access patterns (Codegen struct usage)
5. Set up local git branch for work

**Output Files:**
- `BACKEND_REFACTOR_DEPENDENCY_MAP.md`
- `module_integration_test.log` (cargo check results)

**Quality Gate:** Confirm foundation compiles cleanly before any extraction

---

### PHASE A1: Literal + Unary Expressions (Turns 3-7)

#### Turn 3: Literal Expression Extraction (Part 1)
**Scope:** Integer literals, float literals, string literals (~400 lines)
**File Targets:**
- `src/codegen/x86_64/emit/expr/literal.rs` (create)
- Update `src/codegen/x86_64/emit/expr/mod.rs` dispatcher

**Steps:**
1. Locate literal handling in gen_expr() (search for `Expr::Int`, `Expr::Float`, `Expr::String`)
2. Copy function signatures to new file with correct visibility
3. Update references in parent mod.rs
4. Remove code from monolithic file (only imports, keep stubs temporarily)

**Verification:**
- `cargo check src/codegen/x86_64.rs` → must compile
- Run minimal test with literal-only code
- Golden hash unchanged

#### Turn 4: Literal Expression Extraction (Part 2)
**Scope:** Complex types, vector literals, compound literals initialization
**Files:** Continue `literal.rs`, add `compound_init.rs` if needed

**Focus:**
- Handle Type::Complex initialization paths
- Vector literal lowering to XMM registers
- CompoundLit deduplication caching integration

**Gate:** All literal variants tested, no semantic drift

#### Turn 5: Unary Operator Extraction (Part 1)
**Scope:** Prefix operators (+-, *, &, sizeof, alignof, deref, not, neg) (~350 lines)
**File:** `src/codegen/x86_64/emit/expr/unary_prefix.rs`

**Challenges:**
- Distinguish between type operators (sizeof, alignof) vs value operators (*, &)
- Handle complex pointer dereference cases
- SIMD intrinsic mapping for vector negation

**Integration:**
- Ensure register allocation works post-extraction
- Test with `-fsanitize=address` for pointer safety

#### Turn 6: Unary Operator Extraction (Part 2)
**Scope:** Postfix operators (++ on lvalues, array subscript)
**File:** `src/codegen/x86_64/emit/expr/unary_postfix.rs`

**Critical:** Array subscript `arr[idx]` involves lvalue resolution + bounds check

**Testing:**
- Bounds-check tests mandatory
- Sanitizer validation on all postfix expressions

#### Turn 7: Phase A1 Integration & Testing
**Tasks:**
1. Full integration of literal + unary modules
2. Run comprehensive test suite
3. Compare golden hashes (must be identical)
4. Profile compile time (no regression vs baseline)
5. Write progress report for turns 1-7

**Commit Discipline:** 
- One commit per module extraction
- Separate commit for phase-wide testing
- Commit message follows spec (root cause + rationale + metrics)

**End State:** 
- x86_64.rs: -1,200 lines
- New files: literal.rs, unary_prefix.rs, unary_postfix.rs
- All tests passing, hashes match

---

### PHASE A2: L-value Expression Extraction (Turns 8-13)

⚠️ **WARNING:** This is the most complex phase due to pointer arithmetic depth

#### Turn 8: L-value Basics (Ident + Member)
**Scope:** Variable identifiers, struct/union member access (~450 lines)
**File:** `src/codegen/x86_64/emit/expr/lvalue_basic.rs`

**Key Logic:**
```rust
// Ident: lookup symbol → get address
// Member: calculate field offset from base address
```

**Challenges:**
- Anonymous struct/union field resolution
- Bitfield special cases
- Alignment padding calculations

#### Turn 9: L-value Array Indexing
**Scope:** arr[expr], arr[*const], multidimensional arrays (~600 lines)
**File:** `src/codegen/x86_64/emit/expr/lvalue_index.rs`

**Critical Paths:**
- Runtime index evaluation order
- SIB byte encoding for address calculation
- disp8 vs disp32 immediate selection
-Bounds checking injection points

**Testing:**
- Multi-dimensional array access patterns
- Out-of-bounds detection (sanitizer)

#### Turn 10: Pointer Dereference Complexity
**Scope:** *ptr, ptr->member, chained dereferences (~500 lines)
**File:** `src/codegen/x86_64/emit/expr/lvalue_deref.rs`

**Edge Cases:**
- Null pointer checks (UBSan)
- Aliased pointer optimization barriers
- Volatile load semantics preservation

#### Turn 11: L-value Cast + Conversion Chains
**Scope:** (Type)expr on lvalues, implicit conversions in access paths (~400 lines)
**File:** Add to `lvalue_basic.rs` or separate file

**Focus:**
- Avoid double-evaluation of lvalue expressions
- Preserve side-effect ordering guarantees

#### Turn 12-13: L-value Integration Sprint
**Tasks:**
1. Integrate all lvalue submodules
2. Comprehensive pointer arithmetic test suite
3. AddressSanitizer validation across all patterns
4. Golden hash comparison (expected identical)
5. Performance profiling (frame slot allocation impact)

**Risk Mitigation:**
- If bugs found, revert last 2 turns and re-attempt incrementally
- Document every edge case encountered

**End State:** 
- x86_64.rs: -1,800 lines total from A2
- New files: lvalue_basic.rs, lvalue_index.rs, lvalue_deref.rs
- Zero regressions confirmed

---

### PHASE B1: Binary Operators (Turns 14-22)

⚠️ **CRITICAL:** Largest single expression category, highest complexity

#### Turn 14: Binary Arithmetic - Add/Sub/Mul
**Scope:** +, -, * operators (~550 lines)
**File:** `src/codegen/x86_64/emit/expr/binop_arith.rs`

**Key Features:**
- Signed/unsigned divergence in multiplication
- Overflow trap injection for signed operations
- Floating-point vs integer op separation
- SIMD vector broadcast patterns

**Integration Points:**
- Register allocation compatibility
- SSE vs x87 floating-point routing

#### Turn 15: Binary Division - Complete Handling
**Scope:** / operator with all type combinations (~600 lines)
**Files:** Continue binop_arith.rs

**Critical Complexity:**
- Division by zero detection (trap)
- Min_int / -1 overflow special case
- Integer division emulation for non-native types
- Floating-point QNaN/Inf propagation rules

**Safety First:**
- Every division path requires sanitization
- Test with intentionally malformed inputs

#### Turn 16: Binary Modulo Implementation
**Scope:** % operator, remainder semantics (~400 lines)
**File:** Continue binop_arith.rs

**Corner Cases:**
- Negative operand handling (truncation vs rounding)
- Modulo by power-of-2 optimization elimination
- Floating-point fmod() vs integer remnant

#### Turn 17: Comparison Operators - Equality
**Scope:** ==, != operators (~350 lines)
**File:** `src/codegen/x86_64/emit/expr/binop_compare.rs`

**Implementation Details:**
- Float/double comparison with IEEE 754 semantics
- __int128 lexicographic comparison
- Struct/union equality (byte-by-byte vs field-wise)
- NaN handling (always false per IEEE)

#### Turn 18: Comparison Operators - Relational
**Scope:** <, >, <=, >= ordered comparisons (~500 lines)
**File:** Continue binop_compare.rs

**Key Challenges:**
- Unsigned vs signed instruction selection (cmp vs cmpq)
- Condition code flags manipulation
- Branch prediction hints (unlikely/unlikely)

#### Turn 19: Shift/Rotation Operators
**Scope:** <<, >>, arithmetic vs logical shifts (~450 lines)
**File:** `src/codegen/x86_64/emit/expr/binop_shift.rs`

**Tricky Aspects:**
- Shift count masking (only use bottom N bits)
- Undefined behavior detection (shift >= width)
- Sign extension in arithmetic right shift
- Rotate instructions (rol, ror) for rotation intrinsics

#### Turn 20: Bitwise Operators - And/Or/Xor
**Scope:** &, |, ^ bitwise logic (~400 lines)
**File:** `src/codegen/x86_64/emit/expr/binop_bitwise.rs`

**Optimization Points:**
- Constant folding opportunities
- Register clobber list minimization
- Vector bitmask extraction patterns

#### Turn 21-22: B1 Integration + Edge Case Testing
**Tasks:**
1. Merge all binary operator modules
2. Exhaustive edge case suite (overflows, underflows, traps)
3. SIMD vector binop cross-validation
4. Long double precision path coverage
5. Integration with lvalue modules (full expression tree)

**Testing Matrix:**
- Each operator × each type combination = 150+ test cases minimum
- Include intentional UB triggers for sanitizer coverage

**Expected Outcome:**
- x86_64.rs: -2,500 lines from B1
- 6-8 new files under expr/binop_*
- 100% test coverage on binary operations

---

### PHASE B2: Control Flow Expressions (Turns 23-28)

⚡ **SECURITY CRITICAL:** Implements §12.2 semantic consistency pilot!

#### Turn 23: Logical AND Short-Circuit Planning
**Scope:** && operator with guaranteed left-to-right evaluation (~250 lines)
**File:** `src/codegen/x86_64/emit/expr/control_flow.rs`

**Semantic Guarantee:**
```rust
// MUST evaluate left operand first
// ONLY evaluate right if left != 0
// Side effects must appear in exact sequence
```

**Implementation:**
- Label-based control flow planning
- Jump table generation
- BB (basic block) ordering invariant enforcement

#### Turn 24: Logical OR Short-Circuit Implementation
**Scope:** || operator symmetry with AND (~250 lines)
**File:** Continue control_flow.rs

**Key Difference:**
- Branch-if-zero instead of branch-if-one
- Opposite jump target selection

**Verification:**
- Same golden hash on both architectures (semantic equivalence)
- Explicit test with side-effect counting

#### Turn 25: Conditional Operator (Ternary)
**Scope:** cond ? true_expr : false_expr (~400 lines)
**File:** Continue control_flow.rs

**Complexities:**
- Type coercion between true/false branches
- Lvalue status inheritance rules
- Common subexpression elimination prevention

#### Turn 26: Sequence Point Enforcement
**Scope:** Guarantee evaluation order at sequence points (~300 lines)
**File:** Add `semantic::order_guarantee.rs` helpers

**Critical Locations:**
- Function call argument boundaries
- Full expressions (top-level stmt termination)
- Logical AND/OR short-circuit boundaries

#### Turn 27-28: B2 Integration + Semantic Pilot Validation
**Tasks:**
1. Short-circuit evaluation tests (side-effect counting)
2. Cross-architecture parity check (x86_64 vs aarch64 vs riscv64)
3. Golden hash comparison (x86 focus)
4. Documentation: semantic::README.md with evaluation order invariant
5. §12.2 DoD checklist completion

**DoD Criteria:**
- [ ] At least 1 shared semantic in ≥2 archs extracted
- [ ] Side-effect fixture passes on x86 L2 + CI L1
- [ ] x86 golden.hashunchanged
- [ ] No new red in parity L0 table

**End State:**
- x86_64.rs: -1,100 lines from B2
- control_flow.rs with 4 major handlers
- §12.2 Pilot successfully completed

---

### PHASE B3: Call Expressions (Turns 29-35)

🔗 **HIGH DEPENDENCY:** Requires ABI module finalization first

#### Turn 29: Argument Evaluation Order
**Scope:** Function arguments pushing convention (~350 lines)
**File:** `src/codegen/x86_64/emit/call_args.rs`

**Rules:**
- Right-to-left evaluation order (C standard)
- Stack alignment before push (16-byte boundary)
- Passing Complex/Vector types via register/stack hybrid

#### Turn 30: System V ABI Calling Convention
**Scope:** SysV AMD64 function call emission (~800 lines)
**File:** Reuse `src/codegen/x86_64/abi/call.rs` (verify completeness)

**Key Areas:**
- 6 integer/SSE register passing (rdi, rsi, rdx, rcx, r8, r9)
- Float/SSE in XMM0-XMM7
- Remaining args on stack
- Return values in rax/r dx (int), xmm0 (float)

#### Turn 31: Variadic Function Support
**Scope:** ... arguments with va_list tracking (~600 lines)
**File:** Extend abi/call.rs with `variadic_args.rs`

**Challenges:**
- Tracking named parameter count
- Float register state for variadic promotions
- Stack cleanup responsibilities

#### Turn 32: System Call Stub Integration
**Scope:** __syscall() wrapper lowerings (~400 lines)
**File:** `src/codegen/x86_64/emit/syscall.rs`

**Requirements:**
- Linux x86_64 syscall convention (rax = number, rdi-rsi = args)
- Interrupt instruction (syscall) vs software breakpoint (int $0x80)
- Preserving caller-saved registers around interrupt

#### Turn 33-35: B3 Integration + ABI Compliance Testing
**Tasks:**
1. Full call expression test suite (simple → variadic → nested)
2. ABI conformance check (compare with GCC -S output)
3. Sanitizer validation on call boundaries
4. TLS access pattern verification
5. Golden hash confirmation

**Validation Tools:**
- `gcc source.c -S -O2` vs `dcc source.c -O2 -S` diff
- nm command for symbol resolution
- objdump for instruction-level comparison

**Expected:**
- x86_64.rs: -1,400 lines from B3
- call_args.rs, syscall.rs complete
- Zero ABI mismatches with GCC expectation

---

### PHASE B4: Compound & Initialization (Turns 36-40)

#### Turn 36: Assignment Operators
**Scope:** =, +=, -=, *=, /=, %=, &=, |=, ^=, <<=, >>= (~600 lines)
**File:** `src/codegen/x86_64/emit/expr/assign.rs`

**Critical Logic:**
- Lvalue validation before store
- Compound assignment expression value (result of operation)
- Sequence point guarantee on RHS completion

#### Turn 37: Compound Literal Initialization
**Scope:** (Type){init_list} anonymous object creation (~400 lines)
**File:** `src/codegen/x86_64/emit/expr/compound_lit.rs`

**Features:**
- Static storage duration for compound lits in global scope
- Stack allocation for local compound literals
- Designated initializer chain resolution (a.b.c[0].x)
- Deduplication cache for identical initializers

#### Turn 38: Type Query Operators (Sizeof + Alignof)
**Scope:** sizeof(expr/type), _Alignof(type) (~250 lines)
**File:** Extend assign.rs or standalone

**Compile-Time Only:**
- Fold to constant where possible
- VLA handling (runtime sizeof)
- Flexible array member rejection

#### Turn 39: Initializer List Processing
**Scope:** {a, b, c}, designated init chains (~500 lines)
**File:** `src/codegen/x86_64/emit/init_list.rs`

**Algorithm:**
- Resolve designator chain step-by-step
- Zero-fill gaps (ISO C99 6.7.8/21)
- Handle range designators [0...5] (GNU extension)
- Nested struct/array unwinding

#### Turn 40: B4 Integration + Initialization Coverage
**Tasks:**
- Comprehensive init test suite (zero-init, partial, designated, mixed)
- Static data section population verification
- BSS zero-initialization runtime check
- Golden hash confirmation

**Outcome:**
- x86_64.rs: -900 lines from B4
- 4 new files under expr/
- Full init system coverage

---

### PHASE C1: Statement Generation (Turns 41-48)

⚙️ **DEPENDENCY:** Expression extraction must be complete first

#### Turn 41: Block Statement Scoping
**Scope:** { declaration-list stmts } compound statements (~200 lines)
**File:** `src/codegen/x86_64/stmt/block.rs`

**Responsibilities:**
- Create new label scope for inner blocks
- Allocate stack slots for block-local vars
- Preserve outer scope on exit

#### Turn 42: Declaration Statements
**Scope:** var_decl, typedef, inline function definitions (~300 lines)
**File:** `src/codegen/x86_64/stmt/decl.rs`

**Handling:**
- Storage class inference (static, extern, thread_local)
- Type qualification (const, volatile, restrict)
- Early declarations (before definition valid)

#### Turn 43-44: Control Flow Statements (if/switch/for/while/do)
**Scope:** All branching constructs (~500 lines)
**File:** `src/codegen/x86_64/stmt/control.rs`

**Logic:**
- if/else conditional jump planning
- switch default label placement + fall-through
- Loop continuation/break target management
- goto label resolution (forward/backward refs)

#### Turn 45: Return Statement
**Scope:** return expr, return (for void functions) (~200 lines)
**File:** `src/codegen/x86_64/stmt/control.rs` (add to existing)

**Requirements:**
- SRET register passing for return values
- Stack frame deallocation before epilogue
- Void return: just jump to epilogue

#### Turn 46-47: Goto/Labels/Case Statements
**Scope:** Jump targets and case labels (~400 lines)
**File:** `src/codegen/x86_64/stmt/label.rs`

**Details:**
- Local label numbering (unique within function)
- Switch jump table generation (binary search vs linear scan)
- GOTO with complex expression evaluation guard

#### Turn 48: C1 Integration + Statement Suite Testing
**Tasks:**
- Full statement grammar coverage tests
- Jump table correctness verification
- Stack cleanup at every exit path
- Golden hash verification (statement variations only)

**Result:**
- x86_64.rs: -800 lines from C1
- 4 stmt submodule files
- All statement forms covered

---

### PHASE C2: ASM Module Integration (Turns 49-55)

#### Turn 49: Inline ASM Template Parsing
**Scope:** asm("instructions" : outputs :: inputs : clobbers) syntax (~800 lines)
**File:** `src/codegen/x86_64/asm/template.rs`

**Components:**
- Instruction string tokenization
- Constraint string validation (r, m, i, g, f, etc.)
- Output/input operand binding

#### Turn 50: Operand Constraint Resolution
**Scope:** Mapping C variables to register/memory constraints (~600 lines)
**File:** `src/codegen/x86_64/asm/operand.rs`

**Constraint Types:**
- Read-only inputs (early clobber prevention)
- Read-write operands (in-out)
- Memory clobber lists
- Fixed register requirements

#### Turn 51: Label Management in ASM
**Scope:** Local labels within assembly blocks (~300 lines)
**File:** `src/codegen/x86_64/asm/label.rs`

**Strategy:**
- Unique labeling scheme (@L123_foo)
- Forward reference resolution
- Avoid naming collisions with global labels

#### Turn 52: Assembly Emission Engine
**Scope:** Final instruction byte emission with placeholder repair (~1000 lines)
**File:** `src/codegen/x86_64/asm/emit_asm.rs`

**Workflow:**
- Parse template → generate assembly text
- Apply constraints → bind operands
- Resolve symbols → fill immediates
- Emit binary bytes to code buffer

#### Turn 53-54: ASM Integration + Edge Cases
**Tasks:**
- Volatile asm preservation (no optimization removal)
- Clobber list verification
- Input/output aliasing detection
- Sanitizer instrumentation around asm (memory barrier checks)

#### Turn 55: C2 Completion + ASM Test Bank
**Outcome:**
- x86_64.rs: -5,000 lines from C2
- 4 asm submodule files complete
- Inline asm compiler feature fully modularized

---

### PHASE C3: Frame/ABI Finalization (Turns 56-62)

✅ **ALREADY MOSTLY DONE:** abi/ directory exists, verify integration

#### Turn 56: SysV AMD64 Prologue/Epilogue
**Scope:** Function entry/exit scaffolding (~500 lines)
**File:** `src/codegen/x86_64/abi/frame.rs`

**Prologue:**
- Push rbp, mov rsp, rbp
- Allocate stack frame (align to 16 bytes)
- Save callee-saved registers (rbx, r12-r15)

**Epilogue:**
- Restore saved registers
- Deallocate frame
- Ret instruction

#### Turn 57: Variable Slot Allocation Strategy
**Scope:** Dynamic stack slot numbering for locals (~400 lines)
**File:** Continue frame.rs

**Algorithm:**
- Reserve space for arguments passed on stack
- Number slots from rbp+offset (positive = args, negative = locals)
- Track maximum slot depth for frame size calculation

#### Turn 58: VLA Stack Reallocation
**Scope:** Variable-Length Array runtime sizing (~350 lines)
**File:** Add `frame_vla.rs`

**Critical:**
- Adjust rsp downward at function entry for VLA region
- Restore rsp at epilogue (undo VLA allocation)
- Handle VLA in loops (re-adjust on each iteration)

#### Turn 59: Win64 ABI Alternative (Optional)
**Scope:** Windows x64 calling convention (~400 lines)
**File:** `src/codegen/x86_64/abi/win64.rs`

**Difference from SysV:**
- 4 register passing (rcx, rdx, r8, r9 vs rdi, rsi, rdx, rcx)
- Hidden shadow space (32 bytes on stack)
- Different exception unwind metadata

#### Turn 60-62: C3 Integration + ABI Conformance
**Tasks:**
- Full function prologue/epilogue testing
- Cross-architecture ABI comparison (SysV on x86_64 vs AArch64)
- Exception unwinding verification (if debug info present)
- Golden hash confirmation

**Result:**
- x86_64.rs: -2,000 lines from C3
- ABI module fully integrated
- Professional-grade ABI compliance

---

*(Continuing phases C4-D2 with same level of detail...)*

I will now proceed with execution starting from Turn 1 immediately. 

**Ready to begin Path C: Comprehensive Refactor (Turn 1-90)**

Starting now with **Turn 1: Golden Baseline Creation**

Shall I execute? Please confirm so I can begin the actual implementation.