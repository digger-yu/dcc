// ARM backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{Decl, Program};
use crate::codegen::shared::asm_constraints::{
    self, resolve_asm_operands, resolve_asm_template_enhanced, AsmArch,
};
use crate::codegen::shared::elf::{self, ElfSym, STB_GLOBAL, STT_FUNC};
use crate::ir::{Global, Module, ValueType};
use crate::target::Target;
use std::collections::HashMap;

use super::struct_def::ArmCodegen;

#[derive(Clone, Copy)]
pub(crate) enum BranchKind {
    Beq,
    B,
}

impl ArmCodegen {
    pub(crate) fn new(target: Target, relocatable: bool) -> Self {
        Self {
            code: Vec::new(),
            rodata: Vec::new(),
            relocatable,
            symbols: Vec::new(),
            relocations: Vec::new(),
            rodata_relocations: Vec::new(),
            func_sizes: HashMap::new(),
            labels: HashMap::new(),
            locals: HashMap::new(),
            global_rodata: HashMap::new(),
            string_pool: HashMap::new(),
            sym_index: HashMap::new(),
            stack_bytes: 0,
            frame_bytes: 0,
            source_name: String::new(),
            extra_sections: Vec::new(),
            current_return_ty: None,
            aggregate_fields: HashMap::new(),
            emitted_return: false,
            label_serial: 0,
            pending_bl_calls: Vec::new(),
            pending_branches: Vec::new(),
            break_labels: Vec::new(),
            continue_labels: Vec::new(),
            raw_global_asm_lines: Vec::new(),
            raw_func_asm_lines: Vec::new(),
            asm_text_mode: false,
            entry: if relocatable { 0 } else { 0x400000 + 52 + 32 },
            start_call_pos: None,
            is_current_func_variadic: false,
            va_save_base_off: 0,
            va_gp_offs: 0,
            va_flt_offs: 0,
            float_pool: HashMap::new(),
            canary_slot_off: 0,
            mcount_loc_entries: Vec::new(),
            diagnostics: Vec::new(),
            _target: target,
        }
    }

    pub(crate) fn set_source_name(&mut self, name: impl Into<String>) {
        self.source_name = name.into();
    }

    pub(crate) fn set_asm_text_mode(&mut self, v: bool) {
        self.asm_text_mode = v;
    }

    /// Encode resolved asm text into machine code and emit to code buffer.
    /// P0-5: unrecognized instructions produce hard errors, not NOPs.
    pub(crate) fn encode_and_emit_asm(&mut self, resolved: &str, span_file: &str, span_line: u32) {
        let lines = crate::codegen::shared::mini_asm::parse_asm_lines(resolved);
        match crate::codegen::shared::mini_asm::encode_asm_lines_for_arch(&lines, "arm", false) {
            Ok(bytes) => {
                self.code.extend_from_slice(&bytes);
            },
            Err(unrecognized) => {
                for mnem in &unrecognized {
                    let span = crate::diagnostics::Span::point(span_file, span_line, 1);
                    let mut diag = crate::diagnostics::Diagnostic::err(
                        "E0311",
                        format!("unsupported inline asm instruction '{}' on arm target", mnem),
                        span,
                    );
                    diag.hint = Some("add encoding support or use asm text mode (-S)".to_string());
                    self.diagnostics.push(diag);
                }
            },
        }
    }

    pub(crate) fn push_extra_section(&mut self, sec: crate::codegen::shared::elf::ElfSection) {
        self.extra_sections.push(sec);
    }

    pub(crate) fn func_frames(&self, module: &Module) -> Vec<crate::debug::FuncFrame> {
        module
            .functions
            .iter()
            .filter_map(|f| {
                self.labels.get(&f.name).and_then(|&pc| {
                    self.func_sizes
                        .get(&f.name)
                        .map(|&sz| crate::debug::FuncFrame::new(pc, sz as u32))
                })
            })
            .collect()
    }

