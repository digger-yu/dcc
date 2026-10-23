//! src/ast_cmp_fold.rs — AST 层比较恒等式的折叠。
//!
//! 为什么必须有这一遍（且**不能**只在 -O1+ 跑）：
//! gcc.dg 的 pr27302 / pr27302-2 用 `link_error()` 作探针 ——
//! `if ((a < b) != (b > a)) link_error();` 只有在编译器证明条件恒假时才不产生
//! 对该外部符号的引用，链接期才不会报 undefined reference。GCC 在**前端**
//! fold-const 里就折掉这类关系恒等式（与 -O 档无关），dcc 的 `fold_constants`
//! 只在 -O1+ 跑，且只折两侧都是常量的表达式，所以默认档位下把调用留进了产物。
//!
//! 判据（单一真源在这里，别处不要复制）：
//!   把比较两侧化成同一整数类型下的线性形式 Σ c_i·x_i + k（x_i 为纯原子表达式），
//!   再把关系规范成 `Σ terms <= bound` / `>=` / `==` / `!=`，并统一「首项系数为正」
//!   的取向；两个比较的规范形式相同 ⇒ 恒等 ⇒ 外层 `==`/`!=` 折成常量。
//!
//! 安全边界：
//!   - 只处理**有符号整数**、且两侧操作数类型相同的比较；
//!   - 排序类比较（< <= > >=）依赖「有符号不溢出」语义，`-fno-strict-overflow`
//!     时整遍关闭排序推理（等值推理只用到环公理，wrapping 下仍成立）；
//!   - 浮点 / 指针 / 可能读 volatile 的原子 / 有副作用的表达式一律放弃；
//!   - 只折外层 `==`/`!=`，不 rewrite 单个比较，改动面最小。

use std::collections::{BTreeMap, HashSet};

use crate::ast::{
    BinOp, Block, BlockItem, Decl, Designator, Expr, InitItem, Program, Stmt, Type, UnOp,
};
use crate::ir::Module;

/// 对整棵程序做比较恒等式折叠。
///
/// `strict_overflow` 对应 `-fstrict-overflow`（dcc 默认 true）：为 false 时
/// 不做排序类比较的线性推理。
pub fn run(module: &mut Module, strict_overflow: bool) {
    let prog = module.source_mut();
    let volatiles = collect_volatiles(prog);
    fold_program(prog, &volatiles, strict_overflow);
}

// ---------------------------------------------------------------------------
// 原子与线性形式
// ---------------------------------------------------------------------------

/// 有符号整数类型的规范键；非有符号整数返回 None。
fn sint_key(ty: &Type) -> Option<&'static str> {
    match ty {
        Type::SChar => Some("schar"),
        Type::Short => Some("short"),
        Type::Int => Some("int"),
        Type::Long => Some("long"),
        Type::LongLong => Some("llong"),
        Type::Int128 => Some("i128"),
        _ => None,
    }
}

/// 线性形式：Σ coeff_i·atom_i + konst。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Lin {
    terms: BTreeMap<String, i128>,
    konst: i128,
    /// 子树里非恒定原子的类型键；None 表示整棵子树是纯常量。
    atom_ty: Option<&'static str>,
}

impl Lin {
    fn constant(v: i128) -> Self {
        Self {
            terms: BTreeMap::new(),
            konst: v,
            atom_ty: None,
        }
    }

    fn atom(key: String, ty: &'static str) -> Self {
        let mut terms = BTreeMap::new();
        terms.insert(key, 1);
        Self {
            terms,
            konst: 0,
            atom_ty: Some(ty),
        }
    }

    fn is_constant(&self) -> bool {
        self.terms.is_empty()
    }

    /// 线性缩放；系数溢出 i128 时放弃（返回 None）。
    fn scaled(&self, factor: i128) -> Option<Self> {
        let mut terms = BTreeMap::new();
        for (k, v) in &self.terms {
            terms.insert(k.clone(), v.checked_mul(factor)?);
        }
        Some(Self {
            terms,
            konst: self.konst.checked_mul(factor)?,
            atom_ty: self.atom_ty,
        })
    }

