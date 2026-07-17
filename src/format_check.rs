//! `-Wformat`：对带 `__attribute__((format))` 的函数调用做基础 arity 检查。

use crate::ast::{Decl, Expr, FuncDecl, Program, Stmt};
use crate::diagnostics::{Diagnostic, Span};

/// 扫描程序中对 format 属性函数的调用，返回警告列表。
pub fn check_program(prog: &Program) -> Vec<Diagnostic> {
    let mut funcs: std::collections::HashMap<String, &FuncDecl> = std::collections::HashMap::new();
    for decl in &prog.decls {
        if let Decl::Func(f) = decl {
            if f.format_kind.is_some() {
                funcs.insert(f.name.name.clone(), f);
            }
        }
    }
    let mut out = Vec::new();
    for decl in &prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &f.body {
                check_block(&body.items, &funcs, &mut out);
            }
        }
    }
    out
}

fn check_block(items: &[crate::ast::BlockItem], funcs: &std::collections::HashMap<String, &FuncDecl>, out: &mut Vec<Diagnostic>) {
    for item in items {
        match item {
            crate::ast::BlockItem::Stmt(s) => check_stmt(s, funcs, out),
            crate::ast::BlockItem::Decl(Decl::Func(f)) => {
                if let Some(body) = &f.body {
                    check_block(&body.items, funcs, out);
                }
            }
            _ => {}
        }
    }
}

fn expr_span(e: &Expr) -> Span {
    match e {
        Expr::Int { span, .. }
        | Expr::Float { span, .. }
        | Expr::String { span, .. }
        | Expr::Char { span, .. }
        | Expr::Ident { span, .. }
        | Expr::Binop { span, .. }
        | Expr::Unary { span, .. }
        | Expr::Call { span, .. }
        | Expr::Assign { span, .. }
        | Expr::Cond { span, .. }
        | Expr::Cast { span, .. }
        | Expr::SizeofTy { span, .. }
        | Expr::SizeofExpr { span, .. }
        | Expr::Index { span, .. }
        | Expr::Member { span, .. }
        | Expr::AddrOf { span, .. }
        | Expr::Deref { span, .. }
        | Expr::PostInc { span, .. }
        | Expr::PostDec { span, .. }
        | Expr::PreInc { span, .. }
        | Expr::PreDec { span, .. }
        | Expr::CompoundLit { span, .. }
        | Expr::InitList { span, .. }
        | Expr::AlignofTy { span, .. }
        | Expr::AlignofExpr { span, .. }
        | Expr::StmtExpr { span, .. } => span.clone(),
        _ => Span::point("", 0, 0),
    }
}

fn check_stmt(s: &Stmt, funcs: &std::collections::HashMap<String, &FuncDecl>, out: &mut Vec<Diagnostic>) {
    match s {
        Stmt::Expr { e, span } => check_expr(e, funcs, out, span),
        Stmt::Return { val: Some(e), span } => check_expr(e, funcs, out, span),
        Stmt::If { cond, then_, else_, .. } => {
            check_expr(cond, funcs, out, &expr_span(cond));
            check_stmt(then_, funcs, out);
            if let Some(e) = else_ {
                check_stmt(e, funcs, out);
            }
        }
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            check_expr(cond, funcs, out, &expr_span(cond));
            check_stmt(body, funcs, out);
        }
        Stmt::For { init, cond, step, body, .. } => {
            if let Some(i) = init {
                check_stmt(i, funcs, out);
            }
            if let Some(c) = cond {
                check_expr(c, funcs, out, &expr_span(c));
            }
            if let Some(st) = step {
                check_expr(st, funcs, out, &expr_span(st));
            }
            check_stmt(body, funcs, out);
        }
        Stmt::Block { b, .. } => check_block(&b.items, funcs, out),
        _ => {}
    }
}

fn check_expr(
    e: &Expr,
    funcs: &std::collections::HashMap<String, &FuncDecl>,
    out: &mut Vec<Diagnostic>,
    _span: &Span,
) {
    if let Expr::Call { callee, args, span, .. } = e {
        if let Expr::Ident { name, .. } = callee.as_ref() {
            if let Some(f) = funcs.get(&name.name) {
                check_format_call(f, args, span, out);
            }
        }
        for a in args {
            check_expr(a, funcs, out, &expr_span(a));
        }
        return;
    }
    match e {
        Expr::Binop { l, r, .. } => {
            check_expr(l, funcs, out, &expr_span(l));
            check_expr(r, funcs, out, &expr_span(r));
        }
        Expr::Unary { e: inner, .. } => check_expr(inner, funcs, out, &expr_span(inner)),
        Expr::Assign { l, r, .. } => {
            check_expr(l, funcs, out, &expr_span(l));
            check_expr(r, funcs, out, &expr_span(r));
        }
        _ => {}
    }
}

fn check_format_call(f: &FuncDecl, args: &[Expr], span: &Span, out: &mut Vec<Diagnostic>) {
    let fmt_idx = f.format_string_idx.unwrap_or(1) as usize;
    let first_arg = f.format_first_arg.unwrap_or(2) as usize;
    if fmt_idx == 0 || fmt_idx > args.len() {
        out.push(Diagnostic::warn(
            "W0003",
            format!(
                "format attribute on '{}' expects format string at argument {}",
                f.name.name, fmt_idx
            ),
            span.clone(),
        )
        .with_hint("add a string literal format argument at the declared index"));
        return;
    }
    let fmt_lit = format_string_literal(&args[fmt_idx - 1]);
    let Some(fmt) = fmt_lit else {
        return;
    };
    let expected = count_format_specs(&fmt);
    let provided = args.len().saturating_sub(first_arg.saturating_sub(1));
    if provided < expected {
        out.push(Diagnostic::warn(
            "W0003",
            format!(
                "format '{}' expects at least {} argument(s) for conversion specifiers, got {}",
                f.name.name, expected, provided
            ),
            span.clone(),
        )
        .with_hint("check printf/scanf format string and argument list"));
    }
}

fn format_string_literal(e: &Expr) -> Option<String> {
    match e {
        Expr::String { data, .. } => Some(String::from_utf8_lossy(data).into_owned()),
        _ => None,
    }
}

fn count_format_specs(fmt: &str) -> usize {
    let bytes = fmt.as_bytes();
    let mut i = 0;
    let mut count = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            i += 1;
            if i < bytes.len() && bytes[i] == b'%' {
                i += 1;
                continue;
            }
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'.' {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
            if i < bytes.len() && bytes[i] == b'*' {
                i += 1;
            }
            if i < bytes.len() {
                let c = bytes[i] as char;
                if c != '%' && "diouxXfceEgGnps".contains(c) {
                    count += 1;
                }
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    count
}
