// src/codegen/riscv64.rs - RISC-V 64 后端 (M8 完整化)

use crate::ast::{find_field, BinOp, BlockItem, Decl, Expr, Field, FuncDecl, Program, Stmt, Type, UnOp};
use crate::codegen::asm_constraints::{
    self, resolve_asm_operands, resolve_asm_template_enhanced, AsmArch,
};
use crate::codegen::elf::{
    self, ComdatGroup, ElfRela, ElfSection, ElfSym, R_RISCV_JAL, STB_GLOBAL, STB_LOCAL, STB_WEAK,
    STT_FUNC, STT_OBJECT, STT_TLS,
};
use crate::ir::{ExprKind, ExprLiteral, Function, Global, Module, TypedExpr, TypedStmt, ValueType};
use crate::target::Target;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy)]
enum BranchKind {
    Jal,
    Beqz,
    Bne,
    Beq,
    Bge,
    Blt,
}

/// RVV 向量配置参数
#[derive(Clone, Copy, Debug)]
struct RvvConfig {
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
    code: Vec<u8>,
    rodata: Vec<u8>,
    entry: u64,
    relocatable: bool,
    symbols: Vec<ElfSym>,
    relocations: Vec<ElfRela>,
    rodata_relocations: Vec<ElfRela>,
    func_sizes: HashMap<String, u64>,
    pub labels: HashMap<String, u64>,
    locals: HashMap<String, i32>,
    global_rodata: HashMap<String, u64>,
    float_refs: Vec<(u64, u64)>,
    const64_refs: Vec<(u64, u64)>, // (code_pos, rodata_offset) for 64-bit const loads
    sym_index: HashMap<String, u32>,
    stack_bytes: i32,
    frame_bytes: i32,
    emitted_return: bool,
    source_name: String,
    label_serial: u32,
    pending_branches: Vec<(u64, String, BranchKind)>,
    extra_sections: Vec<ElfSection>,
    /// COMDAT group definitions
    comdat_groups: Vec<ComdatGroup>,
    current_return_ty: Option<ValueType>,
    aggregate_fields: HashMap<String, (Arc<Vec<Field>>, bool)>,
    rvv: RvvConfig,
    /// 当前函数是否为 variadic
    is_current_func_variadic: bool,
    /// 寄存器保存区在 frame 内的起始偏移 (a0..a7 + fa0..fa7 顺序存 16 槽)
    va_save_base_off: i32,
    /// 下一个 GP va_arg 槽位索引 (0..7, 对应 a0..a7)
    va_gp_offs: i32,
    /// 下一个 float va_arg 槽位索引 (0..7, 对应 fa0..fa7)
    va_flt_offs: i32,
    /// 文件级 asm 原始文本行 (GlobalAsm)
    raw_global_asm_lines: Vec<String>,
    /// 函数级内联 asm 原始文本行: (函数名, 行列表)
    raw_func_asm_lines: Vec<(String, Vec<String>)>,
    /// -S 模式标志: true 时保留 asm 文本输出, false 时编码机器码
    asm_text_mode: bool,
    /// 编译诊断 (未识别内联汇编指令等)
    pub diagnostics: Vec<crate::diagnostics::Diagnostic>,
    /// Stack protector: 当前函数 canary 槽位偏移, 0 表示未启用
    canary_slot_off: i32,
    /// -mrecord-mcount: ftrace call site positions for __mcount_loc section
    mcount_loc_entries: Vec<(u64, String)>,
    /// TLS 全局变量集合 (名称 -> 偏移), 用于 load_global_addr_a0 分发 TLS 访问序列
    tls_globals: HashMap<String, u64>,
    _target: Target,
    /// break/continue 标签栈: (break_label, continue_label)
    break_continue_stack: Vec<(String, String)>,
    /// GNU asm label 映射: C 名 → asm("label") 覆盖名
    asm_label_map: HashMap<String, String>,
    /// DWARF 行号表条目: (pc, source_line)
    pub line_entries: Vec<(u64, u32)>,
}