    /// `self + sign * other`；原子类型不一致或常量溢出时返回 None。
    fn combine(&self, other: &Lin, sign: i128) -> Option<Self> {
        let atom_ty = match (self.atom_ty, other.atom_ty) {
            (Some(a), Some(b)) if a != b => return None,
            (Some(a), _) => Some(a),
            (None, b) => b,
        };
        let mut terms = self.terms.clone();
        for (k, v) in &other.terms {
            let add = v.checked_mul(sign)?;
            let e = terms.entry(k.clone()).or_insert(0);
            *e = e.checked_add(add)?;
            if *e == 0 {
                // 系数抵消: 移除该项, 保证规范形式唯一 (a - a 与 0 同键)。
                terms.remove(k);
            }
        }
        Some(Self {
            terms,
            konst: self.konst.checked_add(other.konst.checked_mul(sign)?)?,
            atom_ty,
        })
    }
}

/// 纯原子表达式的结构化键（忽略 span）。不可重复求值的表达式一律 None。
fn atom_key(e: &Expr, volatiles: &HashSet<String>) -> Option<String> {
    match e {
        Expr::Ident { name, ty, .. } => {
            if volatiles.contains(&name.name) {
                return None;
            }
            let k = sint_key(ty)?;
            Some(format!("v:{k}:{}", name.name))
        },
        // 同类型的 Cast 不改变值（有符号整数间仅同类型才允许，见 lint）。
        Expr::Cast { ty, e: inner, .. } if inner.ty() == *ty => atom_key(inner, volatiles),
        _ => None,
    }
}

