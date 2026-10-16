// src/ssa/expr_rebuild.rs - SSA→AST 的表达式重建与类型推导
//
// 放什么: 单条指令→表达式的翻译 (`inst_to_expr{,_resolved}`/`operand_to_expr`/
//   `resolve_operand`/`ssa_ident_expr`)、使用计数 (`collect_op_uses{,_op}`/`op_count_in_*`)
//   与类型推导判据。
// 不放什么: 控制流重建 (`rebuild_block`/`rebuild_bb`/`rebuild_cycle` 等在 `rebuild.rs`)。
// 类型推导判据 (`infer_binop_type`/`store_pointee_ty`/`addr_of_ty`/`unary_result_ty`/
//   `int_const_ty`) 在本文件唯一, 不要在 `rebuild.rs` 复制一份。

use std::collections::{HashMap, HashSet};

use crate::ast::{BinOp, Expr, QualType, Type, UnOp};
use crate::diagnostics::Span;

use crate::ssa::ir::{Function, InstKind, Operand, SsaVar};

/// SSA 变量 → AST `Ident` 的唯一出口。重建的多条路径共用一份构造，避免各自漂移。
///
/// 注意：这里**不能**给数组类型的临时值做「衰减成指针」的改写。试过，错的：同一个
/// 「数组类型的 SSA 值」在本 IR 里有两种存储语义（子对象的别名，与住在自己槽里的
/// 聚合），按类型一刀切会让 `may_alias.c` 在 -O2 段错误。正确的修在 GVN 侧（
/// `val_key` 不给数组类型结果编号），见那里的根因说明。
fn ssa_ident_expr(name: &str, v: &SsaVar) -> Expr {
    Expr::Ident {
        name: crate::ast::Ident {
            name: name.to_string(),
            span: v.span.clone(),
        },
        span: v.span.clone(),
        ty: v.ty.clone(),
    }
}

/// 解析操作数: 先查 subst 内联映射, 再回退到基础名。
pub(super) fn resolve_operand(
    o: &Operand,
    env: &HashMap<String, SsaVar>,
    subst: &HashMap<String, Option<Expr>>,
    func: &Function,
) -> Expr {
    match o {
        Operand::Var(v) => {
            let ssa_name = v.name();
            if let Some(Some(expr)) = subst.get(&ssa_name) {
                return expr.clone();
            }
            // 非内联: 用该版本在 AST 里的局部名（D3/Phase 3.4：同基名的版本被分了
            // 不同的家时，它们不再塌缩成同一个变量）。没分裂时就是基名。
            if let Some(found) = env.get(&ssa_name) {
                ssa_ident_expr(&func.local_splits.ast_name(found), v)
            } else {
                ssa_ident_expr(&func.local_splits.ast_name(v), v)
            }
        },
        _ => operand_to_expr(o, env, func),
    }
}

/// 整数常量的 AST 类型: 与 C++ 字面量类型推导共用 [`crate::cpp::const_text::int_literal_rank_bits`]
/// 这一份真源 (秩 32 → `Int`, 秩 64 → 64 位整型)。
pub(super) fn int_const_ty(v: i128) -> Type {
    match crate::cpp::const_text::int_literal_rank_bits(v) {
        32 => Type::Int,
        _ => Type::LongLong,
    }
}

/// SSA 重建时修正基址表达式的类型。
/// 当 SSA 变量的类型与原始结构体类型不一致时 (如变量被简化为 int),
/// 用原始类型覆盖, 确保后续 IR lowering 能正确解析成员访问。
fn set_expr_ty_if_compatible(expr: &mut Expr, desired_ty: &Type) {
    // 只对 Ident / Deref 等叶子节点修正类型; 复杂表达式保留其计算类型
    match expr {
        Expr::Ident { ty, .. } => {
            *ty = desired_ty.clone();
        },
        Expr::Deref { ty, .. } => {
            *ty = desired_ty.clone();
        },
        _ => {},
    }
}

