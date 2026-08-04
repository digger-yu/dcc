//! SSA 风格局部优化：拷贝传播、GVN/CSE、虚拟寄存器合并。
//!
//! 在 AST 层工作，与 x86_64 栈槽后端配合：消除冗余纯表达式，
//! 合并等价的局部临时变量（类比 register coalescing）。

use std::collections::HashMap;

use crate::ast::{BinOp, Block, BlockItem, Decl, Expr, Program, Stmt, Type};
use crate::ir::Module;

/// `-O2+`：函数级 GVN + 拷贝传播 + 临时变量合并。
pub fn run(module: &mut Module, level: u8) {
    if level < 2 {
        return;
    }
    let prog = module.source_mut();
    gvn_program(prog);
}

fn gvn_program(prog: &mut Program) {
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &mut f.body {
                gvn_block(body);
            }
        }
    }
}

fn gvn_block(block: &mut Block) {
    let mut avail: HashMap<String, String> = HashMap::new();
    let mut copies: HashMap<String, String> = HashMap::new();
    for item in &mut block.items {
        match item {
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(init) = &mut v.init {
                    propagate_copies_expr(init, &copies);
                    if let Some(key) = gvn_key_string(init) {
                        if let Some(prev) = avail.get(&key) {
                            *init = ident_expr(prev, init.ty());
                            copies.insert(v.name.name.clone(), prev.clone());
                        } else {
                            avail.insert(key, v.name.name.clone());
                        }
                    }
                }
            }
            BlockItem::Stmt(s) => gvn_stmt(s, &mut avail, &mut copies),
            _ => {}
        }
    }
    coalesce_block(block);
}

fn gvn_stmt(
    s: &mut Stmt,
    avail: &mut HashMap<String, String>,
    copies: &mut HashMap<String, String>,
) {
    match s {
        Stmt::Return { val: Some(e), .. } => propagate_copies_expr(e, copies),
        Stmt::Expr { e, .. } => {
            if let Expr::Assign { l, r, .. } = e {
                if let Expr::Ident { name, .. } = l.as_ref() {
                    propagate_copies_expr(r, copies);
                    if let Some(key) = gvn_key_string(r) {
                        if let Some(prev) = avail.get(&key) {
                            **r = ident_expr(prev, r.ty());
                            copies.insert(name.name.clone(), prev.clone());
                        } else {
                            avail.insert(key, name.name.clone());
                        }
                    }
                } else {
                    propagate_copies_expr(e, copies);
                }
            } else {
                propagate_copies_expr(e, copies);
            }
        }
        Stmt::If {
            cond, then_, else_, ..
        } => {
            propagate_copies_expr(cond, copies);
            gvn_stmt(then_, avail, copies);
            if let Some(e) = else_ {
                gvn_stmt(e, avail, copies);
            }
        }
        Stmt::Block { b, .. } => gvn_block(b),
        Stmt::While { cond, body, .. } => {
            propagate_copies_expr(cond, copies);
            gvn_stmt(body, avail, copies);
        }
        Stmt::Do { body, cond, .. } => {
            gvn_stmt(body, avail, copies);
            propagate_copies_expr(cond, copies);
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                gvn_stmt(i, avail, copies);
            }
            if let Some(c) = cond {
                propagate_copies_expr(c, copies);
            }
            if let Some(st) = step {
                propagate_copies_expr(st, copies);
            }
            gvn_stmt(body, avail, copies);
        }
        _ => {}
    }
}

/// 合并「仅拷贝另一局部变量」且未再被赋值的冗余 decl（虚拟寄存器 coalesce）。
fn coalesce_block(block: &mut Block) {
    let mut alias: HashMap<String, String> = HashMap::new();
    let mut assigned: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in &block.items {
        collect_assignments(item, &mut assigned);
    }
    for item in &block.items {
        if let BlockItem::Decl(Decl::Var(v)) = item {
            if let Some(Expr::Ident { name, .. }) = &v.init {
                if !assigned.contains(&v.name.name) {
                    alias.insert(v.name.name.clone(), name.name.clone());
                }
            }
        }
    }
    if alias.is_empty() {
        return;
    }
    for item in &mut block.items {
        rewrite_aliases(item, &alias);
    }
    block.items.retain(
        |item| !matches!(item, BlockItem::Decl(Decl::Var(v)) if alias.contains_key(&v.name.name)),
    );
}

fn collect_assignments(item: &BlockItem, assigned: &mut std::collections::HashSet<String>) {
    match item {
        BlockItem::Stmt(Stmt::Expr {
            e: Expr::Assign { l, .. },
            ..
        }) => {
            if let Expr::Ident { name, .. } = l.as_ref() {
                assigned.insert(name.name.clone());
            }
        }
        BlockItem::Stmt(s) => collect_assignments_stmt(s, assigned),
        _ => {}
    }
}

