// src/codegen/mod.rs - 代码生成调度
// 第一性原理：codegen 入口只做"按 target 选后端"。

pub mod coff;
// Note: backend, registry, features, elf, asm_constraints, mini_asm moved to shared/
pub mod shared;
pub mod x86_64;

// 全部后端 family 无条件参与构建: aarch64/arm (AAPCS 家族) 与 mips/mips64/riscv64
// (RISC 家族) 都是受支持的一等后端, 不再用 cargo feature gate 隐藏能力缺口.
pub mod aarch64;
pub mod arm;
pub mod mips;
pub mod riscv64;

use crate::ast::{Block, BlockItem, Decl, Designator, Expr, InitItem, Program, Stmt, Type};
use crate::const_eval::{self, ConstValue};
use crate::debug;
use crate::diagnostics::{Diagnostic, Severity, Span};
use crate::ir::Module;
use crate::target::Target;
pub use shared::backend::{Backend, CommonImage, CompileInput, EmitOptions};
pub use shared::features::FeatureSet;
pub use shared::registry::backend_for;

/// Unified diagnostic policy: fail only on Error severity
fn has_fatal_diagnostics(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(|d| d.sev == Severity::Error)
}

// Note: EmitOptions imported via 'pub use' above from backend module

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
        },
        Decl::Var(v) => {
            if let Some(init) = &v.init {
                validate_inline_asm_expr(init, diags);
            }
        },
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
        _ => {},
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
        },
        Stmt::Expr { e, .. } => validate_inline_asm_expr(e, diags),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            validate_inline_asm_expr(cond, diags);
            validate_inline_asm_stmt(then_, diags);
            if let Some(e) = else_ {
                validate_inline_asm_stmt(e, diags);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            validate_inline_asm_expr(cond, diags);
            validate_inline_asm_stmt(body, diags);
        },
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
        },
        Stmt::Block { b, .. } => validate_inline_asm_block(b, diags),
        Stmt::GotoIndirect { expr, .. } => validate_inline_asm_expr(expr, diags),
        Stmt::Label { stmt, .. } => validate_inline_asm_stmt(stmt, diags),
        // C++ 零成本 EH: 受保护调用的内部调用语句仍按普通语句递归校验,
        // 落地垫序言只含两个栈槽写入, 不含内联汇编。
        Stmt::Invoke { call, .. } => validate_inline_asm_stmt(call, diags),
        Stmt::EhLandingPad { .. } => {},
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
        },
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
        },
        Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::Goto { .. }
        | Stmt::Fallthrough { .. } => {},
    }
}

fn validate_inline_asm_expr(expr: &Expr, diags: &mut Vec<Diagnostic>) {
    match expr {
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            validate_inline_asm_expr(l, diags);
            validate_inline_asm_expr(r, diags);
        },
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
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            validate_inline_asm_expr(cond, diags);
            validate_inline_asm_expr(then_, diags);
            validate_inline_asm_expr(else_, diags);
        },
        Expr::Index { arr, idx, .. } => {
            validate_inline_asm_expr(arr, diags);
            validate_inline_asm_expr(idx, diags);
        },
        Expr::Member { expr, .. } => validate_inline_asm_expr(expr, diags),
        Expr::StmtExpr { block, .. } => validate_inline_asm_block(block, diags),
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for init in inits {
                validate_inline_asm_init(init, diags);
            }
        },
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
        },
        Expr::VaArg { ap, .. } => validate_inline_asm_expr(ap, diags),
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::ComplexLit { .. }
        | Expr::ComplexIntLit { .. }
        | Expr::String { .. }
        | Expr::Char { .. }
        | Expr::Ident { .. }
        | Expr::SizeofTy { .. }
        | Expr::AlignofTy { .. }
        | Expr::LabelAddr { .. }
        | Expr::VaArgPack { .. }
        | Expr::Offsetof { .. } => {},
    }
}