/// 从两个操作数推导 BinOp 的结果类型
pub(super) fn infer_binop_type(op: &BinOp, l: &Expr, r: &Expr) -> Type {
    let lt = l.ty();
    let rt = r.ty();
    // 指针算术: `ptr ± int` 的结果仍是该指针类型 (C11 6.5.6)。
    // 必须在这里保留, 否则下游 x86_64 codegen 会按 `Type::Int`(4 字节)
    // 对加法结果做 movslq 截断, 把 64 位地址的高 32 位丢掉
    // (栈地址 0x7ffd_XXXX_XXXX 被截成 0xFFFFFFFF_xxxx, 直接段错误)。
    if matches!(op, BinOp::Add | BinOp::Sub) {
        // `Type::Func` 也算指针类: 函数名作右值时会衰减成函数指针
        // (C11 6.3.2.1/4)。遗这里不得把结果类型回退成 `Int`: SSA 重建里「把值拷进
        // 变量」的 copy 形态就是 `fn + 0`, 不保留指针就得到 32 位截断的地址
        // (实测 -O2 下 `int (*fn)(...) = printf;` 存进 0xffffffffd8e60100,
        // 而真实值是 0x7f... —— 动态库函数地址全部错高位)。
        let is_ptr = |t: &Type| matches!(t, Type::Ptr(_) | Type::Array(_, _) | Type::Func(..));
        let decay = |t: &Type| -> Type {
            match t {
                Type::Func(..) => Type::Ptr(Box::new(QualType::new(t.clone()))),
                other => other.clone(),
            }
        };
        let is_int_like = |t: &Type| t.is_int();
        if is_ptr(&lt) && is_int_like(&rt) {
            return decay(&lt);
        }
        if is_ptr(&rt) && is_int_like(&lt) {
            return decay(&rt);
        }
    }
    // 向量操作: 结果类型与向量操作数相同 (比较也返回向量, 全 1/0 每元素)
    if matches!(&lt, Type::Vector(_, _)) {
        return lt.clone();
    }
    if matches!(&rt, Type::Vector(_, _)) {
        return rt.clone();
    }
    if matches!(
        op,
        BinOp::EqEq
            | BinOp::Ne
            | BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge
            | BinOp::AndAnd
            | BinOp::OrOr
    ) && !matches!(&lt, Type::Vector(_, _))
        && !matches!(&rt, Type::Vector(_, _))
    {
        // [expr.rel] / [expr.eq] / C11 6.5.13: **关系、相等、逻辑运算符的结果类型是
        // `int`**, 与操作数是不是浮点无关。必须排在浮点分支之前:
        // C 前端 (`src/parser.rs::parse_binary`) 就是先判关系再算算术类型, 后端
        // 则从**两个操作数自己的类型**选出 `ucomisd` / x87 比较。重建时若把
        // 比较节点标成浮点, 消费端 (赋值 / 存栈槽) 会认为需要一次浮点→整数转换,
        // 于是 `int r = (d < 0.4);` 在 `ucomisd/setb/movzbq` 之后又多一条
        // `cvttsd2si %xmm0,%eax`, 把已算好的 0/1 用残留的浮操作数盖掉
        // (实测 dcc r=0 / g++ r=1; `d < 0.4` 直接作变量用时得到 2)。
        // 向量比较是 GNU 扩展的例外: 结果逐元素为全 1/全 0, 类型同操作数。
        return Type::Int;
    }
    // 复数运算：结果类型是复数，其内部类型取两侧实部的通常算术类型 (C11 6.2.5p13
    // 与 6.3.1.8：`_Complex double` 与 `double` 运算得 `_Complex double`)。必须排在
    // 浮点分支之前、比较分支之后：复数的关系/相等运算结果仍是 int（上面已返回），
    // 而复数算术若不在此定型，就会掉到最后的 Int 回退 —— codegen 靠操作数与结果
    // 类型选择 `gen_complex_binop`，类型一变复数加减就退化成标量浮点/整数运算。
    // 实测：`double _Complex add_dc(x,y){ return x + y; }` 在 -O2 得到 (1.0e9, 0)，
    // 而 gcc 与 -O0 都是 (6, 8)。
    let is_ca = matches!(&lt, Type::Complex(_));
    let is_cb = matches!(&rt, Type::Complex(_));
    let is_ia = matches!(&lt, Type::Imaginary(_));
    let is_ib = matches!(&rt, Type::Imaginary(_));
    if is_ca || is_cb || is_ia || is_ib {
        // 复数 / 虚数二元运算的结果 shape 必须与 parser.rs 的 binop 类型推导
        // 保持一致: imag ± imag → imag; imag * imag → real (取负); imag / imag
        // → real; real ± imag → complex; real * imag / real / imag → imag;
        // 所有 complex_* 组合 → complex.
        // 旧实现把 _Imaginary 与 _Complex 混为一谈一律返回 Complex(inner),
        // 导致 O2 SSA→AST 重建后 `_Imaginary long double + _Imaginary long double`
        // 被标注为 Complex(LongDouble), 消费端 `emit_operand_to_st0` 认为它
        // 不在 st(0) 通道, 转而 emit_int_to_st0 → `fild qword [rsp]` 把
        // faddp 算好的 st(0) 覆盖成 rax 里的垃圾 (REX 卫生门禁抓的就是它)。
        let real_part = |t: &Type| -> Type {
            match t {
                Type::Complex(q) | Type::Imaginary(q) => q.base.clone(),
                other => other.clone(),
            }
        };
        let (li, ri) = (real_part(&lt), real_part(&rt));
        let rank = |t: &Type| -> u8 {
            match t {
                Type::LongDouble => 3,
                Type::Double => 2,
                Type::Float => 1,
                _ => 0,
            }
        };
        let base = if rank(&li) >= rank(&ri) { li } else { ri };
        if is_ca || is_cb {
            // 任一侧复数: 结果恒复数 (C11 6.3.1.8p4)。
            return Type::Complex(Box::new(QualType::new(base)));
        }
        // 两侧皆为虚数 / 一边虚一边实。
        return match op {
            BinOp::Add | BinOp::Sub if is_ia && is_ib => {
                // imag ± imag → imag
                Type::Imaginary(Box::new(QualType::new(base)))
            },
            BinOp::Mul | BinOp::Div if is_ia && is_ib => {
                // imag * imag → real (负); imag / imag → real
                base
            },
            BinOp::Add | BinOp::Sub => {
                // real ± imag → complex
                Type::Complex(Box::new(QualType::new(base)))
            },
            BinOp::Mul | BinOp::Div => {
                // real * imag → imag; real / imag → imag
                Type::Imaginary(Box::new(QualType::new(base)))
            },
            _ => base,
        };
    }
    if matches!(&lt, Type::Float | Type::Double | Type::LongDouble)
        || matches!(&rt, Type::Float | Type::Double | Type::LongDouble)
    {
        if matches!(&lt, Type::LongDouble) || matches!(&rt, Type::LongDouble) {
            Type::LongDouble
        } else if matches!(&lt, Type::Double) || matches!(&rt, Type::Double) {
            Type::Double
        } else if matches!(&lt, Type::Float) || matches!(&rt, Type::Float) {
            Type::Float
        } else {
            Type::Double
        }
    } else if lt.is_int() && rt.is_int() {
        // 整数运算。旧实现在这里无条件返回 `Type::Int`, 把 64 位整数运算
        // 降格成 32 位记账: SSA→AST 重建后, 把该结果存回 `long` 对象时插入的
        // `Cast{long}` 被当作 sext(i32→i64) 而非原值搬运, 高 32 位静默截掉
        // (`long fp = 103060990L; fp = fp*100 - 10;` 得 1716164398)。
        // C++ 前端整条链都走这条重建路径, 故同一段代码 .c 正常、.cpp 错。
        // 现在与 C 前端共用 [`Type::usual_arith_type`] 单一真源。
        if matches!(
            op,
            BinOp::EqEq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
                | BinOp::AndAnd
                | BinOp::OrOr
        ) {
            // C11 6.5.13/6.5.14: 关系与逻辑运算符结果为 `int`。
            Type::Int
        } else if matches!(op, BinOp::Shl | BinOp::Shr) {
            // C11 6.5.7: 移位结果类型 = 左操作数整数提升后的类型
            // (右操作数仅是移位计数, 不参与通常算术转换)。
            lt.int_promotion()
        } else {
            lt.usual_arith_type(&rt)
        }
    } else {
        // 指针差、`_Complex`/`_Imaginary`、枚举等非纯整数情形沿用旧口径
        // (调用方或下游 codegen 自己处理结果表示)。
        Type::Int
    }
}

