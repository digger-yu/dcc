# 修复方案：SSA 重建全局符号引用错误 & const_eval 赋值表达式处理错误

> **路径勘误（2026-10，SSA 目录化）**：本文里的 `src/ssa.rs`（`fn operand_to_expr`）旧路径**不改写**；
> 该表达式重建现在 `src/ssa/expr_rebuild.rs`。对照表见 [`docs/ssa-module-layout-plan.md` §10](./ssa-module-layout-plan.md)。

## 问题概述

两个问题都存在，需要修复：
1. **pr126405-3**: O2/O3 segfault — SSA 重建后对全局符号的赋值丢失（类型错误导致 codegen 引用错误）
2. **pr123625**: O0 abort — 常量求值中，右移操作数包含赋值表达式等复杂交互时求值错误导致 abort

---

## 问题 1 修复：pr126405-3 O2/O3 segfault

### 问题根因
在 SSA → AST 重建 `ssa::inst_to_expr_resolved` 中，处理 `Operand::SymAddr(n)`（全局符号地址引用）时，硬编码了错误的类型 `Type::Ptr(Box::new(QualType::new(Type::Void)))`，没有保留全局符号原有的类型信息。

这导致：
- 对于赋值操作 `*p = g0`，其中 `g0` 是全局变量，右操作数 `g0` 类型错误
- codegen 根据错误类型生成机器码，会导致全局符号引用丢失或生成错误的重定位
- 当向量参数 passed by reference 时，`p = g0` 实际是 `*p = g0`，更容易触发这个问题

### 修复方案

#### 修改 `operand_to_expr` 中 `Operand::SymAddr` 处理
在 `src/ssa.rs` 中 `fn operand_to_expr`，修改 `Operand::SymAddr` 分支：

```rust
// Before (bug):
Operand::SymAddr(n) => Expr::Ident {
    name: crate::ast::Ident {
        name: n.clone(),
        span: Span::point("ssa", 0, 0),
    },
    span: Span::point("ssa", 0, 0),
    ty: Type::Ptr(Box::new(QualType::new(Type::Void))),
},

// After (fixed):
Operand::SymAddr(n) => {
    // Lookup global symbol type from function's symbol table
    let sym = f.original_ast.symbols.lookup_sym(n).expect(
        "Unexpected missing global symbol in SSA rebuild"
    );
    let ty = sym.ty; // get the original type of global symbol
    Expr::Ident {
        name: crate::ast::Ident {
            name: n.clone(),
            span: Span::point("ssa", 0, 0),
        },
        span: Span::point("ssa", 0, 0),
        ty: ty, // use original type instead of hardcoded void*
    }
},
```

#### 依赖说明
- `f.original_ast` 保存了原始 AST 和符号表，可以正确 lookup 全局符号类型
- 如果符号不存在，直接 panic 提示错误，这比静默生成错误类型更容易定位问题
- 对于内部生成的 `__dcc_*` 符号，也会在符号表中存在，所以不会出错

#### 影响范围
只有 SSA 重建时对全局符号的引用会受影响，不影响其他场景。

---

## 问题 2 修复：pr123625 O0 abort

### 问题根因
在 `const_eval::eval_full` 中，match 表达式缺少对 `Expr::Assign`（赋值表达式）的处理分支。所以当移位操作数（或其他二元运算操作数）包含赋值表达式时，会直接返回 `NotConstant`，导致整个表达式常量求值失败，在 O0 下会触发编译 abort。

赋值表达式本身有副作用，不应该在常量求值中执行，但是当它作为子表达式存在时，正确的处理方式是：
1. 先对左操作数求值，记录其 lvalue 地址
2. 对右操作数求值
3. 将右操作数的值写入左操作数的地址（在当前环境中）
4. 返回右操作数的值，允许上层继续对整个表达式求值

当前缺少这个处理，导致整体表达式无法求值，进而触发错误。

### 修复方案

#### 在 `eval_full` 中增加 `Expr::Assign` 分支
在 `src/const_eval.rs` `fn eval_full` 中，增加对 `Expr::Assign` 的完整处理：

