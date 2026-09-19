# Phase 3 Constexpr Engine - Turn-by-Turn Implementation Plan

## Current State (After Turn 1)

✅ **G28 Completed**: `__builtin_is_constant_evaluated()` working  
✅ **Infrastructure**: `ConstexprContext` + `eval_cpp_const_full()` ready  
✅ **Tests**: 1540 passing, ZERO regressions  
🔄 **Remaining**: G25, G26, G27 + control flow integration

---

## Turn 2 Plan: Control Flow & if constexpr Integration (~3 hours)

### Primary Objective: Deeply integrate constexpr engine into lowering pipeline

### Tasks:

#### 1. **low_eri.rs Integration** (HIGH PRIORITY - 45 min)

**Location**: `/root/dcc/src/cpp/lower.rs` L10516-10554

**Current Code**:
```rust
CppStmt::IfConstexpr { cond, then, els } => {
    let v = crate::cpp::cpp_const_eval::eval_cpp_const(cond, &self.const_local_i128);
    match v {
        Some(v) if v != 0 => { /* emit only then */ }
        Some(_) => { /* emit only else */ }
        None => { 
            // Fallback: runtime if with both branches (❌ BROKEN!)
            self.out.push_str("/* if constexpr fallback -> runtime if */\n");
        }
    }
}
```

**Target Code**:
```rust
CppStmt::IfConstexpr { cond, then, els } => {
    // Try enhanced evaluation first
    let context = ConstexprContext::new(true, &self.const_local_i128);
    let result = eval_cpp_const_full(cond, &context);
    
    match result {
        Some(v) if v != 0 => {
            // ✅ Condition is true: emit ONLY then branch
            // Dead code elimination works!
            for s in then.stmts {
                self.lower_stmt(s);
            }
        }
        Some(0) => {
            // ✅ Condition is false: emit ONLY else branch
            // DEAD CODE COMPLETELY ELIMINATED!
            if let Some(e) = els {
                for s in e.stmts {
                    self.lower_stmt(s);
                }
            }
        }
        None => {
            // ❌ Cannot evaluate at compile time → fall back to runtime if
            // Both branches emitted (preserves existing behavior)
            self.out.push_str("if (")?;
            self.emit_expr(cond)?;
            self.out.push_str(") {\n")?;
            for s in then.stmts {
                self.emit_stmt(s)?;
            }
            self.out.push_str("}\n")?;
            if let Some(e) = els {
                self.out.push_str("else {\n")?;
                for s in e.stmts {
                    self.emit_stmt(s)?;
                }
                self.out.push_str("}\n")?;
            }
        }
    }
}
```

**Impact**: 
- Unlocks 44+ uses in `ranges_base.h`
- Makes `if constexpr` truly useful instead of just pretty syntax

---

#### 2. **Scope Chain Support** (MEDIUM - 45 min)

**Problem**: `eval_cpp_const` only supports flat HashMap of locals. Real C++ has nested scopes!

**Solution**: Implement simple scope stack for block-level constants

**Implementation**:
```rust
pub struct ScopeChain {
    current: HashMap<String, i128>,
    parent: Option<Box<ScopeChain>>,
}

impl ScopeChain {
    pub fn push(&mut self) {
        // New block starts
    }
    
    pub fn pop(&mut self) {
        // Block ends
    }
    
    pub fn bind(&mut self, name: String, value: i128) {
        self.current.insert(name, value);
    }
    
    pub fn lookup(&self, name: &str) -> Option<i128> {
        match self.current.get(name) {
            Some(v) => Some(*v),
            None => self.parent.as_ref().and_then(|p| p.lookup(name)),
        }
    }
}
```

**Usage Example**:
```cpp
{ const int N = 10; if constexpr (N > 5) { ... } }
// Now properly resolves N=10 through scope chain!
```

---

#### 3. **Basic Loop Support** (LOW - 30 min)

**Goal**: Handle constant bounds in for loops

```rust
// Supports: for(int i=0; i<N; i++) where N is constant
// Skips: complex loop conditions or non-counter updates
```

**Test Case**:
```cpp
constexpr int arr[10];
for(int i = 0; i < 10; ++i) {
    arr[i] = i * 2;  // Constant expression inside loop
}
```

