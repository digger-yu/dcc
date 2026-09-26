// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{Decl, Program};
use crate::codegen::shared::elf::{
    self, ComdatGroup, ElfSection, ElfSym, SHF_ALLOC, SHF_GNU_RETAIN, SHT_PROGBITS, STB_GLOBAL,
    STB_LOCAL, STB_WEAK, STT_FUNC, STT_GNU_IFUNC, STT_OBJECT, STT_TLS,
};
use crate::ir::{ExprLiteral, Global, Module, TypedExpr};
use crate::target::Target;
use std::collections::HashMap;
use std::sync::Arc;

use super::common::{align_rodata, align_up_u64, find_func, value_type_size, var_is_static};
use super::struct_def::Aarch64Codegen;

#[derive(Clone, Copy)]
pub(crate) enum BranchKind {
    B,
    Cbz,
}

impl Aarch64Codegen {
    pub(crate) fn new(target: Target, relocatable: bool) -> Self {
        Self {
            code: Vec::new(),
            rodata: Vec::new(),
            entry: if relocatable { 0 } else { 0x400000 + 64 + 56 },
            relocatable,
            symbols: Vec::new(),
            relocations: Vec::new(),
            rodata_relocations: Vec::new(),
            func_sizes: HashMap::new(),
            labels: HashMap::new(),
            locals: HashMap::new(),
            global_rodata: HashMap::new(),
            float_refs: Vec::new(),
            sym_index: HashMap::new(),
            stack_bytes: 0,
            frame_bytes: 0,
            lr_off: -1,
            emitted_return: false,
            source_name: String::new(),
            label_serial: 0,
            pending_branches: Vec::new(),
            pending_bcond: Vec::new(),
            pending_bl_calls: Vec::new(),
            pending_adr_exec: Vec::new(),
            extra_sections: Vec::new(),
            comdat_groups: Vec::new(),
            current_return_ty: None,
            aggregate_fields: HashMap::new(),
            is_current_func_variadic: false,
            va_save_base_off: 0,
            va_gp_offs: 0,
            va_flt_offs: 0,
            mte_enabled: false,
            tag_frame_size: 0,
            mte_save_off: 0,
            saved_sret_ptr: None,
            canary_slot_off: 0,
            tls_globals: HashMap::new(),
            raw_global_asm_lines: Vec::new(),
            raw_func_asm_lines: Vec::new(),
            asm_text_mode: false,
            diagnostics: Vec::new(),
            mcount_loc_entries: Vec::new(),
            _target: target,
            break_continue_stack: Vec::new(),
            asm_label_map: HashMap::new(),
            line_entries: Vec::new(),
        }
    }

    pub(crate) fn set_source_name(&mut self, name: impl Into<String>) {
        self.source_name = name.into();
    }

    pub(crate) fn set_asm_text_mode(&mut self, v: bool) {
        self.asm_text_mode = v;
    }

    /// 解析 GNU asm label: 如果 C 名有对应的 asm 标签则返回 asm 标签名，
    /// 否则返回原始 C 名。用于 ELF 符号名、重定位目标和汇编标签发射。
    pub(crate) fn resolve_asm_label<'a>(&'a self, c_name: &'a str) -> &'a str {
        self.asm_label_map
            .get(c_name)
            .map(|s| s.as_str())
            .unwrap_or(c_name)
    }

    /// 收集 GNU asm label 映射和 TLS 变量访问模型
    pub(crate) fn collect_asm_labels(&mut self, prog: &Program) {
        for decl in &prog.decls {
            match decl {
                Decl::Func(f) => {
                    if let Some(ref label) = f.asm_label {
                        self.asm_label_map
                            .insert(f.name.name.clone(), label.clone());
                    }
                },
                Decl::Var(v) => {
                    if let Some(ref label) = v.asm_label {
                        self.asm_label_map
                            .insert(v.name.name.clone(), label.clone());
                    }
                },
                _ => {},
            }
        }
    }

    /// Encode resolved asm text into machine code and emit to code buffer.
    /// Used in -c mode to produce actual machine code for inline asm.
    /// P0-5: unrecognized instructions produce hard errors, not NOPs.
    pub(crate) fn encode_and_emit_asm(&mut self, resolved: &str, span_file: &str, span_line: u32) {
        let lines = crate::codegen::shared::mini_asm::parse_asm_lines(resolved);
        match crate::codegen::shared::mini_asm::encode_asm_lines_for_arch(&lines, "aarch64", false)
        {
            Ok(bytes) => {
                self.code.extend_from_slice(&bytes);
            },
            Err(unrecognized) => {
                for mnem in &unrecognized {
                    let span = crate::diagnostics::Span::point(span_file, span_line, 1);
                    let mut diag = crate::diagnostics::Diagnostic::err(
                        "E0311",
                        format!("unsupported inline asm instruction '{}' on aarch64 target", mnem),
                        span,
                    );
                    diag.hint = Some("add encoding support or use asm text mode (-S)".to_string());
                    self.diagnostics.push(diag);
                }
            },
        }
    }