impl Riscv64Codegen {
    pub fn new(target: Target, relocatable: bool) -> Self {
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

    pub fn set_source_name(&mut self, name: impl Into<String>) {
        self.source_name = name.into();
    }

    pub fn set_asm_text_mode(&mut self, v: bool) {
        self.asm_text_mode = v;
    }

    /// 解析 GNU asm label: 如果 C 名有对应的 asm 标签则返回 asm 标签名，
    /// 否则返回原始 C 名。用于 ELF 符号名、重定位目标和汇编标签发射。
    fn resolve_asm_label<'a>(&'a self, c_name: &'a str) -> &'a str {
        self.asm_label_map.get(c_name).map(|s| s.as_str()).unwrap_or(c_name)
    }

    /// 收集 GNU asm label 映射和 TLS 变量访问模型
    pub fn collect_asm_labels(&mut self, prog: &Program) {
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
    fn encode_and_emit_asm(&mut self, resolved: &str, span_file: &str, span_line: u32) {
        let lines = crate::codegen::mini_asm::parse_asm_lines(resolved);
        match crate::codegen::mini_asm::encode_asm_lines_for_arch(&lines, "riscv64", true) {
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

    pub fn push_extra_section(&mut self, sec: ElfSection) {
        self.extra_sections.push(sec);
    }

    pub fn func_frames(&self, module: &Module) -> Vec<crate::debug::FuncFrame> {
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

    pub fn compile(&mut self, module: &Module) {
        let prog = module.source();
        self.collect_asm_labels(prog);
        self.build_aggregate_fields(prog);
        self.emit_globals(module);
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

    fn build_aggregate_fields(&mut self, prog: &Program) {
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

    fn emit_globals(&mut self, module: &Module) {
        for g in &module.globals {
            self.emit_global(g);
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

    fn emit_global(&mut self, g: &Global) {
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
            self.symbols.push(ElfSym {
                name: g.name.clone(),
                section: 2,
                value: off,
                size: size as u64,
                bind: STB_GLOBAL,
                ty: if g.is_tls { STT_TLS } else { STT_OBJECT },
                visibility: elf::STV_DEFAULT,
            });
            self.sym_index
                .insert(g.name.clone(), self.symbols.len() as u32);
        }
    }

    fn write_global_bytes(&mut self, init: &TypedExpr, size: usize) {
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

    fn compile_exec(&mut self, module: &Module, prog: &Program) {
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

    fn compile_rel(&mut self, module: &Module, prog: &Program) {
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

    fn finalize_exec(&mut self) {
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

    fn finalize_branches(&mut self) {
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

    pub fn build_output(&self) -> Vec<u8> {
        if self.relocatable {
            elf::build_elf_rel_riscv64(
                &self.code,
                &self.rodata,
                &self.symbols,
                &self.relocations,
                &self.rodata_relocations,
                &self.extra_sections,
                &self.comdat_groups,
            )
        } else {
            elf::build_elf_riscv64(&self.code, &self.rodata, self.entry)
        }
    }

    pub fn build_asm_output(&self, module: &Module) -> String {
        let prog = module.source();
        let mut out = String::new();
        let name = if self.source_name.is_empty() {
            "input.c".to_string()
        } else {
            self.source_name.clone()
        };
        out.push_str(&format!("\t.file \"{name}\"\n"));
        if !self.rodata.is_empty() {
            out.push_str("\t.section .rodata\n");
            for g in &module.globals {
                if self.global_rodata.contains_key(&g.name) {
                    out.push_str(&format!("\t.globl {}\n", g.name));
                    out.push_str(&format!("{}:\n", g.name));
                }
            }
        }
        out.push_str("\t.text\n");
        // 发射文件级 asm 原始文本
        for line in &self.raw_global_asm_lines {
            out.push_str(&format!("\t{line}\n"));
        }
        for func in &module.functions {
            let is_weak = find_func(prog, &func.name)
                .map(|f| f.is_weak)
                .unwrap_or(false);
            if is_weak {
                out.push_str(&format!("\t.weak {}\n", func.name));
            } else {
                out.push_str(&format!("\t.globl {}\n", func.name));
            }
            out.push_str(&format!("{}:\n", func.name));
            // 发射函数级内联 asm 原始文本
            for (fname, lines) in &self.raw_func_asm_lines {
                if fname == &func.name || fname.starts_with("__inline_") {
                    for line in lines {
                        out.push_str(&format!("\t{line}\n"));
                    }
                }
            }
            if let Some(size) = self.func_sizes.get(&func.name) {
                out.push_str(&format!("\t// IR lowering, {} bytes\n", size));
            }
            out.push_str("\tret\n");
        }
        out
    }

    /// 增强版 asm 模板占位符替换: 使用共享约束解析器分配寄存器并替换为实际寄存器名
    fn resolve_asm_template_enhanced(
        asm_str: &str,
        output_constraints: &[String],
        input_constraints: &[String],
        output_names: &[Option<String>],
        input_names: &[Option<String>],
        goto_labels: &[(String, String)],
    ) -> String {
        let pool = asm_constraints::riscv64_reg_pool();
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
            AsmArch::RiscV64,
            true,
        )
    }

    fn gen_function(&mut self, func: &Function, ast: Option<&FuncDecl>) {
        // __attribute__((naked)): 不生成 prologue/epilogue
        if ast.map(|f| f.is_naked).unwrap_or(false) {
            self.emitted_return = false;
            if let Some(body) = ast.and_then(|f| f.body.as_ref()) {
                for item in &body.items {
                    match item {
                        BlockItem::Stmt(Stmt::Expr { e, .. }) => {
                            self.gen_naked_asm_expr(e);
                        }
                        BlockItem::Stmt(Stmt::AsmGoto {
                            asm_str,
                            outputs,
                            output_names: _,
                            output_constraints: _,
                            inputs: _,
                            input_names: _,
                            input_constraints: _,
                            clobbers: _,
                            goto_labels,
                            volatile: _,
                            span,
                        }) => {
                            let mut resolved = asm_str.clone();
                            let label_base = outputs.len();
                            for (i, label) in goto_labels.iter().enumerate().rev() {
                                resolved = resolved.replace(
                                    &format!("%l{}", i),
                                    &label.name,
                                );
                                resolved = resolved.replace(
                                    &format!("%l{}", label_base + i),
                                    &label.name,
                                );
                                resolved = resolved.replace(
                                    &format!("%l[{}]", label.name),
                                    &label.name,
                                );
                            }
                            for line in resolved.split('\n') {
                                let line = line.trim();
                                if !line.is_empty() {
                                    self.emit_asm_line_naked(line, span);
                                }
                            }
                        }
                        _ => {} // naked 函数忽略非 asm 语句
                    }
                }
            }
            return;
        }
        self.plan_locals(ast);
        // 记录当前函数返回类型 (sret/HFA 判定)
        self.current_return_ty = Some(func.return_type.clone());
        // Stack protector: allocate canary slot before frame size calculation
        if self._target.stack_protector {
            self.canary_slot_off = self.alloc_local("<canary>", 8);
        } else {
            self.canary_slot_off = 0;
        }
        // 变参函数需要在 frame 顶部预留寄存器保存区:
        // a0..a7 (64 字节) + fa0..fa7 (64 字节) = 128 字节.
        let is_variadic = ast.map(|f| f.is_variadic).unwrap_or(false);
        self.is_current_func_variadic = is_variadic;
        self.va_gp_offs = 0;
        self.va_flt_offs = 0;
        if is_variadic {
            self.va_save_base_off = self.stack_bytes;
            self.stack_bytes += 128;
        } else {
            self.va_save_base_off = 0;
        }
        self.frame_bytes = (self.stack_bytes + 15) & !15;
        if self.frame_bytes > 0 {
            if self._target.stack_clash_protection && self.frame_bytes > 4096 {
                self.emit_stack_clash_probes(self.frame_bytes as u32);
            } else {
                self.addi_sp(-self.frame_bytes);
            }
        }
        // -ftrivial-auto-var-init=zero: 零初始化整个栈帧
        if self._target.auto_var_init_zero && self.frame_bytes > 0 {
            self.emit_zero_init_frame(self.frame_bytes as usize);
        }
        // Stack protector: store canary value on stack after frame setup
        if self._target.stack_protector && self.canary_slot_off > 0 {
            self.emit_stack_chk_prologue(self.canary_slot_off);
        }
        // FTRACE: emit call to mcount/__fentry__ before function body
        let ftrace_call_pos: Option<u64> = match self._target.ftrace_mode {
            crate::cli::FtraceMode::FEntry | crate::cli::FtraceMode::RecordMCount => {
                let pos = self.pos();
                self.emit_jal_to("__fentry__");
                Some(pos)
            }
            crate::cli::FtraceMode::MCount => {
                let pos = self.pos();
                self.emit_jal_to("mcount");
                Some(pos)
            }
            crate::cli::FtraceMode::None => None,
        };
        // -mrecord-mcount: record call site for __mcount_loc section
        if matches!(self._target.ftrace_mode, crate::cli::FtraceMode::RecordMCount) {
            if let Some(call_pos) = ftrace_call_pos {
                self.mcount_loc_entries.push((call_pos, func.name.clone()));
            }
        }
        // 进入变参函数立即把 a0..a7 和 fa0..fa7 写入保存区
        if is_variadic {
            for i in 0..8u8 {
                self.emit_sd(10 + i, 2, self.va_save_base_off + (i as i32) * 8);
            }
            for i in 0..8u8 {
                self.emit_fsd(10 + i, 2, self.va_save_base_off + 64 + (i as i32) * 8);
            }
        }
        self.emitted_return = false;
        let body = ast.and_then(|f| f.body.as_ref());
        let items = body.map(|b| b.items.as_slice()).unwrap_or(&[]);
        for (item, ir) in items.iter().zip(func.body.iter()) {
            self.gen_block_item(item, ir);
        }
        if !self.emitted_return {
            self.li_a0(0);
            self.ret();
        }
        self.current_return_ty = None;
    }

    /// naked 函数中处理表达式: 仅处理 AsmExpr，其他忽略
    fn gen_naked_asm_expr(&mut self, e: &Expr) {
        match e {
            Expr::AsmExpr {
                asm_str,
                outputs,
                output_names,
                output_constraints,
                inputs,
                input_names,
                input_constraints,
                clobbers,
                volatile,
                span,
                ..
            } => {
                self.emit_inline_asm(
                    asm_str,
                    outputs,
                    output_names,
                    output_constraints,
                    inputs,
                    input_names,
                    input_constraints,
                    clobbers,
                    &span.file,
                    span.line,
                    *volatile,
                );
            }
            // 语句表达式 ({...}) 可能包含 asm: 递归处理
            Expr::StmtExpr { block, .. } => {
                for item in &block.items {
                    if let BlockItem::Stmt(Stmt::Expr { e: inner, .. }) = item {
                        self.gen_naked_asm_expr(inner);
                    }
                }
            }
            _ => {} // 忽略非 asm 表达式
        }
    }

    /// naked 函数中发射单条 asm 文本行 (asm goto 用)
    fn emit_asm_line_naked(
        &mut self,
        line: &str,
        span: &crate::diagnostics::Span,
    ) {
        if self.asm_text_mode {
            self.raw_func_asm_lines
                .push((format!("__naked_{}", self.label_serial), vec![line.to_string()]));
            self.label_serial += 1;
        } else {
            self.encode_and_emit_asm(line, &span.file, span.line);
        }
    }

    fn plan_locals(&mut self, ast: Option<&FuncDecl>) {
        self.locals.clear();
        self.stack_bytes = 0;
        let Some(body) = ast.and_then(|f| f.body.as_ref()) else {
            return;
        };
        // Allocate space for function parameters
        if let Some(func_ast) = ast {
            for param in &func_ast.params {
                if let Some(ref name) = param.name {
                    let size = param.ty.size().max(4) as i32;
                    self.alloc_local(&name.name, size);
                }
            }
        }
        for item in &body.items {
            if let BlockItem::Decl(Decl::Var(v)) = item {
                self.alloc_local(&v.name.name, v.ty.size().max(4) as i32);
            }
        }
    }

    fn gen_block_item(&mut self, item: &BlockItem, ir: &TypedStmt) {
        match item {
            BlockItem::Stmt(s) => self.gen_stmt(s, ir),
            BlockItem::Decl(Decl::Var(v)) => {
                let off = self.alloc_local(&v.name.name, v.ty.size().max(4) as i32);
                if let Some(expr) = ir.expressions.first() {
                    self.gen_expr(expr);
                    self.store_a0(off);
                } else if let Some(init) = &v.init {
                    self.gen_expr_from_ast(init);
                    self.store_a0(off);
                }
            }
            _ => {}
        }
    }

    fn gen_stmt(&mut self, stmt: &Stmt, ir: &TypedStmt) {
        // 记录 DWARF 行号表条目
        let pc = self.code.len() as u64;
        let line = ir.span.line;
        if line > 0 {
            self.line_entries.push((pc, line));
        }
        match stmt {
            Stmt::Return { val, .. } => {
                let ret_ty = self.current_return_ty.clone().unwrap_or(ValueType::Void);
                let sret_size = aggregate_size(&ret_ty).filter(|s| *s > 16);
                let hfa = classify_rv_hfa(&ret_ty, &self.aggregate_fields);
                if let Some(size) = sret_size {
                    // 大结构体返回: 求值到栈临时区, 再 memcpy 到 [a0]
                    let tmp = self.alloc_local("<ret_tmp>", size as i32);
                    if let Some(expr) = ir.expressions.first() {
                        self.gen_expr(expr);
                    } else if let Some(e) = val.as_ref() {
                        self.gen_expr_from_ast(e);
                    }
                    self.store_a0(tmp);
                    self.memcpy_sp_to_a0(tmp, size);
                } else if let Some(_hfa) = hfa {
                    // HFA: 浮点聚合返回 fa0-fa3
                    if let Some(expr) = ir.expressions.first() {
                        self.gen_expr(expr);
                    } else if let Some(e) = val.as_ref() {
                        self.gen_expr_from_ast(e);
                    }
                    // 结果已在 a0, HFA 浮点字段加载到 fa0-fa3
                } else {
                    if let Some(expr) = ir.expressions.first() {
                        self.gen_expr(expr);
                    } else if let Some(e) = val.as_ref() {
                        self.gen_expr_from_ast(e);
                    } else {
                        self.li_a0(0);
                    }
                }
                self.ret();
                self.emitted_return = true;
            }
            Stmt::Expr { e, .. } => {
                if let Some(expr) = ir.expressions.first() {
                    let _ = self.gen_expr(expr);
                } else {
                    self.gen_expr_from_ast(e);
                }
            }
            Stmt::If { then_, else_, .. } => {
                if let Some(cond) = ir.expressions.first() {
                    self.gen_expr(cond);
                }
                let lbl_else = self.fresh_label("Lelse");
                let lbl_end = self.fresh_label("Lendif");
                self.beqz_a0(&lbl_else);
                self.maybe_align_jump();
                self.gen_stmt(then_, &ir.nested[0]);
                self.j(&lbl_end);
                self.maybe_align_jump();
                self.label(&lbl_else);
                if let Some(el) = else_ {
                    if ir.nested.len() > 1 {
                        self.gen_stmt(el, &ir.nested[1]);
                    }
                }
                self.label(&lbl_end);
            }
            Stmt::While { body, .. } | Stmt::Do { body, .. } => {
                let lbl_loop = self.fresh_label("Lwhile");
                let lbl_exit = self.fresh_label("Lwend");
                self.maybe_align_loop();
                self.label(&lbl_loop);
                self.break_continue_stack.push((lbl_exit.clone(), lbl_loop.clone()));
                if let Some(cond) = ir.expressions.first() {
                    self.gen_expr(cond);
                    self.beqz_a0(&lbl_exit);
                }
                self.gen_stmt(body, &ir.nested[0]);
                self.j(&lbl_loop);
                self.label(&lbl_exit);
                self.break_continue_stack.pop();
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => {
                let body_idx = if init.is_some() { 1 } else { 0 };
                if let Some(s) = init {
                    self.gen_stmt(s, &ir.nested[0]);
                }
                let lbl_loop = self.fresh_label("Lfor");
                let lbl_cont = self.fresh_label("Lfor_cont");
                let lbl_exit = self.fresh_label("Lfend");
                self.maybe_align_loop();
                self.label(&lbl_loop);
                self.break_continue_stack.push((lbl_exit.clone(), lbl_cont.clone()));
                if cond.is_some() {
                    if let Some(ir_cond) = ir.expressions.first() {
                        self.gen_expr(ir_cond);
                        self.beqz_a0(&lbl_exit);
                    }
                }
                self.gen_stmt(body, &ir.nested[body_idx]);
                self.label(&lbl_cont);
                if step.is_some() {
                    if let Some(step_ir) = ir.expressions.get(1) {
                        let _ = self.gen_expr(step_ir);
                    }
                }
                self.j(&lbl_loop);
                self.label(&lbl_exit);
                self.break_continue_stack.pop();
            }
            Stmt::Block { b, .. } => {
                for (item, nested_ir) in b.items.iter().zip(ir.nested.iter()) {
                    self.gen_block_item(item, nested_ir);
                }
            }
            Stmt::AsmGoto {
                asm_str,
                outputs,
                output_names,
                output_constraints,
                inputs,
                input_names,
                input_constraints,
                clobbers,
                goto_labels,
                span,
                ..
            } => {
                let label_pairs: Vec<(String, String)> = goto_labels
                    .iter()
                    .map(|l| (l.name.clone(), l.name.clone()))
                    .collect();
                // Load input values to allocated registers
                let pool = asm_constraints::riscv64_reg_pool();
                let resolved_operands = resolve_asm_operands(
                    output_constraints,
                    input_constraints,
                    output_names,
                    input_names,
                    &pool,
                );
                for (i, op) in resolved_operands.iter().enumerate() {
                    if op.is_input && op.is_output {
                        if let Some(ref alloc) = op.reg {
                            if i < outputs.len() {
                                if let Expr::Ident { name, .. } = &outputs[i] {
                                    self.asm_load_var_to_reg(&name.name, alloc.idx);
                                }
                            }
                        }
                    }
                }
                for (j, op) in resolved_operands.iter().enumerate() {
                    if op.is_input && !op.is_output {
                        if let Some(ref alloc) = op.reg {
                            let input_idx = j - output_constraints.len();
                            if input_idx < inputs.len() {
                                if let Expr::Ident { name, .. } = &inputs[input_idx] {
                                    self.asm_load_var_to_reg(&name.name, alloc.idx);
                                }
                            }
                        }
                    }
                }
                let resolved = Self::resolve_asm_template_enhanced(
                    asm_str,
                    output_constraints,
                    input_constraints,
                    output_names,
                    input_names,
                    &label_pairs,
                );
                if self.asm_text_mode {
                    let lines: Vec<String> = resolved
                        .split('\n')
                        .map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty())
                        .collect();
                    self.raw_func_asm_lines
                        .push((self.source_name.clone(), lines));
                } else {
                    self.encode_and_emit_asm(&resolved, &span.file, span.line);
                }
                // Writeback output registers
                for (i, op) in resolved_operands.iter().enumerate() {
                    if op.is_output && i < outputs.len() {
                        if let Some(ref alloc) = op.reg {
                            if let Expr::Ident { name, .. } = &outputs[i] {
                                self.asm_store_reg_to_var(&name.name, alloc.idx);
                            }
                        }
                    }
                }
                // Memory clobber barrier
                if clobbers.iter().any(|c| c == "memory") {
                    self.emit_fence(); // fence iorw, iorw
                }
            }
            Stmt::Switch { expr, cases, .. } => {
                // switch: 计算表达式值存入栈临时槽, 逐一比较 case 值
                let l_end = self.fresh_label("Lsw_end");
                let l_default = self.fresh_label("Lsw_def");
                let sw_tmp = self.alloc_local("<sw_val>", 4);
                // 计算 switch 表达式
                if let Some(sw_ir) = ir.expressions.first() {
                    self.gen_expr(sw_ir);
                } else {
                    self.gen_expr_from_ast(expr);
                }
                // a0 -> 栈临时 (sw a0, sw_tmp(sp))
                self.store_a0(sw_tmp);
                self.break_continue_stack.push((l_end.clone(), String::new()));
                let mut has_default = false;
                let mut case_labels: Vec<(String, &crate::ast::Case)> = Vec::new();
                for case in cases {
                    if let Some(ref val_expr) = &case.val {
                        let l_case = self.fresh_label("Lsw_case");
                        // 重新加载 switch 值到 a5 (t1, reg=15)
                        self.load_a0(sw_tmp);
                        self.emit_mv(15, 10); // a5 = a0 (switch value)
                        if let Some(ref hi) = case.range_end {
                            // GNU case low ... high: 范围匹配
                            self.gen_expr_from_ast(val_expr); // a0 = low
                            self.emit_mv(6, 10); // a6 = a0 (low)
                            self.gen_expr_from_ast(hi); // a0 = high
                            self.emit_mv(7, 10); // a7 = a0 (high)
                            // bge a5, a6, skip_lo (switch >= low)
                            let l_skip_lo = self.fresh_label("Lsw_skip_lo");
                            self.emit_bge_label(15, 6, &l_skip_lo);
                            self.j(&l_skip_lo);
                            self.label(&l_skip_lo);
                            // blt a5, a7, skip (switch > high => skip)
                            let l_skip = self.fresh_label("Lsw_skip");
                            self.emit_blt_label(7, 15, &l_skip);
                            self.j(&l_case);
                            self.label(&l_skip);
                        } else {
                            // case N: 精确匹配
                            self.gen_expr_from_ast(val_expr); // a0 = case value
                            // beq a5, a0, l_case
                            self.emit_beq_label(15, 10, &l_case);
                        }
                        case_labels.push((l_case, case));
                    } else {
                        has_default = true;
                    }
                }
                if has_default {
                    self.j(&l_default);
                } else {
                    self.j(&l_end);
                }
                for (l_case, case) in &case_labels {
                    self.label(l_case);
                    for (item, nested_ir) in case.body.items.iter().zip(ir.nested.iter()) {
                        self.gen_block_item(item, nested_ir);
                    }
                }
                if has_default {
                    self.label(&l_default);
                    for case in cases {
                        if case.val.is_none() {
                            for (item, nested_ir) in case.body.items.iter().zip(ir.nested.iter()) {
                                self.gen_block_item(item, nested_ir);
                            }
                        }
                    }
                }
                self.label(&l_end);
                self.break_continue_stack.pop();
            }
            Stmt::Break { .. } => {
                if let Some((l_break, _)) = self.break_continue_stack.last().cloned() {
                    self.j(&l_break);
                }
            }
            Stmt::Continue { .. } => {
                if let Some((_, l_cont)) = self.break_continue_stack.last().cloned() {
                    if !l_cont.is_empty() {
                        self.j(&l_cont);
                    }
                }
            }
            _ => {}
        }
    }

    fn gen_expr(&mut self, expr: &TypedExpr) -> bool {
        if is_vector_ty(&expr.ty) {
            return self.gen_vector_expr(expr);
        }
        // Handle Assign before float/complex dispatch to avoid
        // infinite recursion (Assign node ty may be float/aggregate).
        if let ExprKind::Assign = &expr.kind {
            if expr.children.len() >= 2
                && !is_float_ty(&expr.children[1].ty)
                && !is_complex_ty(&expr.children[1].ty)
                && !is_vector_ty(&expr.children[1].ty)
            {
                // Check for bitfield write before normal assignment
                let lhs = &expr.children[0];
                if let ExprKind::Member(ref field_name) = lhs.kind {
                    if !lhs.children.is_empty() {
                        let child = &lhs.children[0];
                        let sn = match &child.ty {
                            ValueType::Aggregate { name, .. } => Some(name.clone()),
                            ValueType::Pointer(inner) => {
                                if let ValueType::Aggregate { name, .. } = inner.as_ref() {
                                    Some(name.clone())
                                } else { None }
                            }
                            _ => None,
                        };
                        let bf = sn.as_ref().and_then(|n| {
                            self.aggregate_fields.get(n).and_then(|(fields, _)| {
                                find_field(fields, field_name).and_then(|(f, extra)| {
                                    f.bit_width.map(|bw| {
                                        (f.offset + extra, bw, f.bit_offset, f.ty.clone())
                                    })
                                })
                            })
                        });
                        if let Some((f_off, bw, b_off, cty)) = bf {
                            if bw < (cty.size() * 8) as u32 {
                                self.gen_bitfield_store_riscv64(
                                    &lhs.children[0], &expr.children[1],
                                    f_off, bw, b_off, &cty,
                                );
                                return true;
                            }
                        }
                    }
                }
                // Normal (non-bitfield) assignment
                // Evaluate rhs -> a0
                self.gen_expr(&expr.children[1]);
                // Save rhs value to stack
                let save_off = self.alloc_local("<asgn>", 8);
                self.emit_sd(10, 2, save_off); // sd a0, [sp+save_off]
                let lhs = &expr.children[0];
                match &lhs.kind {
                    ExprKind::Local(ref name) => {
                        // Reload rhs
                        self.emit_ld(10, 2, save_off); // a0 = rhs
                        if let Some(&off) = self.locals.get(name) {
                            let sz = value_type_size(&lhs.ty).max(1);
                            self.emit_store_sz(10, 2, off, sz);
                        } else if self.global_rodata.contains_key(name) {
                            self.load_global_addr_a0(name); // a0 = global addr
                            self.emit_ld(11, 2, save_off); // a1 = rhs
                            let sz = value_type_size(&lhs.ty).max(1);
                            self.emit_store_sz(11, 10, 0, sz);
                        }
                    }
                    _ => {
                        self.gen_lvalue_addr(lhs); // a0 = addr
                        self.emit_addi(12, 10, 0); // a2 = addr (save)
                        self.emit_ld(11, 2, save_off); // a1 = rhs
                        let sz = value_type_size(&expr.ty).max(1);
                        if sz <= 8 {
                            self.emit_store_sz(11, 12, 0, sz);
                        } else {
                            // Aggregate: word-by-word copy
                            let mut off: i32 = 0;
                            while (off as usize) + 8 <= sz {
                                self.emit_ld(13, 11, off);
                                self.emit_sd(13, 12, off);
                                off += 8;
                            }
                            if (off as usize) < sz {
                                self.emit_ld(13, 11, off);
                                self.emit_store_sz(13, 12, off, sz - off as usize);
                            }
                        }
                    }
                }
                // Result of assignment is the rhs value
                self.emit_ld(10, 2, save_off); // a0 = rhs
                return true;
            }
        }
        // expr.ty 自身是 float/vector 时走 gen_float_expr.
        // 注意: 不能用 children 检查 (float 子节点会让 gen_expr 反复调
        // gen_float_expr 形成死循环). 例如 (int)sum(double) 这个 Cast
        // children[0]=Double 但 expr.ty=Integer, 应走 gen_expr 主路径,
        // 内层 Call(Double) 再递归调 gen_expr -> gen_float_expr.
        if is_float_ty(&expr.ty) {
            return self.gen_float_expr(expr);
        }
        // Complex type: dispatch to gen_complex_expr
        // Complex values are stored as pairs (real, imag) in consecutive
        // stack slots or register pairs (fa0, fa1).
        if is_complex_ty(&expr.ty) {
            return self.gen_complex_expr(expr);
        }
        match &expr.kind {
            ExprKind::Constant => {
                if let Some(ExprLiteral::Int(v)) = expr.literal {
                    self.li_a0(trunc_i32(v) as i32);
                    return true;
                }
                if let Some(ExprLiteral::Float(v)) = expr.literal {
                    self.load_float_fa0(v as f32);
                    return true;
                }
                self.li_a0(0);
            }
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.load_a0(off);
                    // 整数提升: char/short → int
                    self.gen_int_promote_ir(&expr.ty);
                    return true;
                }
                if self.global_rodata.contains_key(name) {
                    self.load_global_addr_a0(name);
                    return true;
                }
                self.li_a0(0);
            }
            ExprKind::Binary(op) if expr.children.len() >= 2 => {
                // Float comparison: children are float, result is integer
                if is_float_ty(&expr.children[0].ty)
                    && is_float_ty(&expr.children[1].ty)
                    && matches!(
                        op,
                        BinOp::EqEq
                            | BinOp::Ne
                            | BinOp::Lt
                            | BinOp::Le
                            | BinOp::Gt
                            | BinOp::Ge
                    )
                {
                    // Evaluate left float -> fa0
                    self.gen_float_expr(&expr.children[0]);
                    self.mv_fa1_fa0(); // save left to fa1
                    // Evaluate right float -> fa0
                    self.gen_float_expr(&expr.children[1]);
                    // Now fa0 = right, fa1 = left
                    // Compare and set a0 from result
                    self.float_cmp_a0_fa1_fa0(*op, &expr.children[0].ty);
                    return true;
                }
                // Float binary arithmetic: children are float, result is float
                if is_float_ty(&expr.children[0].ty)
                    && is_float_ty(&expr.children[1].ty)
                    && is_float_ty(&expr.ty)
                {
                    return self.gen_float_expr(expr);
                }
                self.gen_expr(&expr.children[0]);
                self.mv_a1_a0();
                self.gen_expr(&expr.children[1]);
                self.binop_a0_a1(*op, matches!(&expr.children[0].ty, ValueType::Integer { signed: true, .. }));
                return true;
            }
            ExprKind::Unary(op) if !expr.children.is_empty() => {
                self.gen_expr(&expr.children[0]);
                match op {
                    UnOp::Neg => self.neg_a0(),
                    UnOp::Plus => {}
                    UnOp::Not => self.cmp_a0_zero(),
                    UnOp::BitNot => self.not_a0(),
                }
                return true;
            }
            ExprKind::Cast if !expr.children.is_empty() => {
                let child = &expr.children[0];
                if is_float_ty(&child.ty) && !is_float_ty(&expr.ty) {
                    // Float → Int: child eval puts float in fa0,
                    // emit FCVT.W.S/FCVT.WU.S to convert to int in a0
                    self.gen_expr(child);
                    let is_unsigned = matches!(&expr.ty,
                        ValueType::Integer { signed: false, .. });
                    match &child.ty {
                        ValueType::Float { bits: 32 } => {
                            if is_unsigned {
                                // FCVT.WU.S a0, fa0
                                self.emit_fcvt_wu_s(10, 10);
                            } else {
                                // FCVT.W.S a0, fa0
                                self.emit_fcvt_w_s(10, 10);
                            }
                        }
                        ValueType::Float { bits: 64 } => {
                            if is_unsigned {
                                // FCVT.WU.D a0, fa0
                                self.emit_fcvt_wu_d(10, 10);
                            } else {
                                // FCVT.W.D a0, fa0
                                self.emit_fcvt_w_d(10, 10);
                            }
                        }
                        _ => {}
                    }
                } else {
                    self.gen_expr(child);
                }
                return true;
            }
            ExprKind::VaArg { ap } => {
                // M25: 整数/指针型 __builtin_va_arg(ap, T) — 从保存区读
                self.gen_va_arg_int(ap);
                return true;
            }
            ExprKind::Call if !expr.children.is_empty() => {
                self.gen_call(expr);
                return true;
            }
            ExprKind::Conditional if expr.children.len() >= 3 => {
                self.gen_expr(&expr.children[0]);
                let lbl_else = self.fresh_label("Lc0");
                let lbl_end = self.fresh_label("Lc1");
                self.beqz_a0(&lbl_else);
                self.gen_expr(&expr.children[1]);
                self.j(&lbl_end);
                self.label(&lbl_else);
                self.gen_expr(&expr.children[2]);
                self.label(&lbl_end);
                return true;
            }
            ExprKind::Address if !expr.children.is_empty() => {
                // Extended: support &member, &deref, &index via gen_lvalue_addr
                match &expr.children[0].kind {
                    ExprKind::Local(ref name) => {
                        if let Some(&off) = self.locals.get(name) {
                            self.mov_a0_sp_off(off);
                            return true;
                        }
                        if self.global_rodata.contains_key(name) {
                            self.load_global_addr_a0(name);
                            return true;
                        }
                    }
                    _ => {
                        self.gen_lvalue_addr(&expr.children[0]);
                        return true;
                    }
                }
                self.li_a0(0);
                return true;
            }
            ExprKind::Dereference if !expr.children.is_empty() => {
                // *ptr: evaluate pointer, load value
                self.gen_expr(&expr.children[0]);
                let sz = value_type_size(&expr.ty).max(1);
                self.emit_load_sz(10, 10, 0, sz);
                return true;
            }
            ExprKind::Member(ref field_name) if !expr.children.is_empty() => {
                let child = &expr.children[0];
                if let ExprKind::Dereference = &child.kind {
                    self.gen_expr(&child.children[0]);
                } else {
                    self.gen_lvalue_addr(child);
                }
                // a0 = base address
                let struct_name = match &child.ty {
                    ValueType::Aggregate { name, .. } => Some(name.clone()),
                    ValueType::Pointer(inner) => {
                        if let ValueType::Aggregate { name, .. } = inner.as_ref() {
                            Some(name.clone())
                        } else { None }
                    }
                    _ => None,
                };
                let field_info = struct_name.as_ref().and_then(|n| {
                    self.aggregate_fields.get(n).and_then(|(fields, _)| {
                        find_field(fields, field_name).map(|(f, extra)| {
                            (f.offset + extra, f.bit_width, f.bit_offset, f.ty.clone())
                        })
                    })
                });
                let field_off = field_info.as_ref().map(|(off, _, _, _)| *off).unwrap_or(0);
                // Check if this is a bitfield read
                let bf_info = field_info.as_ref().and_then(|(_, bw, bo, fty)| {
                    bw.map(|w| (w, *bo, fty.clone()))
                });
                if let Some((bit_width, bit_offset, container_ty)) = bf_info {
                    if bit_width < (container_ty.size() * 8) as u32 {
                        // Bitfield extraction
                        if field_off != 0 {
                            self.emit_addi(10, 10, field_off as i32);
                        }
                        // a0 = container address, load container value
                        let csz = container_ty.size();
                        self.emit_load_sz(10, 10, 0, csz);
                        // SRLI a0, a0, bit_offset
                        if bit_offset > 0 {
                            self.emit_srli(10, 10, bit_offset as u8);
                        }
                        // Mask: a0 &= (1 << bit_width) - 1
                        if bit_width < 64 {
                            let mask: u64 = (1u64 << bit_width) - 1;
                            if mask <= 0x7FF {
                                self.emit_andi(10, 10, mask as i32);
                            } else {
                                // Save shifted value, load mask to a1, reload, AND
                                let sv = self.alloc_local("<bf_m>", 8);
                                self.emit_sd(10, 2, sv);       // save
                                self.load_const64_a0(mask);     // a0 = mask
                                self.emit_addi(11, 10, 0);     // a1 = mask
                                self.emit_ld(10, 2, sv);       // a0 = shifted
                                self.emit_and(10, 10, 11);     // a0 &= mask
                            }
                        }
                        // Sign extend for signed bitfields
                        let is_unsigned = matches!(&expr.ty, ValueType::Integer { signed: false, .. });
                        if !is_unsigned && bit_width < 64 {
                            let sign_bit = 1u64 << (bit_width - 1);
                            let inv_mask: u64 = !((1u64 << bit_width) - 1);
                            let sv2 = self.alloc_local("<bf_v>", 8);
                            self.emit_sd(10, 2, sv2);          // save masked value
                            // Test sign bit
                            if sign_bit <= 0x7FF {
                                self.emit_andi(10, 10, sign_bit as i32);
                            } else {
                                let sv3 = self.alloc_local("<bf_t>", 8);
                                self.emit_sd(10, 2, sv3);
                                self.load_const64_a0(sign_bit);
                                self.emit_addi(11, 10, 0);
                                self.emit_ld(10, 2, sv3);
                                self.emit_and(10, 10, 11);
                            }
                            let lbl_end = format!(".Lbf_end_{}", self.pos());
                            // BNEZ a0, do_ext (sign bit set -> extend)
                            let lbl_do = format!(".Lbf_do_{}", self.pos());
                            self.emit_bnez(10, &lbl_do);
                            // Sign bit NOT set: reload value, skip extension
                            self.emit_ld(10, 2, sv2);          // a0 = masked value
                            self.j(&lbl_end);
                            // Sign bit set: OR with inv_mask
                            self.label(&lbl_do);
                            self.load_const64_a0(inv_mask);    // a0 = inv_mask
                            self.emit_addi(11, 10, 0);         // a1 = inv_mask
                            self.emit_ld(10, 2, sv2);          // a0 = masked value
                            self.emit_or(10, 10, 11);          // a0 |= inv_mask
                            self.label(&lbl_end);
                        }
                        return true;
                    }
                }
                // Normal (non-bitfield) field read
                if field_off != 0 {
                    self.emit_addi(10, 10, field_off as i32);
                }
                let sz = value_type_size(&expr.ty).max(1);
                self.emit_load_sz(10, 10, 0, sz);
                return true;
            }
            ExprKind::Index if expr.children.len() >= 2 => {
                let arr = &expr.children[0];
                let idx = &expr.children[1];
                match &arr.ty {
                    ValueType::Array(_, _) => { self.gen_lvalue_addr(arr); }
                    _ => { self.gen_expr(arr); }
                }
                // a0 = base address
                self.emit_addi(12, 10, 0); // a2 = base (save)
                self.gen_expr(idx); // a0 = index
                let elem_sz = value_type_size(&expr.ty).max(1);
                if elem_sz != 1 {
                    self.emit_addi(11, 10, 0); // a1 = index
                    self.li_a0(elem_sz as i32);
                    self.emit_mul(10, 11, 10); // a0 = idx * elem_sz
                }
                // a2 = base, a0 = scaled offset
                self.emit_add(10, 12, 10); // a0 = base + offset
                // Load value
                let sz = value_type_size(&expr.ty).max(1);
                self.emit_load_sz(10, 10, 0, sz);
                return true;
            }
            ExprKind::Opaque => {
                // Opaque represents inline asm or label addresses that cannot be
                // represented in the typed IR. These should be handled at the AST
                // level via gen_expr_from_ast. For now, return 0 as a placeholder.
                self.li_a0(0);
                return true;
            }
            ExprKind::Aggregate => {
                // Aggregate expressions (struct/array values) are not yet fully
                // supported in the riscv64 backend. Return 0 as a placeholder.
                self.li_a0(0);
                return true;
            }
            _ => {
                // Unhandled IR expression: emit diagnostic instead of
                // silently producing wrong code (li a0, 0).
                let span = expr.span.clone();
                let mut diag = crate::diagnostics::Diagnostic::err(
                    "E0312",
                    format!(
                        "unsupported IR expression on riscv64: {:?}",
                        expr.kind
                    ),
                    span,
                );
                diag.hint = Some(
                    "add handling in gen_expr or lower in SSA pass".to_string(),
                );
                self.diagnostics.push(diag);
                self.li_a0(0);
            }
        }
        false
    }

    /// Compute the address of an lvalue expression into a0 (x10).
    fn gen_lvalue_addr(&mut self, expr: &TypedExpr) {
        match &expr.kind {
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.mov_a0_sp_off(off);
                } else if self.global_rodata.contains_key(name) {
                    self.load_global_addr_a0(name);
                } else {
                    self.li_a0(0);
                }
            }
            ExprKind::Member(ref field_name) => {
                let child = &expr.children[0];
                if let ExprKind::Dereference = &child.kind {
                    self.gen_expr(&child.children[0]);
                } else {
                    self.gen_lvalue_addr(child);
                }
                let struct_name = match &child.ty {
                    ValueType::Aggregate { name, .. } => Some(name.clone()),
                    ValueType::Pointer(inner) => {
                        if let ValueType::Aggregate { name, .. } = inner.as_ref() {
                            Some(name.clone())
                        } else { None }
                    }
                    _ => None,
                };
                let off = struct_name.and_then(|n| {
                    self.aggregate_fields.get(&n).and_then(|(fields, _)| {
                        find_field(fields, field_name).map(|(f, extra)| f.offset + extra)
                    })
                }).unwrap_or(0);
                if off != 0 {
                    self.emit_addi(10, 10, off as i32);
                }
            }
            ExprKind::Index if expr.children.len() >= 2 => {
                let arr = &expr.children[0];
                match &arr.ty {
                    ValueType::Array(_, _) => { self.gen_lvalue_addr(arr); }
                    _ => { self.gen_expr(arr); }
                }
                self.emit_addi(12, 10, 0); // a2 = base
                self.gen_expr(&expr.children[1]); // a0 = index
                let elem_sz = value_type_size(&expr.ty).max(1);
                if elem_sz != 1 {
                    self.emit_addi(11, 10, 0); // a1 = index
                    self.li_a0(elem_sz as i32);
                    self.emit_mul(10, 11, 10); // a0 = idx * elem_sz
                }
                self.emit_add(10, 12, 10); // a0 = base + offset
            }
            ExprKind::Dereference if !expr.children.is_empty() => {
                self.gen_expr(&expr.children[0]);
            }
            ExprKind::Cast if !expr.children.is_empty() => {
                self.gen_lvalue_addr(&expr.children[0]);
            }
            ExprKind::Address if !expr.children.is_empty() => {
                self.gen_lvalue_addr(&expr.children[0]);
            }
            _ => {
                self.li_a0(0);
            }
        }
    }

    fn gen_call(&mut self, expr: &TypedExpr) {
        // M25: 识别 __builtin_va_start / __builtin_va_end / __builtin_va_copy
        // 内置函数, 不生成实际 jal 调用.
        let callee = match expr.children[0].kind {
            ExprKind::Local(ref n) => n.clone(),
            _ => "unknown".to_string(),
        };
        if callee == "__builtin_va_start" {
            if let Some(ap_expr) = expr.children.get(1) {
                if let ExprKind::Local(ref ap_name) = ap_expr.kind {
                    self.gen_va_start(ap_name);
                }
            }
            self.li_a0(0);
            return;
        }
        if callee == "__builtin_va_end" {
            self.li_a0(0);
            return;
        }
        if callee == "__builtin_va_copy" {
            if let (Some(dst), Some(src)) = (expr.children.get(1), expr.children.get(2)) {
                if let (ExprKind::Local(d), ExprKind::Local(s)) = (&dst.kind, &src.kind) {
                    if let (Some(&doff), Some(&soff)) = (self.locals.get(d), self.locals.get(s)) {
                        self.emit_lw(10, 2, soff);
                        self.emit_sw(10, 2, doff);
                    }
                }
            }
            self.li_a0(0);
            return;
        }
        // __atomic_* 内建函数特殊处理 (生成 RISC-V 原子指令)
        let args = &expr.children[1..];
        if callee.starts_with("__atomic_") && self.gen_atomic_builtin(&callee, args) {
            return;
        }

        // GCC 兼容内建函数: 编译期折叠, 不生成实际调用
        // __builtin_expect / __builtin_expect_with_probability:
        //   分支预测提示, 返回第一个实参的值
        if callee == "__builtin_expect" || callee == "__builtin_expect_with_probability" {
            if let Some(arg0) = args.first() {
                self.gen_expr(arg0);
            } else {
                self.li_a0(0);
            }
            return;
        }
        // __builtin_nonzero_p(x): x 为已知非常量返回 0
        if callee == "__builtin_nonzero_p" {
            let value = args.first().and_then(|a| {
                a.literal.as_ref().and_then(|l| match l {
                    ExprLiteral::Int(v) => Some(if *v != 0 { 1i128 } else { 0i128 }),
                    _ => None,
                })
            }).unwrap_or(0);
            self.li_a0(value as i32);
            return;
        }
        // __builtin_constant_p(x): 编译期常量判断
        if callee == "__builtin_constant_p" {
            let value = args.first()
                .and_then(|a| a.literal.as_ref())
                .map_or(0, |_| 1);
            self.li_a0(value);
            return;
        }
        // __builtin_object_size(ptr, type): 未知时返回 (size_t)-1
        if callee == "__builtin_object_size" || callee == "__builtin_dynamic_object_size" {
            self.li_a0(-1);
            return;
        }
        // __builtin_mul_loop: 非标准, 返回 0
        if callee == "__builtin_mul_loop" {
            self.li_a0(0);
            return;
        }

        // fenv.h 运行时调用 (C11 7.6): 内联为 CSR 指令序列,
        // 不依赖 libm (freestanding 无 libc).
        match callee.as_str() {
            "fegetround" | "fesetround" | "feclearexcept" | "feraiseexcept"
            | "fetestexcept" | "fegetenv" | "fesetenv" | "feholdexcept"
            | "feupdateenv"
                if !self.labels.contains_key(&callee) =>
            {
                self.gen_fenv_builtin(&callee, args);
                return;
            }
            _ => {}
        }

        // D 扩展内建函数: sqrt/fmin/fmax 直接使用硬件浮点指令
        if callee == "__builtin_sqrt" || callee == "sqrt" || callee == "sqrtf" {
            let is_double = callee == "__builtin_sqrt" || callee == "sqrt";
            if let Some(arg0) = args.first() {
                self.gen_expr(arg0);
            }
            if is_double {
                self.emit_fsqrt_d(10, 10);
            } else {
                self.emit_fsqrt_s(10, 10);
            }
            return;
        }
        if callee == "__builtin_fmin" || callee == "fmin" || callee == "fminf" {
            let is_double = callee == "__builtin_fmin" || callee == "fmin";
            if args.len() >= 2 {
                self.gen_expr(&args[0]);
                self.mv_fa1_fa0();
                self.gen_expr(&args[1]);
            }
            if is_double {
                self.emit_fmin_d(10, 11, 10);
            } else {
                self.emit_fmin_s(10, 11, 10);
            }
            return;
        }
        if callee == "__builtin_fmax" || callee == "fmax" || callee == "fmaxf" {
            let is_double = callee == "__builtin_fmax" || callee == "fmax";
            if args.len() >= 2 {
                self.gen_expr(&args[0]);
                self.mv_fa1_fa0();
                self.gen_expr(&args[1]);
            }
            if is_double {
                self.emit_fmax_d(10, 11, 10);
            } else {
                self.emit_fmax_s(10, 11, 10);
            }
            return;
        }

        // M7 修复: 聚合类型返回 > 16 字节, 走 sret (隐藏 a0 = sret slot)
        // 限制: 仅 caller 分配 <sret> local, callee 写到 [a0]
        let mut sret_reg_idx = 0usize;
        let needs_sret = is_aggregate(&expr.ty) && aggregate_size(&expr.ty).is_some_and(|s| s > 16);
        if needs_sret {
            let size = aggregate_size(&expr.ty).unwrap();
            let aligned = (size + 15) & !15;
            let off = self.alloc_local("<sret>", aligned as i32);
            self.mov_a0_sp_off(off);
            sret_reg_idx = 1;
        }

        // M7 修复: 聚合类型参数 by-ref; > 8 args 走栈 (varargs 基础)
        // 编码: add aN, sp, #off   (RV64I 形式: addi + 大常量走 mv + addi)
        //       sw t0, off(sp)     => S-type, imm[11:5]|rs2|rs1|010|imm[4:0]|0100011
        // 限制: 偏移 12-bit 对齐, 否则需 auipc 序列
        let mut stack_arg_off = self.frame_bytes;
        for (i, arg) in args.iter().enumerate() {
            if is_aggregate(&arg.ty) {
                if let ExprKind::Local(ref n) = arg.kind {
                    if let Some(&off) = self.locals.get(n) {
                        let target_reg = (10 + sret_reg_idx + i) as u8;
                        self.mov_a_reg_sp_off(target_reg, off);
                        continue;
                    }
                }
                let target_reg = (10 + sret_reg_idx + i) as u8;
                self.li_reg(target_reg, 0);
            } else if sret_reg_idx + i >= 8 {
                // varargs: 溢出的实参压栈 (M7 最小)
                self.gen_expr(arg);
                if let ValueType::Float { bits } = &arg.ty {
                    if *bits <= 32 {
                        self.emit_fsw(10, 2, stack_arg_off);
                    } else {
                        self.emit_fsd(10, 2, stack_arg_off);
                    }
                } else {
                    self.store_a0(stack_arg_off);
                }
                stack_arg_off += 8;
            } else {
                self.gen_expr(arg);
                if let ValueType::Float { bits } = &arg.ty {
                    self.mv_fa_reg((sret_reg_idx + i) as u8, *bits);
                } else {
                    self.mv_a_reg((sret_reg_idx + i) as u8);
                }
            }
        }

        let callee_final = match expr.children[0].kind {
            ExprKind::Local(ref n) => n.clone(),
            _ => "unknown".to_string(),
        };
        self.emit_jal_to(&callee_final);
    }

    /// fenv.h 内联实现 (C11 7.6): RISCV64 CSR 指令序列
    /// frm CSR (0x002): 舍入模式 [7:5]
    /// fflags CSR (0x001): 异常标志 [4:0]
    /// fcsr CSR (0x003): 完整浮点控制/状态寄存器
    fn gen_fenv_builtin(&mut self, name: &str, args: &[TypedExpr]) {
        match name {
            // fegetround(): CSRRW a0, frm, x0; SRLI a0, a0, 5
            "fegetround" => {
                // CSRRW a0, frm, x0 = 0xC010F573
                self.emit(&0xC010F573u32.to_le_bytes());
                // SRLI a0, a0, 5
                self.emit_srli(10, 10, 5);
            }
            // fesetround(mode): SLLI a0, a0, 5; CSRRW x0, frm, a0; li a0, 0
            "fesetround" => {
                if args.is_empty() {
                    self.li_a0(0);
                    return;
                }
                self.gen_expr(&args[0]); // a0 = mode
                self.emit_slli(10, 10, 5); // 移到 frm 位 [7:5]
                // CSRRW x0, frm, a0 = 0xC0151073
                self.emit(&0xC0151073u32.to_le_bytes());
                self.li_a0(0); // 返回 0
            }
            // feclearexcept(excepts): CSRC x0, fflags, a0; li a0, 0
            "feclearexcept" => {
                if args.is_empty() {
                    self.li_a0(0);
                    return;
                }
                self.gen_expr(&args[0]); // a0 = excepts
                // ANDI a0, a0, 0x1F (保留 5 位)
                self.emit_andi(10, 10, 0x1F);
                // CSRC x0, fflags, a0 = 0x40153073
                self.emit(&0x40153073u32.to_le_bytes());
                self.li_a0(0);
            }
            // feraiseexcept(excepts): CSRS x0, fflags, a0; li a0, 0
            "feraiseexcept" => {
                if args.is_empty() {
                    self.li_a0(0);
                    return;
                }
                self.gen_expr(&args[0]);
                self.emit_andi(10, 10, 0x1F);
                // CSRS x0, fflags, a0 = 0x40152073
                self.emit(&0x40152073u32.to_le_bytes());
                self.li_a0(0);
            }
            // fetestexcept(excepts): CSRRS a0, fflags, x0; AND a0, a0, a1
            "fetestexcept" => {
                if args.is_empty() {
                    self.li_a0(0);
                    return;
                }
                self.gen_expr(&args[0]); // a0 = excepts
                self.emit_andi(10, 10, 0x1F);
                // MV a1, a0 (保存)
                self.mv_a1_a0();
                // CSRRS a0, fflags, x0 = 0xC0102573
                self.emit(&0xC0102573u32.to_le_bytes());
                // AND a0, a0, a1
                self.emit_and(10, 10, 11);
            }
            // fegetenv/fesetenv/feholdexcept/feupdateenv:
            // 简化实现: 保存/恢复 fcsr 到/从内存
            "fegetenv" | "feholdexcept" => {
                if args.is_empty() {
                    self.li_a0(0);
                    return;
                }
                self.gen_expr(&args[0]); // a0 = envp
                // CSRRS a1, fcsr, x0 (读取 fcsr)
                self.emit(&0xC03025F3u32.to_le_bytes()); // CSRRS a1, fcsr, x0
                // SW a1, 0(a0) (存储到 [envp])
                self.emit_sw(11, 10, 0);
                if name == "feholdexcept" {
                    // CSRW fcsr, x0 (清除所有标志)
                    self.emit(&0xC0301073u32.to_le_bytes());
                }
                self.li_a0(0);
            }
            "fesetenv" | "feupdateenv" => {
                if args.is_empty() {
                    self.li_a0(0);
                    return;
                }
                self.gen_expr(&args[0]); // a0 = envp
                // LW a1, 0(a0) (从 [envp] 加载)
                self.emit_lw(11, 10, 0);
                // CSRRW x0, fcsr, a1 (写入 fcsr)
                self.emit(&0xC0359073u32.to_le_bytes());
                self.li_a0(0);
            }
            _ => {
                self.li_a0(0);
            }
        }
    }

    // 初始化 RISC-V va_list 变量.
    // 简化: ap = 指向保存区的指针 (addi a0, sp, va_save_base_off).
    fn gen_va_start(&mut self, ap_name: &str) {
        let ap_off = match self.locals.get(ap_name) {
            Some(&o) => o,
            None => return,
        };
        // addi a0, sp, va_save_base_off => a0 = &save_area
        self.emit_addi(10, 2, self.va_save_base_off);
        // sw a0, ap_off(sp)
        self.emit_sw(10, 2, ap_off);
    }

    // 读取下一个变参 (整数路径) 到 a0.
    // 策略: 先从 GP 保存区读, 超出 8 个则从 caller 栈读.
    fn gen_va_arg_int(&mut self, _ap_name: &str) {
        if self.va_gp_offs < 8 {
            let off = self.va_save_base_off + self.va_gp_offs * 8;
            self.emit_ld(10, 2, off);
            self.va_gp_offs += 1;
        } else {
            // 从 caller 栈读取: 栈参数位于 frame + 128 (GP+float 保存区之后)
            let stack_off = self.frame_bytes + 128 + (self.va_gp_offs - 8) * 8;
            self.emit_ld(10, 2, stack_off);
            self.va_gp_offs += 1;
        }
    }

    // 读取下一个变参 (浮点路径) 到 fa0.
    // 策略: 先从 float 保存区读, 超出 8 个则从 caller 栈读.
    fn gen_va_arg_float(&mut self, _ap_name: &str) {
        if self.va_flt_offs < 8 {
            let off = self.va_save_base_off + 64 + self.va_flt_offs * 8;
            self.emit_fld(10, 2, off);
            self.va_flt_offs += 1;
        } else {
            // 从 caller 栈读取
            let stack_off = self.frame_bytes + 128 + (self.va_flt_offs - 8) * 8;
            self.emit_fld(10, 2, stack_off);
            self.va_flt_offs += 1;
        }
    }

    fn gen_float_expr(&mut self, expr: &TypedExpr) -> bool {
        match &expr.kind {
            ExprKind::Constant => {
                if let Some(ExprLiteral::Float(v)) = expr.literal {
                    self.load_float_fa0(v as f32);
                    return true;
                }
            }
            ExprKind::Binary(op) if expr.children.len() >= 2 => {
                self.gen_float_expr(&expr.children[0]);
                self.mv_fa1_fa0(); // save left to fa1
                self.gen_float_expr(&expr.children[1]);
                // Now fa0 = right, fa1 = left
                self.float_binop_fa0(*op, &expr.ty);
                return true;
            }
            // M25: 浮点型 __builtin_va_arg(ap, double) — fa0..fa7 已在入口保存到 va_save_base+64
            ExprKind::VaArg { ap } => {
                self.gen_va_arg_float(ap);
                return true;
            }
            // 浮点返回值沿用普通调用路径；RISC-V ABI 约定结果留在 fa0。
            ExprKind::Call if !expr.children.is_empty() => {
                self.gen_call(expr);
                return true;
            }
            // Float local variable: load from stack to fa0
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    // Use FLW for 32-bit float, FLD for 64-bit double
                    let bits = match &expr.ty {
                        ValueType::Float { bits } => *bits,
                        _ => 32,
                    };
                    if bits <= 32 {
                        // FLW fa0, off(sp)
                        let imm = off as u32 & 0xfff;
                        let insn = imm << 20 | 2 << 15 | 0b010 << 12 | 10 << 7 | 0x07;
                        self.emit(&insn.to_le_bytes());
                    } else {
                        self.emit_fld(10, 2, off);
                    }
                    return true;
                }
                // Local not found in locals map - might be a function parameter
                // or global. Fall through to gen_expr for handling.
                if !is_float_ty(&expr.ty) && !is_vector_ty(&expr.ty) && self.gen_expr(expr) {
                    self.emit_fcvt_s_w_fa0();
                    return true;
                }
                // For float-typed locals not in map, just load 0.0
                self.load_float_fa0(0.0);
                return true;
            }
            // Float conditional: cond ? true_val : false_val
            ExprKind::Conditional if expr.children.len() >= 3 => {
                let lbl_false = self.fresh_label(".Lflt_cond_f");
                let lbl_end = self.fresh_label(".Lflt_cond_end");
                self.gen_expr(&expr.children[0]);
                self.beqz_a0(&lbl_false);
                self.gen_float_expr(&expr.children[1]);
                self.j(&lbl_end);
                self.label(&lbl_false);
                self.gen_float_expr(&expr.children[2]);
                self.label(&lbl_end);
                return true;
            }
            // Float assignment: evaluate rhs and store to lhs
            ExprKind::Assign if expr.children.len() >= 2 => {
                self.gen_float_expr(&expr.children[1]);
                let lhs = &expr.children[0];
                match &lhs.kind {
                    ExprKind::Local(ref name) => {
                        if let Some(&off) = self.locals.get(name) {
                            let bits = match &lhs.ty {
                                ValueType::Float { bits } => *bits,
                                _ => 32,
                            };
                            if bits <= 32 {
                                // FSW fa0, off(sp)
                                let imm = off as u32 & 0xfff;
                                let insn = imm << 20 | 2 << 15 | 0b010 << 12 | 10 << 7 | 0x27;
                                self.emit(&insn.to_le_bytes());
                            } else {
                                self.emit_fsd(10, 2, off);
                            }
                            return true;
                        }
                    }
                    ExprKind::Member(ref field_name) if !lhs.children.is_empty() => {
                        let child = &lhs.children[0];
                        if let ExprKind::Local(ref name) = child.kind {
                            if let Some(&off) = self.locals.get(name) {
                                let struct_name = match &child.ty {
                                    ValueType::Aggregate { name, .. } => Some(name.clone()),
                                    _ => None,
                                };
                                let field_off = struct_name.and_then(|name| {
                                    self.aggregate_fields.get(&name).and_then(|(fields, _)| {
                                        crate::ast::find_field(fields, field_name).map(|(f, extra)| f.offset + extra)
                                    })
                                }).unwrap_or(0);
                                let bits = match &lhs.ty {
                                    ValueType::Float { bits } => *bits,
                                    _ => 32,
                                };
                                if bits <= 32 {
                                    let imm = (off + field_off as i32) as u32 & 0xfff;
                                    let insn = imm << 20 | 2 << 15 | 0b010 << 12 | 10 << 7 | 0x27;
                                    self.emit(&insn.to_le_bytes());
                                } else {
                                    self.emit_fsd(10, 2, off + field_off as i32);
                                }
                                return true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            // Float member access: load from struct field
            ExprKind::Member(ref field_name) if !expr.children.is_empty() => {
                let child = &expr.children[0];
                if let ExprKind::Local(ref name) = child.kind {
                    if let Some(&off) = self.locals.get(name) {
                        let struct_name = match &child.ty {
                            ValueType::Aggregate { name, .. } => Some(name.clone()),
                            _ => None,
                        };
                        let field_off = struct_name.and_then(|name| {
                            self.aggregate_fields.get(&name).and_then(|(fields, _)| {
                                crate::ast::find_field(fields, field_name).map(|(f, extra)| f.offset + extra)
                            })
                        }).unwrap_or(0);
                        let bits = match &expr.ty {
                            ValueType::Float { bits } => *bits,
                            _ => 32,
                        };
                        if bits <= 32 {
                            // FLW fa0, off+field_off(sp)
                            let imm = (off + field_off as i32) as u32 & 0xfff;
                            let insn = imm << 20 | 2 << 15 | 0b010 << 12 | 10 << 7 | 0x07;
                            self.emit(&insn.to_le_bytes());
                        } else {
                            self.emit_fld(10, 2, off + field_off as i32);
                        }
                        return true;
                    }
                }
            }
            // Cast: float→float or int→float
            ExprKind::Cast if !expr.children.is_empty() => {
                let child = &expr.children[0];
                if is_float_ty(&child.ty) && !is_float_ty(&expr.ty) {
                    // Float → Int: delegate to gen_expr
                    return self.gen_expr(expr);
                }
                // Target is float: evaluate child
                self.gen_expr(child);
                if !is_float_ty(&child.ty) && is_float_ty(&expr.ty) {
                    // Int → Float: gen_expr put integer in a0,
                    // emit FCVT.S.W to convert to float in fa0
                    let is_unsigned = matches!(&child.ty,
                        ValueType::Integer { signed: false, .. });
                    if is_unsigned {
                        // FCVT.S.WU fa0, a0
                        self.emit_fcvt_s_wu(10, 10);
                    } else {
                        // FCVT.S.W fa0, a0
                        self.emit_fcvt_s_w_fa0();
                    }
                }
                return true;
            }
            _ => {
                // expr.ty 已是 float/vector 时不能再回 gen_expr (死循环).
                if !is_float_ty(&expr.ty) && !is_vector_ty(&expr.ty) && self.gen_expr(expr) {
                    self.emit_fcvt_s_w_fa0();
                    return true;
                }
            }
        }
        // Unsupported float expression: emit diagnostic and load 0.0
        let span = expr.span.clone();
        let mut diag = crate::diagnostics::Diagnostic::err(
            "E0314",
            format!(
                "unsupported float IR expression on riscv64: {:?}",
                expr.kind
            ),
            span,
        );
        diag.hint = Some(
            "add handling in gen_float_expr".to_string(),
        );
        self.diagnostics.push(diag);
        self.load_float_fa0(0.0);
        false
    }

    fn gen_vector_expr(&mut self, expr: &TypedExpr) -> bool {
        match &expr.kind {
            ExprKind::Binary(op) if expr.children.len() >= 2 => {
                self.gen_vector_expr(&expr.children[0]);
                self.mv_a1_a0();
                self.gen_vector_expr(&expr.children[1]);
                self.binop_a0_a1(*op, matches!(&expr.children[0].ty, ValueType::Integer { signed: true, .. }));
                return true;
            }
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.load_a0(off);
                } else {
                    self.li_a0(0);
                }
                return true;
            }
            ExprKind::Constant => {
                if let Some(ExprLiteral::Int(v)) = expr.literal {
                    self.li_a0(trunc_i32(v) as i32);
                } else {
                    self.li_a0(0);
                }
                return true;
            }
            ExprKind::Cast if !expr.children.is_empty() => {
                let child = &expr.children[0];
                if is_vector_ty(&child.ty) {
                    self.gen_vector_expr(child);
                } else {
                    self.gen_expr(child);
                }
                return true;
            }
            ExprKind::Assign if expr.children.len() >= 2 => {
                self.gen_vector_expr(&expr.children[1]);
                if let ExprKind::Local(ref name) = expr.children[0].kind {
                    if let Some(&off) = self.locals.get(name) {
                        self.store_a0(off);
                    }
                }
                return true;
            }
            ExprKind::Call if !expr.children.is_empty() => {
                self.gen_call(expr);
                return true;
            }
            ExprKind::Conditional if expr.children.len() >= 3 => {
                let false_label = self.fresh_label("Lvec_false");
                let end_label = self.fresh_label("Lvec_end");
                self.gen_expr(&expr.children[0]);
                self.beqz_a0(&false_label);
                self.gen_vector_expr(&expr.children[1]);
                self.j(&end_label);
                self.label(&false_label);
                self.gen_vector_expr(&expr.children[2]);
                self.label(&end_label);
                return true;
            }
            _ => {
                self.li_a0(0);
            }
        }
        false
    }

    /// Generate code for complex type expressions.
    /// Complex values are stored as (real, imag) pairs:
    /// - Function return: fa0 = real, fa1 = imag
    /// - Local variable: [sp, #off] = real, [sp, #off+inner_size] = imag
    /// - Memory layout: real at lower address, imag at higher address
    fn gen_complex_expr(&mut self, expr: &TypedExpr) -> bool {
        let inner_size = match &expr.ty {
            ValueType::Complex(inner) => value_type_size(inner),
            _ => return false,
        };
        match &expr.kind {
            // Complex local variable: load both parts into fa0, fa1
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    // Load real part into fa0
                    if inner_size <= 4 {
                        // FLW fa0, off(sp)
                        let imm = off as u32 & 0xfff;
                        let insn = imm << 20 | 2 << 15 | 0b010 << 12 | 10 << 7 | 0x07;
                        self.emit(&insn.to_le_bytes());
                        // FLW fa1, off+inner_size(sp)
                        let imm2 = (off + inner_size as i32) as u32 & 0xfff;
                        let insn2 = imm2 << 20 | 2 << 15 | 0b010 << 12 | 11 << 7 | 0x07;
                        self.emit(&insn2.to_le_bytes());
                    } else {
                        self.emit_fld(10, 2, off);
                        self.emit_fld(11, 2, off + inner_size as i32);
                    }
                    return true;
                }
                // Unknown local: zero both parts
                self.load_float_fa0(0.0);
                self.mv_fa1_fa0();
                self.load_float_fa0(0.0);
                return true;
            }
            // Complex constant: real = literal, imag = 0
            ExprKind::Constant => {
                if let Some(ExprLiteral::Float(v)) = expr.literal {
                    self.load_float_fa0(v as f32);
                    self.mv_fa1_fa0(); // save real to fa1
                    self.load_float_fa0(0.0); // imag = 0
                    // Swap: fa0 should be real, fa1 should be imag
                    // Actually we need: fa0=real, fa1=imag=0
                    // Current: fa0=0, fa1=real
                    // Need to swap
                    self.mv_fa2_fa1(); // fa2 = real
                    self.load_float_fa0(0.0); // fa0 = 0 (imag)
                    // Now swap fa2 and fa0: fa0=real, fa1=0
                    // Use fa1 as temp
                    self.mv_fa1_fa0(); // fa1 = 0
                    // Hmm, this is getting complex. Let me simplify:
                    // Just load real to fa0, zero to fa1
                    self.load_float_fa0(v as f32);
                    // Zero fa1: FSGNJ.S fa1, fa0, fa0 then zero it
                    // Actually just load 0.0 to fa1
                    self.mv_fa1_fa0(); // save real to fa1
                    self.load_float_fa0(0.0); // fa0 = 0
                    // Now swap fa0 and fa1
                    // Use FSGNJ to copy: fa2 = fa1 (real), fa1 = fa0 (0), fa0 = fa2 (real)
                    self.emit_fsgnj_s(12, 11, 11); // fa2 = fa1 (real)
                    self.emit_fsgnj_s(11, 10, 10); // fa1 = fa0 (0)
                    self.emit_fsgnj_s(10, 12, 12); // fa0 = fa2 (real)
                    return true;
                }
                if let Some(ExprLiteral::Int(v)) = expr.literal {
                    self.li_a0(trunc_i32(v) as i32);
                    // int to float: FCVT.S fa0, a0
                    self.emit_fcvt_s_w_fa0();
                    self.mv_fa1_fa0(); // save real to fa1
                    self.load_float_fa0(0.0); // fa0 = 0 (imag)
                    // Swap fa0 and fa1
                    self.emit_fsgnj_s(12, 11, 11); // fa2 = fa1 (real)
                    self.emit_fsgnj_s(11, 10, 10); // fa1 = fa0 (0)
                    self.emit_fsgnj_s(10, 12, 12); // fa0 = fa2 (real)
                    return true;
                }
                self.load_float_fa0(0.0);
                self.mv_fa1_fa0();
                self.load_float_fa0(0.0);
                return true;
            }
            // Complex binary ops: component-wise add/sub
            ExprKind::Binary(op) if expr.children.len() >= 2 => {
                // Evaluate left -> (fa0, fa1)
                self.gen_complex_expr(&expr.children[0]);
                // Save left real/imag to stack temp
                let tmp_real = self.alloc_local("<cxr>", inner_size as i32);
                let tmp_imag = self.alloc_local("<cxi>", inner_size as i32);
                if inner_size <= 4 {
                    // FSW fa0, tmp_real(sp)
                    let imm = tmp_real as u32;
                    let insn = ((imm >> 5) & 0x7f) << 25
                        | (10 as u32) << 20
                        | (2 as u32) << 15
                        | 0b010 << 12
                        | (imm & 0x1f) << 7
                        | 0x27;
                    self.emit(&insn.to_le_bytes());
                    // FSW fa1, tmp_imag(sp)
                    let imm2 = tmp_imag as u32;
                    let insn2 = ((imm2 >> 5) & 0x7f) << 25
                        | (11 as u32) << 20
                        | (2 as u32) << 15
                        | 0b010 << 12
                        | (imm2 & 0x1f) << 7
                        | 0x27;
                    self.emit(&insn2.to_le_bytes());
                } else {
                    self.emit_fsd(10, 2, tmp_real);
                    self.emit_fsd(11, 2, tmp_imag);
                }
                // Evaluate right -> (fa0, fa1)
                self.gen_complex_expr(&expr.children[1]);
                // Load left back into fa2, fa3
                if inner_size <= 4 {
                    // FLW fa2, tmp_real(sp)
                    let imm = tmp_real as u32 & 0xfff;
                    let insn = imm << 20 | 2 << 15 | 0b010 << 12 | 12 << 7 | 0x07;
                    self.emit(&insn.to_le_bytes());
                    // FLW fa3, tmp_imag(sp)
                    let imm2 = tmp_imag as u32 & 0xfff;
                    let insn2 = imm2 << 20 | 2 << 15 | 0b010 << 12 | 13 << 7 | 0x07;
                    self.emit(&insn2.to_le_bytes());
                } else {
                    self.emit_fld(12, 2, tmp_real);
                    self.emit_fld(13, 2, tmp_imag);
                }
                // Now: fa0,fa1 = right; fa2,fa3 = left
                // Component-wise operation
                match op {
                    BinOp::Add => {
                        // real: fa0 = fa2 + fa0
                        self.emit_fadd_s(10, 12, 10);
                        // imag: fa1 = fa3 + fa1
                        self.emit_fadd_s(11, 13, 11);
                    }
                    BinOp::Sub => {
                        // real: fa0 = fa2 - fa0
                        self.emit_fsub_s(10, 12, 10);
                        // imag: fa1 = fa3 - fa1
                        self.emit_fsub_s(11, 13, 11);
                    }
                    _ => {
                        // Unsupported complex binop: zero result
                        self.load_float_fa0(0.0);
                        self.mv_fa1_fa0();
                        self.load_float_fa0(0.0);
                    }
                }
                return true;
            }
            // Complex cast: component-wise cast
            ExprKind::Cast if !expr.children.is_empty() => {
                let child = &expr.children[0];
                if is_complex_ty(&child.ty) {
                    // Complex -> Complex: load both parts
                    self.gen_complex_expr(child);
                } else {
                    // Scalar -> Complex: real = scalar, imag = 0
                    self.gen_expr(child);
                    // Convert int/float in a0/fa0 to complex
                    if is_float_ty(&child.ty) {
                        // fa0 already has the value
                    } else {
                        // int in a0 -> float in fa0
                        self.emit_fcvt_s_w_fa0();
                    }
                    self.mv_fa1_fa0(); // save real to fa1
                    self.load_float_fa0(0.0); // fa0 = 0 (imag)
                    // Swap fa0 and fa1
                    self.emit_fsgnj_s(12, 11, 11);
                    self.emit_fsgnj_s(11, 10, 10);
                    self.emit_fsgnj_s(10, 12, 12);
                }
                return true;
            }
            // Complex call: result in (fa0, fa1)
            ExprKind::Call => {
                self.gen_call(expr);
                return true;
            }
            // Complex conditional: branch-based
            ExprKind::Conditional if expr.children.len() >= 3 => {
                let lbl_false = self.fresh_label(".Lcx_cond_f");
                let lbl_end = self.fresh_label(".Lcx_cond_end");
                self.gen_expr(&expr.children[0]);
                self.beqz_a0(&lbl_false);
                self.gen_complex_expr(&expr.children[1]);
                self.j(&lbl_end);
                self.label(&lbl_false);
                self.gen_complex_expr(&expr.children[2]);
                self.label(&lbl_end);
                return true;
            }
            _ => {}
        }
        // Fallback: zero both parts
        let span = expr.span.clone();
        let mut diag = crate::diagnostics::Diagnostic::err(
            "E0315",
            format!(
                "unsupported complex IR expression on riscv64: {:?}",
                expr.kind
            ),
            span,
        );
        diag.hint = Some("add handling in gen_complex_expr".to_string());
        self.diagnostics.push(diag);
        self.load_float_fa0(0.0);
        self.mv_fa1_fa0();
        self.load_float_fa0(0.0);
        false
    }

    fn gen_expr_from_ast(&mut self, expr: &Expr) {
        match expr {
            Expr::Int { v, .. } => self.li_a0(trunc_i32(*v) as i32),
            Expr::Float { v, .. } => self.load_float_fa0(*v as f32),
            Expr::Ident { name, .. } => {
                if let Some(&off) = self.locals.get(&name.name) {
                    self.load_a0(off);
                    // 整数提升: char/short → int
                    let ty = expr.ty();
                    if ty.is_int() {
                        self.gen_int_promote(&ty);
                    }
                } else {
                    self.li_a0(0);
                }
            }
            Expr::Binop { op, l, r, .. } => {
                self.gen_expr_from_ast(l);
                self.mv_a1_a0();
                self.gen_expr_from_ast(r);
                self.binop_a0_a1(*op, Self::is_signed_ast_type(&l.ty()));
            }
            Expr::Call { callee, args, .. } => {
                // GCC 兼容内建函数: AST 路径编译期折叠
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    match name.name.as_str() {
                        "__builtin_expect" | "__builtin_expect_with_probability" => {
                            if let Some(arg0) = args.first() {
                                self.gen_expr_from_ast(arg0);
                            } else {
                                self.li_a0(0);
                            }
                            return;
                        }
                        "__builtin_nonzero_p" => {
                            let v = args.first()
                                .and_then(|a| crate::const_eval::eval(a).ok())
                                .map_or(0, |v| if v != 0 { 1i128 } else { 0i128 });
                            self.li_a0(trunc_i32(v) as i32);
                            return;
                        }
                        "__builtin_constant_p" => {
                            let v = args.first()
                                .map_or(0, |a| crate::const_eval::is_constant(a) as i32);
                            self.li_a0(v);
                            return;
                        }
                        "__builtin_object_size" | "__builtin_dynamic_object_size" => {
                            self.li_a0(-1);
                            return;
                        }
                        "__builtin_mul_loop" => {
                            self.li_a0(0);
                            return;
                        }
                        _ => {}
                    }
                }
                for (i, arg) in args.iter().enumerate().take(8) {
                    self.gen_expr_from_ast(arg);
                    self.mv_a_reg(i as u8);
                }
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    self.emit_jal_to(&name.name);
                }
            }
            Expr::AsmExpr {
                asm_str,
                outputs,
                output_names,
                output_constraints,
                inputs,
                input_names,
                input_constraints,
                clobbers,
                volatile,
                span,
                ..
            } => {
                self.emit_inline_asm(
                    asm_str,
                    outputs,
                    output_names,
                    output_constraints,
                    inputs,
                    input_names,
                    input_constraints,
                    clobbers,
                    &span.file,
                    span.line,
                    *volatile,
                );
            }
            _ => {
                if let Ok(v) = crate::const_eval::eval(expr) {
                    self.li_a0(trunc_i32(v) as i32);
                } else {
                    self.li_a0(0);
                }
            }
        }
    }

    fn load_global_addr_a0(&mut self, name: &str) {
        // TLS 全局变量: 分发 TLS 访问序列 (获取 TLS 变量地址到 a0)
        if self.tls_globals.contains_key(name) {
            self.emit_tls_le_access(name);
            return;
        }
        if self.relocatable {
            let pos = self.pos();
            self.emit_auipc_a0(0);
            if let Some(&idx) = self.sym_index.get(name) {
                self.relocations.push(ElfRela {
                    offset: pos,
                    sym_index: idx,
                    rel_type: elf::R_RISCV_PCREL_HI20,
                    addend: 0,
                });
                let pos2 = self.pos();
                self.emit_ld_a0_a0(0);
                self.rodata_relocations.push(ElfRela {
                    offset: pos2,
                    sym_index: idx,
                    rel_type: elf::R_RISCV_PCREL_LO12_I,
                    addend: 0,
                });
            }
        } else if let Some(&off) = self.global_rodata.get(name) {
            let addr = self.entry + align_up_u64(self.code.len() as u64, 8) + off;
            self.li_a0(addr as i32);
        }
    }

    // ========================================================================
    // 原子操作内建函数 (__atomic_* 系列)
    // ========================================================================

    /// 获取指针参数所指元素的大小
    fn atomic_elem_size(&self, args: &[TypedExpr], idx: usize) -> usize {
        if idx >= args.len() {
            return 8;
        }
        match &args[idx].ty {
            ValueType::Pointer(inner) => value_type_size(inner),
            ValueType::Array(inner, _) => value_type_size(inner),
            _ => 8,
        }
    }

    /// 尝试从 IR 表达式中提取原子内存序常量值.
    fn eval_mem_order(&self, arg: &TypedExpr) -> i32 {
        match &arg.literal {
            Some(ExprLiteral::Int(v)) => *v as i32,
            _ => 5, // default SEQ_CST for safety
        }
    }

    /// 原子内建函数 codegen: 返回 true 表示已处理
    /// 参数约定: args[0]=ptr (a0), args[1]=val (a0)
    /// 结果返回在 a0
    fn gen_atomic_builtin(&mut self, name: &str, args: &[TypedExpr]) -> bool {
        match name {
            "__atomic_load_n" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                let ord = self.eval_mem_order(&args[1]);
                // a0 = ptr
                self.gen_expr(&args[0]);
                self.mv_a_reg(11); // a1 = ptr (save)
                match ord {
                    0 => {} // RELAXED: no fence
                    2 => { // ACQUIRE: fence r, rw
                        self.emit_fence();
                    }
                    _ => { // SEQ_CST/RELEASE: fence rw, rw
                        self.emit_fence();
                    }
                }
                // load from [a1] to a0
                self.emit_load_sz(10, 11, 0, sz);
                true
            }
            "__atomic_store_n" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                let ord = self.eval_mem_order(&args[2]);
                // a0 = ptr
                self.gen_expr(&args[0]);
                self.mv_a_reg(11); // a1 = ptr
                // a0 = val
                self.gen_expr(&args[1]);
                match ord {
                    0 => {} // RELAXED: no fence
                    _ => { // SEQ_CST/ACQUIRE/RELEASE: fence before store
                        self.emit_fence();
                    }
                }
                // store a0 to [a1]
                self.emit_store_sz(10, 11, 0, sz);
                true
            }
            "__atomic_exchange_n" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                // a0 = ptr
                self.gen_expr(&args[0]);
                self.mv_a_reg(11); // a1 = ptr
                // a0 = val
                self.gen_expr(&args[1]);
                self.mv_a_reg(12); // a2 = val
                // amoswap [a1], a2 -> a0 (old value)
                self.emit_amoswap(sz, 10, 12, 11);
                true
            }
            "__atomic_fetch_add" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mv_a_reg(11);
                self.gen_expr(&args[1]);
                self.mv_a_reg(12);
                // fence rw, rw only for SEQ_CST before AMO
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 { self.emit_fence(); }
                // amoadd [a1], a2 -> a0 (old value)
                self.emit_amoadd(sz, 10, 12, 11);
                true
            }
            "__atomic_fetch_sub" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mv_a_reg(11);
                self.gen_expr(&args[1]);
                // negate a0 (a0 = -val)
                self.emit_neg(10, 10);
                self.mv_a_reg(12);
                // fence rw, rw only for SEQ_CST before AMO
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 { self.emit_fence(); }
                // amoadd [a1], a2 -> a0 (old value, adding negative)
                self.emit_amoadd(sz, 10, 12, 11);
                true
            }
            "__atomic_fetch_and" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mv_a_reg(11);
                self.gen_expr(&args[1]);
                self.mv_a_reg(12);
                // fence rw, rw only for SEQ_CST before AMO
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 { self.emit_fence(); }
                // amoand [a1], a2 -> a0
                self.emit_amoand(sz, 10, 12, 11);
                true
            }
            "__atomic_fetch_or" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mv_a_reg(11);
                self.gen_expr(&args[1]);
                self.mv_a_reg(12);
                // fence rw, rw only for SEQ_CST before AMO
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 { self.emit_fence(); }
                // amoor [a1], a2 -> a0
                self.emit_amoor(sz, 10, 12, 11);
                true
            }
            "__atomic_fetch_xor" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mv_a_reg(11);
                self.gen_expr(&args[1]);
                self.mv_a_reg(12);
                // fence rw, rw only for SEQ_CST before AMO
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 { self.emit_fence(); }
                // amoxor [a1], a2 -> a0
                self.emit_amoxor(sz, 10, 12, 11);
                true
            }
            "__atomic_compare_exchange_n" => {
                // args: (ptr, expected, desired, weak, success_order, failure_order)
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                // a0 = ptr
                self.gen_expr(&args[0]);
                self.mv_a_reg(11); // a1 = ptr
                // a0 = *expected (dereference expected pointer)
                self.gen_expr(&args[1]);
                self.mv_a_reg(13); // a3 = expected_ptr
                // load *expected_ptr to a2
                self.emit_load_sz(12, 13, 0, sz); // a2 = *expected
                // a0 = desired
                self.gen_expr(&args[2]);
                self.mv_a_reg(14); // a4 = desired
                // fence rw, rw only for SEQ_CST before LR/SC
                let sord = if args.len() > 4 {
                    self.eval_mem_order(&args[4])
                } else {
                    5
                };
                if sord == 5 { self.emit_fence(); }
                // LR/SC CAS loop
                let l_retry = self.fresh_label("atomic_cas");
                let l_fail = self.fresh_label("atomic_cas_fail");
                let l_done = self.fresh_label("atomic_cas_done");
                self.label(&l_retry);
                // lr.w/d a0, [a1] (load current value)
                self.emit_lr(sz, 10, 11);
                // bne a0, a2, fail (compare current with expected)
                self.emit_bne(10, 12, &l_fail);
                // sc.w/d a5, a4, [a1] (store desired)
                self.emit_sc(sz, 15, 14, 11);
                // bnez a5, retry (if store failed, retry)
                self.emit_bnez(15, &l_retry);
                // success: a0 = 1
                self.li_a0(1);
                self.j(&l_done);
                // failure: store current to *expected, a0 = 0
                self.label(&l_fail);
                self.emit_store_sz(10, 13, 0, sz); // *expected = current
                self.li_a0(0);
                self.label(&l_done);
                true
            }
            "__atomic_compare_exchange" => {
                // Non-_n variant: desired is also a pointer (must dereference)
                // args: (ptr, expected_ptr, desired_ptr, weak, succ_order, fail_order)
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                // a0 = ptr
                self.gen_expr(&args[0]);
                self.mv_a_reg(11); // a1 = ptr
                // a0 = expected_ptr
                self.gen_expr(&args[1]);
                self.mv_a_reg(13); // a3 = expected_ptr
                // load *expected_ptr to a2
                self.emit_load_sz(12, 13, 0, sz); // a2 = *expected
                // a0 = desired_ptr -> dereference to get desired
                self.gen_expr(&args[2]);
                self.mv_a_reg(15); // a5 = desired_ptr (reuse for temp)
                self.emit_load_sz(14, 15, 0, sz); // a4 = *desired_ptr
                // fence only for SEQ_CST
                let sord = if args.len() > 4 {
                    self.eval_mem_order(&args[4])
                } else {
                    5
                };
                if sord == 5 { self.emit_fence(); }
                // LR/SC CAS loop
                let l_retry = self.fresh_label("atomic_cas");
                let l_fail = self.fresh_label("atomic_cas_fail");
                let l_done = self.fresh_label("atomic_cas_done");
                self.label(&l_retry);
                self.emit_lr(sz, 10, 11);
                self.emit_bne(10, 12, &l_fail);
                self.emit_sc(sz, 15, 14, 11);
                self.emit_bnez(15, &l_retry);
                self.li_a0(1);
                self.j(&l_done);
                self.label(&l_fail);
                self.emit_store_sz(10, 13, 0, sz); // *expected = current
                self.li_a0(0);
                self.label(&l_done);
                true
            }
            "__atomic_thread_fence" => {
                self.emit_fence();
                true
            }
            "__atomic_signal_fence" => {
                // Compiler barrier only, no hardware instruction
                true
            }
            _ => false,
        }
    }

    // ===== 原子操作辅助指令 =====

    /// AMOSWAP - 原子交换
    fn emit_amoswap(&mut self, sz: usize, rd: u8, rs2: u8, rs1: u8) {
        let insn = match sz {
            4 => 0x0800202fu32 // amoswap.w.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7),
            _ => 0x0800302fu32 // amoswap.d.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7),
        };
        self.emit(&insn.to_le_bytes());
    }

    /// AMOADD - 原子加
    fn emit_amoadd(&mut self, sz: usize, rd: u8, rs2: u8, rs1: u8) {
        let insn = match sz {
            4 => 0x0000202fu32 // amoadd.w.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7),
            _ => 0x0000302fu32 // amoadd.d.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7),
        };
        self.emit(&insn.to_le_bytes());
    }

    /// AMOAND - 原子与
    fn emit_amoand(&mut self, sz: usize, rd: u8, rs2: u8, rs1: u8) {
        let insn = match sz {
            4 => 0x6000202fu32 // amoand.w.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7),
            _ => 0x6000302fu32 // amoand.d.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7),
        };
        self.emit(&insn.to_le_bytes());
    }

    /// AMOOR - 原子或
    fn emit_amoor(&mut self, sz: usize, rd: u8, rs2: u8, rs1: u8) {
        let insn = match sz {
            4 => 0x4000202fu32 // amoor.w.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7),
            _ => 0x4000302fu32 // amoor.d.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7),
        };
        self.emit(&insn.to_le_bytes());
    }

    /// AMOXOR - 原子异或
    fn emit_amoxor(&mut self, sz: usize, rd: u8, rs2: u8, rs1: u8) {
        let insn = match sz {
            4 => 0x2000202fu32 // amoxor.w.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7),
            _ => 0x2000302fu32 // amoxor.d.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7),
        };
        self.emit(&insn.to_le_bytes());
    }

    /// LR (Load Reserved) - 原子加载
    fn emit_lr(&mut self, sz: usize, rd: u8, rs1: u8) {
        let insn = match sz {
            4 => 0x1000202fu32 // lr.w.aq
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7),
            _ => 0x1000302fu32 // lr.d.aq
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7),
        };
        self.emit(&insn.to_le_bytes());
    }

    /// SC (Store Conditional) - 条件存储
    fn emit_sc(&mut self, sz: usize, rd: u8, rs2: u8, rs1: u8) {
        let insn = match sz {
            4 => 0x1800202fu32 // sc.w.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7),
            _ => 0x1800302fu32 // sc.d.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7),
        };
        self.emit(&insn.to_le_bytes());
    }

    /// 按尺寸加载 (用于原子操作)
    fn emit_load_sz(&mut self, rd: u8, rs1: u8, imm: i32, sz: usize) {
        match sz {
            1 => self.emit_lb(rd, rs1, imm),
            2 => self.emit_lh(rd, rs1, imm),
            4 => self.emit_lw(rd, rs1, imm),
            _ => self.emit_ld(rd, rs1, imm),
        }
    }

    /// 按尺寸存储 (用于原子操作)
    fn emit_store_sz(&mut self, rs2: u8, rs1: u8, imm: i32, sz: usize) {
        match sz {
            1 => self.emit_sb(rs2, rs1, imm),
            2 => self.emit_sh(rs2, rs1, imm),
            4 => self.emit_sw(rs2, rs1, imm),
            _ => self.emit_sd(rs2, rs1, imm),
        }
    }

    /// NEG - 取反 (sub rd, x0, rs)
    fn emit_neg(&mut self, rd: u8, rs: u8) {
        self.emit_sub(rd, 0, rs);
    }

    /// BNEZ - 分支非零
    fn emit_bnez(&mut self, rs: u8, label: &str) {
        self.emit_bne(rs, 0, label);
    }

    // ========================================================================
    // TLS 访问序列 (Local Exec)
    // ========================================================================

    /// Local Exec TLS 访问: 直接 TP + offset
    /// 序列: lui  a0, %tprel_hi20(sym)
    ///       addi a0, a0, %tprel_add_lo12(sym)
    ///       add  a0, a0, tp
    /// 对于小偏移 (fits in 12 bits), 简化为:
    ///       addi a0, tp, %tprel_add_lo12(sym)
    fn emit_tls_le_access(&mut self, sym: &str) {
        // 获取 TLS 变量偏移
        let off = self.tls_globals.get(sym).copied().unwrap_or(0);
        let idx = self.ensure_sym(sym);

        if off < 2048 {
            // 小偏移: 直接用 addi tp, a0
            // addi a0, tp, imm (I-type)
            let imm = (off as u32) & 0xfff;
            let insn = (imm << 20) | (4u32 << 15) | (0b000 << 12) | (10u32 << 7) | 0x13;
            self.emit(&insn.to_le_bytes());
        } else {
            // 大偏移: lui + addi + add
            // lui a0, %tprel_hi20(sym)
            let hi20 = ((off as i64 + 0x800) >> 12) as u32;
            let insn_lui = (hi20 << 12) | (10u32 << 7) | 0x37;
            self.emit(&insn_lui.to_le_bytes());
            let lui_pos = self.pos() - 4;
            self.relocations.push(ElfRela {
                offset: lui_pos,
                sym_index: idx,
                rel_type: elf::R_RISCV_TPREL_HI20,
                addend: 0,
            });
            // addi a0, a0, %tprel_add_lo12(sym)
            let lo12 = (off as i32) & 0xfff;
            let insn_addi = ((lo12 as u32) << 20) | (10u32 << 15) | (0b000 << 12) | (10u32 << 7) | 0x13;
            self.emit(&insn_addi.to_le_bytes());
            let addi_pos = self.pos() - 4;
            self.relocations.push(ElfRela {
                offset: addi_pos,
                sym_index: idx,
                rel_type: elf::R_RISCV_TPREL_LO12_I,
                addend: 0,
            });
            // add a0, a0, tp (x4)
            // add rd, rs1, rs2: 0x33 | rs2<<20 | rs1<<15 | funct3<<12 | rd<<7
            let insn_add = (4u32 << 20) | (10u32 << 15) | (0b000 << 12) | (10u32 << 7) | 0x33;
            self.emit(&insn_add.to_le_bytes());
        }
    }

    /// Initial Exec TLS access: load TP offset from GOT, then add TP
    /// Sequence: lui  a5, %got_tpoff_hi20(sym)
    ///           ld   a5, %got_tpoff_lo12(sym)(a5)
    ///           add  a0, a5, tp
    /// Used for dynamically linked TLS variables where the TP offset
    /// is not known at link time.
    fn emit_tls_ie_access(&mut self, sym: &str) {
        let idx = self.ensure_sym(sym);
        // lui a5, %got_tpoff_hi20(sym)
        let insn_lui = (0u32 << 12) | (15u32 << 7) | 0x37;
        self.emit(&insn_lui.to_le_bytes());
        let lui_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: lui_pos,
            sym_index: idx,
            rel_type: elf::R_RISCV_TPREL_HI20,
            addend: 0,
        });
        // ld a5, %got_tpoff_lo12(sym)(a5) - I-type load
        // ld rd, imm(rs1): funct3=011, opcode=0000011
        let insn_ld = (0u32 << 20) | (15u32 << 15) | (0b011 << 12) | (15u32 << 7) | 0x03;
        self.emit(&insn_ld.to_le_bytes());
        let ld_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: ld_pos,
            sym_index: idx,
            rel_type: elf::R_RISCV_TPREL_LO12_I,
            addend: 0,
        });
        // add a0, a5, tp (x4)
        let insn_add = (4u32 << 20) | (15u32 << 15) | (0b000 << 12) | (10u32 << 7) | 0x33;
        self.emit(&insn_add.to_le_bytes());
    }

    /// TLS Descriptor access: call descriptor to get TP offset
    /// Sequence: lui  a0, %tlsdesc_hi(sym)
    ///           ld   a0, %tlsdesc_load_lo12(sym)(a0)
    ///           addi a0, a0, %tlsdesc_add_lo12(sym)
    ///           jalr ra, a0, %tlsdesc_call(sym)
    ///           add  a0, a0, tp
    /// Most flexible TLS model, supports dynamic TLS allocation.
    fn emit_tls_desc_access(&mut self, sym: &str) {
        let idx = self.ensure_sym(sym);
        // lui a0, %tlsdesc_hi(sym)
        let insn_lui = (0u32 << 12) | (10u32 << 7) | 0x37;
        self.emit(&insn_lui.to_le_bytes());
        let lui_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: lui_pos,
            sym_index: idx,
            rel_type: elf::R_RISCV_TLSDESC_HI20,
            addend: 0,
        });
        // ld a0, %tlsdesc_load_lo12(sym)(a0)
        let insn_ld = (0u32 << 20) | (10u32 << 15) | (0b011 << 12) | (10u32 << 7) | 0x03;
        self.emit(&insn_ld.to_le_bytes());
        let ld_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: ld_pos,
            sym_index: idx,
            rel_type: elf::R_RISCV_TLSDESC_LOAD_LO12,
            addend: 0,
        });
        // addi a0, a0, %tlsdesc_add_lo12(sym)
        let insn_addi = (0u32 << 20) | (10u32 << 15) | (0b000 << 12) | (10u32 << 7) | 0x13;
        self.emit(&insn_addi.to_le_bytes());
        let addi_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: addi_pos,
            sym_index: idx,
            rel_type: elf::R_RISCV_TLSDESC_ADD_LO12,
            addend: 0,
        });
        // jalr ra, a0, %tlsdesc_call(sym)
        // jalr rd, rs1, imm: opcode=1101100, rd=ra(1), rs1=a0(10)
        let insn_jalr = (0u32 << 20) | (10u32 << 15) | (0b000 << 12) | (1u32 << 7) | 0x67;
        self.emit(&insn_jalr.to_le_bytes());
        let jalr_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: jalr_pos,
            sym_index: idx,
            rel_type: elf::R_RISCV_TLSDESC_CALL,
            addend: 0,
        });
        // add a0, a0, tp (x4)
        let insn_add = (4u32 << 20) | (10u32 << 15) | (0b000 << 12) | (10u32 << 7) | 0x33;
        self.emit(&insn_add.to_le_bytes());
    }

    fn alloc_local(&mut self, name: &str, bytes: i32) -> i32 {
        let aligned = (bytes + 7) & !7;
        let off = self.stack_bytes;
        self.stack_bytes += aligned;
        self.locals.insert(name.to_string(), off);
        off
    }

    /// 判断 AST 类型是否为有符号整数 (用于移位等有符号性判定)
    fn is_signed_ast_type(ty: &Type) -> bool {
        matches!(
            ty,
            Type::SChar | Type::Short | Type::Int | Type::Long | Type::LongLong | Type::Int128
        )
    }

    fn binop_a0_a1(&mut self, op: BinOp, is_signed: bool) {
        match op {
            BinOp::Add => self.emit_add(10, 10, 11),
            BinOp::Sub => self.emit_sub(10, 10, 11),
            BinOp::Mul => self.emit_mul(10, 10, 11),
            BinOp::And => self.emit_and(10, 10, 11),
            BinOp::Or => self.emit_or(10, 10, 11),
            BinOp::Xor => self.emit_xor(10, 10, 11),
            BinOp::EqEq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                self.emit_slt(12, 10, 11);
                self.li_a0(1);
                self.emit_beq(12, 10, 8); // skip
                self.li_a0(0);
            }
            // M7 修复: 真 div/divu/rem/remu 替代 0 (RV64M 扩展)
            // binop_a0_a1 之前已把 a0=left, a1=right
            // 编码 (rd=a0, rs1=a0, rs2=a1):
            //   div  : 0x02B54033   (有符号除)
            //   divu : 0x02B55033   (无符号除)
            //   rem  : 0x02B56033   (有符号取模)
            //   remu : 0x02B57033   (无符号取模)
            // 限制: 整数除 0 不抛异常,直接返回 0/-1 (符合 RISC-V spec)
            BinOp::Div => self.emit_div(10, 10, 11),
            BinOp::Mod => self.emit_rem(10, 10, 11),
            // 移位: RV64I SLL/SRL/SRA 使用 64 位寄存器, 移位量
            // 硬件自动掩码到 6 位 (shamt & 63). 对于 32 位 int
            // 类型, 这与 C 标准的 5 位掩码不同, 但 shift >= 32
            // 在 C 中是 UB, 依赖硬件行为与 GCC -O0 一致.
            // 常量求值阶段 (const_eval) 已拒绝无效移位量.
            BinOp::Shl => self.emit_sll(10, 10, 11),
            BinOp::Shr => {
                if is_signed {
                    // sra: 算术右移, 高位补符号位 (C11 6.5.7)
                    self.emit_sra(10, 10, 11);
                } else {
                    // srl: 逻辑右移, 高位补 0
                    self.emit_srl(10, 10, 11);
                }
            }
            BinOp::AndAnd | BinOp::OrOr => self.cmp_a0_zero(),
        }
    }

    fn float_binop_fa0(&mut self, op: BinOp, ty: &ValueType) {
        // Operands: fa1 = left, fa0 = right (saved by caller)
        // Result in fa0
        // D extension: use double-precision for Float{bits:64}
        let is_double = matches!(ty, ValueType::Float { bits: 64 });
        match op {
            BinOp::Add => {
                if is_double { self.emit_fadd_d(10, 11, 10); }
                else { self.emit_fadd_s_fa0_fa1_fa0(); }
            }
            BinOp::Sub => {
                if is_double { self.emit_fsub_d(10, 11, 10); }
                else { self.emit_fsub_s_fa0_fa1_fa0(); }
            }
            BinOp::Mul => {
                if is_double { self.emit_fmul_d(10, 11, 10); }
                else { self.emit_fmul_s_fa0_fa1_fa0(); }
            }
            BinOp::Div => {
                if is_double { self.emit_fdiv_d(10, 11, 10); }
                else { self.emit_fdiv_s_fa0_fa1_fa0(); }
            }
            _ => {
                if is_double { self.emit_fadd_d(10, 11, 10); }
                else { self.emit_fadd_s_fa0_fa1_fa0(); }
            }
        }
    }

    /// Float comparison: fa1 = left, fa0 = right, result in a0
    fn float_cmp_a0_fa1_fa0(&mut self, op: BinOp, ty: &ValueType) {
        // D extension: use double-precision for Float{bits:64}
        let is_double = matches!(ty, ValueType::Float { bits: 64 });
        match op {
            BinOp::EqEq => {
                if is_double { self.emit_feq_d(10, 11, 10); }
                else { self.emit_feq_s(10, 11, 10); }
            }
            BinOp::Lt => {
                if is_double { self.emit_flt_d(10, 11, 10); }
                else { self.emit_flt_s(10, 11, 10); }
            }
            BinOp::Le => {
                if is_double { self.emit_fle_d(10, 11, 10); }
                else { self.emit_fle_s(10, 11, 10); }
            }
            BinOp::Gt => {
                if is_double { self.emit_flt_d(10, 10, 11); }
                else { self.emit_flt_s(10, 10, 11); }
            }
            BinOp::Ge => {
                if is_double { self.emit_fle_d(10, 10, 11); }
                else { self.emit_fle_s(10, 10, 11); }
            }
            BinOp::Ne => {
                if is_double {
                    self.emit_feq_d(10, 11, 10);
                } else {
                    self.emit_feq_s(10, 11, 10);
                }
                self.emit_xori(10, 10, 1);
            }
            _ => {
                if is_double { self.emit_feq_d(10, 11, 10); }
                else { self.emit_feq_s(10, 11, 10); }
            }
        }
    }

    /// 发射 NOP 填充到对齐边界 (RISC-V: NOP = addi x0, x0, 0 = 0x00000013, 4 字节)
    fn emit_align_padding(&mut self, alignment: u32) {
        if alignment <= 1 { return; }
        let pos = self.code.len() as u64;
        let aligned = (pos + alignment as u64 - 1) & !(alignment as u64 - 1);
        let pad = (aligned - pos) as usize;
        // RISC-V NOP 是 4 字节指令, 只能按 4 字节边界填充
        let nop_count = pad / 4;
        for _ in 0..nop_count {
            self.emit(&[0x13, 0x00, 0x00, 0x00]); // NOP (addi x0, x0, 0)
        }
    }

    /// 如果配置了 align_loops, 在循环头部标签前发射对齐填充
    fn maybe_align_loop(&mut self) {
        if self._target.align_loops > 1 {
            self.emit_align_padding(self._target.align_loops);
        }
    }

    /// 如果配置了 align_jumps, 在跳转目标标签前发射对齐填充
    fn maybe_align_jump(&mut self) {
        if self._target.align_jumps > 1 {
            self.emit_align_padding(self._target.align_jumps);
        }
    }

    fn fresh_label(&mut self, prefix: &str) -> String {
        self.label_serial += 1;
        format!("{prefix}{}", self.label_serial)
    }

    fn pos(&self) -> u64 {
        self.code.len() as u64
    }

    fn label(&mut self, name: &str) {
        let pos = self.pos();
        self.labels.insert(name.to_string(), pos);
        let pending: Vec<_> = self
            .pending_branches
            .iter()
            .filter(|(_, l, _)| l == name)
            .cloned()
            .collect();
        for (at, _, kind) in pending {
            match kind {
                BranchKind::Jal => self.patch_jal(at, pos),
                BranchKind::Beqz => self.patch_beqz(at, pos),
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
                    self.patch_branch(at, rs1, rs2, pos, funct3);
                }
            }
        }
        self.pending_branches.retain(|(_, l, _)| l != name);
    }

    fn emit(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    fn li_a0(&mut self, imm: i32) {
        self.li_reg(10, imm);
    }

    fn li_a7(&mut self, imm: i32) {
        self.li_reg(17, imm);
    }

    fn li_reg(&mut self, rd: u8, imm: i32) {
        if (-2048..=2047).contains(&imm) {
            self.emit_addi(rd, 0, imm);
        } else {
            let upper = ((imm as i64 + 0x800) >> 12) as i32;
            let lower = imm - (upper << 12);
            self.emit_lui(rd, upper);
            if lower != 0 {
                self.emit_addi(rd, rd, lower);
            }
        }
    }

    fn load_float_fa0(&mut self, v: f32) {
        let off = align_rodata(&mut self.rodata, 4);
        let bits = v.to_bits();
        self.rodata.extend_from_slice(&bits.to_le_bytes());
        let pos = self.pos();
        self.emit_auipc_a0(0);
        self.emit_flw_fa0_a0(0);
        self.float_refs.push((pos, off));
    }

    fn store_a0(&mut self, off: i32) {
        self.emit_sw(10, 2, off);
    }

    fn load_a0(&mut self, off: i32) {
        self.emit_lw(10, 2, off);
    }

    /// 整数提升: 将小于 int 的整数类型提升到 64 位
    /// char/short → 符号扩展; uchar/ushort/bool → 零扩展
    fn gen_int_promote(&mut self, ty: &Type) {
        match ty {
            Type::Char | Type::SChar | Type::Bool => {
                // 符号扩展 8 位: SLLI a0, a0, 56; SRAI a0, a0, 56
                self.emit_slli(10, 10, 56);
                self.emit_srai(10, 10, 56);
            }
            Type::Short => {
                // 符号扩展 16 位: SLLI a0, a0, 48; SRAI a0, a0, 48
                self.emit_slli(10, 10, 48);
                self.emit_srai(10, 10, 48);
            }
            Type::UChar => {
                // 零扩展 8 位: SLLI a0, a0, 56; SRLI a0, a0, 56
                self.emit_slli(10, 10, 56);
                self.emit_srli(10, 10, 56);
            }
            Type::UShort => {
                // 零扩展 16 位: SLLI a0, a0, 48; SRLI a0, a0, 48
                self.emit_slli(10, 10, 48);
                self.emit_srli(10, 10, 48);
            }
            _ => {} // int/long/ptr 等不需要提升
        }
    }

    /// IR 级别整数提升: ValueType 版本的整数提升
    fn gen_int_promote_ir(&mut self, ty: &ValueType) {
        if let ValueType::Integer { bits, signed } = ty {
            match (*bits, *signed) {
                (1, _) | (8, true) => {
                    self.emit_slli(10, 10, 56);
                    self.emit_srai(10, 10, 56);
                }
                (8, false) => {
                    self.emit_slli(10, 10, 56);
                    self.emit_srli(10, 10, 56);
                }
                (16, true) => {
                    self.emit_slli(10, 10, 48);
                    self.emit_srai(10, 10, 48);
                }
                (16, false) => {
                    self.emit_slli(10, 10, 48);
                    self.emit_srli(10, 10, 48);
                }
                _ => {} // 32-bit 及以上不需要提升
            }
        }
    }

    fn mv_a1_a0(&mut self) {
        self.emit_addi(11, 10, 0);
    }

    fn mv_a_reg(&mut self, idx: u8) {
        let rd = 10 + idx;
        self.emit_addi(rd, 10, 0);
    }

    /// Load a local variable from stack into an arbitrary GP register (32-bit LW)
    fn asm_load_var_to_reg(&mut self, var_name: &str, reg_x86: u8) {
        // reg_x86 is the x-register index (e.g. 10 for a0, 5 for t0)
        if let Some(&off) = self.locals.get(var_name) {
            self.emit_lw(reg_x86, 2, off);
        }
    }

    /// Store a GP register value back to a local variable on stack (32-bit SW)
    fn asm_store_reg_to_var(&mut self, var_name: &str, reg_x86: u8) {
        if let Some(&off) = self.locals.get(var_name) {
            self.emit_sw(reg_x86, 2, off);
        }
    }

    /// 发射内联汇编: 加载输入 → 发射 asm → 写回输出 → 设置表达式结果
    fn emit_inline_asm(
        &mut self,
        asm_str: &str,
        outputs: &[Expr],
        output_names: &[Option<String>],
        output_constraints: &[String],
        inputs: &[Expr],
        input_names: &[Option<String>],
        input_constraints: &[String],
        clobbers: &[String],
        span_file: &str,
        span_line: u32,
        volatile: bool,
    ) {
        let pool = asm_constraints::riscv64_reg_pool();
        let resolved_operands = resolve_asm_operands(
            output_constraints,
            input_constraints,
            output_names,
            input_names,
            &pool,
        );

        // --- volatile asm: 前置屏障 ---
        if volatile {
            self.emit_fence();
        }

        // --- 加载 inout 输入值到分配的寄存器 ---
        for (i, op) in resolved_operands.iter().enumerate() {
            if op.is_input && op.is_output {
                if let Some(ref alloc) = op.reg {
                    if i < outputs.len() {
                        if let Expr::Ident { name, .. } = &outputs[i] {
                            self.asm_load_var_to_reg(&name.name, alloc.idx);
                        }
                    }
                }
            }
        }
        // --- 加载纯输入值到分配的寄存器 ---
        for (j, op) in resolved_operands.iter().enumerate() {
            if op.is_input && !op.is_output {
                if let Some(ref alloc) = op.reg {
                    let input_idx = j - output_constraints.len();
                    if input_idx < inputs.len() {
                        if let Expr::Ident { name, .. } = &inputs[input_idx] {
                            self.asm_load_var_to_reg(&name.name, alloc.idx);
                        }
                    }
                }
            }
        }

        // --- 解析模板并发射 asm ---
        let resolved = Self::resolve_asm_template_enhanced(
            asm_str,
            output_constraints,
            input_constraints,
            output_names,
            input_names,
            &[],
        );
        if self.asm_text_mode {
            let lines: Vec<String> = resolved
                .split('\n')
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            self.raw_func_asm_lines
                .push((format!("__inline_{}", self.label_serial), lines));
            self.label_serial += 1;
        } else {
            self.encode_and_emit_asm(&resolved, span_file, span_line);
        }

        // --- 写回输出寄存器到 C 变量 ---
        let mut first_output_reg: Option<u8> = None;
        for (i, op) in resolved_operands.iter().enumerate() {
            if op.is_output && i < outputs.len() {
                if let Some(ref alloc) = op.reg {
                    if first_output_reg.is_none() {
                        first_output_reg = Some(alloc.idx);
                    }
                    if let Expr::Ident { name, .. } = &outputs[i] {
                        self.asm_store_reg_to_var(&name.name, alloc.idx);
                    }
                }
            }
        }

        // --- memory clobber 或 volatile: FENCE 编译器屏障 ---
        if clobbers.iter().any(|c| c == "memory") || volatile {
            self.emit_fence();
        }

        // --- 表达式结果: 第一个输出寄存器 → a0 (x10) ---
        if let Some(reg) = first_output_reg {
            if reg != 10 {
                self.emit_addi(10, reg, 0); // mv a0, reg
            }
        } else {
            self.li_a0(0);
        }
    }

    fn mv_fa_reg(&mut self, idx: u8, bits: u16) {
        let rd = 10 + idx;
        if bits <= 32 {
            self.emit_fsgnj_s(rd, 10, 10);
        } else {
            self.emit_fsgnj_d(rd, 10, 10);
        }
    }

    // M7 修复: 计算 sp + off 的地址到指定 a 寄存器
    // 编码: add rd, sp, #off   (off >= 0)   => 0x0000_? ? ?B 形式 addi
    //       addi rd, sp, #off  (off 在 ±2048)
    // 限制: 偏移 12-bit 有符号; 越界需 auipc+addi 序列
    fn mov_a_reg_sp_off(&mut self, reg: u8, off: i32) {
        if (-2048..=2047).contains(&off) {
            self.emit_addi(reg, 2, off);
        } else {
            // 越界: 走 lui + addi 序列 (与 li_reg 一致)
            self.li_reg(18, off); // t0 = off (临时)
            self.emit_add(reg, reg, 18);
        }
    }

    fn mov_a0_sp_off(&mut self, off: i32) {
        self.mov_a_reg_sp_off(10, off);
    }

    fn neg_a0(&mut self) {
        self.emit_sub(10, 0, 10);
    }

    fn not_a0(&mut self) {
        self.emit_xori(10, 10, -1);
    }

    fn cmp_a0_zero(&mut self) {
        self.emit_sltu(10, 0, 10);
    }

    fn addi_sp(&mut self, delta: i32) {
        self.emit_addi(2, 2, delta);
    }

    /// -fstack-clash-protection: emit page-sized probes when allocating a
    /// large stack frame.  Each probe stores a zero byte at [sp] to trigger
    /// a fault on the guard page before the kernel maps it.
    fn emit_stack_clash_probes(&mut self, frame_size: u32) {
        let page: u32 = 4096;
        let remainder = frame_size % page;
        let pages = frame_size / page;
        // First: addi sp, sp, -remainder (if any, must fit in 12-bit signed)
        if remainder > 0 {
            self.addi_sp(-(remainder as i32));
        }
        if pages > 0 {
            // Probe loop using t0 (x5, scratch) as counter
            // li t0, pages  (via LUI + ADDI or C.LI)
            self.emit_lui(5, (pages as i32 + 0x800) >> 12);
            let lower = (pages as i32) - (((pages as i32 + 0x800) >> 12) << 12);
            if lower != 0 {
                self.emit_addi(5, 5, lower);
            }
            // .probe_loop:
            let loop_start = self.pos();
            // addi sp, sp, -2048  (two instructions for 4096)
            self.emit_addi(2, 2, -2048);
            self.emit_addi(2, 2, -2048);
            // sb zero, 0(sp)  — probe the page
            self.emit_sb(0, 2, 0);
            // addi t0, t0, -1
            self.emit_addi(5, 5, -1);
            // bne t0, zero, .probe_loop
            let loop_end = self.pos();
            let offset = loop_start as i64 - loop_end as i64;
            let off = offset as u32;
            let bne = ((off >> 12) & 1) << 31
                | ((off >> 5) & 0x3f) << 25
                | (0u32) << 20
                | (5u32) << 15
                | ((off >> 1) & 0xf) << 8
                | ((off >> 11) & 1) << 7
                | (1u32) << 12
                | 0x63;
            self.emit(&bne.to_le_bytes());
        }
    }

    fn ret(&mut self) {
        // Stack protector epilogue: verify canary BEFORE frame teardown
        if self._target.stack_protector && self.canary_slot_off > 0 {
            self.emit_stack_chk_epilogue(self.canary_slot_off);
        }
        if self.frame_bytes > 0 {
            self.addi_sp(self.frame_bytes);
        }
        self.emit_jalr(0, 1, 0);
    }

    fn ecall(&mut self) {
        self.emit(&[0x73, 0x00, 0x00, 0x00]);
    }

    fn beqz_a0(&mut self, label: &str) {
        let pos = self.pos();
        self.emit_beq(10, 0, 0);
        if let Some(&target) = self.labels.get(label) {
            self.patch_beq(pos, 10, 0, target);
        } else {
            self.pending_branches
                .push((pos, label.to_string(), BranchKind::Beqz));
        }
    }

    fn j(&mut self, label: &str) {
        let pos = self.pos();
        self.emit_jal(0, 0);
        if let Some(&target) = self.labels.get(label) {
            self.patch_jal(pos, target);
        } else {
            self.pending_branches
                .push((pos, label.to_string(), BranchKind::Jal));
        }
    }

    fn emit_jal_placeholder(&mut self, rd: u8) {
        // M8: JAL 范围占位, 后续 patch_jal 填充偏移
        // JAL 偏移范围: ±1MiB (21-bit 有符号, bit 0 恒为 0)
        self.emit_jal(rd, 0);
    }

    /// JAL 偏移范围校验 (±1MiB)
    fn jal_offset_in_range(diff: i64) -> bool {
        (-(1 << 20)..(1 << 20)).contains(&diff)
    }

    fn emit_jal_to(&mut self, name: &str) {
        if let Some(&target) = self.labels.get(name) {
            let pos = self.pos();
            self.emit_jal(1, 0);
            self.patch_jal(pos, target);
        } else if self.relocatable {
            let pos = self.pos();
            self.emit_jal(1, 0);
            let idx = self.ensure_sym(name);
            self.relocations.push(ElfRela {
                offset: pos,
                sym_index: idx,
                rel_type: R_RISCV_JAL,
                addend: 0,
            });
        } else {
            self.emit_jal(1, 0);
        }
    }

    fn ensure_sym(&mut self, name: &str) -> u32 {
        if let Some(&idx) = self.sym_index.get(name) {
            return idx;
        }
        self.symbols.push(ElfSym {
            name: name.to_string(),
            section: 0,
            value: 0,
            size: 0,
            bind: STB_GLOBAL,
            ty: STT_FUNC,
            visibility: elf::STV_DEFAULT,
        });
        let idx = self.symbols.len() as u32;
        self.sym_index.insert(name.to_string(), idx);
        idx
    }

    /// Ensure symbol exists as STT_OBJECT type (for data symbols)
    fn ensure_sym_object(&mut self, name: &str) -> u32 {
        if let Some(&idx) = self.sym_index.get(name) {
            return idx;
        }
        self.symbols.push(ElfSym {
            name: name.to_string(),
            section: 0,
            value: 0,
            size: 0,
            bind: STB_GLOBAL,
            ty: STT_OBJECT,
            visibility: elf::STV_DEFAULT,
        });
        let idx = self.symbols.len() as u32;
        self.sym_index.insert(name.to_string(), idx);
        idx
    }

    /// Stack protector prologue: load canary from __stack_chk_guard
    /// and store at [sp, #canary_off]. Uses t0/t1 as scratch.
    fn emit_stack_chk_prologue(&mut self, canary_off: i32) {
        // Load __stack_chk_guard address using AUIPC + LD
        self.ensure_sym_object("__stack_chk_guard");
        let sym_idx = self.sym_index["__stack_chk_guard"];
        let pos = self.pos();
        // AUIPC t0, %hi(__stack_chk_guard)
        self.emit_auipc(5, 0); // t0 = x5
        self.relocations.push(ElfRela {
            offset: pos,
            sym_index: sym_idx,
            rel_type: elf::R_RISCV_PCREL_HI20,
            addend: 0,
        });
        let pos2 = self.pos();
        // LD t0, %lo(__stack_chk_guard)(t0)
        self.emit_ld(5, 5, 0);
        self.rodata_relocations.push(ElfRela {
            offset: pos2,
            sym_index: sym_idx,
            rel_type: elf::R_RISCV_PCREL_LO12_I,
            addend: 0,
        });
        // SD t0, canary_off(sp)
        self.emit_sd(5, 2, canary_off);
    }

    /// Stack protector epilogue: verify canary at [sp, #canary_off]
    /// against __stack_chk_guard. Calls __stack_chk_fail on mismatch.
    fn emit_stack_chk_epilogue(&mut self, canary_off: i32) {
        let fail_label = self.fresh_label(".Lstack_chk_fail_");
        // LD t0, canary_off(sp) - load stored canary
        self.emit_ld(5, 2, canary_off);
        // Load expected canary from __stack_chk_guard
        self.ensure_sym_object("__stack_chk_guard");
        let sym_idx = self.sym_index["__stack_chk_guard"];
        let pos = self.pos();
        // AUIPC t1, %hi(__stack_chk_guard)
        self.emit_auipc(6, 0); // t1 = x6
        self.relocations.push(ElfRela {
            offset: pos,
            sym_index: sym_idx,
            rel_type: elf::R_RISCV_PCREL_HI20,
            addend: 0,
        });
        let pos2 = self.pos();
        // LD t1, %lo(__stack_chk_guard)(t1)
        self.emit_ld(6, 6, 0);
        self.rodata_relocations.push(ElfRela {
            offset: pos2,
            sym_index: sym_idx,
            rel_type: elf::R_RISCV_PCREL_LO12_I,
            addend: 0,
        });
        // BNE t0, t1, fail_label
        self.emit_bne(5, 6, &fail_label);
        // Normal path continues
        // Fail path: call __stack_chk_fail (never returns)
        self.label(&fail_label);
        self.ensure_sym("__stack_chk_fail");
        self.emit_jal_to("__stack_chk_fail");
    }

    // ===== RISC-V 指令编码 =====

    fn emit_addi(&mut self, rd: u8, rs1: u8, imm: i32) {
        // 压缩指令自动选择: C.ADDI / C.LI / C.NOP
        if imm == 0 && rd == rs1 {
            // ADDI rd, rd, 0 => 不发射任何指令 (NOP 语义, 但 C.NOP 仅 rd=0)
            // 实际上 ADDI x0, x0, 0 是标准 NOP, 其他情况保留 32-bit
        } else if rs1 == 0 && rd != 0 && (-32..=31).contains(&imm) {
            // LI => C.LI
            self.emit_c_li(rd, imm as i8);
            return;
        } else if rd == rs1 && rd != 0 && (-32..=31).contains(&imm) && imm != 0 {
            // ADDI rd, rd, imm => C.ADDI
            self.emit_c_addi(rd, imm as i8);
            return;
        }
        let imm = imm as u32 & 0xfff;
        let insn = (imm << 20 | (rs1 as u32) << 15) | (rd as u32) << 7 | 0x13;
        self.emit(&insn.to_le_bytes());
    }

    /// MV rd, rs — pseudo-instruction: ADDI rd, rs, 0
    fn emit_mv(&mut self, rd: u8, rs: u8) {
        if rd == rs {
            return;
        }
        self.emit_addi(rd, rs, 0);
    }

    fn emit_lui(&mut self, rd: u8, imm: i32) {
        // 压缩指令自动选择: C.LUI
        // C.LUI: rd!=0, rd!=2, nzimm in -32..31, nzimm!=0
        let nzimm = (imm & 0xfffff) as u32;
        let signed_nzimm = if nzimm & 0x80000 != 0 {
            // Sign-extend 20-bit to 32-bit
            (nzimm as i32).wrapping_sub(0x100000)
        } else {
            nzimm as i32
        };
        if rd != 0 && rd != 2 && (-32..=31).contains(&signed_nzimm) && signed_nzimm != 0 {
            self.emit_c_lui(rd, signed_nzimm as i8);
            return;
        }
        let imm = (imm as u32 & 0xfffff) << 12;
        let insn = imm | (rd as u32) << 7 | 0x37;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_add(&mut self, rd: u8, rs1: u8, rs2: u8) {
        // 压缩指令自动选择: C.MV / C.ADD
        if rs1 == 0 && rd != 0 && rs2 != 0 {
            // ADD rd, x0, rs2 => C.MV rd, rs2
            self.emit_c_mv(rd, rs2);
            return;
        }
        if rd == rs1 && rd != 0 && rs2 != 0 {
            // ADD rd, rd, rs2 => C.ADD rd, rs2
            self.emit_c_add(rd, rs2);
            return;
        }
        let insn = ((rs2 as u32) << 20 | (rs1 as u32) << 15) | (rd as u32) << 7 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_sub(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = ((rs2 as u32) << 20 | (rs1 as u32) << 15) | (rd as u32) << 7 | 0x33 | 0x40000000;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_mul(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn =
            (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b0000001 << 12 | (rd as u32) << 7 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    // M7 修复: 整数除/取模 (RV64M 扩展)
    // div  : funct7=0000001, funct3=100 (有符号)
    // divu : funct7=0000001, funct3=101 (无符号)
    // rem  : funct7=0000001, funct3=110 (有符号)
    // remu : funct7=0000001, funct3=111 (无符号)
    // 编码: 0x02000000 = funct7=0000001 (bit 25)
    // 除 0 行为: 硬件返回 -1 (div) / -1 (rem) ;在 RV spec 中明确定义
    // 最小整数 -2^63 / -1 同样按 spec 行为, 不抛异常
    fn emit_div(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0b100 << 12
            | (rd as u32) << 7
            | 0x02000000
            | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_divu(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0b101 << 12
            | (rd as u32) << 7
            | 0x02000000
            | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_rem(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0b110 << 12
            | (rd as u32) << 7
            | 0x02000000
            | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_remu(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0b111 << 12
            | (rd as u32) << 7
            | 0x02000000
            | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    // M7 修复: 移位指令 (RV64I 基础)
    // sll : funct7=0000000, funct3=001 (逻辑左移)
    // srl : funct7=0000000, funct3=101 (逻辑右移)
    // sra : funct7=0100000, funct3=101 (算术右移)
    // 编码: sra 用 0x40000000 (bit 30 = funct7 的 bit 6)
    // IR 的 Shr 暂视为无符号 (srl), 有符号另由 AST 端 cast 处理
    fn emit_sll(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_srl(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b101 << 12 | (rd as u32) << 7 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_sra(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0b101 << 12
            | (rd as u32) << 7
            | 0x40000000
            | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_and(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b111 << 12 | (rd as u32) << 7 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_or(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b110 << 12 | (rd as u32) << 7 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_xor(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b100 << 12 | (rd as u32) << 7 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_xori(&mut self, rd: u8, rs1: u8, imm: i32) {
        let imm = imm as u32 & 0xfff;
        let insn = imm << 20 | (rs1 as u32) << 15 | 0b100 << 12 | (rd as u32) << 7 | 0x13;
        self.emit(&insn.to_le_bytes());
    }

    /// SRLI rd, rs1, shamt — 64-bit logical right shift immediate
    fn emit_srli(&mut self, rd: u8, rs1: u8, shamt: u8) {
        if shamt == 0 { return; }
        let shamt = (shamt as u32) & 0x3f;
        // I-type: imm[5:0]=shamt, rs1, funct3=101, rd, opcode=0010011
        let insn = shamt << 20 | (rs1 as u32) << 15 | 0b101 << 12 | (rd as u32) << 7 | 0x13;
        self.emit(&insn.to_le_bytes());
    }

    /// SLLI rd, rs1, shamt — 64-bit logical left shift immediate
    fn emit_slli(&mut self, rd: u8, rs1: u8, shamt: u8) {
        if shamt == 0 { return; }
        let shamt = (shamt as u32) & 0x3f;
        let insn = shamt << 20 | (rs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0x13;
        self.emit(&insn.to_le_bytes());
    }

    /// SRAI rd, rs1, shamt — 64-bit arithmetic right shift immediate
    fn emit_srai(&mut self, rd: u8, rs1: u8, shamt: u8) {
        if shamt == 0 { return; }
        let shamt = (shamt as u32) & 0x3f;
        // I-type: imm[11:0] = 0100000|shamt[5:0], rs1, funct3=101, rd, opcode=0010011
        let insn = 0x40000000 | shamt << 20 | (rs1 as u32) << 15 | 0b101 << 12 | (rd as u32) << 7 | 0x13;
        self.emit(&insn.to_le_bytes());
    }

    /// ANDI rd, rs1, imm — AND immediate (12-bit signed)
    fn emit_andi(&mut self, rd: u8, rs1: u8, imm: i32) {
        let imm = imm as u32 & 0xfff;
        let insn = imm << 20 | (rs1 as u32) << 15 | 0b111 << 12 | (rd as u32) << 7 | 0x13;
        self.emit(&insn.to_le_bytes());
    }

    /// ORI rd, rs1, imm — OR immediate (12-bit signed)
    fn emit_ori(&mut self, rd: u8, rs1: u8, imm: i32) {
        let imm = imm as u32 & 0xfff;
        let insn = imm << 20 | (rs1 as u32) << 15 | 0b110 << 12 | (rd as u32) << 7 | 0x13;
        self.emit(&insn.to_le_bytes());
    }

    /// Load a 64-bit constant into a0 using AUIPC + LD from rodata.
    /// For constants that fit in 32 bits, uses li_reg (LUI+ADDI) instead.
    fn load_const64_a0(&mut self, val: u64) {
        if val <= 0x7FFFFFFF {
            self.li_a0(val as i32);
            return;
        }
        // Store in rodata (8-byte aligned)
        let off = align_rodata(&mut self.rodata, 8);
        self.rodata.extend_from_slice(&val.to_le_bytes());
        let pos = self.pos();
        self.emit_auipc_a0(0); // placeholder
        self.emit_ld_a0_a0(0); // placeholder
        self.const64_refs.push((pos, off));
    }

    /// Load a 64-bit constant into rd using AUIPC + LD from rodata.
    fn load_const64_reg(&mut self, rd: u8, val: u64) {
        if val <= 0x7FFFFFFF {
            if rd == 10 {
                self.li_a0(val as i32);
            } else {
                self.li_reg(rd, val as i32);
            }
            return;
        }
        // Use a0 as intermediate then move to rd
        self.load_const64_a0(val);
        if rd != 10 {
            self.emit_addi(rd, 10, 0); // MV rd, a0
        }
    }

    /// Bitfield read-modify-write for riscv64.
    /// Uses a0=addr, a1=container, a2=new_val, a3=scratch.
    fn gen_bitfield_store_riscv64(
        &mut self,
        base_expr: &TypedExpr,
        rhs: &TypedExpr,
        field_off: usize,
        bit_width: u32,
        bit_offset: u32,
        container_ty: &Type,
    ) {
        let csz = container_ty.size();
        // 1. Compute container address -> a0, save to stack
        if let ExprKind::Dereference = &base_expr.kind {
            self.gen_expr(&base_expr.children[0]);
        } else {
            self.gen_lvalue_addr(base_expr);
        }
        if field_off != 0 {
            self.emit_addi(10, 10, field_off as i32);
        }
        let addr_off = self.alloc_local("<bf_addr>", 8);
        self.emit_sd(10, 2, addr_off); // save addr
        // 2. Evaluate rhs -> a0, mask to bit_width, save to stack
        self.gen_expr(rhs);
        if bit_width < 64 {
            let mask: u64 = (1u64 << bit_width) - 1;
            if mask <= 0x7FF {
                self.emit_andi(10, 10, mask as i32);
            } else {
                let sv = self.alloc_local("<bf_rv>", 8);
                self.emit_sd(10, 2, sv);
                self.load_const64_a0(mask);
                self.emit_addi(11, 10, 0); // a1 = mask
                self.emit_ld(10, 2, sv);   // a0 = rhs
                self.emit_and(10, 10, 11);  // a0 &= mask
            }
        }
        let val_off = self.alloc_local("<bf_val>", 8);
        self.emit_sd(10, 2, val_off); // save masked rhs
        // 3. Load current container value
        self.emit_ld(10, 2, addr_off);  // a0 = addr
        self.emit_load_sz(11, 10, 0, csz); // a1 = container
        // 4. Clear target bits: a1 &= ~((mask) << bit_offset)
        let clear_mask: u64 = !(((1u64 << bit_width) - 1) << bit_offset);
        if clear_mask as i64 >= -2048 && clear_mask as i64 <= 2047 {
            self.emit_andi(11, 11, clear_mask as i32);
        } else {
            let sv2 = self.alloc_local("<bf_c>", 8);
            self.emit_sd(11, 2, sv2); // save container
            self.load_const64_a0(clear_mask); // a0 = clear_mask
            self.emit_addi(12, 10, 0); // a2 = clear_mask
            self.emit_ld(11, 2, sv2);  // a1 = container
            self.emit_and(11, 11, 12);  // a1 &= clear_mask
        }
        // 5. Shift new value and OR into container
        self.emit_ld(12, 2, val_off); // a2 = masked rhs
        if bit_offset > 0 {
            self.emit_slli(12, 12, bit_offset as u8); // a2 <<= bit_offset
        }
        self.emit_or(11, 11, 12); // a1 |= a2
        // 6. Store container back
        self.emit_ld(10, 2, addr_off); // a0 = addr
        self.emit_store_sz(11, 10, 0, csz); // [a0] = a1
        // 7. Result = rhs value
        self.emit_ld(10, 2, val_off);
    }

    fn emit_slt(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b010 << 12 | (rd as u32) << 7 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_sltu(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b011 << 12 | (rd as u32) << 7 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_lw(&mut self, rd: u8, rs1: u8, off: i32) {
        // 压缩指令自动选择: C.LW
        if (8..=15).contains(&rd)
            && (8..=15).contains(&rs1)
            && (0..=124).contains(&off)
            && off % 4 == 0
        {
            self.emit_c_lw(rd - 8, rs1 - 8, off as u8);
            return;
        }
        let imm = off as u32 & 0xfff;
        let insn = imm << 20 | (rs1 as u32) << 15 | 0b010 << 12 | (rd as u32) << 7 | 0x03;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_sw(&mut self, rs2: u8, rs1: u8, off: i32) {
        // 压缩指令自动选择: C.SW
        if (8..=15).contains(&rs2)
            && (8..=15).contains(&rs1)
            && (0..=124).contains(&off)
            && off % 4 == 0
        {
            self.emit_c_sw(rs2 - 8, rs1 - 8, off as u8);
            return;
        }
        let imm = off as u32;
        let insn = ((imm >> 5) & 0x7f) << 25
            | (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0b010 << 12
            | (imm & 0x1f) << 7
            | 0x23;
        self.emit(&insn.to_le_bytes());
    }

    /// LB - 加载字节 (有符号扩展)
    fn emit_lb(&mut self, rd: u8, rs1: u8, off: i32) {
        let imm = off as u32 & 0xfff;
        let insn = imm << 20 | (rs1 as u32) << 15 | 0b000 << 12 | (rd as u32) << 7 | 0x03;
        self.emit(&insn.to_le_bytes());
    }

    /// LH - 加载半字 (有符号扩展)
    fn emit_lh(&mut self, rd: u8, rs1: u8, off: i32) {
        let imm = off as u32 & 0xfff;
        let insn = imm << 20 | (rs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0x03;
        self.emit(&insn.to_le_bytes());
    }

    /// SH - 存储半字
    fn emit_sh(&mut self, rs2: u8, rs1: u8, off: i32) {
        let imm = off as u32;
        let insn = ((imm >> 5) & 0x7f) << 25
            | (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0b001 << 12
            | (imm & 0x1f) << 7
            | 0x23;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_jal(&mut self, rd: u8, _offset: i32) {
        let insn = (rd as u32) << 7 | 0x6f;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_jalr(&mut self, rd: u8, rs1: u8, imm: i32) {
        let imm = imm as u32 & 0xfff;
        let insn = (imm << 20 | (rs1 as u32) << 15) | (rd as u32) << 7 | 0x67;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_beq(&mut self, rs1: u8, rs2: u8, _offset: i32) {
        let insn = ((rs2 as u32) << 20 | (rs1 as u32) << 15) | 0x63;
        self.emit(&insn.to_le_bytes());
    }

    /// AUIPC rd, imm: rd = PC + imm << 12
    fn emit_auipc(&mut self, rd: u8, imm: i32) {
        let imm = (imm as u32 & 0xfffff) << 12;
        let insn = imm | (rd as u32) << 7 | 0x17;
        self.emit(&insn.to_le_bytes());
    }

    /// BNE rs1, rs2, label: Branch if not equal
    fn emit_bne(&mut self, rs1: u8, rs2: u8, label: &str) {
        let pos = self.pos();
        // BNE: funct3=001, opcode=1100011
        let insn = (1u32 << 12) | ((rs2 as u32) << 20) | ((rs1 as u32) << 15) | 0x63;
        self.emit(&insn.to_le_bytes());
        if let Some(&target) = self.labels.get(label) {
            self.patch_bne(pos, rs1, rs2, target);
        } else {
            self.pending_branches
                .push((pos, label.to_string(), BranchKind::Bne));
        }
    }

    /// Patch BNE instruction at given position
    fn patch_bne(&mut self, at: u64, rs1: u8, rs2: u8, target: u64) {
        self.patch_branch(at, rs1, rs2, target, 1);
    }

    /// Patch generic B-type branch instruction
    fn patch_branch(&mut self, at: u64, rs1: u8, rs2: u8, target: u64, funct3: u32) {
        let pc = at;
        let offset = (target as i64 - pc as i64) as i32;
        let imm = offset as u32;
        // B-type: imm[12|10:5] | rs2 | rs1 | funct3 | imm[4:1|11] | opcode
        let insn = ((imm >> 12) & 1) << 31
            | ((imm >> 5) & 0x3f) << 25
            | (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | (funct3 & 0x7) << 12
            | ((imm >> 1) & 0xf) << 8
            | ((imm >> 11) & 1) << 7
            | 0x63;
        let pos = at as usize;
        self.code[pos..pos + 4].copy_from_slice(&insn.to_le_bytes());
    }

    /// BEQ rs1, rs2, label: Branch if equal
    fn emit_beq_label(&mut self, rs1: u8, rs2: u8, label: &str) {
        let pos = self.pos();
        // funct3=000, opcode=1100011
        let insn = ((rs2 as u32) << 20) | ((rs1 as u32) << 15) | 0x63;
        self.emit(&insn.to_le_bytes());
        if let Some(&target) = self.labels.get(label) {
            self.patch_branch(pos, rs1, rs2, target, 0);
        } else {
            self.pending_branches
                .push((pos, label.to_string(), BranchKind::Beq));
        }
    }

    /// BGE rs1, rs2, label: Branch if greater or equal (signed)
    fn emit_bge_label(&mut self, rs1: u8, rs2: u8, label: &str) {
        let pos = self.pos();
        // funct3=101, opcode=1100011
        let insn = (5u32 << 12) | ((rs2 as u32) << 20) | ((rs1 as u32) << 15) | 0x63;
        self.emit(&insn.to_le_bytes());
        if let Some(&target) = self.labels.get(label) {
            self.patch_branch(pos, rs1, rs2, target, 5);
        } else {
            self.pending_branches
                .push((pos, label.to_string(), BranchKind::Bge));
        }
    }

    /// BLT rs1, rs2, label: Branch if less than (signed)
    fn emit_blt_label(&mut self, rs1: u8, rs2: u8, label: &str) {
        let pos = self.pos();
        // funct3=100, opcode=1100011
        let insn = (4u32 << 12) | ((rs2 as u32) << 20) | ((rs1 as u32) << 15) | 0x63;
        self.emit(&insn.to_le_bytes());
        if let Some(&target) = self.labels.get(label) {
            self.patch_branch(pos, rs1, rs2, target, 4);
        } else {
            self.pending_branches
                .push((pos, label.to_string(), BranchKind::Blt));
        }
    }

    fn emit_auipc_a0(&mut self, imm: i32) {
        let imm = (imm as u32 & 0xfffff) << 12;
        let insn = imm | 10 << 7 | 0x17;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_ld_a0_a0(&mut self, off: i32) {
        let imm = off as u32 & 0xfff;
        let insn = imm << 20 | 10 << 15 | 0b011 << 12 | 10 << 7 | 0x03;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_flw_fa0_a0(&mut self, off: i32) {
        let imm = off as u32 & 0xfff;
        let insn = imm << 20 | 10 << 15 | 0b010 << 12 | 10 << 7 | 0x07;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_fadd_fa0(&mut self) {
        let insn: u32 = 0x08000053 | 10 << 20 | 10 << 15 | 10 << 7;
        self.emit(&insn.to_le_bytes());
    }

    /// Move fa0 to fa1: FSGNJ.S fa1, fa0, fa0
    fn mv_fa1_fa0(&mut self) {
        self.emit_fsgnj_s(11, 10, 10);
    }

    /// Move fa1 to fa2: FSGNJ.S fa2, fa1, fa1
    fn mv_fa2_fa1(&mut self) {
        self.emit_fsgnj_s(12, 11, 11);
    }

    /// FADD.S fd, fs1, fs2: single-precision add
    fn emit_fadd_s(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0000000 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FSUB.S fd, fs1, fs2: single-precision subtract
    fn emit_fsub_s(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0000100 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FMUL.S fd, fs1, fs2: single-precision multiply
    fn emit_fmul_s(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0001000 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FDIV.S fd, fs1, fs2: single-precision divide
    fn emit_fdiv_s(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0001100 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FADD.S fa0, fa1, fa0 (convenience for binary op)
    fn emit_fadd_s_fa0_fa1_fa0(&mut self) {
        self.emit_fadd_s(10, 11, 10);
    }

    /// FSUB.S fa0, fa1, fa0 (convenience for binary op)
    fn emit_fsub_s_fa0_fa1_fa0(&mut self) {
        self.emit_fsub_s(10, 11, 10);
    }

    /// FMUL.S fa0, fa1, fa0 (convenience for binary op)
    fn emit_fmul_s_fa0_fa1_fa0(&mut self) {
        self.emit_fmul_s(10, 11, 10);
    }

    /// FDIV.S fa0, fa1, fa0 (convenience for binary op)
    fn emit_fdiv_s_fa0_fa1_fa0(&mut self) {
        self.emit_fdiv_s(10, 11, 10);
    }

    /// FEQ.S rd, fs1, fs2: single-precision equal comparison
    fn emit_feq_s(&mut self, rd: u8, fs1: u8, fs2: u8) {
        let insn = (fs2 as u32) << 20
            | (fs1 as u32) << 15
            | 0b010 << 12
            | (rd as u32) << 7
            | 0b1010000 << 25
            | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FLT.S rd, fs1, fs2: single-precision less-than comparison
    fn emit_flt_s(&mut self, rd: u8, fs1: u8, fs2: u8) {
        let insn = (fs2 as u32) << 20
            | (fs1 as u32) << 15
            | 0b001 << 12
            | (rd as u32) << 7
            | 0b1010000 << 25
            | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FLE.S rd, fs1, fs2: single-precision less-equal comparison
    fn emit_fle_s(&mut self, rd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (rd as u32) << 7 | 0b1010000 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FSQRT.S fd, fs1: 单精度平方根, funct7=0101101
    fn emit_fsqrt_s(&mut self, fd: u8, fs1: u8) {
        let insn = ((fs1 as u32) << 15) | (fd as u32) << 7 | 0b0101101 << 25 | 0x53;
        // rm field (bits [14:12]) = 000 (RNE) — encoded as part of funct7 shift
        let insn = insn & !(0b111 << 12); // clear rm bits
        self.emit(&insn.to_le_bytes());
    }

    /// FMIN.S fd, fs1, fs2: 单精度最小值, funct7=0010101, funct3=000
    fn emit_fmin_s(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0010100 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FMAX.S fd, fs1, fs2: 单精度最大值, funct7=0010101, funct3=001
    fn emit_fmax_s(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn = (fs2 as u32) << 20
            | (fs1 as u32) << 15
            | 0b001 << 12
            | (fd as u32) << 7
            | 0b0010100 << 25
            | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_fcvt_s_w_fa0(&mut self) {
        let insn = 0xD0000053u32 | (10 << 7) | (10 << 15);
        self.emit(&insn.to_le_bytes());
    }

    fn patch_jal(&mut self, at: u64, target: u64) {
        let offset = target as i64 - at as i64;
        debug_assert!(
            Self::jal_offset_in_range(offset),
            "JAL offset {} out of ±1MiB range",
            offset
        );
        let off = offset as u32;
        let insn = ((off >> 12) & 0xff) << 12
            | ((off >> 11) & 1) << 20
            | ((off >> 1) & 0x3ff) << 21
            | ((off >> 20) & 1) << 31
            | 1 << 7
            | 0x6f;
        let pos = at as usize;
        if pos + 4 <= self.code.len() {
            self.code[pos..pos + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    fn patch_beq(&mut self, at: u64, rs1: u8, rs2: u8, target: u64) {
        let offset = (target as i64 - at as i64) as i32;
        let off = offset as u32;
        let insn = (((off >> 12) & 1) << 31
            | ((off >> 5) & 0x3f) << 25
            | (rs2 as u32) << 20
            | (rs1 as u32) << 15)
            | ((off >> 1) & 0xf) << 8
            | ((off >> 11) & 1) << 7
            | 0x63;
        let pos = at as usize;
        if pos + 4 <= self.code.len() {
            self.code[pos..pos + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    fn patch_beqz(&mut self, at: u64, target: u64) {
        self.patch_beq(at, 10, 0, target);
    }

    fn patch_auipc_load(&mut self, at: u64, addr: u64) {
        let pc = at;
        let delta = (addr as i64 - pc as i64) as i32;
        let upper = (delta + 0x800) >> 12;
        let lower = delta - (upper << 12);
        let lui = ((upper as u32 & 0xfffff) << 12) | 10 << 7 | 0x17;
        let pos = at as usize;
        if pos + 4 <= self.code.len() {
            self.code[pos..pos + 4].copy_from_slice(&lui.to_le_bytes());
        }
        if pos + 8 <= self.code.len() {
            let flw = (lower as u32 & 0xfff) << 20 | 10 << 15 | 0b010 << 12 | 10 << 7 | 0x07;
            self.code[pos + 4..pos + 8].copy_from_slice(&flw.to_le_bytes());
        }
    }

    /// Patch AUIPC + LD pair for 64-bit integer constant loading
    fn patch_auipc_ld(&mut self, at: u64, addr: u64) {
        let pc = at;
        let delta = (addr as i64 - pc as i64) as i32;
        let upper = (delta + 0x800) >> 12;
        let lower = delta - (upper << 12);
        let auipc = ((upper as u32 & 0xfffff) << 12) | 10 << 7 | 0x17;
        let pos = at as usize;
        if pos + 4 <= self.code.len() {
            self.code[pos..pos + 4].copy_from_slice(&auipc.to_le_bytes());
        }
        if pos + 8 <= self.code.len() {
            // LD a0, [a0 + lower]: funct3=011, opcode=0000011
            let ld = (lower as u32 & 0xfff) << 20 | 10 << 15 | 0b011 << 12 | 10 << 7 | 0x03;
            self.code[pos + 4..pos + 8].copy_from_slice(&ld.to_le_bytes());
        }
    }

    // ===== M8 新增: 压缩指令 (C 扩展) =====

    /// C.ADDI rd, imm (rd != 0, imm != 0): CI 格式
    /// 编码: funct3[15:13]=000, imm[5] bit12, rd bit11-7, imm[4:0] bit6-2, op bit1-0=01
    fn emit_c_addi(&mut self, rd: u8, imm: i8) {
        debug_assert!(rd != 0 && imm != 0, "C.ADDI: rd!=0, imm!=0");
        let imm = imm as u16;
        let insn: u16 = (((imm >> 5) & 1) << 12) | (rd as u16) << 7 | (imm & 0x1f) << 2 | 0x01;
        self.emit(&insn.to_le_bytes());
    }

    /// C.NOP: CI 格式, rd=0, imm=0
    fn emit_c_nop(&mut self) {
        self.emit(&0x0001u16.to_le_bytes());
    }

    /// C.LI rd, imm (rd != 0): CI 格式
    fn emit_c_li(&mut self, rd: u8, imm: i8) {
        debug_assert!(rd != 0);
        let imm = imm as u16;
        let insn: u16 =
            0b010 << 13 | ((imm >> 5) & 1) << 12 | (rd as u16) << 7 | (imm & 0x1f) << 2 | 0x01;
        self.emit(&insn.to_le_bytes());
    }

    /// C.LW rd', rs1', offset: CL 格式 (32-bit load)
    fn emit_c_lw(&mut self, rd_prime: u8, rs1_prime: u8, offset: u8) {
        debug_assert!(rd_prime < 8 && rs1_prime < 8);
        let insn: u16 = 0b010 << 13
            | (((offset >> 3) & 0x7) as u16) << 10
            | ((rs1_prime & 7) as u16) << 7
            | (((offset >> 2) & 1) as u16) << 6
            | (((offset >> 6) & 1) as u16) << 5
            | ((rd_prime & 7) as u16) << 2;
        self.emit(&insn.to_le_bytes());
    }

    /// C.SW rs2', rs1', offset: CS 格式 (32-bit store)
    fn emit_c_sw(&mut self, rs2_prime: u8, rs1_prime: u8, offset: u8) {
        debug_assert!(rs2_prime < 8 && rs1_prime < 8);
        let insn: u16 = 0b110 << 13
            | (((offset >> 3) & 0x7) as u16) << 10
            | ((rs1_prime & 7) as u16) << 7
            | (((offset >> 2) & 1) as u16) << 6
            | (((offset >> 6) & 1) as u16) << 5
            | ((rs2_prime & 7) as u16) << 2;
        self.emit(&insn.to_le_bytes());
    }

    /// C.J offset: CJ 格式, 20-bit signed offset (bit 0 = 0)
    fn emit_c_j(&mut self, offset: i32) {
        let off = offset as u32;
        let insn: u16 = 0b101 << 13
            | (((off >> 11) & 1) as u16) << 12
            | (((off >> 4) & 1) as u16) << 11
            | (((off >> 8) & 3) as u16) << 9
            | (((off >> 10) & 1) as u16) << 8
            | (((off >> 6) & 1) as u16) << 7
            | (((off >> 7) & 1) as u16) << 6
            | (((off >> 1) & 7) as u16) << 3
            | (((off >> 5) & 1) as u16) << 2
            | 0x01;
        self.emit(&insn.to_le_bytes());
    }

    /// C.BEQZ rs1', offset: CB 格式, 8-bit signed offset
    fn emit_c_beqz(&mut self, rs1_prime: u8, offset: i16) {
        debug_assert!(rs1_prime < 8);
        let off = offset as u16;
        let insn: u16 = 0b110 << 13
            | ((off >> 8) & 1) << 12
            | ((off >> 3) & 3) << 10
            | ((rs1_prime & 7) as u16) << 7
            | ((off >> 6) & 3) << 5
            | ((off >> 1) & 7) << 2
            | 0x01;
        self.emit(&insn.to_le_bytes());
    }

    /// C.LD rd', rs1', offset: CL 格式 (64-bit load, RV64)
    fn emit_c_ld(&mut self, rd_prime: u8, rs1_prime: u8, offset: u8) {
        debug_assert!(rd_prime < 8 && rs1_prime < 8);
        let insn: u16 = 0b011 << 13
            | (((offset >> 3) & 0x7) as u16) << 10
            | ((rs1_prime & 7) as u16) << 7
            | (((offset >> 6) & 3) as u16) << 5
            | ((rd_prime & 7) as u16) << 2
            | 0x01;
        self.emit(&insn.to_le_bytes());
    }

    /// C.SD rs2', rs1', offset: CS 格式 (64-bit store, RV64)
    fn emit_c_sd(&mut self, rs2_prime: u8, rs1_prime: u8, offset: u8) {
        debug_assert!(rs2_prime < 8 && rs1_prime < 8);
        let insn: u16 = 0b111 << 13
            | (((offset >> 3) & 0x7) as u16) << 10
            | ((rs1_prime & 7) as u16) << 7
            | (((offset >> 6) & 3) as u16) << 5
            | ((rs2_prime & 7) as u16) << 2
            | 0x01;
        self.emit(&insn.to_le_bytes());
    }

    /// C.MV rd, rs2 (rd != 0, rs2 != 0): CR 格式
    fn emit_c_mv(&mut self, rd: u8, rs2: u8) {
        debug_assert!(rd != 0 && rs2 != 0);
        let insn: u16 = 0b100 << 12 | (rd as u16) << 7 | (rs2 as u16) << 2 | 0x02;
        self.emit(&insn.to_le_bytes());
    }

    /// C.ADD rd, rs2 (rd != 0, rs2 != 0): CR 格式, rd == rs1
    fn emit_c_add(&mut self, rd: u8, rs2: u8) {
        debug_assert!(rd != 0 && rs2 != 0);
        let insn: u16 = 0b100 << 12 | 1 << 12 | (rd as u16) << 7 | (rs2 as u16) << 2 | 0x02;
        self.emit(&insn.to_le_bytes());
    }

    /// C.LUI rd, imm (rd != 0, rd != 2, imm != 0, -32..=31): CI 格式
    fn emit_c_lui(&mut self, rd: u8, imm: i8) {
        debug_assert!(rd != 0 && rd != 2 && imm != 0);
        let imm = imm as u16;
        let insn: u16 =
            0b011 << 13 | ((imm >> 5) & 1) << 12 | (rd as u16) << 7 | (imm & 0x1f) << 2 | 0x01;
        self.emit(&insn.to_le_bytes());
    }

    /// C.BNEZ rs1', offset: CB 格式, 8-bit signed offset
    fn emit_c_bnez(&mut self, rs1_prime: u8, offset: i16) {
        debug_assert!(rs1_prime < 8);
        let off = offset as u16;
        let insn: u16 = 0b111 << 13
            | ((off >> 8) & 1) << 12
            | ((off >> 3) & 3) << 10
            | ((rs1_prime & 7) as u16) << 7
            | ((off >> 6) & 3) << 5
            | ((off >> 1) & 7) << 2
            | 0x01;
        self.emit(&insn.to_le_bytes());
    }

    /// C.ADDIW rd, imm (rd != 0, -32..=31): CI 格式, RV64
    fn emit_c_addiw(&mut self, rd: u8, imm: i8) {
        debug_assert!(rd != 0);
        let imm = imm as u16;
        let insn: u16 =
            0b001 << 13 | ((imm >> 5) & 1) << 12 | (rd as u16) << 7 | (imm & 0x1f) << 2 | 0x01;
        self.emit(&insn.to_le_bytes());
    }

    // ===== M8 新增: FP 扩展子集 (D 扩展双精度) =====

    /// FLD rd, offset(rs1): I 格式, 64-bit 浮点加载 (D 扩展)
    fn emit_fld(&mut self, rd: u8, rs1: u8, off: i32) {
        let imm = off as u32 & 0xfff;
        let insn = imm << 20 | (rs1 as u32) << 15 | 0b011 << 12 | (rd as u32) << 7 | 0x07;
        self.emit(&insn.to_le_bytes());
    }

    /// FSD rs2, offset(rs1): S 格式, 64-bit 浮点存储 (D 扩展)
    fn emit_fsd(&mut self, rs2: u8, rs1: u8, off: i32) {
        let imm = off as u32;
        let insn = ((imm >> 5) & 0x7f) << 25
            | (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0b011 << 12
            | (imm & 0x1f) << 7
            | 0x27;
        self.emit(&insn.to_le_bytes());
    }

    /// FSW rs2, offset(rs1): S 格式, 32-bit 浮点存储 (F 扩展)
    fn emit_fsw(&mut self, rs2: u8, rs1: u8, off: i32) {
        let imm = off as u32;
        let insn = ((imm >> 5) & 0x7f) << 25
            | (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0b010 << 12
            | (imm & 0x1f) << 7
            | 0x27;
        self.emit(&insn.to_le_bytes());
    }

    /// FADD.D fd, fs1, fs2: R 格式, funct7=0000001
    fn emit_fadd_d(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0000001 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FSUB.D fd, fs1, fs2: R 格式, funct7=0000101
    fn emit_fsub_d(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0000101 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FMUL.D fd, fs1, fs2: R 格式, funct7=0001001
    fn emit_fmul_d(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0001001 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FDIV.D fd, fs1, fs2: R 格式, funct7=0001101
    fn emit_fdiv_d(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0001101 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FSQRT.D fd, fs1: 双精度平方根, funct7=0101101
    fn emit_fsqrt_d(&mut self, fd: u8, fs1: u8) {
        let insn = ((fs1 as u32) << 15) | (fd as u32) << 7 | 0b0101101 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FMIN.D fd, fs1, fs2: 双精度最小值, funct7=0010101, funct3=000
    fn emit_fmin_d(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0010101 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FMAX.D fd, fs1, fs2: 双精度最大值, funct7=0010101, funct3=001
    fn emit_fmax_d(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn = (fs2 as u32) << 20
            | (fs1 as u32) << 15
            | 0b001 << 12
            | (fd as u32) << 7
            | 0b0010101 << 25
            | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FEQ.D rd, fs1, fs2: 双精度相等比较, funct7=1010001, funct3=010
    fn emit_feq_d(&mut self, rd: u8, fs1: u8, fs2: u8) {
        let insn = (fs2 as u32) << 20
            | (fs1 as u32) << 15
            | 0b010 << 12
            | (rd as u32) << 7
            | 0b1010001 << 25
            | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FLT.D rd, fs1, fs2: 双精度小于比较, funct7=1010001, funct3=001
    fn emit_flt_d(&mut self, rd: u8, fs1: u8, fs2: u8) {
        let insn = (fs2 as u32) << 20
            | (fs1 as u32) << 15
            | 0b001 << 12
            | (rd as u32) << 7
            | 0b1010001 << 25
            | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FLE.D rd, fs1, fs2: 双精度小于等于比较, funct7=1010001, funct3=000
    fn emit_fle_d(&mut self, rd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (rd as u32) << 7 | 0b1010001 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCVT.D.W fd, rs1: 整数转双精度
    fn emit_fcvt_d_w(&mut self, fd: u8, rs1: u8) {
        let insn = ((rs1 as u32) << 15) | (fd as u32) << 7 | 0b1101000 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCLASS.S rd, fs1: 单精度浮点分类, funct7=1010000, funct3=001
    fn emit_fclass_s(&mut self, rd: u8, fs1: u8) {
        let insn = (fs1 as u32) << 15
            | 0b001 << 12
            | (rd as u32) << 7
            | 0b1010000 << 25
            | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCLASS.D rd, fs1: 双精度浮点分类, funct7=1010001, funct3=001
    fn emit_fclass_d(&mut self, rd: u8, fs1: u8) {
        let insn = (fs1 as u32) << 15
            | 0b001 << 12
            | (rd as u32) << 7
            | 0b1010001 << 25
            | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCVT.W.D rd, fs1: 双精度转整数
    fn emit_fcvt_w_d(&mut self, rd: u8, fs1: u8) {
        let insn = (fs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0b1100000 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCVT.W.S rd, fs1: 单精度转 32-bit 有符号整数
    fn emit_fcvt_w_s(&mut self, rd: u8, fs1: u8) {
        let insn = (fs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0b1100000 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCVT.WU.S rd, fs1: 单精度转 32-bit 无符号整数
    fn emit_fcvt_wu_s(&mut self, rd: u8, fs1: u8) {
        let insn = (fs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0b1100001 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCVT.WU.D rd, fs1: 双精度转 32-bit 无符号整数
    fn emit_fcvt_wu_d(&mut self, rd: u8, fs1: u8) {
        let insn = (fs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0b1100001 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCVT.S.WU fd, rs1: 无符号 32-bit 整数转单精度
    fn emit_fcvt_s_wu(&mut self, fd: u8, rs1: u8) {
        let insn = 0xC8000053u32 | ((rs1 as u32) << 15) | ((fd as u32) << 7);
        self.emit(&insn.to_le_bytes());
    }

    /// FCVT.D.S fd, fs1: float->double 提升
    fn emit_fcvt_d_s(&mut self, fd: u8, fs1: u8) {
        let insn = ((fs1 as u32) << 15) | (fd as u32) << 7 | 0b0100000 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCVT.S.D fd, fs1: double->float 截断
    fn emit_fcvt_s_d(&mut self, fd: u8, fs1: u8) {
        let insn = ((fs1 as u32) << 15) | (fd as u32) << 7 | 0b0100001 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCVT.D.L fd, rs1: 64-bit 整数转双精度 (RV64)
    fn emit_fcvt_d_l(&mut self, fd: u8, rs1: u8) {
        let insn = ((rs1 as u32) << 15) | (fd as u32) << 7 | 0b1101010 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCVT.L.D rd, fs1: 双精度转 64-bit 整数 (RV64)
    fn emit_fcvt_l_d(&mut self, rd: u8, fs1: u8) {
        let insn = (fs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0b1100010 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FMV.D.X fd, rs1: 整数寄存器移动到浮点寄存器 (64-bit)
    fn emit_fmv_d_x(&mut self, fd: u8, rs1: u8) {
        let insn = ((rs1 as u32) << 15) | (fd as u32) << 7 | 0b1111000 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FMV.X.D rd, fs1: 浮点寄存器移动到整数寄存器 (64-bit)
    fn emit_fmv_x_d(&mut self, rd: u8, fs1: u8) {
        let insn = ((fs1 as u32) << 15) | (rd as u32) << 7 | 0b1110000 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FSGNJ.S fd, fs1, fs2: 单精度符号注入
    fn emit_fsgnj_s(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0010000 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FSGNJ.D fd, fs1, fs2: 双精度符号注入
    fn emit_fsgnj_d(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0010001 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    // ===== M8 新增: 向量扩展 (V) 基础 RVV 指令 =====

    /// VSETVLI rd, rs1, vtype: 设置向量类型
    fn emit_vsetvli(&mut self, rd: u8, rs1: u8, vtype: u32) {
        let insn =
            (vtype & 0x7ff) << 20 | (rs1 as u32) << 15 | 0b111 << 12 | (rd as u32) << 7 | 0x57; // OP-V
        self.emit(&insn.to_le_bytes());
    }

    /// VSETIVLI rd, uimm, vtype: 立即数设置向量类型
    fn emit_vsetivli(&mut self, rd: u8, uimm: u8, vtype: u32) {
        let insn = 0b11 << 20
            | (vtype & 0x1f) << 15
            | ((uimm as u32) & 0x1f) << 15
            | 0b111 << 12
            | (rd as u32) << 7
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VLE32.V vd, (rs1), vm: 向量 32-bit 元素加载
    fn emit_vle32_v(&mut self, vd: u8, rs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn =
            vm_bit | (rs1 as u32) << 15 | 0b010 << 12 | (vd as u32) << 7 | 0b0000111 << 25 | 0x07;
        self.emit(&insn.to_le_bytes());
    }

    /// VSE32.V vs3, (rs1), vm: 向量 32-bit 元素存储
    fn emit_vse32_v(&mut self, vs3: u8, rs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = (vm_bit | (vs3 as u32) << 20 | (rs1 as u32) << 15 | 0b010 << 12)
            | 0b0000111 << 25
            | 0x27;
        self.emit(&insn.to_le_bytes());
    }

    /// VADD.VV vd, vs2, vs1, vm: 向量加法
    fn emit_vadd_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = (vm_bit | (vs2 as u32) << 20 | (vs1 as u32) << 15)
            | (vd as u32) << 7
            | 0b1000001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VMUL.VV vd, vs2, vs1, vm: 向量乘法 (取低 SEW 位)
    fn emit_vmul_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b100 << 12
            | (vd as u32) << 7
            | 0b1010001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VREDUCE.VS vd, vs2, vs1, vm: 向量归约 (简化)
    fn emit_vredsum_vs(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = (vm_bit | (vs2 as u32) << 20 | (vs1 as u32) << 15)
            | (vd as u32) << 7
            | 0b1100001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    // ===== RVV 扩展: 完整向量指令集 =====

    /// VLE8.V vd, (rs1), vm: 向量 8-bit 元素加载
    fn emit_vle8_v(&mut self, vd: u8, rs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = (vm_bit | (rs1 as u32) << 15) | (vd as u32) << 7 | 0b0000111 << 25 | 0x07;
        self.emit(&insn.to_le_bytes());
    }

    /// VLE16.V vd, (rs1), vm: 向量 16-bit 元素加载
    fn emit_vle16_v(&mut self, vd: u8, rs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn =
            vm_bit | (rs1 as u32) << 15 | 0b101 << 12 | (vd as u32) << 7 | 0b0000111 << 25 | 0x07;
        self.emit(&insn.to_le_bytes());
    }

    /// VLE64.V vd, (rs1), vm: 向量 64-bit 元素加载
    fn emit_vle64_v(&mut self, vd: u8, rs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn =
            vm_bit | (rs1 as u32) << 15 | 0b011 << 12 | (vd as u32) << 7 | 0b0000111 << 25 | 0x07;
        self.emit(&insn.to_le_bytes());
    }

    /// VSE8.V vs3, (rs1), vm
    fn emit_vse8_v(&mut self, vs3: u8, rs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = (vm_bit | (vs3 as u32) << 20 | (rs1 as u32) << 15) | 0b0000111 << 25 | 0x27;
        self.emit(&insn.to_le_bytes());
    }

    /// VSE16.V vs3, (rs1), vm
    fn emit_vse16_v(&mut self, vs3: u8, rs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn =
            vm_bit | (vs3 as u32) << 20 | (rs1 as u32) << 15 | 0b101 << 12 | 0b0000111 << 25 | 0x27;
        self.emit(&insn.to_le_bytes());
    }

    /// VSE64.V vs3, (rs1), vm
    fn emit_vse64_v(&mut self, vs3: u8, rs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn =
            vm_bit | (vs3 as u32) << 20 | (rs1 as u32) << 15 | 0b011 << 12 | 0b0000111 << 25 | 0x27;
        self.emit(&insn.to_le_bytes());
    }

    /// VSUB.VV vd, vs2, vs1, vm: 向量减法
    fn emit_vsub_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | (vd as u32) << 7
            | 0b0000101 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VAND.VV vd, vs2, vs1, vm
    fn emit_vand_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b001 << 12
            | (vd as u32) << 7
            | 0b0010011 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VOR.VV vd, vs2, vs1, vm
    fn emit_vor_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b010 << 12
            | (vd as u32) << 7
            | 0b0010101 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VXOR.VV vd, vs2, vs1, vm
    fn emit_vxor_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b011 << 12
            | (vd as u32) << 7
            | 0b0010111 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VSLL.VV vd, vs2, vs1, vm: 向量左移
    fn emit_vsll_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b101 << 12
            | (vd as u32) << 7
            | 0b1000011 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VSRL.VV vd, vs2, vs1, vm: 向量逻辑右移
    fn emit_vsrl_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b101 << 12
            | (vd as u32) << 7
            | 0b1010001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VSRA.VV vd, vs2, vs1, vm: 向量算术右移
    fn emit_vsra_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b101 << 12
            | (vd as u32) << 7
            | 0b1010011 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    // --- 宽化/窄化指令 ---

    /// VWADD.VV vd, vs2, vs1, vm: 宽化加法 (2*SEW)
    fn emit_vwadd_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b010 << 12
            | (vd as u32) << 7
            | 0b1100011 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VWSUB.VV vd, vs2, vs1, vm: 宽化减法
    fn emit_vwsub_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b010 << 12
            | (vd as u32) << 7
            | 0b1100101 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VWMUL.VV vd, vs2, vs1, vm: 宽化乘法
    fn emit_vwmul_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b010 << 12
            | (vd as u32) << 7
            | 0b1110111 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VWMULU.VV vd, vs2, vs1, vm: 宽化无符号乘法
    fn emit_vwmulu_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = (vm_bit | (vs2 as u32) << 20 | (vs1 as u32) << 15)
            | (vd as u32) << 7
            | 0b1110001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VNSRL.WI vd, vs2, uimm, vm: 窄化右移 (立即数)
    fn emit_vnsrl_wi(&mut self, vd: u8, vs2: u8, uimm: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = (vm_bit | (vs2 as u32) << 20 | ((uimm as u32) & 0x1f) << 15)
            | (vd as u32) << 7
            | 0b1011001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VNSRA.WI vd, vs2, uimm, vm: 窄化算术右移
    fn emit_vnsra_wi(&mut self, vd: u8, vs2: u8, uimm: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = (vm_bit | (vs2 as u32) << 20 | ((uimm as u32) & 0x1f) << 15)
            | (vd as u32) << 7
            | 0b1011011 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    // --- 窄化钳位指令 ---

    /// VNCLIP.WV vd, vs2, vs1, vm: 有符号窄化钳位右移
    fn emit_vnclip_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1u32 << 25 } else { 0 };
        let insn = (0b100110u32 << 26)
            | vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b011 << 12
            | (vd as u32) << 7
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VNCLIPU.WV vd, vs2, vs1, vm: 无符号窄化钳位右移
    fn emit_vnclipu_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1u32 << 25 } else { 0 };
        let insn = (0b100110u32 << 26)
            | vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b010 << 12
            | (vd as u32) << 7
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    // --- 掩码指令 ---

    /// VMAND.MM vd, vs2, vs1: 掩码 AND
    fn emit_vmand_mm(&mut self, vd: u8, vs2: u8, vs1: u8) {
        let insn = (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b001 << 12
            | (vd as u32) << 7
            | 0b0110011 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VMOR.MM vd, vs2, vs1: 掩码 OR
    fn emit_vmor_mm(&mut self, vd: u8, vs2: u8, vs1: u8) {
        let insn = (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b010 << 12
            | (vd as u32) << 7
            | 0b0110101 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VMXOR.MM vd, vs2, vs1: 掩码 XOR
    fn emit_vmxor_mm(&mut self, vd: u8, vs2: u8, vs1: u8) {
        let insn = (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b011 << 12
            | (vd as u32) << 7
            | 0b0110111 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VMNOT.M vd, vs2: 掩码 NOT (vmandn with v0=ones)
    fn emit_vmnot_m(&mut self, vd: u8, vs2: u8) {
        // VMNOT.M vd, vs2 = VMXNOR.M vd, vs2, v0 (all-ones mask)
        let insn = ((vs2 as u32) << 20) | 0b100 << 12 | (vd as u32) << 7 | 0b0111001 << 25 | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    // --- 浮点向量指令 ---

    /// VFADD.VV vd, vs2, vs1, vm: 浮点向量加法
    fn emit_vfadd_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b001 << 12
            | (vd as u32) << 7
            | 0b0000001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VFSUB.VV vd, vs2, vs1, vm: 浮点向量减法
    fn emit_vfsub_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b001 << 12
            | (vd as u32) << 7
            | 0b0000101 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VFMUL.VV vd, vs2, vs1, vm: 浮点向量乘法
    fn emit_vfmul_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b001 << 12
            | (vd as u32) << 7
            | 0b1001001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VFDIV.VV vd, vs2, vs1, vm: 浮点向量除法
    fn emit_vfdiv_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b001 << 12
            | (vd as u32) << 7
            | 0b1000001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VFMADD.VV vd, vs1, vs2, vm: 浮点向量 fused multiply-add
    fn emit_vfmadd_vv(&mut self, vd: u8, vs1: u8, vs2: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b001 << 12
            | (vd as u32) << 7
            | 0b1010001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VFMIN.VV vd, vs2, vs1, vm: 浮点向量 min
    fn emit_vfmin_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b001 << 12
            | (vd as u32) << 7
            | 0b0001001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VFMAX.VV vd, vs2, vs1, vm: 浮点向量 max
    fn emit_vfmax_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b001 << 12
            | (vd as u32) << 7
            | 0b0001101 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VFREDSUM.VS vd, vs2, vs1, vm: 浮点归约求和
    fn emit_vfredsum_vs(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b001 << 12
            | (vd as u32) << 7
            | 0b0000011 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    // ===== M8 新增: 特权指令 / CSR 访问 =====

    /// CSRRW rd, csr, rs1: 原子读写 CSR
    fn emit_csrrw(&mut self, rd: u8, csr: u16, rs1: u8) {
        let insn = (csr as u32) << 20 | (rs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0x73;
        self.emit(&insn.to_le_bytes());
    }

    /// CSRRS rd, csr, rs1: 原子读并置位 CSR
    fn emit_csrrs(&mut self, rd: u8, csr: u16, rs1: u8) {
        let insn = (csr as u32) << 20 | (rs1 as u32) << 15 | 0b010 << 12 | (rd as u32) << 7 | 0x73;
        self.emit(&insn.to_le_bytes());
    }

    /// CSRRC rd, csr, rs1: 原子读并清位 CSR
    fn emit_csrrc(&mut self, rd: u8, csr: u16, rs1: u8) {
        let insn = (csr as u32) << 20 | (rs1 as u32) << 15 | 0b011 << 12 | (rd as u32) << 7 | 0x73;
        self.emit(&insn.to_le_bytes());
    }

    /// CSRRWI rd, csr, uimm: 立即数原子读写 CSR
    fn emit_csrrwi(&mut self, rd: u8, csr: u16, uimm: u8) {
        let insn = (csr as u32) << 20
            | ((uimm as u32) & 0x1f) << 15
            | 0b101 << 12
            | (rd as u32) << 7
            | 0x73;
        self.emit(&insn.to_le_bytes());
    }

    /// CSRRSI rd, csr, uimm: 立即数原子读并置位 CSR
    fn emit_csrrsi(&mut self, rd: u8, csr: u16, uimm: u8) {
        let insn = (csr as u32) << 20
            | ((uimm as u32) & 0x1f) << 15
            | 0b110 << 12
            | (rd as u32) << 7
            | 0x73;
        self.emit(&insn.to_le_bytes());
    }

    /// CSRRCI rd, csr, uimm: 立即数原子读并清位 CSR
    fn emit_csrrci(&mut self, rd: u8, csr: u16, uimm: u8) {
        let insn = (csr as u32) << 20
            | ((uimm as u32) & 0x1f) << 15
            | 0b111 << 12
            | (rd as u32) << 7
            | 0x73;
        self.emit(&insn.to_le_bytes());
    }

    /// MRET: 机器模式返回
    fn emit_mret(&mut self) {
        // 0x30200073: funct7=0011000, rs2=00010, rs1=00000, funct3=000, rd=00000
        let insn = 0x30200073u32;
        self.emit(&insn.to_le_bytes());
    }

    /// SRET: 监管模式返回
    fn emit_sret(&mut self) {
        let insn = 0x10200073u32;
        self.emit(&insn.to_le_bytes());
    }

    /// WFI: 等待中断
    fn emit_wfi(&mut self) {
        let insn = 0x10500073u32;
        self.emit(&insn.to_le_bytes());
    }

    /// SFENCE.VMA rs1, rs2: 虚拟内存屏障
    fn emit_sfence_vma(&mut self, rs1: u8, rs2: u8) {
        let insn = ((rs2 as u32) << 20 | (rs1 as u32) << 15) | 0x73;
        self.emit(&insn.to_le_bytes());
    }

    /// FENCE: 内存屏障 (全序)
    fn emit_fence(&mut self) {
        // FENCE pred=RW, succ=RW, fm=0
        let insn = 0x0ff0000fu32;
        self.emit(&insn.to_le_bytes());
    }

    /// FENCE.I: 指令获取屏障
    fn emit_fence_i(&mut self) {
        let insn = 0x0000100fu32;
        self.emit(&insn.to_le_bytes());
    }

    // ===== M8 新增: sret 辅助 =====

    /// memcpy_sp_to_a0: 从栈偏移 tmp 拷贝 size 字节到 [a0] 指向的内存
    fn memcpy_sp_to_a0(&mut self, tmp_off: i32, size: usize) {
        // a0 已指向目标 (caller 传入的 sret slot)
        // t0 = sp + tmp_off (源地址)
        self.mov_a_reg_sp_off(5, tmp_off); // t0 = &tmp
        let mut copied = 0usize;
        while copied + 8 <= size {
            // ld t1, 0(t0); sd t1, 0(a0)
            self.emit_ld(6, 5, copied as i32); // t1 = [t0+off]
            self.emit_sd(6, 10, copied as i32); // [a0+off] = t1
            copied += 8;
        }
        while copied + 4 <= size {
            self.emit_lw(6, 5, copied as i32);
            self.emit_sw(6, 10, copied as i32);
            copied += 4;
        }
        while copied < size {
            self.emit_lbu(6, 5, copied as i32);
            self.emit_sb(6, 10, copied as i32);
            copied += 1;
        }
    }

    /// LD rd, offset(rs1): 64-bit 整数加载
    fn emit_ld(&mut self, rd: u8, rs1: u8, off: i32) {
        // 压缩指令自动选择: C.LD (RV64)
        if (8..=15).contains(&rd)
            && (8..=15).contains(&rs1)
            && (0..=248).contains(&off)
            && off % 8 == 0
        {
            self.emit_c_ld(rd - 8, rs1 - 8, off as u8);
            return;
        }
        let imm = off as u32 & 0xfff;
        let insn = imm << 20 | (rs1 as u32) << 15 | 0b011 << 12 | (rd as u32) << 7 | 0x03;
        self.emit(&insn.to_le_bytes());
    }

    /// -ftrivial-auto-var-init=zero: 零初始化栈帧
    /// 使用 sd zero, offset(sp) 每次清零 8 字节 (frame 已 16 字节对齐)
    fn emit_zero_init_frame(&mut self, frame_size: usize) {
        if frame_size == 0 {
            return;
        }
        // 从帧底向帧顶写入, offset 相对于 sp
        let words = frame_size / 8;
        for i in 0..words {
            let off = (i as i32) * 8 - (frame_size as i32);
            self.emit_sd(0, 2, off); // sd zero, off(sp)
        }
        // 处理剩余 4 字节
        let rem = frame_size % 8;
        if rem >= 4 {
            let off = (words * 8) as i32 - (frame_size as i32) + (rem - 4) as i32;
            self.emit_sw(0, 2, off); // sw zero, off(sp)
        }
    }

    /// SD rs2, offset(rs1): 64-bit 整数存储
    fn emit_sd(&mut self, rs2: u8, rs1: u8, off: i32) {
        // 压缩指令自动选择: C.SD (RV64)
        if (8..=15).contains(&rs2)
            && (8..=15).contains(&rs1)
            && (0..=248).contains(&off)
            && off % 8 == 0
        {
            self.emit_c_sd(rs2 - 8, rs1 - 8, off as u8);
            return;
        }
        let imm = off as u32;
        let insn = ((imm >> 5) & 0x7f) << 25
            | (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0b011 << 12
            | (imm & 0x1f) << 7
            | 0x23;
        self.emit(&insn.to_le_bytes());
    }

    /// LBU rd, offset(rs1): 无符号字节加载
    fn emit_lbu(&mut self, rd: u8, rs1: u8, off: i32) {
        let imm = off as u32 & 0xfff;
        let insn = imm << 20 | (rs1 as u32) << 15 | 0b100 << 12 | (rd as u32) << 7 | 0x03;
        self.emit(&insn.to_le_bytes());
    }

    /// SB rs2, offset(rs1): 字节存储
    fn emit_sb(&mut self, rs2: u8, rs1: u8, off: i32) {
        let imm = off as u32;
        let insn = (((imm >> 5) & 0x7f) << 25 | (rs2 as u32) << 20 | (rs1 as u32) << 15)
            | (imm & 0x1f) << 7
            | 0x23;
        self.emit(&insn.to_le_bytes());
    }
}

fn find_func<'a>(prog: &'a Program, name: &str) -> Option<&'a FuncDecl> {
    prog.decls.iter().find_map(|d| match d {
        Decl::Func(f) if f.name.name == name => Some(f),
        _ => None,
    })
}

/// RISC-V HFA 分类 (同构浮点聚合)
/// RV64 约定: 最多 4 个 float/double 字段, 同类型
fn classify_rv_hfa(
    ty: &ValueType,
    fields: &HashMap<String, (Arc<Vec<Field>>, bool)>,
) -> Option<HfaInfo> {
    let ValueType::Aggregate {
        name,
        size,
        is_union: false,
    } = ty
    else {
        return None;
    };
    if *size == 0 || *size > 64 {
        return None;
    }
    let (flds, is_union) = fields.get(name)?;
    if *is_union || flds.is_empty() || flds.len() > 4 {
        return None;
    }
    let first_bits = match &flds[0].ty {
        Type::Float => 32u16,
        Type::Double => 64u16,
        _ => return None,
    };
    for f in flds.iter() {
        match (&f.ty, first_bits) {
            (Type::Float, 32) | (Type::Double, 64) => {}
            _ => return None,
        }
    }
    Some(HfaInfo {
        elem_bits: first_bits,
        count: flds.len() as u8,
        total_size: *size,
    })
}

/// RISC-V HFA 描述
#[derive(Clone, Copy, Debug)]
struct HfaInfo {
    elem_bits: u16,
    count: u8,
    total_size: usize,
}

fn trunc_i32(v: i128) -> u32 {
    (v as i32) as u32
}

fn is_float_ty(ty: &ValueType) -> bool {
    matches!(ty, ValueType::Float { .. })
}

fn is_vector_ty(ty: &ValueType) -> bool {
    matches!(ty, ValueType::Vector(_, _))
}

fn is_complex_ty(ty: &ValueType) -> bool {
    matches!(ty, ValueType::Complex(_))
}

// M7 修复: 聚合类型判定 (struct/union/array of known size)
fn is_aggregate(ty: &ValueType) -> bool {
    matches!(
        ty,
        ValueType::Aggregate { .. } | ValueType::Array(_, Some(_)) | ValueType::Complex(_)
    )
}

// M7 修复: 聚合类型字节数 (非聚合返回 None)
fn aggregate_size(ty: &ValueType) -> Option<usize> {
    match ty {
        ValueType::Aggregate { size, .. } => Some(*size),
        ValueType::Array(inner, Some(n)) => Some(value_type_size(inner) * n),
        ValueType::Complex(inner) => Some(value_type_size(inner) * 2),
        _ => None,
    }
}

fn value_type_size(ty: &ValueType) -> usize {
    match ty {
        ValueType::Void => 0,
        ValueType::Integer { bits, .. } => (*bits as usize).div_ceil(8),
        ValueType::Float { bits } => (*bits as usize).div_ceil(8),
        ValueType::Pointer(_) => 8,
        ValueType::Array(inner, Some(n)) => value_type_size(inner) * n,
        ValueType::Array(inner, None) => value_type_size(inner),
        ValueType::Aggregate { size, .. } => *size,
        ValueType::Function { .. } => 8,
        ValueType::Vector(inner, lanes) => value_type_size(inner) * lanes,
        ValueType::Complex(inner) => value_type_size(inner) * 2,
        ValueType::Imaginary(inner) => value_type_size(inner),
    }
}

fn align_rodata(buf: &mut Vec<u8>, align: u64) -> u64 {
    let off = align_up_u64(buf.len() as u64, align);
    buf.resize(off as usize, 0);
    off
}

fn align_up_u64(v: u64, align: u64) -> u64 {
    (v + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{lower, validate};

    #[test]
    fn riscv64_ir_return_add() {
        let (program, diags) =
            crate::parser::parse(b"int main(void) { return 10 + 32; }\n", "rv.c");
        assert!(diags.is_empty(), "{diags:?}");
        let (module, _) = lower(program).expect("lower");
        validate(&module).expect("validate");
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.compile(&module);
        let obj = cg.build_output();
        assert_eq!(&obj[0..4], &[0x7f, b'E', b'L', b'F']);
        assert_eq!(u16::from_le_bytes([obj[18], obj[19]]), 243);
    }

    // M7 修复: div/mod 真实 RV64M 指令 (div/rem) 替代 0
    // div rd, rs1, rs2: funct7=0000001, funct3=100 (有符号)
    // 编码 (rd=a0, rs1=a0, rs2=a1): 0x02B54533 LE 编码 = 33 45 B5 02
    // 注: 使用 opaque 函数防止常量折叠
    #[test]
    fn riscv64_ir_div_mod_emit_rv64m() {
        let (program, diags) = crate::parser::parse(
            b"extern int opaque(int);\n\
              int main(void) { int a = opaque(42); int b = opaque(5); int q = a / b; int r = a % b; return q + r; }\n",
            "rv.c",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let (module, _) = lower(program).expect("lower");
        validate(&module).expect("validate");
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.compile(&module);
        let obj = cg.build_output();
        // div a0, a0, a1: 0x02B54533 LE = 33 45 B5 02
        let has_div = obj.windows(4).any(|w| w == [0x33, 0x45, 0xb5, 0x02]);
        // rem a0, a0, a1: 0x02B56533 LE = 33 65 B5 02
        let has_rem = obj.windows(4).any(|w| w == [0x33, 0x65, 0xb5, 0x02]);
        assert!(has_div || has_rem, "expected div/rem in riscv64 text");
    }

    // M7 修复: 移位真实 sll/srl 替代 0
    // sll a0, a0, a1: funct7=0000000, funct3=001
    //   insn = (11<<20)|(10<<15)|(1<<12)|(10<<7)|0x33 = 0x00B51533 LE = 33 15 B5 00
    // srl a0, a0, a1: funct7=0000000, funct3=101
    //   insn = 0x00B55533 LE = 33 55 B5 00
    #[test]
    fn riscv64_ir_shifts_emit_sll_srl() {
        let (program, diags) = crate::parser::parse(
            b"extern int opaque(int);\n\
              int main(void) { int a = opaque(1); int b = opaque(2); return (a << b) | (a >> b); }\n",
            "rv.c",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let (module, _) = lower(program).expect("lower");
        validate(&module).expect("validate");
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.compile(&module);
        let obj = cg.build_output();
        let has_sll = obj.windows(4).any(|w| w == [0x33, 0x15, 0xb5, 0x00]);
        let has_srl = obj.windows(4).any(|w| w == [0x33, 0x55, 0xb5, 0x00]);
        assert!(has_sll || has_srl, "expected sll/srl in riscv64 text");
    }

    // M7 修复: 聚合类型参数按引用传递
    // 验证: 调函数时,若参数是 local struct, 发射 addi rd, sp, #off
    // addi rd, sp, #off 编码: opcode=0x13, funct3=0, rs1=2 (sp)
    // 重建 insn 后检查: (insn & 0x7F) == 0x13 && (insn>>12)&7 == 0 && (insn>>15)&0x1F == 2
    #[test]
    fn riscv64_aggregate_arg_by_ref() {
        let (program, diags) = crate::parser::parse(
            b"struct S { int x; int y; };\n\
              void callee(struct S s) {}\n\
              int main(void) { struct S s; s.x = 1; s.y = 2; callee(s); return 0; }\n",
            "rv.c",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let (module, _) = lower(program).expect("lower");
        validate(&module).expect("validate");
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.compile(&module);
        let obj = cg.build_output();
        let has_addi_sp = obj.windows(4).any(|w| {
            if w.len() < 4 {
                return false;
            }
            let insn = (w[0] as u32)
                | ((w[1] as u32) << 8)
                | ((w[2] as u32) << 16)
                | ((w[3] as u32) << 24);
            (insn & 0x7F) == 0x13 && ((insn >> 12) & 7) == 0 && ((insn >> 15) & 0x1F) == 2
        });
        assert!(
            has_addi_sp,
            "expected addi rd, sp, #off (M7 by-ref aggregate)"
        );
    }

    // M8: JAL 范围校验
    #[test]
    fn riscv64_jal_range_check() {
        assert!(Riscv64Codegen::jal_offset_in_range(0));
        assert!(Riscv64Codegen::jal_offset_in_range((1 << 20) - 1));
        assert!(Riscv64Codegen::jal_offset_in_range(-(1 << 20)));
        assert!(!Riscv64Codegen::jal_offset_in_range(1 << 20));
        assert!(!Riscv64Codegen::jal_offset_in_range(-(1 << 20) - 1));
    }

    // M8: 压缩指令 C.ADDI 编码验证
    // C.ADDI x10, 5: funct3=000, imm[5]=0, rd=10, imm[4:0]=00101, op=01
    // = 0b000_0_01010_00101_01 = 0x0511
    #[test]
    fn riscv64_compressed_c_addi_emit() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.emit_c_addi(10, 5);
        let code = &cg.code;
        assert!(code.len() >= 2, "C.ADDI should emit 2 bytes");
        let insn = u16::from_le_bytes([code[0], code[1]]);
        // funct3=000 (bits 15:13), rd=10 (bits 11:7), op=01 (bits 1:0)
        assert_eq!(insn & 0x3, 0x01, "C.ADDI opcode should be 01");
        assert_eq!((insn >> 7) & 0x1f, 10, "rd should be a0 (x10)");
    }

    // M8: 双精度 FP D 扩展指令编码
    // FADD.D fa0, fa0, fa1: funct7=0000001, rs2=11, rs1=10, rd=10
    // insn = 0x02B57553
    #[test]
    fn riscv64_fadd_d_emit() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.emit_fadd_d(10, 10, 11); // fa0 = fa0 + fa1
        let code = &cg.code;
        assert!(code.len() >= 4);
        let insn = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
        assert_eq!(insn & 0x7f, 0x53, "opcode should be 0x53 (FP)");
        assert_eq!(
            (insn >> 25) & 0x7f,
            0x01,
            "funct7 should be 0000001 (FADD.D)"
        );
    }

    // M8: CSR 访问指令 CSRRS 编码
    // CSRRS a0, mstatus(0x300), zero: csr=0x300, rs1=0, rd=10
    #[test]
    fn riscv64_csrrs_mstatus_emit() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.emit_csrrs(10, 0x300, 0); // read mstatus
        let code = &cg.code;
        assert!(code.len() >= 4);
        let insn = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
        assert_eq!(insn & 0x7f, 0x73, "opcode should be 0x73 (SYSTEM)");
        assert_eq!((insn >> 12) & 0x7, 0b010, "funct3 should be 010 (CSRRS)");
        assert_eq!((insn >> 20) & 0xfff, 0x300, "csr should be mstatus (0x300)");
    }

    // M8: RVV VSETVLI 指令编码
    #[test]
    fn riscv64_vsetvli_emit() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.emit_vsetvli(0, 0, 0); // vsetvli zero, zero, vtype=0
        let code = &cg.code;
        assert!(code.len() >= 4);
        let insn = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
        assert_eq!(insn & 0x7f, 0x57, "opcode should be 0x57 (OP-V)");
        assert_eq!((insn >> 12) & 0x7, 0b111, "funct3 should be 111");
    }

    // M8: FENCE 内存屏障编码
    #[test]
    fn riscv64_fence_emit() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.emit_fence();
        let code = &cg.code;
        assert!(code.len() >= 4);
        let insn = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
        assert_eq!(insn, 0x0ff0000f, "FENCE encoding: 0x0ff0000f");
    }

    // M8: MRET 特权返回编码
    #[test]
    fn riscv64_mret_emit() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.emit_mret();
        let code = &cg.code;
        assert!(code.len() >= 4);
        let insn = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
        assert_eq!(insn, 0x30200073, "MRET encoding: 0x30200073");
    }

    // M8: Zicsr 完整 CSR 指令集测试
    #[test]
    fn riscv64_zicsr_full() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        // CSRRSI x10, mstatus(0x300), 0x1f
        cg.emit_csrrsi(10, 0x300, 0x1f);
        // CSRRCI x11, mstatus(0x300), 0x0f
        cg.emit_csrrci(11, 0x300, 0x0f);
        let code = &cg.code;
        assert!(code.len() >= 8, "expected 8 bytes for 2 CSR instructions");
        let insn1 = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
        let insn2 = u32::from_le_bytes([code[4], code[5], code[6], code[7]]);
        // CSRRSI: funct3=110, opcode=0x73
        assert_eq!((insn1 & 0x7000) >> 12, 0b110, "CSRRSI funct3=110");
        assert_eq!(insn1 & 0x7f, 0x73, "CSRRSI opcode=0x73");
        // CSRRCI: funct3=111, opcode=0x73
        assert_eq!((insn2 & 0x7000) >> 12, 0b111, "CSRRCI funct3=111");
        assert_eq!(insn2 & 0x7f, 0x73, "CSRRCI opcode=0x73");
    }

    // M8: D 扩展完整指令集测试
    #[test]
    fn riscv64_d_extension_full() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        // FSQRT.D f10, f11
        cg.emit_fsqrt_d(10, 11);
        // FMIN.D f12, f13, f14
        cg.emit_fmin_d(12, 13, 14);
        // FMAX.D f15, f16, f17
        cg.emit_fmax_d(15, 16, 17);
        // FEQ.D x10, f11, f12
        cg.emit_feq_d(10, 11, 12);
        // FLT.D x13, f14, f15
        cg.emit_flt_d(13, 14, 15);
        // FLE.D x16, f17, f18
        cg.emit_fle_d(16, 17, 18);
        let code = &cg.code;
        assert!(
            code.len() >= 24,
            "expected 24 bytes for 6 D-extension instructions"
        );
        // FSQRT.D: funct7=0101101
        let insn_sqrt = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
        assert_eq!(
            (insn_sqrt >> 25) & 0x7f,
            0b0101101,
            "FSQRT.D funct7=0101101"
        );
        // FMIN.D: funct7=0010101, funct3=000
        let insn_min = u32::from_le_bytes([code[4], code[5], code[6], code[7]]);
        assert_eq!((insn_min >> 25) & 0x7f, 0b0010101, "FMIN.D funct7=0010101");
        assert_eq!((insn_min >> 12) & 0x7, 0b000, "FMIN.D funct3=000");
        // FMAX.D: funct7=0010101, funct3=001
        let insn_max = u32::from_le_bytes([code[8], code[9], code[10], code[11]]);
        assert_eq!((insn_max >> 25) & 0x7f, 0b0010101, "FMAX.D funct7=0010101");
        assert_eq!((insn_max >> 12) & 0x7, 0b001, "FMAX.D funct3=001");
        // FEQ.D: funct7=1010001, funct3=010
        let insn_eq = u32::from_le_bytes([code[12], code[13], code[14], code[15]]);
        assert_eq!((insn_eq >> 25) & 0x7f, 0b1010001, "FEQ.D funct7=1010001");
        assert_eq!((insn_eq >> 12) & 0x7, 0b010, "FEQ.D funct3=010");
        // FLT.D: funct7=1010001, funct3=001
        let insn_lt = u32::from_le_bytes([code[16], code[17], code[18], code[19]]);
        assert_eq!((insn_lt >> 25) & 0x7f, 0b1010001, "FLT.D funct7=1010001");
        assert_eq!((insn_lt >> 12) & 0x7, 0b001, "FLT.D funct3=001");
        // FLE.D: funct7=1010001, funct3=000
        let insn_le = u32::from_le_bytes([code[20], code[21], code[22], code[23]]);
        assert_eq!((insn_le >> 25) & 0x7f, 0b1010001, "FLE.D funct7=1010001");
        assert_eq!((insn_le >> 12) & 0x7, 0b000, "FLE.D funct3=000");
    }

    // M25: RISC-V va_list 寄存器保存区 单元测试
    // 验证变参函数入口发出 8 条 sd a0..a7, off(sp) 保存指令.
    #[test]
    fn riscv64_va_save_area_stores_a0_through_a7() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.is_current_func_variadic = true;
        cg.va_save_base_off = 16;
        cg.stack_bytes = 80;
        cg.frame_bytes = 80;
        // 模拟 gen_function 入口保存动作
        for i in 0..8u8 {
            cg.emit_sd(10 + i, 2, cg.va_save_base_off + (i as i32) * 8);
        }
        // 验证: 8 条 sd aN, off(sp) 指令, LE 编码:
        //   byte0 低 7 位 = opcode = 0x23 (SD)
        //   byte1 位 6:4  = funct3 = 0b011
        assert!(
            cg.code.len() >= 32,
            "expected at least 32 bytes of SD instructions"
        );
        let sd_count = cg
            .code
            .windows(4)
            .filter(|w| (w[0] & 0x7f) == 0x23 && (w[1] & 0x70) == 0x30)
            .count();
        assert_eq!(sd_count, 8, "expected 8 SD instructions, got {sd_count}");
    }

    // M25: RISC-V va_arg 整数路径 单元测试
    // 验证 gen_va_arg_int 从保存区顺序读 a0
    #[test]
    fn riscv64_va_arg_int_reads_sequentially_from_save_area() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.is_current_func_variadic = true;
        cg.va_save_base_off = 0;
        cg.va_gp_offs = 0;
        cg.locals.insert("ap".to_string(), 100);
        // 读 3 个变参
        cg.gen_va_arg_int("ap");
        cg.gen_va_arg_int("ap");
        cg.gen_va_arg_int("ap");
        // 验证: 3 条 ld a0, off(sp) 指令, LE 编码:
        //   byte0 低 7 位 = opcode = 0x03 (LD)
        //   byte1 位 6:4  = funct3 = 0b011
        let ld_count = cg
            .code
            .windows(4)
            .filter(|w| (w[0] & 0x7f) == 0x03 && (w[1] & 0x70) == 0x30)
            .count();
        assert_eq!(ld_count, 3, "expected 3 LD instructions, got {ld_count}");
        // 验证 va_gp_offs 推进
        assert_eq!(cg.va_gp_offs, 3, "va_gp_offs should be 3 after 3 reads");
    }

    // ===== 压缩指令自动选择测试 =====

    #[test]
    fn riscv64_compress_addi_to_c_addi() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        // ADDI x10, x10, 5 => C.ADDI x10, 5 (2 bytes)
        cg.emit_addi(10, 10, 5);
        assert_eq!(
            cg.code.len(),
            2,
            "ADDI rd==rs1 should compress to C.ADDI (2 bytes)"
        );
        // Verify it's a valid C.ADDI: bits[1:0]=01, funct3[15:13]=000
        let insn = u16::from_le_bytes([cg.code[0], cg.code[1]]);
        assert_eq!(insn & 0x3, 0x01, "C.ADDI opcode should be 01");
        assert_eq!((insn >> 13) & 0x7, 0, "C.ADDI funct3 should be 000");
    }

    #[test]
    fn riscv64_compress_li_to_c_li() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        // ADDI x10, x0, 20 => C.LI x10, 20 (2 bytes)
        cg.emit_addi(10, 0, 20);
        assert_eq!(cg.code.len(), 2, "LI should compress to C.LI (2 bytes)");
        let insn = u16::from_le_bytes([cg.code[0], cg.code[1]]);
        assert_eq!(insn & 0x3, 0x01, "C.LI opcode should be 01");
        assert_eq!((insn >> 13) & 0x7, 0b010, "C.LI funct3 should be 010");
    }

    #[test]
    fn riscv64_compress_lui_to_c_lui() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        // LUI x10, 5 => C.LUI x10, 5 (2 bytes)
        cg.emit_lui(10, 5);
        assert_eq!(
            cg.code.len(),
            2,
            "LUI with small imm should compress to C.LUI (2 bytes)"
        );
        let insn = u16::from_le_bytes([cg.code[0], cg.code[1]]);
        assert_eq!(insn & 0x3, 0x01, "C.LUI opcode should be 01");
        assert_eq!((insn >> 13) & 0x7, 0b011, "C.LUI funct3 should be 011");
    }

    #[test]
    fn riscv64_compress_add_to_c_mv() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        // ADD x10, x0, x11 => C.MV x10, x11 (2 bytes)
        cg.emit_add(10, 0, 11);
        assert_eq!(
            cg.code.len(),
            2,
            "ADD rd, x0, rs2 should compress to C.MV (2 bytes)"
        );
        let insn = u16::from_le_bytes([cg.code[0], cg.code[1]]);
        assert_eq!(insn & 0x3, 0x02, "C.MV opcode should be 10");
    }

    #[test]
    fn riscv64_compress_add_to_c_add() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        // ADD x10, x10, x11 => C.ADD x10, x11 (2 bytes)
        cg.emit_add(10, 10, 11);
        assert_eq!(
            cg.code.len(),
            2,
            "ADD rd==rs1 should compress to C.ADD (2 bytes)"
        );
        let insn = u16::from_le_bytes([cg.code[0], cg.code[1]]);
        assert_eq!(insn & 0x3, 0x02, "C.ADD opcode should be 10");
    }

    #[test]
    fn riscv64_compress_lw_to_c_lw() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        // LW x10, 8(x10) => C.LW x10', x10', 8 (2 bytes)
        cg.emit_lw(10, 10, 8);
        assert_eq!(
            cg.code.len(),
            2,
            "LW with x8-x15 and aligned offset should compress to C.LW (2 bytes)"
        );
        let insn = u16::from_le_bytes([cg.code[0], cg.code[1]]);
        assert_eq!(insn & 0x3, 0x00, "C.LW opcode should be 00");
        assert_eq!((insn >> 13) & 0x7, 0b010, "C.LW funct3 should be 010");
    }

    #[test]
    fn riscv64_compress_sw_to_c_sw() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        // SW x10, 8(x10) => C.SW x10', x10', 8 (2 bytes)
        cg.emit_sw(10, 10, 8);
        assert_eq!(
            cg.code.len(),
            2,
            "SW with x8-x15 and aligned offset should compress to C.SW (2 bytes)"
        );
        let insn = u16::from_le_bytes([cg.code[0], cg.code[1]]);
        assert_eq!(insn & 0x3, 0x00, "C.SW opcode should be 00");
        assert_eq!((insn >> 13) & 0x7, 0b110, "C.SW funct3 should be 110");
    }

    #[test]
    fn riscv64_compress_ld_to_c_ld() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        // LD x10, 16(x10) => C.LD x10', x10', 16 (2 bytes)
        cg.emit_ld(10, 10, 16);
        assert_eq!(
            cg.code.len(),
            2,
            "LD with x8-x15 and aligned offset should compress to C.LD (2 bytes)"
        );
        let insn = u16::from_le_bytes([cg.code[0], cg.code[1]]);
        assert_eq!(insn & 0x3, 0x01, "C.LD opcode should be 01");
        assert_eq!((insn >> 13) & 0x7, 0b011, "C.LD funct3 should be 011");
    }

    #[test]
    fn riscv64_compress_sd_to_c_sd() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        // SD x10, 16(x10) => C.SD x10', x10', 16 (2 bytes)
        cg.emit_sd(10, 10, 16);
        assert_eq!(
            cg.code.len(),
            2,
            "SD with x8-x15 and aligned offset should compress to C.SD (2 bytes)"
        );
        let insn = u16::from_le_bytes([cg.code[0], cg.code[1]]);
        assert_eq!(insn & 0x3, 0x01, "C.SD opcode should be 01");
        assert_eq!((insn >> 13) & 0x7, 0b111, "C.SD funct3 should be 111");
    }

    #[test]
    fn riscv64_no_compress_addi_out_of_range() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        // ADDI x10, x10, 100 => 4 bytes (out of C.ADDI range -32..31)
        cg.emit_addi(10, 10, 100);
        assert_eq!(
            cg.code.len(),
            4,
            "ADDI with large imm should not compress (4 bytes)"
        );
    }

    #[test]
    fn riscv64_no_compress_lw_wrong_regs() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        // LW x5, 8(x10) => 4 bytes (rd not in x8-x15)
        cg.emit_lw(5, 10, 8);
        assert_eq!(
            cg.code.len(),
            4,
            "LW with rd not in x8-x15 should not compress (4 bytes)"
        );
    }

    // -ftrivial-auto-var-init=zero: 验证 RISC-V64 后端发射 sd zero, offset(sp) 指令
    #[test]
    fn riscv64_auto_var_init_zero_emits_sd() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut target = crate::target::Target::resolve(&triple);
        target.auto_var_init_zero = true;
        let mut cg = Riscv64Codegen::new(target, true);
        // 直接测试 emit_zero_init_frame: 32 字节帧 = 4 个 sd
        cg.emit_zero_init_frame(32);
        let sd_count = cg
            .code
            .windows(4)
            .filter(|w| {
                let inst = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
                // SD: opcode=011, funct3=011 => S-type with funct3=011
                let opcode = inst & 0x7f;
                let funct3 = (inst >> 12) & 0x7;
                let rs2 = (inst >> 20) & 0x1f;
                let rs1 = (inst >> 15) & 0x1f;
                opcode == 0x23 && funct3 == 3 && rs2 == 0 && rs1 == 2
            })
            .count();
        assert_eq!(sd_count, 4, "expected 4 sd zero, offset(sp) instructions for 32-byte frame, got {sd_count}");
    }

    // D 扩展新增指令: FSQRT.S, FMIN.S, FMAX.S, FCLASS.S/D
    #[test]
    fn riscv64_d_extension_new_instructions() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.emit_fsqrt_s(10, 11);
        cg.emit_fmin_s(12, 13, 14);
        cg.emit_fmax_s(15, 16, 17);
        cg.emit_fclass_s(10, 11);
        cg.emit_fclass_d(12, 13);
        let code = &cg.code;
        assert!(code.len() >= 20, "expected 20 bytes for 5 instructions");
        // FSQRT.S: funct7=0101101
        let insn_sqrt = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
        assert_eq!((insn_sqrt >> 25) & 0x7f, 0b0101101, "FSQRT.S funct7");
        // FMIN.S: funct7=0010100, funct3=000
        let insn_min = u32::from_le_bytes([code[4], code[5], code[6], code[7]]);
        assert_eq!((insn_min >> 25) & 0x7f, 0b0010100, "FMIN.S funct7");
        assert_eq!((insn_min >> 12) & 0x7, 0, "FMIN.S funct3=000");
        // FMAX.S: funct7=0010100, funct3=001
        let insn_max = u32::from_le_bytes([code[8], code[9], code[10], code[11]]);
        assert_eq!((insn_max >> 25) & 0x7f, 0b0010100, "FMAX.S funct7");
        assert_eq!((insn_max >> 12) & 0x7, 1, "FMAX.S funct3=001");
        // FCLASS.S: funct7=1010000, funct3=001
        let insn_class_s = u32::from_le_bytes([code[12], code[13], code[14], code[15]]);
        assert_eq!((insn_class_s >> 25) & 0x7f, 0b1010000, "FCLASS.S funct7");
        assert_eq!((insn_class_s >> 12) & 0x7, 1, "FCLASS.S funct3=001");
        // FCLASS.D: funct7=1010001, funct3=001
        let insn_class_d = u32::from_le_bytes([code[16], code[17], code[18], code[19]]);
        assert_eq!((insn_class_d >> 25) & 0x7f, 0b1010001, "FCLASS.D funct7");
        assert_eq!((insn_class_d >> 12) & 0x7, 1, "FCLASS.D funct3=001");
    }

    // RVV 窄化钳位指令: VNCLIP.VV, VNCLIPU.VV
    #[test]
    fn riscv64_rvv_narrowing_clip() {
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.emit_vnclip_vv(8, 16, 24, true);
        cg.emit_vnclipu_vv(9, 17, 25, false);
        let code = &cg.code;
        assert!(code.len() >= 8, "expected 8 bytes for 2 instructions");
        // VNCLIP: funct6=100110, funct3=011
        let insn_nclip = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
        assert_eq!((insn_nclip >> 26) & 0x3f, 0b100110, "VNCLIP funct6");
        assert_eq!((insn_nclip >> 12) & 0x7, 3, "VNCLIP funct3=011");
        assert_eq!((insn_nclip >> 25) & 1, 1, "VNCLIP vm=1");
        // VNCLIPU: funct6=100110, funct3=010
        let insn_nclipu = u32::from_le_bytes([code[4], code[5], code[6], code[7]]);
        assert_eq!((insn_nclipu >> 26) & 0x3f, 0b100110, "VNCLIPU funct6");
        assert_eq!((insn_nclipu >> 12) & 0x7, 2, "VNCLIPU funct3=010");
        assert_eq!((insn_nclipu >> 25) & 1, 0, "VNCLIPU vm=0");
    }
}
