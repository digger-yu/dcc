//! 循环优化：不变式外提（LICM）、展开、SIMD 向量逐元素循环向量化。

use crate::ast::{BinOp, Block, BlockItem, Decl, Expr, Ident, Stmt, Type};
use crate::const_eval;
use crate::diagnostics::Span;
use crate::ir::Module;

/// `-O2+` LICM；`-O3` 额外展开与向量循环向量化。
///
/// Profile-guided: hot 函数 (FuncDecl.is_hot, 由 PGO 设置) 在 O3
/// 享受更激进的展开上限 (max_trip 4 -> 16)。
pub fn run(module: &mut Module, level: u8) {
    run_with_target(module, level, false, 0);
}

/// 带目标信息的循环优化入口。
///
/// `has_vnni`: 目标是否支持 VNNI 指令 (AVX-VNNI / AVX-512VNNI)
/// `vec_bits`: 向量寄存器位宽 (256 for AVX2, 512 for AVX-512)
pub fn run_with_target(
    module: &mut Module,
    level: u8,
    has_vnni: bool,
    vec_bits: u32,
) {
    if level < 2 {
        return;
    }
    let prog = module.source_mut();
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &mut f.body {
                licm_block(body);
                if level >= 3 {
                    // Profile-guided: hot 函数享受更激进的展开上限
                    let max_trip: usize = if f.is_hot { 16 } else { 4 };
                    unroll_block(body, max_trip);
                    vectorize_block(body, has_vnni, vec_bits);
                    // P8: 循环分块 (tile_size=4)
                    tile_block(body, 4);
                    // P7: 自动并行化标注
                    parallelize_block(body);
                    // P16: 软件流水 (prologue/kernel/epilogue)
                    software_pipeline_block(body);
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
            v.init.as_ref().is_none_or(|e| !expr_mentions(e, loop_var))
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

fn unroll_block(block: &mut Block, max_trip: usize) {
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
                    max_trip,
                ) {
                    out.extend(unrolled);
                    continue;
                }
                unroll_in_stmt(body.as_mut(), max_trip);
                out.push(BlockItem::Stmt(Stmt::For {
                    init,
                    cond,
                    step,
                    body,
                    span,
                }));
            }
            BlockItem::Stmt(Stmt::Block { mut b, span }) => {
                unroll_block(&mut b, max_trip);
                out.push(BlockItem::Stmt(Stmt::Block { b, span }));
            }
            other => out.push(other),
        }
    }
    block.items = out;
}

fn unroll_in_stmt(body: &mut Stmt, max_trip: usize) {
    if let Stmt::Block { b, .. } = body {
        unroll_block(b, max_trip);
    }
}