/// 把整数表达式化成线性形式。非整数 / 非同类型原子 / 含副作用 → None。
fn linearize(e: &Expr, volatiles: &HashSet<String>) -> Option<Lin> {
    match e {
        Expr::Int { v, ty, .. } | Expr::Char { v, ty, .. } => {
            sint_key(ty)?;
            Some(Lin::constant(*v))
        },
        Expr::Ident { .. } => {
            let key = atom_key(e, volatiles)?;
            let ty = sint_key(&e.ty())?;
            Some(Lin::atom(key, ty))
        },
        Expr::Unary {
            op: UnOp::Plus,
            e: inner,
            ty,
            ..
        } => {
            sint_key(ty)?;
            linearize(inner, volatiles)
        },
        Expr::Unary {
            op: UnOp::Neg,
            e: inner,
            ty,
            ..
        } => {
            sint_key(ty)?;
            linearize(inner, volatiles)?.scaled(-1)
        },
        Expr::Binop { op, l, r, ty, .. } => {
            sint_key(ty)?;
            let a = linearize(l, volatiles)?;
            let b = linearize(r, volatiles)?;
            match op {
                BinOp::Add => a.combine(&b, 1),
                BinOp::Sub => a.combine(&b, -1),
                // 只支持「常数 × 线性式」; 两个非常数相乘超出本判据。
                BinOp::Mul if a.is_constant() => b.scaled(a.konst),
                BinOp::Mul if b.is_constant() => a.scaled(b.konst),
                _ => None,
            }
        },
        Expr::Cast { ty, e: inner, .. } if inner.ty() == *ty => {
            sint_key(ty)?;
            linearize(inner, volatiles)
        },
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 关系规范形式
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelKind {
    Le,
    Ge,
    Eq,
    Ne,
}

/// 规范关系：`Σ terms <= bound` / `>=` / `==` / `!=`，首项系数恒为正。
#[derive(Debug, Clone, PartialEq, Eq)]
struct RelKey {
    kind: RelKind,
    terms: BTreeMap<String, i128>,
    bound: i128,
}

/// 把比较表达式规范成 [`RelKey`]；不是可推理的整数比较时返回 None。
fn rel_key(e: &Expr, volatiles: &HashSet<String>, strict_overflow: bool) -> Option<RelKey> {
    let Expr::Binop { op, l, r, ty, .. } = e else {
        return None;
    };
    let ordering = match op {
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => true,
        BinOp::EqEq | BinOp::Ne => false,
        _ => return None,
    };
    if ordering && !strict_overflow {
        return None;
    }
    sint_key(ty)?;
    if l.ty() != r.ty() {
        return None;
    }
    let operand_ty = sint_key(&l.ty())?;
    let a = linearize(l, volatiles)?;
    let b = linearize(r, volatiles)?;
    if a.atom_ty.is_some_and(|t| t != operand_ty) || b.atom_ty.is_some_and(|t| t != operand_ty) {
        return None;
    }
    // D = L - R，比较等价于 D 与 0 的关系。
    let d = a.combine(&b, -1)?;
    if d.is_constant() {
        // 纯常量比较交给常量折叠，不在这里造常量。
        return None;
    }
    // D REL 0 规范成 terms REL bound（常量全部移到 bound）。
    let (kind, bound) = match op {
        BinOp::Lt => (RelKind::Le, d.konst.checked_neg()?.checked_sub(1)?),
        BinOp::Le => (RelKind::Le, d.konst.checked_neg()?),
        BinOp::Gt => (RelKind::Ge, d.konst.checked_neg()?.checked_add(1)?),
        BinOp::Ge => (RelKind::Ge, d.konst.checked_neg()?),
        BinOp::EqEq => (RelKind::Eq, d.konst.checked_neg()?),
        BinOp::Ne => (RelKind::Ne, d.konst.checked_neg()?),
        _ => return None,
    };
    // 取向：首项系数为正；取反时 <= / >= 互换。
    if d.terms.values().next().copied().unwrap_or(0) < 0 {
        let mut terms = BTreeMap::new();
        for (k, v) in &d.terms {
            terms.insert(k.clone(), v.checked_neg()?);
        }
        let kind = match kind {
            RelKind::Le => RelKind::Ge,
            RelKind::Ge => RelKind::Le,
            other => other,
        };
        return Some(RelKey {
            kind,
            terms,
            bound: bound.checked_neg()?,
        });
    }
    Some(RelKey {
        kind,
        terms: d.terms,
        bound,
    })
}

// ---------------------------------------------------------------------------
// volatile 名字收集
// ---------------------------------------------------------------------------

fn collect_volatiles(prog: &Program) -> HashSet<String> {
    let mut out = HashSet::new();
    for decl in &prog.decls {
        collect_volatiles_decl(decl, &mut out);
    }
    out
}

fn collect_volatiles_decl(decl: &Decl, out: &mut HashSet<String>) {
    match decl {
        Decl::Var(v) => {
            if v.quals.is_volatile() {
                out.insert(v.name.name.clone());
            }
            if let Some(init) = &v.init {
                collect_volatiles_expr(init, out);
            }
        },
        Decl::Func(f) => {
            if let Some(body) = &f.body {
                collect_volatiles_block(body, out);
            }
        },
        Decl::GlobalAsm {
            outputs, inputs, ..
        } => {
            for e in outputs.iter().chain(inputs) {
                collect_volatiles_expr(e, out);
            }
        },
        _ => {},
    }
}

fn collect_volatiles_block(b: &Block, out: &mut HashSet<String>) {
    for item in &b.items {
        match item {
            BlockItem::Stmt(s) => collect_volatiles_stmt(s, out),
            BlockItem::Decl(d) => collect_volatiles_decl(d, out),
        }
    }
}

fn collect_volatiles_stmt(s: &Stmt, out: &mut HashSet<String>) {
    match s {
        Stmt::Return { val, .. } => {
            if let Some(e) = val {
                collect_volatiles_expr(e, out);
            }
        },
        Stmt::Expr { e, .. } => collect_volatiles_expr(e, out),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            collect_volatiles_expr(cond, out);
            collect_volatiles_stmt(then_, out);
            if let Some(e) = else_ {
                collect_volatiles_stmt(e, out);
            }
        },
        Stmt::While { cond, body, .. } => {
            collect_volatiles_expr(cond, out);
            collect_volatiles_stmt(body, out);
        },
        Stmt::Do { body, cond, .. } => {
            collect_volatiles_stmt(body, out);
            collect_volatiles_expr(cond, out);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                collect_volatiles_stmt(i, out);
            }
            if let Some(c) = cond {
                collect_volatiles_expr(c, out);
            }
            if let Some(st) = step {
                collect_volatiles_expr(st, out);
            }
            collect_volatiles_stmt(body, out);
        },
        Stmt::Block { b, .. } => collect_volatiles_block(b, out),
        Stmt::Label { stmt, .. } => collect_volatiles_stmt(stmt, out),
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            collect_volatiles_expr(expr, out);
            for item in prelude {
                match item {
                    BlockItem::Stmt(s) => collect_volatiles_stmt(s, out),
                    BlockItem::Decl(d) => collect_volatiles_decl(d, out),
                }
            }
            for case in cases {
                if let Some(v) = &case.val {
                    collect_volatiles_expr(v, out);
                }
                if let Some(v) = &case.range_end {
                    collect_volatiles_expr(v, out);
                }
                collect_volatiles_block(&case.body, out);
            }
        },
        Stmt::AsmGoto {
            outputs, inputs, ..
        } => {
            for e in outputs.iter().chain(inputs) {
                collect_volatiles_expr(e, out);
            }
        },
        Stmt::Invoke { call, .. } => collect_volatiles_stmt(call, out),
        Stmt::EhLandingPad {
            object, selector, ..
        } => {
            collect_volatiles_expr(object, out);
            collect_volatiles_expr(selector, out);
        },
        _ => {},
    }
}

