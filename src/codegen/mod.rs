// src/codegen/mod.rs - 代码生成调度
// 第一性原理: codegen 入口只做"按 target 选后端"。

pub mod aarch64;
pub mod arm;
pub mod asm_constraints;
pub mod coff;
pub mod disasm;
pub mod elf;
pub mod mini_asm;
pub mod mips;
pub mod mips64;
pub mod riscv64;
pub mod x86_64;

use crate::ast::{Block, BlockItem, Decl, Designator, Expr, InitItem, Program, Stmt, Type};
use crate::const_eval::{self, ConstValue};
use crate::debug;
use crate::diagnostics::{Diagnostic, Span};
use crate::ir::Module;
use crate::preproc::LineSpan;
use crate::sanitizer::{sanitizer_metadata, SanitizerFlags};
use crate::target::Target;

pub fn validate_inline_asm_constraints(module: &Module) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    validate_inline_asm_program(module.source(), &mut diags);
    diags
}

fn validate_inline_asm_program(prog: &Program, diags: &mut Vec<Diagnostic>) {
    for decl in &prog.decls {
        validate_inline_asm_decl(decl, diags);
    }
}

fn validate_inline_asm_decl(decl: &Decl, diags: &mut Vec<Diagnostic>) {
    match decl {
        Decl::Func(f) => {
            if let Some(body) = &f.body {
                validate_inline_asm_block(body, diags);
            }
        }
        Decl::Var(v) => {
            if let Some(init) = &v.init {
                validate_inline_asm_expr(init, diags);
            }
        }
        Decl::GlobalAsm {
            outputs,
            output_constraints,
            inputs,
            input_constraints,
            span,
            ..
        } => validate_inline_asm_operands(
            outputs,
            output_constraints,
            inputs,
            input_constraints,
            span,
            diags,
        ),
        _ => {}
    }
}

fn validate_inline_asm_block(block: &Block, diags: &mut Vec<Diagnostic>) {
    for item in &block.items {
        match item {
            BlockItem::Stmt(stmt) => validate_inline_asm_stmt(stmt, diags),
            BlockItem::Decl(decl) => validate_inline_asm_decl(decl, diags),
        }
    }
}

fn validate_inline_asm_stmt(stmt: &Stmt, diags: &mut Vec<Diagnostic>) {
    match stmt {
        Stmt::Return { val, .. } => {
            if let Some(e) = val {
                validate_inline_asm_expr(e, diags);
            }
        }
        Stmt::Expr { e, .. } => validate_inline_asm_expr(e, diags),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            validate_inline_asm_expr(cond, diags);
            validate_inline_asm_stmt(then_, diags);
            if let Some(e) = else_ {
                validate_inline_asm_stmt(e, diags);
            }
        }
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            validate_inline_asm_expr(cond, diags);
            validate_inline_asm_stmt(body, diags);
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(init) = init {
                validate_inline_asm_stmt(init, diags);
            }
            if let Some(cond) = cond {
                validate_inline_asm_expr(cond, diags);
            }
            if let Some(step) = step {
                validate_inline_asm_expr(step, diags);
            }
            validate_inline_asm_stmt(body, diags);
        }
        Stmt::Block { b, .. } => validate_inline_asm_block(b, diags),
        Stmt::GotoIndirect { expr, .. } => validate_inline_asm_expr(expr, diags),
        Stmt::Label { stmt, .. } => validate_inline_asm_stmt(stmt, diags),
        Stmt::Switch { expr, cases, .. } => {
            validate_inline_asm_expr(expr, diags);
            for case in cases {
                if let Some(val) = &case.val {
                    validate_inline_asm_expr(val, diags);
                }
                if let Some(val) = &case.range_end {
                    validate_inline_asm_expr(val, diags);
                }
                validate_inline_asm_block(&case.body, diags);
            }
        }
        Stmt::AsmGoto {
            outputs,
            output_constraints,
            inputs,
            input_constraints,
            span,
            ..
        } => {
            validate_inline_asm_operands(
                outputs,
                output_constraints,
                inputs,
                input_constraints,
                span,
                diags,
            );
        }
        Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Goto { .. } => {}
    }
}

fn validate_inline_asm_expr(expr: &Expr, diags: &mut Vec<Diagnostic>) {
    match expr {
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            validate_inline_asm_expr(l, diags);
            validate_inline_asm_expr(r, diags);
        }
        Expr::Unary { e, .. }
        | Expr::Cast { e, .. }
        | Expr::SizeofExpr { e, .. }
        | Expr::AddrOf { e, .. }
        | Expr::Deref { e, .. }
        | Expr::PostInc { e, .. }
        | Expr::PostDec { e, .. }
        | Expr::PreInc { e, .. }
        | Expr::PreDec { e, .. }
        | Expr::AlignofExpr { e, .. }
        | Expr::Real { e, .. }
        | Expr::Imag { e, .. } => validate_inline_asm_expr(e, diags),
        Expr::Call { callee, args, .. } => {
            validate_inline_asm_expr(callee, diags);
            for arg in args {
                validate_inline_asm_expr(arg, diags);
            }
        }
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            validate_inline_asm_expr(cond, diags);
            validate_inline_asm_expr(then_, diags);
            validate_inline_asm_expr(else_, diags);
        }
        Expr::Index { arr, idx, .. } => {
            validate_inline_asm_expr(arr, diags);
            validate_inline_asm_expr(idx, diags);
        }
        Expr::Member { expr, .. } => validate_inline_asm_expr(expr, diags),
        Expr::StmtExpr { block, .. } => validate_inline_asm_block(block, diags),
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for init in inits {
                validate_inline_asm_init(init, diags);
            }
        }
        Expr::AsmExpr {
            outputs,
            output_constraints,
            inputs,
            input_constraints,
            span,
            ..
        } => {
            validate_inline_asm_operands(
                outputs,
                output_constraints,
                inputs,
                input_constraints,
                span,
                diags,
            );
        }
        Expr::VaArg { ap, .. } => validate_inline_asm_expr(ap, diags),
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::ComplexLit { .. }
        | Expr::String { .. }
        | Expr::Char { .. }
        | Expr::Ident { .. }
        | Expr::SizeofTy { .. }
        | Expr::AlignofTy { .. }
        | Expr::LabelAddr { .. }
        | Expr::Offsetof { .. } => {}
    }
}

fn validate_inline_asm_init(init: &InitItem, diags: &mut Vec<Diagnostic>) {
    match init {
        InitItem::Expr(e) => validate_inline_asm_expr(e, diags),
        InitItem::Designated { designator, value } => {
            validate_inline_asm_designator(designator, diags);
            validate_inline_asm_expr(value, diags);
        }
    }
}

fn validate_inline_asm_designator(designator: &Designator, diags: &mut Vec<Diagnostic>) {
    match designator {
        Designator::Index(e) => validate_inline_asm_expr(e, diags),
        Designator::Range(l, r) => {
            validate_inline_asm_expr(l, diags);
            validate_inline_asm_expr(r, diags);
        }
        Designator::Chain(items) => {
            for item in items {
                validate_inline_asm_designator(item, diags);
            }
        }
        Designator::Field(_) => {}
    }
}