    pub(crate) fn build_aggregate_fields(&mut self, prog: &Program) {
        self.aggregate_fields.clear();
        for decl in &prog.decls {
            match decl {
                Decl::StructDef(s) => {
                    self.aggregate_fields.insert(
                        s.name.name.clone(),
                        (std::sync::Arc::new(s.fields.clone()), false),
                    );
                },
                Decl::UnionDef(u) => {
                    self.aggregate_fields
                        .insert(u.name.name.clone(), (std::sync::Arc::new(u.fields.clone()), true));
                },
                _ => {},
            }
        }
    }

    pub(crate) fn compile(&mut self, module: &Module) {
        let prog = module.source();
        self.build_aggregate_fields(prog);
        self.emit_globals(module);
        // 文件级 asm: 收集原始文本行 (增强版约束解析 + 寄存器名替换)
        for decl in &prog.decls {
            if let Decl::GlobalAsm {
                asm_str,
                output_names,
                output_constraints,
                input_names,
                input_constraints,
                ..
            } = decl
            {
                let resolved = Self::resolve_asm_template_enhanced(
                    asm_str,
                    output_constraints,
                    input_constraints,
                    output_names,
                    input_names,
                    &[],
                );
                for line in resolved.split('\n') {
                    let t = line.trim();
                    if !t.is_empty() {
                        self.raw_global_asm_lines.push(t.to_string());
                    }
                }
            }
        }
        if self.relocatable {
            self.compile_rel(module, prog);
        } else {
            self.compile_exec(module, prog);
        }
        // __attribute__((alias)): 创建函数别名符号
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if let Some(ref alias_target) = f.alias_name {
                    if let Some(&target_idx) = self.sym_index.get(alias_target) {
                        let target_sym = &self.symbols[target_idx as usize];
                        let alias_sym = ElfSym {
                            name: f.name.name.clone(),
                            section: target_sym.section,
                            value: target_sym.value,
                            size: target_sym.size,
                            bind: STB_GLOBAL,
                            ty: STT_FUNC,
                            visibility: 0,
                        };
                        self.sym_index
                            .insert(f.name.name.clone(), self.symbols.len() as u32);
                        self.symbols.push(alias_sym);
                    }
                }
            }
        }
        // Stack canary: add __stack_chk_guard/__stack_chk_fail undefined symbols
        if self._target.stack_protector {
            self.ensure_sym_object("__stack_chk_guard");
            self.ensure_sym("__stack_chk_fail");
        }
        // FTRACE: add mcount/__fentry__ undefined symbols
        match self._target.ftrace_mode {
            crate::cli::FtraceMode::MCount => {
                self.ensure_sym("mcount");
            },
            crate::cli::FtraceMode::FEntry | crate::cli::FtraceMode::RecordMCount => {
                self.ensure_sym("__fentry__");
            },
            crate::cli::FtraceMode::None => {},
        }
        // -mrecord-mcount: generate __mcount_loc section with call site addresses
        if matches!(self._target.ftrace_mode, crate::cli::FtraceMode::RecordMCount)
            && !self.mcount_loc_entries.is_empty()
        {
            let mut mcount_data = Vec::with_capacity(self.mcount_loc_entries.len() * 4); // ARM: 32-bit addresses
            for &(call_pos, _) in &self.mcount_loc_entries {
                mcount_data.extend_from_slice(&(call_pos as u32).to_le_bytes());
            }
            self.extra_sections.push(elf::ElfSection {
                name: "__mcount_loc".to_string(),
                data: mcount_data,
                ty: elf::SHT_PROGBITS,
                flags: elf::SHF_ALLOC,
                align: 4,
                group_name: None,
            });
        }
    }

    pub(crate) fn compile_exec(&mut self, module: &Module, prog: &Program) {
        // _start: BL main (placeholder)
        let start_pos = self.code.len() as u64;
        self.labels.insert("_start".to_string(), start_pos);
        // BL placeholder (will be patched to main)
        let bl_pos = self.code.len() as u64;
        self.start_call_pos = Some(bl_pos);
        self.code.extend_from_slice(&0xEB000000u32.to_le_bytes()); // BL offset=0
                                                                   // r0 already has main's return value
                                                                   // mov r7, #1 (__NR_exit)
        self.emit_mov_imm(7, 1);
        // svc #0
        let svc_inst: u32 = 0xEF000000; // SVC #0
        self.code.extend_from_slice(&svc_inst.to_le_bytes());
        // Compile functions
        for func in &module.functions {
            let has_body = prog
                .decls
                .iter()
                .find_map(|d| match d {
                    Decl::Func(f) if f.name.name == func.name => Some(f),
                    _ => None,
                })
                .and_then(|f| f.body.as_ref())
                .is_some();
            if !has_body {
                continue;
            }
            self.gen_function(
                func,
                prog.decls.iter().find_map(|d| match d {
                    Decl::Func(f) if f.name.name == func.name => Some(f),
                    _ => None,
                }),
            );
            // Patch BL to main
            if func.name == "main" {
                if let Some(bl_pos) = self.start_call_pos {
                    if let Some(&main_addr) = self.labels.get("main") {
                        self.patch_b(bl_pos, main_addr); // BL uses same offset encoding as B
                    }
                }
            }
        }
        // Finalize pending branches
        self.finalize_branches();
    }

    pub(crate) fn finalize_branches(&mut self) {
        let pending: Vec<_> = self.pending_branches.drain(..).collect();
        for (at, label, kind) in pending {
            if let Some(&target) = self.labels.get(&label) {
                match kind {
                    BranchKind::Beq => self.patch_beq(at, target),
                    BranchKind::B => self.patch_b(at, target),
                }
            }
        }
    }

    pub(crate) fn emit_globals(&mut self, module: &Module) {
        for g in &module.globals {
            if g.initializer.is_some() {
                self.emit_global_init(g);
            }
        }
    }

    pub(crate) fn emit_global_init(&mut self, g: &Global) {
        let offset = self.rodata.len() as u64;
        self.global_rodata.insert(g.name.clone(), offset);
        // Evaluate constant initializer or default to zero
        if let Some(init_expr) = &g.initializer {
            // Try to extract constant value from TypedExpr
            let val = match (&init_expr.kind, &init_expr.literal) {
                (crate::ir::ExprKind::Constant, Some(crate::ir::ExprLiteral::Int(v))) => {
                    Some(*v as i64)
                },
                _ => None,
            };
            if let Some(val) = val {
                // Emit proper size based on type
                let size = match &g.ty {
                    ValueType::Integer { bits, .. } => (*bits / 8).max(4) as usize,
                    ValueType::Pointer(_) => 4, // ARM32: 4-byte pointers
                    _ => 4,
                };
                if size == 8 {
                    self.rodata
                        .extend_from_slice(&(val as i32 as i64).to_le_bytes());
                } else {
                    self.rodata.extend_from_slice(&(val as i32).to_le_bytes());
                }
            } else {
                // Non-constant initializer: emit zero for now
                self.rodata.extend_from_slice(&[0u8; 4]);
            }
        } else {
            // No initializer: emit zero
            self.rodata.extend_from_slice(&[0u8; 4]);
        }
    }

    pub(crate) fn compile_rel(&mut self, module: &Module, prog: &Program) {
        for func in &module.functions {
            // __attribute__((alias/copy)): 别名函数无函数体, 跳过代码生成
            if prog.decls.iter().any(|d| matches!(d, Decl::Func(f) if f.name.name == func.name && f.alias_name.is_some())) {
                continue;
            }
            let ast = module.source().decls.iter().find_map(|d| match d {
                Decl::Func(f) if f.name.name == func.name => Some(f),
                _ => None,
            });
            self.gen_function(func, ast);
        }
    }

    /// 增强版 asm 模板占位符替换: 使用共享约束解析器分配寄存器并替换为实际寄存器名
    pub(crate) fn resolve_asm_template_enhanced(
        asm_str: &str,
        output_constraints: &[String],
        input_constraints: &[String],
        output_names: &[Option<String>],
        input_names: &[Option<String>],
        goto_labels: &[(String, String)],
    ) -> String {
        let pool = asm_constraints::arm_reg_pool();
        let operands = resolve_asm_operands(
            output_constraints,
            input_constraints,
            output_names,
            input_names,
            &pool,
        );
        resolve_asm_template_enhanced(
            asm_str,
            &operands,
            output_names,
            input_names,
            goto_labels,
            AsmArch::Arm,
            false,
        )
    }
}