fn collect_volatiles_expr(e: &Expr, out: &mut HashSet<String>) {
    // 只为「同名变量是否可能带 volatile」做保守判据：所有声明都扫一遍，
    // 同名即视为 volatile（多标一个只会让折叠更保守，不会错）。
    match e {
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            collect_volatiles_expr(l, out);
            collect_volatiles_expr(r, out);
        },
        Expr::Unary { e, .. }
        | Expr::Cast { e, .. }
        | Expr::Deref { e, .. }
        | Expr::AddrOf { e, .. }
        | Expr::SizeofExpr { e, .. }
        | Expr::AlignofExpr { e, .. }
        | Expr::Real { e, .. }
        | Expr::Imag { e, .. }
        | Expr::PostInc { e, .. }
        | Expr::PostDec { e, .. }
        | Expr::PreInc { e, .. }
        | Expr::PreDec { e, .. }
        | Expr::VaArg { ap: e, .. } => collect_volatiles_expr(e, out),
        Expr::Call { callee, args, .. } => {
            collect_volatiles_expr(callee, out);
            for a in args {
                collect_volatiles_expr(a, out);
            }
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            collect_volatiles_expr(cond, out);
            collect_volatiles_expr(then_, out);
            collect_volatiles_expr(else_, out);
        },
        Expr::Index { arr, idx, .. } => {
            collect_volatiles_expr(arr, out);
            collect_volatiles_expr(idx, out);
        },
        Expr::Member { expr, .. } => collect_volatiles_expr(expr, out),
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits {
                collect_volatiles_init_item(it, out);
            }
        },
        Expr::StmtExpr { block, .. } => collect_volatiles_block(block, out),
        Expr::AsmExpr {
            outputs, inputs, ..
        } => {
            for e in outputs.iter().chain(inputs) {
                collect_volatiles_expr(e, out);
            }
        },
        Expr::Offsetof { indices, .. } => {
            for e in indices {
                collect_volatiles_expr(e, out);
            }
        },
        _ => {},
    }
}

fn collect_volatiles_init_item(it: &InitItem, out: &mut HashSet<String>) {
    match it {
        InitItem::Expr(e) => collect_volatiles_expr(e, out),
        InitItem::Designated {
            designator, value, ..
        } => {
            collect_volatiles_designator(designator, out);
            collect_volatiles_expr(value, out);
        },
    }
}

fn collect_volatiles_designator(d: &Designator, out: &mut HashSet<String>) {
    match d {
        Designator::Index(e) => collect_volatiles_expr(e, out),
        Designator::Range(a, b) => {
            collect_volatiles_expr(a, out);
            collect_volatiles_expr(b, out);
        },
        Designator::Chain(list) => {
            for x in list {
                collect_volatiles_designator(x, out);
            }
        },
        Designator::Field(_) => {},
    }
}

// ---------------------------------------------------------------------------
// 折叠遍历
// ---------------------------------------------------------------------------

fn fold_program(prog: &mut Program, volatiles: &HashSet<String>, strict_overflow: bool) {
    for decl in &mut prog.decls {
        fold_decl(decl, volatiles, strict_overflow);
    }
}

fn fold_decl(decl: &mut Decl, volatiles: &HashSet<String>, so: bool) {
    match decl {
        Decl::Var(v) => {
            if let Some(init) = &mut v.init {
                fold_expr(init, volatiles, so);
            }
        },
        Decl::Func(f) => {
            if let Some(body) = &mut f.body {
                fold_block(&mut body.items, volatiles, so);
            }
        },
        Decl::GlobalAsm {
            outputs, inputs, ..
        } => {
            for e in outputs.iter_mut().chain(inputs.iter_mut()) {
                fold_expr(e, volatiles, so);
            }
        },
        _ => {},
    }
}

fn fold_block(items: &mut Vec<BlockItem>, volatiles: &HashSet<String>, so: bool) {
    for item in items.iter_mut() {
        match item {
            BlockItem::Stmt(s) => fold_stmt(s, volatiles, so),
            BlockItem::Decl(d) => fold_decl(d, volatiles, so),
        }
    }
}

