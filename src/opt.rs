// src/opt.rs - typed IR 优化 passes
// 第一性原理: 优化在 IR 层做数据流/常量传播，后端只负责指令选择。

use crate::ast::{BinOp, Expr, Program, Stmt, BlockItem, Decl, FuncDecl, Block};
use crate::const_eval;
use crate::diagnostics::Span;
use crate::ir::Module;

/// 按 -O 级别运行 IR 优化。
pub fn optimize(module: &mut Module, level: u8) {
    if level == 0 {
        return;
    }
    fold_constants(module);
    if level >= 2 {
        prune_dead_stores(module);
        fold_constant_branches(module);
        eliminate_unreachable(module);
    }
    if level >= 3 {
        simple_inline(module);
    }
}

fn fold_constants(module: &mut Module) {
    let prog = module.source_mut();
    fold_program(prog);
}

fn fold_program(prog: &mut Program) {
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &mut f.body {
                fold_block(&mut body.items);
            }
        }
    }
}

fn fold_block(items: &mut Vec<BlockItem>) {
    for item in items.iter_mut() {
        match item {
            BlockItem::Stmt(s) => fold_stmt(s),
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(init) = &mut v.init {
                    *init = fold_expr(init.clone());
                }
            }
            _ => {}
        }
    }
}

fn fold_stmt(s: &mut Stmt) {
    match s {
        Stmt::If { cond, then_, else_, .. } => {
            *cond = fold_expr(cond.clone());
            fold_stmt(then_);
            if let Some(e) = else_ {
                fold_stmt(e);
            }
        }
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            *cond = fold_expr(cond.clone());
            fold_stmt(body);
        }
        Stmt::For { init, cond, step, body, .. } => {
            if let Some(c) = cond {
                *c = fold_expr(c.clone());
            }
            if let Some(st) = step {
                *st = fold_expr(st.clone());
            }
            if let Some(i) = init {
                fold_stmt(i);
            }
            fold_stmt(body);
        }
        Stmt::Return { val, .. } => {
            if let Some(e) = val {
                *e = fold_expr(e.clone());
            }
        }
        Stmt::Expr { e, .. } => *e = fold_expr(e.clone()),
        Stmt::Block { b, .. } => fold_block(&mut b.items),
        _ => {}
    }
}

fn fold_expr(e: Expr) -> Expr {
    let e = match e {
        Expr::Binop { op, l, r, span, ty } => {
            let l = Box::new(fold_expr(*l));
            let r = Box::new(fold_expr(*r));
            if let (Expr::Int { v: lv, .. }, Expr::Int { v: rv, .. }) = (l.as_ref(), r.as_ref()) {
                if let Some(v) = eval_binop(op, *lv, *rv) {
                    return Expr::Int { v, span, ty };
                }
            }
            Expr::Binop { op, l, r, span, ty }
        }
        Expr::Unary { op, e, span, ty } => {
            let e = Box::new(fold_expr(*e));
            if let Expr::Int { v, .. } = e.as_ref() {
                if let Some(v) = eval_unary(op, *v) {
                    return Expr::Int { v, span, ty };
                }
            }
            Expr::Unary { op, e, span, ty }
        }
        other => other,
    };
    if let Ok(v) = const_eval::eval(&e) {
        if let Expr::Int { span, ty, .. } = &e {
            return Expr::Int { v, span: span.clone(), ty: ty.clone() };
        }
    }
    e
}

fn eval_binop(op: BinOp, l: i128, r: i128) -> Option<i128> {
    Some(match op {
        BinOp::Add => l.checked_add(r)?,
        BinOp::Sub => l.checked_sub(r)?,
        BinOp::Mul => l.checked_mul(r)?,
        BinOp::Div => if r == 0 { return None; } else { l / r },
        BinOp::Mod => if r == 0 { return None; } else { l % r },
        BinOp::And => l & r,
        BinOp::Or => l | r,
        BinOp::Xor => l ^ r,
        BinOp::Shl => l << (r as u32),
        BinOp::Shr => l >> (r as u32),
        _ => return None,
    })
}