/// `InstKind::Store` 重建 LHS `Expr::Deref` 时的 **pointee 类型** (store 宽度真源)。
///
/// SSA 的 `Store` 不携带「被写对象类型」, 旧实现一律重建为
/// `Expr::Deref { ty: Type::Void }`; 后端按 `lvalue.ty().size()` 选 store 指令,
/// `Void` 回退 8 字节 ⇒ 恒发 `mov %rax,(%rcx)`。对窄对象就是越界写: 实测 C++
/// 析构里 `int` 全局 `g_b = g_b + 1` 会把紧邻的 `g_d` 高 4 字节清零
/// (Itanium 对象模型下 `delete p` 的基类析构计数被抹掉)。
///
/// 宽度只能从**地址操作数自身的类型**还原: `lower_ir` 侧所有 `Store` 的地址
/// 都是 `Ptr(对象类型)` —— 静态存储 (`AddrOf`/`lower_sym_addr`)、引用槽、
/// `*p = v`、vptr 槽。
///
/// 聚合 pointee 必须**原样带出类型**, 不能压成 `Void`: 后端的整体拷贝分支
/// (`emit/expr/dispatch.rs` 的 `value_is_whole_copy` + `gen_aggregate_assign_to`)
/// 正是靠 LHS 的类型认出「这笔写装不进一个寄存器」。压成 `Void` 后它只看见一次
/// 恒 8 字节的标量 store, 于是 `*pItem = zeroItem` 这类**聚合整体赋值**只搬走
/// 第一字 —— 实测 -O2 下 sqlite 的 `sqlite3ExprListAppend` 因此留下未初始化
/// `ExprList.a[i].zEName`, 名字解析读它而段错误。尺寸 ≤8 的聚合与非聚合走同
/// 一条标量路径, 宽度由 `size()` 给, 与 -O0 的 AST 通道一致。
fn store_pointee_ty(addr: &Expr) -> Type {
    match addr.ty() {
        Type::Ptr(inner) => inner.base.clone(),
        _ => Type::Void,
    }
}

/// `InstKind::AddrOf` 重建 `&base` 的指针类型。
///
/// 旧实现恒写 `Ptr(void)`, 丢掉了「`base` 到底是什么类型」这一件信息:
/// 下游 `*(&sym) = v` (静态存储 / volatile 局部 / 引用槽的回写全部长这样)
/// 只能看到一个 `Ptr(void)` 地址, store 宽度因此回退到 8 字节 —— 对 `int`
/// 对象就是越界写邻位。指真类型就在指令自己的 `dest` 上 (`lower_ir` /
/// `build_from_func` 都以 `Ptr(对象类型)` 声明), 原样带回去即可。
fn addr_of_ty(dest_ty: Option<&Type>) -> Type {
    match dest_ty {
        Some(t @ Type::Ptr(_)) => t.clone(),
        _ => Type::Ptr(Box::new(QualType::new(Type::Void))),
    }
}

/// 一元指令重建时的**结果类型**。
///
/// 第一真源是指令 `dest` 的类型：构建器在 `lower_expr` 里就是用原始
/// `Expr::Unary.ty` 给 dest 定型的，所以它已经带好了「`-d` 是 double、`!x` 是 int」
/// 这类 C 语义。旧实现只认向量，其余一律写 `Type::Int` ⇒ `n = -b`（b 是 double）
/// 重建后成了 `Unary{ty: Int}`，codegen 的一元负号分支（`emit/expr/unary.rs` 靠
/// `e.ty()` 判走不走 xorpd）于是落回整数 `neg r64`，浮点取负直接丢失（实测 -O2
/// 打印 `0.000000` 而不是 `-2.300000`）。dest 缺失时才按 op 与操作数类型推。
fn unary_result_ty(dest_ty: Option<&Type>, op: &UnOp, inner_ty: &Type) -> Type {
    if let Some(t) = dest_ty {
        if !matches!(t, Type::Void) {
            return t.clone();
        }
    }
    match op {
        // `!x` 的结果在 C 里永远是 int。
        UnOp::Not => Type::Int,
        // 其余一元运算保持操作数类型（Neg/BitNot/Plus 在向量上逐元素，类型不变）。
        UnOp::Neg | UnOp::BitNot | UnOp::Plus => inner_ty.clone(),
    }
}