fn validate_inline_asm_init(init: &InitItem, diags: &mut Vec<Diagnostic>) {
    match init {
        InitItem::Expr(e) => validate_inline_asm_expr(e, diags),
        InitItem::Designated { designator, value } => {
            validate_inline_asm_designator(designator, diags);
            validate_inline_asm_expr(value, diags);
        },
    }
}

fn validate_inline_asm_designator(designator: &Designator, diags: &mut Vec<Diagnostic>) {
    match designator {
        Designator::Index(e) => validate_inline_asm_expr(e, diags),
        Designator::Range(l, r) => {
            validate_inline_asm_expr(l, diags);
            validate_inline_asm_expr(r, diags);
        },
        Designator::Chain(items) => {
            for item in items {
                validate_inline_asm_designator(item, diags);
            }
        },
        Designator::Field(_) => {},
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
                    format!("output operand {} constraint '{}' missing '=' or '+' prefix", i, c),
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
        let effective =
            matched_constraint(constraint, output_constraints, input_constraints, outputs.len())
                .unwrap_or(constraint);
        validate_inline_asm_operand(expr, effective, true, span, diags);
    }
    for (idx, expr) in inputs.iter().enumerate() {
        let constraint = input_constraints
            .get(idx)
            .map(String::as_str)
            .unwrap_or("r");
        let effective =
            matched_constraint(constraint, output_constraints, input_constraints, outputs.len())
                .unwrap_or(constraint);
        validate_inline_asm_operand(expr, effective, false, span, diags);
    }

    // 4. P0-5: 早期约束模式验证 — 拒绝不支持的约束字母
    let constraint_errors =
        shared::asm_constraints::validate_asm_constraints(output_constraints, input_constraints);
    for err_msg in &constraint_errors {
        diags.push(
            Diagnostic::warn("W0311", err_msg.clone(), span.clone()).with_hint(
                "use supported constraints: r, m, i, g, n, q, =r, =m, +r, +m, memory, cc",
            ),
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
    // C 函数名在表达式中自动衰减为函数指针, 对 asm 约束检查也应如此.
    // 内核 DEFINE_IDTENTRY_IRQ 等宏将函数名作为 'i' 约束操作数.
    if let Type::Func(_, _) = ty {
        // 函数类型衰减为指针后视为整数类 (指针)
        return;
    }
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
            },
            Some('+') => {
                is_output = true;
                is_inout = true;
                s = &s[1..];
            },
            Some('&') => {
                early = true;
                s = &s[1..];
            },
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
            },
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
        .any(|c| matches!(c, 'i' | 'n' | 'I' | 'N' | 'M' | 'X' | 'E' | 'J' | 'K' | 'L' | 'O' | 'P'))
}

