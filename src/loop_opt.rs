//! 循环优化：不变式外提（LICM）、展开、SIMD 向量逐元素循环向量化。

use crate::ast::{BinOp, Block, BlockItem, Decl, Expr, Stmt, Type};
use crate::const_eval;
use crate::diagnostics::Span;
use crate::ir::Module;

/// `-O2+` LICM；`-O3` 额外展开与向量循环向量化。
pub fn run(module: &mut Module, level: u8) {
    if level < 2 {
        return;
    }
    let prog = module.source_mut();
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &mut f.body {
                licm_block(body);
                if level >= 3 {
                    unroll_block(body);
                    vectorize_block(body);
                }
            }
        }
    }
}

// ===== LICM =====

fn licm_block(block: &mut Block) {
    let mut hoisted = Vec::new();
    let mut kept = Vec::new();
    for item in block.items.drain(..) {
        match item {
            BlockItem::Stmt(Stmt::For {
                init,
                cond,
                step,
                mut body,
                span,
            }) => {
                let loop_var = init.as_deref().and_then(init_stmt_var);
                let mut hoisted_here = Vec::new();
                licm_in_stmt(body.as_mut(), &mut hoisted_here, loop_var.as_deref());
                hoisted.extend(hoisted_here);
                kept.push(BlockItem::Stmt(Stmt::For {
                    init,
                    cond,
                    step,
                    body,
                    span,
                }));
            }
            BlockItem::Stmt(Stmt::While {
                cond,
                mut body,
                span,
            }) => {
                let mut hoisted_here = Vec::new();
                licm_in_stmt(body.as_mut(), &mut hoisted_here, None);
                hoisted.extend(hoisted_here);
                kept.push(BlockItem::Stmt(Stmt::While { cond, body, span }));
            }
            BlockItem::Stmt(Stmt::Block { mut b, span }) => {
                licm_block(&mut b);
                kept.push(BlockItem::Stmt(Stmt::Block { b, span }));
            }
            other => kept.push(other),
        }
    }
    block.items = hoisted;
    block.items.extend(kept);
}

fn licm_in_stmt(body: &mut Stmt, hoisted: &mut Vec<BlockItem>, loop_var: Option<&str>) {
    match body {
        Stmt::Block { b, .. } => {
            licm_block(b);
            hoisted.extend(split_invariants(b, loop_var));
        }
        other => {
            let mut pseudo = Block {
                items: vec![BlockItem::Stmt(other.clone())],
                span: stmt_span(other),
            };
            hoisted.extend(split_invariants(&mut pseudo, loop_var));
            *other = match pseudo.items.into_iter().next() {
                Some(BlockItem::Stmt(s)) => s,
                _ => Stmt::Block {
                    b: Block {
                        items: vec![],
                        span: stmt_span(other),
                    },
                    span: stmt_span(other),
                },
            };
        }
    }
}

fn split_invariants(body: &mut Block, loop_var: Option<&str>) -> Vec<BlockItem> {
    let mut hoisted = Vec::new();
    let mut kept = Vec::new();
    for item in body.items.drain(..) {
        if is_invariant_item(&item, loop_var) {
            hoisted.push(item);
        } else {
            kept.push(item);
        }
    }
    body.items = kept;
    hoisted
}

fn is_invariant_item(item: &BlockItem, loop_var: Option<&str>) -> bool {
    match item {
        BlockItem::Decl(Decl::Var(v)) => {
            v.init
                .as_ref()
                .is_none_or(|e| !expr_mentions(e, loop_var))
        }
        BlockItem::Stmt(Stmt::Expr { e, .. }) => match e {
            Expr::Assign { l, r, .. } => !expr_mentions(l, loop_var) && !expr_mentions(r, loop_var),
            _ => !expr_mentions(e, loop_var),
        },
        _ => false,
    }
}

fn expr_mentions(e: &Expr, name: Option<&str>) -> bool {
    let Some(name) = name else { return false };
    match e {
        Expr::Ident { name: id, .. } => id.name == name,
        Expr::Binop { l, r, .. } => expr_mentions(l, Some(name)) || expr_mentions(r, Some(name)),
        Expr::Unary { e: inner, .. } => expr_mentions(inner, Some(name)),
        Expr::Assign { l, r, .. } => expr_mentions(l, Some(name)) || expr_mentions(r, Some(name)),
        Expr::Index { arr, idx, .. } => {
            expr_mentions(arr, Some(name)) || expr_mentions(idx, Some(name))
        }
        Expr::Cast { e: inner, .. } => expr_mentions(inner, Some(name)),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            expr_mentions(cond, Some(name))
                || expr_mentions(then_, Some(name))
                || expr_mentions(else_, Some(name))
        }
        _ => false,
    }
}

