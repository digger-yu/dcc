// src/opt.rs - typed IR 优化 passes
// 第一性原理: 优化在 IR 层做数据流/常量传播，后端只负责指令选择。

use crate::ast::{BinOp, Expr, Program, Stmt, BlockItem, Decl};
use crate::const_eval;
use crate::ir::Module;

/// 按 -O 级别运行 IR 优化。
pub fn optimize(module: &mut Module, level: u8) {
    if level == 0 {
        return;
    }
    fold_constants(module);
    if level >= 2 {
        prune_dead_stores(module);
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