fn asm_expr_can_be_immediate(expr: &Expr) -> bool {
    match const_eval::eval_full(expr, false) {
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
            'i' | 'n'
                | 'I'
                | 'J'
                | 'K'
                | 'L'
                | 'M'
                | 'N'
                | 'O'
                | 'P'
                | 'X'
                | 'E'
                | 'F'
                | 'G'
                | 'H'
                | 's'
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
                _ => {
                    if *signed {
                        "int"
                    } else {
                        "unsigned int"
                    }
                },
            };
            debug::DwarfType {
                name: name.to_string(),
                byte_size: (*bits as u64).div_ceil(8),
                kind: debug::DwarfTypeKind::Base(if *signed {
                    debug::TypeEncoding::Signed
                } else {
                    debug::TypeEncoding::Unsigned
                }),
            }
        },
        ValueType::Float { bits } => {
            let name = match *bits {
                32 => "float",
                64 => "double",
                128 => "long double",
                _ => "double",
            };
            debug::DwarfType {
                name: name.to_string(),
                byte_size: (*bits as u64).div_ceil(8),
                kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Float),
            }
        },
        ValueType::Float128 => debug::DwarfType {
            name: "__float128".to_string(),
            byte_size: 16,
            kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Float),
        },
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
        ValueType::Aggregate {
            name,
            size,
            is_union,
        } => {
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
        },
        ValueType::Function { ret, params } => debug::DwarfType {
            name: "function".to_string(),
            byte_size: 0,
            kind: debug::DwarfTypeKind::Func {
                ret: Box::new(value_type_to_dwarf(ret)),
                params: params.iter().map(value_type_to_dwarf).collect(),
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
pub(crate) fn ast_type_to_dwarf(ty: &crate::ast::Type) -> debug::DwarfType {
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
        Type::Char8 => debug::DwarfType {
            name: "char8_t".to_string(),
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
        Type::Float128 => debug::DwarfType {
            name: "__float128".to_string(),
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
        },
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
        },
        Type::Named(name) => debug::DwarfType {
            name: name.clone(),
            byte_size: 0,                     // 未知大小，稍后解析
            kind: debug::DwarfTypeKind::Void, // 占位
        },
        Type::Struct(name, fields, size, _) => {
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
        },
        Type::Union(name, fields, size, _) => {
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
        },
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
        },
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
        },
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
            },
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
    let func_ranges: Vec<(u64, u64)> = functions.iter().map(|f| (f.low_pc, f.high_pc)).collect();

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
                let size = sd
                    .fields
                    .last()
                    .map(|f| f.offset + f.ty.size())
                    .unwrap_or(0);
                types.push(debug::DwarfType {
                    name: format!("struct {}", sd.name.name),
                    byte_size: size as u64,
                    kind: debug::DwarfTypeKind::Struct { members },
                });
            },
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
            },
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
            },
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
            },
            _ => {},
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

/// Return the absolute 64-bit (or 32-bit for 32-bit archs) relocation
/// type used for DWARF debug sections.  Each architecture defines its
/// own absolute relocation type in the ELF specification.
fn debug_abs_reloc_type(arch: crate::cli::Arch) -> u32 {
    use crate::cli::Arch;
    match arch {
        Arch::X86_64 => shared::elf::R_X86_64_64,
        Arch::AArch64 => shared::elf::R_AARCH64_ABS64,
        Arch::RiscV64 => shared::elf::R_RISCV_64,
        Arch::RiscV32 => shared::elf::R_RISCV_32,
        Arch::Mips64 => shared::elf::R_MIPS_64,
        Arch::Mips => shared::elf::R_MIPS_32,
        Arch::Arm => shared::elf::R_ARM_ABS32,
    }
}