// ===== 循环展开 =====

fn unroll_block(block: &mut Block) {
    let mut out = Vec::new();
    for item in block.items.drain(..) {
        match item {
            BlockItem::Stmt(Stmt::For {
                init,
                cond,
                step,
                mut body,
                span,
            }) => {
                if let Some(unrolled) = try_unroll_for_stmt(
                    init.as_deref(),
                    cond.as_ref(),
                    step.as_ref(),
                    body.as_ref(),
                ) {
                    out.extend(unrolled);
                    continue;
                }
                unroll_in_stmt(body.as_mut());
                out.push(BlockItem::Stmt(Stmt::For {
                    init,
                    cond,
                    step,
                    body,
                    span,
                }));
            }
            BlockItem::Stmt(Stmt::Block { mut b, span }) => {
                unroll_block(&mut b);
                out.push(BlockItem::Stmt(Stmt::Block { b, span }));
            }
            other => out.push(other),
        }
    }
    block.items = out;
}

fn unroll_in_stmt(body: &mut Stmt) {
    if let Stmt::Block { b, .. } = body {
        unroll_block(b);
    }
}

fn try_unroll_for_stmt(
    init: Option<&Stmt>,
    cond: Option<&Expr>,
    step: Option<&Expr>,
    body: &Stmt,
) -> Option<Vec<BlockItem>> {
    let (ivar, trip) = parse_counted_for(init, cond, step)?;
    let body_items = loop_body_items(body)?;
    if trip == 0 || trip > 4 || body_has_control_items(&body_items) {
        return None;
    }
    let mut items = Vec::new();
    if let Some(i) = init {
        match i {
            Stmt::Block { b, .. } => items.extend(b.items.clone()),
            other => items.push(BlockItem::Stmt(other.clone())),
        }
    }
    for k in 0..trip {
        for bi in &body_items {
            items.push(subst_induction(bi, &ivar, k as i128));
        }
    }
    Some(items)
}

fn loop_body_items(body: &Stmt) -> Option<Vec<BlockItem>> {
    match body {
        Stmt::Block { b, .. } => Some(b.items.clone()),
        other => Some(vec![BlockItem::Stmt(other.clone())]),
    }
}

fn body_has_control_items(items: &[BlockItem]) -> bool {
    items.iter().any(|item| match item {
        BlockItem::Stmt(Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Return { .. }) => true,
        BlockItem::Stmt(Stmt::Block { b, .. }) => body_has_control(b),
        _ => false,
    })
}

fn body_has_control(body: &Block) -> bool {
    body.items.iter().any(|item| match item {
        BlockItem::Stmt(Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Return { .. }) => true,
        BlockItem::Stmt(Stmt::Block { b, .. }) => body_has_control(b),
        _ => false,
    })
}

fn subst_induction(item: &BlockItem, ivar: &str, value: i128) -> BlockItem {
    let mut cloned = item.clone();
    rewrite_ivars(&mut cloned, ivar, value);
    cloned
}

fn rewrite_ivars(item: &mut BlockItem, ivar: &str, value: i128) {
    match item {
        BlockItem::Decl(Decl::Var(v)) => {
            if let Some(init) = &mut v.init {
                rewrite_expr_ivars(init, ivar, value);
            }
        }
        BlockItem::Stmt(s) => rewrite_stmt_ivars(s, ivar, value),
        _ => {}
    }
}

fn rewrite_stmt_ivars(s: &mut Stmt, ivar: &str, value: i128) {
    match s {
        Stmt::Expr { e, .. } => rewrite_expr_ivars(e, ivar, value),
        Stmt::Return { val: Some(e), .. } => rewrite_expr_ivars(e, ivar, value),
        Stmt::Block { b, .. } => {
            for sub in &mut b.items {
                rewrite_ivars(sub, ivar, value);
            }
        }
        _ => {}
    }
}