/// 将 SSA 指令转换为 AST 表达式, 同时应用内联替换。
/// `inst_span` 是原始 SSA 指令的 span, 用于保留源码位置信息。
/// `dest_ty` 是该指令 `dest` 变器的 SSA 类型 —— 它是 `AddrOf` 重建 `&base` 与
/// `Un` 重建一元结果（`-d` 必须仍是 double）这两个分支的类型**唯一真源**。
pub(super) fn inst_to_expr_resolved(
    kind: &InstKind,
    subst: &HashMap<String, Option<Expr>>,
    inst_span: &Span,
    func: &Function,
    dest_ty: Option<&Type>,
) -> Expr {
    let sp = inst_span.clone();
    let fake_env: HashMap<String, SsaVar> = HashMap::new();
    match kind {
        InstKind::Bin { op, l, r } => {
            let l_expr = resolve_operand(l, &fake_env, subst, func);
            let r_expr = resolve_operand(r, &fake_env, subst, func);
            let binop_ty = infer_binop_type(op, &l_expr, &r_expr);
            Expr::Binop {
                op: *op,
                l: Box::new(l_expr),
                r: Box::new(r_expr),
                span: sp.clone(),
                ty: binop_ty,
            }
        },
        InstKind::Un { op, e } => {
            let inner = resolve_operand(e, &fake_env, subst, func);
            let ty = unary_result_ty(dest_ty, op, &inner.ty());
            Expr::Unary {
                op: *op,
                e: Box::new(inner),
                span: sp.clone(),
                ty,
            }
        },
        InstKind::Cast { ty, e } => Expr::Cast {
            ty: ty.clone(),
            e: Box::new(resolve_operand(e, &fake_env, subst, func)),
            span: sp.clone(),
        },
        InstKind::AddrOf { base } => Expr::AddrOf {
            e: Box::new(Expr::Ident {
                name: crate::ast::Ident {
                    name: base.clone(),
                    span: sp.clone(),
                },
                span: sp.clone(),
                ty: Type::Void,
            }),
            span: sp.clone(),
            ty: addr_of_ty(dest_ty),
        },
        InstKind::Load { e, ty, .. } => Expr::Deref {
            e: Box::new(resolve_operand(e, &fake_env, subst, func)),
            span: sp.clone(),
            ty: ty.clone(),
        },
        InstKind::Store { e, src, .. } => {
            let addr = resolve_operand(e, &fake_env, subst, func);
            let dty = store_pointee_ty(&addr);
            Expr::Assign {
                op: BinOp::Add,
                l: Box::new(Expr::Deref {
                    e: Box::new(addr),
                    span: sp.clone(),
                    ty: dty,
                }),
                r: Box::new(resolve_operand(src, &fake_env, subst, func)),
                span: sp.clone(),
                ty: Type::Void,
            }
        },
        InstKind::IndexLoad { arr, idx, ty, .. } => Expr::Index {
            arr: Box::new(resolve_operand(arr, &fake_env, subst, func)),
            idx: Box::new(resolve_operand(idx, &fake_env, subst, func)),
            span: sp.clone(),
            ty: ty.clone(),
        },
        InstKind::IndexAddr { arr, idx, ty } => Expr::AddrOf {
            // 下标取址: 装回 `&arr[idx]`，由 codegen 的 AddrOf(Index) 分支算步长。
            e: Box::new(Expr::Index {
                arr: Box::new(resolve_operand(arr, &fake_env, subst, func)),
                idx: Box::new(resolve_operand(idx, &fake_env, subst, func)),
                span: sp.clone(),
                ty: ty.clone(),
            }),
            span: sp.clone(),
            ty: Type::Ptr(Box::new(crate::ast::QualType::new(ty.clone()))),
        },
        InstKind::IndexStore {
            arr, idx, src, ty, ..
        } => Expr::Assign {
            op: BinOp::Add,
            l: Box::new(Expr::Index {
                arr: Box::new(resolve_operand(arr, &fake_env, subst, func)),
                idx: Box::new(resolve_operand(idx, &fake_env, subst, func)),
                span: sp.clone(),
                ty: ty.clone(),
            }),
            r: Box::new(resolve_operand(src, &fake_env, subst, func)),
            span: sp.clone(),
            ty: Type::Void,
        },
        InstKind::Call {
            callee,
            args,
            ret_ty,
        } => Expr::Call {
            callee: Box::new(resolve_operand(callee, &fake_env, subst, func)),
            args: args
                .iter()
                .map(|a| resolve_operand(a, &fake_env, subst, func))
                .collect(),
            span: sp.clone(),
            ty: ret_ty.clone(),
        },
        // C++ 零成本 EH: 受保护调用的**正常路径**与普通调用完全同形 (落地垫由
        // unwinder 按地址区间进入, 不是表达式的一部分)。`rebuild_bb` 会先把这条
        // 指令拦下来包成 `Stmt::Invoke`, 这里只负责调用表达式本身。
        InstKind::Invoke {
            callee,
            args,
            ret_ty,
            ..
        } => Expr::Call {
            callee: Box::new(resolve_operand(callee, &fake_env, subst, func)),
            args: args
                .iter()
                .map(|a| resolve_operand(a, &fake_env, subst, func))
                .collect(),
            span: sp.clone(),
            ty: ret_ty.clone(),
        },
        // 落地垫序言由 `rebuild_bb` 拦截成 `Stmt::EhLandingPad`, 不会走到这里;
        // 保留一个无副作用占位, 以免新增 InstKind 被误当成可内联的纯计算。
        InstKind::EhLandingPad { .. } => Expr::Int {
            v: 0,
            span: sp.clone(),
            ty: Type::Int,
        },
        InstKind::MemberAddr {
            obj,
            obj_ty,
            field,
            ty,
            is_arrow,
            ..
        } => {
            // 子对象地址: 装回 `&obj->field` / `&(obj.field)`，偏移由 codegen 的
            // 地址形态给出（与 `MemberLoad` 同一个单一真源约定）。
            let mut base = resolve_operand(obj, &fake_env, subst, func);
            set_expr_ty_if_compatible(&mut base, obj_ty);
            Expr::AddrOf {
                e: Box::new(Expr::Member {
                    expr: Box::new(base),
                    field: crate::ast::Ident {
                        name: field.clone(),
                        span: sp.clone(),
                    },
                    span: sp.clone(),
                    ty: ty.clone(),
                    is_arrow: *is_arrow,
                }),
                span: sp.clone(),
                ty: Type::Ptr(Box::new(crate::ast::QualType::new(ty.clone()))),
            }
        },
        InstKind::MemberLoad {
            obj,
            obj_ty,
            field,
            ty,
            is_arrow,
            ..
        } => {
            let mut base = resolve_operand(obj, &fake_env, subst, func);
            // 箭头访问 `p->fld` 时, resolve_operand 返回的 Ident 类型可能因 SSA
            // 简化而丢失原始指针类型 (例如 pDb 被简化成 Int/Ptr(Int)), 导致
            // codegen 按错误的基类型查字段偏移 — 实测 `pX = pDb->pBt` 编译成
            // movslq (%rax),%rax, 把 zDbSName 当 32 位 int 截断塞进 pX。
            // 对 is_arrow 同样调用 set_expr_ty_if_compatible, 把 obj_ty (Ptr(Db))
            // 写回 base 的 Ident/Deref.ty。
            set_expr_ty_if_compatible(&mut base, obj_ty);
            Expr::Member {
                expr: Box::new(base),
                field: crate::ast::Ident {
                    name: field.clone(),
                    span: sp.clone(),
                },
                span: sp.clone(),
                ty: ty.clone(),
                is_arrow: *is_arrow,
            }
        },
        InstKind::MemberStore {
            obj,
            obj_ty,
            field,
            src,
            ty,
            is_arrow,
            ..
        } => {
            let mut base = resolve_operand(obj, &fake_env, subst, func);
            set_expr_ty_if_compatible(&mut base, obj_ty);
            Expr::Assign {
                op: BinOp::Add,
                l: Box::new(Expr::Member {
                    expr: Box::new(base),
                    field: crate::ast::Ident {
                        name: field.clone(),
                        span: sp.clone(),
                    },
                    span: sp.clone(),
                    ty: ty.clone(),
                    is_arrow: *is_arrow,
                }),
                r: Box::new(resolve_operand(src, &fake_env, subst, func)),
                span: sp.clone(),
                // C11 6.5.16p3: 赋值表达式的类型 = 左操作数的类型
                ty: ty.clone(),
            }
        },
        InstKind::Select { cond, then_, else_ } => Expr::Cond {
            cond: Box::new(resolve_operand(cond, &fake_env, subst, func)),
            then_: Box::new(resolve_operand(then_, &fake_env, subst, func)),
            else_: Box::new(resolve_operand(else_, &fake_env, subst, func)),
            span: sp.clone(),
            ty: Type::Int,
        },
        InstKind::InlineAsm {
            asm_str,
            outputs,
            output_names,
            output_constraints,
            inputs,
            input_names,
            input_constraints,
            clobbers,
            volatile,
            ty,
            output_vars,
            ..
        } => {
            let resolved_outputs = outputs
                .iter()
                .enumerate()
                .map(|(idx, output)| {
                    output_vars
                        .get(idx)
                        .map(|v| Expr::Ident {
                            name: crate::ast::Ident {
                                name: v.base.clone(),
                                span: v.span.clone(),
                            },
                            span: v.span.clone(),
                            ty: v.ty.clone(),
                        })
                        .unwrap_or_else(|| output.clone())
                })
                .collect();
            Expr::AsmExpr {
                asm_str: asm_str.clone(),
                outputs: resolved_outputs,
                output_names: output_names.clone(),
                output_constraints: output_constraints.clone(),
                inputs: inputs
                    .iter()
                    .take(input_names.len())
                    .map(|o| resolve_operand(o, &fake_env, subst, func))
                    .collect(),
                input_names: input_names.clone(),
                input_constraints: input_constraints.clone(),
                clobbers: clobbers.clone(),
                volatile: *volatile,
                span: sp.clone(),
                ty: ty.clone(),
            }
        },
        InstKind::PreservedDecl { .. } => {
            // Handled before inst_to_expr_resolved is called
            Expr::Int {
                v: 0,
                span: sp.clone(),
                ty: Type::Int,
            }
        },
    }
}