    /// 启用/禁用 AArch64 MTE 硬件 tag (HWASAN 栈帧 tag 模式)。
    /// 由 `-fsanitize=hwaddress` 在 AArch64 目标上触发。
    pub(crate) fn set_mte_enabled(&mut self, on: bool) {
        self.mte_enabled = on;
    }

    /// MTE 调用点前缀: 调用外部 (非 MTE) 代码前清除 sp 的 tag。
    /// `and sp, x19, #0x00ffffffffffffff` (x19 = 本帧带 tag 的帧基址)
    pub(crate) fn mte_untag_sp_for_call(&mut self) {
        self.emit(&0x9280de7fu32.to_le_bytes());
    }

    /// MTE 调用点后缀: 调用返回后恢复本帧 tag。
    /// `add sp, x19, #0` (mov sp, x19)
    pub(crate) fn mte_retag_sp_after_call(&mut self) {
        self.emit(&0x9100027fu32.to_le_bytes());
    }

    /// 当前函数是否需要 MTE tag 栈帧 (prologue 打 tag / 调用点 untag-retag)。
    pub(crate) fn mte_active(&self) -> bool {
        self.mte_enabled && self.frame_bytes > 0
    }

    pub(crate) fn push_extra_section(&mut self, sec: ElfSection) {
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

    pub(crate) fn compile(&mut self, module: &Module) {
        let prog = module.source();
        self.collect_asm_labels(prog);
        self.build_aggregate_fields(prog);
        self.emit_globals(module, prog);
        if self.relocatable {
            self.compile_rel(module, prog);
            self.finalize_branches();
            self.finalize_pending_bl();
        } else {
            self.compile_exec(module, prog);
            self.finalize_exec();
        }
    }

    pub(crate) fn build_aggregate_fields(&mut self, prog: &Program) {
        self.aggregate_fields.clear();
        for decl in &prog.decls {
            match decl {
                Decl::StructDef(s) => {
                    self.aggregate_fields
                        .insert(s.name.name.clone(), (Arc::new(s.fields.clone()), false));
                },
                Decl::UnionDef(u) => {
                    self.aggregate_fields
                        .insert(u.name.name.clone(), (Arc::new(u.fields.clone()), true));
                },
                _ => {},
            }
        }
    }

    pub(crate) fn emit_globals(&mut self, module: &Module, prog: &Program) {
        for g in &module.globals {
            self.emit_global(g, prog);
        }
        if !self.rodata.is_empty() {
            self.symbols.push(ElfSym {
                name: ".rodata".to_string(),
                section: 2,
                value: 0,
                size: self.rodata.len() as u64,
                bind: STB_LOCAL,
                ty: elf::STT_SECTION,
                visibility: elf::STV_DEFAULT,
            });
            self.sym_index
                .insert(".rodata".to_string(), self.symbols.len() as u32);
        }
    }

    pub(crate) fn emit_global(&mut self, g: &Global, prog: &Program) {
        let size = value_type_size(&g.ty).max(1);
        let align = size.max(4) as u64;
        let off = align_rodata(&mut self.rodata, align);
        self.global_rodata.insert(g.name.clone(), off);
        if let Some(init) = &g.initializer {
            self.write_global_bytes(init, size);
        } else {
            self.rodata.resize(self.rodata.len() + size, 0);
        }
        // TLS 全局变量: 记录到 tls_globals 集合, codegen 在访问时分发 TLS 序列
        if g.is_tls {
            self.tls_globals.insert(g.name.clone(), off);
        }
        if self.relocatable {
            // 内部链接的文件作用域变量必须是 STB_LOCAL: 函数内 static local
            // (g.is_static_local) 和顶层 `static int g;` 都不能以可导出符号出现,
            // 否则跨 TU 同名会误链接/冲突。此前无条件 STB_GLOBAL。判据抄已正确
            // 处理函数的 func_bind (is_static/is_weak 查 AST)。
            let is_internal = g.is_static_local || var_is_static(prog, &g.name);
            let bind = if is_internal { STB_LOCAL } else { STB_GLOBAL };
            self.symbols.push(ElfSym {
                name: g.name.clone(),
                section: 2,
                value: off,
                size: size as u64,
                bind,
                ty: if g.is_tls { STT_TLS } else { STT_OBJECT },
                visibility: elf::STV_DEFAULT,
            });
            self.sym_index
                .insert(g.name.clone(), self.symbols.len() as u32);
        }
    }

    pub(crate) fn write_global_bytes(&mut self, init: &TypedExpr, size: usize) {
        let start = self.rodata.len();
        self.rodata.resize(start + size, 0);
        match init.literal {
            Some(ExprLiteral::Int(v)) => {
                let bytes = (v as i64).to_le_bytes();
                self.rodata[start..start + size.min(8)].copy_from_slice(&bytes[..size.min(8)]);
            },
            Some(ExprLiteral::Float(v)) => {
                let bits = (v.as_f64() as f32).to_bits();
                self.rodata[start..start + 4.min(size)]
                    .copy_from_slice(&bits.to_le_bytes()[..4.min(size)]);
            },
            _ => {},
        }
    }

    pub(crate) fn compile_exec(&mut self, module: &Module, prog: &Program) {
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
        self.label("_start");
        let bl_pos = self.pos();
        self.emit_bl_placeholder();
        self.mov_x8_imm(93);
        self.svc_zero();
        for func in &module.functions {
            // __attribute__((alias/copy)): 别名函数无函数体, 跳过代码生成
            if find_func(prog, &func.name)
                .and_then(|f| f.alias_name.as_ref())
                .is_some()
            {
                continue;
            }
            if find_func(prog, &func.name)
                .and_then(|f| f.body.as_ref())
                .is_none()
            {
                continue;
            }
            // -fpatchable-function-entry=(N,M): emit NOPs around function entry
            if let Some((n, m)) = self._target.patchable_function_entry {
                let prefix_len = m.min(n);
                let suffix_len = n.saturating_sub(prefix_len);
                for _ in 0..prefix_len {
                    self.emit(&0xd503201fu32.to_le_bytes());
                }
                // 使用 asm label 覆盖符号名 (如果有)
                let sym_name = self.resolve_asm_label(&func.name).to_string();
                self.label(&sym_name);
                for _ in 0..suffix_len {
                    self.emit(&0xd503201fu32.to_le_bytes());
                }
            } else {
                // 使用 asm label 覆盖符号名 (如果有)
                let sym_name = self.resolve_asm_label(&func.name).to_string();
                self.label(&sym_name);
            }
            let start = self.pos();
            self.gen_function(func, find_func(prog, &func.name));
            self.func_sizes
                .insert(func.name.clone(), self.pos() - start);
            if func.name == "main" {
                self.patch_bl(bl_pos, start);
            }
        }
    }

    pub(crate) fn compile_rel(&mut self, module: &Module, prog: &Program) {
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
        let mut text_off = 0u64;
        for func in &module.functions {
            // __attribute__((alias/copy)): 别名函数无函数体, 跳过代码生成
            if find_func(prog, &func.name)
                .and_then(|f| f.alias_name.as_ref())
                .is_some()
            {
                continue;
            }
            if find_func(prog, &func.name)
                .and_then(|f| f.body.as_ref())
                .is_none()
            {
                continue;
            }
            // -fpatchable-function-entry=(N,M): emit NOPs around function entry
            // M NOPs before the label, (N-M) NOPs after
            if let Some((n, m)) = self._target.patchable_function_entry {
                let prefix_len = m.min(n);
                let suffix_len = n.saturating_sub(prefix_len);
                for _ in 0..prefix_len {
                    // AArch64 NOP: 0xD503201F
                    self.emit(&0xd503201fu32.to_le_bytes());
                }
                // 使用 asm label 覆盖符号名 (如果有)
                let sym_name = self.resolve_asm_label(&func.name).to_string();
                self.label(&sym_name);
                for _ in 0..suffix_len {
                    self.emit(&0xd503201fu32.to_le_bytes());
                }
            } else {
                // 使用 asm label 覆盖符号名 (如果有)
                let sym_name = self.resolve_asm_label(&func.name).to_string();
                self.label(&sym_name);
            }
            let start = self.pos();
            self.gen_function(func, find_func(prog, &func.name));
            let size = self.pos() - start;
            self.func_sizes.insert(func.name.clone(), size);
            let func_bind = if find_func(prog, &func.name)
                .map(|f| f.is_weak)
                .unwrap_or(false)
            {
                STB_WEAK
            } else if find_func(prog, &func.name)
                .map(|f| f.is_static)
                .unwrap_or(false)
            {
                STB_LOCAL
            } else {
                STB_GLOBAL
            };
            self.symbols.push(ElfSym {
                name: func.name.clone(),
                section: 1,
                value: text_off,
                size,
                bind: func_bind,
                ty: if find_func(prog, &func.name)
                    .and_then(|f| f.ifunc_resolver.as_ref())
                    .is_some()
                {
                    STT_GNU_IFUNC
                } else {
                    STT_FUNC
                },
                visibility: elf::STV_DEFAULT,
            });
            self.sym_index
                .insert(func.name.clone(), self.symbols.len() as u32);

            // COMDAT group for weak/inline functions: linker deduplicates
            if find_func(prog, &func.name)
                .map(|f| f.is_weak)
                .unwrap_or(false)
            {
                self.comdat_groups.push(ComdatGroup {
                    signature: func.name.clone(),
                    member_sec_indices: vec![],
                });
            }

            // attribute((used)): 发射 .used.<name> 节并标记 SHF_GNU_RETAIN
            if find_func(prog, &func.name)
                .map(|f| f.is_used_attr)
                .unwrap_or(false)
            {
                let sec = ElfSection {
                    name: format!(".used.{}", func.name),
                    data: vec![],
                    ty: SHT_PROGBITS,
                    flags: SHF_ALLOC | SHF_GNU_RETAIN,
                    align: 1,
                    group_name: None,
                };
                self.extra_sections.push(sec);
            }
            text_off += size;
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
                            visibility: elf::STV_DEFAULT,
                        };
                        self.sym_index
                            .insert(f.name.name.clone(), self.symbols.len() as u32);
                        self.symbols.push(alias_sym);
                    }
                }
            }
        }

        // Stack protector: add undefined symbols for canary and fail handler
        if self._target.stack_protector {
            self.ensure_sym_object("__stack_chk_guard");
            self.ensure_sym("__stack_chk_fail");
        }
        // ftrace: add undefined symbols for tracing
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
            let mut mcount_data = Vec::with_capacity(self.mcount_loc_entries.len() * 8);
            for &(call_pos, _) in &self.mcount_loc_entries {
                // Each entry is a 64-bit address (placeholder, patched by
                // linker via R_AARCH64_ABS64 relocation or direct address)
                mcount_data.extend_from_slice(&call_pos.to_le_bytes());
            }
            self.extra_sections.push(elf::ElfSection {
                name: "__mcount_loc".to_string(),
                data: mcount_data,
                ty: elf::SHT_PROGBITS,
                flags: elf::SHF_ALLOC,
                align: 8,
                group_name: None,
            });
        }
        // Retpoline/thunk extern: add undefined symbols for Spectre mitigations
        // Note: These are primarily x86 options, but we emit the symbols for
        // build compatibility if they're accidentally enabled on AArch64.
        if self._target.function_return_thunk_extern {
            self.ensure_sym("__x86_return_thunk");
        }
        if self._target.indirect_branch_thunk_extern {
            self.ensure_sym("__x86_indirect_call_thunk_r11");
            self.ensure_sym("__x86_indirect_jump_thunk_r11");
        }
    }

    pub(crate) fn finalize_exec(&mut self) {
        let rodata_start = self.entry + align_up_u64(self.code.len() as u64, 8);
        let refs: Vec<_> = self.float_refs.drain(..).collect();
        for (code_pos, rodata_off) in refs {
            self.patch_ldr_u32(code_pos, rodata_start + rodata_off);
        }
        let adr_pending: Vec<_> = self.pending_adr_exec.drain(..).collect();
        for (at, rd, target) in adr_pending {
            self.patch_adr(at, rd, target);
        }
        self.finalize_branches();
        self.finalize_pending_bl();
    }

    pub(crate) fn finalize_pending_bl(&mut self) {
        let pending: Vec<_> = self.pending_bl_calls.drain(..).collect();
        for (at, name) in pending {
            if let Some(&target) = self.labels.get(&name) {
                self.patch_bl(at, target);
            }
        }
    }

    pub(crate) fn finalize_branches(&mut self) {
        let pending: Vec<_> = self.pending_branches.drain(..).collect();
        for (at, label, kind) in pending {
            if let Some(&target) = self.labels.get(&label) {
                match kind {
                    BranchKind::B => self.patch_b26(at, target),
                    BranchKind::Cbz => self.patch_cbz(at, target),
                }
            }
        }
        let bcond_pending: Vec<_> = self.pending_bcond.drain(..).collect();
        for (at, label, cond) in bcond_pending {
            if let Some(&target) = self.labels.get(&label) {
                self.patch_bcond(at, target, cond);
            }
        }
    }
}