fn validate_inline_asm_operands(
    outputs: &[Expr],
    output_constraints: &[String],
    inputs: &[Expr],
    input_constraints: &[String],
    span: &Span,
    diags: &mut Vec<Diagnostic>,
) {
    let output_count = outputs.len();
    let total_ops = output_count + inputs.len();

    // 0. 防御性长度检查: 约束数组 vs 表达式数组不匹配
    if output_constraints.len() != output_count {
        diags.push(Diagnostic::err(
            "E0014",
            format!(
                "asm output constraint count ({}) != output operand count ({})",
                output_constraints.len(),
                output_count
            ),
            span.clone(),
        ));
    }
    if input_constraints.len() != inputs.len() {
        diags.push(Diagnostic::err(
            "E0014",
            format!(
                "asm input constraint count ({}) != input operand count ({})",
                input_constraints.len(),
                inputs.len()
            ),
            span.clone(),
        ));
    }

    // 1. 输出约束必须含 '=' 或 '+' 前缀 (E0014) — parser 层校验的第二道防线
    for (i, c) in output_constraints.iter().enumerate() {
        let stripped = c.trim();
        if !stripped.starts_with('=') && !stripped.starts_with('+') {
            diags.push(
                Diagnostic::err(
                    "E0014",
                    format!(
                        "output operand {} constraint '{}' missing '=' or '+' prefix",
                        i, c
                    ),
                    span.clone(),
                )
                .with_hint("add '=' for write-only or '+' for read-write output"),
            );
        }
    }

    // 2. 匹配约束 (matching constraint) 数字越界检测 (W0008)
    //    mini-asm 兼容策略: 非法 asm 操作数只告警，不阻断后端
    //    warning + nop fallback。
    for (input_idx, constraint) in input_constraints.iter().enumerate() {
        let (_, _, _, match_digit) = parse_asm_constraint_flags(constraint);
        if let Some(n) = match_digit {
            let n_usize = n as usize;
            if n_usize >= output_count {
                diags.push(Diagnostic::warn(
                    "W0008",
                    format!(
                        "matching constraint references invalid operand number '{}' for input operand {}",
                        n, input_idx
                    ),
                    span.clone(),
                ).with_hint(
                    if output_count == 0 {
                        "no output operands defined; matching constraints normally require outputs".into()
                    } else {
                        format!(
                            "matching constraint digits normally reference an output operand (0 to {})",
                            output_count.saturating_sub(1)
                        )
                    }
                ));
            }
            if n_usize >= total_ops {
                diags.push(Diagnostic::warn(
                    "W0008",
                    format!(
                        "matching constraint references operand {} but only {} operand(s) provided",
                        n, total_ops
                    ),
                    span.clone(),
                ));
            }
        }
    }
    for (output_idx, constraint) in output_constraints.iter().enumerate() {
        let (_, _, _, match_digit) = parse_asm_constraint_flags(constraint);
        if let Some(n) = match_digit {
            diags.push(Diagnostic::warn(
                "W0008",
                format!(
                    "matching constraint digit '{}' not allowed in output operand {} constraint",
                    n, output_idx
                ),
                span.clone(),
            ).with_hint(
                "output operands already define their own number; use '+' for read-write output instead"
            ));
        }
    }

    // 3. 类型校验 (W0005) — 与 parser 层保持 warning 语义
    for (idx, expr) in outputs.iter().enumerate() {
        let constraint = output_constraints
            .get(idx)
            .map(String::as_str)
            .unwrap_or("=r");
        let effective = matched_constraint(
            constraint,
            output_constraints,
            input_constraints,
            outputs.len(),
        )
        .unwrap_or(constraint);
        validate_inline_asm_operand(expr, effective, true, span, diags);
    }
    for (idx, expr) in inputs.iter().enumerate() {
        let constraint = input_constraints
            .get(idx)
            .map(String::as_str)
            .unwrap_or("r");
        let effective = matched_constraint(
            constraint,
            output_constraints,
            input_constraints,
            outputs.len(),
        )
        .unwrap_or(constraint);
        validate_inline_asm_operand(expr, effective, false, span, diags);
    }

    // 4. P0-5: 早期约束模式验证 — 拒绝不支持的约束字母
    let constraint_errors = asm_constraints::validate_asm_constraints(
        output_constraints,
        input_constraints,
    );
    for err_msg in &constraint_errors {
        diags.push(
            Diagnostic::err("E0311", err_msg.clone(), span.clone())
                .with_hint("use supported constraints: r, m, i, g, n, q, =r, =m, +r, +m, memory, cc"),
        );
    }
}

fn push_asm_type_mismatch_warning(
    constraint: &str,
    ty: &Type,
    span: &Span,
    diags: &mut Vec<Diagnostic>,
) {
    diags.push(
        Diagnostic::warn(
            "W0005",
            format!(
                "impossible constraint: asm constraint '{}' does not match expression type '{}'",
                constraint, ty
            ),
            span.clone(),
        )
        .with_hint(
            "use an XMM/x87 constraint for floating-point operands or change the expression type",
        ),
    );
}

fn matched_constraint<'a>(
    constraint: &'a str,
    output_constraints: &'a [String],
    input_constraints: &'a [String],
    output_count: usize,
) -> Option<&'a str> {
    let (_, _, _, match_digit) = parse_asm_constraint_flags(constraint);
    let idx = match_digit? as usize;
    if idx < output_count {
        output_constraints.get(idx).map(String::as_str)
    } else {
        input_constraints
            .get(idx.saturating_sub(output_count))
            .map(String::as_str)
    }
}

fn validate_inline_asm_operand(
    expr: &Expr,
    constraint: &str,
    is_output: bool,
    span: &Span,
    diags: &mut Vec<Diagnostic>,
) {
    let (_, is_inout, _, match_digit) = parse_asm_constraint_flags(constraint);
    if match_digit.is_some() {
        return;
    }
    let ty = expr.ty();
    if constraint_allows_type(constraint, &ty, is_output, is_inout, expr) {
        return;
    }
    push_asm_type_mismatch_warning(constraint, &ty, span, diags);
}

fn parse_asm_constraint_flags(constraint: &str) -> (bool, bool, bool, Option<u32>) {
    let mut is_output = false;
    let mut is_inout = false;
    let mut early = false;
    let mut match_digit: Option<u32> = None;
    let mut s = constraint.trim();
    while !s.is_empty() {
        match s.chars().next() {
            Some('=') => {
                is_output = true;
                s = &s[1..];
            }
            Some('+') => {
                is_output = true;
                is_inout = true;
                s = &s[1..];
            }
            Some('&') => {
                early = true;
                s = &s[1..];
            }
            Some('%') => s = &s[1..],
            Some(c) if c.is_ascii_digit() => {
                let mut val: u32 = 0;
                while let Some(d) = s.chars().next() {
                    if !d.is_ascii_digit() {
                        break;
                    }
                    val = val.wrapping_mul(10).wrapping_add(d.to_digit(10).unwrap());
                    s = &s[1..];
                }
                match_digit = Some(val);
            }
            _ => break,
        }
    }
    (is_output, is_inout, early, match_digit)
}

fn constraint_allows_type(
    constraint: &str,
    ty: &Type,
    is_output: bool,
    is_inout: bool,
    expr: &Expr,
) -> bool {
    let core = constraint.trim_start_matches(['=', '+', '&', '%']);
    if core.is_empty() {
        return true;
    }
    if core.starts_with("@cc") {
        return ty.is_int();
    }
    if core.contains('{') {
        return explicit_constraint_allows_type(core, ty);
    }

    let letters: Vec<char> = core.chars().filter(|c| c.is_ascii_alphabetic()).collect();
    if letters.is_empty() {
        return true;
    }

    let is_float = asm_type_is_float_like(ty);
    let is_int_or_ptr = asm_type_is_integer_like(ty);

    if letters.iter().any(|c| matches!(c, 'f' | 't' | 'u')) {
        return is_float;
    }
    if letters.contains(&'y') {
        return is_int_or_ptr || matches!(ty, Type::Vector(_, _));
    }

    let selected = asm_pick_sub_constraint(core, is_inout);
    if selected == "i" && asm_constraint_is_immediate(core) {
        return !is_output && (asm_expr_can_be_immediate(expr) || is_int_or_ptr);
    }
    if asm_constraint_prefers_memory(core, is_inout) {
        return true;
    }
    if letters.iter().any(|c| matches!(c, 'x' | 'Y')) {
        return is_float;
    }
    if letters
        .iter()
        .any(|c| matches!(c, 'r' | 'R' | 'q' | 'Q' | 'a' | 'b' | 'c' | 'd' | 'S' | 'D'))
    {
        return is_int_or_ptr;
    }
    if letters.contains(&'p') {
        return ty.is_ptr();
    }

    // The x86_64 backend falls through to a GP register for unsupported
    // general alternatives such as "g". Validate against that behavior so
    // type diagnostics match the generated operand class.
    is_int_or_ptr
}

