// src/codegen/x86_64/elf/attr_diag.rs
// Diagnostics for error/warning/deprecated function attributes.

use crate::ast::*;
use crate::codegen::x86_64::Codegen;

pub type AttrDiagFn<'a> = (&'a str, Option<&'a str>, Option<&'a str>, Option<&'a str>);

impl Codegen {
    fn check_attr_diagnostics<'a>(
        items: &[crate::ast::BlockItem],
        diag_fns: &[AttrDiagFn<'a>],
        _caller: &str,
    ) {
        for item in items {
            match item {
                crate::ast::BlockItem::Stmt(s) => {
                    Self::check_attr_diagnostics_stmt(s, diag_fns, _caller)
                }
                crate::ast::BlockItem::Decl(Decl::Func(f)) => {
                    if let Some(body) = &f.body {
                        Self::check_attr_diagnostics(&body.items, diag_fns, _caller);
                    }
                }
                _ => {}
            }
        }
    }

    fn check_attr_diagnostics_stmt<'a>(
        s: &crate::ast::Stmt,
        diag_fns: &[AttrDiagFn<'a>],
        _caller: &str,
    ) {
        match s {
            Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => {
                Self::check_attr_diagnostics_expr(e, diag_fns, _caller);
            }
            Stmt::If {
                cond, then_, else_, ..
            } => {
                Self::check_attr_diagnostics_expr(cond, diag_fns, _caller);
                Self::check_attr_diagnostics_stmt(then_, diag_fns, _caller);
                if let Some(e) = else_ {
                    Self::check_attr_diagnostics_stmt(e, diag_fns, _caller);
                }
            }
            Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
                Self::check_attr_diagnostics_expr(cond, diag_fns, _caller);
                Self::check_attr_diagnostics_stmt(body, diag_fns, _caller);
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => {
                if let Some(i) = init {
                    Self::check_attr_diagnostics_stmt(i, diag_fns, _caller);
                }
                if let Some(c) = cond {
                    Self::check_attr_diagnostics_expr(c, diag_fns, _caller);
                }
                if let Some(st) = step {
                    Self::check_attr_diagnostics_expr(st, diag_fns, _caller);
                }
                Self::check_attr_diagnostics_stmt(body, diag_fns, _caller);
            }
            Stmt::Block { b, .. } => Self::check_attr_diagnostics(&b.items, diag_fns, _caller),
            _ => {}
        }
    }

    fn check_attr_diagnostics_expr<'a>(
        e: &crate::ast::Expr,
        diag_fns: &[AttrDiagFn<'a>],
        _caller: &str,
    ) {
        if let Expr::Call { callee, args, .. } = e {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                for (fname, err, warn, dep) in diag_fns {
                    if name.name == *fname {
                        if let Some(msg) = err {
                            eprintln!(
                                "error: call to '{}' declared with error attribute: {}",
                                fname, msg
                            );
                        }
                        if let Some(msg) = warn {
                            eprintln!(
                                "warning: call to '{}' declared with warning attribute: {}",
                                fname, msg
                            );
                        }
                        if let Some(msg) = dep {
                            eprintln!("warning: '{}' is deprecated: {}", fname, msg);
                        }
                    }
                }
            }
            for a in args {
                Self::check_attr_diagnostics_expr(a, diag_fns, _caller);
            }
        } else {
            match e {
                Expr::Binop { l, r, .. } => {
                    Self::check_attr_diagnostics_expr(l, diag_fns, _caller);
                    Self::check_attr_diagnostics_expr(r, diag_fns, _caller);
                }
                Expr::Unary { e: inner, .. }
                | Expr::Cast { e: inner, .. }
                | Expr::AddrOf { e: inner, .. }
                | Expr::Deref { e: inner, .. } => {
                    Self::check_attr_diagnostics_expr(inner, diag_fns, _caller);
                }
                Expr::Assign { l, r, .. } => {
                    Self::check_attr_diagnostics_expr(l, diag_fns, _caller);
                    Self::check_attr_diagnostics_expr(r, diag_fns, _caller);
                }
                Expr::Cond {
                    cond, then_, else_, ..
                } => {
                    Self::check_attr_diagnostics_expr(cond, diag_fns, _caller);
                    Self::check_attr_diagnostics_expr(then_, diag_fns, _caller);
                    Self::check_attr_diagnostics_expr(else_, diag_fns, _caller);
                }
                Expr::Index { arr, idx, .. } => {
                    Self::check_attr_diagnostics_expr(arr, diag_fns, _caller);
                    Self::check_attr_diagnostics_expr(idx, diag_fns, _caller);
                }
                _ => {}
            }
        }
    }
}