fn eval_unary(op: crate::ast::UnOp, v: i128) -> Option<i128> {
    Some(match op {
        crate::ast::UnOp::Neg => v.checked_neg()?,
        crate::ast::UnOp::Not => if v == 0 { 1 } else { 0 },
        crate::ast::UnOp::BitNot => !v,
        _ => return None,
    })
}

fn prune_dead_stores(module: &mut Module) {
    let prog = module.source_mut();
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &mut f.body {
                prune_block(&mut body.items);
            }
        }
    }
}

fn prune_block(items: &mut Vec<BlockItem>) {
    items.retain(|item| !matches!(item, BlockItem::Stmt(Stmt::Expr { e, .. }) if is_side_effect_free(e)));
    for item in items.iter_mut() {
        match item {
            BlockItem::Stmt(s) => prune_stmt(s),
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(body) = &mut f.body {
                    prune_block(&mut body.items);
                }
            }
            _ => {}
        }
    }
}

fn prune_stmt(s: &mut Stmt) {
    match s {
        Stmt::If { then_, else_, .. } => {
            prune_stmt(then_);
            if let Some(e) = else_ {
                prune_stmt(e);
            }
        }
        Stmt::While { body, .. } | Stmt::Do { body, .. } => prune_stmt(body),
        Stmt::For { init, body, .. } => {
            if let Some(i) = init {
                prune_stmt(i);
            }
            prune_stmt(body);
        }
        Stmt::Block { b, .. } => prune_block(&mut b.items),
        _ => {}
    }
}

/// -O2: 常量条件分支折叠（DCE 的一种）。
fn fold_constant_branches(module: &mut Module) {
    let prog = module.source_mut();
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &mut f.body {
                fold_branches_block(&mut body.items);
            }
        }
    }
}

fn fold_branches_block(items: &mut Vec<BlockItem>) {
    let mut i = 0;
    while i < items.len() {
        let fold_if = matches!(
            items.get(i),
            Some(BlockItem::Stmt(Stmt::If { cond, .. })) if const_eval::eval(
                match items.get(i) {
                    Some(BlockItem::Stmt(Stmt::If { cond, .. })) => cond,
                    _ => unreachable!(),
                }
            ).is_ok()
        );
        if fold_if {
            let item = items.remove(i);
            if let BlockItem::Stmt(Stmt::If { cond, then_, else_, span, .. }) = item {
                let v = const_eval::eval(&cond).unwrap_or(0);
                let taken = if v != 0 {
                    *then_
                } else {
                    else_.map(|e| *e).unwrap_or(Stmt::Block {
                        b: Block { items: vec![], span: span.clone() },
                        span: span.clone(),
                    })
                };
                match taken {
                    Stmt::Block { mut b, .. } => {
                        fold_branches_block(&mut b.items);
                        for it in b.items.into_iter().rev() {
                            items.insert(i, it);
                        }
                        continue;
                    }
                    other => items.insert(i, BlockItem::Stmt(other)),
                }
            }
        } else if let BlockItem::Stmt(s) = &mut items[i] {
            fold_branches_stmt(s);
        }
        i += 1;
    }
}

fn fold_branches_stmt(s: &mut Stmt) {
    match s {
        Stmt::If { cond, then_, else_, .. } => {
            *cond = fold_expr(cond.clone());
            fold_branches_stmt(then_);
            if let Some(e) = else_ {
                fold_branches_stmt(e);
            }
        }
        Stmt::While { cond, body, span, .. } => {
            *cond = fold_expr(cond.clone());
            if let Ok(v) = const_eval::eval(cond) {
                if v == 0 {
                    *s = Stmt::Block {
                        b: Block { items: vec![], span: span.clone() },
                        span: span.clone(),
                    };
                    return;
                }
            }
            fold_branches_stmt(body);
        }
        Stmt::Block { b, .. } => fold_branches_block(&mut b.items),
        Stmt::For { init, cond, body, span, .. } => {
            if let Some(c) = cond {
                *c = fold_expr(c.clone());
                if let Ok(v) = const_eval::eval(c) {
                    if v == 0 {
                        if let Some(i) = init.take() {
                            *s = Stmt::Expr { e: stmt_to_void_expr(*i), span: span.clone() };
                        } else {
                            *s = Stmt::Block {
                                b: Block { items: vec![], span: span.clone() },
                                span: span.clone(),
                            };
                        }
                        return;
                    }
                }
            }
            if let Some(i) = init {
                fold_branches_stmt(i);
            }
            fold_branches_stmt(body);
        }
        _ => {}
    }
}