/// Unified code generation entry point
/// First Principle: use registry to find backend, then delegate
/// This replaces the giant match statement with a clean abstraction
///
/// 返回 `Emitted` 而不是 `Vec<u8>`: “这次发的是 ELF 还是 GAS 文本”必须
/// 在类型上不可省略, 否则降级后的文本会被写进 `.o` 并按可执行文件算权限位。
///
/// Ok 的第二项是**非致命诊断**（警告）。为什么要在类型上带出来：以前后端的
/// `self.diagnostics` 里的警告在 `image.diagnostics` 里只参与「是否致命」的判定就
/// 被丢掉，于是 codegen 阶段根本没有警告通道 —— 而 Phase 6.4 要求的「不得静默」
/// 与「不得把 gcc 能编的源码弄红」两件事只能靠这个通道同时成立（否则只能选一边）。
pub fn emit(
    module: &Module,
    target: &Target,
    opts: EmitOptions,
) -> Result<(shared::asm_source::Emitted, Vec<Diagnostic>), Vec<Diagnostic>> {
    use crate::cli::AsmFallback;
    use shared::asm_source::{passthrough_diagnostics, Emitted};
    use shared::features::Feature;

    // -S 与 -c 都需后端以可重定位模式构建 (旧行为: relocatable || emit_asm)
    let relocatable = opts.relocatable || opts.emit_asm;
    // Check if backend exists for this architecture (no panic!)
    let mut backend = backend_for(target.arch, target, relocatable).ok_or_else(|| {
        vec![Diagnostic::err(
            "E0399",
            format!("Architecture {:?} has no backend implementation", target.arch),
            Span::point("", 0, 0),
        )]
    })?;

    // Prepare compilation input
    let input = CompileInput {
        module,
        target,
        opts: &opts,
    };

    // 「这个后端的家能力面」必须在后端被调用**之前**查（Phase 6.4）：-S 与 -c 走
    // 的是两条不同的发射入口，只在其中一条上查就会给出「同一个源 -c 报错、
    // -S 静默产出」的分裂答案。今天只有 x86_64 能认寄存器家，其他六个后端收到
    // 非栈家或非法寄存器名都是硬错误；合法名但本后端实现不了是警告（不改变更
    // 能编的事实），所以这里把非致命的几条一路带到最后再交给调用方。
    let home_diags = shared::homes::check_program_homes(target.arch, module.source());
    if has_fatal_diagnostics(&home_diags) {
        return Err(home_diags);
    }
    let home_warnings: Vec<Diagnostic> = home_diags
        .into_iter()
        .filter(|d| !matches!(d.sev, Severity::Error))
        .collect();

    // -S 路径: 直接返回汇编文本, 不做 ELF 打包
    if opts.emit_asm {
        let bytes = backend.render_asm(&input)?;
        return Ok((Emitted::Text(utf8_text(bytes, &opts.source_name)?), home_warnings));
    }

    // Perform compilation through unified trait
    let image = backend.compile(&input)?;

    // Apply unified diagnostic policy
    if has_fatal_diagnostics(&image.diagnostics) {
        return Err(image.diagnostics);
    }
    // 后端自己采集的非致命诊断（以前在这一步被丢掉）现在与家能力面的警告一起上行。
    let mut notes = home_warnings;
    notes.extend(
        image
            .diagnostics
            .iter()
            .filter(|d| !matches!(d.sev, Severity::Error))
            .cloned(),
    );

    // ===== TU 级降级判定 (全仓唯一一处) =====
    //
    // 判据是结构化的 `image.asm_passthrough`, 不是扫 diagnostic 字符串。
    // 降级动作调 `render_asm_from_state`: 消费刚跑完的那一次 codegen,
    // 绝不重跑 —— 重跑就是本方案要消灭的“同一源码两次结果不同”。
    if !image.asm_passthrough.is_empty() {
        match opts.asm_fallback {
            AsmFallback::Error => return Err(passthrough_diagnostics(&image.asm_passthrough)),
            AsmFallback::Gas => {
                // 降级的前提: 本后端的 -S 是真文本。不具备时给出准确的
                // 能力缺失错误, 而不是写一个汇编不过的 .s 冒充成功。
                if !backend.features().has(Feature::GasTextOutput) {
                    return Err(vec![Diagnostic::err(
                        "E0312",
                        format!(
                            "--asm-fallback=gas is unavailable for {:?} target: this backend's \
                             -S output is not yet a complete, assemblable translation of the TU",
                            target.arch
                        ),
                        Span::point(&opts.source_name, 0, 0),
                    )]);
                }
                let bytes = backend.render_asm_from_state(module)?;
                return Ok((Emitted::Text(utf8_text(bytes, &opts.source_name)?), notes));
            },
        }
    }

    // Packaging (ELF ET_REL / ET_EXEC / COFF) is the backend's own responsibility
    Ok((Emitted::Object(backend.build_object()?), notes))
}

/// 后端文本均以 ASCII/GAS 语法输出; 非 UTF-8 字节意味着后端渲染出错,
/// 必须作为硬错误报出而非用 lossy 转换静默替换成 U+FFFD。
fn utf8_text(bytes: Vec<u8>, source: &str) -> Result<String, Vec<Diagnostic>> {
    String::from_utf8(bytes).map_err(|_| {
        vec![Diagnostic::err(
            "E0301",
            format!("assembly text output for '{source}' is not valid UTF-8"),
            Span::point(source, 0, 0),
        )]
    })
}