fn collect_assignments_stmt(s: &Stmt, assigned: &mut std::collections::HashSet<String>) {
    match s {
        Stmt::Expr {
            e: Expr::Assign { l, .. },
            ..
        } => {
            if let Expr::Ident { name, .. } = l.as_ref() {
                assigned.insert(name.name.clone());
            }
        }
        Stmt::Block { b, .. } => {
            for sub in &b.items {
                collect_assignments(sub, assigned);
            }
        }
        Stmt::If { then_, else_, .. } => {
            collect_assignments_stmt(then_, assigned);
            if let Some(e) = else_ {
                collect_assignments_stmt(e, assigned);
            }
        }
        Stmt::While { body, .. } | Stmt::Do { body, .. } => {
            collect_assignments_stmt(body, assigned);
        }
        Stmt::For { init, body, .. } => {
            if let Some(i) = init {
                collect_assignments_stmt(i, assigned);
            }
            collect_assignments_stmt(body, assigned);
        }
        Stmt::Label { stmt, .. } => collect_assignments_stmt(stmt, assigned),
        Stmt::Switch { cases, .. } => {
            for c in cases {
                for sub in &c.body.items {
                    collect_assignments(sub, assigned);
                }
            }
        }
        _ => {}
    }
}

fn rewrite_aliases(item: &mut BlockItem, alias: &HashMap<String, String>) {
    match item {
        BlockItem::Decl(Decl::Var(v)) => {
            if let Some(init) = &mut v.init {
                rewrite_expr_aliases(init, alias);
            }
        }
        BlockItem::Stmt(s) => rewrite_stmt_aliases(s, alias),
        _ => {}
    }
}

fn rewrite_stmt_aliases(s: &mut Stmt, alias: &HashMap<String, String>) {
    match s {
        Stmt::Return { val: Some(e), .. } => rewrite_expr_aliases(e, alias),
        Stmt::Expr { e, .. } => rewrite_expr_aliases(e, alias),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            rewrite_expr_aliases(cond, alias);
            rewrite_stmt_aliases(then_, alias);
            if let Some(e) = else_ {
                rewrite_stmt_aliases(e, alias);
            }
        }
        Stmt::Block { b, .. } => {
            for sub in &mut b.items {
                rewrite_aliases(sub, alias);
            }
        }
        Stmt::While { cond, body, .. } => {
            rewrite_expr_aliases(cond, alias);
            rewrite_stmt_aliases(body, alias);
        }
        Stmt::Do { body, cond, .. } => {
            rewrite_stmt_aliases(body, alias);
            rewrite_expr_aliases(cond, alias);
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                rewrite_stmt_aliases(i, alias);
            }
            if let Some(c) = cond {
                rewrite_expr_aliases(c, alias);
            }
            if let Some(st) = step {
                rewrite_expr_aliases(st, alias);
            }
            rewrite_stmt_aliases(body, alias);
        }
        Stmt::Label { stmt, .. } => rewrite_stmt_aliases(stmt, alias),
        Stmt::GotoIndirect { expr, .. } => rewrite_expr_aliases(expr, alias),
        Stmt::Switch { cases, .. } => {
            for c in cases {
                for sub in &mut c.body.items {
                    rewrite_aliases(sub, alias);
                }
            }
        }
        _ => {}
    }
}

fn rewrite_expr_aliases(e: &mut Expr, alias: &HashMap<String, String>) {
    match e {
        Expr::Ident { name, .. } => {
            if let Some(target) = alias.get(&name.name) {
                name.name = target.clone();
            }
        }
        Expr::Binop { l, r, .. } => {
            rewrite_expr_aliases(l, alias);
            rewrite_expr_aliases(r, alias);
        }
        Expr::Unary { e: inner, .. } => rewrite_expr_aliases(inner, alias),
        Expr::Assign { l, r, .. } => {
            rewrite_expr_aliases(l, alias);
            rewrite_expr_aliases(r, alias);
        }
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            rewrite_expr_aliases(cond, alias);
            rewrite_expr_aliases(then_, alias);
            rewrite_expr_aliases(else_, alias);
        }
        Expr::Cast { e: inner, .. } => rewrite_expr_aliases(inner, alias),
        Expr::Index { arr, idx, .. } => {
            rewrite_expr_aliases(arr, alias);
            rewrite_expr_aliases(idx, alias);
        }
        _ => {}
    }
}

