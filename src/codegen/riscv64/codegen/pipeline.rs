// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{Decl, Program};
use crate::codegen::shared::asm_source::AsmPassthroughRecord;
use crate::codegen::shared::elf::{
    self, ComdatGroup, ElfSection, ElfSym, STB_GLOBAL, STB_LOCAL, STB_WEAK, STT_FUNC, STT_OBJECT,
    STT_TLS,
};
use crate::ir::{ExprLiteral, Global, Module, TypedExpr};
use crate::target::Target;
use std::collections::HashMap;
use std::sync::Arc;

use super::common::{align_rodata, align_up_u64, find_func, value_type_size, var_is_static};
use super::struct_def::{Riscv64Codegen, RvvConfig};
use crate::codegen::shared::asm_operand::LayoutMap;

impl Riscv64Codegen {
    /// 产出当前 `labels`/`func_sizes` 的只读快照 (`LayoutMap`)。
    pub(crate) fn snapshot_layout(&self) -> LayoutMap {
        let text_end = self.entry + self.code.len() as u64;
        LayoutMap::snapshot(&self.labels, &self.func_sizes, text_end)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum BranchKind {
    Jal,
    Beqz,
    Bne,
    Beq,
    Bge,
    Blt,
}

impl Riscv64Codegen {
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
            const64_refs: Vec::new(),
            sym_index: HashMap::new(),
            stack_bytes: 0,
            frame_bytes: 0,
            ret_addr_off: -1,
            emitted_return: false,
            source_name: String::new(),
            label_serial: 0,
            pending_branches: Vec::new(),
            extra_sections: Vec::new(),
            comdat_groups: Vec::new(),
            current_return_ty: None,
            aggregate_fields: HashMap::new(),
            rvv: RvvConfig::default(),
            is_current_func_variadic: false,
            va_save_base_off: 0,
            va_gp_offs: 0,
            va_flt_offs: 0,
            raw_global_asm_lines: Vec::new(),
            raw_func_asm_lines: Vec::new(),
            asm_passthrough: Vec::new(),
            asm_passthrough_regions: Vec::new(),
            diagnostics: Vec::new(),
            canary_slot_off: 0,
            mcount_loc_entries: Vec::new(),
            tls_globals: HashMap::new(),
            _target: target,
            break_continue_stack: Vec::new(),
            asm_label_map: HashMap::new(),
            line_entries: Vec::new(),
        }
    }

