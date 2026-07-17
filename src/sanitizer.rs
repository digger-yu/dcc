// src/sanitizer.rs - KASAN/UBSAN/KCSAN/KCOV 插桩（AST 层，初版桩）
//
// 设计边界（非完整 sanitizer 运行时）：
// - TU 级 init 回调（__asan_init 等）仅在首个有体函数前插入一次
// - 每个有体函数入口插入 coverage / KCSAN 回调桩
// - 不做内存访问/UB 条件的指令级插桩；完整 KASAN/UBSAN 需 IR/codegen 扩展
// - codegen 写入 `.dcc.san_info` 节记录启用的后端位图

use crate::ast::{Block, BlockItem, Decl, Expr, FuncDecl, Ident, Program, Stmt, Type, int_t};
use crate::diagnostics::Span;

/// 启用的 sanitizer 标志（与 GCC `-fsanitize=*` 子集对应）。
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
            Self::apply_flag(a, &mut f);
        }
        f
    }

    fn apply_flag(arg: &str, f: &mut Self) {
        // 逗号分隔组合：-fsanitize=address,undefined
        if let Some(rest) = arg.strip_prefix("-fsanitize=") {
            for part in rest.split(',') {
                Self::apply_sanitize_name(part.trim(), f);
            }
            return;
        }
        if arg.contains("sanitize-coverage") || arg == "-fsanitize-coverage" {
            f.kcov = true;
        }
    }

    fn apply_sanitize_name(name: &str, f: &mut Self) {
        match name {
            "address" | "kernel-address" | "hwaddress" | "kernel-hwaddress" => f.kasan = true,
            "undefined" => f.ubsan = true,
            "thread" => f.kcsan = true,
            "fuzzer" | "coverage" => f.kcov = true,
            _ if name.contains("coverage") => f.kcov = true,
            _ => {}
        }
    }

    pub fn any(&self) -> bool {
        self.kasan || self.ubsan || self.kcsan || self.kcov
    }

    /// TU 级 init 是否需要在首个函数前插入。
    pub fn needs_tu_init(&self) -> bool {
        self.kasan || self.ubsan || self.kcsan
    }
}

/// 在 AST 层插桩：TU init 一次 + 每个有体函数入口桩（含嵌套函数）。
pub fn instrument_program(prog: &mut Program, flags: SanitizerFlags) {
    if !flags.any() {
        return;
    }
    let mut tu_init_pending = flags.needs_tu_init();
    for decl in prog.decls.iter_mut() {
        if let Decl::Func(f) = decl {
            instrument_func(f, flags, &mut tu_init_pending);
        }
    }
}

fn instrument_func(func: &mut FuncDecl, flags: SanitizerFlags, tu_init_pending: &mut bool) {
    if func.body.is_none() {
        return;
    }
    let span = func.span.clone();
    let mut prologue = Vec::new();
    if *tu_init_pending {
        prologue.extend(tu_init_calls(flags, &span));
        *tu_init_pending = false;
    }
    prologue.extend(func_entry_calls(flags, &span));
    if !prologue.is_empty() {
        inject_func_prologue(func, &prologue);
    }
    if let Some(body) = &mut func.body {
        instrument_block_items(&mut body.items, flags, tu_init_pending);
    }
}

fn instrument_block_items(
    items: &mut [BlockItem],
    flags: SanitizerFlags,
    tu_init_pending: &mut bool,
) {
    for item in items.iter_mut() {
        match item {
            BlockItem::Decl(Decl::Func(f)) => instrument_func(f, flags, tu_init_pending),
            BlockItem::Stmt(stmt) => instrument_stmt(stmt, flags, tu_init_pending),
            _ => {}
        }
    }
}

fn instrument_stmt(stmt: &mut Stmt, flags: SanitizerFlags, tu_init_pending: &mut bool) {
    match stmt {
        Stmt::Block { b, .. } => instrument_block_items(&mut b.items, flags, tu_init_pending),
        Stmt::If { then_, else_, .. } => {
            instrument_stmt(then_, flags, tu_init_pending);
            if let Some(e) = else_ {
                instrument_stmt(e, flags, tu_init_pending);
            }
        }
        Stmt::While { body, .. } | Stmt::Do { body, .. } => {
            instrument_stmt(body, flags, tu_init_pending);
        }
        Stmt::For { init, body, .. } => {
            if let Some(s) = init {
                instrument_stmt(s, flags, tu_init_pending);
            }
            instrument_stmt(body, flags, tu_init_pending);
        }
        Stmt::Switch { cases, .. } => {
            for case in cases.iter_mut() {
                instrument_block_items(&mut case.body.items, flags, tu_init_pending);
            }
        }
        Stmt::Label { stmt, .. } => instrument_stmt(stmt, flags, tu_init_pending),
        _ => {}
    }
}

/// TU 级一次性 init（不含 per-function coverage）。
fn tu_init_calls(flags: SanitizerFlags, span: &Span) -> Vec<BlockItem> {
    let mut out = Vec::new();
    if flags.kasan {
        out.push(sanitizer_call("__asan_init", span));
    }
    if flags.ubsan {
        out.push(sanitizer_call("__ubsan_init", span));
    }
    if flags.kcsan {
        out.push(sanitizer_call("__kcsan_init", span));
    }
    out
}