/// 收集 InstKind 中所有被引用的 SSA 变量名。
pub(super) fn collect_op_uses(kind: &InstKind, out: &mut HashSet<String>) {
    match kind {
        InstKind::Bin { l, r, .. } => {
            collect_op_uses_op(l, out);
            collect_op_uses_op(r, out);
        },
        InstKind::Un { e, .. } => collect_op_uses_op(e, out),
        InstKind::Cast { e, .. } => collect_op_uses_op(e, out),
        InstKind::Load { e, .. } => collect_op_uses_op(e, out),
        InstKind::Store { e, src, .. } => {
            collect_op_uses_op(e, out);
            collect_op_uses_op(src, out);
        },
        InstKind::IndexLoad { arr, idx, .. } => {
            collect_op_uses_op(arr, out);
            collect_op_uses_op(idx, out);
        },
        // `IndexAddr` 与 `IndexLoad` 同一操作数形状（`arr` 基址 + `idx` 下标），内联计划
        // 的使用计数必须一视同仁：漏了它，作为下一层基址的行地址临时值会被当成
        // 「无消费者」而内联进取址操作数 → 嵌套 AddrOf（历史段错误的根因）。
        InstKind::IndexAddr { arr, idx, .. } => {
            collect_op_uses_op(arr, out);
            collect_op_uses_op(idx, out);
        },
        InstKind::IndexStore { arr, idx, src, .. } => {
            collect_op_uses_op(arr, out);
            collect_op_uses_op(idx, out);
            collect_op_uses_op(src, out);
        },
        InstKind::Call { callee, args, .. } => {
            collect_op_uses_op(callee, out);
            for a in args {
                collect_op_uses_op(a, out);
            }
        },
        // C++ 零成本 EH: 同 `collect_var_uses_op`。
        InstKind::Invoke { callee, args, .. } => {
            collect_op_uses_op(callee, out);
            for a in args {
                collect_op_uses_op(a, out);
            }
        },
        InstKind::MemberLoad { obj, .. } => collect_op_uses_op(obj, out),
        InstKind::MemberStore { obj, src, .. } => {
            collect_op_uses_op(obj, out);
            collect_op_uses_op(src, out);
        },
        InstKind::Select { cond, then_, else_ } => {
            collect_op_uses_op(cond, out);
            collect_op_uses_op(then_, out);
            collect_op_uses_op(else_, out);
        },
        InstKind::InlineAsm { inputs, .. } => {
            for input in inputs {
                collect_op_uses_op(input, out);
            }
        },
        _ => {},
    }
}

