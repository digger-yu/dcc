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
    let is_scanf = f.format_kind.as_deref() == Some("scanf");
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
    let specs = parse_format_specs(&fmt);
    // 计算需要的参数数 (包括 * width/precision 消耗的额外参数)
    let mut needed = 0usize;
    for spec in &specs {
        needed += 1; // 转换说明符本身需要一个参数
        if spec.width_star { needed += 1; }
        if spec.prec_star { needed += 1; }
    }
    let provided = args.len().saturating_sub(first_arg.saturating_sub(1));
    if provided < needed {
        out.push(Diagnostic::warn(
            "W0003",
            format!(
                "format '{}' expects at least {} argument(s) for conversion specifiers, got {}",
                f.name.name, needed, provided
            ),
            span.clone(),
        )
        .with_hint("check printf/scanf format string and argument list"));
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
        if i >= bytes.len() { break; }
        if bytes[i] == b'%' { i += 1; continue; } // %%
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
            while i < bytes.len() && bytes[i].is_ascii_digit() { i += 1; }
        }
        // 解析 precision
        let mut prec_star = false;
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            if i < bytes.len() && bytes[i] == b'*' {
                prec_star = true;
                i += 1;
            } else {
                while i < bytes.len() && bytes[i].is_ascii_digit() { i += 1; }
            }
        }
        // 解析 length modifier
        let mut length = String::new();
        if i < bytes.len() {
            match bytes[i] {
                b'h' => {
                    length.push('h'); i += 1;
                    if i < bytes.len() && bytes[i] == b'h' { length.push('h'); i += 1; }
                }
                b'l' => {
                    length.push('l'); i += 1;
                    if i < bytes.len() && bytes[i] == b'l' { length.push('l'); i += 1; }
                }
                b'z' | b'j' | b't' | b'L' => {
                    length.push(bytes[i] as char); i += 1;
                }
                _ => {}
            }
        }
        // 解析 conversion specifier
        if i < bytes.len() {
            let conv = bytes[i] as char;
            if "diouxXfFeEgGaAcCsSpnm".contains(conv) {
                specs.push(FmtSpec { _flags: flags, width_star, prec_star, length, conv });
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
    let Some(ty) = arg_ty else { return; };
    // scanf 参数应该是指针类型
    let effective_ty = if is_scanf {
        match ty {
            Type::Ptr(inner) => inner.as_ref(),
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
            }
        }
    } else {
        ty
    };
    let mismatch = match spec.conv {
        'd' | 'i' => !matches!(effective_ty, Type::Int | Type::Short | Type::Char | Type::Long),
        'u' | 'o' | 'x' | 'X' => !matches!(effective_ty, Type::UInt | Type::Int | Type::Short | Type::Char | Type::ULong | Type::Long),
        'f' | 'F' | 'e' | 'E' | 'g' | 'G' | 'a' | 'A' => {
            if spec.length == "L" {
                !matches!(effective_ty, Type::LongDouble | Type::Double | Type::Float)
            } else {
                !matches!(effective_ty, Type::Double | Type::Float)
            }
        }
        'c' | 's' => !matches!(effective_ty, Type::Ptr(_)),
        'p' => !matches!(effective_ty, Type::Ptr(_)),
        'n' => !matches!(effective_ty, Type::Ptr(_)),
        _ => false,
    };
    if mismatch {
        out.push(Diagnostic::warn(
            "W0003",
            format!(
                "format '%{}{}' expects {} argument, but argument has incompatible type",
                spec.length, spec.conv,
                match spec.conv {
                    'd' | 'i' => "integer",
                    'u' | 'o' | 'x' | 'X' => "unsigned integer",
                    'f' | 'F' | 'e' | 'E' | 'g' | 'G' | 'a' | 'A' => "floating-point",
                    'c' => "character",
                    's' => "string",
                    'p' => "pointer",
                    _ => "compatible",
                }
            ),
            span.clone(),
        )
        .with_hint(format!("check argument type for '{}' call", func_name)));
    }
}

fn format_string_literal(e: &Expr) -> Option<String> {
    match e {
        Expr::String { data, .. } => Some(String::from_utf8_lossy(data).into_owned()),
        _ => None,
    }
}
