// src/opt.rs - typed IR 优化 passes
// 第一性原理: 优化在 IR 层做数据流/常量传播，后端只负责指令选择。

use std::collections::HashMap;

use crate::ast::{BinOp, Expr, Program, Stmt, BlockItem, Decl, FuncDecl, Block};
use crate::const_eval;
use crate::diagnostics::Span;
use crate::ir::Module;

/// 按 -O 级别运行 IR 优化。
pub fn optimize(module: &mut Module, level: u8) {
    if level == 0 {
        function_inline(module, 0);
        return;
    }
    fold_constants(module);
    function_inline(module, level);
    if level >= 2 {
        prune_dead_stores(module);
        fold_constant_branches(module);
        eliminate_unreachable(module);
        crate::ssa_gvn::run(module, level);
        crate::loop_opt::run(module, level);
        // M16: 跨函数 DCE (call graph reachability)
        cross_function_dce(module);
        // M16: 暂不跑新 SSA pass - rebuild_block 当前是 stub,会丢掉所有非终结符指令
        // run_ssa_passes(module, level);
    }
}

/// M16: 把 AST -> SSA, 跑 ssa_passes::run, 再 SSA -> AST 重建。
/// 含 goto / label / switch 的函数自动跳过 (走 is_structured 检测)。
fn run_ssa_passes(module: &mut Module, level: u8) {
    let prog = module.source_mut();
    let mut modified = false;
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(mut ssa) = crate::ssa::build_from_func(f) {
                crate::ssa_passes::run(&mut ssa, level);
                *f.body.as_mut().unwrap() = crate::ssa::rebuild_block(&ssa);
                modified = true;
            }
        }
    }
    let _ = modified; // 重建后的 AST 由外层 resync 接管
}