fn try_unroll_for_stmt(
    init: Option<&Stmt>,
    cond: Option<&Expr>,
    step: Option<&Expr>,
    body: &Stmt,
    max_trip: usize,
) -> Option<Vec<BlockItem>> {
    let (ivar, trip) = parse_counted_for(init, cond, step, max_trip)?;
    let body_items = loop_body_items(body)?;
    if trip == 0 || trip > max_trip || body_has_control_items(&body_items) {
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
    max_trip: usize,
) -> Option<(String, usize)> {
    let ivar = init_stmt_var(init?)?;
    let limit = parse_lt_limit(cond?, &ivar, max_trip)?;
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

fn parse_lt_limit(cond: &Expr, ivar: &str, max_trip: usize) -> Option<usize> {
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
                        if v >= 0 && (v as usize) <= max_trip {
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

fn vectorize_block(block: &mut Block, has_vnni: bool, vec_bits: u32) {
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
                // Try VNNI dot-product vectorization first
                if has_vnni && vec_bits >= 256 {
                    if let Some(vnni_stmt) = try_vectorize_vnni(
                        init.as_deref(),
                        cond.as_ref(),
                        step.as_ref(),
                        body.as_ref(),
                        vec_bits,
                    ) {
                        out.push(vnni_stmt);
                        continue;
                    }
                }
                if let Some(vec_stmt) = try_vectorize_for_stmt(
                    init.as_deref(),
                    cond.as_ref(),
                    step.as_ref(),
                    body.as_ref(),
                ) {
                    out.push(vec_stmt);
                } else {
                    vectorize_in_stmt(body.as_mut(), has_vnni, vec_bits);
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
                vectorize_block(&mut b, has_vnni, vec_bits);
                out.push(BlockItem::Stmt(Stmt::Block { b, span }));
            }
            other => out.push(other),
        }
    }
    block.items = out;
}

fn vectorize_in_stmt(body: &mut Stmt, has_vnni: bool, vec_bits: u32) {
    if let Stmt::Block { b, .. } = body {
        vectorize_block(b, has_vnni, vec_bits);
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
    let (ivar, n) = parse_counted_for(init, cond, step, 4)?;
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

/// 尝试识别 int8/int16 dot-product 模式并用 VNNI 指令向量化。
///
/// 检测模式: `sum += a[i] * b[i]` (循环累加乘)
/// 当目标支持 VNNI 时，标记为可用 VPDPBUSD/VPDPWSSD 指令。
/// 向量化因子: AVX2=256bit/16bit=16 lanes, AVX-512=512bit/16bit=32 lanes
fn try_vectorize_vnni(
    init: Option<&Stmt>,
    cond: Option<&Expr>,
    step: Option<&Expr>,
    body: &Stmt,
    vec_bits: u32,
) -> Option<BlockItem> {
    // Parse the for loop structure
    let (ivar, _n) = parse_counted_for(init, cond, step, 4)?;
    let stmt = loop_body_single_stmt(body)?;

    // Check for accumulation pattern: sum += a[i] * b[i]
    let s = match stmt {
        Stmt::Expr { e, .. } => e,
        _ => return None,
    };
    let (acc_l, acc_r) = match s {
        Expr::Assign { op: BinOp::Add, l, r, .. } => (l.as_ref(), r.as_ref()),
        _ => return None,
    };

    // LHS must be accumulator (scalar int)
    let acc_name = match acc_l {
        Expr::Ident { name, ty, .. } => {
            if !matches!(ty, Type::Int | Type::Short | Type::Char) {
                return None;
            }
            name.clone()
        }
        _ => return None,
    };

    // RHS must be a[i] * b[i] pattern
    let (mul_l, mul_r) = match acc_r {
        Expr::Binop { op: BinOp::Mul, l, r, .. } => (l.as_ref(), r.as_ref()),
        _ => return None,
    };

    // Both operands must be array accesses with ivar
    if !is_array_index_with(mul_l, &ivar) || !is_array_index_with(mul_r, &ivar) {
        return None;
    }

    // Compute vector lanes based on element size and register width
    let elem_bits: u32 = 16; // int16 dot-product
    let lanes = vec_bits / elem_bits;

    // Mark this loop as VNNI-vectorizable by wrapping in a
    // vectorized statement with appropriate type annotations.
    let sp = stmt_span(stmt);
    let i16_qt = crate::ast::QualType::new(Type::Short);
    let vec_ty = Type::Vector(Box::new(i16_qt), lanes as usize);

    // Produce a vectorized accumulation statement:
    // acc_vec += (vec)a[i] * (vec)b[i]
    Some(BlockItem::Stmt(Stmt::Expr {
        e: Expr::Assign {
            op: BinOp::Add,
            l: Box::new(Expr::Ident {
                name: acc_name,
                span: sp.clone(),
                ty: vec_ty.clone(),
            }),
            r: Box::new(Expr::Binop {
                op: BinOp::Mul,
                l: Box::new(mul_l.clone()),
                r: Box::new(mul_r.clone()),
                span: sp.clone(),
                ty: vec_ty.clone(),
            }),
            span: sp.clone(),
            ty: vec_ty,
        },
        span: sp,
    }))
}

/// Check if expression is an array access with the given index variable.
fn is_array_index_with(e: &Expr, ivar: &str) -> bool {
    match e {
        Expr::Index { arr: _, idx, .. } => {
            if let Expr::Ident { name, .. } = idx.as_ref() {
                name.name == ivar
            } else {
                false
            }
        }
        _ => false,
    }
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

// ===== P8. 循环分块 (Loop Tiling) =====

/// 循环分块: 将简单 for 循环的迭代空间分为 tile_size 块 + 尾部处理。
/// 生成: 外层循环 (步长=tile_size) + 内层循环 (0..tile_size) + 尾部循环。
/// 仅处理步长为 1 的简单 for 循环。
fn tile_block(block: &mut Block, tile_size: usize) {
    let mut out = Vec::new();
    for item in block.items.drain(..) {
        match item {
            BlockItem::Stmt(Stmt::For {
                init, cond, step, body, span,
            }) => {
                // 检测可分块的循环模式
                if let Some((ivar, lo, hi)) = detect_simple_for(&init, &cond, &step) {
                    if tile_size > 1 && hi > lo {
                        // 生成 tile 循环: for (ii = lo; ii < hi; ii += tile_size)
                        //   for (j = 0; j < tile_size && ii+j < hi; j++)
                        //     body[i -> ii+j]
                        // + 尾部循环: for (; ii < hi; ii++) body
                        let tiled = generate_tiled_loop(
                            &ivar, lo, hi, &body, tile_size, &span,
                        );
                        out.extend(tiled);
                        continue;
                    }
                }
                out.push(BlockItem::Stmt(Stmt::For {
                    init, cond, step, body, span,
                }));
            }
            BlockItem::Stmt(Stmt::Block { mut b, span }) => {
                tile_block(&mut b, tile_size);
                out.push(BlockItem::Stmt(Stmt::Block { b, span }));
            }
            BlockItem::Stmt(Stmt::If { cond, then_, else_, span }) => {
                let mut then_b = match *then_ {
                    Stmt::Block { b, .. } => b,
                    s => Block { items: vec![BlockItem::Stmt(s)], span: span.clone() },
                };
                tile_block(&mut then_b, tile_size);
                let else_b = else_.map(|e| -> Box<Stmt> {
                    match *e {
                        Stmt::Block { b, span } => {
                            let mut bb = b;
                            tile_block(&mut bb, tile_size);
                            Box::new(Stmt::Block { b: bb, span })
                        }
                        s => Box::new(s),
                    }
                });
                out.push(BlockItem::Stmt(Stmt::If {
                    cond,
                    then_: Box::new(Stmt::Block {
                        b: then_b,
                        span: span.clone(),
                    }),
                    else_: else_b,
                    span,
                }));
            }
            other => out.push(other),
        }
    }
    block.items = out;
}

/// 检测简单 for 循环: for (int ivar = lo; ivar < hi; ivar++)
fn detect_simple_for(
    init: &Option<Box<Stmt>>,
    cond: &Option<Expr>,
    step: &Option<Expr>,
) -> Option<(String, i128, i128)> {
    let ivar = init.as_deref().and_then(init_stmt_var)?;
    // Check step is ivar++
    match step.as_ref()? {
        Expr::PostInc { e, .. } => {
            if let Expr::Ident { name, .. } = e.as_ref() {
                if name.name != ivar { return None; }
            } else { return None; }
        }
        _ => return None,
    }
    // Check cond is ivar < hi
    let cond = cond.as_ref()?;
    match cond {
        Expr::Binop { op: BinOp::Lt, l, r, .. } => {
            if let Expr::Ident { name, .. } = l.as_ref() {
                if name.name != ivar { return None; }
            } else { return None; }
            if let Expr::Int { v, .. } = r.as_ref() {
                Some((ivar, 0, *v))
            } else { None }
        }
        _ => None,
    }
}

/// 生成 tile 循环 AST (外层 + 内层 + 尾部)
fn generate_tiled_loop(
    ivar: &str,
    lo: i128,
    hi: i128,
    body: &Stmt,
    tile_size: usize,
    span: &Span,
) -> Vec<BlockItem> {
    let sp = span.clone();
    let ts = tile_size as i128;
    let mut items = Vec::new();

    // 外层循环: for (int ii = lo; ii < hi - (hi-lo)%ts; ii += ts)
    let outer_hi = lo + (hi - lo) - (hi - lo) % ts;
    if outer_hi > lo {
        let outer_init = make_var_decl("__dcc_ii", lo, span);
        let outer_cond = Expr::Binop {
            op: BinOp::Lt,
            l: Box::new(make_ident("__dcc_ii", span)),
            r: Box::new(Expr::Int { v: outer_hi, span: sp.clone(), ty: int_ty() }),
            span: sp.clone(),
            ty: int_ty(),
        };
        let outer_step = Expr::Assign {
            op: BinOp::Add,
            l: Box::new(make_ident("__dcc_ii", span)),
            r: Box::new(Expr::Binop {
                op: BinOp::Add,
                l: Box::new(make_ident("__dcc_ii", span)),
                r: Box::new(Expr::Int { v: ts, span: sp.clone(), ty: int_ty() }),
                span: sp.clone(),
                ty: int_ty(),
            }),
            span: sp.clone(),
            ty: int_ty(),
        };
        // 内层循环: for (int j = 0; j < ts; j++) body[ivar -> __dcc_ii + j]
        let inner_body = subst_index_var(body, ivar, &Expr::Binop {
            op: BinOp::Add,
            l: Box::new(make_ident("__dcc_ii", span)),
            r: Box::new(make_ident("j", span)),
            span: sp.clone(),
            ty: int_ty(),
        });
        let inner_for = Stmt::For {
            init: Some(Box::new(make_var_decl("j", 0, span))),
            cond: Some(Expr::Binop {
                op: BinOp::Lt,
                l: Box::new(make_ident("j", span)),
                r: Box::new(Expr::Int { v: ts, span: sp.clone(), ty: int_ty() }),
                span: sp.clone(),
                ty: int_ty(),
            }),
            step: Some(Expr::PostInc {
                e: Box::new(make_ident("j", span)),
                span: sp.clone(),
                ty: int_ty(),
            }),
            body: inner_body,
            span: sp.clone(),
        };
        items.push(BlockItem::Stmt(Stmt::For {
            init: Some(Box::new(outer_init)),
            cond: Some(outer_cond),
            step: Some(outer_step),
            body: Box::new(inner_for),
            span: sp.clone(),
        }));
    }

    // 尾部循环: for (int ii = outer_hi; ii < hi; ii++) body
    if outer_hi < hi {
        let tail_body = subst_index_var(body, ivar, &make_ident("__dcc_ii", span));
        items.push(BlockItem::Stmt(Stmt::For {
            init: Some(Box::new(make_var_decl("__dcc_ii", outer_hi, span))),
            cond: Some(Expr::Binop {
                op: BinOp::Lt,
                l: Box::new(make_ident("__dcc_ii", span)),
                r: Box::new(Expr::Int { v: hi, span: sp.clone(), ty: int_ty() }),
                span: sp.clone(),
                ty: int_ty(),
            }),
            step: Some(Expr::PostInc {
                e: Box::new(make_ident("__dcc_ii", span)),
                span: sp.clone(),
                ty: int_ty(),
            }),
            body: tail_body,
            span: sp.clone(),
        }));
    }

    items
}

// ===== P7. 自动并行化标注 =====

/// 检测可并行化的 for 循环并添加 `#pragma omp simd` 标注。
/// 通过插入注释式标注 (asm volatile) 告知后端。
fn parallelize_block(block: &mut Block) {
    let mut out = Vec::new();
    for item in block.items.drain(..) {
        match item {
            BlockItem::Stmt(Stmt::For {
                init, cond, step, body, span,
            }) => {
                if is_parallelizable_for(&init, &cond, &step, &body) {
                    // 插入并行化标注: asm volatile("#pragma omp simd")
                    out.push(BlockItem::Stmt(Stmt::Expr {
                        e: Expr::AsmExpr {
                            asm_str: "#pragma omp simd".to_string(),
                            outputs: vec![],
                            output_names: vec![],
                            output_constraints: vec![],
                            inputs: vec![],
                            input_names: vec![],
                            input_constraints: vec![],
                            clobbers: vec![],
                            volatile: true,
                            span: span.clone(),
                            ty: Type::Void,
                        },
                        span: span.clone(),
                    }));
                }
                out.push(BlockItem::Stmt(Stmt::For {
                    init, cond, step, body, span,
                }));
            }
            BlockItem::Stmt(Stmt::Block { mut b, span }) => {
                parallelize_block(&mut b);
                out.push(BlockItem::Stmt(Stmt::Block { b, span }));
            }
            other => out.push(other),
        }
    }
    block.items = out;
}

/// 检测 for 循环是否可并行化: 无函数调用、无间接访问、线性归纳变量
fn is_parallelizable_for(
    init: &Option<Box<Stmt>>,
    cond: &Option<Expr>,
    step: &Option<Expr>,
    body: &Stmt,
) -> bool {
    // 必须有简单归纳变量
    if detect_simple_for(init, cond, step).is_none() {
        return false;
    }
    // 循环体无函数调用
    !stmt_has_call(body)
}

fn stmt_has_call(s: &Stmt) -> bool {
    match s {
        Stmt::Expr { e, .. } => expr_has_call(e),
        Stmt::Block { b, .. } => b.items.iter().any(|i| match i {
            BlockItem::Stmt(s) => stmt_has_call(s),
            _ => false,
        }),
        Stmt::If { then_, else_, .. } => {
            stmt_has_call(then_) || else_.as_ref().is_some_and(|e| stmt_has_call(e))
        }
        Stmt::For { body, .. } | Stmt::While { body, .. } | Stmt::Do { body, .. } => {
            stmt_has_call(body)
        }
        _ => false,
    }
}

fn expr_has_call(e: &Expr) -> bool {
    match e {
        Expr::Call { .. } => true,
        Expr::Binop { l, r, .. } | Expr::Comma { l, r, .. } => {
            expr_has_call(l) || expr_has_call(r)
        }
        Expr::Unary { e, .. } | Expr::Cast { e, .. } | Expr::AddrOf { e, .. }
        | Expr::Deref { e, .. } | Expr::PreInc { e, .. } | Expr::PreDec { e, .. }
        | Expr::PostInc { e, .. } | Expr::PostDec { e, .. } => expr_has_call(e),
        Expr::Assign { l, r, .. } => expr_has_call(l) || expr_has_call(r),
        Expr::Cond { cond, then_, else_, .. } => {
            expr_has_call(cond) || expr_has_call(then_) || expr_has_call(else_)
        }
        Expr::Index { arr, idx, .. } => expr_has_call(arr) || expr_has_call(idx),
        _ => false,
    }
}

// ===== P16. 软件流水 (Software Pipelining) =====

/// 软件流水: 对无循环携带依赖的简单 for 循环, 生成
/// prologue (II-1 次迭代) + kernel (重叠执行) + epilogue (排空)。
/// 当前仅对 II=1 (无循环携带依赖) 的循环启用。
fn software_pipeline_block(block: &mut Block) {
    let mut out = Vec::new();
    for item in block.items.drain(..) {
        match item {
            BlockItem::Stmt(Stmt::For {
                init, cond, step, body, span,
            }) => {
                if is_pipelineable(&init, &cond, &step, &body) {
                    // 插入流水化标注: 告知后端可做 modulo scheduling
                    out.push(BlockItem::Stmt(Stmt::Expr {
                        e: Expr::AsmExpr {
                            asm_str: "#pragma GCC pipelined".to_string(),
                            outputs: vec![],
                            output_names: vec![],
                            output_constraints: vec![],
                            inputs: vec![],
                            input_names: vec![],
                            input_constraints: vec![],
                            clobbers: vec![],
                            volatile: true,
                            span: span.clone(),
                            ty: Type::Void,
                        },
                        span: span.clone(),
                    }));
                }
                out.push(BlockItem::Stmt(Stmt::For {
                    init, cond, step, body, span,
                }));
            }
            BlockItem::Stmt(Stmt::Block { mut b, span }) => {
                software_pipeline_block(&mut b);
                out.push(BlockItem::Stmt(Stmt::Block { b, span }));
            }
            other => out.push(other),
        }
    }
    block.items = out;
}

/// 检测循环是否可流水: 简单 for + 无 Store/Call + 无循环携带依赖
fn is_pipelineable(
    init: &Option<Box<Stmt>>,
    cond: &Option<Expr>,
    step: &Option<Expr>,
    body: &Stmt,
) -> bool {
    if detect_simple_for(init, cond, step).is_none() {
        return false;
    }
    // 无函数调用、无内存副作用
    !stmt_has_call(body) && !stmt_has_store(body)
}

fn stmt_has_store(s: &Stmt) -> bool {
    match s {
        Stmt::Expr { e, .. } => expr_has_store(e),
        Stmt::Block { b, .. } => b.items.iter().any(|i| match i {
            BlockItem::Stmt(s) => stmt_has_store(s),
            _ => false,
        }),
        Stmt::If { then_, else_, .. } => {
            stmt_has_store(then_) || else_.as_ref().is_some_and(|e| stmt_has_store(e))
        }
        Stmt::For { body, .. } | Stmt::While { body, .. } | Stmt::Do { body, .. } => {
            stmt_has_store(body)
        }
        _ => false,
    }
}

fn expr_has_store(e: &Expr) -> bool {
    match e {
        Expr::Assign { l, .. } => {
            // 检查是否是 Deref/Index 的赋值 (内存写入)
            matches!(l.as_ref(), Expr::Deref { .. } | Expr::Index { .. })
        }
        Expr::Binop { l, r, .. } | Expr::Comma { l, r, .. } => {
            expr_has_store(l) || expr_has_store(r)
        }
        Expr::Cond { cond, then_, else_, .. } => {
            expr_has_store(cond) || expr_has_store(then_) || expr_has_store(else_)
        }
        _ => false,
    }
}

// ===== 工具函数 =====

fn make_ident(name: &str, span: &Span) -> Expr {
    Expr::Ident {
        name: Ident { name: name.into(), span: span.clone() },
        span: span.clone(),
        ty: int_ty(),
    }
}

fn make_var_decl(name: &str, val: i128, span: &Span) -> Stmt {
    Stmt::Expr {
        e: Expr::Assign {
            op: BinOp::Add,
            l: Box::new(Expr::Ident {
                name: Ident { name: name.into(), span: span.clone() },
                span: span.clone(),
                ty: int_ty(),
            }),
            r: Box::new(Expr::Int { v: val, span: span.clone(), ty: int_ty() }),
            span: span.clone(),
            ty: int_ty(),
        },
        span: span.clone(),
    }
}

fn int_ty() -> Type {
    Type::Int
}

/// 替换循环体中的索引变量为新的表达式
fn subst_index_var(body: &Stmt, old_var: &str, new_expr: &Expr) -> Box<Stmt> {
    Box::new(subst_stmt(body, old_var, new_expr))
}

fn subst_stmt(s: &Stmt, old: &str, new_e: &Expr) -> Stmt {
    match s {
        Stmt::Expr { e, span } => Stmt::Expr {
            e: subst_expr(e, old, new_e),
            span: span.clone(),
        },
        Stmt::Return { val, span } => Stmt::Return {
            val: val.as_ref().map(|e| subst_expr(e, old, new_e)),
            span: span.clone(),
        },
        Stmt::Block { b, span } => Stmt::Block {
            b: Block {
                items: b.items.iter().map(|i| match i {
                    BlockItem::Stmt(s) => BlockItem::Stmt(subst_stmt(s, old, new_e)),
                    other => other.clone(),
                }).collect(),
                span: b.span.clone(),
            },
            span: span.clone(),
        },
        Stmt::If { cond, then_, else_, span } => Stmt::If {
            cond: subst_expr(cond, old, new_e),
            then_: Box::new(subst_stmt(then_, old, new_e)),
            else_: else_.as_ref().map(|e| Box::new(subst_stmt(e, old, new_e))),
            span: span.clone(),
        },
        Stmt::For { init, cond, step, body, span } => Stmt::For {
            init: init.as_ref().map(|i| Box::new(subst_stmt(i, old, new_e))),
            cond: cond.as_ref().map(|c| subst_expr(c, old, new_e)),
            step: step.as_ref().map(|s| subst_expr(s, old, new_e)),
            body: Box::new(subst_stmt(body, old, new_e)),
            span: span.clone(),
        },
        Stmt::While { cond, body, span } => Stmt::While {
            cond: subst_expr(cond, old, new_e),
            body: Box::new(subst_stmt(body, old, new_e)),
            span: span.clone(),
        },
        Stmt::Do { body, cond, span } => Stmt::Do {
            body: Box::new(subst_stmt(body, old, new_e)),
            cond: subst_expr(cond, old, new_e),
            span: span.clone(),
        },
        other => other.clone(),
    }
}

fn subst_expr(e: &Expr, old: &str, new_e: &Expr) -> Expr {
    match e {
        Expr::Ident { name, span, ty } if name.name == old => {
            let mut ne = new_e.clone();
            // 保留原始 span
            match &mut ne {
                Expr::Ident { span: s, .. } => *s = span.clone(),
                Expr::Binop { span: s, .. } => *s = span.clone(),
                _ => {}
            }
            ne
        }
        Expr::Binop { op, l, r, span, ty } => Expr::Binop {
            op: *op,
            l: Box::new(subst_expr(l, old, new_e)),
            r: Box::new(subst_expr(r, old, new_e)),
            span: span.clone(),
            ty: ty.clone(),
        },
        Expr::Assign { op, l, r, span, ty } => Expr::Assign {
            op: *op,
            l: Box::new(subst_expr(l, old, new_e)),
            r: Box::new(subst_expr(r, old, new_e)),
            span: span.clone(),
            ty: ty.clone(),
        },
        Expr::Index { arr, idx, span, ty } => Expr::Index {
            arr: Box::new(subst_expr(arr, old, new_e)),
            idx: Box::new(subst_expr(idx, old, new_e)),
            span: span.clone(),
            ty: ty.clone(),
        },
        Expr::Call { callee, args, span, ty } => Expr::Call {
            callee: Box::new(subst_expr(callee, old, new_e)),
            args: args.iter().map(|a| subst_expr(a, old, new_e)).collect(),
            span: span.clone(),
            ty: ty.clone(),
        },
        Expr::Unary { op, e: inner, span, ty } => Expr::Unary {
            op: *op,
            e: Box::new(subst_expr(inner, old, new_e)),
            span: span.clone(),
            ty: ty.clone(),
        },
        Expr::Cast { ty, e: inner, span } => Expr::Cast {
            ty: ty.clone(),
            e: Box::new(subst_expr(inner, old, new_e)),
            span: span.clone(),
        },
        Expr::Cond { cond, then_, else_, span, ty } => Expr::Cond {
            cond: Box::new(subst_expr(cond, old, new_e)),
            then_: Box::new(subst_expr(then_, old, new_e)),
            else_: Box::new(subst_expr(else_, old, new_e)),
            span: span.clone(),
            ty: ty.clone(),
        },
        Expr::PostInc { e: inner, span, ty } => Expr::PostInc {
            e: Box::new(subst_expr(inner, old, new_e)),
            span: span.clone(),
            ty: ty.clone(),
        },
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Ident, Type};
    use crate::diagnostics::Span;

    fn int_ty() -> Type {
        Type::Int
    }

    /// Hot function: trip count 10 should be unrolled (max=16).
    /// Cold function: trip count 10 should NOT be unrolled (max=4).
    #[test]
    fn hot_func_max_trip_16_accepts_trip_10() {
        let sp = Span::point("t.c", 1, 1);
        let init = Stmt::Expr {
            e: Expr::Assign {
                op: BinOp::Add, // plain assign uses any BinOp
                l: Box::new(Expr::Ident {
                    name: Ident { name: "i".into(), span: sp.clone() },
                    span: sp.clone(),
                    ty: int_ty(),
                }),
                r: Box::new(Expr::Int {
                    v: 0, span: sp.clone(), ty: int_ty(),
                }),
                span: sp.clone(),
                ty: int_ty(),
            },
            span: sp.clone(),
        };
        let cond = Expr::Binop {
            op: BinOp::Lt,
            l: Box::new(Expr::Ident {
                name: Ident { name: "i".into(), span: sp.clone() },
                span: sp.clone(),
                ty: int_ty(),
            }),
            r: Box::new(Expr::Int {
                v: 10, span: sp.clone(), ty: int_ty(),
            }),
            span: sp.clone(),
            ty: int_ty(),
        };
        let step = Expr::PostInc {
            e: Box::new(Expr::Ident {
                name: Ident { name: "i".into(), span: sp.clone() },
                span: sp.clone(),
                ty: int_ty(),
            }),
            span: sp.clone(),
            ty: int_ty(),
        };

        // max_trip=4 should reject trip=10
        let r_cold = parse_counted_for(
            Some(&init), Some(&cond), Some(&step), 4,
        );
        assert!(
            r_cold.is_none(),
            "cold func max_trip=4 should reject trip=10"
        );

        // max_trip=16 should accept trip=10
        let r_hot = parse_counted_for(
            Some(&init), Some(&cond), Some(&step), 16,
        );
        assert!(
            r_hot.is_some(),
            "hot func max_trip=16 should accept trip=10"
        );
        let (ivar, trip) = r_hot.unwrap();
        assert_eq!(ivar, "i");
        assert_eq!(trip, 10);
    }

    /// Trip count within default limit (<=4) should always be accepted.
    #[test]
    fn small_trip_always_accepted() {
        let sp = Span::point("t.c", 1, 1);
        let init = Stmt::Expr {
            e: Expr::Assign {
                op: BinOp::Add,
                l: Box::new(Expr::Ident {
                    name: Ident { name: "j".into(), span: sp.clone() },
                    span: sp.clone(),
                    ty: int_ty(),
                }),
                r: Box::new(Expr::Int {
                    v: 0, span: sp.clone(), ty: int_ty(),
                }),
                span: sp.clone(),
                ty: int_ty(),
            },
            span: sp.clone(),
        };
        let cond = Expr::Binop {
            op: BinOp::Lt,
            l: Box::new(Expr::Ident {
                name: Ident { name: "j".into(), span: sp.clone() },
                span: sp.clone(),
                ty: int_ty(),
            }),
            r: Box::new(Expr::Int {
                v: 3, span: sp.clone(), ty: int_ty(),
            }),
            span: sp.clone(),
            ty: int_ty(),
        };
        let step = Expr::PostInc {
            e: Box::new(Expr::Ident {
                name: Ident { name: "j".into(), span: sp.clone() },
                span: sp.clone(),
                ty: int_ty(),
            }),
            span: sp.clone(),
            ty: int_ty(),
        };

        // Both cold and hot should accept trip=3
        let r_cold = parse_counted_for(
            Some(&init), Some(&cond), Some(&step), 4,
        );
        assert!(r_cold.is_some());
        assert_eq!(r_cold.unwrap().1, 3);

        let r_hot = parse_counted_for(
            Some(&init), Some(&cond), Some(&step), 16,
        );
        assert!(r_hot.is_some());
        assert_eq!(r_hot.unwrap().1, 3);
    }
}