pub(super) fn collect_op_uses_op(o: &Operand, out: &mut HashSet<String>) {
    if let Operand::Var(v) = o {
        out.insert(v.name());
    }
}

/// 计算某个 SSA 变量在 InstKind 中被引用的次数。
pub(super) fn op_count_in_kind(kind: &InstKind, name: &str) -> usize {
    let mut count = 0;
    match kind {
        InstKind::Bin { l, r, .. } => {
            count += op_count_in_op(l, name);
            count += op_count_in_op(r, name);
        },
        InstKind::Un { e, .. } => count += op_count_in_op(e, name),
        InstKind::Cast { e, .. } => count += op_count_in_op(e, name),
        InstKind::Load { e, .. } => count += op_count_in_op(e, name),
        InstKind::Store { e, src, .. } => {
            count += op_count_in_op(e, name);
            count += op_count_in_op(src, name);
        },
        InstKind::IndexLoad { arr, idx, .. } => {
            count += op_count_in_op(arr, name);
            count += op_count_in_op(idx, name);
        },
        // 同 `collect_op_uses`：`IndexAddr` 的 `arr`/`idx` 都是真实使用，必须计入使用次数。
        InstKind::IndexAddr { arr, idx, .. } => {
            count += op_count_in_op(arr, name);
            count += op_count_in_op(idx, name);
        },
        InstKind::IndexStore { arr, idx, src, .. } => {
            count += op_count_in_op(arr, name);
            count += op_count_in_op(idx, name);
            count += op_count_in_op(src, name);
        },
        InstKind::Call { callee, args, .. } => {
            count += op_count_in_op(callee, name);
            for a in args {
                count += op_count_in_op(a, name);
            }
        },
        // C++ 零成本 EH: 受保护调用的操作数必须计入使用次数。
        InstKind::Invoke { callee, args, .. } => {
            count += op_count_in_op(callee, name);
            for a in args {
                count += op_count_in_op(a, name);
            }
        },
        InstKind::MemberLoad { obj, .. } => count += op_count_in_op(obj, name),
        InstKind::MemberStore { obj, src, .. } => {
            count += op_count_in_op(obj, name);
            count += op_count_in_op(src, name);
        },
        InstKind::Select { cond, then_, else_ } => {
            count += op_count_in_op(cond, name);
            count += op_count_in_op(then_, name);
            count += op_count_in_op(else_, name);
        },
        InstKind::InlineAsm { inputs, .. } => {
            for input in inputs {
                count += op_count_in_op(input, name);
            }
        },
        _ => {},
    }
    count
}

pub(super) fn op_count_in_op(o: &Operand, name: &str) -> usize {
    if let Operand::Var(v) = o {
        if v.name() == name {
            return 1;
        }
    }
    0
}