fn rewrite_expr_ivars(e: &mut Expr, ivar: &str, value: i128) {
    match e {
        Expr::Ident { name, ty, span } if name.name == ivar => {
            *e = Expr::Int {
                v: value,
                span: span.clone(),
                ty: ty.clone(),
            };
        }
        Expr::Binop { l, r, .. } => {
            rewrite_expr_ivars(l, ivar, value);
            rewrite_expr_ivars(r, ivar, value);
        }
        Expr::Unary { e: inner, .. } => rewrite_expr_ivars(inner, ivar, value),
        Expr::Assign { l, r, .. } => {
            rewrite_expr_ivars(l, ivar, value);
            rewrite_expr_ivars(r, ivar, value);
        }
        Expr::Index { arr, idx, .. } => {
            rewrite_expr_ivars(arr, ivar, value);
            rewrite_expr_ivars(idx, ivar, value);
        }
        _ => {}
    }
}

fn parse_counted_for(
    init: Option<&Stmt>,
    cond: Option<&Expr>,
    step: Option<&Expr>,
) -> Option<(String, usize)> {
    let ivar = init_stmt_var(init?)?;
    let limit = parse_lt_limit(cond?, &ivar)?;
    if !is_inc_step(step?, &ivar) {
        return None;
    }
    Some((ivar, limit))
}