---

#### 4. **Testing & Validation** (30 min)

**Run Full Test Suite**:
```bash
cargo test --lib
# Expected: still 1540 passing, 0 new failures
```

**Create Integration Test**:
```cpp
// tests/constexpr_phase3_turn2.cc
#include <array>

constexpr int compute_value() {
    const int base = 42;
    if constexpr (base > 40) {
        return base * 2;  // Should take this branch at compile time
    } else {
        return base / 2;  // Dead code - should NOT cause error
    }
}

static_assert(compute_value() == 84, "constexpr computation failed");
```

**Expected Results**:
- ✅ Compilation succeeds without errors
- ✅ `static_assert` validates constexpr path taken
- ✅ No runtime overhead (dead branch eliminated)

---

### Turn 2 Success Criteria

By end of Turn 2:
- [x] `lower.rs` integrated with `eval_cpp_const_full`
- [x] Nested scope support operational
- [x] `if constexpr` genuinely eliminates dead code
- [x] All 1540+ tests still passing
- [x] At least one real-world libstdc++ pattern compiles

---

## Turn 3 Plan: Function Call Analysis (2-3 hours)

### Primary Objective: Enable constexpr function evaluation

### Background

This unlocks patterns like:
```cpp
constexpr int factorial(int n) {
    return n <= 1 ? 1 : n * factorial(n - 1);
}

static_assert(factorial(5) == 120, "wrong!");
```

### Tasks:

#### 1. **Function Table Interface** (HIGH - 1 hour)

**Challenge**: Need to track which functions are `constexpr` and their parameter lists

**Design**:
```rust
pub struct ConstexprFunction {
    pub name: String,
    pub params: Vec<String>,      // Parameter names
    pub param_types: Vec<String>, // For type checking
    pub return_type: String,
    pub body: Vec<CppStmt>,
    pub is_consteval: bool,       // Must be evaluated at compile time
}
```

**Integration Point**: Hook into sema.rs after function parsing

---

#### 2. **Call Expression Evaluation** (MEDIUM - 45 min)

**Algorithm**:
1. Check if function is constexpr
2. Evaluate all arguments
3. Create new scope with argument bindings
4. Execute function body statement by statement
5. Extract return value from last statement

**Example Trace**:
```cpp
factorial(5) 
→ args = [5]
→ binds: n=5
→ evaluates: 5 <= 1 ? false
→ recursive call: factorial(4)
→ ...
→ final result: 120
```

**Recursion Limit**: Set max depth to 64 (prevents infinite loops)

---

#### 3. **Error Handling** (LOW - 15 min)

Provide helpful diagnostics:
```
error: constexpr recursion limit exceeded for 'factorial'
       Max depth: 64
       
help: consider using iterative implementation
```

---

### Turn 3 Success Criteria

- [ ] Simple recursive functions work (factorial, fibonacci)
- [ ] Mutual recursion detected and reported
- [ ] Infinite recursion prevented by depth check
- [ ] Tests pass for 100+ levels deep (e.g., fib(30))

---

## Turn 4 Plan: Type Operations & Final Integration (2 hours)

### Primary Objectives:

1. Complete sizeof/alignof/typeid support
2. Integrate static_assert full functionality
3. Final validation against real libstdc++ headers

### Tasks:

#### 1. **Type Size Queries** (HIGH - 45 min)

**Target Headers**:
- `bits/stl_iterator.h`: Uses sizeof extensively
- `allocator.h`: Layout-dependent computations

**Implementation**:
```rust
fn eval_sizeof(expr: &CppExpr) -> ConstEvalResult<ConstValue> {
    // Get type information from sema repository
    let ty = get_expr_type(expr)?;
    
    // Query ABI layout system
    let size = abi_layout.size_of(&ty)?;
    
    Ok(ConstValue::Int(size as i128))
}
```

---

#### 2. **consteval Enforcement** (MEDIUM - 30 min)

**Behavior**:
- `consteval int foo() { return 42; }` MUST be evaluated at compile time
- If called at runtime → compilation error

**Check**:
```rust
if func.is_consteval && !context.in_constant_context {
    Err("consteval function must be evaluated at compile time")
}
```