fn asm_constraint_prefers_memory(constraint: &str, is_inout: bool) -> bool {
    let letters: Vec<char> = constraint
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .collect();
    let has_m = letters.contains(&'m');
    let has_r = letters
        .iter()
        .any(|c| matches!(c, 'r' | 'R' | 'q' | 'Q' | 'a' | 'b' | 'c' | 'd' | 'S' | 'D'));
    has_m && (!has_r || is_inout)
}

fn asm_constraint_is_immediate(constraint: &str) -> bool {
    constraint
        .chars()
        .any(|c| matches!(c, 'i' | 'n' | 'I' | 'N' | 'M' | 'X' | 'E'))
}

fn asm_expr_can_be_immediate(expr: &Expr) -> bool {
    match const_eval::eval_full(expr) {
        Ok(ConstValue::Int(_)) | Ok(ConstValue::Float(_)) | Ok(ConstValue::Addr { .. }) => true,
        Ok(ConstValue::Str { .. }) | Ok(ConstValue::Bytes { .. }) => true,
        Err(_) => false,
    }
}

fn asm_pick_sub_constraint(constraint: &str, prefer_memory: bool) -> &'static str {
    let letters: Vec<char> = constraint
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .collect();
    if prefer_memory && letters.contains(&'m') {
        return "m";
    }
    if letters.contains(&'r') {
        return "r";
    }
    if letters.contains(&'m') {
        return "m";
    }
    if letters.iter().any(|c| {
        matches!(
            c,
            'i' | 'n' | 'I' | 'J' | 'K' | 'L' | 'M' | 'N' | 'O' | 'X' | 'E' | 'F' | 'G' | 'H' | 's'
        )
    }) {
        return "i";
    }
    if letters.contains(&'g') {
        return "g";
    }
    "r"
}

fn explicit_constraint_allows_type(constraint: &str, ty: &Type) -> bool {
    let Some(start) = constraint.find('{') else {
        return true;
    };
    let Some(end) = constraint[start + 1..].find('}') else {
        return true;
    };
    let reg = constraint[start + 1..start + 1 + end]
        .trim()
        .trim_start_matches('%')
        .to_ascii_lowercase();
    let is_float = asm_type_is_float_like(ty);
    let is_int_or_ptr = asm_type_is_integer_like(ty);
    if reg.starts_with("xmm")
        || reg.starts_with("ymm")
        || reg.starts_with("zmm")
        || reg == "st"
        || reg.starts_with("st(")
    {
        return is_float;
    }
    if reg.starts_with("mm") {
        return is_int_or_ptr || matches!(ty, Type::Vector(_, _));
    }
    is_int_or_ptr
}

fn asm_type_is_float_like(ty: &Type) -> bool {
    matches!(ty, Type::Float | Type::Double | Type::LongDouble)
}

fn asm_type_is_integer_like(ty: &Type) -> bool {
    ty.is_int() || matches!(ty, Type::Ptr(_) | Type::Array(_, _) | Type::Enum(_, _, _))
}

/// Convert IR ValueType to DWARF type for debug info generation
fn value_type_to_dwarf(vt: &crate::ir::ValueType) -> debug::DwarfType {
    use crate::ir::ValueType;
    match vt {
        ValueType::Void => debug::DwarfType {
            name: "void".to_string(),
            byte_size: 0,
            kind: debug::DwarfTypeKind::Void,
        },
        ValueType::Integer { bits, signed } => {
            let name = match (*bits, *signed) {
                (8, true) => "signed char",
                (8, false) => "unsigned char",
                (16, true) => "short",
                (16, false) => "unsigned short",
                (32, true) => "int",
                (32, false) => "unsigned int",
                (64, true) => "long int",
                (64, false) => "long unsigned int",
                (128, true) => "__int128",
                (128, false) => "unsigned __int128",
                _ => if *signed { "int" } else { "unsigned int" },
            };
            debug::DwarfType {
                name: name.to_string(),
                byte_size: (*bits as u64 + 7) / 8,
                kind: debug::DwarfTypeKind::Base(if *signed {
                    debug::TypeEncoding::Signed
                } else {
                    debug::TypeEncoding::Unsigned
                }),
            }
        }
        ValueType::Float { bits } => {
            let name = match *bits {
                32 => "float",
                64 => "double",
                128 => "long double",
                _ => "double",
            };
            debug::DwarfType {
                name: name.to_string(),
                byte_size: (*bits as u64 + 7) / 8,
                kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Float),
            }
        }
        ValueType::Pointer(inner) => debug::DwarfType {
            name: format!("{}*", value_type_to_dwarf(inner).name),
            byte_size: 8,
            kind: debug::DwarfTypeKind::Pointer(Box::new(value_type_to_dwarf(inner))),
        },
        ValueType::Array(elem, Some(count)) => debug::DwarfType {
            name: format!("{}[{}]", value_type_to_dwarf(elem).name, count),
            byte_size: value_type_to_dwarf(elem).byte_size * (*count as u64),
            kind: debug::DwarfTypeKind::Array {
                element: Box::new(value_type_to_dwarf(elem)),
                count: *count as u64,
            },
        },
        ValueType::Array(elem, None) => debug::DwarfType {
            name: format!("{}[]", value_type_to_dwarf(elem).name),
            byte_size: 0,
            kind: debug::DwarfTypeKind::Pointer(Box::new(value_type_to_dwarf(elem))),
        },
        ValueType::Aggregate { name, size, is_union } => {
            if *is_union {
                debug::DwarfType {
                    name: name.clone(),
                    byte_size: *size as u64,
                    kind: debug::DwarfTypeKind::Union { members: vec![] },
                }
            } else {
                debug::DwarfType {
                    name: name.clone(),
                    byte_size: *size as u64,
                    kind: debug::DwarfTypeKind::Struct { members: vec![] },
                }
            }
        }
        ValueType::Function { ret, params } => debug::DwarfType {
            name: "function".to_string(),
            byte_size: 0,
            kind: debug::DwarfTypeKind::Func {
                ret: Box::new(value_type_to_dwarf(ret)),
                params: params.iter().map(|p| value_type_to_dwarf(p)).collect(),
            },
        },
        ValueType::Vector(elem, count) => debug::DwarfType {
            name: format!("vector({},{})", value_type_to_dwarf(elem).name, count),
            byte_size: value_type_to_dwarf(elem).byte_size * (*count as u64),
            kind: debug::DwarfTypeKind::Array {
                element: Box::new(value_type_to_dwarf(elem)),
                count: *count as u64,
            },
        },
        ValueType::Complex(inner) => debug::DwarfType {
            name: format!("_Complex {}", value_type_to_dwarf(inner).name),
            byte_size: value_type_to_dwarf(inner).byte_size * 2,
            kind: debug::DwarfTypeKind::Struct { members: vec![] },
        },
        ValueType::Imaginary(inner) => debug::DwarfType {
            name: format!("_Imaginary {}", value_type_to_dwarf(inner).name),
            byte_size: value_type_to_dwarf(inner).byte_size,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Float),
        },
    }
}

/// Convert AST Type with qualifier flags to DWARF type for BTF generation.
/// Wraps the base type with Const/Volatile/Restrict qualifier nodes as needed.
/// This ensures BTF output contains complete qualifier information for
/// pahole and BPF CO-RE compatibility.
pub fn qual_type_to_dwarf(
    ty: &crate::ast::Type,
    quals: crate::ast::QualifierFlags,
) -> debug::DwarfType {
    let base = ast_type_to_dwarf(ty);
    let iso = quals.iso_c();
    if iso.is_empty() {
        return base;
    }
    // Apply qualifiers from innermost to outermost:
    // restrict -> volatile -> const (matching DWARF/BTF convention)
    let mut result = base;
    if iso.is_restrict() {
        result = debug::DwarfType {
            name: format!("restrict {}", result.name),
            byte_size: result.byte_size,
            kind: debug::DwarfTypeKind::Restrict(Box::new(result)),
        };
    }
    if iso.is_volatile() {
        result = debug::DwarfType {
            name: format!("volatile {}", result.name),
            byte_size: result.byte_size,
            kind: debug::DwarfTypeKind::Volatile(Box::new(result)),
        };
    }
    if iso.is_const() {
        result = debug::DwarfType {
            name: format!("const {}", result.name),
            byte_size: result.byte_size,
            kind: debug::DwarfTypeKind::Const(Box::new(result)),
        };
    }
    result
}