fn init_stmt_var(init: &Stmt) -> Option<String> {
    match init {
        Stmt::Expr {
            e: Expr::Assign { l, r, .. },
            ..
        } => {
            if let Expr::Ident { name, .. } = l.as_ref() {
                if matches!(r.as_ref(), Expr::Int { v: 0, .. }) {
                    return Some(name.name.clone());
                }
            }
            None
        }
        Stmt::Block { b, .. } => {
            if b.items.len() == 1 {
                if let BlockItem::Decl(Decl::Var(v)) = &b.items[0] {
                    if matches!(v.init.as_ref(), Some(Expr::Int { v: 0, .. })) {
                        return Some(v.name.name.clone());
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn parse_lt_limit(cond: &Expr, ivar: &str) -> Option<usize> {
    match cond {
        Expr::Binop {
            op: BinOp::Lt,
            l,
            r,
            ..
        } => {
            if let Expr::Ident { name, .. } = l.as_ref() {
                if name.name == ivar {
                    if let Ok(v) = const_eval::eval(r) {
                        if (0..=4).contains(&v) {
                            return Some(v as usize);
                        }
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn is_inc_step(step: &Expr, ivar: &str) -> bool {
    match step {
        Expr::PostInc { e, .. } | Expr::PreInc { e, .. } => {
            matches!(e.as_ref(), Expr::Ident { name, .. } if name.name == ivar)
        }
        Expr::Assign { l, r, .. } => {
            if !matches!(l.as_ref(), Expr::Ident { name, .. } if name.name == ivar) {
                return false;
            }
            match r.as_ref() {
                Expr::Binop {
                    op: BinOp::Add,
                    l: li,
                    r: rv,
                    ..
                } => {
                    matches!(li.as_ref(), Expr::Ident { name, .. } if name.name == ivar)
                        && matches!(rv.as_ref(), Expr::Int { v: 1, .. })
                }
                _ => false,
            }
        }
        _ => false,
    }
}

// ===== 向量循环向量化 =====

fn vectorize_block(block: &mut Block) {
    let mut out = Vec::new();
    for item in block.items.drain(..) {
        match item {
            BlockItem::Stmt(Stmt::For {
                init,
                cond,
                step,
                mut body,
                span,
            }) => {
                if let Some(vec_stmt) = try_vectorize_for_stmt(
                    init.as_deref(),
                    cond.as_ref(),
                    step.as_ref(),
                    body.as_ref(),
                ) {
                    out.push(vec_stmt);
                } else {
                    vectorize_in_stmt(body.as_mut());
                    out.push(BlockItem::Stmt(Stmt::For {
                        init,
                        cond,
                        step,
                        body,
                        span,
                    }));
                }
            }
            BlockItem::Stmt(Stmt::Block { mut b, span }) => {
                vectorize_block(&mut b);
                out.push(BlockItem::Stmt(Stmt::Block { b, span }));
            }
            other => out.push(other),
        }
    }
    block.items = out;
}

fn vectorize_in_stmt(body: &mut Stmt) {
    if let Stmt::Block { b, .. } = body {
        vectorize_block(b);
    }
}

fn try_vectorize_for_stmt(
    init: Option<&Stmt>,
    cond: Option<&Expr>,
    step: Option<&Expr>,
    body: &Stmt,
) -> Option<BlockItem> {
    let stmt = loop_body_single_stmt(body)?;
    let block_items = loop_body_items(body)?;
    let pseudo = Block {
        items: block_items,
        span: stmt_span(body),
    };
    try_vectorize_for(init, cond, step, &pseudo, stmt)
}

fn loop_body_single_stmt(body: &Stmt) -> Option<&Stmt> {
    match body {
        Stmt::Block { b, .. } if b.items.len() == 1 => match &b.items[0] {
            BlockItem::Stmt(s) => Some(s),
            _ => None,
        },
        s @ (Stmt::Expr { .. } | Stmt::Return { .. }) => Some(s),
        _ => None,
    }
}

fn try_vectorize_for(
    init: Option<&Stmt>,
    cond: Option<&Expr>,
    step: Option<&Expr>,
    _body: &Block,
    stmt: &Stmt,
) -> Option<BlockItem> {
    let (ivar, n) = parse_counted_for(init, cond, step)?;
    let (dst, src_l, src_r, op) = parse_vector_elem_assign(stmt, &ivar)?;
    let (elem_ty, count) = match dst.1 {
        Type::Vector(ref e, c) if c == n => (e.as_ref().clone(), c),
        _ => return None,
    };
    if count != n {
        return None;
    }
    let vec_ty = Type::Vector(Box::new(elem_ty), count);
    let sp = stmt_span(stmt);
    Some(BlockItem::Stmt(Stmt::Expr {
        e: Expr::Assign {
            op: BinOp::Add,
            l: Box::new(Expr::Ident {
                name: dst.0,
                span: sp.clone(),
                ty: vec_ty.clone(),
            }),
            r: Box::new(Expr::Binop {
                op,
                l: Box::new(Expr::Ident {
                    name: src_l.0,
                    span: sp.clone(),
                    ty: vec_ty.clone(),
                }),
                r: Box::new(Expr::Ident {
                    name: src_r.0,
                    span: sp.clone(),
                    ty: vec_ty.clone(),
                }),
                span: sp.clone(),
                ty: vec_ty.clone(),
            }),
            span: sp.clone(),
            ty: vec_ty,
        },
        span: sp,
    }))
}

fn parse_vector_elem_assign(
    s: &Stmt,
    ivar: &str,
) -> Option<(NamedVector, NamedVector, NamedVector, BinOp)> {
    let e = match s {
        Stmt::Expr { e, .. } => e,
        _ => return None,
    };
    let (l, r) = match e {
        Expr::Assign { l, r, .. } => (l.as_ref(), r.as_ref()),
        _ => return None,
    };
    let dst = index_vector_ident(l, ivar)?;
    let (op, src_l, src_r) = match r {
        Expr::Binop { op, l, r, .. } => (
            *op,
            index_vector_ident(l, ivar)?,
            index_vector_ident(r, ivar)?,
        ),
        _ => return None,
    };
    if dst.1 != src_l.1 || dst.1 != src_r.1 {
        return None;
    }
    Some((dst, src_l, src_r, op))
}

type NamedVector = (crate::ast::Ident, Type);

fn index_vector_ident(e: &Expr, ivar: &str) -> Option<NamedVector> {
    match e {
        Expr::Index { arr, idx, .. } => {
            let arr = arr.as_ref();
            let idx = idx.as_ref();
            match (arr, idx) {
                (
                    Expr::Ident {
                        name,
                        ty: Type::Vector(elem, n),
                        ..
                    },
                    Expr::Ident { name: idx_name, .. },
                ) if idx_name.name == ivar => Some((name.clone(), Type::Vector(elem.clone(), *n))),
                _ => None,
            }
        }
        _ => None,
    }
}

fn stmt_span(s: &Stmt) -> Span {
    match s {
        Stmt::Expr { span, .. }
        | Stmt::Return { span, .. }
        | Stmt::If { span, .. }
        | Stmt::While { span, .. }
        | Stmt::Do { span, .. }
        | Stmt::For { span, .. }
        | Stmt::Block { span, .. } => span.clone(),
        _ => Span::point("loop", 1, 1),
    }
}