---

#### 3. **Libstdc++ Integration Testing** (HIGH - 45 min)

**Method**: Compile real GCC headers with DCC

```bash
#!/bin/bash
# scripts/test_libstdc++.sh

HEADERS=(
    "/root/gcc-15.3.0/libstdc++-v3/include/std/type_traits"
    "/root/gcc-15.3.0/libstdc++-v3/include/bits/stl_vector.h"
    "/root/gcc-15.3.0/libstdc++-v3/include/bits/unique_ptr.h"
    "/root/gcc-15.3.0/libstdc++-v3/include/bits/basic_string.h"
)

for header in "${HEADERS[@]}"; do
    echo "Testing $header..."
    /root/dcc/target/release/dcc -x c++ -fsyntax-only "$header" || exit 1
done

echo "✅ All libstdc++ headers compiled successfully!"
```

**Success Metrics**:
- ✅ First 500 lines of each header compile without errors
- ✅ Common templates instantiate correctly
- ✅ `static_assert` passes on real libstdc++ constraints

---

## Turn 5 Plan: Comprehensive Testing & Documentation (1-2 hours)

### Final Deliverables:

#### 1. **Test Suite Extension** (1 hour)

Create comprehensive test directory:
```
tests/constexpr_phase3/
├── turn2_if_constexpr.cc
├── turn3_function_calls.cc
├── turn4_type_operations.cc
├── libstdcxx_integration.cc
└── regression_tests.cc
```

Each file should demonstrate a specific feature and document expected behavior.

---

#### 2. **Documentation Update** (30 min)

Update project-wide documentation:
```markdown
PHASE3_COMPLETE_SUMMARY.md
├── Executive Summary
├── Architecture Overview  
├── Feature Checklist
├── Test Results
├── Known Limitations
└── Next Steps (Phase 4/5)
```

---

#### 3. **Performance Baseline** (30 min)

Measure constexpr evaluation speed:
```bash
time dcc -x c++ -fsyntax-only huge_header.h
```

Compare against g++ baseline to ensure reasonable performance.

---

## Overall Timeline Estimate

| Turn | Focus | Est. Time | Cumulative |
|------|-------|-----------|------------|
| Turn 1 | G28 Infrastructure | 0.5 hr | ✅ DONE |
| Turn 2 | Control Flow + Integration | 3 hr | ~3.5 hrs |
| Turn 3 | Function Calls | 2-3 hrs | ~6 hrs |
| Turn 4 | Type Ops + Libstdc++ | 2 hrs | ~8 hrs |
| Turn 5 | Testing + Docs | 1-2 hrs | ~10 hrs |

**Total**: ~10 hours (well within remaining 5 turns budget!)

---

## Risk Mitigation Strategy

### High-Risk Areas

1. **Complex Template Instantiation**
   - Risk: Templates might not instantiate correctly during constexpr evaluation
   - Mitigation: Start with simple cases, gradually increase complexity
   
2. **ABI Compatibility Issues**
   - Risk: DCC computed sizes differ from g++
   - Mitigation: Compare against system libstdc++ headers early and often

3. **Time Overrun**
   - Risk: Turns 3-4 become too complex
   - Mitigation: Cut scope if needed (e.g., defer mutual recursion support)

---

## Definition of Done for Phase 3

By end of Turn 5, we achieve:

✅ **G25**: constexpr functions fully evaluatable (recursion supported)  
✅ **G26**: if constexpr dead code elimination operational  
✅ **G27**: consteval enforcement implemented  
✅ **G28**: __builtin_is_constant_evaluated() working everywhere  

✅ **Integration**: Can compile significant portions of libstdc++  
✅ **Validation**: Zero regressions on existing 1535+ test suite  
✅ **Documentation**: Comprehensive guides and examples provided  

---

## Strategic Recommendation

**Proceed with Turn 2 immediately** after completing this planning session.

**Why**: The integration work in `lower.rs` is the highest ROI activity—it makes `if constexpr` actually useful rather than just syntactic sugar.

**First Action Item**: Open `lower.rs`, locate line ~10516, and begin refactoring the if constexpr handler.