/// Convert AST Type to DWARF type for BTF generation
fn ast_type_to_dwarf(ty: &crate::ast::Type) -> debug::DwarfType {
    use crate::ast::Type;
    match ty {
        Type::Void => debug::DwarfType {
            name: "void".to_string(),
            byte_size: 0,
            kind: debug::DwarfTypeKind::Void,
        },
        Type::Char => debug::DwarfType {
            name: "char".to_string(),
            byte_size: 1,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::SignedChar),
        },
        Type::SChar => debug::DwarfType {
            name: "signed char".to_string(),
            byte_size: 1,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::SignedChar),
        },
        Type::UChar => debug::DwarfType {
            name: "unsigned char".to_string(),
            byte_size: 1,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::UnsignedChar),
        },
        Type::Char16 => debug::DwarfType {
            name: "char16_t".to_string(),
            byte_size: 2,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Unsigned),
        },
        Type::Char32 => debug::DwarfType {
            name: "char32_t".to_string(),
            byte_size: 4,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Unsigned),
        },
        Type::Short => debug::DwarfType {
            name: "short".to_string(),
            byte_size: 2,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Signed),
        },
        Type::UShort => debug::DwarfType {
            name: "unsigned short".to_string(),
            byte_size: 2,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Unsigned),
        },
        Type::Int => debug::DwarfType {
            name: "int".to_string(),
            byte_size: 4,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Signed),
        },
        Type::UInt => debug::DwarfType {
            name: "unsigned int".to_string(),
            byte_size: 4,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Unsigned),
        },
        Type::Long => debug::DwarfType {
            name: "long".to_string(),
            byte_size: 8,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Signed),
        },
        Type::ULong => debug::DwarfType {
            name: "unsigned long".to_string(),
            byte_size: 8,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Unsigned),
        },
        Type::LongLong => debug::DwarfType {
            name: "long long".to_string(),
            byte_size: 8,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Signed),
        },
        Type::ULongLong => debug::DwarfType {
            name: "unsigned long long".to_string(),
            byte_size: 8,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Unsigned),
        },
        Type::Int128 => debug::DwarfType {
            name: "__int128".to_string(),
            byte_size: 16,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Signed),
        },
        Type::UInt128 => debug::DwarfType {
            name: "unsigned __int128".to_string(),
            byte_size: 16,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Unsigned),
        },
        Type::Float => debug::DwarfType {
            name: "float".to_string(),
            byte_size: 4,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Float),
        },
        Type::Double => debug::DwarfType {
            name: "double".to_string(),
            byte_size: 8,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Float),
        },
        Type::LongDouble => debug::DwarfType {
            name: "long double".to_string(),
            byte_size: 16,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Float),
        },
        Type::Bool => debug::DwarfType {
            name: "_Bool".to_string(),
            byte_size: 1,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Boolean),
        },
        Type::Ptr(inner) => {
            let inner_dw = ast_type_to_dwarf(&inner.base);
            debug::DwarfType {
                name: format!("{}*", inner_dw.name),
                byte_size: 8,
                kind: debug::DwarfTypeKind::Pointer(Box::new(inner_dw)),
            }
        }
        Type::Array(elem, count) => {
            let elem_dw = ast_type_to_dwarf(&elem.base);
            let cnt = match count {
                crate::ast::ArraySize::Const(n) => *n as u64,
                crate::ast::ArraySize::Var(_) => 0,
            };
            debug::DwarfType {
                name: format!("{}[{}]", elem_dw.name, cnt),
                byte_size: elem_dw.byte_size * cnt,
                kind: debug::DwarfTypeKind::Array {
                    element: Box::new(elem_dw),
                    count: cnt,
                },
            }
        }
        Type::Named(name) => debug::DwarfType {
            name: name.clone(),
            byte_size: 0, // 未知大小，稍后解析
            kind: debug::DwarfTypeKind::Void, // 占位
        },
        Type::Struct(name, fields, size) => {
            let members: Vec<debug::DwarfMember> = fields
                .iter()
                .map(|f| {
                    let field_ty = qual_type_to_dwarf(&f.ty, f.quals);
                    let offset_bits = (f.offset as u64) * 8;
                    if let Some(bit_width) = f.bit_width {
                        debug::DwarfMember::bitfield(
                            &f.name,
                            offset_bits + f.bit_offset as u64,
                            bit_width,
                            field_ty,
                        )
                    } else {
                        debug::DwarfMember::regular(&f.name, offset_bits, field_ty)
                    }
                })
                .collect();
            debug::DwarfType {
                name: format!("struct {}", name),
                byte_size: *size as u64,
                kind: debug::DwarfTypeKind::Struct { members },
            }
        }
        Type::Union(name, fields, size) => {
            let members: Vec<debug::DwarfMember> = fields
                .iter()
                .map(|f| {
                    let field_ty = qual_type_to_dwarf(&f.ty, f.quals);
                    debug::DwarfMember::regular(&f.name, 0, field_ty)
                })
                .collect();
            debug::DwarfType {
                name: format!("union {}", name),
                byte_size: *size as u64,
                kind: debug::DwarfTypeKind::Union { members },
            }
        }
        Type::Enum(name, variants, _underlying) => {
            let members: Vec<(String, i128)> = variants
                .iter()
                .map(|(n, v)| (n.clone(), *v as i128))
                .collect();
            debug::DwarfType {
                name: format!("enum {}", name),
                byte_size: 4,
                kind: debug::DwarfTypeKind::Enum {
                    members,
                    underlying: Box::new(debug::DwarfType {
                        name: "unsigned int".to_string(),
                        byte_size: 4,
                        kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Unsigned),
                    }),
                },
            }
        }
        Type::Func(ret, params) => debug::DwarfType {
            name: "function".to_string(),
            byte_size: 0,
            kind: debug::DwarfTypeKind::Func {
                ret: Box::new(ast_type_to_dwarf(&ret.base)),
                params: params.iter().map(|p| ast_type_to_dwarf(&p.base)).collect(),
            },
        },
        Type::Complex(inner) => debug::DwarfType {
            name: format!("_Complex {}", ast_type_to_dwarf(&inner.base).name),
            byte_size: ast_type_to_dwarf(&inner.base).byte_size * 2,
            kind: debug::DwarfTypeKind::Struct { members: vec![] },
        },
        Type::Imaginary(inner) => debug::DwarfType {
            name: format!("_Imaginary {}", ast_type_to_dwarf(&inner.base).name),
            byte_size: ast_type_to_dwarf(&inner.base).byte_size,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Float),
        },
        Type::Atomic(inner) => ast_type_to_dwarf(&inner.base),
        Type::Fract => debug::DwarfType {
            name: "_Fract".to_string(),
            byte_size: 4,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Float),
        },
        Type::Accum => debug::DwarfType {
            name: "_Accum".to_string(),
            byte_size: 8,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Float),
        },
        Type::Vector(elem, count) => {
            let elem_dw = ast_type_to_dwarf(&elem.base);
            debug::DwarfType {
                name: format!("vector({},{})", elem_dw.name, count),
                byte_size: elem_dw.byte_size * (*count as u64),
                kind: debug::DwarfTypeKind::Array {
                    element: Box::new(elem_dw),
                    count: *count as u64,
                },
            }
        }
    }
}