fn operand_to_expr(o: &Operand, env: &HashMap<String, SsaVar>, func: &Function) -> Expr {
    match o {
        Operand::Var(v) => {
            if let Some(found) = env.get(&v.name()) {
                ssa_ident_expr(&func.local_splits.ast_name(found), v)
            } else {
                ssa_ident_expr(&func.local_splits.ast_name(v), v)
            }
        },
        Operand::Int(v) => Expr::Int {
            v: *v,
            span: Span::point("ssa", 0, 0),
            // 整数常量在 SSA 里是**无类型**的 (`Operand::Int(i128)`), 重建时必须
            // 还原它的类型 —— 旧实现硬写 `Type::Int`, 于是
            // ① 立即数按 32 位物化, `10306098990L / 100L` 的高位被截成 1716164398;
            // ② `infer_binop_type` 从操作数类型推常见算术转换, 整个运算也跟着降格
            //    成 32 位记账 (与下面 `infer_binop_type` 的根因说明同一轴)。
            // 类型取「能表示该值的最小秩」(= C++ [lex.icon] 无符号后缀十进制字面量
            // 的量级规则): 32 位可表示时仍是 `Int`, 现有代码路径零变化; 只有原本就
            // 被截断的宽常量才升秩。
            ty: int_const_ty(*v),
        },
        Operand::Float(c, ty) => Expr::Float {
            c: *c,
            span: Span::point("ssa", 0, 0),
            ty: ty.clone(),
        },
        Operand::Str(d) => Expr::String {
            data: d.clone(),
            span: Span::point("ssa", 0, 0),
            ty: Type::Array(
                Box::new(QualType::new(Type::Char)),
                crate::ast::ArraySize::Const(d.len()),
            ),
        },
        Operand::SymAddr(n, ty) => Expr::Ident {
            name: crate::ast::Ident {
                name: n.clone(),
                span: Span::point("ssa", 0, 0),
            },
            span: Span::point("ssa", 0, 0),
            ty: ty.clone(),
        },
        Operand::FnAddr(n, ty) => Expr::Ident {
            name: crate::ast::Ident {
                name: n.clone(),
                span: Span::point("ssa", 0, 0),
            },
            span: Span::point("ssa", 0, 0),
            ty: Type::Ptr(Box::new(QualType::new(ty.clone()))),
        },
        Operand::Void => Expr::Int {
            v: 0,
            span: Span::point("ssa", 0, 0),
            ty: Type::Void,
        },
        Operand::Call(_) => Expr::Int {
            v: 0,
            span: Span::point("ssa", 0, 0),
            ty: Type::Int,
        },
        Operand::Phi(_) => Expr::Int {
            v: 0,
            span: Span::point("ssa", 0, 0),
            ty: Type::Int,
        },
    }
}