fn items_clear_for_stmt(s: &mut Stmt) {
    let span = s.span().clone();
    *s = Stmt::Block {
        b: Block { items: vec![], span: span.clone() },
        span,
    };
}

fn stmt_to_void_expr(s: Stmt) -> Expr {
    match s {
        Stmt::Expr { e, .. } => e,
        Stmt::Block { .. } => Expr::Int { v: 0, span: Span::point("", 0, 0), ty: crate::ast::Type::Int },
        _ => Expr::Int { v: 0, span: Span::point("", 0, 0), ty: crate::ast::Type::Int },
    }
}

/// -O2: return 后不可达语句删除。
fn eliminate_unreachable(module: &mut Module) {
    let prog = module.source_mut();
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &mut f.body {
                dce_block(&mut body.items);
            }
        }
    }
}

fn dce_block(items: &mut Vec<BlockItem>) {
    let mut stopped = false;
    items.retain(|item| {
        if stopped {
            return false;
        }
        if let BlockItem::Stmt(Stmt::Return { .. }) = item {
            stopped = true;
        }
        true
    });
    for item in items.iter_mut() {
        if let BlockItem::Stmt(s) = item {
            dce_stmt(s);
        }
    }
}

fn dce_stmt(s: &mut Stmt) {
    match s {
        Stmt::If { then_, else_, .. } => {
            dce_block_in_stmt(then_);
            if let Some(e) = else_ {
                dce_block_in_stmt(e);
            }
        }
        Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
            dce_block_in_stmt(body);
        }
        Stmt::Block { b, .. } => dce_block(&mut b.items),
        _ => {}
    }
}

fn dce_block_in_stmt(s: &mut Stmt) {
    if let Stmt::Block { b, .. } = s {
        dce_block(&mut b.items);
    } else {
        dce_stmt(s);
    }
}

/// -O3: 单调用小函数内联（仅 static 且无递归）。
fn simple_inline(module: &mut Module) {
    let prog = module.source_mut();
    let candidates: Vec<(String, FuncDecl)> = prog
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::Func(f) if f.body.is_some() && f.is_static && f.params.is_empty() => {
                let lines = f.body.as_ref().map(count_block_items).unwrap_or(0);
                if lines <= 4 {
                    Some((f.name.name.clone(), f.clone()))
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();
    if candidates.is_empty() {
        return;
    }
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &mut f.body {
                inline_calls_in_block(body, &candidates);
            }
        }
    }
}

fn count_block_items(b: &Block) -> usize {
    b.items.len()
}

fn inline_calls_in_block(body: &mut Block, candidates: &[(String, FuncDecl)]) {
    for item in &mut body.items {
        match item {
            BlockItem::Stmt(s) => inline_calls_in_stmt(s, candidates),
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(b) = &mut f.body {
                    inline_calls_in_block(b, candidates);
                }
            }
            _ => {}
        }
    }
}

fn inline_calls_in_stmt(s: &mut Stmt, candidates: &[(String, FuncDecl)]) {
    match s {
        Stmt::Return { val: Some(e), .. } => {
            *e = inline_expr(e.clone(), candidates);
        }
        Stmt::Expr { e, .. } => *e = inline_expr(e.clone(), candidates),
        Stmt::If { cond, then_, else_, .. } => {
            *cond = inline_expr(cond.clone(), candidates);
            inline_calls_in_stmt(then_, candidates);
            if let Some(e) = else_ {
                inline_calls_in_stmt(e, candidates);
            }
        }
        Stmt::Block { b, .. } => inline_calls_in_block(b, candidates),
        Stmt::While { body, .. } | Stmt::Do { body, .. } => inline_calls_in_stmt(body, candidates),
        Stmt::For { init, cond, step, body, .. } => {
            if let Some(c) = cond {
                *c = inline_expr(c.clone(), candidates);
            }
            if let Some(st) = step {
                *st = inline_expr(st.clone(), candidates);
            }
            if let Some(i) = init {
                inline_calls_in_stmt(i, candidates);
            }
            inline_calls_in_stmt(body, candidates);
        }
        _ => {}
    }
}