/// M16: 跨函数 DCE - 基于 call graph 标记不可达 internal static 函数
fn cross_function_dce(module: &mut Module) {
    let prog = module.source_mut();
    let mut funcs: Vec<FuncDecl> = Vec::new();
    let mut others: Vec<Decl> = Vec::new();
    for d in prog.decls.drain(..) {
        match d {
            Decl::Func(f) => funcs.push(f),
            other => others.push(other),
        }
    }
    crate::ssa_passes::cross_function_dce(&mut funcs, true);
    let mut all: Vec<Decl> = others;
    for f in funcs { all.push(Decl::Func(f)); }
    prog.decls = all;
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

fn fold_block(items: &mut [BlockItem]) {
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
        Stmt::Return { val: Some(e), .. } => {
            *e = fold_expr(e.clone());
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
            if let (Expr::Float { v: lv, .. }, Expr::Float { v: rv, .. }) = (l.as_ref(), r.as_ref()) {
                if let Some(v) = eval_float_binop(op, *lv, *rv) {
                    return Expr::Float { v, span, ty };
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
        Expr::Call { callee, args, span, ty } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                if name.name == "__builtin_constant_p" && args.len() == 1 {
                    return Expr::Int {
                        v: const_eval::is_constant(&args[0]) as i128,
                        span,
                        ty,
                    };
                }
            }
            Expr::Call {
                callee: Box::new(fold_expr(*callee)),
                args: args.into_iter().map(fold_expr).collect(),
                span,
                ty,
            }
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

fn eval_float_binop(op: BinOp, l: f64, r: f64) -> Option<f64> {
    Some(match op {
        BinOp::Add => l + r,
        BinOp::Sub => l - r,
        BinOp::Mul => l * r,
        BinOp::Div => if r == 0.0 { return None; } else { l / r },
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

/// 语句（递归）内是否含 Label。goto 可跳入常量死分支中的标签,
/// 折叠时若丢弃标签会导致 codegen "unresolved label" 崩溃。
fn stmt_has_label(s: &Stmt) -> bool {
    match s {
        Stmt::Label { .. } => true,
        Stmt::Block { b, .. } => block_has_label(b),
        Stmt::If { then_, else_, .. } => {
            stmt_has_label(then_)
                || else_.as_ref().map(|e| stmt_has_label(e)).unwrap_or(false)
        }
        Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
            stmt_has_label(body)
        }
        Stmt::Switch { cases, .. } => cases.iter().any(|c| block_has_label(&c.body)),
        _ => false,
    }
}

fn block_has_label(b: &Block) -> bool {
    b.items.iter().any(|item| match item {
        BlockItem::Stmt(s) => stmt_has_label(s),
        BlockItem::Decl(Decl::Func(f)) => {
            f.body.as_ref().map(block_has_label).unwrap_or(false)
        }
        _ => false,
    })
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
                // 被丢弃的分支内含 Label 时不能折叠: goto 仍可跳入, 删除会丢标签
                let has_label = if v != 0 {
                    else_.as_ref().map(|e| stmt_has_label(e)).unwrap_or(false)
                } else {
                    stmt_has_label(&then_)
                };
                if has_label {
                    items.insert(i, BlockItem::Stmt(Stmt::If {
                        cond, then_, else_, span,
                    }));
                    i += 1;
                    continue;
                }
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
                if v == 0 && !stmt_has_label(body) {
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
                    if v == 0 && !stmt_has_label(body) {
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
            // goto 仍可跳入 return 之后的标签，不能当不可达代码删除
            if matches!(item, BlockItem::Stmt(Stmt::Label { .. })) {
                return true;
            }
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

/// -O1+: `always_inline` 函数内联；-O3 额外内联小型 static 零参函数。
fn function_inline(module: &mut Module, level: u8) {
    let prog = module.source_mut();
    let funcs: HashMap<String, FuncDecl> = prog
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::Func(f) if f.body.is_some() => Some((f.name.name.clone(), f.clone())),
            _ => None,
        })
        .collect();
    if funcs.is_empty() {
        return;
    }
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &mut f.body {
                inline_calls_in_block(body, &funcs, level);
            }
        }
    }
}

fn count_block_items(b: &Block) -> usize {
    b.items.len()
}

fn inline_calls_in_block(body: &mut Block, funcs: &HashMap<String, FuncDecl>, level: u8) {
    for item in &mut body.items {
        match item {
            BlockItem::Stmt(s) => inline_calls_in_stmt(s, funcs, level),
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(b) = &mut f.body {
                    inline_calls_in_block(b, funcs, level);
                }
            }
            _ => {}
        }
    }
}

fn inline_calls_in_stmt(s: &mut Stmt, funcs: &HashMap<String, FuncDecl>, level: u8) {
    match s {
        Stmt::Return { val: Some(e), .. } => {
            *e = inline_expr(e.clone(), funcs, level);
        }
        Stmt::Expr { e, .. } => *e = inline_expr(e.clone(), funcs, level),
        Stmt::If { cond, then_, else_, .. } => {
            *cond = inline_expr(cond.clone(), funcs, level);
            inline_calls_in_stmt(then_, funcs, level);
            if let Some(e) = else_ {
                inline_calls_in_stmt(e, funcs, level);
            }
        }
        Stmt::Block { b, .. } => inline_calls_in_block(b, funcs, level),
        Stmt::While { body, .. } | Stmt::Do { body, .. } => inline_calls_in_stmt(body, funcs, level),
        Stmt::For { init, cond, step, body, .. } => {
            if let Some(c) = cond {
                *c = inline_expr(c.clone(), funcs, level);
            }
            if let Some(st) = step {
                *st = inline_expr(st.clone(), funcs, level);
            }
            if let Some(i) = init {
                inline_calls_in_stmt(i, funcs, level);
            }
            inline_calls_in_stmt(body, funcs, level);
        }
        _ => {}
    }
}

fn extract_inline_return(body: &Block) -> Option<&Expr> {
    match body.items.as_slice() {
        [BlockItem::Stmt(Stmt::Return { val: Some(e), .. })] => Some(e),
        items if !items.is_empty() => {
            if let BlockItem::Stmt(Stmt::Return { val: Some(e), .. }) = items.last()? {
                if items.iter().all(|it| {
                    matches!(it, BlockItem::Decl(_) | BlockItem::Stmt(Stmt::Return { .. }))
                }) {
                    Some(e)
                } else {
                    None
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

fn expr_calls_name(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::Call { callee, args, .. } => {
            let direct = matches!(callee.as_ref(), Expr::Ident { name: n, .. } if n.name == name);
            direct || args.iter().any(|a| expr_calls_name(a, name))
        }
        Expr::Binop { l, r, .. } => expr_calls_name(l, name) || expr_calls_name(r, name),
        Expr::Unary { e, .. } => expr_calls_name(e, name),
        Expr::Cast { e, .. } => expr_calls_name(e, name),
        Expr::Cond { cond, then_, else_, .. } => {
            expr_calls_name(cond, name) || expr_calls_name(then_, name) || expr_calls_name(else_, name)
        }
        Expr::Comma { l, r, .. } => expr_calls_name(l, name) || expr_calls_name(r, name),
        _ => false,
    }
}

fn substitute_params(expr: Expr, params: &[(String, Expr)]) -> Expr {
    match expr {
        Expr::Ident { name, span, ty } => {
            if let Some((_, arg)) = params.iter().find(|(n, _)| n == &name.name) {
                arg.clone()
            } else {
                Expr::Ident { name, span, ty }
            }
        }
        Expr::Binop { op, l, r, span, ty } => Expr::Binop {
            op,
            l: Box::new(substitute_params(*l, params)),
            r: Box::new(substitute_params(*r, params)),
            span,
            ty,
        },
        Expr::Unary { op, e, span, ty } => Expr::Unary {
            op,
            e: Box::new(substitute_params(*e, params)),
            span,
            ty,
        },
        Expr::Cast { ty, e, span } => Expr::Cast {
            ty,
            e: Box::new(substitute_params(*e, params)),
            span,
        },
        Expr::Cond { cond, then_, else_, span, ty } => Expr::Cond {
            cond: Box::new(substitute_params(*cond, params)),
            then_: Box::new(substitute_params(*then_, params)),
            else_: Box::new(substitute_params(*else_, params)),
            span,
            ty,
        },
        Expr::Comma { l, r, span, ty } => Expr::Comma {
            l: Box::new(substitute_params(*l, params)),
            r: Box::new(substitute_params(*r, params)),
            span,
            ty,
        },
        other => other,
    }
}

fn try_inline_call(
    name: &str,
    args: &[Expr],
    funcs: &HashMap<String, FuncDecl>,
    level: u8,
) -> Option<Expr> {
    let f = funcs.get(name)?;
    if f.is_noinline {
        return None;
    }
    let body = f.body.as_ref()?;
    if f.is_variadic || args.len() != f.params.len() {
        return None;
    }
    let item_count = count_block_items(body);
    let allowed = if f.is_always_inline {
        item_count <= 8
    } else if f.is_static && f.params.is_empty() && level >= 3 {
        item_count <= 4
    } else {
        return None;
    };
    if !allowed {
        return None;
    }
    let ret = extract_inline_return(body)?;
    if expr_calls_name(ret, name) {
        return None;
    }
    let bindings: Vec<(String, Expr)> = f
        .params
        .iter()
        .zip(args.iter())
        .filter_map(|(p, a)| p.name.as_ref().map(|id| (id.name.clone(), a.clone())))
        .collect();
    if bindings.len() != f.params.len() {
        return None;
    }
    Some(substitute_params(ret.clone(), &bindings))
}

fn inline_expr(e: Expr, funcs: &HashMap<String, FuncDecl>, level: u8) -> Expr {
    let e = match e {
        Expr::Call { callee, args, span, ty } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                if let Some(inlined) = try_inline_call(&name.name, &args, funcs, level) {
                    return inline_expr(inlined, funcs, level);
                }
            }
            Expr::Call {
                callee: Box::new(inline_expr(*callee, funcs, level)),
                args: args.into_iter().map(|a| inline_expr(a, funcs, level)).collect(),
                span,
                ty,
            }
        }
        Expr::Binop { op, l, r, span, ty } => Expr::Binop {
            op,
            l: Box::new(inline_expr(*l, funcs, level)),
            r: Box::new(inline_expr(*r, funcs, level)),
            span,
            ty,
        },
        Expr::Unary { op, e, span, ty } => Expr::Unary {
            op,
            e: Box::new(inline_expr(*e, funcs, level)),
            span,
            ty,
        },
        Expr::Cond { cond, then_, else_, span, ty } => Expr::Cond {
            cond: Box::new(inline_expr(*cond, funcs, level)),
            then_: Box::new(inline_expr(*then_, funcs, level)),
            else_: Box::new(inline_expr(*else_, funcs, level)),
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
            is_hot: false,
                section: None,
                alignment: None,
                visibility: None,
                alias_name: None,
                upvars: vec![],
                is_unused: false,
                is_noinline: false,
                is_always_inline: false,
                is_old_style: false,
                format_kind: None,
                format_string_idx: None,
                format_first_arg: None,
                no_sanitize: vec![],
                target_clones: vec![],
                returns_nonnull: false,
                ctor_priority: None, dtor_priority: None, is_retain: false, is_used_attr: false,
                is_flatten: false, error_msg: None, warning_msg: None, deprecated_msg: None,
                ifunc_resolver: None,
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

    #[test]
    fn o2_preserves_goto_label_after_return() {
        let prog = Program {
            decls: vec![Decl::Func(FuncDecl {
                ret_ty: int_ty(),
                name: Ident { name: "f".into(), span: Span::point("t.c", 1, 1) },
                params: vec![],
                body: Some(Block {
                    items: vec![
                        BlockItem::Stmt(Stmt::If {
                            cond: Expr::Ident {
                                name: Ident { name: "x".into(), span: Span::point("t.c", 2, 1) },
                                span: Span::point("t.c", 2, 1),
                                ty: int_ty(),
                            },
                            then_: Box::new(Stmt::Goto {
                                target: Ident { name: "found".into(), span: Span::point("t.c", 3, 1) },
                                span: Span::point("t.c", 3, 1),
                            }),
                            else_: None,
                            span: Span::point("t.c", 2, 1),
                        }),
                        BlockItem::Stmt(Stmt::Return {
                            val: Some(Expr::Int { v: 0, span: Span::point("t.c", 4, 1), ty: int_ty() }),
                            span: Span::point("t.c", 4, 1),
                        }),
                        BlockItem::Stmt(Stmt::Label {
                            name: Ident { name: "found".into(), span: Span::point("t.c", 5, 1) },
                            stmt: Box::new(Stmt::Return {
                                val: Some(Expr::Int { v: 1, span: Span::point("t.c", 6, 1), ty: int_ty() }),
                                span: Span::point("t.c", 6, 1),
                            }),
                            span: Span::point("t.c", 5, 1),
                        }),
                    ],
                    span: Span::point("t.c", 1, 1),
                }),
                span: Span::point("t.c", 1, 1),
                is_static: true,
                is_extern: false,
                is_variadic: false,
                is_weak: false,
                is_constructor: false,
                is_destructor: false,
                is_nested: false,
                is_cold: false,
            is_hot: false,
                section: None,
                alignment: None,
                visibility: None,
                alias_name: None,
                upvars: vec![],
                is_unused: false,
                is_noinline: false,
                is_always_inline: false,
                is_old_style: false,
                format_kind: None,
                format_string_idx: None,
                format_first_arg: None,
                no_sanitize: vec![],
                target_clones: vec![],
                returns_nonnull: false,
                ctor_priority: None, dtor_priority: None, is_retain: false, is_used_attr: false,
                is_flatten: false, error_msg: None, warning_msg: None, deprecated_msg: None,
                ifunc_resolver: None,
            })],
        };
        let mut module = ir::lower(prog).expect("lower");
        optimize(&mut module, 2);
        let body = &module.source().decls[0];
        if let Decl::Func(f) = body {
            assert_eq!(
                f.body.as_ref().unwrap().items.len(),
                3,
                "label after return must survive DCE"
            );
            assert!(matches!(
                f.body.as_ref().unwrap().items[2],
                BlockItem::Stmt(Stmt::Label { .. })
            ));
        }
    }

    #[test]
    fn o1_inlines_always_inline_with_params() {
        let span = Span::point("t.c", 1, 1);
        let int = int_ty();
        let x_ident = || Expr::Ident {
            name: Ident { name: "x".into(), span: span.clone() },
            span: span.clone(),
            ty: int.clone(),
        };
        let sq = Decl::Func(FuncDecl {
            ret_ty: int.clone(),
            name: Ident { name: "sq".into(), span: span.clone() },
            params: vec![crate::ast::Param {
                ty: int.clone(),
                name: Some(Ident { name: "x".into(), span: span.clone() }),
                span: span.clone(),
            }],
            body: Some(Block {
                items: vec![BlockItem::Stmt(Stmt::Return {
                    val: Some(Expr::Binop {
                        op: BinOp::Mul,
                        l: Box::new(x_ident()),
                        r: Box::new(x_ident()),
                        span: span.clone(),
                        ty: int.clone(),
                    }),
                    span: span.clone(),
                })],
                span: span.clone(),
            }),
            span: span.clone(),
            is_static: true,
            is_extern: false,
            is_variadic: false,
            is_weak: false,
            is_constructor: false,
            is_destructor: false,
            is_nested: false,
            is_cold: false,
            is_hot: false,
            section: None,
            alignment: None,
            visibility: None,
            alias_name: None,
            upvars: vec![],
            is_unused: false,
            is_noinline: false,
            is_always_inline: true,
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
            returns_nonnull: false,
            ctor_priority: None, dtor_priority: None, is_retain: false, is_used_attr: false,
            is_flatten: false, error_msg: None, warning_msg: None, deprecated_msg: None,
            ifunc_resolver: None,
        });
        let main_fn = Decl::Func(FuncDecl {
            ret_ty: int.clone(),
            name: Ident { name: "main".into(), span: span.clone() },
            params: vec![],
            body: Some(Block {
                items: vec![BlockItem::Stmt(Stmt::Return {
                    val: Some(Expr::Call {
                        callee: Box::new(Expr::Ident {
                            name: Ident { name: "sq".into(), span: span.clone() },
                            span: span.clone(),
                            ty: int.clone(),
                        }),
                        args: vec![Expr::Int { v: 5, span: span.clone(), ty: int.clone() }],
                        span: span.clone(),
                        ty: int.clone(),
                    }),
                    span: span.clone(),
                })],
                span: span.clone(),
            }),
            span: span.clone(),
            is_static: false,
            is_extern: false,
            is_variadic: false,
            is_weak: false,
            is_constructor: false,
            is_destructor: false,
            is_nested: false,
            is_cold: false,
            is_hot: false,
            section: None,
            alignment: None,
            visibility: None,
            alias_name: None,
            upvars: vec![],
            is_unused: false,
            is_noinline: false,
            is_always_inline: false,
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
            returns_nonnull: false,
            ctor_priority: None, dtor_priority: None, is_retain: false, is_used_attr: false,
            is_flatten: false, error_msg: None, warning_msg: None, deprecated_msg: None,
            ifunc_resolver: None,
        });
        let prog = Program {
            decls: vec![sq, main_fn],
        };
        let mut module = ir::lower(prog).expect("lower");
        optimize(&mut module, 1);
        let main_body = match &module.source().decls[1] {
            Decl::Func(f) => f.body.as_ref().unwrap(),
            _ => panic!("expected main"),
        };
        match &main_body.items[0] {
            BlockItem::Stmt(Stmt::Return {
                val: Some(Expr::Int { v: 25, .. }),
                ..
            }) => {}
            other => panic!("expected inlined constant 25, got {other:?}"),
        }
    }

    #[test]
    fn o2_preserves_label_in_dead_if_branch() {
        // `if (0) { lbl: ... }` 中的标签可被 goto 跳入,
        // 常量分支折叠不能丢弃它 (否则 codegen unresolved label 崩溃)。
        let span = Span::point("t.c", 1, 1);
        let int = int_ty();
        let func = Decl::Func(FuncDecl {
            ret_ty: int.clone(),
            name: Ident { name: "f".into(), span: span.clone() },
            params: vec![],
            body: Some(Block {
                items: vec![
                    BlockItem::Stmt(Stmt::If {
                        cond: Expr::Int { v: 0, span: span.clone(), ty: int.clone() },
                        then_: Box::new(Stmt::Block {
                            b: Block {
                                items: vec![
                                    BlockItem::Stmt(Stmt::Label {
                                        name: Ident { name: "undo".into(), span: span.clone() },
                                        stmt: Box::new(Stmt::Expr {
                                            e: Expr::Int { v: 1, span: span.clone(), ty: int.clone() },
                                            span: span.clone(),
                                        }),
                                        span: span.clone(),
                                    }),
                                ],
                                span: span.clone(),
                            },
                            span: span.clone(),
                        }),
                        else_: None,
                        span: span.clone(),
                    }),
                ],
                span: span.clone(),
            }),
            span: span.clone(),
            is_static: true,
            is_extern: false,
            is_variadic: false,
            is_weak: false,
            is_constructor: false,
            is_destructor: false,
            is_nested: false,
            is_cold: false,
            is_hot: false,
            section: None,
            alignment: None,
            visibility: None,
            alias_name: None,
            upvars: vec![],
            is_unused: false,
            is_noinline: false,
            is_always_inline: false,
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
            returns_nonnull: false,
            ctor_priority: None, dtor_priority: None, is_retain: false, is_used_attr: false,
            is_flatten: false, error_msg: None, warning_msg: None, deprecated_msg: None,
            ifunc_resolver: None,
        });
        let prog = Program { decls: vec![func] };
        let mut module = ir::lower(prog).expect("lower");
        optimize(&mut module, 2);
        let body = &module.source().decls[0];
        if let Decl::Func(f) = body {
            let items = f.body.as_ref().unwrap().items.clone();
            let has_label = items.iter().any(|item| match item {
                BlockItem::Stmt(Stmt::If { then_, .. }) => {
                    stmt_has_label(then_)
                }
                BlockItem::Stmt(s) => stmt_has_label(s),
                _ => false,
            });
            assert!(has_label, "label in dead if branch must survive -O2 fold");
        }
    }
}