/// Build a CompileUnit from IR module + AST source with rich type/line info.
/// `func_pcs` entries: (name, low_pc, high_pc).
fn build_dwarf_cu(
    module: &Module,
    source_name: &str,
    func_pcs: &[(&str, u64, u64)],
    line_entries: Vec<debug::LineEntry>,
) -> debug::CompileUnit {
    // Build AST lookup: function name → param names
    let ast_param_names: std::collections::HashMap<&str, Vec<Option<String>>> = module
        .source()
        .decls
        .iter()
        .filter_map(|d| match d {
            crate::ast::Decl::Func(fd) => {
                let names: Vec<Option<String>> = fd
                    .params
                    .iter()
                    .map(|p| p.name.as_ref().map(|id| id.name.clone()))
                    .collect();
                Some((fd.name.name.as_str(), names))
            }
            _ => None,
        })
        .collect();

    let functions: Vec<debug::DwarfFuncInfo> = func_pcs
        .iter()
        .filter_map(|(name, low_pc, high_pc)| {
            let ir_func = module.functions.iter().find(|f| f.name == *name)?;
            let ret_ty = value_type_to_dwarf(&ir_func.return_type);
            let param_names = ast_param_names.get(*name);
            let params: Vec<(String, debug::DwarfType)> = ir_func
                .params
                .iter()
                .enumerate()
                .map(|(i, vt)| {
                    let p_name = param_names
                        .and_then(|ns| ns.get(i))
                        .and_then(|n| n.clone())
                        .unwrap_or_else(|| format!("arg{}", i));
                    (p_name, value_type_to_dwarf(vt))
                })
                .collect();
            Some(debug::DwarfFuncInfo {
                name: name.to_string(),
                linkage_name: None,
                low_pc: *low_pc,
                high_pc: *high_pc,
                decl_line: ir_func.span.line,
                decl_file_idx: 0,
                return_type: ret_ty,
                params,
                param_locations: vec![],
                local_vars: vec![],
                lexical_blocks: vec![],
                is_external: *name == "main",
                is_inline: false,
                is_declaration: false,
            })
        })
        .collect();

    let low_pc = functions.first().map(|f| f.low_pc).unwrap_or(0);
    let high_pc = functions.last().map(|f| f.high_pc).unwrap_or(0);
    let func_ranges: Vec<(u64, u64)> =
        functions.iter().map(|f| (f.low_pc, f.high_pc)).collect();

    // 从 AST 声明中收集类型信息（struct/union/enum/typedef）
    let mut types: Vec<debug::DwarfType> = Vec::new();
    for decl in &module.source().decls {
        match decl {
            crate::ast::Decl::StructDef(sd) => {
                let members: Vec<debug::DwarfMember> = sd
                    .fields
                    .iter()
                    .map(|f| {
                        let field_ty = qual_type_to_dwarf(&f.ty, f.quals);
                        let offset_bits = (f.offset as u64) * 8;
                        if let Some(bit_width) = f.bit_width {
                            debug::DwarfMember::bitfield(
                                &f.name,
                                offset_bits + f.bit_offset as u64,
                                bit_width,
                                field_ty,
                            )
                        } else {
                            debug::DwarfMember::regular(&f.name, offset_bits, field_ty)
                        }
                    })
                    .collect();
                let size = sd.fields.last().map(|f| {
                    f.offset + f.ty.size()
                }).unwrap_or(0);
                types.push(debug::DwarfType {
                    name: format!("struct {}", sd.name.name),
                    byte_size: size as u64,
                    kind: debug::DwarfTypeKind::Struct { members },
                });
            }
            crate::ast::Decl::UnionDef(sd) => {
                let members: Vec<debug::DwarfMember> = sd
                    .fields
                    .iter()
                    .map(|f| {
                        let field_ty = qual_type_to_dwarf(&f.ty, f.quals);
                        debug::DwarfMember::regular(&f.name, 0, field_ty)
                    })
                    .collect();
                let size = sd.fields.iter().map(|f| f.ty.size()).max().unwrap_or(0);
                types.push(debug::DwarfType {
                    name: format!("union {}", sd.name.name),
                    byte_size: size as u64,
                    kind: debug::DwarfTypeKind::Union { members },
                });
            }
            crate::ast::Decl::EnumDef(ed) => {
                let mut variants: Vec<(String, i128)> = Vec::new();
                let mut next_val: i128 = 0;
                for (ident, val) in &ed.variants {
                    let v = val.map(|v| v as i128).unwrap_or(next_val);
                    variants.push((ident.name.clone(), v));
                    next_val = v + 1;
                }
                types.push(debug::DwarfType {
                    name: format!("enum {}", ed.name.name),
                    byte_size: 4,
                    kind: debug::DwarfTypeKind::Enum {
                        members: variants,
                        underlying: Box::new(debug::DwarfType {
                            name: "unsigned int".to_string(),
                            byte_size: 4,
                            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Unsigned),
                        }),
                    },
                });
            }
            crate::ast::Decl::Typedef { name, ty, .. } => {
                let target = ast_type_to_dwarf(ty);
                types.push(debug::DwarfType {
                    name: name.name.clone(),
                    byte_size: target.byte_size,
                    kind: debug::DwarfTypeKind::Typedef {
                        alias: name.name.clone(),
                        target: Box::new(target),
                    },
                });
            }
            _ => {}
        }
    }

    debug::CompileUnit {
        source_name: source_name.to_string(),
        comp_dir: None,
        producer: format!("dcc {}", env!("CARGO_PKG_VERSION")),
        language: 0x000C, // DW_LANG_C99
        functions,
        globals: vec![],
        types,
        low_pc,
        high_pc,
        line_entries,
        line_files: vec![source_name.to_string()],
        line_dirs: vec![".".to_string()],
        func_ranges,
        cfi_rows: vec![],
    }
}