fn inline_expr(e: Expr, candidates: &[(String, FuncDecl)]) -> Expr {
    let e = match e {
        Expr::Call { callee, args, span, ty } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                if args.is_empty() {
                    if let Some((_, f)) = candidates.iter().find(|(n, _)| n == &name.name) {
                        if let Some(body) = &f.body {
                            if let Some(BlockItem::Stmt(Stmt::Return { val: Some(ret), .. })) =
                                body.items.last()
                            {
                                return inline_expr(ret.clone(), candidates);
                            }
                        }
                    }
                }
            }
            Expr::Call { callee, args, span, ty }
        }
        Expr::Binop { op, l, r, span, ty } => Expr::Binop {
            op,
            l: Box::new(inline_expr(*l, candidates)),
            r: Box::new(inline_expr(*r, candidates)),
            span,
            ty,
        },
        Expr::Unary { op, e, span, ty } => Expr::Unary {
            op,
            e: Box::new(inline_expr(*e, candidates)),
            span,
            ty,
        },
        other => other,
    };
    fold_expr(e)
}

fn is_side_effect_free(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Int { .. }
            | Expr::SizeofTy { .. }
            | Expr::SizeofExpr { .. }
            | Expr::AlignofTy { .. }
            | Expr::AlignofExpr { .. }
            | Expr::Offsetof { .. }
    )
}

trait StmtSpan {
    fn span(&self) -> &Span;
}

impl StmtSpan for Stmt {
    fn span(&self) -> &Span {
        match self {
            Stmt::If { span, .. }
            | Stmt::While { span, .. }
            | Stmt::Do { span, .. }
            | Stmt::For { span, .. }
            | Stmt::Return { span, .. }
            | Stmt::Expr { span, .. }
            | Stmt::Block { span, .. }
            | Stmt::Break { span, .. }
            | Stmt::Continue { span, .. }
            | Stmt::Goto { span, .. }
            | Stmt::GotoIndirect { span, .. }
            | Stmt::Label { span, .. }
            | Stmt::Switch { span, .. }
            | Stmt::AsmGoto { span, .. } => span,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Ident, Type};
    use crate::diagnostics::Span;
    use crate::ir;

    fn int_ty() -> Type {
        Type::Int
    }

    #[test]
    fn o2_folds_constant_if() {
        let prog = Program {
            decls: vec![            Decl::Func(FuncDecl {
                ret_ty: int_ty(),
                name: Ident { name: "main".into(), span: Span::point("t.c", 1, 1) },
                params: vec![],
                body: Some(Block {
                    items: vec![BlockItem::Stmt(Stmt::If {
                        cond: Expr::Int { v: 1, span: Span::point("t.c", 2, 1), ty: int_ty() },
                        then_: Box::new(Stmt::Return {
                            val: Some(Expr::Int { v: 7, span: Span::point("t.c", 3, 1), ty: int_ty() }),
                            span: Span::point("t.c", 3, 1),
                        }),
                        else_: None,
                        span: Span::point("t.c", 2, 1),
                    })],
                    span: Span::point("t.c", 1, 1),
                }),
                span: Span::point("t.c", 1, 1),
                is_static: false,
                is_extern: false,
                is_variadic: false,
                is_weak: false,
                is_constructor: false,
                is_destructor: false,
                is_nested: false,
                is_cold: false,
                section: None,
                alignment: None,
                visibility: None,
                alias_name: None,
                upvars: vec![],
                is_unused: false,
                is_noinline: false,
                is_always_inline: false,
            })],
        };
        let mut module = ir::lower(prog).expect("lower");
        optimize(&mut module, 2);
        let body = &module.source().decls[0];
        if let Decl::Func(f) = body {
            assert_eq!(f.body.as_ref().unwrap().items.len(), 1);
            assert!(matches!(
                f.body.as_ref().unwrap().items[0],
                BlockItem::Stmt(Stmt::Return { .. })
            ));
        }
    }
}
