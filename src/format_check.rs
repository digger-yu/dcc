//! `-Wformat`: 对带 `__attribute__((format))` 的函数调用做完整格式检查。
//! 支持 flags (-+ #0 )、width、precision、length modifier (h/hh/l/ll/z/j/t/L)
//! 和 conversion specifier 类型匹配。

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

fn check_block(
    items: &[crate::ast::BlockItem],
    funcs: &std::collections::HashMap<String, &FuncDecl>,
    out: &mut Vec<Diagnostic>,
) {
    for item in items {
        match item {
            crate::ast::BlockItem::Stmt(s) => check_stmt(s, funcs, out),
            crate::ast::BlockItem::Decl(Decl::Func(f)) => {
                if let Some(body) = &f.body {
                    check_block(&body.items, funcs, out);
                }
            },
            _ => {},
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

fn check_stmt(
    s: &Stmt,
    funcs: &std::collections::HashMap<String, &FuncDecl>,
    out: &mut Vec<Diagnostic>,
) {
    match s {
        Stmt::Expr { e, span } => check_expr(e, funcs, out, span),
        Stmt::Return { val: Some(e), span } => check_expr(e, funcs, out, span),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            check_expr(cond, funcs, out, &expr_span(cond));
            check_stmt(then_, funcs, out);
            if let Some(e) = else_ {
                check_stmt(e, funcs, out);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            check_expr(cond, funcs, out, &expr_span(cond));
            check_stmt(body, funcs, out);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
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
        },
        Stmt::Block { b, .. } => check_block(&b.items, funcs, out),
        _ => {},
    }
}

fn check_expr(
    e: &Expr,
    funcs: &std::collections::HashMap<String, &FuncDecl>,
    out: &mut Vec<Diagnostic>,
    _span: &Span,
) {
    if let Expr::Call {
        callee, args, span, ..
    } = e
    {
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
        },
        Expr::Unary { e: inner, .. } => check_expr(inner, funcs, out, &expr_span(inner)),
        Expr::Assign { l, r, .. } => {
            check_expr(l, funcs, out, &expr_span(l));
            check_expr(r, funcs, out, &expr_span(r));
        },
        _ => {},
    }
}

fn check_format_call(f: &FuncDecl, args: &[Expr], span: &Span, out: &mut Vec<Diagnostic>) {
    let fmt_idx = f.format_string_idx.unwrap_or(1) as usize;
    let first_arg = f.format_first_arg.unwrap_or(2) as usize;
    let is_scanf = f.format_kind.as_deref() == Some("scanf");
    if fmt_idx == 0 || fmt_idx > args.len() {
        out.push(
            Diagnostic::warn(
                "W0003",
                format!(
                    "format attribute on '{}' expects format string at argument {}",
                    f.name.name, fmt_idx
                ),
                span.clone(),
            )
            .with_hint("add a string literal format argument at the declared index"),
        );
        return;
    }
    let fmt_lit = format_string_literal(&args[fmt_idx - 1]);
    let Some(fmt) = fmt_lit else {
        return;
    };
    let specs = parse_format_specs(&fmt);
    // 计算需要的参数数 (包括 * width/precision 消耗的额外参数)
    let mut needed = 0usize;
    for spec in &specs {
        needed += 1; // 转换说明符本身需要一个参数
        if spec.width_star {
            needed += 1;
        }
        if spec.prec_star {
            needed += 1;
        }
    }
    let provided = args.len().saturating_sub(first_arg.saturating_sub(1));
    if provided < needed {
        out.push(
            Diagnostic::warn(
                "W0003",
                format!(
                    "format '{}' expects at least {} argument(s) for conversion specifiers, got {}",
                    f.name.name, needed, provided
                ),
                span.clone(),
            )
            .with_hint("check printf/scanf format string and argument list"),
        );
    }
    // 类型匹配检查
    let arg_tys: Vec<Option<crate::ast::Type>> = args.iter().map(|a| Some(a.ty())).collect();
    let mut arg_pos = first_arg.saturating_sub(1);
    for spec in &specs {
        if spec.width_star && arg_pos < arg_tys.len() {
            // * width 需要一个 int 参数
            arg_pos += 1;
        }
        if spec.prec_star && arg_pos < arg_tys.len() {
            // * precision 需要一个 int 参数
            arg_pos += 1;
        }
        if arg_pos < arg_tys.len() {
            check_format_type(spec, arg_tys[arg_pos].as_ref(), is_scanf, span, &f.name.name, out);
        }
        arg_pos += 1;
    }
}

/// 解析后的格式说明符
struct FmtSpec {
    _flags: String,
    width_star: bool,
    prec_star: bool,
    length: String,
    conv: char,
}

/// 解析格式字符串中的所有转换说明符
fn parse_format_specs(fmt: &str) -> Vec<FmtSpec> {
    let bytes = fmt.as_bytes();
    let mut i = 0;
    let mut specs = Vec::new();
    while i < bytes.len() {
        if bytes[i] != b'%' {
            i += 1;
            continue;
        }
        i += 1;
        if i >= bytes.len() {
            break;
        }
        if bytes[i] == b'%' {
            i += 1;
            continue;
        } // %%
          // 解析 flags
        let mut flags = String::new();
        while i < bytes.len() && b"-+ #0".contains(&bytes[i]) {
            flags.push(bytes[i] as char);
            i += 1;
        }
        // 解析 width
        let mut width_star = false;
        if i < bytes.len() && bytes[i] == b'*' {
            width_star = true;
            i += 1;
        } else {
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
        // 解析 precision
        let mut prec_star = false;
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            if i < bytes.len() && bytes[i] == b'*' {
                prec_star = true;
                i += 1;
            } else {
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
        }
        // 解析 length modifier
        let mut length = String::new();
        if i < bytes.len() {
            match bytes[i] {
                b'h' => {
                    length.push('h');
                    i += 1;
                    if i < bytes.len() && bytes[i] == b'h' {
                        length.push('h');
                        i += 1;
                    }
                },
                b'l' => {
                    length.push('l');
                    i += 1;
                    if i < bytes.len() && bytes[i] == b'l' {
                        length.push('l');
                        i += 1;
                    }
                },
                b'z' | b'j' | b't' | b'L' => {
                    length.push(bytes[i] as char);
                    i += 1;
                },
                _ => {},
            }
        }
        // 解析 conversion specifier
        if i < bytes.len() {
            let conv = bytes[i] as char;
            // C23 %b/%B: 无符号二进制格式 (同 %u 参数类型)
            if "diouxXbBfFeEgGaAcCsSpnm".contains(conv) {
                specs.push(FmtSpec {
                    _flags: flags,
                    width_star,
                    prec_star,
                    length,
                    conv,
                });
            }
            i += 1;
        }
    }
    specs
}

/// 检查单个转换说明符的参数类型是否匹配
fn check_format_type(
    spec: &FmtSpec,
    arg_ty: Option<&crate::ast::Type>,
    is_scanf: bool,
    span: &Span,
    func_name: &str,
    out: &mut Vec<Diagnostic>,
) {
    use crate::ast::Type;
    let Some(ty) = arg_ty else {
        return;
    };
    // scanf 参数应该是指针类型
    let effective_ty = if is_scanf {
        match ty {
            Type::Ptr(inner) => &inner.base,
            _ => {
                out.push(Diagnostic::warn(
                    "W0003",
                    format!(
                        "format '%{}{}' expects pointer argument, but got non-pointer type",
                        spec.length, spec.conv
                    ),
                    span.clone(),
                ));
                return;
            },
        }
    } else {
        ty
    };
    let mismatch = match spec.conv {
        'd' | 'i' => !matches!(
            effective_ty,
            Type::Int
                | Type::Short
                | Type::UShort
                | Type::Char
                | Type::SChar
                | Type::UChar
                | Type::Long
                | Type::ULong
                | Type::LongLong
                | Type::ULongLong
                | Type::Bool
                | Type::Enum(_, _, _)
        ),
        'u' | 'o' | 'x' | 'X' | 'b' | 'B' => !matches!(
            effective_ty,
            Type::UInt
                | Type::Int
                | Type::Short
                | Type::UShort
                | Type::Char
                | Type::SChar
                | Type::UChar
                | Type::ULong
                | Type::Long
                | Type::ULongLong
                | Type::LongLong
                | Type::Bool
                | Type::Enum(_, _, _)
        ),
        'f' | 'F' | 'e' | 'E' | 'g' | 'G' | 'a' | 'A' => {
            if spec.length == "L" {
                !matches!(effective_ty, Type::LongDouble | Type::Double | Type::Float)
            } else {
                !matches!(effective_ty, Type::Double | Type::Float)
            }
        },
        // %c accepts integer types (char promoted to int in variadic args)
        'c' => !matches!(
            effective_ty,
            Type::Int
                | Type::UInt
                | Type::Char
                | Type::SChar
                | Type::UChar
                | Type::Short
                | Type::UShort
                | Type::Long
                | Type::ULong
                | Type::Enum(_, _, _)
        ),
        // %s accepts any pointer (char*) or array (decays to pointer)
        's' => !matches!(effective_ty, Type::Ptr(_) | Type::Array(_, _)),
        // %p accepts any pointer, array (decays to ptr), or function (decays to fn ptr).
        // 内核 %p 扩展 (%pS/%pF/%pU 等) 也接受函数指针和各类指针.
        'p' => !matches!(effective_ty, Type::Ptr(_) | Type::Array(_, _) | Type::Func(_, _)),
        'n' => !matches!(effective_ty, Type::Ptr(_)),
        _ => false,
    };
    if mismatch {
        out.push(
            Diagnostic::warn(
                "W0003",
                format!(
                    "format '%{}{}' expects {} argument, but argument has incompatible type",
                    spec.length,
                    spec.conv,
                    match spec.conv {
                        'd' | 'i' => "integer",
                        'u' | 'o' | 'x' | 'X' | 'b' | 'B' => "unsigned integer",
                        'f' | 'F' | 'e' | 'E' | 'g' | 'G' | 'a' | 'A' => "floating-point",
                        'c' => "character",
                        's' => "string",
                        'p' => "pointer",
                        _ => "compatible",
                    }
                ),
                span.clone(),
            )
            .with_hint(format!("check argument type for '{}' call", func_name)),
        );
    }
}

fn format_string_literal(e: &Expr) -> Option<String> {
    match e {
        Expr::String { data, .. } => Some(String::from_utf8_lossy(data).into_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Block, BlockItem, Ident, Param, Type};
    use crate::diagnostics::Span;

    // M24: parse_format_specs —— 基础 conversion
    #[test]
    fn parse_simple_conversion() {
        let specs = parse_format_specs("%d");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].conv, 'd');
        assert_eq!(specs[0].length, "");
        assert!(!specs[0].width_star);
        assert!(!specs[0].prec_star);
    }

    // %% 不算 conversion
    #[test]
    fn parse_double_percent_skipped() {
        let specs = parse_format_specs("100%% %d");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].conv, 'd');
    }

    // flags: -+ #0 空格
    #[test]
    fn parse_flags_consumed() {
        let specs = parse_format_specs("%-+#0 d");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].conv, 'd');
        // 顺序: - + # 0 空格
        assert_eq!(specs[0]._flags, "-+#0 ");
    }

    // width 和 precision 数字
    #[test]
    fn parse_width_and_precision_digits() {
        let specs = parse_format_specs("%10.3f");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].conv, 'f');
        assert!(!specs[0].width_star);
        assert!(!specs[0].prec_star);
    }

    // width 和 precision 用 * 消耗额外参数
    #[test]
    fn parse_star_width_and_precision() {
        let specs = parse_format_specs("%*.*d");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].conv, 'd');
        assert!(specs[0].width_star);
        assert!(specs[0].prec_star);
    }

    // length modifier: hh / h / l / ll / z / j / t / L
    #[test]
    fn parse_length_modifiers() {
        assert_eq!(parse_format_specs("%hhd")[0].length, "hh");
        assert_eq!(parse_format_specs("%hd")[0].length, "h");
        assert_eq!(parse_format_specs("%ld")[0].length, "l");
        assert_eq!(parse_format_specs("%lld")[0].length, "ll");
        assert_eq!(parse_format_specs("%zd")[0].length, "z");
        assert_eq!(parse_format_specs("%jd")[0].length, "j");
        assert_eq!(parse_format_specs("%td")[0].length, "t");
        assert_eq!(parse_format_specs("%Le")[0].length, "L");
    }

    // 多个 spec + 普通文本混合
    #[test]
    fn parse_mixed_specs_and_text() {
        let specs = parse_format_specs("name=%s age=%d hex=%x\n");
        assert_eq!(specs.len(), 3);
        assert_eq!(specs[0].conv, 's');
        assert_eq!(specs[1].conv, 'd');
        assert_eq!(specs[2].conv, 'x');
    }

    // check_format_type: int 配 d/i 通过
    #[test]
    fn check_type_int_matches_d() {
        let spec = FmtSpec {
            _flags: String::new(),
            width_star: false,
            prec_star: false,
            length: String::new(),
            conv: 'd',
        };
        let mut out = Vec::new();
        let span = Span::point("t.c", 1, 1);
        check_format_type(&spec, Some(&Type::Int), false, &span, "log", &mut out);
        assert!(out.is_empty(), "int should match %d: {out:?}");
    }

    // check_format_type: int 配 f 应报告 mismatch
    #[test]
    fn check_type_int_mismatch_f() {
        let spec = FmtSpec {
            _flags: String::new(),
            width_star: false,
            prec_star: false,
            length: String::new(),
            conv: 'f',
        };
        let mut out = Vec::new();
        let span = Span::point("t.c", 1, 1);
        check_format_type(&spec, Some(&Type::Int), false, &span, "log", &mut out);
        assert_eq!(out.len(), 1, "int should NOT match %f");
        assert!(out[0].msg.contains("floating-point"), "msg: {}", out[0].msg);
    }

    // check_format_type: scanf 非指针应警告
    #[test]
    fn check_type_scanf_non_pointer_warns() {
        let spec = FmtSpec {
            _flags: String::new(),
            width_star: false,
            prec_star: false,
            length: String::new(),
            conv: 'd',
        };
        let mut out = Vec::new();
        let span = Span::point("t.c", 1, 1);
        check_format_type(&spec, Some(&Type::Int), true, &span, "scan", &mut out);
        assert_eq!(out.len(), 1, "scanf expects pointer, got int");
        assert!(out[0].msg.contains("pointer"), "msg: {}", out[0].msg);
    }

    // check_format_type: scanf 指针应通过
    #[test]
    fn check_type_scanf_pointer_passes() {
        let spec = FmtSpec {
            _flags: String::new(),
            width_star: false,
            prec_star: false,
            length: String::new(),
            conv: 'd',
        };
        let mut out = Vec::new();
        let span = Span::point("t.c", 1, 1);
        check_format_type(
            &spec,
            Some(&Type::Ptr(Box::new(Type::Int.into()))),
            true,
            &span,
            "scan",
            &mut out,
        );
        assert!(out.is_empty(), "scanf pointer should pass: {out:?}");
    }

    // check_format_type: char* 配 %s 通过
    #[test]
    fn check_type_string_matches_s() {
        let spec = FmtSpec {
            _flags: String::new(),
            width_star: false,
            prec_star: false,
            length: String::new(),
            conv: 's',
        };
        let mut out = Vec::new();
        let span = Span::point("t.c", 1, 1);
        check_format_type(
            &spec,
            Some(&Type::Ptr(Box::new(Type::Char.into()))),
            false,
            &span,
            "log",
            &mut out,
        );
        assert!(out.is_empty(), "char* should match %s: {out:?}");
    }

    // check_format_type: long double 配 %Lf 通过
    #[test]
    fn check_type_longdouble_matches_lf() {
        let spec = FmtSpec {
            _flags: String::new(),
            width_star: false,
            prec_star: false,
            length: "L".to_string(),
            conv: 'f',
        };
        let mut out = Vec::new();
        let span = Span::point("t.c", 1, 1);
        check_format_type(&spec, Some(&Type::LongDouble), false, &span, "log", &mut out);
        assert!(out.is_empty(), "long double should match %Lf: {out:?}");
    }

    // end-to-end: check_program 对 %d 传 int 不告警
    #[test]
    fn check_program_no_warning_for_correct_types() {
        let prog = Program {
            decls: vec![Decl::Func(FuncDecl {
                ret_ty: Type::Void,
                name: Ident {
                    name: "log".to_string(),
                    span: Span::point("t.c", 1, 1),
                },
                params: vec![Param {
                    quals: crate::ast::QualifierFlags::NONE,
                    ty: Type::Ptr(Box::new(Type::Char.into())),
                    name: Some(Ident {
                        name: "fmt".to_string(),
                        span: Span::point("t.c", 1, 1),
                    }),
                    span: Span::point("t.c", 1, 1),
                    vla_dim: None,
                    home: None,
                }],
                body: Some(Block {
                    items: vec![BlockItem::Stmt(Stmt::Expr {
                        e: Expr::Call {
                            callee: Box::new(Expr::Ident {
                                name: Ident {
                                    name: "log".to_string(),
                                    span: Span::point("t.c", 1, 1),
                                },
                                span: Span::point("t.c", 1, 1),
                                ty: Type::Void,
                            }),
                            args: vec![
                                Expr::String {
                                    data: b"value=%d".to_vec(),
                                    span: Span::point("t.c", 2, 5),
                                    ty: Type::Ptr(Box::new(Type::Char.into())),
                                },
                                Expr::Int {
                                    v: 42,
                                    span: Span::point("t.c", 2, 17),
                                    ty: Type::Int,
                                },
                            ],
                            span: Span::point("t.c", 2, 5),
                            ty: Type::Void,
                        },
                        span: Span::point("t.c", 2, 5),
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
                is_hot: false,
                is_noinline: false,
                is_always_inline: false,
                is_inline: false,
                is_variadic: true,
                is_constructor: false,
                is_destructor: false,
                alias_name: None,
                is_nested: false,
                upvars: vec![],
                is_old_style: false,
                format_kind: Some("printf".to_string()),
                format_string_idx: Some(1),
                format_first_arg: Some(2),
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
                asm_output_template: None,
                is_global: false,
                is_device: false,
                is_host: false,
                is_naked: false,
                is_noreturn: false,
                is_pure: false,
                is_const_attr: false,
                returns_twice: false,
                no_instrument: false,
                is_malloc: false,
                malloc_dealloc: None,
                malloc_dealloc_argno: None,
                alloc_size: None,
                assume_aligned: None,
                warn_unused_result: false,
                is_sentinel: false,
                format_arg_idx: None,
                is_nodiscard: false,
                asm_label: None,
            })],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let diags = check_program(&prog);
        assert!(diags.is_empty(), "expected no warnings, got: {diags:?}");
    }

    // end-to-end: check_program 对参数不足应告警
    #[test]
    fn check_program_warns_on_too_few_args() {
        let prog = Program {
            decls: vec![Decl::Func(FuncDecl {
                ret_ty: Type::Void,
                name: Ident {
                    name: "log".to_string(),
                    span: Span::point("t.c", 1, 1),
                },
                params: vec![Param {
                    quals: crate::ast::QualifierFlags::NONE,
                    ty: Type::Ptr(Box::new(Type::Char.into())),
                    name: Some(Ident {
                        name: "fmt".to_string(),
                        span: Span::point("t.c", 1, 1),
                    }),
                    span: Span::point("t.c", 1, 1),
                    vla_dim: None,
                    home: None,
                }],
                body: Some(Block {
                    items: vec![BlockItem::Stmt(Stmt::Expr {
                        e: Expr::Call {
                            callee: Box::new(Expr::Ident {
                                name: Ident {
                                    name: "log".to_string(),
                                    span: Span::point("t.c", 1, 1),
                                },
                                span: Span::point("t.c", 1, 1),
                                ty: Type::Void,
                            }),
                            args: vec![Expr::String {
                                data: b"%d %s".to_vec(),
                                span: Span::point("t.c", 2, 5),
                                ty: Type::Ptr(Box::new(Type::Char.into())),
                            }],
                            span: Span::point("t.c", 2, 5),
                            ty: Type::Void,
                        },
                        span: Span::point("t.c", 2, 5),
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
                is_hot: false,
                is_noinline: false,
                is_always_inline: false,
                is_inline: false,
                is_variadic: true,
                is_constructor: false,
                is_destructor: false,
                alias_name: None,
                is_nested: false,
                upvars: vec![],
                is_old_style: false,
                format_kind: Some("printf".to_string()),
                format_string_idx: Some(1),
                format_first_arg: Some(2),
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
                asm_output_template: None,
                is_global: false,
                is_device: false,
                is_host: false,
                is_naked: false,
                is_noreturn: false,
                is_pure: false,
                is_const_attr: false,
                returns_twice: false,
                no_instrument: false,
                is_malloc: false,
                malloc_dealloc: None,
                malloc_dealloc_argno: None,
                alloc_size: None,
                assume_aligned: None,
                warn_unused_result: false,
                is_sentinel: false,
                format_arg_idx: None,
                is_nodiscard: false,
                asm_label: None,
            })],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let diags = check_program(&prog);
        assert!(!diags.is_empty(), "expected warning for missing args");
        assert!(diags[0].msg.contains("at least 2"), "msg: {}", diags[0].msg);
    }

    // C23: %b 解析为 binary 格式
    #[test]
    fn parse_binary_b() {
        let specs = parse_format_specs("%b");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].conv, 'b');
    }

    // C23: %B 解析为 BINARY 格式
    #[test]
    fn parse_binary_upper_b() {
        let specs = parse_format_specs("%B");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].conv, 'B');
    }

    // C23: unsigned int 匹配 %b 通过
    #[test]
    fn check_type_uint_matches_b() {
        let spec = FmtSpec {
            _flags: String::new(),
            width_star: false,
            prec_star: false,
            length: String::new(),
            conv: 'b',
        };
        let mut out = Vec::new();
        let span = Span::point("t.c", 1, 1);
        check_format_type(&spec, Some(&Type::UInt), false, &span, "log", &mut out);
        assert!(out.is_empty(), "unsigned int should match %b: {out:?}");
    }

    // C23: unsigned int 匹配 %B 通过
    #[test]
    fn check_type_uint_matches_upper_b() {
        let spec = FmtSpec {
            _flags: String::new(),
            width_star: false,
            prec_star: false,
            length: String::new(),
            conv: 'B',
        };
        let mut out = Vec::new();
        let span = Span::point("t.c", 1, 1);
        check_format_type(&spec, Some(&Type::UInt), false, &span, "log", &mut out);
        assert!(out.is_empty(), "unsigned int should match %B: {out:?}");
    }

    // C23: double 匹配 %b 应报错 (期望 unsigned integer)
    #[test]
    fn check_type_double_mismatch_b() {
        let spec = FmtSpec {
            _flags: String::new(),
            width_star: false,
            prec_star: false,
            length: String::new(),
            conv: 'b',
        };
        let mut out = Vec::new();
        let span = Span::point("t.c", 1, 1);
        check_format_type(&spec, Some(&Type::Double), false, &span, "log", &mut out);
        assert_eq!(out.len(), 1, "double should NOT match %b");
        assert!(out[0].msg.contains("unsigned integer"), "msg: {}", out[0].msg);
    }

    // C23: int 匹配 %B 通过 (GCC 兼容: 接受有符号整型)
    #[test]
    fn check_type_int_matches_upper_b() {
        let spec = FmtSpec {
            _flags: String::new(),
            width_star: false,
            prec_star: false,
            length: String::new(),
            conv: 'B',
        };
        let mut out = Vec::new();
        let span = Span::point("t.c", 1, 1);
        check_format_type(&spec, Some(&Type::Int), false, &span, "log", &mut out);
        assert!(out.is_empty(), "int should match %B: {out:?}");
    }
}
