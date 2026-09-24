// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{Decl, Field, Program};
use crate::codegen::shared::elf::{self, ComdatGroup, ElfRela, ElfSection, ElfSym, STB_GLOBAL, STB_LOCAL, STB_WEAK, STT_FUNC, STT_OBJECT, STT_TLS};
use crate::ir::{ExprLiteral, Global, Module, TypedExpr, ValueType};
use crate::target::Target;
use std::collections::HashMap;
use std::sync::Arc;

use super::common::{align_rodata, align_up_u64, find_func, value_type_size, var_is_static};

#[derive(Clone, Copy)]
pub(crate) enum BranchKind {
    Jal,
    Beqz,
    Bne,
    Beq,
    Bge,
    Blt,
}

/// RVV 向量配置参数
#[derive(Clone, Copy, Debug)]
pub(crate) struct RvvConfig {
    vlen: u16,      // 向量寄存器位宽 (128/256/512/1024)
    elen: u16,      // 最大元素位宽
    vtype_val: u32, // vtype 编码值
}

impl Default for RvvConfig {
    fn default() -> Self {
        Self {
            vlen: 128,
            elen: 64,
            vtype_val: 0,
        } // vtype=0: SEW=64, LMUL=1, TA, MA
    }
}

pub struct Riscv64Codegen {
    pub(crate) code: Vec<u8>,
    pub(crate) rodata: Vec<u8>,
    pub(crate) entry: u64,
    pub(crate) relocatable: bool,
    pub(crate) symbols: Vec<ElfSym>,
    pub(crate) relocations: Vec<ElfRela>,
    pub(crate) rodata_relocations: Vec<ElfRela>,
    pub(crate) func_sizes: HashMap<String, u64>,
    pub(crate) labels: HashMap<String, u64>,
    pub(crate) locals: HashMap<String, i32>,
    pub(crate) global_rodata: HashMap<String, u64>,
    pub(crate) float_refs: Vec<(u64, u64)>,
    pub(crate) const64_refs: Vec<(u64, u64)>, // (code_pos, rodata_offset) for 64-bit const loads
    pub(crate) sym_index: HashMap<String, u32>,
    pub(crate) stack_bytes: i32,
    pub(crate) frame_bytes: i32,
    /// 帧内保存 ra(x1)/fp(s0) 的偏移; -1 = 本函数为叶子，不保存 (不扰动帧)。
    pub(crate) ret_addr_off: i32,
    pub(crate) emitted_return: bool,
    pub(crate) source_name: String,
    pub(crate) label_serial: u32,
    pub(crate) pending_branches: Vec<(u64, String, BranchKind)>,
    pub(crate) extra_sections: Vec<ElfSection>,
    /// COMDAT group definitions
    pub(crate) comdat_groups: Vec<ComdatGroup>,
    pub(crate) current_return_ty: Option<ValueType>,
    pub(crate) aggregate_fields: HashMap<String, (Arc<Vec<Field>>, bool)>,
    pub(crate) rvv: RvvConfig,
    /// 当前函数是否为 variadic
    pub(crate) is_current_func_variadic: bool,
    /// 寄存器保存区在 frame 内的起始偏移 (a0..a7 + fa0..fa7 顺序存 16 槽)
    pub(crate) va_save_base_off: i32,
    /// 下一个 GP va_arg 槽位索引 (0..7, 对应 a0..a7)
    pub(crate) va_gp_offs: i32,
    /// 下一个 float va_arg 槽位索引 (0..7, 对应 fa0..fa7)
    pub(crate) va_flt_offs: i32,
    /// 文件级 asm 原始文本行 (GlobalAsm)
    pub(crate) raw_global_asm_lines: Vec<String>,
    /// 函数级内联 asm 原始文本行: (函数名, 行列表)
    pub(crate) raw_func_asm_lines: Vec<(String, Vec<String>)>,
    /// -S 模式标志: true 时保留 asm 文本输出, false 时编码机器码
    pub(crate) asm_text_mode: bool,
    /// 编译诊断 (未识别内联汇编指令等)
    pub(crate) diagnostics: Vec<crate::diagnostics::Diagnostic>,
    /// Stack protector: 当前函数 canary 槽位偏移, 0 表示未启用
    pub(crate) canary_slot_off: i32,
    /// -mrecord-mcount: ftrace call site positions for __mcount_loc section
    pub(crate) mcount_loc_entries: Vec<(u64, String)>,
    /// TLS 全局变量集合 (名称 -> 偏移), 用于 load_global_addr_a0 分发 TLS 访问序列
    pub(crate) tls_globals: HashMap<String, u64>,
    pub(crate) _target: Target,
    /// break/continue 标签栈: (break_label, continue_label)
    pub(crate) break_continue_stack: Vec<(String, String)>,
    /// GNU asm label 映射: C 名 → asm("label") 覆盖名
    pub(crate) asm_label_map: HashMap<String, String>,
    /// DWARF 行号表条目: (pc, source_line)
    pub(crate) line_entries: Vec<(u64, u32)>,
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
            asm_text_mode: false,
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