#[allow(clippy::too_many_arguments)]
pub fn emit(
    module: &Module,
    target: &Target,
    relocatable: bool,
    emit_asm: bool,
    source_name: &str,
    debug_level: u8,
    dwarf_version: u8,
    sanitizers: SanitizerFlags,
    lto_enabled: bool,
    function_sections: bool,
    parallel_codegen: bool,
    bounds_check: bool,
    trapv: bool,
    offload: Option<&crate::profile::offload::OffloadSection>,
    line_map: &[LineSpan],
    verbose: bool,
) -> Result<Vec<u8>, Vec<Diagnostic>> {
    Ok(match target.arch {
        crate::cli::Arch::X86_64 => {
            let mut cg = x86_64::Codegen::new(target.clone());
            cg.relocatable = relocatable || emit_asm;
            cg.function_sections = function_sections || lto_enabled;
            cg.emit_asm_text = emit_asm;
            cg.parallel_codegen = parallel_codegen && !emit_asm;
            cg.source_name = source_name.to_string();
            cg.line_map = line_map.to_vec();
            cg.debug_level = debug_level;
            cg.emit_cfi = debug_level > 0 || target.frame_pointer;
            cg.bounds_check = bounds_check;
            cg.trapv = trapv;
            cg.kasan = sanitizers.kasan;
            cg.compile(module);
            if !cg.diagnostics.is_empty() {
                return Err(cg.diagnostics);
            }
            if emit_asm {
                return Ok(cg.build_asm_output(module.source()).into_bytes());
            }
            if debug_level > 0 && relocatable {
                // Build func_pcs with actual sizes from codegen
                let func_pcs: Vec<(&str, u64, u64)> = module
                    .functions
                    .iter()
                    .filter_map(|f| {
                        cg.labels.get(&f.name).map(|&pc| {
                            let sz = cg.func_sizes.get(&f.name).copied().unwrap_or(16);
                            (f.name.as_str(), pc, pc + sz)
                        })
                    })
                    .collect();
                // 将 asm 块行号记录转换为 DWARF LineEntry
                // 组合块级条目 (is_stmt=true) 和逐指令条目 (is_stmt=false, discriminator>0)
                // 块级条目: GDB 在 asm 块起始行设置语句边界
                // 逐指令条目: GDB 单步进入 asm 块内部每条指令 (类似 GCC .debug_insn)
                let mut combined_line_entries: Vec<debug::LineEntry> = Vec::new();
                // 块级条目: asm 块起始 PC, is_stmt=true
                for &(pc_start, _pc_end, src_line) in cg.asm_line_entries.iter() {
                    combined_line_entries.push(debug::LineEntry {
                        pc: pc_start,
                        line: src_line,
                        column: 1,
                        file_idx: 0,
                        discriminator: 0,
                        is_stmt: true,
                        basic_block: false,
                    });
                }
                // 逐指令条目: 每条 asm 指令一个条目, is_stmt=false
                // 同一 asm 块内用 discriminator 区分不同指令
                let mut last_block_pc = u64::MAX;
                for &(pc, src_line, disc) in cg.asm_insn_debug_entries.iter() {
                    let is_first_in_block = pc != last_block_pc;
                    last_block_pc = pc;
                    combined_line_entries.push(debug::LineEntry {
                        pc,
                        line: src_line,
                        column: 1,
                        file_idx: 0,
                        discriminator: if is_first_in_block { 0 } else { disc },
                        is_stmt: is_first_in_block,
                        basic_block: false,
                    });
                }
                // 按 PC 排序, 同 PC 时 is_stmt=true 优先
                combined_line_entries.sort_by(|a, b| {
                    a.pc.cmp(&b.pc)
                        .then_with(|| b.is_stmt.cmp(&a.is_stmt))
                        .then_with(|| a.discriminator.cmp(&b.discriminator))
                });
                // Always use build_dwarf_cu with rich IR info
                let cu = build_dwarf_cu(module, source_name, &func_pcs, combined_line_entries);
                let opts = debug::DebugOptions {
                    level: debug_level,
                    dwarf_version,
                    source_name: source_name.to_string(),
                    ..Default::default()
                };
                let dbg = debug::build_dwarf_full(&cu, &opts);
                cg.extra_sections.extend(debug::dwarf_elf_sections(&dbg));
                // Build .rela.debug_info for function address relocations
                if !dbg.debug_info_relocs.is_empty() {
                    let debug_info_sec_idx = cg
                        .extra_sections
                        .iter()
                        .position(|s| s.name == ".debug_info")
                        .map(|i| 7 + i as u32)
                        .unwrap_or(8);
                    let debug_relocs: Vec<elf::ElfRela> = dbg
                        .debug_info_relocs
                        .iter()
                        .filter_map(|(off, sym_name)| {
                            let sym_idx = cg.sym_index.get(sym_name).copied()?;
                            Some(elf::ElfRela {
                                offset: *off,
                                sym_index: sym_idx,
                                rel_type: elf::R_X86_64_64,
                                addend: 0,
                            })
                        })
                        .collect();
                    if !debug_relocs.is_empty() {
                        cg.extra_rela_sections.push(elf::ExtraRelaSection {
                            name: ".rela.debug_info".to_string(),
                            relocations: debug_relocs,
                            target_sec_index: debug_info_sec_idx,
                        });
                    }
                }
                // Build .rela.debug_line for line program address relocations
                if !dbg.debug_line_relocs.is_empty() {
                    let debug_line_sec_idx = cg
                        .extra_sections
                        .iter()
                        .position(|s| s.name == ".debug_line")
                        .map(|i| 7 + i as u32)
                        .unwrap_or(10);
                    let line_relocs: Vec<elf::ElfRela> = dbg
                        .debug_line_relocs
                        .iter()
                        .filter_map(|(off, sym_name)| {
                            let sym_idx = cg.sym_index.get(sym_name).copied()?;
                            Some(elf::ElfRela {
                                offset: *off,
                                sym_index: sym_idx,
                                rel_type: elf::R_X86_64_64,
                                addend: 0,
                            })
                        })
                        .collect();
                    if !line_relocs.is_empty() {
                        cg.extra_rela_sections.push(elf::ExtraRelaSection {
                            name: ".rela.debug_line".to_string(),
                            relocations: line_relocs,
                            target_sec_index: debug_line_sec_idx,
                        });
                    }
                }
                // Build .rela.debug_aranges for aranges address relocations
                if !dbg.debug_aranges_relocs.is_empty() {
                    let debug_aranges_sec_idx = cg
                        .extra_sections
                        .iter()
                        .position(|s| s.name == ".debug_aranges")
                        .map(|i| 7 + i as u32)
                        .unwrap_or(9);
                    let aranges_relocs: Vec<elf::ElfRela> = dbg
                        .debug_aranges_relocs
                        .iter()
                        .filter_map(|(off, sym_name)| {
                            let sym_idx = cg.sym_index.get(sym_name).copied()?;
                            Some(elf::ElfRela {
                                offset: *off,
                                sym_index: sym_idx,
                                rel_type: elf::R_X86_64_64,
                                addend: 0,
                            })
                        })
                        .collect();
                    if !aranges_relocs.is_empty() {
                        cg.extra_rela_sections.push(elf::ExtraRelaSection {
                            name: ".rela.debug_aranges".to_string(),
                            relocations: aranges_relocs,
                            target_sec_index: debug_aranges_sec_idx,
                        });
                    }
                }
                // BTF 已在 build_dwarf_full 中生成，使用 dbg.btf
                if !dbg.btf.is_empty() {
                    cg.extra_sections.push(elf::ElfSection {
                        name: ".BTF".to_string(),
                        data: dbg.btf,
                        ty: elf::SHT_PROGBITS,
                        flags: 0,
                        align: 4,
                    });
                }
            }
            if cg.emit_cfi && relocatable && target.unwind_tables {
                // dcc 输出 .eh_frame FDE 需带 .rela.eh_frame relocations 才能在 ld 链接时
                // 正确解析 PC begin / PC range. 之前 (M15 阶段) 临时跳过是因 R_X86_64_PC32
                // 未生成, 报 "PHDR segment not covered by LOAD segment". 现 relocation 已
                // 正常生成 (.rela.eh_frame, 见 mod.rs 下方), 这里默认恢复 .eh_frame 输出,
                // setjmp / longjmp / C++ 异常等栈展开路径可正确恢复 callee-saved 寄存器.
                // 注意: -fno-asynchronous-unwind-tables 时不生成 .eh_frame (内核等场景).
                let frames: Vec<debug::FuncFrame> = cg.func_frames(module);
                cg.extra_sections.push(elf::ElfSection {
                    name: ".eh_frame".to_string(),
                    data: debug::build_eh_frame(&frames),
                    ty: elf::SHT_PROGBITS,
                    flags: elf::SHF_ALLOC,
                    align: 8,
                });
            }
            // ORC unwind 信息: 由 objtool --orc 生成, dcc 不再生成
            // 避免与 objtool 冲突 ("file already has .orc_unwind section")
            // if relocatable && (!target.unwind_tables || cg.emit_cfi) {
            //     cg.build_orc_sections(module);
            // }
            if sanitizers.any() {
                cg.extra_sections.push(elf::ElfSection {
                    name: ".dcc.san_info".to_string(),
                    data: sanitizer_metadata(sanitizers),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
            }
            if let Some(o) = offload {
                if !o.is_empty() {
                    cg.extra_sections.push(elf::ElfSection {
                        name: ".dcc.offload".to_string(),
                        data: o.to_binary(),
                        ty: elf::SHT_PROGBITS,
                        flags: 0,
                        align: 8,
                    });
                }
            }
            // ifunc 属性: 收集所有 ifunc 函数, 生成 .dcc.ifunc 节
            // 格式: 每条记录 = u16 name_len + name bytes + u16 resolver_len + resolver bytes
            if relocatable {
                let mut ifunc_data = Vec::new();
                let prog = module.source();
                for decl in &prog.decls {
                    if let crate::ast::Decl::Func(f) = decl {
                        if let Some(resolver) = &f.ifunc_resolver {
                            let name = f.name.name.as_bytes();
                            let res = resolver.as_bytes();
                            ifunc_data.extend_from_slice(&(name.len() as u16).to_le_bytes());
                            ifunc_data.extend_from_slice(name);
                            ifunc_data.extend_from_slice(&(res.len() as u16).to_le_bytes());
                            ifunc_data.extend_from_slice(res);
                        }
                    }
                }
                if !ifunc_data.is_empty() {
                    cg.extra_sections.push(elf::ElfSection {
                        name: ".dcc.ifunc".to_string(),
                        data: ifunc_data,
                        ty: elf::SHT_PROGBITS,
                        flags: 0,
                        align: 1,
                    });
                }
            }
            if lto_enabled {
                // 嵌入 dcc 自有 IR 位码（供 src/link 多 TU LTO 使用）
                let bc = crate::link::lto_bitcode::serialize_module(module, source_name);
                if verbose {
                    eprintln!(
                        "dcc: lto    = .dcc.lto {} bytes ({} funcs, {} globals)",
                        bc.len(),
                        module.functions.len(),
                        module.globals.len()
                    );
                }
                cg.extra_sections.push(elf::ElfSection {
                    name: ".dcc.lto".to_string(),
                    data: bc,
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
                // 同时输出 .llvm.lto 文本摘要，兼容：
                //   1) GC-sections 探测（外部工具检查 .llvm.lto 判断是否有 LTO 信息）
                //   2) ld -r 合并路径（lto.rs 读写 .llvm.lto 进行跨 TU 合并）
                // 与 .dcc.lto DBC 位码并存，两者职责不同。
                let funcs: Vec<(&str, u64, bool)> = module
                    .functions
                    .iter()
                    .map(|f| {
                        let sz = cg.func_sizes.get(&f.name).copied().unwrap_or(0);
                        (f.name.as_str(), sz, true)
                    })
                    .collect();
                let summary_unit = crate::lto::build_unit(source_name, &funcs);
                let summary_data = crate::lto::serialize_summary(&summary_unit);
                if verbose {
                    eprintln!(
                        "dcc: lto    = .llvm.lto {} bytes ({} funcs)",
                        summary_data.len(),
                        funcs.len()
                    );
                }
                cg.extra_sections.push(elf::ElfSection {
                    name: ".llvm.lto".to_string(),
                    data: summary_data,
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
            }
            if relocatable {
                if target.triple.os == crate::cli::Os::Windows {
                    coff::build_coff_rel_x86_64(
                        &cg.code,
                        &cg.rodata,
                        &cg.data_section,
                        cg.bss_size,
                        &cg.tdata_section,
                        cg.tbss_size,
                        cg.text_align,
                        cg.data_align,
                        cg.bss_align,
                        cg.tdata_align,
                        cg.tbss_align,
                        &cg.symbols,
                        &cg.relocations,
                        &cg.extra_sections,
                        &cg.data_relocations,
                        &cg.extra_rela_sections,
                    )
                } else {
                    elf::build_elf_rel_x86_64(
                        &cg.code,
                        &cg.rodata,
                        &cg.data_section,
                        cg.bss_size,
                        &cg.tdata_section,
                        cg.tbss_size,
                        cg.text_align,
                        cg.data_align,
                        cg.bss_align,
                        cg.tdata_align,
                        cg.tbss_align,
                        &cg.symbols,
                        &cg.relocations,
                        &cg.extra_sections,
                        &cg.data_relocations,
                        &cg.extra_rela_sections,
                    )
                }
            } else {
                elf::build_elf_exec_x86_64(
                    &cg.code,
                    cg.entry,
                    cg.exec_text_size,
                    cg.rodata.len() as u64,
                    cg.tdata_section.len() as u64,
                    cg.tbss_size,
                    &cg.symbols,
                )
            }
        }
        crate::cli::Arch::AArch64 => {
            let mut cg = aarch64::Aarch64Codegen::new(target.clone(), relocatable || emit_asm);
            cg.set_source_name(source_name);
            cg.set_asm_text_mode(emit_asm);
            // HWASAN (-fsanitize=hwaddress) 在 AArch64 上启用 MTE 硬件 tag:
            // prologue tag 栈帧 + 指针 tag 流程由后端直接发射 IRG/ADDG/STG。
            cg.set_mte_enabled(sanitizers.hwasan);
            if emit_asm {
                cg.compile(module);
                if !cg.diagnostics.is_empty() {
                    return Err(cg.diagnostics.clone());
                }
                return Ok(cg.build_asm_output(module).into_bytes());
            }
            cg.compile(module);
            if !cg.diagnostics.is_empty() {
                return Err(cg.diagnostics.clone());
            }
            if debug_level > 0 && relocatable {
                let func_pcs: Vec<(&str, u64, u64)> = module
                    .functions
                    .iter()
                    .filter_map(|f| {
                        cg.labels.get(&f.name).map(|&pc| (f.name.as_str(), pc, pc + 64))
                    })
                    .collect();
                let opts = debug::DebugOptions {
                    level: debug_level,
                    dwarf_version,
                    source_name: source_name.to_string(),
                    arch: debug::ArchKind::AArch64,
                    ..debug::DebugOptions::default()
                };
                let cu = build_dwarf_cu(module, source_name, &func_pcs, vec![]);
                let dbg = debug::build_dwarf_full(&cu, &opts);
                let extra_secs = debug::dwarf_elf_sections(&dbg);
                cg.push_extra_section(elf::ElfSection {
                    name: ".debug_info".to_string(),
                    data: dbg.info,
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
                for sec in extra_secs {
                    cg.push_extra_section(sec);
                }
                // BTF 已在 build_dwarf_full 中生成
                if !dbg.btf.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".BTF".to_string(),
                        data: dbg.btf,
                        ty: elf::SHT_PROGBITS,
                        flags: 0,
                        align: 4,
                    });
                }
            }
            if relocatable && target.unwind_tables {
                let frames = cg.func_frames(module);
                if !frames.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".eh_frame".to_string(),
                        data: debug::build_eh_frame_with_arch(&frames, debug::ArchKind::AArch64),
                        ty: elf::SHT_PROGBITS,
                        flags: elf::SHF_ALLOC,
                        align: 8,
                    });
                }
            }
            if sanitizers.any() {
                cg.push_extra_section(elf::ElfSection {
                    name: ".dcc.san_info".to_string(),
                    data: sanitizer_metadata(sanitizers),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
            }
            if let Some(o) = offload {
                if !o.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".dcc.offload".to_string(),
                        data: o.to_binary(),
                        ty: elf::SHT_PROGBITS,
                        flags: 0,
                        align: 8,
                    });
                }
            }
            cg.build_output()
        }
        crate::cli::Arch::RiscV64 => {
            let mut cg = riscv64::Riscv64Codegen::new(target.clone(), relocatable || emit_asm);
            cg.set_source_name(source_name);
            cg.set_asm_text_mode(emit_asm);
            if emit_asm {
                cg.compile(module);
                if !cg.diagnostics.is_empty() {
                    return Err(cg.diagnostics.clone());
                }
                return Ok(cg.build_asm_output(module).into_bytes());
            }
            cg.compile(module);
            if !cg.diagnostics.is_empty() {
                return Err(cg.diagnostics.clone());
            }
            if debug_level > 0 && relocatable {
                let func_pcs: Vec<(&str, u64, u64)> = module
                    .functions
                    .iter()
                    .filter_map(|f| {
                        cg.labels.get(&f.name).map(|&pc| (f.name.as_str(), pc, pc + 64))
                    })
                    .collect();
                let opts = debug::DebugOptions {
                    level: debug_level,
                    dwarf_version,
                    source_name: source_name.to_string(),
                    arch: debug::ArchKind::RiscV64,
                    ..debug::DebugOptions::default()
                };
                let cu = build_dwarf_cu(module, source_name, &func_pcs, vec![]);
                let dbg = debug::build_dwarf_full(&cu, &opts);
                let extra_secs = debug::dwarf_elf_sections(&dbg);
                cg.push_extra_section(elf::ElfSection {
                    name: ".debug_info".to_string(),
                    data: dbg.info,
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
                for sec in extra_secs {
                    cg.push_extra_section(sec);
                }
                // BTF 已在 build_dwarf_full 中生成
                if !dbg.btf.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".BTF".to_string(),
                        data: dbg.btf,
                        ty: elf::SHT_PROGBITS,
                        flags: 0,
                        align: 4,
                    });
                }
            }
            if relocatable && target.unwind_tables {
                let frames = cg.func_frames(module);
                if !frames.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".eh_frame".to_string(),
                        data: debug::build_eh_frame_with_arch(&frames, debug::ArchKind::RiscV64),
                        ty: elf::SHT_PROGBITS,
                        flags: elf::SHF_ALLOC,
                        align: 8,
                    });
                }
            }
            if sanitizers.any() {
                cg.push_extra_section(elf::ElfSection {
                    name: ".dcc.san_info".to_string(),
                    data: sanitizer_metadata(sanitizers),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
            }
            if let Some(o) = offload {
                if !o.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".dcc.offload".to_string(),
                        data: o.to_binary(),
                        ty: elf::SHT_PROGBITS,
                        flags: 0,
                        align: 8,
                    });
                }
            }
            cg.build_output()
        }
        crate::cli::Arch::Mips => {
            let mut cg = mips::MipsCodegen::new(target.clone(), relocatable || emit_asm);
            cg.set_source_name(source_name);
            cg.set_asm_text_mode(emit_asm);
            if emit_asm {
                cg.compile(module);
                if !cg.diagnostics.is_empty() {
                    return Err(cg.diagnostics.clone());
                }
                return Ok(cg.build_asm_output(module).into_bytes());
            }
            cg.compile(module);
            if !cg.diagnostics.is_empty() {
                return Err(cg.diagnostics.clone());
            }
            if debug_level > 0 && relocatable {
                let func_pcs: Vec<(&str, u64, u64)> = module
                    .functions
                    .iter()
                    .filter_map(|f| {
                        cg.labels.get(&f.name).map(|&pc| (f.name.as_str(), pc, pc + 64))
                    })
                    .collect();
                let opts = debug::DebugOptions {
                    level: debug_level,
                    dwarf_version,
                    source_name: source_name.to_string(),
                    arch: debug::ArchKind::Mips,
                    ..debug::DebugOptions::default()
                };
                let cu = build_dwarf_cu(module, source_name, &func_pcs, vec![]);
                let dbg = debug::build_dwarf_full(&cu, &opts);
                let extra_secs = debug::dwarf_elf_sections(&dbg);
                cg.push_extra_section(elf::ElfSection {
                    name: ".debug_info".to_string(),
                    data: dbg.info,
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
                for sec in extra_secs {
                    cg.push_extra_section(sec);
                }
            }
            if relocatable {
                let frames = cg.func_frames(module);
                if !frames.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".eh_frame".to_string(),
                        data: debug::build_eh_frame_with_arch(&frames, debug::ArchKind::Mips),
                        ty: elf::SHT_PROGBITS,
                        flags: elf::SHF_ALLOC,
                        align: 4,
                    });
                }
            }
            if sanitizers.any() {
                cg.push_extra_section(elf::ElfSection {
                    name: ".dcc.san_info".to_string(),
                    data: sanitizer_metadata(sanitizers),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
            }
            if let Some(o) = offload {
                if !o.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".dcc.offload".to_string(),
                        data: o.to_binary(),
                        ty: elf::SHT_PROGBITS,
                        flags: 0,
                        align: 8,
                    });
                }
            }
            cg.build_output()
        }
        crate::cli::Arch::Mips64 => {
            let mut cg = mips64::Mips64Codegen::new(target.clone(), relocatable || emit_asm);
            cg.set_source_name(source_name);
            cg.set_asm_text_mode(emit_asm);
            if emit_asm {
                cg.compile(module);
                if !cg.diagnostics().is_empty() {
                    return Err(cg.diagnostics().to_vec());
                }
                return Ok(cg.build_asm_output(module).into_bytes());
            }
            cg.compile(module);
            if !cg.diagnostics().is_empty() {
                return Err(cg.diagnostics().to_vec());
            }
            if debug_level > 0 && relocatable {
                let func_pcs: Vec<(&str, u64, u64)> = module
                    .functions
                    .iter()
                    .filter_map(|f| {
                        cg.labels().get(&f.name).map(|&pc| (f.name.as_str(), pc, pc + 64))
                    })
                    .collect();
                let opts = debug::DebugOptions {
                    level: debug_level,
                    dwarf_version,
                    source_name: source_name.to_string(),
                    arch: debug::ArchKind::Mips64,
                    ..debug::DebugOptions::default()
                };
                let cu = build_dwarf_cu(module, source_name, &func_pcs, vec![]);
                let dbg = debug::build_dwarf_full(&cu, &opts);
                let extra_secs = debug::dwarf_elf_sections(&dbg);
                cg.push_extra_section(elf::ElfSection {
                    name: ".debug_info".to_string(),
                    data: dbg.info,
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
                for sec in extra_secs {
                    cg.push_extra_section(sec);
                }
            }
            if relocatable {
                let frames = cg.func_frames(module);
                if !frames.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".eh_frame".to_string(),
                        data: debug::build_eh_frame_with_arch(&frames, debug::ArchKind::Mips64),
                        ty: elf::SHT_PROGBITS,
                        flags: elf::SHF_ALLOC,
                        align: 8,
                    });
                }
            }
            if sanitizers.any() {
                cg.push_extra_section(elf::ElfSection {
                    name: ".dcc.san_info".to_string(),
                    data: sanitizer_metadata(sanitizers),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
            }
            if let Some(o) = offload {
                if !o.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".dcc.offload".to_string(),
                        data: o.to_binary(),
                        ty: elf::SHT_PROGBITS,
                        flags: 0,
                        align: 8,
                    });
                }
            }
            cg.build_output()
        }
        crate::cli::Arch::Arm => {
            let mut cg = arm::ArmCodegen::new(target.clone(), relocatable || emit_asm);
            cg.set_source_name(source_name);
            cg.set_asm_text_mode(emit_asm);
            if emit_asm {
                cg.compile(module);
                if !cg.diagnostics.is_empty() {
                    return Err(cg.diagnostics.clone());
                }
                return Ok(cg.build_asm_output(module).into_bytes());
            }
            cg.compile(module);
            if !cg.diagnostics.is_empty() {
                return Err(cg.diagnostics.clone());
            }
            if debug_level > 0 && relocatable {
                let func_pcs: Vec<(&str, u64, u64)> = module
                    .functions
                    .iter()
                    .filter_map(|f| {
                        cg.labels.get(&f.name).map(|&pc| (f.name.as_str(), pc, pc + 64))
                    })
                    .collect();
                let opts = debug::DebugOptions {
                    level: debug_level,
                    dwarf_version,
                    source_name: source_name.to_string(),
                    arch: debug::ArchKind::Arm,
                    ..debug::DebugOptions::default()
                };
                let cu = build_dwarf_cu(module, source_name, &func_pcs, vec![]);
                let dbg = debug::build_dwarf_full(&cu, &opts);
                let extra_secs = debug::dwarf_elf_sections(&dbg);
                cg.push_extra_section(elf::ElfSection {
                    name: ".debug_info".to_string(),
                    data: dbg.info,
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
                for sec in extra_secs {
                    cg.push_extra_section(sec);
                }
            }
            if relocatable {
                let frames = cg.func_frames(module);
                if !frames.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".eh_frame".to_string(),
                        data: debug::build_eh_frame_with_arch(&frames, debug::ArchKind::Arm),
                        ty: elf::SHT_PROGBITS,
                        flags: elf::SHF_ALLOC,
                        align: 4,
                    });
                }
            }
            if sanitizers.any() {
                cg.push_extra_section(elf::ElfSection {
                    name: ".dcc.san_info".to_string(),
                    data: sanitizer_metadata(sanitizers),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
            }
            if let Some(o) = offload {
                if !o.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".dcc.offload".to_string(),
                        data: o.to_binary(),
                        ty: elf::SHT_PROGBITS,
                        flags: 0,
                        align: 8,
                    });
                }
            }
            cg.build_output()
        }
        a => {
            panic!("{}", a.backend_missing_msg());
        }
    })
}
