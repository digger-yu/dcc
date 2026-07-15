// src/sanitizer.rs - KASAN/UBSAN/KCSAN/KCOV 插桩（AST 层）
// 第一性原理: 在函数入口插入运行时回调，供内核 sanitizer 链接。

use crate::ast::{BlockItem, Decl, Expr, FuncDecl, Ident, Program, Stmt, Type, int_t};
use crate::diagnostics::Span;

/// 启用的 sanitizer 标志（与 GCC -fsanitize=* 对应）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SanitizerFlags {
    pub kasan: bool,
    pub ubsan: bool,
    pub kcsan: bool,
    pub kcov: bool,
}

impl SanitizerFlags {
    pub fn from_args(args: &[String]) -> Self {
        let mut f = Self::default();
        for a in args {
            if a == "-fsanitize=address" || a.starts_with("-fsanitize=address,") {
                f.kasan = true;
            }
            if a == "-fsanitize=undefined" || a.starts_with("-fsanitize=undefined,") {
                f.ubsan = true;
            }
            if a == "-fsanitize=thread" || a.starts_with("-fsanitize=thread,") {
                f.kcsan = true;
            }
            if a.contains("sanitize-coverage") || a == "-fsanitize-coverage" {
                f.kcov = true;
            }
        }
        f
    }

    pub fn any(&self) -> bool {
        self.kasan || self.ubsan || self.kcsan || self.kcov
    }
}

/// 在 AST 层为每个函数注入 sanitizer 入口桩。
pub fn instrument_program(prog: &mut Program, flags: SanitizerFlags) {
    if !flags.any() {
        return;
    }
    for decl in prog.decls.iter_mut() {
        if let Decl::Func(f) = decl {
            inject_func_prologue(f, flags);
        }
    }
}

fn inject_func_prologue(func: &mut FuncDecl, flags: SanitizerFlags) {
    if let Some(body) = &mut func.body {
        let mut prologue = Vec::new();
        if flags.kasan {
            prologue.push(sanitizer_call("__asan_init", &func.span));
        }
        if flags.ubsan {
            prologue.push(sanitizer_call("__ubsan_init", &func.span));
        }
        if flags.kcsan {
            prologue.push(sanitizer_call("__kcsan_init", &func.span));
        }
        if flags.kcov {
            prologue.push(sanitizer_call("__sanitizer_cov_trace_pc", &func.span));
        }
        let mut items = prologue;
        items.append(&mut body.items);
        body.items = items;
    }
}

fn sanitizer_call(name: &str, span: &Span) -> BlockItem {
    let id = Ident { name: name.to_string(), span: span.clone() };
    BlockItem::Stmt(Stmt::Expr {
        e: Expr::Call {
            callee: Box::new(Expr::Ident {
                name: id,
                span: span.clone(),
                ty: Type::Func(Box::new(Type::Void), vec![]),
            }),
            args: vec![],
            span: span.clone(),
            ty: int_t(),
        },
        span: span.clone(),
    })
}

/// 生成 sanitizer 元数据节内容（记录启用的后端）。
pub fn sanitizer_metadata(flags: SanitizerFlags) -> Vec<u8> {
    let mut out = b"DCC_SAN\0".to_vec();
    out.push(flags.kasan as u8);
    out.push(flags.ubsan as u8);
    out.push(flags.kcsan as u8);
    out.push(flags.kcov as u8);
    out
}
