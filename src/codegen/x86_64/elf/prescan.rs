// src/codegen/x86_64/elf/prescan.rs
// Prescan helpers: nested function collection and signature registration.

use crate::ast::*;
use crate::codegen::x86_64::Codegen;
use std::collections::HashMap;

impl Codegen {
    pub fn collect_nested_funcs(items: &[BlockItem], out: &mut Vec<FuncDecl>) {
        for item in items {
            if let BlockItem::Decl(Decl::Func(f)) = item {
                if f.body.is_some() {
                    out.push(f.clone());
                    if let Some(body) = &f.body {
                        Self::collect_nested_funcs(&body.items, out);
                    }
                }
            }
            // 也扫描嵌套块 (if/for/while 等)
            if let BlockItem::Stmt(s) = item {
                Self::collect_nested_funcs_stmt(s, out);
            }
        }
    }
    fn collect_nested_funcs_stmt(s: &Stmt, out: &mut Vec<FuncDecl>) {
        match s {
            Stmt::Block { b, .. } => Self::collect_nested_funcs(&b.items, out),
            Stmt::If { then_, else_, .. } => {
                if let Stmt::Block { b, .. } = then_.as_ref() {
                    Self::collect_nested_funcs(&b.items, out);
                }
                if let Some(e) = else_ {
                    if let Stmt::Block { b, .. } = e.as_ref() {
                        Self::collect_nested_funcs(&b.items, out);
                    }
                }
            },
            Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
                if let Stmt::Block { b, .. } = body.as_ref() {
                    Self::collect_nested_funcs(&b.items, out);
                }
            },
            _ => {},
        }
    }
    pub fn register_func_sig(f: &FuncDecl, map: &mut HashMap<String, Vec<Type>>) {
        let params: Vec<Type> = f.params.iter().map(|p| p.ty.clone()).collect();
        map.insert(f.name.name.clone(), params);
    }
}