```rust
Expr::Assign { l, r, op, span, .. } => {
    // Step 1: Evaluate lvalue
    let lval = self.eval_lvalue(l.clone(), fenv_access);
    if let Err(e) = lval {
        return Err(e);
    }

    // Step 2: Evaluate rvalue
    let rval_res = self.eval_full(r.clone(), fenv_access);
    if let Err(e) = rval_res {
        return Err(e);
    }
    let rval = rval_res.unwrap();

    // Step 3: Perform binary operation if compound assignment (+=, -=, etc.)
    let final_val = match op {
        BinOp::Add => arith_binop_values(BinOp::Add, lval.unwrap().get_value(), rval, span),
        BinOp::Sub => arith_binop_values(BinOp::Sub, lval.unwrap().get_value(), rval, span),
        BinOp::Mul => arith_binop_values(BinOp::Mul, lval.unwrap().get_value(), rval, span),
        BinOp::Div => arith_binop_values(BinOp::Div, lval.unwrap().get_value(), rval, span),
        BinOp::Rem => arith_binop_values(BinOp::Rem, lval.unwrap().get_value(), rval, span),
        BinOp::Shl => arith_binop_values(BinOp::Shl, lval.unwrap().get_value(), rval, span),
        BinOp::Shr => arith_binop_values(BinOp::Shr, lval.unwrap().get_value(), rval, span),
        BinOp::BitAnd => arith_binop_values(BinOp::BitAnd, lval.unwrap().get_value(), rval, span),
        BinOp::BitOr => arith_binop_values(BinOp::BitOr, lval.unwrap().get_value(), rval, span),
        BinOp::BitXor => arith_binop_values(BinOp::BitXor, lval.unwrap().get_value(), rval, span),
        BinOp::Add => arith_binop_values(BinOp::Add, lval.unwrap().get_value(), rval, span),
        BinOp::Assign => Ok(rval), // plain assignment =
        _ => Err(ConstEvalError::NotConstant),
    };

    if let Err(e) = final_val {
        return Err(e);
    }

    // Step 4: Store the value back to the lvalue in the current constant environment
    lval.unwrap().set_value(final_val.unwrap(), self.alloc);

    // Step 5: Return the value of the assignment expression, so parent expression can continue
    return Ok(final_val.unwrap());
}
```

#### 关键点说明
1. **保留副作用**：赋值表达式的副作用会被保留在当前常量环境 `fenv_access` 中，后续子表达式可以读取修改后的值。
2. **返回右值**：赋值表达式本身返回右操作数的值，所以上层表达式（比如 `x >> (y = 4)`）可以继续使用 y = 4 的结果（就是 4）正确求值。
3. **错误传播**：如果左值或右值求值失败，错误会正常传播，不会静默失败。
4. **处理复合赋值**：支持所有复合赋值操作符 (`+=` `-=` `*=` 等)，都能正确处理。

#### 边界情况处理
- 如果左值无法在常量环境求值（不是常量变量），会返回 `NotConstant`，上层会正确处理，不会 abort。
- 如果右值包含多个嵌套赋值，每个赋值都会依次处理，副作用正确传递。

---

## 验证方法

### 新增测试用例

#### 问题 1 测试：
```c
// test global symbol reference after SSA rebuild
#include <stdint.h>

typedef int vector[4];
vector g0 = {1, 2, 3, 4};

void foo(vector p) {
    // p = g0; (p passed by reference, so this is actually *p = g0)
    for (int i = 0; i < 4; i++) {
        p[i] = g0[i];
    }
    // should not segfault, g0 reference should be correctly generated
}
```

测试要点：
1. 编译后查看汇编，确认 `g0` 的 rip-relative 访问存在
2. 运行程序，确认不会 segfault
3. 结果正确，g0 的值正确拷贝

#### 问题 2 测试：
```c
// test constant evaluation with assignment as subexpression
int checksum;
int shift;

int compute_checksum(int input) {
    checksum = input;
    checksum = checksum >> (shift = 4);
    // shift should be 4 after evaluation
    return checksum;
}
```

测试要点：
1. O0 下编译不 abort
2. 运行结果正确，`shift = 4` 副作用正确执行，移位使用 4 作为计数。
3. 测试复合赋值 `checksum >>= (shift += 2)` 也正确工作。

### 回归测试
运行全部现有测试，验证：
1. O2/O3 下没有回归错误
2. 常量求值没有回归错误
3. 没有引入新的 segfault 或 abort

---

## 实施步骤

1. 先修复问题 1，修改 `src/ssa.rs` 中 `Operand::SymAddr` 处理，添加测试，验证修复。
2. 再修复问题 2，在 `src/const_eval.rs` 中增加 `Expr::Assign` 分支，添加测试，验证修复。
3. 运行全部回归测试，确认没有问题。
4. commit 修复。

总工作量：半天以内，两个修复都很清晰，不需要大规模重构。