fn fold_stmt(s: &mut Stmt, volatiles: &HashSet<String>, so: bool) {
    match s {
        Stmt::Return { val, .. } => {
            if let Some(e) = val {
                fold_expr(e, volatiles, so);
            }
        },
        Stmt::Expr { e, .. } => fold_expr(e, volatiles, so),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            fold_expr(cond, volatiles, so);
            fold_stmt(then_, volatiles, so);
            if let Some(e) = else_ {
                fold_stmt(e, volatiles, so);
            }
        },
        Stmt::While { cond, body, .. } => {
            fold_expr(cond, volatiles, so);
            fold_stmt(body, volatiles, so);
        },
        Stmt::Do { body, cond, .. } => {
            fold_stmt(body, volatiles, so);
            fold_expr(cond, volatiles, so);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                fold_stmt(i, volatiles, so);
            }
            if let Some(c) = cond {
                fold_expr(c, volatiles, so);
            }
            if let Some(st) = step {
                fold_expr(st, volatiles, so);
            }
            fold_stmt(body, volatiles, so);
        },
        Stmt::Block { b, .. } => fold_block(&mut b.items, volatiles, so),
        Stmt::Label { stmt, .. } => fold_stmt(stmt, volatiles, so),
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            fold_expr(expr, volatiles, so);
            for item in prelude.iter_mut() {
                match item {
                    BlockItem::Stmt(s) => fold_stmt(s, volatiles, so),
                    BlockItem::Decl(d) => fold_decl(d, volatiles, so),
                }
            }
            for case in cases {
                if let Some(v) = &mut case.val {
                    fold_expr(v, volatiles, so);
                }
                if let Some(v) = &mut case.range_end {
                    fold_expr(v, volatiles, so);
                }
                fold_block(&mut case.body.items, volatiles, so);
            }
        },
        Stmt::GotoIndirect { expr, .. } => fold_expr(expr, volatiles, so),
        Stmt::AsmGoto {
            outputs, inputs, ..
        } => {
            for e in outputs.iter_mut().chain(inputs.iter_mut()) {
                fold_expr(e, volatiles, so);
            }
        },
        Stmt::Invoke { call, .. } => fold_stmt(call, volatiles, so),
        Stmt::EhLandingPad {
            object, selector, ..
        } => {
            fold_expr(object, volatiles, so);
            fold_expr(selector, volatiles, so);
        },
        _ => {},
    }
}

fn fold_expr(e: &mut Expr, volatiles: &HashSet<String>, so: bool) {
    // 先递归子表达式（自底向上：内层恒等式先折掉，外层才看得到常量）。
    match e {
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            fold_expr(l, volatiles, so);
            fold_expr(r, volatiles, so);
        },
        Expr::Unary { e, .. }
        | Expr::Cast { e, .. }
        | Expr::Deref { e, .. }
        | Expr::AddrOf { e, .. }
        | Expr::SizeofExpr { e, .. }
        | Expr::AlignofExpr { e, .. }
        | Expr::Real { e, .. }
        | Expr::Imag { e, .. }
        | Expr::PostInc { e, .. }
        | Expr::PostDec { e, .. }
        | Expr::PreInc { e, .. }
        | Expr::PreDec { e, .. }
        | Expr::VaArg { ap: e, .. } => fold_expr(e, volatiles, so),
        Expr::Call { callee, args, .. } => {
            fold_expr(callee, volatiles, so);
            for a in args {
                fold_expr(a, volatiles, so);
            }
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            fold_expr(cond, volatiles, so);
            fold_expr(then_, volatiles, so);
            fold_expr(else_, volatiles, so);
        },
        Expr::Index { arr, idx, .. } => {
            fold_expr(arr, volatiles, so);
            fold_expr(idx, volatiles, so);
        },
        Expr::Member { expr, .. } => fold_expr(expr, volatiles, so),
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits {
                fold_init_item(it, volatiles, so);
            }
        },
        Expr::StmtExpr { block, .. } => fold_block(&mut block.items, volatiles, so),
        Expr::AsmExpr {
            outputs, inputs, ..
        } => {
            for e in outputs.iter_mut().chain(inputs.iter_mut()) {
                fold_expr(e, volatiles, so);
            }
        },
        Expr::Offsetof { indices, .. } => {
            for e in indices {
                fold_expr(e, volatiles, so);
            }
        },
        _ => {},
    }

    // 恒等式折叠：`(关系 A) == (关系 B)` / `!=`，A、B 规范形式相同即常量。
    let folded = if let Expr::Binop { op, l, r, span, .. } = e {
        if *op == BinOp::EqEq || *op == BinOp::Ne {
            let is_eq = *op == BinOp::EqEq;
            match (rel_key(l, volatiles, so), rel_key(r, volatiles, so)) {
                (Some(a), Some(b)) if a == b => Some((i128::from(is_eq), span.clone())),
                _ => None,
            }
        } else {
            None
        }
    } else {
        None
    };
    if let Some((v, span)) = folded {
        *e = Expr::Int {
            v,
            span,
            ty: Type::Int,
        };
    }
}

