// src/codegen/mod.rs - 代码生成调度
// 第一性原理: codegen 入口只做"按 target 选后端"。

pub mod aarch64;
pub mod arm;
pub mod disasm;
pub mod elf;
pub mod mips;
pub mod mips64;
pub mod riscv64;
pub mod x86_64;

use crate::ast::{Block, BlockItem, Decl, Designator, Expr, InitItem, Program, Stmt, Type};
use crate::const_eval;
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

    // 2. 匹配约束 (matching constraint) 数字越界检测 (E0015)
    for (input_idx, constraint) in input_constraints.iter().enumerate() {
        let (_, _, _, match_digit) = parse_asm_constraint_flags(constraint);
        if let Some(n) = match_digit {
            let n_usize = n as usize;
            if n_usize >= output_count {
                diags.push(Diagnostic::err(
                    "E0015",
                    format!(
                        "matching constraint references invalid operand number '{}' for input operand {}",
                        n, input_idx
                    ),
                    span.clone(),
                ).with_hint(
                    if output_count == 0 {
                        "no output operands defined; matching constraints require outputs".into()
                    } else {
                        format!(
                            "matching constraint digits must reference an output operand (0 to {})",
                            output_count.saturating_sub(1)
                        )
                    }
                ));
            }
            if n_usize >= total_ops {
                diags.push(Diagnostic::err(
                    "E0015",
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
            diags.push(Diagnostic::err(
                "E0015",
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

    // 3. 类型校验 (E0310) — 原始功能
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
    diags.push(
        Diagnostic::err(
            "E0310",
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
        return !is_output && const_eval::eval(expr).is_ok();
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

#[allow(clippy::too_many_arguments)]
pub fn emit(
    module: &Module,
    target: &Target,
    relocatable: bool,
    emit_asm: bool,
    source_name: &str,
    debug_level: u8,
    sanitizers: SanitizerFlags,
    lto_enabled: bool,
    function_sections: bool,
    parallel_codegen: bool,
    bounds_check: bool,
    trapv: bool,
    offload: Option<&crate::profile::offload::OffloadSection>,
    line_map: &[LineSpan],
) -> Vec<u8> {
    match target.arch {
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
            if emit_asm {
                return cg.build_asm_output(module.source()).into_bytes();
            }
            if debug_level > 0 && relocatable {
                let funcs: Vec<(&str, u64)> = module
                    .functions
                    .iter()
                    .filter_map(|f| cg.labels.get(&f.name).map(|&pc| (f.name.as_str(), pc)))
                    .collect();
                // 将 asm 块行号记录转换为 DWARF LineEntry
                let asm_line_entries: Vec<debug::LineEntry> = cg
                    .asm_line_entries
                    .iter()
                    .map(|&(pc_start, _pc_end, src_line)| debug::LineEntry {
                        pc: pc_start,
                        line: src_line,
                        column: 1,
                        file_idx: 0,
                        discriminator: 0,
                        is_stmt: true,
                        basic_block: false,
                    })
                    .collect();
                let mut dbg = debug::build_dwarf(source_name, &funcs);
                // 如果有 asm 行号条目, 重新生成带行号的 DWARF
                if !asm_line_entries.is_empty() {
                    let cu = debug::CompileUnit {
                        source_name: source_name.to_string(),
                        comp_dir: None,
                        producer: format!("dcc {}", env!("CARGO_PKG_VERSION")),
                        language: 0x000C, // DW_LANG_C99
                        functions: funcs
                            .iter()
                            .map(|(name, pc)| debug::DwarfFuncInfo {
                                name: name.to_string(),
                                linkage_name: None,
                                low_pc: *pc,
                                high_pc: *pc + cg.func_sizes.get(*name).copied().unwrap_or(16),
                                decl_line: 0,
                                decl_file_idx: 0,
                                return_type: debug::DwarfType {
                                    name: "void".to_string(),
                                    byte_size: 0,
                                    kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Unsigned),
                                },
                                params: vec![],
                                is_external: true,
                                is_inline: false,
                                is_declaration: false,
                            })
                            .collect(),
                        globals: vec![],
                        types: vec![],
                        line_entries: asm_line_entries,
                        line_files: vec![source_name.to_string()],
                        line_dirs: vec![".".to_string()],
                        low_pc: cg.asm_line_entries.first().map(|e| e.0).unwrap_or(0),
                        high_pc: cg.asm_line_entries.last().map(|e| e.1).unwrap_or(0),
                        func_ranges: funcs
                            .iter()
                            .map(|(name, pc)| {
                                (*pc, *pc + cg.func_sizes.get(*name).copied().unwrap_or(16))
                            })
                            .collect(),
                        cfi_rows: vec![],
                    };
                    let opts = debug::DebugOptions {
                        level: debug_level,
                        source_name: source_name.to_string(),
                        ..Default::default()
                    };
                    dbg = debug::build_dwarf_full(&cu, &opts);
                }
                cg.extra_sections.extend(debug::dwarf_elf_sections(&dbg));
                cg.extra_sections.push(elf::ElfSection {
                    name: ".BTF".to_string(),
                    data: debug::build_btf_probe(),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 4,
                });
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
            // ORC unwind 信息: .orc_unwind_ip + .orc_unwind 节区对
            // 内核 ORC 解包器需要这对节区进行栈展开. 内核构建用
            // -fno-asynchronous-unwind-tables (unwind_tables=false) 关闭 .eh_frame,
            // 此时 ORC 是内核解包器的唯一栈展开来源; -g 调试构建也一并输出.
            if relocatable && (!target.unwind_tables || cg.emit_cfi) {
                cg.build_orc_sections(module);
            }
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
                cg.extra_sections.push(elf::ElfSection {
                    name: ".llvm.lto".to_string(),
                    data: crate::lto::serialize_summary(&summary_unit),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
            }
            if relocatable {
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
            // HWASAN (-fsanitize=hwaddress) 在 AArch64 上启用 MTE 硬件 tag:
            // prologue tag 栈帧 + 指针 tag 流程由后端直接发射 IRG/ADDG/STG。
            cg.set_mte_enabled(sanitizers.hwasan);
            if emit_asm {
                cg.compile(module);
                return cg.build_asm_output(module).into_bytes();
            }
            cg.compile(module);
            if debug_level > 0 && relocatable {
                let funcs: Vec<(&str, u64)> = module
                    .functions
                    .iter()
                    .filter_map(|f| cg.labels.get(&f.name).map(|&pc| (f.name.as_str(), pc)))
                    .collect();
                let opts = debug::DebugOptions {
                    level: debug_level,
                    source_name: source_name.to_string(),
                    arch: debug::ArchKind::AArch64,
                    ..debug::DebugOptions::default()
                };
                let cu = debug::CompileUnit {
                    source_name: source_name.to_string(),
                    comp_dir: None,
                    producer: "dcc".to_string(),
                    language: 12, // DW_LANG_C99
                    functions: funcs
                        .iter()
                        .map(|(n, pc)| debug::DwarfFuncInfo {
                            name: n.to_string(),
                            linkage_name: None,
                            low_pc: *pc,
                            high_pc: *pc + 64,
                            decl_line: 0,
                            decl_file_idx: 0,
                            return_type: debug::DwarfType {
                                name: "void".to_string(),
                                byte_size: 0,
                                kind: debug::DwarfTypeKind::Void,
                            },
                            params: Vec::new(),
                            is_external: *n == "main",
                            is_inline: false,
                            is_declaration: false,
                        })
                        .collect(),
                    globals: Vec::new(),
                    types: Vec::new(),
                    line_entries: Vec::new(),
                    line_files: vec![source_name.to_string()],
                    line_dirs: Vec::new(),
                    low_pc: funcs.first().map(|(_, pc)| *pc).unwrap_or(0),
                    high_pc: funcs.last().map(|(_, pc)| *pc + 64).unwrap_or(0),
                    func_ranges: funcs.iter().map(|(_, pc)| (*pc, 64)).collect(),
                    cfi_rows: Vec::new(),
                };
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
                cg.push_extra_section(elf::ElfSection {
                    name: ".BTF".to_string(),
                    data: debug::build_btf_probe(),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 4,
                });
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
            if emit_asm {
                cg.compile(module);
                return cg.build_asm_output(module).into_bytes();
            }
            cg.compile(module);
            if debug_level > 0 && relocatable {
                let funcs: Vec<(&str, u64)> = module
                    .functions
                    .iter()
                    .filter_map(|f| cg.labels.get(&f.name).map(|&pc| (f.name.as_str(), pc)))
                    .collect();
                let opts = debug::DebugOptions {
                    level: debug_level,
                    source_name: source_name.to_string(),
                    arch: debug::ArchKind::RiscV64,
                    ..debug::DebugOptions::default()
                };
                let cu = debug::CompileUnit {
                    source_name: source_name.to_string(),
                    comp_dir: None,
                    producer: "dcc".to_string(),
                    language: 12, // DW_LANG_C99
                    functions: funcs
                        .iter()
                        .map(|(n, pc)| debug::DwarfFuncInfo {
                            name: n.to_string(),
                            linkage_name: None,
                            low_pc: *pc,
                            high_pc: *pc + 64,
                            decl_line: 0,
                            decl_file_idx: 0,
                            return_type: debug::DwarfType {
                                name: "void".to_string(),
                                byte_size: 0,
                                kind: debug::DwarfTypeKind::Void,
                            },
                            params: Vec::new(),
                            is_external: *n == "main",
                            is_inline: false,
                            is_declaration: false,
                        })
                        .collect(),
                    globals: Vec::new(),
                    types: Vec::new(),
                    line_entries: Vec::new(),
                    line_files: vec![source_name.to_string()],
                    line_dirs: Vec::new(),
                    low_pc: funcs.first().map(|(_, pc)| *pc).unwrap_or(0),
                    high_pc: funcs.last().map(|(_, pc)| *pc + 64).unwrap_or(0),
                    func_ranges: funcs.iter().map(|(_, pc)| (*pc, 64)).collect(),
                    cfi_rows: Vec::new(),
                };
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
                cg.push_extra_section(elf::ElfSection {
                    name: ".BTF".to_string(),
                    data: debug::build_btf_probe(),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 4,
                });
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
            if emit_asm {
                cg.compile(module);
                return cg.build_asm_output(module).into_bytes();
            }
            cg.compile(module);
            if debug_level > 0 && relocatable {
                let funcs: Vec<(&str, u64)> = module
                    .functions
                    .iter()
                    .filter_map(|f| cg.labels.get(&f.name).map(|&pc| (f.name.as_str(), pc)))
                    .collect();
                let opts = debug::DebugOptions {
                    level: debug_level,
                    source_name: source_name.to_string(),
                    arch: debug::ArchKind::Mips,
                    ..debug::DebugOptions::default()
                };
                let cu = debug::CompileUnit {
                    source_name: source_name.to_string(),
                    comp_dir: None,
                    producer: "dcc".to_string(),
                    language: 12,
                    functions: funcs
                        .iter()
                        .map(|(n, pc)| debug::DwarfFuncInfo {
                            name: n.to_string(),
                            linkage_name: None,
                            low_pc: *pc,
                            high_pc: *pc + 64,
                            decl_line: 0,
                            decl_file_idx: 0,
                            return_type: debug::DwarfType {
                                name: "void".to_string(),
                                byte_size: 0,
                                kind: debug::DwarfTypeKind::Void,
                            },
                            params: Vec::new(),
                            is_external: *n == "main",
                            is_inline: false,
                            is_declaration: false,
                        })
                        .collect(),
                    globals: Vec::new(),
                    types: Vec::new(),
                    line_entries: Vec::new(),
                    line_files: vec![source_name.to_string()],
                    line_dirs: Vec::new(),
                    low_pc: funcs.first().map(|(_, pc)| *pc).unwrap_or(0),
                    high_pc: funcs.last().map(|(_, pc)| *pc + 64).unwrap_or(0),
                    func_ranges: funcs.iter().map(|(_, pc)| (*pc, 64)).collect(),
                    cfi_rows: Vec::new(),
                };
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
            if emit_asm {
                cg.compile(module);
                return cg.build_asm_output(module).into_bytes();
            }
            cg.compile(module);
            if debug_level > 0 && relocatable {
                let funcs: Vec<(&str, u64)> = module
                    .functions
                    .iter()
                    .filter_map(|f| cg.labels().get(&f.name).map(|&pc| (f.name.as_str(), pc)))
                    .collect();
                let opts = debug::DebugOptions {
                    level: debug_level,
                    source_name: source_name.to_string(),
                    arch: debug::ArchKind::Mips64,
                    ..debug::DebugOptions::default()
                };
                let cu = debug::CompileUnit {
                    source_name: source_name.to_string(),
                    comp_dir: None,
                    producer: "dcc".to_string(),
                    language: 12,
                    functions: funcs
                        .iter()
                        .map(|(n, pc)| debug::DwarfFuncInfo {
                            name: n.to_string(),
                            linkage_name: None,
                            low_pc: *pc,
                            high_pc: *pc + 64,
                            decl_line: 0,
                            decl_file_idx: 0,
                            return_type: debug::DwarfType {
                                name: "void".to_string(),
                                byte_size: 0,
                                kind: debug::DwarfTypeKind::Void,
                            },
                            params: Vec::new(),
                            is_external: *n == "main",
                            is_inline: false,
                            is_declaration: false,
                        })
                        .collect(),
                    globals: Vec::new(),
                    types: Vec::new(),
                    line_entries: Vec::new(),
                    line_files: vec![source_name.to_string()],
                    line_dirs: Vec::new(),
                    low_pc: funcs.first().map(|(_, pc)| *pc).unwrap_or(0),
                    high_pc: funcs.last().map(|(_, pc)| *pc + 64).unwrap_or(0),
                    func_ranges: funcs.iter().map(|(_, pc)| (*pc, 64)).collect(),
                    cfi_rows: Vec::new(),
                };
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
            if emit_asm {
                cg.compile(module);
                return cg.build_asm_output(module).into_bytes();
            }
            cg.compile(module);
            if debug_level > 0 && relocatable {
                let funcs: Vec<(&str, u64)> = module
                    .functions
                    .iter()
                    .filter_map(|f| cg.labels.get(&f.name).map(|&pc| (f.name.as_str(), pc)))
                    .collect();
                let opts = debug::DebugOptions {
                    level: debug_level,
                    source_name: source_name.to_string(),
                    arch: debug::ArchKind::Arm,
                    ..debug::DebugOptions::default()
                };
                let cu = debug::CompileUnit {
                    source_name: source_name.to_string(),
                    comp_dir: None,
                    producer: "dcc".to_string(),
                    language: 12,
                    functions: funcs
                        .iter()
                        .map(|(n, pc)| debug::DwarfFuncInfo {
                            name: n.to_string(),
                            linkage_name: None,
                            low_pc: *pc,
                            high_pc: *pc + 64,
                            decl_line: 0,
                            decl_file_idx: 0,
                            return_type: debug::DwarfType {
                                name: "void".to_string(),
                                byte_size: 0,
                                kind: debug::DwarfTypeKind::Void,
                            },
                            params: Vec::new(),
                            is_external: *n == "main",
                            is_inline: false,
                            is_declaration: false,
                        })
                        .collect(),
                    globals: Vec::new(),
                    types: Vec::new(),
                    line_entries: Vec::new(),
                    line_files: vec![source_name.to_string()],
                    line_dirs: Vec::new(),
                    low_pc: funcs.first().map(|(_, pc)| *pc).unwrap_or(0),
                    high_pc: funcs.last().map(|(_, pc)| *pc + 64).unwrap_or(0),
                    func_ranges: funcs.iter().map(|(_, pc)| (*pc, 64)).collect(),
                    cfi_rows: Vec::new(),
                };
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
    }
}