/// 每个函数入口桩（coverage / KCSAN；KASAN/UBSAN 完整版需指令级插桩）。
fn func_entry_calls(flags: SanitizerFlags, span: &Span) -> Vec<BlockItem> {
    let mut out = Vec::new();
    if flags.kcov {
        out.push(sanitizer_call("__sanitizer_cov_trace_pc", span));
    }
    if flags.kcsan {
        out.push(sanitizer_call("__kcsan_func_entry", span));
    }
    out
}

fn inject_func_prologue(func: &mut FuncDecl, prologue: &[BlockItem]) {
    if let Some(body) = &mut func.body {
        let mut items = prologue.to_vec();
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

/// `.dcc.san_info` 节载荷（格式 v1：magic + 4×u8 标志位）。
pub fn sanitizer_metadata(flags: SanitizerFlags) -> Vec<u8> {
    let mut out = b"DCC_SAN\0".to_vec();
    out.push(1); // 格式版本
    out.push(flags.kasan as u8);
    out.push(flags.ubsan as u8);
    out.push(flags.kcsan as u8);
    out.push(flags.kcov as u8);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Block, FuncDecl, Program};

    fn empty_func(name: &str) -> FuncDecl {
        FuncDecl {
            ret_ty: Type::Int,
            name: Ident { name: name.into(), span: Span::point("t.c", 1, 1) },
            params: vec![],
            body: Some(Block {
                items: vec![BlockItem::Stmt(Stmt::Return {
                    val: None,
                    span: Span::point("t.c", 2, 1),
                })],
                span: Span::point("t.c", 1, 1),
            }),
            span: Span::point("t.c", 1, 1),
            is_extern: false,
            is_static: false,
            section: None,
            alignment: None,
            is_weak: false,
            visibility: None,
            is_unused: false,
            is_cold: false,
            is_noinline: false,
            is_always_inline: false,
            is_variadic: false,
            is_constructor: false,
            is_destructor: false,
            alias_name: None,
            is_nested: false,
            upvars: vec![],
        }
    }

    fn call_names(body: &Block) -> Vec<String> {
        body
            .items
            .iter()
            .filter_map(|item| {
                if let BlockItem::Stmt(Stmt::Expr {
                    e: Expr::Call { callee, .. },
                    ..
                }) = item
                {
                    if let Expr::Ident { name, .. } = callee.as_ref() {
                        return Some(name.name.clone());
                    }
                }
                None
            })
            .collect()
    }

    #[test]
    fn from_args_parses_combined_sanitize() {
        let f = SanitizerFlags::from_args(&[
            "-fsanitize=address,undefined".to_string(),
            "-fsanitize-coverage".to_string(),
        ]);
        assert!(f.kasan && f.ubsan && f.kcov);
    }

    #[test]
    fn tu_init_only_in_first_function() {
        let mut prog = Program {
            decls: vec![
                Decl::Func(empty_func("first")),
                Decl::Func(empty_func("second")),
            ],
        };
        instrument_program(
            &mut prog,
            SanitizerFlags { kasan: true, kcov: true, ..Default::default() },
        );
        let Decl::Func(f0) = &prog.decls[0] else { panic!() };
        let Decl::Func(f1) = &prog.decls[1] else { panic!() };
        let n0 = call_names(f0.body.as_ref().unwrap());
        let n1 = call_names(f1.body.as_ref().unwrap());
        assert!(n0.iter().any(|n| n == "__asan_init"));
        assert_eq!(n0.iter().filter(|n| *n == "__asan_init").count(), 1);
        assert!(!n1.iter().any(|n| n == "__asan_init"));
        assert!(n0.iter().any(|n| n == "__sanitizer_cov_trace_pc"));
        assert!(n1.iter().any(|n| n == "__sanitizer_cov_trace_pc"));
    }

    #[test]
    fn every_function_gets_kcsan_entry() {
        let mut prog = Program {
            decls: vec![
                Decl::Func(empty_func("a")),
                Decl::Func(empty_func("b")),
            ],
        };
        instrument_program(&mut prog, SanitizerFlags { kcsan: true, ..Default::default() });
        for decl in &prog.decls {
            let Decl::Func(f) = decl else { continue };
            let names = call_names(f.body.as_ref().unwrap());
            assert!(names.iter().any(|n| n == "__kcsan_init") || names.iter().any(|n| n == "__kcsan_func_entry"));
        }
        let Decl::Func(f1) = &prog.decls[1] else { panic!() };
        let names = call_names(f1.body.as_ref().unwrap());
        assert!(!names.iter().any(|n| n == "__kcsan_init"));
        assert!(names.iter().any(|n| n == "__kcsan_func_entry"));
    }

    #[test]
    fn metadata_includes_version_byte() {
        let meta = sanitizer_metadata(SanitizerFlags { kasan: true, ..Default::default() });
        assert_eq!(&meta[..8], b"DCC_SAN\0");
        assert_eq!(meta[8], 1);
        assert_eq!(meta[9], 1);
    }
}