fn fold_init_item(it: &mut InitItem, volatiles: &HashSet<String>, so: bool) {
    match it {
        InitItem::Expr(e) => fold_expr(e, volatiles, so),
        InitItem::Designated {
            designator, value, ..
        } => {
            fold_designator(designator, volatiles, so);
            fold_expr(value, volatiles, so);
        },
    }
}

fn fold_designator(d: &mut Designator, volatiles: &HashSet<String>, so: bool) {
    match d {
        Designator::Index(e) => fold_expr(e, volatiles, so),
        Designator::Range(a, b) => {
            fold_expr(a, volatiles, so);
            fold_expr(b, volatiles, so);
        },
        Designator::Chain(list) => {
            for x in list {
                fold_designator(x, volatiles, so);
            }
        },
        Designator::Field(_) => {},
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::Severity;

    /// 解析 + 降级，返回 module 与第一个函数的 return 表达式（折叠后）。
    fn fold_src(src: &str, strict_overflow: bool) -> Expr {
        let (prog, diags) = crate::parser::parse(src.as_bytes(), "cmp-fold.c");
        assert!(!diags.iter().any(|d| d.sev == Severity::Error), "parse diagnostics: {diags:?}");
        let (mut module, _) = crate::ir::lower(prog).expect("lower");
        run(&mut module, strict_overflow);
        let prog = module.source();
        let f = prog
            .decls
            .iter()
            .find_map(|d| match d {
                Decl::Func(f) if f.name.name == "f" => Some(f),
                _ => None,
            })
            .expect("function f");
        let body = f.body.as_ref().expect("body");
        match &body.items[0] {
            BlockItem::Stmt(Stmt::Return { val: Some(e), .. }) => e.clone(),
            other => panic!("unexpected first item: {other:?}"),
        }
    }

    fn assert_int(e: &Expr, want: i128) {
        match e {
            Expr::Int { v, .. } => assert_eq!(*v, want),
            other => panic!("expected Int({want}), got {other:?}"),
        }
    }

    #[test]
    fn folds_swapped_comparison_inequality() {
        let e = fold_src("int f(int a, int b) { return (a < b) != (b > a); }", true);
        assert_int(&e, 0);
    }

    #[test]
    fn folds_additive_comparison_equivalence() {
        for src in [
            "int f(int a, int b) { return (a - 1 < b) != (a <= b); }",
            "int f(int a, int b) { return (a - 2 < b) != (a - 1 <= b); }",
            "int f(int a, int b) { return (a + -1 < b) != (a <= b); }",
            "int f(int a, int b) { return (a + 1 > b) != (a >= b); }",
            "int f(int a, int b) { return (a - 1 >= b) != (a > b); }",
            "int f(int a, int b) { return (a + 2 <= b) != (a + 1 < b); }",
        ] {
            assert_int(&fold_src(src, true), 0);
        }
    }

    #[test]
    fn equality_identity_holds_under_wrapping() {
        // 等值推理只用环公理，-fno-strict-overflow 下也必须成立。
        let e = fold_src("int f(int a, int b) { return (a + 1 == b) != (a == b - 1); }", false);
        assert_int(&e, 0);
    }

    #[test]
    fn keeps_non_equivalent_comparisons() {
        let e = fold_src("int f(int a, int b) { return (a < b) != (a > b); }", true);
        assert!(matches!(e, Expr::Binop { op: BinOp::Ne, .. }), "got {e:?}");
    }

    #[test]
    fn ordering_not_folded_without_strict_overflow() {
        let e = fold_src("int f(int a, int b) { return (a - 1 < b) != (a <= b); }", false);
        assert!(matches!(e, Expr::Binop { op: BinOp::Ne, .. }), "got {e:?}");
    }
}