/// 将 SSA 指令的 kind 转换为 AST 表达式 (右侧值)。
fn inst_to_expr(kind: &InstKind, env: &HashMap<String, SsaVar>, func: &Function) -> Expr {
    let sp = Span::point("ssa", 0, 0);
    match kind {
        InstKind::Bin { op, l, r } => {
            let l_expr = operand_to_expr(l, env, func);
            let r_expr = operand_to_expr(r, env, func);
            let binop_ty = infer_binop_type(op, &l_expr, &r_expr);
            Expr::Binop {
                op: *op,
                l: Box::new(l_expr),
                r: Box::new(r_expr),
                span: sp.clone(),
                ty: binop_ty,
            }
        },
        InstKind::Un { op, e } => Expr::Unary {
            op: *op,
            e: Box::new(operand_to_expr(e, env, func)),
            span: sp.clone(),
            // 与 `inst_to_expr_resolved` 共用同一推型规则（本函数目前无调用点，
            // 但两套规则一旦分叉，将来启用它就会拿到错类型）。
            ty: unary_result_ty(
                None,
                op,
                &match e {
                    Operand::Var(v) => v.ty.clone(),
                    Operand::Float(_, t) => t.clone(),
                    Operand::SymAddr(_, t) => t.clone(),
                    Operand::FnAddr(_, t) => t.clone(),
                    Operand::Int(_) => Type::Int,
                    _ => Type::Int,
                },
            ),
        },
        InstKind::Cast { ty, e } => Expr::Cast {
            ty: ty.clone(),
            e: Box::new(operand_to_expr(e, env, func)),
            span: sp.clone(),
        },
        InstKind::AddrOf { base } => Expr::AddrOf {
            e: Box::new(Expr::Ident {
                name: crate::ast::Ident {
                    name: base.clone(),
                    span: sp.clone(),
                },
                span: sp.clone(),
                ty: Type::Void,
            }),
            span: sp.clone(),
            ty: addr_of_ty(None),
        },
        InstKind::Load { e, ty, .. } => Expr::Deref {
            e: Box::new(operand_to_expr(e, env, func)),
            span: sp.clone(),
            ty: ty.clone(),
        },
        InstKind::Store { e, src, .. } => {
            let addr = operand_to_expr(e, env, func);
            let dty = store_pointee_ty(&addr);
            Expr::Assign {
                op: BinOp::Add,
                l: Box::new(Expr::Deref {
                    e: Box::new(addr),
                    span: sp.clone(),
                    ty: dty,
                }),
                r: Box::new(operand_to_expr(src, env, func)),
                span: sp.clone(),
                ty: Type::Void,
            }
        },
        InstKind::IndexLoad { arr, idx, ty, .. } => Expr::Index {
            arr: Box::new(operand_to_expr(arr, env, func)),
            idx: Box::new(operand_to_expr(idx, env, func)),
            span: sp.clone(),
            ty: ty.clone(),
        },
        InstKind::IndexAddr { arr, idx, ty } => Expr::AddrOf {
            // 与上面同一形态（本函数是内联临时值用的另一份表达式构造器）。
            e: Box::new(Expr::Index {
                arr: Box::new(operand_to_expr(arr, env, func)),
                idx: Box::new(operand_to_expr(idx, env, func)),
                span: sp.clone(),
                ty: ty.clone(),
            }),
            span: sp.clone(),
            ty: Type::Ptr(Box::new(crate::ast::QualType::new(ty.clone()))),
        },
        InstKind::IndexStore {
            arr, idx, src, ty, ..
        } => Expr::Assign {
            op: BinOp::Add,
            l: Box::new(Expr::Index {
                arr: Box::new(operand_to_expr(arr, env, func)),
                idx: Box::new(operand_to_expr(idx, env, func)),
                span: sp.clone(),
                ty: ty.clone(),
            }),
            r: Box::new(operand_to_expr(src, env, func)),
            span: sp.clone(),
            ty: Type::Void,
        },
        InstKind::Call {
            callee,
            args,
            ret_ty,
        } => Expr::Call {
            callee: Box::new(operand_to_expr(callee, env, func)),
            args: args.iter().map(|a| operand_to_expr(a, env, func)).collect(),
            span: sp.clone(),
            ty: ret_ty.clone(),
        },
        // C++ 零成本 EH: 同 `inst_to_expr_resolved`, 受保护调用的正常路径就是
        // 一次普通调用; 落地垫序言由 `rebuild_bb` 拦截, 此处仅占位。
        InstKind::Invoke {
            callee,
            args,
            ret_ty,
            ..
        } => Expr::Call {
            callee: Box::new(operand_to_expr(callee, env, func)),
            args: args.iter().map(|a| operand_to_expr(a, env, func)).collect(),
            span: sp.clone(),
            ty: ret_ty.clone(),
        },
        InstKind::EhLandingPad { .. } => Expr::Int {
            v: 0,
            span: sp.clone(),
            ty: Type::Int,
        },
        InstKind::MemberAddr {
            obj,
            obj_ty,
            field,
            ty,
            is_arrow,
            ..
        } => {
            // 子对象地址: 装回 `&obj->field` / `&(obj.field)`, 偏移交给 codegen
            // 现有的地址形态（与 `MemberLoad` 单一真源同一约定）。
            let mut base = operand_to_expr(obj, env, func);
            set_expr_ty_if_compatible(&mut base, obj_ty);
            Expr::AddrOf {
                e: Box::new(Expr::Member {
                    expr: Box::new(base),
                    field: crate::ast::Ident {
                        name: field.clone(),
                        span: sp.clone(),
                    },
                    span: sp.clone(),
                    ty: ty.clone(),
                    is_arrow: *is_arrow,
                }),
                span: sp.clone(),
                ty: Type::Ptr(Box::new(crate::ast::QualType::new(ty.clone()))),
            }
        },
        InstKind::MemberLoad {
            obj,
            obj_ty,
            field,
            ty,
            is_arrow,
            ..
        } => {
            let mut base = operand_to_expr(obj, env, func);
            set_expr_ty_if_compatible(&mut base, obj_ty);
            Expr::Member {
                expr: Box::new(base),
                field: crate::ast::Ident {
                    name: field.clone(),
                    span: sp.clone(),
                },
                span: sp.clone(),
                ty: ty.clone(),
                is_arrow: *is_arrow,
            }
        },
        InstKind::MemberStore {
            obj,
            obj_ty,
            field,
            src,
            ty,
            is_arrow,
            ..
        } => {
            let mut base = operand_to_expr(obj, env, func);
            set_expr_ty_if_compatible(&mut base, obj_ty);
            Expr::Assign {
                op: BinOp::Add,
                l: Box::new(Expr::Member {
                    expr: Box::new(base),
                    field: crate::ast::Ident {
                        name: field.clone(),
                        span: sp.clone(),
                    },
                    span: sp.clone(),
                    ty: ty.clone(),
                    is_arrow: *is_arrow,
                }),
                r: Box::new(operand_to_expr(src, env, func)),
                span: sp.clone(),
                // C11 6.5.16p3: 赋值表达式的类型 = 左操作数的类型
                ty: ty.clone(),
            }
        },
        InstKind::Select { cond, then_, else_ } => Expr::Cond {
            cond: Box::new(operand_to_expr(cond, env, func)),
            then_: Box::new(operand_to_expr(then_, env, func)),
            else_: Box::new(operand_to_expr(else_, env, func)),
            span: sp.clone(),
            ty: Type::Int,
        },
        InstKind::InlineAsm {
            asm_str,
            outputs,
            output_names,
            output_constraints,
            inputs,
            input_names,
            input_constraints,
            clobbers,
            volatile,
            ty,
            ..
        } => Expr::AsmExpr {
            asm_str: asm_str.clone(),
            outputs: outputs.clone(),
            output_names: output_names.clone(),
            output_constraints: output_constraints.clone(),
            inputs: inputs
                .iter()
                .take(input_names.len())
                .map(|a| operand_to_expr(a, env, func))
                .collect(),
            input_names: input_names.clone(),
            input_constraints: input_constraints.clone(),
            clobbers: clobbers.clone(),
            volatile: *volatile,
            span: sp.clone(),
            ty: ty.clone(),
        },
        InstKind::PreservedDecl { .. } => {
            // Handled separately in rebuild; should not reach here
            Expr::Int {
                v: 0,
                span: sp.clone(),
                ty: Type::Int,
            }
        },
    }
}