    pub(crate) fn set_asm_text_mode(&mut self, v: bool) {
        self.asm_text_mode = v;
    }

    /// 解析 GNU asm label: 如果 C 名有对应的 asm 标签则返回 asm 标签名，
    /// 否则返回原始 C 名。用于 ELF 符号名、重定位目标和汇编标签发射。
    pub(crate) fn resolve_asm_label<'a>(&'a self, c_name: &'a str) -> &'a str {
        self.asm_label_map.get(c_name).map(|s| s.as_str()).unwrap_or(c_name)
    }

    /// 收集 GNU asm label 映射和 TLS 变量访问模型
    pub(crate) fn collect_asm_labels(&mut self, prog: &Program) {
        for decl in &prog.decls {
            match decl {
                Decl::Func(f) => {
                    if let Some(ref label) = f.asm_label {
                        self.asm_label_map.insert(f.name.name.clone(), label.clone());
                    }
                }
                Decl::Var(v) => {
                    if let Some(ref label) = v.asm_label {
                        self.asm_label_map.insert(v.name.name.clone(), label.clone());
                    }
                }
                _ => {}
            }
        }
    }

    /// Encode resolved asm text into machine code and emit to code buffer.
    /// P0-5: unrecognized instructions produce hard errors, not NOPs.
    pub(crate) fn encode_and_emit_asm(&mut self, resolved: &str, span_file: &str, span_line: u32) {
        let lines = crate::codegen::shared::mini_asm::parse_asm_lines(resolved);
        match crate::codegen::shared::mini_asm::encode_asm_lines_for_arch(&lines, "riscv64", true) {
            Ok(bytes) => {
                self.code.extend_from_slice(&bytes);
            }
            Err(unrecognized) => {
                for mnem in &unrecognized {
                    let span = crate::diagnostics::Span::point(span_file, span_line, 1);
                    let mut diag = crate::diagnostics::Diagnostic::err(
                        "E0311",
                        format!(
                            "unsupported inline asm instruction '{}' on riscv64 target",
                            mnem
                        ),
                        span,
                    );
                    diag.hint = Some(
                        "add encoding support or use asm text mode (-S)".to_string(),
                    );
                    self.diagnostics.push(diag);
                }
            }
        }
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
            }
            crate::cli::FtraceMode::FEntry | crate::cli::FtraceMode::RecordMCount => {
                self.ensure_sym("__fentry__");
            }
            crate::cli::FtraceMode::None => {}
        }
        // -mrecord-mcount: generate __mcount_loc section with call site addresses
        if matches!(self._target.ftrace_mode, crate::cli::FtraceMode::RecordMCount)
            && !self.mcount_loc_entries.is_empty()
        {
            let mut mcount_data =
                Vec::with_capacity(self.mcount_loc_entries.len() * 8);
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
                }
                Decl::UnionDef(u) => {
                    self.aggregate_fields
                        .insert(u.name.name.clone(), (Arc::new(u.fields.clone()), true));
                }
                _ => {}
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
            }
            Some(ExprLiteral::Float(v)) => {
                let bits = (v as f32).to_bits();
                self.rodata[start..start + 4.min(size)]
                    .copy_from_slice(&bits.to_le_bytes()[..4.min(size)]);
            }
            _ => {}
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
                        self.sym_index.insert(f.name.name.clone(), self.symbols.len() as u32);
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
                    }
                }
            }
        }
    }

}