fn propagate_copies_expr(e: &mut Expr, copies: &HashMap<String, String>) {
    match e {
        Expr::Ident { name, .. } => {
            if let Some(src) = copies.get(&name.name) {
                name.name = src.clone();
            }
        }
        Expr::Binop { l, r, .. } => {
            propagate_copies_expr(l, copies);
            propagate_copies_expr(r, copies);
        }
        Expr::Unary { e: inner, .. } => propagate_copies_expr(inner, copies),
        Expr::Assign { l, r, .. } => {
            propagate_copies_expr(l, copies);
            propagate_copies_expr(r, copies);
        }
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            propagate_copies_expr(cond, copies);
            propagate_copies_expr(then_, copies);
            propagate_copies_expr(else_, copies);
        }
        Expr::Cast { e: inner, .. } => propagate_copies_expr(inner, copies),
        Expr::Index { arr, idx, .. } => {
            propagate_copies_expr(arr, copies);
            propagate_copies_expr(idx, copies);
        }
        _ => {}
    }
}

fn gvn_key_string(e: &Expr) -> Option<String> {
    match e {
        Expr::Int { v, .. } => Some(format!("i:{v}")),
        Expr::Ident { name, .. } => Some(format!("v:{}", name.name)),
        Expr::Binop { op, l, r, .. } if is_gvn_pure_op(*op) => Some(format!(
            "b:{op:?}:{}:{}",
            gvn_key_string(l)?,
            gvn_key_string(r)?
        )),
        Expr::Unary { op, e: inner, .. } => Some(format!("u:{op:?}:{}", gvn_key_string(inner)?)),
        _ => None,
    }
}

fn is_gvn_pure_op(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::And
            | BinOp::Or
            | BinOp::Xor
            | BinOp::Shl
            | BinOp::Shr
    )
}

fn ident_expr(name: &str, ty: Type) -> Expr {
    use crate::diagnostics::Span;
    Expr::Ident {
        name: crate::ast::Ident {
            name: name.to_string(),
            span: Span::point("gvn", 1, 1),
        },
        span: Span::point("gvn", 1, 1),
        ty,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{FuncDecl, Ident, Type};
    use crate::diagnostics::Span;
    use crate::ir;

    fn int_ty() -> Type {
        Type::Int
    }

    #[test]
    fn gvn_reuses_prior_add() {
        let sp = Span::point("t.c", 1, 1);
        let x = || Expr::Ident {
            name: Ident {
                name: "x".into(),
                span: sp.clone(),
            },
            span: sp.clone(),
            ty: int_ty(),
        };
        let y = || Expr::Ident {
            name: Ident {
                name: "y".into(),
                span: sp.clone(),
            },
            span: sp.clone(),
            ty: int_ty(),
        };
        let add = || Expr::Binop {
            op: BinOp::Add,
            l: Box::new(x()),
            r: Box::new(y()),
            span: sp.clone(),
            ty: int_ty(),
        };
        let prog = Program {
            decls: vec![Decl::Func(FuncDecl {
                ret_ty: int_ty(),
                name: Ident {
                    name: "f".into(),
                    span: sp.clone(),
                },
                params: vec![],
                body: Some(Block {
                    items: vec![
                        BlockItem::Decl(Decl::Var(crate::ast::VarDecl {
                            ty: int_ty(),
                            name: Ident {
                                name: "a".into(),
                                span: sp.clone(),
                            },
                            init: Some(add()),
                            span: sp.clone(),
                            is_extern: false,
                            is_static: false,
                            section: None,
                            alignment: None,
                            is_weak: false,
                            visibility: None,
                            is_unused: false,
                            register_name: None,
                            cleanup_fn: None,
                            is_thread_local: false,
                            no_sanitize: vec![],
                            copy_fn: None,
                            is_retain: false,
                            is_used_attr: false,
                        })),
                        BlockItem::Decl(Decl::Var(crate::ast::VarDecl {
                            ty: int_ty(),
                            name: Ident {
                                name: "b".into(),
                                span: sp.clone(),
                            },
                            init: Some(add()),
                            span: sp.clone(),
                            is_extern: false,
                            is_static: false,
                            section: None,
                            alignment: None,
                            is_weak: false,
                            visibility: None,
                            is_unused: false,
                            register_name: None,
                            cleanup_fn: None,
                            is_thread_local: false,
                            no_sanitize: vec![],
                            copy_fn: None,
                            is_retain: false,
                            is_used_attr: false,
                        })),
                    ],
                    span: sp.clone(),
                }),
                span: sp.clone(),
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
                ctor_priority: None,
                dtor_priority: None,
                is_retain: false,
                is_used_attr: false,
                is_flatten: false,
                error_msg: None,
                warning_msg: None,
                deprecated_msg: None,
                ifunc_resolver: None,
            })],
        };
        let mut module = ir::lower(prog).expect("lower");
        run(&mut module, 2);
        let body = match &module.source().decls[0] {
            Decl::Func(f) => f.body.as_ref().unwrap(),
            _ => panic!("func"),
        };
        assert_eq!(body.items.len(), 1, "coalesce should drop redundant b decl");
        if let BlockItem::Decl(Decl::Var(v)) = &body.items[0] {
            assert_eq!(v.name.name, "a");
        }
    }
}