    pub(crate) fn set_source_name(&mut self, name: impl Into<String>) {
        self.source_name = name.into();
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

    /// 处理一条已解析的 asm 语句 —— 薄壳。
    ///
    /// 策略、编码、透传判定全部在 `asm_source::record_asm_stmt` (全仓唯一实现);
    /// 本后端只提供数据容器 (见下方 `AsmStmtSink` 实现)。旧版本在这里各自
    /// 写了一份“能不能编、编不出怎么报告”的逻辑, 5 个后端 5 微妙不一致。
    pub(crate) fn emit_asm_stmt(
        &mut self,
        key: &str,
        resolved: &str,
        span_file: &str,
        span_line: u32,
    ) {
        crate::codegen::shared::asm_source::record_asm_stmt(
            self,
            crate::codegen::shared::asm_source::AsmIsa::RiscV64,
            key,
            resolved,
            span_file,
            span_line,
        );
    }

    /// 兼容旧名: 无显式 key 的调用点 (naked / 单行)。
    pub(crate) fn encode_and_emit_asm(
        &mut self,
        resolved: &str,
        span_file: &str,
        span_line: u32,
    ) {
        let key = format!("__inline_{}", self.label_serial);
        self.label_serial += 1;
        self.emit_asm_stmt(&key, resolved, span_file, span_line);
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
        } else {
            self.compile_exec(module, prog);
            self.finalize_exec();
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
            let mut mcount_data = Vec::with_capacity(self.mcount_loc_entries.len() * 8);
            for &(call_pos, _) in &self.mcount_loc_entries {
                // Each entry is a 64-bit address
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
            // 否则跨 TU 同名会误链接/冲突。此前无条件 STB_GLOBAL。
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
        let jal_pos = self.pos();
        self.emit_jal_placeholder(1); // ra
        self.li_a7(93);
        self.li_a0(0);
        self.ecall();
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
            // -fpatchable-function-entry: emit NOPs before and after label
            if let Some((n, m)) = self._target.patchable_function_entry {
                let prefix_len = m.min(n);
                let suffix_len = n.saturating_sub(prefix_len);
                for _ in 0..prefix_len {
                    // RISC-V NOP: ADDI x0, x0, 0 = 0x00000013
                    self.code.extend_from_slice(&0x00000013u32.to_le_bytes());
                }
                // 使用 asm label 覆盖符号名 (如果有)
                let sym_name = self.resolve_asm_label(&func.name).to_string();
                self.label(&sym_name);
                for _ in 0..suffix_len {
                    self.code.extend_from_slice(&0x00000013u32.to_le_bytes());
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
                self.patch_jal(jal_pos, start);
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
            // -fpatchable-function-entry: emit NOPs before and after label
            if let Some((n, m)) = self._target.patchable_function_entry {
                let prefix_len = m.min(n);
                let suffix_len = n.saturating_sub(prefix_len);
                for _ in 0..prefix_len {
                    // RISC-V NOP: ADDI x0, x0, 0 = 0x00000013
                    self.code.extend_from_slice(&0x00000013u32.to_le_bytes());
                }
                // 使用 asm label 覆盖符号名 (如果有)
                let sym_name = self.resolve_asm_label(&func.name).to_string();
                self.label(&sym_name);
                for _ in 0..suffix_len {
                    self.code.extend_from_slice(&0x00000013u32.to_le_bytes());
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
                ty: STT_FUNC,
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
    }

    pub(crate) fn finalize_exec(&mut self) {
        let rodata_start = self.entry + align_up_u64(self.code.len() as u64, 8);
        let refs: Vec<_> = self.float_refs.drain(..).collect();
        for (code_pos, rodata_off) in refs {
            self.patch_auipc_load(code_pos, rodata_start + rodata_off);
        }
        let c64: Vec<_> = self.const64_refs.drain(..).collect();
        for (code_pos, rodata_off) in c64 {
            self.patch_auipc_ld(code_pos, rodata_start + rodata_off);
        }
        self.finalize_branches();
    }

    pub(crate) fn finalize_branches(&mut self) {
        let pending: Vec<_> = self.pending_branches.drain(..).collect();
        for (at, label, kind) in pending {
            if let Some(&target) = self.labels.get(&label) {
                match kind {
                    BranchKind::Jal => self.patch_jal(at, target),
                    BranchKind::Beqz => self.patch_beqz(at, target),
                    BranchKind::Bne | BranchKind::Beq | BranchKind::Bge | BranchKind::Blt => {
                        let bytes = &self.code[at as usize..at as usize + 4];
                        let insn = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                        let rs1 = ((insn >> 15) & 0x1f) as u8;
                        let rs2 = ((insn >> 20) & 0x1f) as u8;
                        let funct3 = match kind {
                            BranchKind::Beq => 0u32,
                            BranchKind::Bne => 1u32,
                            BranchKind::Bge => 5u32,
                            BranchKind::Blt => 4u32,
                            _ => unreachable!(),
                        };
                        self.patch_branch(at, rs1, rs2, target, funct3);
                    },
                }
            }
        }
    }
}

// asm_source 的数据容器接入: 本后端只负责存储, 不负责任何“能不能编、
// 编不出怎么办”的判断 —— 那份策略全仓只有一份 (record_asm_stmt)。
impl crate::codegen::shared::asm_source::AsmOutcomeSink for Riscv64Codegen {
    fn pc(&self) -> u64 {
        self.pos()
    }
    fn push_code(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }
    fn save_passthrough_region(&mut self, pc: u64, lines: Vec<String>) {
        self.asm_passthrough_regions.push((pc, lines));
    }
    fn add_passthrough_records(&mut self, recs: Vec<AsmPassthroughRecord>) {
        self.asm_passthrough.extend(recs);
    }
}

// AsmStmtSink 的文本面: 本后端保存内联 asm 原文, 使 -S 与降级都有可信文本来源。
impl crate::codegen::shared::asm_source::AsmStmtSink for Riscv64Codegen {
    fn save_asm_text(&mut self, key: &str, lines: Vec<String>) {
        self.raw_func_asm_lines.push((key.to_string(), lines));
    }
}
