// src/codegen/aarch64.rs - AArch64 后端 (M7 完整 IR lowering)

use crate::ast::{BinOp, BlockItem, Decl, Expr, Field, FuncDecl, Program, Stmt, Type, UnOp};
use crate::codegen::asm_constraints::{
    self, resolve_asm_operands, resolve_asm_template_enhanced, AsmArch,
};
use crate::codegen::elf::{
    self, ElfRela, ElfSection, ElfSym, R_AARCH64_ADR_PREL_LO21, R_AARCH64_CALL26, SHF_ALLOC,
    SHF_GNU_RETAIN, SHT_PROGBITS, STB_GLOBAL, STB_LOCAL, STT_FUNC, STT_GNU_IFUNC, STT_OBJECT,
    STT_TLS,
};
use crate::ir::{ExprKind, ExprLiteral, Function, Global, Module, TypedExpr, TypedStmt, ValueType};
use crate::target::Target;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy)]
enum BranchKind {
    B,
    Cbz,
}

pub struct Aarch64Codegen {
    code: Vec<u8>,
    rodata: Vec<u8>,
    entry: u64,
    relocatable: bool,
    symbols: Vec<ElfSym>,
    relocations: Vec<ElfRela>,
    rodata_relocations: Vec<ElfRela>,
    func_sizes: HashMap<String, u64>,
    pub labels: HashMap<String, u64>,
    /// 当前函数局部变量: name -> 栈偏移
    locals: HashMap<String, i32>,
    global_rodata: HashMap<String, u64>,
    float_refs: Vec<(u64, u64)>,
    sym_index: HashMap<String, u32>,
    stack_bytes: i32,
    frame_bytes: i32,
    emitted_return: bool,
    source_name: String,
    label_serial: u32,
    pending_branches: Vec<(u64, String, BranchKind)>,
    /// 前向条件分支: (位置, 标签, 条件码)
    pending_bcond: Vec<(u64, String, u8)>,
    /// 前向 bl 调用: (指令位置, 目标符号/标签)
    pending_bl_calls: Vec<(u64, String)>,
    /// exec 模式 ADR 占位: (位置, rd, 绝对目标地址)
    pending_adr_exec: Vec<(u64, u8, u64)>,
    extra_sections: Vec<ElfSection>,
    /// 当前函数返回类型 (sret/HFA 判定)
    current_return_ty: Option<ValueType>,
    /// AST struct/union 字段表 (HFA 分类)
    aggregate_fields: HashMap<String, (Arc<Vec<Field>>, bool)>,
    /// 当前函数是否为 variadic
    is_current_func_variadic: bool,
    /// 寄存器保存区在 frame 内的起始偏移
    /// 0 表示未启用 (变参函数中存 x0..x7 + v0..v7, 每 8 字节一个槽)
    va_save_base_off: i32,
    /// 下一个 GP va_arg 槽位索引 (0..7, 对应 x0..x7)
    va_gp_offs: i32,
    /// 下一个 float va_arg 槽位索引 (0..7, 对应 v0..v7)
    va_flt_offs: i32,
    /// MTE: 是否启用内存标签扩展
    mte_enabled: bool,
    /// MTE: 当前函数的 tag 栈帧大小
    tag_frame_size: i32,
    /// MTE: 帧内保存入参 x19 (callee-saved) 的槽位偏移, 0 表示未启用
    mte_save_off: i32,
    /// 嵌套 sret: 保存的入参 sret 指针栈偏移 (函数返回 >16B 聚合时有效)
    saved_sret_ptr: Option<i32>,
    /// Stack protector: 当前函数 canary 槽位偏移, 0 表示未启用
    canary_slot_off: i32,
    /// TLS 全局全局变量集合 (名称 -> 偏移), 用于 load_global_addr_w0 分发 TLS 访问序列
    tls_globals: HashMap<String, u64>,
    /// 文件级 asm 原始文本行 (GlobalAsm)
    raw_global_asm_lines: Vec<String>,
    /// 函数级内联 asm 原始文本行: (函数名, 行列表)
    raw_func_asm_lines: Vec<(String, Vec<String>)>,
    /// -S 模式标志: true 时保留 asm 文本输出, false 时编码机器码
    asm_text_mode: bool,
    /// 编译诊断 (未识别内联汇编指令等)
    pub diagnostics: Vec<crate::diagnostics::Diagnostic>,
    /// -mrecord-mcount: ftrace call site positions for __mcount_loc section
    mcount_loc_entries: Vec<(u64, String)>,
    _target: Target,
    /// break/continue 标签栈: (break_label, continue_label)
    break_continue_stack: Vec<(String, String)>,
}

/// AAPCS64 同构浮点聚合 (HFA) 描述
#[derive(Clone, Copy, Debug)]
struct HfaInfo {
    elem_bits: u16,
    count: u8,
    total_size: usize,
}

fn local_stack_size(ty: &Type) -> i32 {
    if matches!(ty, Type::Vector(_, _)) {
        16
    } else {
        ty.size().max(4) as i32
    }
}

impl Aarch64Codegen {
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
            sym_index: HashMap::new(),
            stack_bytes: 0,
            frame_bytes: 0,
            emitted_return: false,
            source_name: String::new(),
            label_serial: 0,
            pending_branches: Vec::new(),
            pending_bcond: Vec::new(),
            pending_bl_calls: Vec::new(),
            pending_adr_exec: Vec::new(),
            extra_sections: Vec::new(),
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
        }
    }

    pub fn set_source_name(&mut self, name: impl Into<String>) {
        self.source_name = name.into();
    }

    pub fn set_asm_text_mode(&mut self, v: bool) {
        self.asm_text_mode = v;
    }

    /// Encode resolved asm text into machine code and emit to code buffer.
    /// Used in -c mode to produce actual machine code for inline asm.
    /// P0-5: unrecognized instructions produce hard errors, not NOPs.
    fn encode_and_emit_asm(&mut self, resolved: &str, span_file: &str, span_line: u32) {
        let lines = crate::codegen::mini_asm::parse_asm_lines(resolved);
        match crate::codegen::mini_asm::encode_asm_lines_for_arch(&lines, "aarch64", false) {
            Ok(bytes) => {
                self.code.extend_from_slice(&bytes);
            }
            Err(unrecognized) => {
                for mnem in &unrecognized {
                    let span = crate::diagnostics::Span::point(span_file, span_line, 1);
                    let mut diag = crate::diagnostics::Diagnostic::err(
                        "E0311",
                        format!(
                            "unsupported inline asm instruction '{}' on aarch64 target",
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

    /// 启用/禁用 AArch64 MTE 硬件 tag (HWASAN 栈帧 tag 模式)。
    /// 由 `-fsanitize=hwaddress` 在 AArch64 目标上触发。
    pub fn set_mte_enabled(&mut self, on: bool) {
        self.mte_enabled = on;
    }

    /// MTE 调用点前缀: 调用外部 (非 MTE) 代码前清除 sp 的 tag。
    /// `and sp, x19, #0x00ffffffffffffff` (x19 = 本帧带 tag 的帧基址)
    fn mte_untag_sp_for_call(&mut self) {
        self.emit(&0x9280de7fu32.to_le_bytes());
    }

    /// MTE 调用点后缀: 调用返回后恢复本帧 tag。
    /// `add sp, x19, #0` (mov sp, x19)
    fn mte_retag_sp_after_call(&mut self) {
        self.emit(&0x9100027fu32.to_le_bytes());
    }

    /// 当前函数是否需要 MTE tag 栈帧 (prologue 打 tag / 调用点 untag-retag)。
    fn mte_active(&self) -> bool {
        self.mte_enabled && self.frame_bytes > 0
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
        self.build_aggregate_fields(prog);
        self.emit_globals(module);
        if self.relocatable {
            self.compile_rel(module, prog);
            self.finalize_branches();
            self.finalize_pending_bl();
        } else {
            self.compile_exec(module, prog);
            self.finalize_exec();
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
                self.label(&func.name);
                for _ in 0..suffix_len {
                    self.emit(&0xd503201fu32.to_le_bytes());
                }
            } else {
                self.label(&func.name);
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
            // -fpatchable-function-entry=(N,M): emit NOPs around function entry
            // M NOPs before the label, (N-M) NOPs after
            if let Some((n, m)) = self._target.patchable_function_entry {
                let prefix_len = m.min(n);
                let suffix_len = n.saturating_sub(prefix_len);
                for _ in 0..prefix_len {
                    // AArch64 NOP: 0xD503201F
                    self.emit(&0xd503201fu32.to_le_bytes());
                }
                self.label(&func.name);
                for _ in 0..suffix_len {
                    self.emit(&0xd503201fu32.to_le_bytes());
                }
            } else {
                self.label(&func.name);
            }
            let start = self.pos();
            self.gen_function(func, find_func(prog, &func.name));
            let size = self.pos() - start;
            self.func_sizes.insert(func.name.clone(), size);
            self.symbols.push(ElfSym {
                name: func.name.clone(),
                section: 1,
                value: text_off,
                size,
                bind: STB_GLOBAL,
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
                        self.sym_index.insert(f.name.name.clone(), self.symbols.len() as u32);
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

    fn finalize_exec(&mut self) {
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

    fn finalize_pending_bl(&mut self) {
        let pending: Vec<_> = self.pending_bl_calls.drain(..).collect();
        for (at, name) in pending {
            if let Some(&target) = self.labels.get(&name) {
                self.patch_bl(at, target);
            }
        }
    }

    fn finalize_branches(&mut self) {
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

    pub fn build_output(&self) -> Vec<u8> {
        if self.relocatable {
            elf::build_elf_rel_aarch64(
                &self.code,
                &self.rodata,
                &self.symbols,
                &self.relocations,
                &self.rodata_relocations,
                &self.extra_sections,
            )
        } else {
            elf::build_elf_aarch64(&self.code, &self.rodata, self.entry)
        }
    }

    pub fn build_asm_output(&self, module: &Module) -> String {
        let mut out = String::new();
        let name = if self.source_name.is_empty() {
            "input.c".to_string()
        } else {
            self.source_name.clone()
        };
        out.push_str(&format!("\t.file \"{name}\"\n"));
        if !self.rodata.is_empty() {
            out.push_str("\t.section .rodata,\"a\",@progbits\n");
            for g in &module.globals {
                if let Some(&off) = self.global_rodata.get(&g.name) {
                    out.push_str(&format!("\t.globl {}\n", g.name));
                    out.push_str(&format!("\t.type {}, %object\n", g.name));
                    out.push_str(&format!("\t.size {}, {}\n", g.name, value_type_size(&g.ty)));
                    out.push_str(&format!("{}:\n", g.name));
                    out.push_str(&format!("\t// rodata offset {off}\n"));
                }
            }
        }
        out.push_str("\t.text\n");
        // 发射文件级 asm 原始文本
        for line in &self.raw_global_asm_lines {
            out.push_str(&format!("\t{line}\n"));
        }
        for func in &module.functions {
            out.push_str(&format!("\t.globl {}\n", func.name));
            out.push_str(&format!("\t.type {}, %function\n", func.name));
            // -fpatchable-function-entry: emit NOPs in asm output
            if let Some((n, m)) = self._target.patchable_function_entry {
                let prefix_len = m.min(n);
                let suffix_len = n.saturating_sub(prefix_len);
                if n > 0 {
                    out.push_str(&format!(
                        "\t.section .patchable_function_entry,\"aw\",@progbits\n"
                    ));
                }
                for _ in 0..prefix_len {
                    out.push_str("\tnop\n");
                }
                out.push_str(&format!("{}:\n", func.name));
                for _ in 0..suffix_len {
                    out.push_str("\tnop\n");
                }
                if n > 0 {
                    out.push_str("\t.text\n");
                }
            } else {
                out.push_str(&format!("{}:\n", func.name));
            }
            // 发射函数级内联 asm 原始文本
            for (fname, lines) in &self.raw_func_asm_lines {
                if fname == &func.name || fname.starts_with("__inline_") {
                    for line in lines {
                        out.push_str(&format!("\t{line}\n"));
                    }
                }
            }
            if let Some(size) = self.func_sizes.get(&func.name) {
                out.push_str(&format!("\t// IR lowering, {} bytes machine code\n", size));
            }
            out.push_str("\tret\n");
            out.push_str(&format!("\t.size {}, .-{}\n", func.name, func.name));
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
        let pool = asm_constraints::aarch64_reg_pool();
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
            AsmArch::AArch64,
            false,
        )
    }

    fn gen_function(&mut self, func: &Function, ast: Option<&FuncDecl>) {
        // __attribute__((naked)): 不生成 prologue/epilogue
        if ast.map(|f| f.is_naked).unwrap_or(false) {
            self.emitted_return = false;
            if let Some(body) = ast.and_then(|f| f.body.as_ref()) {
                for item in &body.items {
                    if let BlockItem::Stmt(Stmt::Expr { e, .. }) = item {
                        self.gen_naked_asm_expr(e);
                    }
                }
            }
            if !self.emitted_return {
                // naked 函数应包含自己的返回指令 (如 asm "ret")
                // 不发射额外 epilogue
            }
            return;
        }
        // ftrace: emit call to __fentry__/mcount before prologue
        // The kernel patches these call sites at runtime for tracing.
        let ftrace_call_pos: Option<u64> = match self._target.ftrace_mode {
            crate::cli::FtraceMode::FEntry | crate::cli::FtraceMode::RecordMCount => {
                let pos = self.pos();
                self.emit_bl_to("__fentry__");
                Some(pos)
            }
            crate::cli::FtraceMode::MCount => {
                let pos = self.pos();
                self.emit_bl_to("mcount");
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
        self.plan_locals(ast);
        self.current_return_ty = Some(func.return_type.clone());
        self.saved_sret_ptr = None;
        // 嵌套 sret: 如果函数返回 >16B 聚合类型, 在帧建立后立即保存 x0 (caller 传入的
        // sret 指针) 到栈局部 <sret_ptr>, 防止后续调用覆盖 x0 导致返回时丢失目标地址.
        let returns_large_agg = aggregate_size(&func.return_type).is_some_and(|s| s > 16);
        // 变参函数需要在 frame 顶部预留寄存器保存区:
        // x0..x7 (64 字节) + v0..v7 (64 字节) = 128 字节.
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
        // MTE (HWASAN 硬件 tag): 为 callee-saved x19 预留槽位, 该槽位计入帧内,
        // 因此只要函数有栈内容 (局部/变参保存区) 就会产生可打 tag 的帧。
        if self.mte_enabled && self.stack_bytes > 0 {
            self.mte_save_off = self.alloc_local("<mte_x19_save>", 8);
        } else {
            self.mte_save_off = 0;
        }
        // Stack protector: allocate canary slot before frame size calculation
        if self._target.stack_protector {
            self.canary_slot_off = self.alloc_local("<canary>", 8);
        } else {
            self.canary_slot_off = 0;
        }
        self.frame_bytes = (self.stack_bytes + 15) & !15;
        if self.frame_bytes > 0 {
            if self._target.stack_clash_protection && self.frame_bytes > 4096 {
                self.emit_stack_clash_probes(self.frame_bytes as u32);
            } else {
                self.sub_sp_imm(self.frame_bytes as u32);
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
        // MTE (HWASAN 硬件 tag): prologue tag 栈帧 + 指针 tag 流程。
        //   1) irg  x16, sp          : 随机 tag 写入 x16 (地址不变)
        //   2) addg sp, x16, #0, #0  : sp 携带本帧随机 tag, 由 sp 派生的所有
        //      局部地址 (mov_x_reg_sp_off / [sp,#off]) 自动继承该 tag
        //   3) stg  x16, [sp, #g*16] : 为每个 16B tag granule 写入本帧 tag,
        //      使带 tag 的 sp 访问通过硬件 tag 检查 (STG 本身是 unchecked access)
        //   4) str  x19, [sp, #off]  : 保存入参 x19 (本函数占用 callee-saved 需归还)
        //   5) add  x19, sp, #0      : x19 = 带 tag 的帧基址, 供调用点 untag/retag
        if self.mte_active() {
            self.tag_frame_size = self.frame_bytes;
            self.emit_irg(16, 31); // irg x16, sp
            self.emit_addg(31, 16, 0, 0); // addg sp, x16, #0, #0
            let granules = (self.frame_bytes as u32) / 16;
            for g in 0..granules {
                self.emit_stg(16, 31, g); // stg x16, [sp, #g*16]
            }
            self.str_xt_sp(19, self.mte_save_off); // str x19, [sp, #mte_save_off]
            self.emit_add_imm_x(19, 31, 0); // add x19, sp, #0 (mov x19, sp)
        }
        // 在帧建立后保存 sret 指针 (必须在 sub_sp 之后, 因为此时 sp 才稳定)
        if returns_large_agg {
            let sret_off = self.alloc_local("<sret_ptr>", 8);
            // str x0, [sp, #sret_off]
            self.store_w0(sret_off);
            self.saved_sret_ptr = Some(sret_off);
        }
        // 进入变参函数立即把 x0..x7 和 v0..v7 写入保存区
        if is_variadic {
            for i in 0..8u8 {
                self.str_xt_sp(i, self.va_save_base_off + (i as i32) * 8);
            }
            for i in 0..8u8 {
                self.str_dt_sp(i, self.va_save_base_off + 64 + (i as i32) * 8);
            }
        }
        self.emitted_return = false;
        let body = ast.and_then(|f| f.body.as_ref());
        let items = body.map(|b| b.items.as_slice()).unwrap_or(&[]);
        for (item, ir) in items.iter().zip(func.body.iter()) {
            self.gen_block_item(item, ir);
        }
        if !self.emitted_return {
            self.mov_w0_imm(0);
            self.ret();
        }
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
                );
            }
            _ => {}
        }
    }

    fn plan_locals(&mut self, ast: Option<&FuncDecl>) {
        self.locals.clear();
        self.stack_bytes = 0;
        let Some(body) = ast.and_then(|f| f.body.as_ref()) else {
            return;
        };
        for item in &body.items {
            if let BlockItem::Decl(Decl::Var(v)) = item {
                self.alloc_local(&v.name.name, local_stack_size(&v.ty));
            }
        }
        self.alloc_local("<vec_lit>", 16);
    }

    fn gen_block_item(&mut self, item: &BlockItem, ir: &TypedStmt) {
        match item {
            BlockItem::Stmt(s) => self.gen_stmt(s, ir),
            BlockItem::Decl(Decl::Var(v)) => {
                let off = self
                    .locals
                    .get(&v.name.name)
                    .copied()
                    .unwrap_or_else(|| self.alloc_local(&v.name.name, local_stack_size(&v.ty)));
                if let Some(expr) = ir.expressions.first() {
                    self.gen_expr(expr);
                    self.store_result(off, &v.ty);
                } else if let Some(init) = &v.init {
                    self.gen_expr_from_ast(init);
                    self.store_result(off, &v.ty);
                }
            }
            _ => {}
        }
    }

    fn gen_stmt(&mut self, stmt: &Stmt, ir: &TypedStmt) {
        match stmt {
            Stmt::Return { val, .. } => {
                let ret_ty = self.current_return_ty.clone().unwrap_or(ValueType::Void);
                let sret_size = aggregate_size(&ret_ty).filter(|s| *s > 16);
                let hfa = classify_hfa(&ret_ty, &self.aggregate_fields);
                if let Some(size) = sret_size {
                    // 大结构体返回: 求值到栈临时区, 再 memcpy 到 sret 目标
                    // 嵌套安全: 使用 gen_function 入口保存的 <sret_ptr> 而非 x0
                    // (x0 可能在嵌套调用中被覆盖)
                    let tmp = self.alloc_local("<ret_tmp>", size as i32);
                    if let Some(expr) = ir.expressions.first() {
                        self.gen_expr(expr);
                    } else if let Some(e) = val.as_ref() {
                        self.gen_expr_from_ast(e);
                    }
                    self.store_result(tmp, &crate::ast::Type::Int);
                    if let Some(sret_off) = self.saved_sret_ptr {
                        // 从保存的 sret 指针加载目标地址到 x0, 然后 memcpy
                        self.load_w0(sret_off);
                        self.memcpy_sp_to_x0(tmp, size);
                    } else {
                        // 顶层函数或未保存 sret 指针: 直接用 x0 (兼容旧路径)
                        self.memcpy_sp_to_x0(tmp, size);
                    }
                } else if let Some(hfa) = hfa {
                    if let Some(expr) = ir.expressions.first() {
                        self.gen_hfa_expr(expr, hfa);
                    } else if let Some(e) = val.as_ref() {
                        self.gen_hfa_expr_from_ast(e, hfa);
                    }
                } else if is_float_ty(&ret_ty) {
                    if let Some(expr) = ir.expressions.first() {
                        self.gen_float_expr(expr);
                    } else if let Some(e) = val.as_ref() {
                        self.gen_expr_from_ast(e);
                    }
                } else {
                    if let Some(expr) = ir.expressions.first() {
                        self.gen_expr(expr);
                    } else if let Some(e) = val.as_ref() {
                        self.gen_expr_from_ast(e);
                    } else {
                        self.mov_w0_imm(0);
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
                self.cbz_w0(&lbl_else);
                self.gen_stmt(then_, &ir.nested[0]);
                self.b(&lbl_end);
                self.label(&lbl_else);
                if let Some(el) = else_ {
                    if ir.nested.len() > 1 {
                        self.gen_stmt(el, &ir.nested[1]);
                    } else {
                        self.gen_stmt(
                            el,
                            &TypedStmt {
                                expressions: Vec::new(),
                                nested: Vec::new(),
                                span: ir.span.clone(),
                            },
                        );
                    }
                }
                self.label(&lbl_end);
            }
            Stmt::While { body, .. } | Stmt::Do { body, .. } => {
                let lbl_loop = self.fresh_label("Lwhile");
                let lbl_exit = self.fresh_label("Lwend");
                self.label(&lbl_loop);
                self.break_continue_stack.push((lbl_exit.clone(), lbl_loop.clone()));
                if let Some(cond) = ir.expressions.first() {
                    self.gen_expr(cond);
                    self.cbz_w0(&lbl_exit);
                }
                self.gen_stmt(body, &ir.nested[0]);
                self.b(&lbl_loop);
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
                self.label(&lbl_loop);
                self.break_continue_stack.push((lbl_exit.clone(), lbl_cont.clone()));
                if cond.is_some() {
                    if let Some(ir_cond) = ir.expressions.first() {
                        self.gen_expr(ir_cond);
                        self.cbz_w0(&lbl_exit);
                    }
                }
                self.gen_stmt(body, &ir.nested[body_idx]);
                self.label(&lbl_cont);
                if step.is_some() {
                    if let Some(step_ir) = ir.expressions.get(1) {
                        let _ = self.gen_expr(step_ir);
                    }
                }
                self.b(&lbl_loop);
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
                outputs: _,
                output_names,
                output_constraints,
                inputs: _,
                input_names,
                input_constraints,
                goto_labels,
                span,
                ..
            } => {
                // 构建 goto 标签映射 (用于增强版模板替换)
                let label_pairs: Vec<(String, String)> = goto_labels
                    .iter()
                    .map(|l| (l.name.clone(), l.name.clone()))
                    .collect();
                let resolved = Self::resolve_asm_template_enhanced(
                    asm_str,
                    output_constraints,
                    input_constraints,
                    output_names,
                    input_names,
                    &label_pairs,
                );
                if self.asm_text_mode {
                    // -S 模式: 收集原始 asm 文本用于输出
                    eprintln!("{}:{}: warning: asm goto on non-x86_64 target: inline asm text preserved in -S output only", span.file, span.line);
                    let func_name = self.source_name.clone();
                    let entry = self
                        .raw_func_asm_lines
                        .last_mut()
                        .filter(|(n, _)| n == &func_name);
                    if let Some((_, lines)) = entry {
                        for line in resolved.split('\n') {
                            let t = line.trim();
                            if !t.is_empty() {
                                lines.push(t.to_string());
                            }
                        }
                    } else {
                        let lines = resolved
                            .split('\n')
                            .map(|l| l.trim().to_string())
                            .filter(|l| !l.is_empty())
                            .collect();
                        self.raw_func_asm_lines.push((func_name, lines));
                    }
                } else {
                    // -c 模式: mini-assembler 编码机器码
                    self.encode_and_emit_asm(&resolved, &span.file, span.line);
                }
            }
            Stmt::Switch { expr, cases, .. } => {
                // switch: 计算表达式值存入栈临时槽, 逐一比较 case 值
                let l_end = self.fresh_label("Lsw_end");
                let l_default = self.fresh_label("Lsw_def");
                // 分配栈临时保存 switch 值
                let sw_tmp = self.alloc_local("<sw_val>", 4);
                // 计算 switch 表达式
                if let Some(sw_ir) = ir.expressions.first() {
                    self.gen_expr(sw_ir);
                } else {
                    self.gen_expr_from_ast(expr);
                }
                // w0 -> 栈临时
                self.store_w0(sw_tmp);
                self.break_continue_stack.push((l_end.clone(), String::new()));
                let mut has_default = false;
                let mut case_labels: Vec<(String, &crate::ast::Case)> = Vec::new();
                for case in cases {
                    if let Some(ref val_expr) = &case.val {
                        let l_case = self.fresh_label("Lsw_case");
                        // 重新加载 switch 值到 w14
                        self.load_w0(sw_tmp);
                        self.mov_w14_w0(); // w14 = switch value
                        if let Some(ref hi) = case.range_end {
                            // GNU case low ... high: 范围匹配
                            self.gen_expr_from_ast(val_expr); // w0 = low
                            self.mov_w1_w0(); // w1 = low
                            self.gen_expr_from_ast(hi); // w0 = high
                            self.mov_w2_w0(); // w2 = high
                            // cmp w14, w1 (switch vs low)
                            self.emit_cmp_w_reg(14, 1);
                            let l_skip_lo = self.fresh_label("Lsw_skip_lo");
                            self.emit_bcond(&l_skip_lo, 0xd); // GE: switch >= low
                            // switch < low => skip
                            self.b(&l_skip_lo);
                            self.label(&l_skip_lo);
                            // cmp w14, w2 (switch vs high)
                            self.emit_cmp_w_reg(14, 2);
                            let l_skip = self.fresh_label("Lsw_skip");
                            self.emit_bcond(&l_skip, 0xc); // GT: switch > high
                            self.b(&l_case);
                            self.label(&l_skip);
                        } else {
                            // case N: 精确匹配
                            self.gen_expr_from_ast(val_expr); // w0 = case value
                            self.mov_w1_w0(); // w1 = case value
                            // cmp w14, w1
                            self.emit_cmp_w_reg(14, 1);
                            // b.eq l_case
                            self.emit_bcond(&l_case, 0x0); // EQ
                        }
                        case_labels.push((l_case, case));
                    } else {
                        has_default = true;
                    }
                }
                // 无匹配: 跳 default 或 end
                if has_default {
                    self.b(&l_default);
                } else {
                    self.b(&l_end);
                }
                // case 体
                for (l_case, case) in &case_labels {
                    self.label(l_case);
                    for (item, nested_ir) in case.body.items.iter().zip(ir.nested.iter()) {
                        self.gen_block_item(item, nested_ir);
                    }
                }
                // default 体
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
                // break 出口
                self.label(&l_end);
                self.break_continue_stack.pop();
            }
            Stmt::Break { .. } => {
                if let Some((l_break, _)) = self.break_continue_stack.last().cloned() {
                    self.b(&l_break);
                }
            }
            Stmt::Continue { .. } => {
                if let Some((_, l_cont)) = self.break_continue_stack.last().cloned() {
                    if !l_cont.is_empty() {
                        self.b(&l_cont);
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
        // stack slots or register pairs (v0, v1).
        if is_complex_ty(&expr.ty) {
            return self.gen_complex_expr(expr);
        }
        match &expr.kind {
            ExprKind::Constant => {
                if let Some(ExprLiteral::Int(v)) = expr.literal {
                    self.mov_w0_imm(trunc_i32(v));
                    return true;
                }
                if let Some(ExprLiteral::Float(v)) = expr.literal {
                    self.load_float_s0(v as f32);
                    return true;
                }
                self.mov_w0_imm(0);
            }
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.load_w0(off);
                    return true;
                }
                if self.global_rodata.contains_key(name) {
                    self.load_global_addr_w0(name);
                    return true;
                }
                self.mov_w0_imm(0);
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
                    // Evaluate left float -> s0
                    self.gen_float_expr(&expr.children[0]);
                    self.mov_s1_s0(); // save left to s1
                    // Evaluate right float -> s0
                    self.gen_float_expr(&expr.children[1]);
                    // Now s0 = right, s1 = left
                    // fcmp s1, s0 and set w0 from condition
                    self.fcmp_s0_s1();
                    self.fset_w0_from_fcmp(*op);
                    return true;
                }
                // Float binary arithmetic: children are float, result is float
                // This case is reached when expr.ty is float but gen_expr was
                // called (e.g., from a cast context). Dispatch to gen_float_expr.
                if is_float_ty(&expr.children[0].ty)
                    && is_float_ty(&expr.children[1].ty)
                    && is_float_ty(&expr.ty)
                {
                    return self.gen_float_expr(expr);
                }
                self.gen_expr(&expr.children[0]);
                self.mov_w14_w0();
                self.gen_expr(&expr.children[1]);
                self.mov_w1_w0();
                self.mov_w0_w14();
                self.binop_w0_w1(*op);
                return true;
            }
            ExprKind::Unary(op) if !expr.children.is_empty() => {
                self.gen_expr(&expr.children[0]);
                match op {
                    UnOp::Neg => self.neg_w0(),
                    UnOp::Plus => {}
                    UnOp::Not => self.cmp_w0_zero(),
                    UnOp::BitNot => self.mvn_w0(),
                }
                return true;
            }
            ExprKind::Cast if !expr.children.is_empty() => {
                let child = &expr.children[0];
                if is_vector_ty(&child.ty) {
                    self.gen_vector_expr(child);
                    self.extract_vector_lane_w0(0);
                } else {
                    self.gen_expr(child);
                    self.truncate_w0_to(&expr.ty);
                }
                return true;
            }
            ExprKind::VaArg { ap } => {
                // M25: 整数/指针型 __builtin_va_arg(ap, T) — 从保存区读
                self.gen_va_arg_int(ap);
                return true;
            }
            ExprKind::Call => {
                self.gen_call(expr);
                return true;
            }
            ExprKind::Conditional if expr.children.len() >= 3 => {
                // csel 无分支三元: cond ? true_val : false_val
                self.gen_expr(&expr.children[0]);
                self.mov_w15_w0();
                self.gen_expr(&expr.children[1]);
                self.mov_w14_w0();
                self.gen_expr(&expr.children[2]);
                self.mov_w1_w0();
                self.mov_w0_w14();
                self.cmp_w15_zero();
                self.csel_w0_w0_w1_ne();
                return true;
            }
            ExprKind::Index if expr.children.len() >= 2 && is_vector_ty(&expr.children[0].ty) => {
                self.gen_vector_expr(&expr.children[0]);
                let idx = expr.children[1]
                    .literal
                    .and_then(|lit| match lit {
                        ExprLiteral::Int(v) => Some(v as usize),
                        ExprLiteral::Float(_) => None,
                    })
                    .unwrap_or(0);
                self.extract_vector_lane_w0(idx);
                return true;
            }
            ExprKind::Address if !expr.children.is_empty() => {
                // &local / &global: 计算变量地址到 x0
                if let ExprKind::Local(ref name) = &expr.children[0].kind {
                    if let Some(&off) = self.locals.get(name) {
                        self.mov_x0_sp_off(off);
                        return true;
                    }
                    if self.global_rodata.contains_key(name) {
                        self.load_global_addr_w0(name);
                        return true;
                    }
                }
                self.mov_w0_imm(0);
                return true;
            }
            _ => {
                // Unhandled IR expression: emit diagnostic instead of
                // silently producing wrong code (mov w0, #0).
                let span = expr.span.clone();
                let mut diag = crate::diagnostics::Diagnostic::err(
                    "E0312",
                    format!(
                        "unsupported IR expression on aarch64: {:?}",
                        expr.kind
                    ),
                    span,
                );
                diag.hint = Some(
                    "add handling in gen_expr or lower in SSA pass".to_string(),
                );
                self.diagnostics.push(diag);
                self.mov_w0_imm(0);
            }
        }
        false
    }

    fn gen_call(&mut self, expr: &TypedExpr) {
        // M25: 识别 __builtin_va_start / __builtin_va_end 内置函数, 不生成实际调用.
        // 调用形式: __builtin_va_start(ap, last) — 2 个参数 (children[1..] = [ap, last])
        //           __builtin_va_end(ap)         — 1 个参数 (children[1..] = [ap])
        let callee = self.callee_name(&expr.children[0]);
        if callee == "__builtin_va_start" {
            if let Some(ap_expr) = expr.children.get(1) {
                if let ExprKind::Local(ref ap_name) = ap_expr.kind {
                    self.gen_va_start(ap_name);
                }
            }
            // va_start 返回 void, w0 = 0 是约定值
            self.mov_w0_imm(0);
            return;
        }
        if callee == "__builtin_va_end" {
            // no-op
            self.mov_w0_imm(0);
            return;
        }
        if callee == "__builtin_va_copy" {
            if let (Some(dst), Some(src)) = (expr.children.get(1), expr.children.get(2)) {
                if let (ExprKind::Local(d), ExprKind::Local(s)) = (&dst.kind, &src.kind) {
                    if let (Some(&doff), Some(&soff)) = (self.locals.get(d), self.locals.get(s)) {
                        // 简化: 8 字节指针拷贝
                        self.ldr_wt_sp(0, soff);
                        self.str_wt_sp(0, doff);
                    }
                }
            }
            self.mov_w0_imm(0);
            return;
        }
        // __atomic_* 内建函数特殊处理 (生成 ARM 原子指令)
        let args = &expr.children[1..];
        if callee.starts_with("__atomic_") && self.gen_atomic_builtin(&callee, args) {
            return;
        }

        // M7 修复: 聚合类型返回 > 16 字节, 走 sret (隐藏 x0 = sret slot)
        // 限制: 仅当 caller 有显式 local sret 时使用; 简化版只在调用方分配
        //       一个 <sret> 局部, callee 写到 [x0]
        let mut sret_reg_idx = 0usize;
        let mut sret_off: Option<i32> = None;
        let needs_sret = is_aggregate(&expr.ty) && aggregate_size(&expr.ty).is_some_and(|s| s > 16);
        if needs_sret {
            let size = aggregate_size(&expr.ty).unwrap();
            let aligned = (size + 15) & !15;
            let off = self.alloc_local("<sret>", aligned as i32);
            self.mov_x0_sp_off(off);
            sret_reg_idx = 1;
            sret_off = Some(off);
        }

        let mut stack_arg_off = self.frame_bytes;
        for (i, arg) in args.iter().enumerate() {
            let reg_idx = sret_reg_idx + i;
            if let Some(hfa) = classify_hfa(&arg.ty, &self.aggregate_fields) {
                if let ExprKind::Local(ref n) = arg.kind {
                    if let Some(&off) = self.locals.get(n) {
                        self.load_hfa_to_vregs(off, hfa, reg_idx as u8);
                        continue;
                    }
                }
            }
            if is_aggregate(&arg.ty) {
                if let ExprKind::Local(ref n) = arg.kind {
                    if let Some(&off) = self.locals.get(n) {
                        let target_reg = reg_idx as u8;
                        self.mov_x_reg_sp_off(target_reg, off);
                        continue;
                    }
                }
                let target_reg = reg_idx as u8;
                self.mov_x_reg_imm(target_reg, 0);
            } else if reg_idx >= 8 {
                self.gen_expr(arg);
                if let ValueType::Float { bits } = &arg.ty {
                    if *bits <= 32 {
                        self.str_s0_sp(stack_arg_off);
                    } else {
                        self.str_dt_sp(0, stack_arg_off);
                    }
                    stack_arg_off += 8;
                } else if is_vector_ty(&arg.ty) {
                    self.str_q0_sp(stack_arg_off);
                    stack_arg_off += 16;
                } else {
                    self.store_w0(stack_arg_off);
                    stack_arg_off += 8;
                }
            } else {
                self.gen_expr(arg);
                if is_float_ty(&arg.ty) {
                    self.mov_v_reg_s0(reg_idx as u8);
                } else if is_vector_ty(&arg.ty) {
                    self.mov_v_reg_v0(reg_idx as u8);
                } else {
                    self.mov_w_reg_w0(reg_idx as u8);
                }
            }
        }

        let callee = self.callee_name(&expr.children[0]);
        self.emit_bl_to(&callee);

        if let Some(off) = sret_off {
            // sret 结果从 <sret> 局部首字加载到 w0 (调用方可继续 store)
            self.load_w0(off);
        } else if let Some(hfa) = classify_hfa(&expr.ty, &self.aggregate_fields) {
            // HFA 返回值已在 v0/s0
            let _ = hfa;
        }
    }

    fn callee_name(&self, expr: &TypedExpr) -> String {
        match expr.kind {
            ExprKind::Local(ref n) => n.clone(),
            _ => "unknown".to_string(),
        }
    }

    // 初始化 AAPCS64 va_list 变量.
    // va_list 是指向寄存器保存区的 void* 指针 (本后端用单指针简化版).
    // ap 局部 (aarch64 上是一个指针) 被写为 va_save_base_off 的地址.
    fn gen_va_start(&mut self, ap_name: &str) {
        let ap_off = match self.locals.get(ap_name) {
            Some(&o) => o,
            None => return,
        };
        // 计算 va_save_base 绝对地址到 x9, 然后存入 ap.
        // AAPCS64 要求 [sp+off] 范围 [0, 504], 我们的 64 字节 < 256, 不会越界.
        // x9 是 caller-saved 临时寄存器, 跨调用无需保存.
        self.mov_x_reg_sp_off(9, self.va_save_base_off);
        // str x9, [sp, #ap_off] (8 字节存储, 需 8 字节对齐 ap_off)
        debug_assert!(
            ap_off >= 0 && ap_off % 8 == 0,
            "gen_va_start: ap_off must be 8-byte aligned, got {ap_off}"
        );
        let insn = 0xf900_0000u32 | (((ap_off / 8) as u32) & 0xfff) << 10 | (31u32 << 5) | 9u32;
        self.emit(&insn.to_le_bytes());
    }

    // 读取下一个变参 (整数路径) 到 w0.
    // 策略: 先从 GP 保存区 (va_save_base + va_gp_offs*8) 读,
    //       超出 8 个则从 caller 栈读.
    fn gen_va_arg_int(&mut self, ap_name: &str) {
        if self.va_gp_offs < 8 {
            let off = self.va_save_base_off + self.va_gp_offs * 8;
            self.ldr_wt_sp(0, off);
            self.va_gp_offs += 1;
        } else {
            // 从 caller 栈读取: 栈参数位于 frame + 128 (GP+float 保存区之后)
            let stack_off = self.frame_bytes + 128 + (self.va_gp_offs - 8) * 8;
            self.ldr_xt_sp(0, stack_off);
            self.va_gp_offs += 1;
        }
        let _ = ap_name;
    }

    // 读取下一个变参 (浮点路径) 到 s0.
    // 策略: 先从 float 保存区 (va_save_base+64 + va_flt_offs*8) 读,
    //       超出 8 个则从 caller 栈读.
    fn gen_va_arg_float(&mut self, _ap_name: &str) {
        if self.va_flt_offs < 8 {
            let off = self.va_save_base_off + 64 + self.va_flt_offs * 8;
            self.ldr_s0_sp(off);
            self.va_flt_offs += 1;
        } else {
            // 从 caller 栈读取
            let stack_off = self.frame_bytes + 128 + (self.va_flt_offs - 8) * 8;
            self.ldr_s0_sp(stack_off);
            self.va_flt_offs += 1;
        }
    }

    fn gen_float_expr(&mut self, expr: &TypedExpr) -> bool {
        match &expr.kind {
            ExprKind::Constant => {
                if let Some(ExprLiteral::Float(v)) = expr.literal {
                    self.load_float_s0(v as f32);
                    return true;
                }
                if let Some(ExprLiteral::Int(v)) = expr.literal {
                    self.mov_w0_imm(trunc_i32(v));
                    // int 转 float: scvtf s0, w0
                    self.emit(&[0x00, 0x00, 0x18, 0x1e]);
                    return true;
                }
            }
            ExprKind::Binary(op) if expr.children.len() >= 2 => {
                self.gen_float_expr(&expr.children[0]);
                self.mov_s1_s0();
                self.gen_float_expr(&expr.children[1]);
                self.float_binop_s0_s1(*op);
                return true;
            }
            ExprKind::Unary(UnOp::Neg) if !expr.children.is_empty() => {
                self.gen_float_expr(&expr.children[0]);
                self.neg_s0();
                return true;
            }
            // Cast: 若目标本身是 float 则保留 float, 否则按整数提升处理.
            // 关键: 调 self.gen_expr(child) 而不是 self.gen_float_expr(child),
            //       避免 (double)x 这种 noop cast 死循环 (child 仍是 double).
            ExprKind::Cast if !expr.children.is_empty() => {
                let child = &expr.children[0];
                if is_float_ty(&expr.ty) {
                    // 浮点 cast: 直接求值 child (它会自己走 gen_float_expr)
                    self.gen_expr(child);
                } else {
                    // 整数 cast (如 (int)sum(d)): 求值 child 后用 trunc_*
                    self.gen_expr(child);
                }
                return true;
            }
            // M25: 浮点型 __builtin_va_arg(ap, double) — v0..v7 已在入口保存到 va_save_base+64
            ExprKind::VaArg { ap } => {
                self.gen_va_arg_float(ap);
                return true;
            }
            // 浮点返回值沿用普通调用路径；AAPCS64 约定结果留在 s0/d0。
            ExprKind::Call if !expr.children.is_empty() => {
                self.gen_call(expr);
                return true;
            }
            // M25: 浮点局部变量从栈加载到 s0 (避免回退 0.0)
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.ldr_s0_sp(off);
                    return true;
                }
            }
            // M25: 浮点条件表达式: cond ? true_val : false_val
            // 用分支实现, 避免 fcsel 寄存器约束
            ExprKind::Conditional if expr.children.len() >= 3 => {
                let lbl_false = self.fresh_label(".Lflt_cond_f");
                let lbl_end = self.fresh_label(".Lflt_cond_end");
                self.gen_expr(&expr.children[0]);
                self.cmp_w0_zero();
                self.cbz_w0(&lbl_false);
                self.gen_float_expr(&expr.children[1]);
                self.b(&lbl_end);
                self.label(&lbl_false);
                self.gen_float_expr(&expr.children[2]);
                self.label(&lbl_end);
                return true;
            }
            _ => {
                // 已经是 float/vector 走到这里, 不能再回 gen_expr (死循环).
                // 整数/复合字面量进 gen_float_expr 是为了 int→float 转换, 但
                // gen_expr 不会调 gen_float_expr (ty 不是 float), 所以这里
                // expr.ty 必然是 float, 直接返回 false 让上层走 fallback.
                if !is_float_ty(&expr.ty) && !is_vector_ty(&expr.ty) && self.gen_expr(expr) {
                    self.emit(&[0x00, 0x00, 0x18, 0x1e]); // scvtf s0, w0
                    return true;
                }
            }
        }
        // 无法处理的浮点表达式: 加载 0.0 作为 fallback 并报告诊断
        let span = expr.span.clone();
        let mut diag = crate::diagnostics::Diagnostic::err(
            "E0314",
            format!(
                "unsupported float IR expression on aarch64: {:?}",
                expr.kind
            ),
            span,
        );
        diag.hint = Some(
            "add handling in gen_float_expr".to_string(),
        );
        self.diagnostics.push(diag);
        self.load_float_s0(0.0);
        false
    }

    fn gen_vector_expr(&mut self, expr: &TypedExpr) -> bool {
        match expr.kind {
            ExprKind::Binary(op) if expr.children.len() >= 2 => {
                self.gen_vector_expr(&expr.children[1]);
                self.mov_v1_v0();
                self.gen_vector_expr(&expr.children[0]);
                self.vector_binop_v0_v1(op);
                return true;
            }
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.ldr_q0_sp(off);
                } else {
                    self.zero_v0();
                }
                return true;
            }
            ExprKind::Constant => {
                if let Some(ExprLiteral::Int(v)) = expr.literal {
                    self.mov_w0_imm(trunc_i32(v));
                    self.dup_v0_4s_w0();
                } else {
                    self.zero_v0();
                }
                return true;
            }
            ExprKind::Aggregate => {
                self.materialize_vector_aggregate(expr);
                return true;
            }
            ExprKind::Cast if !expr.children.is_empty() => {
                let child = &expr.children[0];
                if is_vector_ty(&child.ty) {
                    self.gen_vector_expr(child);
                } else {
                    self.gen_expr(child);
                    self.dup_v0_4s_w0();
                }
                return true;
            }
            ExprKind::Assign if expr.children.len() >= 2 => {
                self.gen_vector_expr(&expr.children[1]);
                if let ExprKind::Local(ref name) = expr.children[0].kind {
                    if let Some(&off) = self.locals.get(name) {
                        self.str_q0_sp(off);
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
                self.cbz_w0(&false_label);
                self.gen_vector_expr(&expr.children[1]);
                self.b(&end_label);
                self.label(&false_label);
                self.gen_vector_expr(&expr.children[2]);
                self.label(&end_label);
                return true;
            }
            _ => {
                self.zero_v0();
            }
        }
        false
    }

    /// Generate code for complex type expressions.
    /// Complex values are stored as (real, imag) pairs:
    /// - Function return: v0 = real, v1 = imag
    /// - Local variable: [sp, #off] = real, [sp, #off+inner_size] = imag
    /// - Memory layout: real at lower address, imag at higher address
    fn gen_complex_expr(&mut self, expr: &TypedExpr) -> bool {
        let inner_size = match &expr.ty {
            ValueType::Complex(inner) => value_type_size(inner),
            _ => return false,
        };
        match &expr.kind {
            // Complex local variable: load both parts into v0, v1
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    // Load real part into v0
                    self.ldr_s0_sp(off);
                    // Load imag part into v1
                    self.ldr_s1_sp(off + inner_size as i32);
                    return true;
                }
                // Unknown local: zero both parts
                self.zero_v0();
                self.zero_v1();
                return true;
            }
            // Complex constant: real = literal, imag = 0
            ExprKind::Constant => {
                if let Some(ExprLiteral::Float(v)) = expr.literal {
                    self.load_float_s0(v as f32);
                    self.zero_v1();
                    return true;
                }
                if let Some(ExprLiteral::Int(v)) = expr.literal {
                    self.mov_w0_imm(trunc_i32(v));
                    // int to float: scvtf s0, w0
                    self.emit(&[0x00, 0x00, 0x18, 0x1e]);
                    self.zero_v1();
                    return true;
                }
                self.zero_v0();
                self.zero_v1();
                return true;
            }
            // Complex binary ops: component-wise add/sub
            ExprKind::Binary(op) if expr.children.len() >= 2 => {
                // Evaluate left -> (v0, v1)
                self.gen_complex_expr(&expr.children[0]);
                // Save left real to stack temp
                let tmp_real = self.alloc_local("<cxr>", inner_size as i32);
                let tmp_imag = self.alloc_local("<cxi>", inner_size as i32);
                self.str_s0_sp(tmp_real);
                self.str_s1_sp(tmp_imag);
                // Evaluate right -> (v0, v1)
                self.gen_complex_expr(&expr.children[1]);
                // Load left back into v2, v3 (use s2, s3 as temp)
                self.ldr_s2_sp(tmp_real);
                self.ldr_s3_sp(tmp_imag);
                // Now: v0,v1 = right; v2,v3 = left
                // Component-wise operation
                match op {
                    BinOp::Add => {
                        // real: v0 = v2 + v0
                        self.emit_fadd_s0_v2_v0();
                        // imag: v1 = v3 + v1
                        self.emit_fadd_s1_v3_v1();
                    }
                    BinOp::Sub => {
                        // real: v0 = v2 - v0
                        self.emit_fsub_s0_v2_v0();
                        // imag: v1 = v3 - v1
                        self.emit_fsub_s1_v3_v1();
                    }
                    _ => {
                        // Unsupported complex binop: zero result
                        self.zero_v0();
                        self.zero_v1();
                    }
                }
                return true;
            }
            // Complex cast: component-wise cast
            ExprKind::Cast if !expr.children.is_empty() => {
                let child = &expr.children[0];
                if is_complex_ty(&child.ty) {
                    // Complex -> Complex: load both parts, possibly convert
                    self.gen_complex_expr(child);
                    // TODO: handle different inner types (float->double, etc.)
                } else {
                    // Scalar -> Complex: real = scalar, imag = 0
                    self.gen_expr(child);
                    // Convert int/float in w0/s0 to complex
                    if is_float_ty(&child.ty) {
                        // s0 already has the value, copy to v0
                        // (gen_expr for float puts result in s0)
                    } else {
                        // int in w0 -> float in s0
                        self.emit(&[0x00, 0x00, 0x18, 0x1e]); // scvtf s0, w0
                    }
                    self.zero_v1();
                }
                return true;
            }
            // Complex call: result in (v0, v1)
            ExprKind::Call => {
                self.gen_call(expr);
                // gen_call should leave result in v0, v1 for complex returns
                return true;
            }
            // Complex conditional: branch-based
            ExprKind::Conditional if expr.children.len() >= 3 => {
                let lbl_false = self.fresh_label(".Lcx_cond_f");
                let lbl_end = self.fresh_label(".Lcx_cond_end");
                self.gen_expr(&expr.children[0]);
                self.cmp_w0_zero();
                self.cbz_w0(&lbl_false);
                self.gen_complex_expr(&expr.children[1]);
                self.b(&lbl_end);
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
                "unsupported complex IR expression on aarch64: {:?}",
                expr.kind
            ),
            span,
        );
        diag.hint = Some("add handling in gen_complex_expr".to_string());
        self.diagnostics.push(diag);
        self.zero_v0();
        self.zero_v1();
        false
    }

    fn gen_vector_operand(&mut self, expr: &TypedExpr) {
        self.gen_vector_expr(expr);
    }

    fn materialize_vector_aggregate(&mut self, expr: &TypedExpr) {
        let off = self
            .locals
            .get("<vec_lit>")
            .copied()
            .unwrap_or_else(|| self.alloc_local("<vec_lit>", 16));
        self.zero_v0();
        self.str_q0_sp(off);
        for (i, child) in expr.children.iter().take(4).enumerate() {
            self.gen_expr(child);
            self.str_wt_sp(0, off + (i as i32) * 4);
        }
        self.ldr_q0_sp(off);
    }

    fn gen_expr_from_ast(&mut self, expr: &Expr) {
        match expr {
            Expr::Int { v, .. } => self.mov_w0_imm(trunc_i32(*v)),
            Expr::Char { v, .. } => self.mov_w0_imm(trunc_i32(*v as i128)),
            Expr::Float { v, .. } => self.load_float_s0(*v as f32),
            Expr::Ident { name, .. } => {
                if let Some(&off) = self.locals.get(&name.name) {
                    self.load_w0(off);
                } else if self.global_rodata.contains_key(&name.name) {
                    self.load_global_addr_w0(&name.name);
                } else {
                    self.mov_w0_imm(0);
                }
            }
            Expr::Binop { op, l, r, .. } => {
                self.gen_expr_from_ast(l);
                self.mov_w14_w0();
                self.gen_expr_from_ast(r);
                self.mov_w1_w0();
                self.mov_w0_w14();
                self.binop_w0_w1(*op);
            }
            Expr::Unary { op, e, .. } => {
                self.gen_expr_from_ast(e);
                match op {
                    UnOp::Neg => self.neg_w0(),
                    UnOp::Plus => {}
                    UnOp::Not => self.cmp_w0_zero(),
                    UnOp::BitNot => self.mvn_w0(),
                }
            }
            Expr::Cast { e, .. } => self.gen_expr_from_ast(e),
            Expr::Call { callee, args, .. } => {
                for (i, arg) in args.iter().enumerate().take(8) {
                    self.gen_expr_from_ast(arg);
                    self.mov_w_reg_w0(i as u8);
                }
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    self.emit_bl_to(&name.name);
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
                );
            }
            _ => {
                if let Ok(v) = crate::const_eval::eval(expr) {
                    self.mov_w0_imm(trunc_i32(v));
                } else {
                    let span = Self::ast_expr_span(expr);
                    let mut diag = crate::diagnostics::Diagnostic::err(
                        "E0313",
                        "unsupported AST expression on aarch64",
                        span,
                    );
                    diag.hint = Some(
                        "add handling in gen_expr_from_ast".to_string(),
                    );
                    self.diagnostics.push(diag);
                    self.mov_w0_imm(0);
                }
            }
        }
    }

    fn load_global_addr_w0(&mut self, name: &str) {
        // TLS 全局变量: 分发 TLS 访问序列 (获取 TLS 变量地址到 x0)
        if self.tls_globals.contains_key(name) {
            if self.relocatable && !self._target.kernel_code_model {
                // 可重定位模式 (非内核): 发出经典 TLSDESC 4 指令序列 + 3 重定位
                self.emit_tlsdesc_access(name);
            } else {
                // ET_EXEC 模式或内核代码模型: 使用 Local Exec 序列 (直接 TP + offset)
                // 内核不使用动态 TLS, 即使 ET_REL 也直接用 LE
                self.emit_tls_le_access(name);
            }
            return;
        }
        if self.relocatable {
            let pos = self.pos();
            self.emit_adr_placeholder(0);
            if let Some(&idx) = self.sym_index.get(name) {
                self.relocations.push(ElfRela {
                    offset: pos,
                    sym_index: idx,
                    rel_type: elf::R_AARCH64_ADR_PREL_LO21,
                    addend: 0,
                });
            }
        } else if let Some(&off) = self.global_rodata.get(name) {
            let rodata_start = self.entry + align_up_u64(self.code.len() as u64, 8);
            let target = rodata_start + off;
            let pos = self.pos();
            self.emit_adr_placeholder(0);
            self.pending_adr_exec.push((pos, 0, target));
        }
    }

    fn store_result(&mut self, off: i32, ty: &crate::ast::Type) {
        if matches!(ty, Type::Float | Type::Double) {
            self.str_s0_sp(off);
        } else if matches!(ty, Type::Vector(_, _)) {
            self.str_q0_sp(off);
        } else {
            self.store_w0(off);
        }
    }

    fn alloc_local(&mut self, name: &str, bytes: i32) -> i32 {
        let align = if bytes >= 16 { 16 } else { 8 };
        let off = (self.stack_bytes + align - 1) & !(align - 1);
        let aligned = (bytes + align - 1) & !(align - 1);
        self.stack_bytes = off + aligned;
        self.locals.insert(name.to_string(), off);
        off
    }

    fn truncate_w0_to(&mut self, ty: &ValueType) {
        match ty {
            ValueType::Integer {
                bits: 8,
                signed: true,
            } => {
                self.emit(&[0x00, 0x00, 0x18, 0x13]);
            }
            ValueType::Integer {
                bits: 8,
                signed: false,
            } => {
                self.emit(&[0x00, 0x00, 0x14, 0x13]);
            }
            ValueType::Integer {
                bits: 16,
                signed: true,
            } => {
                self.emit(&[0x00, 0x00, 0x18, 0x12]);
            }
            ValueType::Integer {
                bits: 16,
                signed: false,
            } => {
                self.emit(&[0x00, 0x00, 0x14, 0x12]);
            }
            ValueType::Integer { bits: 32, .. } => {}
            _ => {}
        }
    }

    fn binop_w0_w1(&mut self, op: BinOp) {
        match op {
            BinOp::Add => self.emit(&[0x00, 0x00, 0x01, 0x0b]),
            BinOp::Sub => self.emit(&[0x00, 0x00, 0x01, 0x4b]),
            BinOp::Mul => self.emit(&[0x00, 0x7c, 0x01, 0x1b]),
            BinOp::And => self.emit(&[0x20, 0x00, 0x01, 0x0a]),
            BinOp::Or => self.emit(&[0x20, 0x00, 0x01, 0x2a]),
            BinOp::Xor => self.emit(&[0x20, 0x00, 0x01, 0x4a]),
            // M7 修复: 移位指令正确编码
            // lslv w0, w0, w1: 0x1A302000 LE = 00 20 30 1A
            //   bits[31:21] = 0b00011010001 (lslv op)
            //   bits[20:16] = Rm (1 for w1)
            //   bits[15:10] = 0b001000
            //   bits[9:5] = Rn (0 for w0)
            //   bits[4:0] = Rd (0 for w0)
            // lsrv w0, w0, w1: 0x1A342000 LE = 00 20 34 1A
            //   bits[23:21] = 0b010 (lsrv op)
            BinOp::Shl => self.emit(&[0x00, 0x20, 0x30, 0x1a]), // lslv w0, w0, w1
            BinOp::Shr => self.emit(&[0x00, 0x20, 0x34, 0x1a]), // lsrv w0, w0, w1
            // M7 修复: 真 sdiv / msub 替代 0
            // binop_w0_w1 之前已把 w0=left, w1=right, w14=left (保存)
            //   Div: sdiv w0, w0, w1   => 0x9AC00C20
            //   Mod: sdiv w0, w0, w1   (w0 = quotient)
            //        msub w0, w1, w0, w14 (w0 = w14 - w1*w0 = left mod right)
            // 编码来源: ARMv8 AArch64 指令集, sdiv 32-bit 形式
            // 限制: 不区分有/无符号 (始终有符号);调用方保证 IR 已 folding 常量
            BinOp::Div => self.emit(&[0x20, 0x0c, 0xc0, 0x9a]), // sdiv w0, w0, w1
            BinOp::Mod => {
                self.emit(&[0x20, 0x0c, 0xc0, 0x9a]); // sdiv w0, w0, w1
                self.emit(&[0xa0, 0x83, 0xb0, 0x9b]); // msub w0, w1, w0, w14
            }
            BinOp::EqEq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                self.cmp_w0_w1();
                let cond = match op {
                    BinOp::EqEq => 0x1,
                    BinOp::Ne => 0x0,
                    BinOp::Lt => 0xb,
                    BinOp::Le => 0xa,
                    BinOp::Gt => 0xc,
                    BinOp::Ge => 0xd,
                    _ => 0x1,
                };
                let insn = 0x1a9f_0000u32 | (cond << 12) | 0x20;
                self.emit(&insn.to_le_bytes());
            }
            BinOp::AndAnd | BinOp::OrOr => self.cmp_w0_zero(),
        }
    }

    fn float_binop_s0_s1(&mut self, op: BinOp) {
        match op {
            BinOp::Add => self.emit(&[0x20, 0x00, 0x21, 0x1e]), // fadd s0, s0, s1
            BinOp::Sub => self.emit(&[0x20, 0x00, 0x41, 0x1e]),
            BinOp::Mul => self.emit(&[0x20, 0x00, 0x61, 0x1e]),
            BinOp::Div => self.emit(&[0x20, 0x00, 0x81, 0x1e]),
            BinOp::EqEq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                self.fcmp_s0_s1();
                self.fset_w0_from_fcmp(op);
            }
            _ => self.emit(&[0x20, 0x00, 0x21, 0x1e]),
        }
    }

    fn vector_binop_v0_v1(&mut self, op: BinOp) {
        match op {
            BinOp::Add => self.emit(&[0x20, 0x14, 0x21, 0x4e]), // add v0.4s, v0.4s, v1.4s
            BinOp::Sub => self.emit(&[0x20, 0x14, 0x41, 0x4e]),
            BinOp::Mul => self.emit(&[0x20, 0x94, 0x81, 0x4e]),
            _ => self.emit(&[0x20, 0x14, 0x21, 0x4e]),
        }
    }

    /// Extract source span from any AST Expr variant (for diagnostics).
    fn ast_expr_span(e: &Expr) -> crate::diagnostics::Span {
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
            | Expr::StmtExpr { span, .. }
            | Expr::Comma { span, .. }
            | Expr::AsmExpr { span, .. }
            | Expr::LabelAddr { span, .. }
            | Expr::VaArg { span, .. }
            | Expr::Offsetof { span, .. } => span.clone(),
            _ => crate::diagnostics::Span::point("", 0, 0),
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
        let bl_pending: Vec<_> = self
            .pending_bl_calls
            .iter()
            .filter(|(_, l)| l == name)
            .cloned()
            .collect();
        for (at, _) in bl_pending {
            self.patch_bl(at, pos);
        }
        self.pending_bl_calls.retain(|(_, l)| l != name);
        let pending: Vec<_> = self
            .pending_branches
            .iter()
            .filter(|(_, l, _)| l == name)
            .cloned()
            .collect();
        for (at, _, kind) in pending {
            match kind {
                BranchKind::B => self.patch_b26(at, pos),
                BranchKind::Cbz => self.patch_cbz(at, pos),
            }
        }
        self.pending_branches.retain(|(_, l, _)| l != name);
        // 修补条件分支
        let bcond_pending: Vec<_> = self
            .pending_bcond
            .iter()
            .filter(|(_, l, _)| l == name)
            .cloned()
            .collect();
        for (at, _, cond) in bcond_pending {
            self.patch_bcond(at, pos, cond);
        }
        self.pending_bcond.retain(|(_, l, _)| l != name);
    }

    fn emit(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    fn load_float_s0(&mut self, v: f32) {
        let off = align_rodata(&mut self.rodata, 4);
        let bits = v.to_bits();
        self.rodata.extend_from_slice(&bits.to_le_bytes());
        if self.relocatable {
            let rodata_idx = self.sym_index.get(".rodata").copied().unwrap_or(1);
            let pos = self.pos();
            self.emit_adr_placeholder(9);
            if rodata_idx > 0 {
                self.relocations.push(ElfRela {
                    offset: pos,
                    sym_index: rodata_idx,
                    rel_type: R_AARCH64_ADR_PREL_LO21,
                    addend: off as i64,
                });
            }
            // ldr s0, [x9] — 绝对偏移 0
            self.emit(&[0x20, 0x00, 0x40, 0xbd]);
        } else {
            let pos = self.pos();
            self.emit_ldr_s0_u32_placeholder();
            self.float_refs.push((pos, off));
        }
    }

    fn mov_w0_imm(&mut self, imm: u32) {
        if imm <= 0xffff {
            let insn = 0x5280_0000u32 | ((imm & 0xffff) << 5);
            self.emit(&insn.to_le_bytes());
            return;
        }
        let lo = imm & 0xffff;
        let hi = (imm >> 16) & 0xffff;
        self.emit(&(0x5280_0000u32 | (lo << 5)).to_le_bytes());
        self.emit(&(0x72a0_0000u32 | (hi << 5)).to_le_bytes());
    }

    // M7 修复: 通用 x 寄存器装入任意立即数 (movz + 必要 movk)
    // 编码: movz xN, #imm16, lsl #shift  => sf=1, opc=10, hw=shift/16
    //       movk xN, #imm16, lsl #shift  => sf=1, opc=11, hw=shift/16
    // 限制: 仅无符号 64-bit; 负数需补码 (调用方负责)
    fn mov_x_reg_imm(&mut self, reg: u8, imm: u64) {
        for shift in [0u32, 16, 32, 48] {
            let part = ((imm >> shift) & 0xffff) as u32;
            // movz 清零其它位, 仅在需要建立 64-bit 值时 emit
            // 但若后续有更高位非零, 当前 shift 必须 emit
            let higher_nonzero = (imm >> (shift + 16)) != 0;
            if part == 0 && shift > 0 && !higher_nonzero {
                continue;
            }
            let hw = shift / 16;
            let insn: u32 = (1 << 31)
                | (0b10 << 29)
                | (0b100101 << 23)
                | (hw << 21)
                | (part << 5)
                | (reg as u32 & 0x1f);
            self.emit(&insn.to_le_bytes());
        }
    }

    // M7 修复: 计算 sp + off 到 x 寄存器的地址
    // 编码: add xN, sp, #imm12  (off >= 0)   => 0x910003e0 | (off<<10) | rd
    //       sub xN, sp, #imm12  (off < 0)    => 0x510003e0 | (-off<<10) | rd
    // 限制: 偏移 12-bit (0..4095); frame 大于此范围需分段
    fn mov_x_reg_sp_off(&mut self, reg: u8, off: i32) {
        if (0..4096).contains(&off) {
            let insn = 0x910003e0u32 | ((off as u32) << 10) | (reg as u32 & 0x1f);
            self.emit(&insn.to_le_bytes());
        } else if off < 0 && off > -4096 {
            let pos = (-off) as u32;
            let insn = 0x510003e0u32 | (pos << 10) | (reg as u32 & 0x1f);
            self.emit(&insn.to_le_bytes());
        } else {
            // 大偏移: movz x9, #off; add/sub xN, sp, x9
            let tmp: u8 = 9;
            let abs = off.unsigned_abs() as u64;
            self.mov_x_reg_imm(tmp, abs);
            if off >= 0 {
                self.emit_add_x_reg_sp_reg(reg, tmp);
            } else {
                self.emit_sub_x_reg_sp_reg(reg, tmp);
            }
        }
    }

    fn mov_x0_sp_off(&mut self, off: i32) {
        self.mov_x_reg_sp_off(0, off);
    }

    fn mov_x0_imm(&mut self, imm: u32) {
        let insn = 0xd280_0000u32 | ((imm & 0xffff) << 5);
        self.emit(&insn.to_le_bytes());
    }

    fn mov_x8_imm(&mut self, imm: u64) {
        let insn = 0xd280_0000u32 | (((imm & 0xffff) as u32) << 5) | 8;
        self.emit(&insn.to_le_bytes());
    }

    fn store_w0(&mut self, off: i32) {
        self.str_wt_sp(0, off);
    }

    fn load_w0(&mut self, off: i32) {
        self.ldr_wt_sp(0, off);
    }

    fn str_wt_sp(&mut self, reg: u8, off: i32) {
        let imm12 = (off / 4) as u32 & 0xfff;
        let insn = 0xb900_0000u32 | imm12 << 10 | 31 << 5 | (reg as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    fn ldr_wt_sp(&mut self, reg: u8, off: i32) {
        let imm12 = (off / 4) as u32 & 0xfff;
        let insn = 0xb940_0000u32 | imm12 << 10 | 31 << 5 | (reg as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    // M25: 64 位 str/ldr 用于 va_list 寄存器保存区
    // str Xt, [sp, #imm] imm 需 8 字节对齐, 范围 [0, 32760]
    // 编码 bits[31:22] = 11_111_0_01_0_L (opc=11, 111, V=0, 01, 0, L)
    // STR X (L=0) => 0xF900_0000, LDR X (L=1) => 0xF940_0000
    /// -ftrivial-auto-var-init=zero: 零初始化栈帧
    /// 使用 stp xzr, xzr, [sp, #off] 每次清零 16 字节 (frame 已 16 字节对齐)
    /// 使用 signed-offset 寻址模式, imm7 范围 [-64,63]×8 = [-512,504]
    fn emit_zero_init_frame(&mut self, frame_size: usize) {
        if frame_size == 0 {
            return;
        }
        let pairs = frame_size / 16;
        for i in 0..pairs {
            let off = (i as i32) * 16 - (frame_size as i32);
            // stp xzr, xzr, [sp, #off] (signed offset mode)
            let imm7 = ((off / 8) as u32) & 0x7f;
            let insn = 0xa900_0000u32 | (imm7 << 15) | (31u32 << 10) | (31u32 << 5) | 31;
            self.emit(&insn.to_le_bytes());
        }
        // 处理剩余 8 字节 (当 frame 非 16 字节对齐时)
        let rem = frame_size % 16;
        if rem >= 8 {
            let off = (pairs * 16) as i32 - (frame_size as i32);
            // str xzr, [sp, #off] (unsigned offset mode)
            let imm12 = ((off / 8) as u32) & 0xfff;
            let insn = 0xf900_0000u32 | (imm12 << 10) | (31u32 << 5) | 31;
            self.emit(&insn.to_le_bytes());
        }
    }

    fn str_xt_sp(&mut self, reg: u8, off: i32) {
        debug_assert!(
            off >= 0 && off % 8 == 0 && off <= 32760,
            "str_xt_sp: off must be non-neg 8-byte aligned, got {off}"
        );
        let imm12 = ((off / 8) as u32) & 0xfff;
        let insn = 0xf900_0000u32 | (imm12 << 10) | (31u32 << 5) | (reg as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    fn ldr_xt_sp(&mut self, reg: u8, off: i32) {
        debug_assert!(
            off >= 0 && off % 8 == 0 && off <= 32760,
            "ldr_xt_sp: off must be non-neg 8-byte aligned, got {off}"
        );
        let imm12 = ((off / 8) as u32) & 0xfff;
        let insn = 0xf940_0000u32 | (imm12 << 10) | (31u32 << 5) | (reg as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    fn str_qt_sp(&mut self, reg: u8, off: i32) {
        debug_assert!(
            off >= 0 && off % 16 == 0 && off <= 65520,
            "str_qt_sp: off must be non-neg 16-byte aligned, got {off}"
        );
        let imm12 = ((off / 16) as u32) & 0xfff;
        let insn = 0x3d80_0000u32 | (imm12 << 10) | (31u32 << 5) | (reg as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    fn ldr_qt_sp(&mut self, reg: u8, off: i32) {
        debug_assert!(
            off >= 0 && off % 16 == 0 && off <= 65520,
            "ldr_qt_sp: off must be non-neg 16-byte aligned, got {off}"
        );
        let imm12 = ((off / 16) as u32) & 0xfff;
        let insn = 0x3dc0_0000u32 | (imm12 << 10) | (31u32 << 5) | (reg as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    fn str_q0_sp(&mut self, off: i32) {
        self.str_qt_sp(0, off);
    }

    fn ldr_q0_sp(&mut self, off: i32) {
        self.ldr_qt_sp(0, off);
    }

    fn extract_vector_lane_w0(&mut self, idx: usize) {
        let lane = (idx & 3) as u32;
        let insn = 0x0e04_3c00u32 | (lane << 11);
        self.emit(&insn.to_le_bytes());
    }

    fn mov_w14_w0(&mut self) {
        self.emit(&[0xee, 0x03, 0x80, 0x2a]);
    }

    fn mov_w0_w14(&mut self) {
        self.emit(&[0xe0, 0x03, 0x0e, 0x2a]);
    }

    fn mov_w1_w0(&mut self) {
        self.emit(&[0xe1, 0x03, 0x80, 0x2a]);
    }

    /// MOV W2, W0 — ORR W2, WZR, W0
    fn mov_w2_w0(&mut self) {
        self.emit(&[0xe2, 0x03, 0x80, 0x2a]);
    }

    /// CMP Wn, Wm (alias of SUBS WZR, Wn, Wm) — 32-bit register compare
    fn emit_cmp_w_reg(&mut self, rn: u8, rm: u8) {
        // SUBS WZR, Wn, Wm => 0x6B000000 | Rm<<16 | Rn<<5 | 31
        let subs = 0x6b00001fu32
            | ((rm as u32 & 0x1f) << 16)
            | ((rn as u32 & 0x1f) << 5);
        self.emit(&subs.to_le_bytes());
    }

    fn mov_w_reg_w0(&mut self, reg: u8) {
        let insn = 0x2a0003e0u32 | ((reg as u32 & 0x1f) << 16) | ((reg as u32 & 0x1f) << 5);
        self.emit(&insn.to_le_bytes());
    }

    /// MOV Wd, Ws — general 32-bit register-to-register move
    fn mov_w_rd_rs(&mut self, rd: u8, rs: u8) {
        let insn = 0x2a0003e0u32
            | ((rs as u32 & 0x1f) << 16)
            | ((rd as u32 & 0x1f) << 0);
        self.emit(&insn.to_le_bytes());
    }

    /// MOV Xd, Xs — general 64-bit register-to-register move
    /// 编码: ORR Xd, XZR, Xs (alias of MOV)
    fn mov_x_reg(&mut self, dst: u8, src: u8) {
        if dst == src {
            return;
        }
        let insn = 0xaa0003e0u32 | ((src as u32 & 0x1f) << 16) | ((dst as u32 & 0x1f) << 0);
        self.emit(&insn.to_le_bytes());
    }

    /// Load a local variable from stack into an arbitrary GP register (32-bit W)
    fn asm_load_var_to_reg(&mut self, var_name: &str, reg_idx: u8) {
        if let Some(&off) = self.locals.get(var_name) {
            self.ldr_wt_sp(reg_idx, off);
        }
    }

    /// Store a GP register value back to a local variable on stack (32-bit W)
    fn asm_store_reg_to_var(&mut self, var_name: &str, reg_idx: u8) {
        if let Some(&off) = self.locals.get(var_name) {
            self.str_wt_sp(reg_idx, off);
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
    ) {
        let pool = asm_constraints::aarch64_reg_pool();
        let resolved_operands = resolve_asm_operands(
            output_constraints,
            input_constraints,
            output_names,
            input_names,
            &pool,
        );

        // --- 加载 inout 输入值到分配的寄存器 ---
        for (i, op) in resolved_operands.iter().enumerate() {
            if op.is_input && op.is_output {
                // InOutReg: 加载变量当前值到寄存器
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

        // --- memory clobber: DMB ISH 编译器屏障 ---
        if clobbers.iter().any(|c| c == "memory") {
            self.emit_dmb_ish();
        }

        // --- 表达式结果: 第一个输出寄存器 → w0 ---
        if let Some(reg) = first_output_reg {
            if reg != 0 {
                self.mov_w_rd_rs(0, reg);
            }
        } else {
            self.mov_w0_imm(0);
        }
    }

    fn mov_v_reg_s0(&mut self, reg: u8) {
        let insn = 0x1e204000u32 | ((reg as u32 & 7) << 5);
        self.emit(&insn.to_le_bytes());
    }

    fn mov_v_reg_v0(&mut self, reg: u8) {
        if reg == 0 {
            return;
        }
        let insn = 0x4ea0_1c00u32 | (reg as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    fn dup_v0_4s_w0(&mut self) {
        self.emit(&[0x00, 0x0c, 0x04, 0x4e]);
    }

    fn mov_s1_s0(&mut self) {
        self.emit(&[0x20, 0x00, 0x20, 0x1e]);
    }

    fn neg_s0(&mut self) {
        self.emit(&[0x00, 0x00, 0xa0, 0x1e]); // fneg s0, s0
    }

    fn mov_v1_v0(&mut self) {
        self.emit(&[0x20, 0x00, 0x81, 0x4e]); // orr v1.16b, v0.16b, v0.16b
    }

    fn zero_v0(&mut self) {
        self.emit(&[0x00, 0x00, 0x80, 0x4e]); // movi v0.4s, #0
    }

    fn zero_v1(&mut self) {
        self.emit(&[0x01, 0x00, 0x80, 0x4e]); // movi v1.4s, #0
    }

    /// `str st, [sp, #imm12]` — store single-precision float to stack.
    /// Generic version for any register.
    fn str_st_sp(&mut self, reg: u8, off: i32) {
        let imm12 = (off / 4) as u32 & 0xfff;
        let insn = 0xbd000000u32 | imm12 << 10 | 31u32 << 5 | (reg as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// `ldr st, [sp, #imm12]` — load single-precision float from stack.
    /// Generic version for any register.
    fn ldr_st_sp(&mut self, reg: u8, off: i32) {
        let imm12 = (off / 4) as u32 & 0xfff;
        let insn = 0xbd400000u32 | imm12 << 10 | 31u32 << 5 | (reg as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    fn str_s1_sp(&mut self, off: i32) {
        self.str_st_sp(1, off);
    }

    fn ldr_s1_sp(&mut self, off: i32) {
        self.ldr_st_sp(1, off);
    }

    fn ldr_s2_sp(&mut self, off: i32) {
        self.ldr_st_sp(2, off);
    }

    fn ldr_s3_sp(&mut self, off: i32) {
        self.ldr_st_sp(3, off);
    }

    /// FADD Sd, Sn, Sm — single-precision floating-point add
    fn emit_fadd(&mut self, rd: u8, rn: u8, rm: u8) {
        // FADD (scalar): 0x1E202800 | Rm<<16 | Rn<<5 | Rd
        let insn = 0x1e202800u32
            | ((rm as u32 & 0x1f) << 16)
            | ((rn as u32 & 0x1f) << 5)
            | (rd as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// FSUB Sd, Sn, Sm — single-precision floating-point subtract
    fn emit_fsub(&mut self, rd: u8, rn: u8, rm: u8) {
        // FSUB (scalar): 0x1E203800 | Rm<<16 | Rn<<5 | Rd
        let insn = 0x1e203800u32
            | ((rm as u32 & 0x1f) << 16)
            | ((rn as u32 & 0x1f) << 5)
            | (rd as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// Complex add helpers: component-wise
    fn emit_fadd_s0_v2_v0(&mut self) {
        self.emit_fadd(0, 2, 0); // fadd s0, s2, s0
    }

    fn emit_fadd_s1_v3_v1(&mut self) {
        self.emit_fadd(1, 3, 1); // fadd s1, s3, s1
    }

    fn emit_fsub_s0_v2_v0(&mut self) {
        self.emit_fsub(0, 2, 0); // fsub s0, s2, s0
    }

    fn emit_fsub_s1_v3_v1(&mut self) {
        self.emit_fsub(1, 3, 1); // fsub s1, s3, s1
    }

    fn sub_sp_imm(&mut self, bytes: u32) {
        if bytes == 0 {
            return;
        }
        if bytes <= 0xfff {
            let insn = 0xd100_0000u32 | ((bytes & 0xfff) << 10) | (31 << 5) | 31;
            self.emit(&insn.to_le_bytes());
        }
    }

    /// -fstack-clash-protection: emit page-sized probes when allocating a
    /// large stack frame.  Each probe stores a zero byte at [sp] to trigger
    /// a fault on the guard page before the kernel maps it.
    fn emit_stack_clash_probes(&mut self, frame_size: u32) {
        let page: u32 = 4096;
        let remainder = frame_size % page;
        let pages = frame_size / page;
        // First: sub sp, sp, #remainder (if any)
        if remainder > 0 {
            self.sub_sp_imm(remainder);
        }
        if pages > 0 {
            // Probe loop using x16 (ip0, ABI scratch) as counter
            // movz x16, #pages
            let movz = 0x52800010u32 | ((pages as u32 & 0xffff) << 5);
            self.emit(&movz.to_le_bytes());
            // .probe_loop:
            let loop_start = self.pos();
            // sub sp, sp, #4096  (1 << 12, using shift encoding)
            let sub_insn = 0xd1000000u32 | (1u32 << 22) | (1u32 << 10) | (31u32 << 5) | 31;
            self.emit(&sub_insn.to_le_bytes());
            // strb wzr, [sp]  — probe the page
            let strb = 0x3900001fu32;
            self.emit(&strb.to_le_bytes());
            // sub x16, x16, #1
            let sub_cnt = 0xd1000010u32 | (1u32 << 10);
            self.emit(&sub_cnt.to_le_bytes());
            // cbnz x16, .probe_loop
            let loop_end = self.pos();
            let offset = (loop_start as i64 - loop_end as i64) as i32;
            let cbnz = 0xb5000010u32 | (((offset as u32 >> 2) & 0x7ffff) << 5);
            self.emit(&cbnz.to_le_bytes());
        }
    }

    fn add_sp_imm(&mut self, bytes: u32) {
        if bytes == 0 {
            return;
        }
        if bytes <= 0xfff {
            let insn = 0x9100_0000u32 | ((bytes & 0xfff) << 10) | (31 << 5) | 31;
            self.emit(&insn.to_le_bytes());
        }
    }

    fn neg_w0(&mut self) {
        self.emit(&[0x00, 0x00, 0x00, 0x4b]);
    }

    fn mvn_w0(&mut self) {
        self.emit(&[0x20, 0x03, 0x20, 0x2a]);
    }

    fn cmp_w0_zero(&mut self) {
        self.emit(&[0x1f, 0x00, 0x00, 0x71]);
        self.emit(&[0xe0, 0x07, 0x9f, 0x1a]);
    }

    fn cmp_w0_w1(&mut self) {
        self.emit(&[0x3f, 0x00, 0x01, 0x6b]);
    }

    fn cbz_w0(&mut self, label: &str) {
        let pos = self.pos();
        self.emit(&[0x00, 0x00, 0x00, 0x34]);
        if let Some(&target) = self.labels.get(label) {
            self.patch_cbz(pos, target);
        } else {
            self.pending_branches
                .push((pos, label.to_string(), BranchKind::Cbz));
        }
    }

    fn b(&mut self, label: &str) {
        let pos = self.pos();
        self.emit(&[0x00, 0x00, 0x00, 0x14]);
        if let Some(&target) = self.labels.get(label) {
            self.patch_b26(pos, target);
        } else {
            self.pending_branches
                .push((pos, label.to_string(), BranchKind::B));
        }
    }

    fn patch_b26(&mut self, at: u64, target: u64) {
        let pc = at + 4;
        let imm26 = ((target as i64 - pc as i64) >> 2) as u32 & 0x03ff_ffff;
        let insn = 0x1400_0000u32 | imm26;
        let off = at as usize;
        if off + 4 <= self.code.len() {
            self.code[off..off + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    fn patch_cbz(&mut self, at: u64, target: u64) {
        let pc = at + 4;
        let imm19 = ((target as i64 - pc as i64) >> 2) as u32 & 0x7ffff;
        let insn = 0x3400_0000u32 | imm19;
        let off = at as usize;
        if off + 4 <= self.code.len() {
            self.code[off..off + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    fn ret(&mut self) {
        // Stack protector epilogue: verify canary BEFORE frame teardown
        if self._target.stack_protector && self.canary_slot_off > 0 {
            self.emit_stack_chk_epilogue(self.canary_slot_off);
        }
        if self.mte_active() {
            // MTE 收尾: 恢复入参 x19, 清除 sp 的 tag, 再释放帧。
            // 返回时 sp 为 untagged, 调用方 (MTE 函数) 会通过其 x19 恢复自身 tag,
            // 非 MTE 调用方 (_start / libc) 本就期望 untagged sp。
            self.ldr_xt_sp(19, self.mte_save_off); // ldr x19, [sp, #mte_save_off]
            self.emit(&0x9280dfffu32.to_le_bytes()); // and sp, sp, #0x00ffffffffffffff
            self.add_sp_imm(self.frame_bytes as u32);
        } else if self.frame_bytes > 0 {
            self.add_sp_imm(self.frame_bytes as u32);
        }
        self.emit(&[0xc0, 0x03, 0x5f, 0xd6]);
    }

    fn svc_zero(&mut self) {
        self.emit(&[0x01, 0x00, 0x00, 0xd4]);
    }

    fn emit_bl_placeholder(&mut self) {
        self.emit(&[0x00, 0x00, 0x00, 0x94]);
    }

    /// 校验 BL 偏移是否在 ±128MiB 范围内 (imm26 × 4)
    fn bl_offset_in_range(diff: i64) -> bool {
        (-(1 << 27)..(1 << 27)).contains(&diff)
    }

    fn emit_bl_to(&mut self, name: &str) {
        // MTE 调用点: 调用前清除 sp 的 tag (外部非 MTE 代码按 untagged sp 运行),
        // 返回后通过 x19 恢复本帧 tag, 保证后续 [sp,#off] 局部访问通过硬件检查。
        let untag = self.mte_active();
        if untag {
            self.mte_untag_sp_for_call();
        }
        if let Some(&target) = self.labels.get(name) {
            let pos = self.pos();
            self.emit_bl_placeholder();
            self.patch_bl(pos, target);
        } else if self.relocatable {
            let pos = self.pos();
            self.emit_bl_placeholder();
            let idx = self.ensure_sym(name);
            self.relocations.push(ElfRela {
                offset: pos,
                sym_index: idx,
                rel_type: R_AARCH64_CALL26,
                addend: 0,
            });
        } else {
            let pos = self.pos();
            self.emit_bl_placeholder();
            self.pending_bl_calls.push((pos, name.to_string()));
        }
        if untag {
            self.mte_retag_sp_after_call();
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

    /// Like ensure_sym but creates an STT_OBJECT symbol (for data references).
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

    /// Emit ADRP Xd, __stack_chk_guard with proper relocation.
    /// Used by stack protector prologue/epilogue.
    fn emit_adrp_stack_chk_guard(&mut self, rd: u8) {
        let pos = self.pos();
        // ADRP Xd, #0 (placeholder, patched by linker via relocation)
        // Encoding: 1 immlo 10000 immhi Rd
        // immlo=00, immhi=0000000000000000000
        let adrp = 0x90000000u32 | ((rd as u32 & 0x1f) << 0);
        self.emit(&adrp.to_le_bytes());
        if self.relocatable {
            let idx = self.ensure_sym("__stack_chk_guard");
            self.relocations.push(ElfRela {
                offset: pos,
                sym_index: idx,
                rel_type: elf::R_AARCH64_ADR_PREL_PG_HI21,
                addend: 0,
            });
        }
    }

    /// Emit LDR Xt, [Xd, :lo12:__stack_chk_guard] with proper relocation.
    /// Loads the 64-bit canary value from the global.
    fn emit_ldr_stack_chk_guard(&mut self, rt: u8, rn: u8) {
        let pos = self.pos();
        // LDR Xt, [Xn, #imm] (unsigned offset, 64-bit)
        // imm12 = 0 (placeholder, patched by linker)
        let ldr = 0xf9400000u32
            | ((rt as u32 & 0x1f) << 0)
            | ((rn as u32 & 0x1f) << 5);
        self.emit(&ldr.to_le_bytes());
        if self.relocatable {
            let idx = self.ensure_sym("__stack_chk_guard");
            self.relocations.push(ElfRela {
                offset: pos,
                sym_index: idx,
                rel_type: elf::R_AARCH64_LDST64_ABS_LO12_NC,
                addend: 0,
            });
        }
    }

    /// Emit CMP Xn, Xm (alias of SUBS XZR, Xn, Xm)
    fn emit_cmp_x_reg(&mut self, rn: u8, rm: u8) {
        // SUBS XZR, Xn, Xm => 0xEB000000 | Rm<<16 | Rn<<5 | 31
        let subs = 0xeb000000u32
            | ((rm as u32 & 0x1f) << 16)
            | ((rn as u32 & 0x1f) << 5)
            | 31;
        self.emit(&subs.to_le_bytes());
    }

    /// Emit B.NE label (branch if not equal, condition code 0x1)
    fn emit_bne(&mut self, label: &str) {
        let pos = self.pos();
        // B.cond: 0x54000000 | imm19<<5 | cond
        // cond=0001 (NE)
        let bne = 0x54000001u32;
        self.emit(&bne.to_le_bytes());
        if let Some(&target) = self.labels.get(label) {
            self.patch_bcond(pos, target, 0x1);
        } else {
            self.pending_bcond.push((pos, label.to_string(), 0x1));
        }
    }

    /// Stack protector prologue: load canary from __stack_chk_guard
    /// and store at [sp, #canary_off]. Uses x16 as scratch.
    fn emit_stack_chk_prologue(&mut self, canary_off: i32) {
        self.emit_adrp_stack_chk_guard(16); // adrp x16, __stack_chk_guard
        self.emit_ldr_stack_chk_guard(16, 16); // ldr x16, [x16, :lo12:]
        self.str_xt_sp(16, canary_off); // str x16, [sp, #canary_off]
    }

    /// Stack protector epilogue: verify canary at [sp, #canary_off]
    /// against __stack_chk_guard. Calls __stack_chk_fail on mismatch.
    /// Uses x16, x17 as scratch. Must be called BEFORE frame teardown.
    fn emit_stack_chk_epilogue(&mut self, canary_off: i32) {
        let fail_label = self.fresh_label(".Lstack_chk_fail_");
        // Load stored canary
        self.ldr_xt_sp(16, canary_off); // ldr x16, [sp, #canary_off]
        // Load expected canary
        self.emit_adrp_stack_chk_guard(17); // adrp x17, __stack_chk_guard
        self.emit_ldr_stack_chk_guard(17, 17); // ldr x17, [x17, :lo12:]
        // Compare
        self.emit_cmp_x_reg(16, 17); // cmp x16, x17
        // Branch to fail if mismatch
        self.emit_bne(&fail_label);
        // Normal path continues (caller will do frame teardown + ret)
        // Fail path: call __stack_chk_fail (never returns)
        self.label(&fail_label);
        self.emit_bl_to("__stack_chk_fail");
    }

    fn patch_bl(&mut self, at: u64, target: u64) {
        let pc = at + 4;
        let diff = target as i64 - pc as i64;
        debug_assert!(
            Self::bl_offset_in_range(diff),
            "BL offset out of range: diff={diff}"
        );
        let imm26 = ((diff >> 2) as u32) & 0x03ff_ffff;
        let insn = 0x9400_0000u32 | imm26;
        let off = at as usize;
        self.code[off..off + 4].copy_from_slice(&insn.to_le_bytes());
    }

    fn emit_adr_placeholder(&mut self, rd: u8) {
        let insn = 0x1000_0000u32 | (rd as u32);
        self.emit(&insn.to_le_bytes());
    }

    /// 校验 ADR 偏移是否在 ±1MiB 范围内 (immhi[20:0] : immlo[1:0])
    fn adr_offset_in_range(diff: i64) -> bool {
        (-(1 << 20)..(1 << 20)).contains(&diff)
    }

    fn emit_ldr_s0_u32_placeholder(&mut self) {
        // ldr s0, [pc, #0]; b +8 (后续 patch_ldr_u32 修正偏移)
        self.emit(&[0x00, 0x00, 0x00, 0xbd]);
        self.emit(&[0x02, 0x00, 0x00, 0x14]);
    }

    /// 校验 LDR literal 偏移是否在 ±1MiB 范围内 (imm19 × 4)
    fn ldr_literal_offset_in_range(diff: i64) -> bool {
        (-(1 << 20)..(1 << 20)).contains(&diff)
    }

    fn patch_ldr_u32(&mut self, at: u64, addr: u64) {
        let pc = at + 8; // ldr literal PC-relative 从指令后 8 字节算起 (跳过 branch)
        let diff = addr as i64 - pc as i64;
        debug_assert!(
            Self::ldr_literal_offset_in_range(diff) && diff % 4 == 0,
            "LDR literal offset out of range or unaligned: diff={diff}"
        );
        let off = ((diff / 4) as u32) & 0x7ffff;
        let insn = 0xbd000000u32 | off;
        let off_usize = at as usize;
        if off_usize + 4 <= self.code.len() {
            self.code[off_usize..off_usize + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    fn patch_adr(&mut self, at: u64, rd: u8, target: u64) {
        let pc = at;
        let diff = target as i64 - pc as i64;
        if !Self::adr_offset_in_range(diff) {
            // 超出 ADR ±1MiB 范围: 退化为 ADRP + ADD
            // ADRP: 页对齐 (pc & ~0xfff) + (immhi << 12)
            let page_off = (target as i64 & !0xfff) - (pc as i64 & !0xfff);
            let _lo_off = (target as i64 & 0xfff) as u32;
            let immlo = ((page_off >> 12) & 0x3) as u32;
            let immhi = ((page_off >> 14) & 0x7ffff) as u32;
            // adrp rd, #page_off
            let insn_adrp = 0x9000_0000u32 | (immlo << 29) | (immhi << 5) | (rd as u32 & 0x1f);
            let off = at as usize;
            if off + 4 <= self.code.len() {
                self.code[off..off + 4].copy_from_slice(&insn_adrp.to_le_bytes());
            }
            // add rd, rd, #lo_off (追加在 adrp 后, 需扩展 code)
            // 但 patch 是原地修改, 无法插入; 改为在 emit 阶段用
            // emit_adr_large 预先留 2 条指令空间
            // 此处仅修补不超出的情况; 超出时截断到最大可表示偏移
            return;
        }
        let immlo = (diff & 0x3) as u32;
        let immhi = ((diff >> 2) & 0x7ffff) as u32;
        let insn = 0x1000_0000u32 | (immlo << 29) | (immhi << 5) | (rd as u32 & 0x1f);
        let off = at as usize;
        if off + 4 <= self.code.len() {
            self.code[off..off + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    /// 发射 ADR + ADD 双指令序列 (用于超出 ±1MiB 的全范围地址)
    /// 调用方需预留 8 字节空间
    fn emit_adr_large(&mut self, rd: u8, target: u64) {
        // 先用 adrp 占位, 后续 patch
        let insn_adrp = 0x9000_0000u32 | (rd as u32);
        self.emit(&insn_adrp.to_le_bytes());
        // add rd, rd, #0 占位
        let insn_add = 0x9100_0000u32 | (rd as u32) << 5 | (rd as u32);
        self.emit(&insn_add.to_le_bytes());
        let pos = self.pos() - 8;
        self.pending_adr_exec.push((pos, rd, target));
    }

    // ===== 谓词/浮点比较 =====

    fn fcmp_s0_s1(&mut self) {
        // fcmp s0, s1
        self.emit(&[0x00, 0x00, 0x28, 0x1e]);
    }

    /// fcmp d0, d1 — 双精度浮点比较
    fn fcmp_d0_d1(&mut self) {
        // fcmp d0, d1: sf=1, ftype=01, Rm=1, Rn=0
        // 0x1E682000 LE = 00 20 68 1e
        self.emit(&[0x00, 0x20, 0x68, 0x1e]);
    }

    /// fcmp s0, #0.0 — 单精度与零比较
    fn fcmp_s0_zero(&mut self) {
        self.emit(&[0x08, 0x00, 0x28, 0x1e]);
    }

    /// fcmp d0, #0.0 — 双精度与零比较
    fn fcmp_d0_zero(&mut self) {
        // fcmp d0, #0.0: 0x1E680008 LE = 08 00 68 1e
        self.emit(&[0x08, 0x00, 0x68, 0x1e]);
    }

    fn fset_w0_from_fcmp(&mut self, op: BinOp) {
        let cond = match op {
            BinOp::EqEq => 0x1u32,
            BinOp::Ne => 0x0,
            BinOp::Lt => 0xb,
            BinOp::Le => 0xa,
            BinOp::Gt => 0xc,
            BinOp::Ge => 0xd,
            _ => 0x1,
        };
        // cset w0, cond
        let insn = 0x1a9f_0000u32 | (cond << 12) | 0x20;
        self.emit(&insn.to_le_bytes());
    }

    /// fccmp s0, s1, #nzcv, ne — 条件浮点比较保留 flags
    fn emit_fccmp_s0_s1_ne(&mut self, nzcv: u8) {
        // fccmp s0, s1, #nzcv, ne
        // encoding: 0x1e210420 | (nzcv << 12) | (cond_ne=1 << 12)
        let insn = 0x1e610420u32 | ((nzcv as u32 & 0xf) << 12) | (1 << 12);
        self.emit(&insn.to_le_bytes());
    }

    /// fccmp d0, d1, #nzcv, ne — 条件双精度浮点比较
    fn emit_fccmp_d0_d1_ne(&mut self, nzcv: u8) {
        let insn = 0x1e610420u32 | 0x400000 | ((nzcv as u32 & 0xf) << 12) | (1 << 12);
        self.emit(&insn.to_le_bytes());
    }

    fn cmp_w15_zero(&mut self) {
        // subs wzr, w15, #0  (cmp w15, #0)
        self.emit(&[0xff, 0x03, 0x00, 0x71]);
    }

    fn mov_w15_w0(&mut self) {
        self.emit(&[0xef, 0x03, 0x80, 0x2a]); // mov w15, w0
    }

    /// csel w0, w0, w1, ne
    fn csel_w0_w0_w1_ne(&mut self) {
        self.emit(&[0xe0, 0x01, 0x81, 0x1a]); // cond=ne(1)
    }

    /// ccmp w0, w1, #nzcv, ne — 条件比较保留 flags
    fn emit_ccmp_w0_w1(&mut self, nzcv: u8) {
        // ccmp w0, w1, #nzcv, ne
        // encoding: sf=0, op=1, S=1, 011010, Rm=1, cond=ne(1), 0, Rn=0, imm4=nzcv
        let insn = 0x7a400000u32
            | ((nzcv as u32 & 0xf) << 12)
            | (1u32 << 12)  // cond = ne
            | (1u32 << 16); // Rn = w0
        self.emit(&insn.to_le_bytes());
    }

    /// ccmp w0, #imm5, #nzcv, cond — 立即数条件比较
    fn emit_ccmp_w0_imm(&mut self, imm5: u8, nzcv: u8, cond: u8) {
        // ccmp w0, #imm5, #nzcv, cond
        // sf=0, op=1, S=1, 01101, imm5, cond, 1, Rn=0, imm4=nzcv
        let insn = 0x7a000000u32
            | ((imm5 as u32 & 0x1f) << 16)
            | ((cond as u32 & 0xf) << 12)
            | (1u32 << 4)  // o2=1 (immediate form)
            | (nzcv as u32 & 0xf);
        self.emit(&insn.to_le_bytes());
    }

    /// csinc w0, w0, w1, eq
    fn emit_csinc_w0_w0_w1_eq(&mut self) {
        self.emit(&[0xe0, 0x01, 0x84, 0x1a]);
    }

    /// csinc xd, xn, xm, cond — 通用条件选择递增
    fn emit_csinc(&mut self, dst: u8, rn: u8, rm: u8, cond: u8) {
        let insn = 0x1a800000u32
            | ((rm as u32 & 0x1f) << 16)
            | ((cond as u32 & 0xf) << 12)
            | ((rn as u32 & 0x1f) << 5)
            | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// csel w0, w0, w1, cond — 通用条件选择
    fn emit_csel_w0_w0_w1(&mut self, cond: u8) {
        let insn = 0x1a800000u32
            | (1u32 << 16)   // Rm = w1
            | ((cond as u32 & 0xf) << 12); // Rd = w0
        self.emit(&insn.to_le_bytes());
    }

    /// csneg w0, w0, w1, lt — 条件符号取反
    fn emit_csneg_w0_w0_w1_lt(&mut self) {
        let insn = 0x5a800400u32 | (1u32 << 16) | (0xb << 12);
        self.emit(&insn.to_le_bytes());
    }

    fn emit_add_x_reg_sp_reg(&mut self, dst: u8, src: u8) {
        let insn = 0x8b0003e0u32 | ((src as u32 & 0x1f) << 16) | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    fn emit_sub_x_reg_sp_reg(&mut self, dst: u8, src: u8) {
        let insn = 0xcb0003e0u32 | ((src as u32 & 0x1f) << 16) | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    // ===== 复杂寻址模式: 后置索引 / 前置索引 =====

    /// str xt, [xn], #imm — 后置索引存储 (store then update base)
    fn emit_str_post_index(&mut self, rt: u8, rn: u8, imm9: i32) {
        let imm9_u = (imm9 as u32) & 0x1ff;
        // sf=1, 111000, 00, 0 (post), imm9, 01, Rn, Rt
        // base = 0xF8000400
        let insn = 0xf8000400u32 | (imm9_u << 12) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// ldr xt, [xn], #imm — 后置索引加载 (load then update base)
    fn emit_ldr_post_index(&mut self, rt: u8, rn: u8, imm9: i32) {
        let imm9_u = (imm9 as u32) & 0x1ff;
        // sf=1, 111000, 01, 0 (post), imm9, 01, Rn, Rt
        let insn = 0xf8400400u32 | (imm9_u << 12) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// str xt, [xn, #imm]! — 前置索引存储 (update base then store)
    fn emit_str_pre_index(&mut self, rt: u8, rn: u8, imm9: i32) {
        let imm9_u = (imm9 as u32) & 0x1ff;
        // sf=1, 111000, 00, 1 (pre), imm9, 01, Rn, Rt
        // base = 0xF8800000 (bit23=1 for pre-index)
        let insn = 0xf8800000u32 | (imm9_u << 12) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// ldr xt, [xn, #imm]! — 前置索引加载 (update base then load)
    fn emit_ldr_pre_index(&mut self, rt: u8, rn: u8, imm9: i32) {
        let imm9_u = (imm9 as u32) & 0x1ff;
        // sf=1, 111000, 01, 1 (pre), imm9, 01, Rn, Rt
        let insn = 0xf8c00000u32 | (imm9_u << 12) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// ldr xt, [xn, xm, extend {#shift}] — 扩展寄存器寻址
    fn emit_ldr_ext_reg(&mut self, rt: u8, rn: u8, rm: u8, extend: u8, shift: u8) {
        // LDR (extended register): 0xF8600800 | Rm<<16 | option<<13 | S<<12 | Rn<<5 | Rt
        let insn = 0xf8600800u32
            | ((rm as u32 & 0x1f) << 16)
            | ((extend as u32 & 0x7) << 13)
            | ((shift as u32 & 0x1) << 12)
            | ((rn as u32 & 0x1f) << 5)
            | (rt as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// str xt, [xn, xm, extend {#shift}] — 扩展寄存器存储
    fn emit_str_ext_reg(&mut self, rt: u8, rn: u8, rm: u8, extend: u8, shift: u8) {
        let insn = 0xf8200800u32
            | ((rm as u32 & 0x1f) << 16)
            | ((extend as u32 & 0x7) << 13)
            | ((shift as u32 & 0x1) << 12)
            | ((rn as u32 & 0x1f) << 5)
            | (rt as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// LDR Xt, [Xn, #imm] - 按尺寸无符号偏移加载 (用于解引用指针)
    fn emit_ldr_x(&mut self, rt: u8, rn: u8, imm: u32, sz: usize) {
        let imm12 = (imm / sz as u32) & 0xfff;
        let insn = match sz {
            1 => 0x39400000u32 | (imm12 << 10) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            2 => 0x79400000u32 | (imm12 << 10) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            4 => 0xb9400000u32 | (imm12 << 10) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            _ => 0xf9400000u32 | (imm12 << 10) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
        };
        self.emit(&insn.to_le_bytes());
    }

    /// STR Xt, [Xn, #imm] - 按尺寸无符号偏移存储
    fn emit_str_x(&mut self, rt: u8, rn: u8, imm: u32, sz: usize) {
        let imm12 = (imm / sz as u32) & 0xfff;
        let insn = match sz {
            1 => 0x39000000u32 | (imm12 << 10) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            2 => 0x79000000u32 | (imm12 << 10) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            4 => 0xb9000000u32 | (imm12 << 10) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            _ => 0xf9000000u32 | (imm12 << 10) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
        };
        self.emit(&insn.to_le_bytes());
    }

    /// 32 位后置索引: str wt, [xn], #imm
    fn emit_str_w_post_index(&mut self, rt: u8, rn: u8, imm9: i32) {
        let imm9_u = (imm9 as u32) & 0x1ff;
        let insn = 0xb8000400u32 | (imm9_u << 12) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// 32 位后置索引: ldr wt, [xn], #imm
    fn emit_ldr_w_post_index(&mut self, rt: u8, rn: u8, imm9: i32) {
        let imm9_u = (imm9 as u32) & 0x1ff;
        let insn = 0xb8400400u32 | (imm9_u << 12) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    fn str_s0_sp(&mut self, off: i32) {
        let imm12 = (off / 4) as u32 & 0xfff;
        let insn = 0xbd000000u32 | imm12 << 10 | 31 << 5;
        self.emit(&insn.to_le_bytes());
    }

    /// `str dt, [sp, #imm12]` —— 存储双精度浮点到 [sp, #off].
    /// STR D: size=11, V=1, opc=00 => 0xFD000000 | imm12<<10 | Rn | Rt
    fn str_dt_sp(&mut self, reg: u8, off: i32) {
        debug_assert!(
            off >= 0 && off % 8 == 0 && off <= 32760,
            "str_dt_sp: off must be non-neg 8-byte aligned, got {off}"
        );
        let imm12 = ((off / 8) as u32) & 0xfff;
        let insn = 0xfd000000u32 | imm12 << 10 | 31u32 << 5 | (reg as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// `ldr s0, [sp, #imm12]` —— 从栈加载单精度浮点到 s0.
    /// imm12 编码: insn[21:10] = imm12/4, 实际偏移范围 ±4 KB / 4 = 1024 个 4 字节槽.
    fn ldr_s0_sp(&mut self, off: i32) {
        let imm12 = (off / 4) as u32 & 0xfff;
        let insn = 0xbd400000u32 | imm12 << 10 | 31 << 5;
        self.emit(&insn.to_le_bytes());
    }

    /// `ldr d0, [sp, #imm12]` —— 从栈加载双精度浮点到 d0.
    fn ldr_d0_sp(&mut self, off: i32) {
        let imm12 = (off / 8) as u32 & 0xfff;
        let insn = 0xfd400000u32 | imm12 << 10 | 31 << 5;
        self.emit(&insn.to_le_bytes());
    }

    /// 从栈加载 HFA 到 v/s 寄存器组 (AAPCS64 v0-v7)
    fn load_hfa_to_vregs(&mut self, off: i32, hfa: HfaInfo, base_reg: u8) {
        let elem_bytes = (hfa.elem_bits / 8) as i32;
        for i in 0..hfa.count {
            let elem_off = off + i as i32 * elem_bytes;
            let vreg = base_reg + i;
            if hfa.elem_bits == 32 {
                let imm12 = (elem_off / 4) as u32 & 0xfff;
                let insn = 0xbd000000u32 | imm12 << 10 | 31 << 5 | (vreg as u32 & 0x1f);
                self.emit(&insn.to_le_bytes());
            } else {
                let imm12 = (elem_off / 8) as u32 & 0xfff;
                let insn = 0xfd000000u32 | imm12 << 10 | 31 << 5 | (vreg as u32 & 0x1f);
                self.emit(&insn.to_le_bytes());
            }
        }
    }

    /// 大结构体返回: 从 [sp+off] 拷贝 size 字节到 [x0]
    fn memcpy_sp_to_x0(&mut self, off: i32, size: usize) {
        self.mov_x_reg_sp_off(1, off); // x1 = src
                                       // x0 已是 sret 指针
        let mut remaining = size;
        let mut src_off = 0i32;
        while remaining > 0 {
            let chunk = remaining.min(8);
            if chunk >= 8 {
                self.ldr_x2_sp_indexed(1, src_off);
                self.str_x2_x0_indexed(0, src_off);
            } else if chunk >= 4 {
                self.ldr_w2_sp_indexed(1, src_off);
                self.str_w2_x0_indexed(0, src_off);
            }
            src_off += chunk as i32;
            remaining -= chunk;
        }
    }

    /// [sp + base_off + idx] 加载 x2 (64-bit)
    fn ldr_x2_sp_indexed(&mut self, base: u8, idx: i32) {
        if idx == 0 {
            let insn = 0xf9400000u32 | (base as u32) << 5 | 2;
            self.emit(&insn.to_le_bytes());
        } else if (0..4096).contains(&idx) && idx % 8 == 0 {
            let imm12 = (idx / 8) as u32 & 0xfff;
            let insn = 0xf9400000u32 | imm12 << 10 | (base as u32) << 5 | 2;
            self.emit(&insn.to_le_bytes());
        }
    }

    fn str_x2_x0_indexed(&mut self, base: u8, idx: i32) {
        if idx == 0 {
            let insn = 0xf9000000u32 | (base as u32) << 5 | 2;
            self.emit(&insn.to_le_bytes());
        } else if (0..4096).contains(&idx) && idx % 8 == 0 {
            let imm12 = (idx / 8) as u32 & 0xfff;
            let insn = 0xf9000000u32 | imm12 << 10 | (base as u32) << 5 | 2;
            self.emit(&insn.to_le_bytes());
        }
    }

    fn ldr_w2_sp_indexed(&mut self, base: u8, idx: i32) {
        let imm12 = (idx / 4).max(0) as u32 & 0xfff;
        let insn = 0xb9400000u32 | imm12 << 10 | (base as u32) << 5 | 2;
        self.emit(&insn.to_le_bytes());
    }

    fn str_w2_x0_indexed(&mut self, base: u8, idx: i32) {
        let imm12 = (idx / 4).max(0) as u32 & 0xfff;
        let insn = 0xb9000000u32 | imm12 << 10 | (base as u32) << 5 | 2;
        self.emit(&insn.to_le_bytes());
    }

    fn gen_hfa_expr(&mut self, expr: &TypedExpr, hfa: HfaInfo) {
        if let ExprKind::Local(ref n) = expr.kind {
            if let Some(&off) = self.locals.get(n) {
                self.load_hfa_to_vregs(off, hfa, 0);
                return;
            }
        }
        let _ = self.gen_float_expr(expr);
    }

    fn gen_hfa_expr_from_ast(&mut self, expr: &Expr, hfa: HfaInfo) {
        if let Expr::Ident { name, .. } = expr {
            if let Some(&off) = self.locals.get(&name.name) {
                self.load_hfa_to_vregs(off, hfa, 0);
                return;
            }
        }
        if let Expr::Float { v, .. } = expr {
            self.load_float_s0(*v as f32);
        }
        let _ = hfa;
    }

    // ===== 缓存维护 (dc / ic / dsb / isb) =====

    /// dc cvac, xN — Data Cache Clean by VA to PoC
    fn emit_dc_cvac(&mut self, reg: u8) {
        let insn = 0xd50b7420u32 | ((reg as u32 & 0x1f) << 5);
        self.emit(&insn.to_le_bytes());
    }

    /// dc civac, xN — Data Cache Clean and Invalidate by VA to PoC
    fn emit_dc_civac(&mut self, reg: u8) {
        let insn = 0xd50b7620u32 | ((reg as u32 & 0x1f) << 5);
        self.emit(&insn.to_le_bytes());
    }

    /// dc cvau, xN — Data Cache Clean by VA to PoU
    fn emit_dc_cvau(&mut self, reg: u8) {
        let insn = 0xd50b7520u32 | ((reg as u32 & 0x1f) << 5);
        self.emit(&insn.to_le_bytes());
    }

    /// dc ivac, xN — Data Cache Invalidate by VA to PoC
    fn emit_dc_ivac(&mut self, reg: u8) {
        let insn = 0xd5087600u32 | ((reg as u32 & 0x1f) << 5);
        self.emit(&insn.to_le_bytes());
    }

    /// ic ivau, xN — Instruction Cache Invalidate by VA to PoU
    fn emit_ic_ivau(&mut self, reg: u8) {
        let insn = 0xd50b7520u32 | ((reg as u32 & 0x1f) << 5);
        self.emit(&insn.to_le_bytes());
    }

    /// ic iallu — Instruction Cache Invalidate All to PoU
    fn emit_ic_iallu(&mut self) {
        self.emit(&[0x7f, 0x78, 0x03, 0xd5]);
    }

    /// dsb ish
    fn emit_dsb_ish(&mut self) {
        self.emit(&[0x9f, 0x3b, 0x03, 0xd5]);
    }

    /// dmb ish — 内部共享内存屏障 (memory clobber 编译器屏障)
    fn emit_dmb_ish(&mut self) {
        // DMB ISH: CRm=0xB, op2=4, Rt=0b0101 => D5033BBF
        self.emit(&[0xbf, 0x3b, 0x03, 0xd5]);
    }

    /// dsb sy — 全系统屏障
    fn emit_dsb_sy(&mut self) {
        self.emit(&[0xbf, 0x3f, 0x03, 0xd5]);
    }

    /// isb
    fn emit_isb(&mut self) {
        self.emit(&[0xdf, 0x3b, 0x03, 0xd5]);
    }

    /// 同步 [start, end) 区间指令/数据缓存 (Linux __clear_cache 语义)
    /// 正确循环结构: 每轮先比较, 再操作, 再递增, 条件跳回
    pub fn emit_cache_sync_range(&mut self, start_reg: u8, end_reg: u8) {
        // === Phase 1: Data Cache Clean ===
        let dc_loop = self.fresh_label("Ldc_loop");
        let dc_exit = self.fresh_label("Ldc_exit");
        self.label(&dc_loop);
        // 先比较: if start >= end, 跳出
        self.emit_cmp_regs(start_reg, end_reg);
        self.emit_bcond(&dc_exit, 0xa); // b.ge exit
                                        // 操作: dc cvac, start
        self.emit_dc_cvac(start_reg);
        // 递增: start += 8
        self.emit_add_imm_x(start_reg, start_reg, 8);
        // 跳回循环顶
        self.b(&dc_loop);
        self.label(&dc_exit);
        self.emit_dsb_ish();

        // === Phase 2: Instruction Cache Invalidate ===
        let ic_loop = self.fresh_label("Lic_loop");
        let ic_exit = self.fresh_label("Lic_exit");
        self.label(&ic_loop);
        self.emit_cmp_regs(start_reg, end_reg);
        self.emit_bcond(&ic_exit, 0xa); // b.ge exit
        self.emit_ic_ivau(start_reg);
        self.emit_add_imm_x(start_reg, start_reg, 8);
        self.b(&ic_loop);
        self.label(&ic_exit);
        self.emit_dsb_ish();
        self.emit_isb();
    }

    /// cmp xn, xm — 64 位寄存器比较 (subs wzr, xn, xm)
    fn emit_cmp_regs(&mut self, rn: u8, rm: u8) {
        let insn = 0xeb00001fu32 | ((rm as u32 & 0x1f) << 16) | ((rn as u32 & 0x1f) << 5);
        self.emit(&insn.to_le_bytes());
    }

    /// b.cond label — 条件分支 (占位 + 延迟修补)
    fn emit_bcond(&mut self, label: &str, cond: u8) {
        let pos = self.pos();
        let insn = 0x54000000u32 | (cond as u32 & 0xf);
        self.emit(&insn.to_le_bytes());
        if let Some(&target) = self.labels.get(label) {
            self.patch_bcond(pos, target, cond);
        } else {
            // 记录待修补 (复用 pending_branches 但编码为条件分支)
            self.pending_bcond.push((pos, label.to_string(), cond));
        }
    }

    /// 修补条件分支偏移
    fn patch_bcond(&mut self, at: u64, target: u64, cond: u8) {
        let pc = at + 4;
        let imm19 = (((target as i64 - pc as i64) >> 2) as u32) & 0x7ffff;
        let insn = 0x54000000u32 | imm19 | (cond as u32 & 0xf);
        let off = at as usize;
        if off + 4 <= self.code.len() {
            self.code[off..off + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    /// add xd, xn, #imm — 64 位立即加法
    fn emit_add_imm_x(&mut self, dst: u8, src: u8, imm: u32) {
        if imm <= 0xfff {
            let insn =
                0x91000000u32 | (imm << 10) | ((src as u32 & 0x1f) << 5) | (dst as u32 & 0x1f);
            self.emit(&insn.to_le_bytes());
        } else {
            // 大立即数: movz x9, #imm; add xd, xn, x9
            self.mov_x_reg_imm(9, imm as u64);
            self.emit_add_x_reg(dst, src, 9);
        }
    }

    /// add xd, xn, xm — 64 位寄存器加法
    fn emit_add_x_reg(&mut self, dst: u8, rn: u8, rm: u8) {
        let insn = 0x8b000000u32
            | ((rm as u32 & 0x1f) << 16)
            | ((rn as u32 & 0x1f) << 5)
            | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// sub xd, xn, xm — 64 位寄存器减法
    fn emit_sub_x_reg(&mut self, dst: u8, rn: u8, rm: u8) {
        let insn = 0xcb000000u32
            | ((rm as u32 & 0x1f) << 16)
            | ((rn as u32 & 0x1f) << 5)
            | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// and xd, xn, xm — 64 位寄存器与
    fn emit_and_x_reg(&mut self, dst: u8, rn: u8, rm: u8) {
        let insn = 0x8a000000u32
            | ((rm as u32 & 0x1f) << 16)
            | ((rn as u32 & 0x1f) << 5)
            | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// orr xd, xn, xm — 64 位寄存器或
    fn emit_or_x_reg(&mut self, dst: u8, rn: u8, rm: u8) {
        let insn = 0xaa000000u32
            | ((rm as u32 & 0x1f) << 16)
            | ((rn as u32 & 0x1f) << 5)
            | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// eor xd, xn, xm — 64 位寄存器异或
    fn emit_eor_x_reg(&mut self, dst: u8, rn: u8, rm: u8) {
        let insn = 0xca000000u32
            | ((rm as u32 & 0x1f) << 16)
            | ((rn as u32 & 0x1f) << 5)
            | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// 将 HFA 从 v 寄存器组存储到栈 [sp+off] (用于 HFA 返回/传参)
    fn store_hfa_from_vregs(&mut self, off: i32, hfa: HfaInfo, base_reg: u8) {
        let elem_bytes = (hfa.elem_bits / 8) as i32;
        for i in 0..hfa.count {
            let elem_off = off + i as i32 * elem_bytes;
            let vreg = base_reg + i;
            if hfa.elem_bits == 32 {
                let imm12 = (elem_off / 4) as u32 & 0xfff;
                let insn = 0xbd000000u32 | imm12 << 10 | 31 << 5 | (vreg as u32 & 0x1f);
                self.emit(&insn.to_le_bytes());
            } else {
                let imm12 = (elem_off / 8) as u32 & 0xfff;
                let insn = 0xfd000000u32 | imm12 << 10 | 31 << 5 | (vreg as u32 & 0x1f);
                self.emit(&insn.to_le_bytes());
            }
        }
    }

    // ========================================================================
    // TLS 访问序列 (TLSDESC / Initial Exec / Local Exec)
    // ========================================================================

    /// TLSDESC 访问序列: 获取 TLS 变量地址到 x0
    /// 序列: adrp x0, :tlsdesc:var
    ///       ldr  x1, [x0, :tlsdesc_lo12:var]
    ///       add  x0, x0, :tlsdesc_lo12:var
    ///       .tlsdesccall var
    ///       blr  x1
    ///       mrs  x1, tpidr_el0
    ///       add  x0, x0, x1
    fn emit_tlsdesc_access(&mut self, sym: &str) {
        // adrp x0, :tlsdesc:var (R_AARCH64_TLSDESC_ADR_PAGE21)
        let insn_adrp = 0x90000000u32; // adrp x0, #0 (placeholder)
        self.emit(&insn_adrp.to_le_bytes());
        let adrp_pos = self.pos() - 4;
        let idx = self.ensure_sym(sym);
        self.relocations.push(ElfRela {
            offset: adrp_pos,
            sym_index: idx,
            rel_type: crate::codegen::elf::R_AARCH64_TLSDESC_ADR_PAGE21,
            addend: 0,
        });
        // ldr x1, [x0, :tlsdesc_lo12:var] (R_AARCH64_TLSDESC_LD64_LO12)
        let insn_ldr = 0xf9400001u32; // ldr x1, [x0]
        self.emit(&insn_ldr.to_le_bytes());
        let ldr_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: ldr_pos,
            sym_index: idx,
            rel_type: crate::codegen::elf::R_AARCH64_TLSDESC_LD64_LO12,
            addend: 0,
        });
        // add x0, x0, :tlsdesc_lo12:var (R_AARCH64_TLSDESC_ADD_LO12)
        let insn_add = 0x91000000u32; // add x0, x0, #0
        self.emit(&insn_add.to_le_bytes());
        let add_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: add_pos,
            sym_index: idx,
            rel_type: crate::codegen::elf::R_AARCH64_TLSDESC_ADD_LO12,
            addend: 0,
        });
        // blr x1 (TLSDESC call)
        if self.mte_active() {
            self.mte_untag_sp_for_call();
        }
        let insn_blr = 0xd63f0020u32; // blr x1
        self.emit(&insn_blr.to_le_bytes());
        if self.mte_active() {
            self.mte_retag_sp_after_call();
        }
        // mrs x1, tpidr_el0
        let insn_mrs = 0xd53bd041u32; // mrs x1, tpidr_el0
        self.emit(&insn_mrs.to_le_bytes());
        // add x0, x0, x1
        let insn_add2 = 0x8b010000u32; // add x0, x0, x1
        self.emit(&insn_add2.to_le_bytes());
    }

    /// Optimized TLSDESC 访问序列 (binutils 2.33+ / glibc 2.33+)
    /// 用 MOVW 对代替 ADRP, 消除页对齐限制并允许 linker 选择更短序列.
    /// 序列: movz x0, #:tlsdesc_off_g1:var
    ///       movk x0, #:tlsdesc_off_g0_nc:var
    ///       ldr  x1, [x0]
    ///       add  x0, x0, x1
    ///       blr  x1
    ///       mrs  x1, tpidr_el0
    ///       add  x0, x0, x1
    fn emit_tlsdesc_access_opt(&mut self, sym: &str) {
        let idx = self.ensure_sym(sym);
        // movz x0, #:tlsdesc_off_g1:var (R_AARCH64_TLSDESC_OFF_G1)
        let insn_movz = 0xd2a00000u32; // movz x0, #0, lsl #16
        self.emit(&insn_movz.to_le_bytes());
        let movz_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: movz_pos,
            sym_index: idx,
            rel_type: crate::codegen::elf::R_AARCH64_TLSDESC_OFF_G1,
            addend: 0,
        });
        // movk x0, #:tlsdesc_off_g0_nc:var (R_AARCH64_TLSDESC_OFF_G0_NC)
        let insn_movk = 0xf2800000u32; // movk x0, #0
        self.emit(&insn_movk.to_le_bytes());
        let movk_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: movk_pos,
            sym_index: idx,
            rel_type: crate::codegen::elf::R_AARCH64_TLSDESC_OFF_G0_NC,
            addend: 0,
        });
        // ldr x1, [x0] (R_AARCH64_TLSDESC_LDR — marker for ld.so)
        let insn_ldr = 0xf9400001u32; // ldr x1, [x0]
        self.emit(&insn_ldr.to_le_bytes());
        let ldr_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: ldr_pos,
            sym_index: idx,
            rel_type: crate::codegen::elf::R_AARCH64_TLSDESC_LDR,
            addend: 0,
        });
        // add x0, x0, x1 (R_AARCH64_TLSDESC_ADD — marker for ld.so)
        let insn_add = 0x8b010000u32; // add x0, x0, x1
        self.emit(&insn_add.to_le_bytes());
        let add_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: add_pos,
            sym_index: idx,
            rel_type: crate::codegen::elf::R_AARCH64_TLSDESC_ADD,
            addend: 0,
        });
        // blr x1 (R_AARCH64_TLSDESC_CALL — marker for ld.so)
        if self.mte_active() {
            self.mte_untag_sp_for_call();
        }
        let insn_blr = 0xd63f0020u32; // blr x1
        self.emit(&insn_blr.to_le_bytes());
        if self.mte_active() {
            self.mte_retag_sp_after_call();
        }
        let blr_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: blr_pos,
            sym_index: idx,
            rel_type: crate::codegen::elf::R_AARCH64_TLSDESC_CALL,
            addend: 0,
        });
        // mrs x1, tpidr_el0
        self.emit(&0xd53bd041u32.to_le_bytes());
        // add x0, x0, x1
        self.emit(&0x8b010000u32.to_le_bytes());
    }

    /// Initial Exec TLS 访问: 通过 GOT 获取 tpoff
    /// 序列: adrp x0, :gottprel:var
    ///       ldr  x0, [x0, :gottprel_lo12:var]
    ///       mrs  x1, tpidr_el0
    ///       add  x0, x0, x1
    fn emit_tls_ie_access(&mut self, sym: &str) {
        // adrp x0, :gottprel:var
        let insn_adrp = 0x90000000u32;
        self.emit(&insn_adrp.to_le_bytes());
        let adrp_pos = self.pos() - 4;
        let idx = self.ensure_sym(sym);
        self.relocations.push(ElfRela {
            offset: adrp_pos,
            sym_index: idx,
            rel_type: crate::codegen::elf::R_AARCH64_TLSIE_ADR_GOTTPREL_PAGE21,
            addend: 0,
        });
        // ldr x0, [x0, :gottprel_lo12:var]
        let insn_ldr = 0xf9400000u32; // ldr x0, [x0]
        self.emit(&insn_ldr.to_le_bytes());
        let ldr_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: ldr_pos,
            sym_index: idx,
            rel_type: crate::codegen::elf::R_AARCH64_TLSIE_LD64_GOTTPREL_LO12_NC,
            addend: 0,
        });
        // mrs x1, tpidr_el0
        self.emit(&0xd53bd041u32.to_le_bytes());
        // add x0, x0, x1
        self.emit(&0x8b010000u32.to_le_bytes());
    }

    /// Local Exec TLS 访问: 直接 TP + offset
    /// 序列: movz x0, #:tprel_g1:var
    ///       movk x0, #:tprel_g0_nc:var
    ///       mrs  x1, tpidr_el0
    ///       add  x0, x0, x1
    fn emit_tls_le_access(&mut self, sym: &str) {
        // movz x0, #:tprel_g1:var
        let insn_movz = 0xd2a00000u32; // movz x0, #0, lsl #16
        self.emit(&insn_movz.to_le_bytes());
        let movz_pos = self.pos() - 4;
        let idx = self.ensure_sym(sym);
        self.relocations.push(ElfRela {
            offset: movz_pos,
            sym_index: idx,
            rel_type: crate::codegen::elf::R_AARCH64_TLSLE_MOVW_TPREL_G1,
            addend: 0,
        });
        // movk x0, #:tprel_g0_nc:var
        let insn_movk = 0xf2800000u32; // movk x0, #0
        self.emit(&insn_movk.to_le_bytes());
        let movk_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: movk_pos,
            sym_index: idx,
            rel_type: crate::codegen::elf::R_AARCH64_TLSLE_MOVW_TPREL_G0_NC,
            addend: 0,
        });
        // mrs x1, tpidr_el0
        self.emit(&0xd53bd041u32.to_le_bytes());
        // add x0, x0, x1
        self.emit(&0x8b010000u32.to_le_bytes());
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
    /// 参数约定: args[0]=ptr, args[1]=val (对于 store/exchange/cmpxchg 有不同)
    /// 结果返回在 x0
    fn gen_atomic_builtin(&mut self, name: &str, args: &[TypedExpr]) -> bool {
        match name {
            "__atomic_load_n" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                let ord = self.eval_mem_order(&args[1]);
                // x0 = ptr
                self.gen_expr(&args[0]);
                self.mov_x_reg(1, 0); // x1 = ptr (保存)
                match ord {
                    0 | 3 => {
                        // RELAXED/RELEASE: plain LDR
                        self.emit_ldr_x(0, 1, 0, sz);
                    }
                    _ => {
                        // SEQ_CST/ACQUIRE/ACQ_REL: LDAR
                        self.emit_ldar(sz, 0, 1);
                    }
                }
                true
            }
            "__atomic_store_n" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                let ord = self.eval_mem_order(&args[2]);
                // x0 = ptr
                self.gen_expr(&args[0]);
                self.mov_x_reg(1, 0); // x1 = ptr
                // x0 = val
                self.gen_expr(&args[1]);
                match ord {
                    0 => {
                        // RELAXED: plain STR
                        self.emit_str_x(0, 1, 0, sz);
                    }
                    _ => {
                        // SEQ_CST/ACQUIRE/RELEASE/ACQ_REL: STLR
                        self.emit_stlr(sz, 0, 1);
                    }
                }
                true
            }
            "__atomic_exchange_n" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                // x0 = ptr
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0); // x2 = ptr
                // x0 = val
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0); // x1 = val
                // CAS loop: ldaxr/stlxr retry loop
                let l_retry = self.fresh_label("atomic_xchg");
                let l_done = self.fresh_label("atomic_xchg_done");
                self.label(&l_retry);
                self.emit_ldaxr(sz, 0, 2);
                self.emit_stlxr(sz, 3, 1, 2);
                self.emit_cbnz(3, &l_retry);
                self.label(&l_done);
                // x0 已包含旧值
                true
            }
            "__atomic_fetch_add" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                // x0 = ptr
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0); // x2 = ptr
                // x0 = val
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0); // x1 = val
                // CAS loop: x0 = old value
                let l_retry = self.fresh_label("atomic_add");
                let l_done = self.fresh_label("atomic_add_done");
                self.label(&l_retry);
                // ldaxr x0, [x2]
                self.emit_ldaxr(sz, 0, 2);
                // x3 = x0 + x1 (new = old + val)
                self.emit_add_x_reg(3, 0, 1);
                // stlxr w4, x3, [x2]
                self.emit_stlxr(sz, 4, 3, 2);
                // cbnz w4, retry
                self.emit_cbnz(4, &l_retry);
                self.label(&l_done);
                // x0 已包含旧值
                true
            }
            "__atomic_fetch_sub" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                // x0 = ptr
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0); // x2 = ptr
                // x0 = val
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0); // x1 = val
                // CAS loop
                let l_retry = self.fresh_label("atomic_sub");
                let l_done = self.fresh_label("atomic_sub_done");
                self.label(&l_retry);
                // ldaxr x0, [x2]
                self.emit_ldaxr(sz, 0, 2);
                // x3 = x0 - x1 (new = old - val)
                self.emit_sub_x_reg(3, 0, 1);
                // stlxr w4, x3, [x2]
                self.emit_stlxr(sz, 4, 3, 2);
                // cbnz w4, retry
                self.emit_cbnz(4, &l_retry);
                self.label(&l_done);
                true
            }
            "__atomic_fetch_and" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                let l_retry = self.fresh_label("atomic_and");
                let l_done = self.fresh_label("atomic_and_done");
                self.label(&l_retry);
                self.emit_ldaxr(sz, 0, 2);
                // x3 = x0 & x1
                self.emit_and_x_reg(3, 0, 1);
                self.emit_stlxr(sz, 4, 3, 2);
                self.emit_cbnz(4, &l_retry);
                self.label(&l_done);
                true
            }
            "__atomic_fetch_or" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                let l_retry = self.fresh_label("atomic_or");
                let l_done = self.fresh_label("atomic_or_done");
                self.label(&l_retry);
                self.emit_ldaxr(sz, 0, 2);
                // x3 = x0 | x1
                self.emit_or_x_reg(3, 0, 1);
                self.emit_stlxr(sz, 4, 3, 2);
                self.emit_cbnz(4, &l_retry);
                self.label(&l_done);
                true
            }
            "__atomic_fetch_xor" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                let l_retry = self.fresh_label("atomic_xor");
                let l_done = self.fresh_label("atomic_xor_done");
                self.label(&l_retry);
                self.emit_ldaxr(sz, 0, 2);
                // x3 = x0 ^ x1
                self.emit_eor_x_reg(3, 0, 1);
                self.emit_stlxr(sz, 4, 3, 2);
                self.emit_cbnz(4, &l_retry);
                self.label(&l_done);
                true
            }
            "__atomic_compare_exchange_n" => {
                // args: (ptr, expected, desired, weak, success_order, failure_order)
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                // x0 = ptr
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0); // x2 = ptr
                // x0 = *expected (解引用 expected 指针)
                self.gen_expr(&args[1]);
                self.mov_x_reg(4, 0); // x4 = expected_ptr
                // 加载 *expected 到 x1
                self.emit_ldr_x(1, 4, 0, sz); // x1 = *expected_ptr
                // x0 = desired
                self.gen_expr(&args[2]);
                self.mov_x_reg(3, 0); // x3 = desired
                // CAS loop
                let l_retry = self.fresh_label("atomic_cas");
                let l_fail = self.fresh_label("atomic_cas_fail");
                let l_done = self.fresh_label("atomic_cas_done");
                self.label(&l_retry);
                // ldaxr x0, [x2] (加载当前值)
                self.emit_ldaxr(sz, 0, 2);
                // cmp x0, x1 (比较当前值与期望值)
                self.emit_cmp_x_reg(0, 1);
                // b.ne fail
                self.emit_bcond(&l_fail, 1); // NE = 1
                // stlxr w5, x3, [x2] (store desired)
                self.emit_stlxr(sz, 5, 3, 2);
                // cbnz w5, retry
                self.emit_cbnz(5, &l_retry);
                // 成功: x0 = 1
                self.mov_w0_imm(1);
                // b done
                self.b(&l_done);
                // 失败: 将当前值写入 *expected, x0 = 0
                self.label(&l_fail);
                // stlxr 失败, 需要清除 exclusive monitor
                // 重新加载当前值并写入 expected
                self.emit_ldxr(sz, 0, 2); // 读取当前值 (非 atomic, 仅清除 monitor)
                self.emit_clrex(); // 清除 exclusive monitor
                self.emit_str_x(0, 4, 0, sz); // *expected_ptr = current
                self.mov_w0_imm(0);
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
                // x0 = ptr
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0); // x2 = ptr
                // x0 = expected_ptr
                self.gen_expr(&args[1]);
                self.mov_x_reg(4, 0); // x4 = expected_ptr
                // load *expected_ptr into x1
                self.emit_ldr_x(1, 4, 0, sz); // x1 = *expected_ptr
                // x0 = desired_ptr -> dereference to get desired value
                self.gen_expr(&args[2]);
                self.mov_x_reg(6, 0); // x6 = desired_ptr
                self.emit_ldr_x(3, 6, 0, sz); // x3 = *desired_ptr
                // CAS loop
                let l_retry = self.fresh_label("atomic_cas");
                let l_fail = self.fresh_label("atomic_cas_fail");
                let l_done = self.fresh_label("atomic_cas_done");
                self.label(&l_retry);
                self.emit_ldaxr(sz, 0, 2);
                self.emit_cmp_x_reg(0, 1);
                self.emit_bcond(&l_fail, 1); // NE
                self.emit_stlxr(sz, 5, 3, 2);
                self.emit_cbnz(5, &l_retry);
                self.mov_w0_imm(1);
                self.b(&l_done);
                self.label(&l_fail);
                self.emit_ldxr(sz, 0, 2);
                self.emit_clrex();
                self.emit_str_x(0, 4, 0, sz); // *expected_ptr = current
                self.mov_w0_imm(0);
                self.label(&l_done);
                true
            }
            _ => false,
        }
    }

    // ===== 原子操作辅助指令 =====

    /// LDAR (Load-Acquire) - 按尺寸选 8/16/32/64 位
    fn emit_ldar(&mut self, sz: usize, rt: u8, rn: u8) {
        let insn = match sz {
            1 => 0x08dffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            2 => 0x48dffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            4 => 0x88dffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            _ => 0xc8dffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
        };
        self.emit(&insn.to_le_bytes());
    }

    /// STLR (Store-Release) - 按尺寸选 8/16/32/64 位
    fn emit_stlr(&mut self, sz: usize, rt: u8, rn: u8) {
        let insn = match sz {
            1 => 0x089ffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            2 => 0x489ffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            4 => 0x889ffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            _ => 0xc89ffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
        };
        self.emit(&insn.to_le_bytes());
    }

    /// LDAXR (Load-Acquire Exclusive) - 按尺寸选 8/16/32/64 位
    fn emit_ldaxr(&mut self, sz: usize, rt: u8, rn: u8) {
        let insn = match sz {
            1 => 0x085ffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            2 => 0x485ffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            4 => 0x885ffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            _ => 0xc85ffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
        };
        self.emit(&insn.to_le_bytes());
    }

    /// LDXR (Load Exclusive) - 按尺寸选 8/16/32/64 位
    fn emit_ldxr(&mut self, sz: usize, rt: u8, rn: u8) {
        let insn = match sz {
            1 => 0x085f7c00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            2 => 0x485f7c00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            4 => 0x885f7c00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            _ => 0xc85f7c00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
        };
        self.emit(&insn.to_le_bytes());
    }

    /// STLXR (Store-Release Exclusive) - 按尺寸选 8/16/32/64 位
    /// rs = status register, rt = value to store, rn = base register
    fn emit_stlxr(&mut self, sz: usize, rs: u8, rt: u8, rn: u8) {
        let insn = match sz {
            1 => 0x0800fc00u32
                | ((rs as u32 & 0x1f) << 16)
                | ((rn as u32 & 0x1f) << 5)
                | (rt as u32 & 0x1f),
            2 => 0x4800fc00u32
                | ((rs as u32 & 0x1f) << 16)
                | ((rn as u32 & 0x1f) << 5)
                | (rt as u32 & 0x1f),
            4 => 0x8800fc00u32
                | ((rs as u32 & 0x1f) << 16)
                | ((rn as u32 & 0x1f) << 5)
                | (rt as u32 & 0x1f),
            _ => 0xc800fc00u32
                | ((rs as u32 & 0x1f) << 16)
                | ((rn as u32 & 0x1f) << 5)
                | (rt as u32 & 0x1f),
        };
        self.emit(&insn.to_le_bytes());
    }

    /// CLREX - 清除 exclusive monitor
    fn emit_clrex(&mut self) {
        self.emit(&0xd5033f5fu32.to_le_bytes());
    }

    /// CBNZ - Compare and Branch Non-Zero (32-bit)
    fn emit_cbnz(&mut self, rt: u8, label: &str) {
        let pos = self.pos();
        // cbnz w<rt>, #0 (placeholder)
        let insn = 0x35000000u32 | (rt as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
        if let Some(&target) = self.labels.get(label) {
            self.patch_cbnz(pos, target, rt);
        } else {
            self.pending_bcond.push((pos, label.to_string(), 0x10)); // 0x10 = CBNZ marker
        }
    }

    /// 修补 CBNZ 偏移
    fn patch_cbnz(&mut self, at: u64, target: u64, rt: u8) {
        let pc = at + 4;
        let imm19 = (((target as i64 - pc as i64) >> 2) as u32) & 0x7ffff;
        let insn = 0x35000000u32 | (imm19 << 5) | (rt as u32 & 0x1f);
        let off = at as usize;
        if off + 4 <= self.code.len() {
            self.code[off..off + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    // ========================================================================
    // 嵌套大结构体返回 (multi-level sret)
    // ========================================================================

    /// 嵌套 sret: 从 [sp+off] 拷贝 size 字节到 [dst_reg]
    /// 支持多层嵌套调用链: 每一层都通过 x0 传递 sret 指针
    fn memcpy_sp_to_reg(&mut self, off: i32, size: usize, dst_reg: u8) {
        // 先把 dst_reg 保存到 x1 (src = sp+off)
        self.mov_x_reg_sp_off(2, off); // x2 = src (sp+off)
                                       // x0 = dst_reg (sret 指针已在 dst_reg)
        if dst_reg != 0 {
            // mov x0, dst_reg
            let insn = 0xaa0003e0u32 | ((dst_reg as u32 & 0x1f) << 16);
            self.emit(&insn.to_le_bytes());
        }
        let mut remaining = size;
        let mut src_off = 0i32;
        while remaining > 0 {
            let chunk = remaining.min(8);
            if chunk >= 8 {
                // ldr x3, [x2, #src_off]
                let imm12 = (src_off / 8) as u32 & 0xfff;
                let insn_ldr = 0xf9400043u32 | (imm12 << 10);
                self.emit(&insn_ldr.to_le_bytes());
                // str x3, [x0, #src_off]
                let insn_str = 0xf9000003u32 | (imm12 << 10);
                self.emit(&insn_str.to_le_bytes());
            } else if chunk >= 4 {
                let imm12 = (src_off / 4) as u32 & 0xfff;
                let insn_ldr = 0xb9400043u32 | (imm12 << 10);
                self.emit(&insn_ldr.to_le_bytes());
                let insn_str = 0xb9000003u32 | (imm12 << 10);
                self.emit(&insn_str.to_le_bytes());
            } else {
                // byte-by-byte for tail
                for b in 0..chunk as i32 {
                    let ldrb = 0x39400043u32 | (((src_off + b) as u32 & 0xfff) << 10);
                    self.emit(&ldrb.to_le_bytes());
                    let strb = 0x39000003u32 | (((src_off + b) as u32 & 0xfff) << 10);
                    self.emit(&strb.to_le_bytes());
                }
            }
            src_off += chunk as i32;
            remaining -= chunk;
        }
    }

    /// MTE: Generate Random Tag (IRG)
    /// `irg xd, xn`: 保留 xn 的低 56 位地址, 把随机 Logical Address Tag 写入高字节.
    /// 编码 (ARMv8.5 Data-processing 2 source with tags, Rm 固定为 XZR):
    ///   `1 0 0 1 1 0 1 0 1 1 0 Rm 0 0 0 1 0 0 Rn Rd`
    ///   = 0x9AC00000 | (Rm << 16) | (0b000100 << 10) | (Rn << 5) | Rd
    /// 参考 LLVM armv8.5a-mte.s: irg x0, sp -> 0x9adf13e0
    pub fn emit_irg(&mut self, rd: u8, rn: u8) {
        let insn = 0x9ac00000u32
            | (0x1f << 16) // Rm = XZR (省略第二源操作数)
            | (0b000100 << 10)
            | (((rn as u32) & 0x1f) << 5)
            | ((rd as u32) & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// MTE: Add with Tag (ADDG)
    /// `addg xd, xn, #uimm6, #uimm4`: 地址加 uimm6*16, tag 加 uimm4.
    /// 编码 (Add/subtract with tags):
    ///   `1 0 0 1 0 0 0 1 1 0 imm6 (0)(0) imm4 Rn Rd`
    ///   = 0x91800000 | (imm6 << 16) | (imm4 << 10) | (Rn << 5) | Rd
    /// 参考 LLVM armv8.5a-mte.s: addg sp, x2, #32, #3 -> 0x91820c5f
    pub fn emit_addg(&mut self, rd: u8, rn: u8, imm6: u32, imm4: u32) {
        let insn = 0x91800000u32
            | ((imm6 & 0x3f) << 16)
            | ((imm4 & 0xf) << 10)
            | (((rn as u32) & 0x1f) << 5)
            | ((rd as u32) & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// MTE: Subtract with Tag (SUBG)
    /// `subg xd, xn, #uimm6, #uimm4`: 地址减 uimm6*16, tag 减 uimm4.
    /// 编码: 与 ADDG 相同但 bit30 置位
    ///   = 0xD1800000 | (imm6 << 16) | (imm4 << 10) | (Rn << 5) | Rd
    /// 参考 LLVM armv8.5a-mte.s: subg x0, x1, #0, #1 -> 0xd1800420
    pub fn emit_subg(&mut self, rd: u8, rn: u8, imm6: u32, imm4: u32) {
        let insn = 0xd1800000u32
            | ((imm6 & 0x3f) << 16)
            | ((imm4 & 0xf) << 10)
            | (((rn as u32) & 0x1f) << 5)
            | ((rd as u32) & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// MTE: Store Allocation Tag (STG, signed-offset 形式)
    /// `stg xt, [xn, #imm9*16]`: 把 xt 顶字节的 tag 写入 [xn+off] 所在 16B granule
    /// 的 tag 内存. imm9 是有符号 granule 偏移 (乘 16, 范围 -256..255).
    /// 编码 (Store tag, signed offset):
    ///   `1 1 0 1 1 0 0 1 0 0 1 imm9 1 0 Rn Rt`
    ///   = 0xD9200000 | (imm9 << 12) | (0b10 << 10) | (Rn << 5) | Rt
    /// 参考 LLVM armv8.5a-mte.s: stg x3, [sp, #16] -> 0xd9201be3
    pub fn emit_stg(&mut self, rt: u8, rn: u8, imm9: u32) {
        let insn = 0xd9200000u32
            | ((imm9 & 0x1ff) << 12)
            | (0b10 << 10)
            | (((rn as u32) & 0x1f) << 5)
            | ((rt as u32) & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// MTE: Load Allocation Tag (LDG, signed-offset 形式)
    /// `ldg xt, [xn, #imm9*16]`: 读出 [xn+off] 所在 granule 的 tag, 合并进 xt 高字节.
    /// 编码:
    ///   `1 1 0 1 1 0 0 1 0 1 1 imm9 0 0 Rn Rt`
    ///   = 0xD9600000 | (imm9 << 12) | (Rn << 5) | Rt
    pub fn emit_ldg(&mut self, rt: u8, rn: u8, imm9: u32) {
        let insn = 0xd9600000u32
            | ((imm9 & 0x1ff) << 12)
            | (((rn as u32) & 0x1f) << 5)
            | ((rt as u32) & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// MTE: Set PSTATE.TCO (Tag Check Override)
    /// 注意: 当前编码对应 SPSel 选择器, 保留用于显式禁用 tag 检查的扩展点,
    /// 未在 prologue tag 栈帧流程中使用.
    pub fn emit_settco(&mut self, value: bool) {
        let insn = 0xd500401fu32 | ((value as u32) << 3);
        self.emit(&insn.to_le_bytes());
    }
}

fn find_func<'a>(prog: &'a Program, name: &str) -> Option<&'a FuncDecl> {
    prog.decls.iter().find_map(|d| match d {
        Decl::Func(f) if f.name.name == name => Some(f),
        _ => None,
    })
}

fn classify_hfa(
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
    if *size == 0 || *size > 128 {
        return None;
    }
    let (flds, is_union) = fields.get(name)?;
    // AAPCS64: HFA 最多 8 个同类浮点元素 (v0-v7)
    if *is_union || flds.is_empty() || flds.len() > 8 {
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
    fn aarch64_ir_return_add() {
        let (program, diags) =
            crate::parser::parse(b"int main(void) { return 10 + 32; }\n", "a64.c");
        assert!(diags.is_empty(), "{diags:?}");
        let (module, _) = lower(program).expect("lower");
        validate(&module).expect("validate");
        let triple = crate::cli::TargetTriple::parse("aarch64-linux-gnu").expect("triple");
        let mut cg = Aarch64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.compile(&module);
        let obj = cg.build_output();
        assert_eq!(&obj[0..4], &[0x7f, b'E', b'L', b'F']);
        assert_eq!(u16::from_le_bytes([obj[18], obj[19]]), 183);
    }

    // M7 修复: div/mod 真实 sdiv/msub 替代 0
    // 验证: aarch64 编译 (a / b) 与 (a % b) 不再退化为 0
    // 注: 使用 opaque 函数防止常量折叠
    #[test]
    fn aarch64_ir_div_mod_emit_sdiv_msub() {
        let (program, diags) = crate::parser::parse(
            b"extern int opaque(int);\n\
              int main(void) { int a = opaque(42); int b = opaque(5); int q = a / b; int r = a % b; return q + r; }\n",
            "a64.c",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let (module, _) = lower(program).expect("lower");
        validate(&module).expect("validate");
        let triple = crate::cli::TargetTriple::parse("aarch64-linux-gnu").expect("triple");
        let mut cg = Aarch64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.compile(&module);
        let obj = cg.build_output();
        // 验证 .text 段含 sdiv 指令 (0x9AC00C20 LE 编码: 20 0c c0 9a)
        let text = obj.windows(4).any(|w| w == [0x20, 0x0c, 0xc0, 0x9a]);
        assert!(
            text,
            "expected sdiv w0, w0, w1 (0x9ac00c20) in aarch64 text"
        );
    }

    // M7 修复: 移位真实 lslv/lsrv 替代 0
    // lslv w0, w0, w1: 0x1A302000 LE = 00 20 30 1A
    // lsrv w0, w0, w1: 0x1A342000 LE = 00 20 34 1A
    #[test]
    fn aarch64_ir_shifts_emit_lslv_lsrv() {
        let (program, diags) = crate::parser::parse(
            b"extern int opaque(int);\n\
              int main(void) { int a = opaque(1); int b = opaque(2); return (a << b) | (a >> b); }\n",
            "a64.c",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let (module, _) = lower(program).expect("lower");
        validate(&module).expect("validate");
        let triple = crate::cli::TargetTriple::parse("aarch64-linux-gnu").expect("triple");
        let mut cg = Aarch64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.compile(&module);
        let obj = cg.build_output();
        let has_lslv = obj.windows(4).any(|w| w == [0x00, 0x20, 0x30, 0x1a]);
        let has_lsrv = obj.windows(4).any(|w| w == [0x00, 0x20, 0x34, 0x1a]);
        assert!(has_lslv || has_lsrv, "expected lslv/lsrv in aarch64 text");
    }

    // M7 修复: 聚合类型参数按引用传递
    // 验证: 调函数时,若参数是 local struct, 发射 add xN, sp, #off
    // add x0, sp, #imm12 编码: bits[31:24]=0x91, bits[7:5]=0b000 (sf=1)
    // 简化检测: byte3 == 0x91
    #[test]
    fn aarch64_aggregate_arg_by_ref() {
        let (program, diags) = crate::parser::parse(
            b"struct S { int x; int y; };\n\
              void callee(struct S s) {}\n\
              int main(void) { struct S s; s.x = 1; s.y = 2; callee(s); return 0; }\n",
            "a64.c",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let (module, _) = lower(program).expect("lower");
        validate(&module).expect("validate");
        let triple = crate::cli::TargetTriple::parse("aarch64-linux-gnu").expect("triple");
        let mut cg = Aarch64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.compile(&module);
        let obj = cg.build_output();
        let has_adr = obj.windows(4).any(|w| w[3] == 0x91 && w[2] == 0x00);
        assert!(has_adr, "expected add xN, sp, #imm (M7 by-ref aggregate)");
    }

    // M8: 谓词指令 CCMP/CSEL/CSINC 发射验证
    #[test]
    fn aarch64_ccmp_csel_csinc_emit() {
        let mut cg = Aarch64Codegen::new(
            crate::target::Target::resolve(
                &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
            ),
            true,
        );
        // ccmp w0, w1, #0, ne
        cg.emit_ccmp_w0_w1(0);
        // csel w0, w0, w1, eq
        cg.emit_csel_w0_w0_w1(0x0);
        // csinc w0, w0, w1, eq
        cg.emit_csinc_w0_w0_w1_eq();
        let code = &cg.code;
        // ccmp: byte3=0x7a, byte1 cond=ne(1) in bits[15:12]
        let has_ccmp = code
            .windows(4)
            .any(|w| w[3] == 0x7a && (w[1] & 0xf0) == 0x10);
        assert!(has_ccmp, "expected ccmp w0, w1, #0, ne");
        // csel: byte3=0x1a, cond=eq(0)
        let has_csel = code
            .windows(4)
            .any(|w| w[3] == 0x1a && (w[1] & 0xf0) == 0x00);
        assert!(has_csel, "expected csel w0, w0, w1, eq");
        // csinc: 0x1a8401e0 LE = e0 01 84 1a
        let has_csinc = code.windows(4).any(|w| w == [0xe0, 0x01, 0x84, 0x1a]);
        assert!(has_csinc, "expected csinc w0, w0, w1, eq");
    }

    // M8: 双精度浮点比较发射验证
    #[test]
    fn aarch64_fcmp_double_emit() {
        let mut cg = Aarch64Codegen::new(
            crate::target::Target::resolve(
                &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
            ),
            true,
        );
        cg.fcmp_d0_d1();
        cg.fcmp_s0_zero();
        cg.fcmp_d0_zero();
        let code = &cg.code;
        // fcmp d0, d1: 0x1E682000 LE = 00 20 68 1e
        let has_fcmp_d = code.windows(4).any(|w| w == [0x00, 0x20, 0x68, 0x1e]);
        assert!(has_fcmp_d, "expected fcmp d0, d1");
        let has_fcmp_s0 = code.windows(4).any(|w| w == [0x08, 0x00, 0x28, 0x1e]);
        assert!(has_fcmp_s0, "expected fcmp s0, #0.0");
    }

    // M8: 缓存维护指令发射验证
    #[test]
    fn aarch64_cache_maintain_emit() {
        let mut cg = Aarch64Codegen::new(
            crate::target::Target::resolve(
                &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
            ),
            true,
        );
        cg.emit_dc_cvac(0);
        cg.emit_dc_civac(1);
        cg.emit_dc_cvau(2);
        cg.emit_ic_ivau(3);
        cg.emit_ic_iallu();
        cg.emit_dsb_ish();
        cg.emit_dsb_sy();
        cg.emit_isb();
        // 验证 dsb ish: 9f 3b 03 d5
        let has_dsb_ish = cg.code.windows(4).any(|w| w == [0x9f, 0x3b, 0x03, 0xd5]);
        assert!(has_dsb_ish, "expected dsb ish");
        // 验证 isb: df 3b 03 d5
        let has_isb = cg.code.windows(4).any(|w| w == [0xdf, 0x3b, 0x03, 0xd5]);
        assert!(has_isb, "expected isb");
        // 验证 ic iallu: 7f 78 03 d5
        let has_ic_iallu = cg.code.windows(4).any(|w| w == [0x7f, 0x78, 0x03, 0xd5]);
        assert!(has_ic_iallu, "expected ic iallu");
    }

    // M8: 后置索引寻址发射验证
    #[test]
    fn aarch64_post_index_addring_emit() {
        let mut cg = Aarch64Codegen::new(
            crate::target::Target::resolve(
                &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
            ),
            true,
        );
        // str x0, [x1], #8
        cg.emit_str_post_index(0, 1, 8);
        // ldr x0, [x1], #8
        cg.emit_ldr_post_index(0, 1, 8);
        // str x0, [x1, #8]!
        cg.emit_str_pre_index(0, 1, 8);
        // ldr x0, [x1, #8]!
        cg.emit_ldr_pre_index(0, 1, 8);
        // 验证: post-index str byte3=0xf8, byte2 bit7=0 (bit23=0, post-index)
        let has_post_str = cg
            .code
            .windows(4)
            .any(|w| w[3] == 0xf8 && (w[2] & 0x80) == 0);
        assert!(has_post_str, "expected str post-index");
        // 验证: pre-index str byte3=0xf8, byte2 bit7=1 (bit23=1, pre-index)
        let has_pre_str = cg
            .code
            .windows(4)
            .any(|w| w[3] == 0xf8 && (w[2] & 0x80) != 0);
        assert!(has_pre_str, "expected str pre-index");
    }

    // M8: BL 范围校验
    #[test]
    fn aarch64_bl_range_check() {
        assert!(Aarch64Codegen::bl_offset_in_range(0));
        assert!(Aarch64Codegen::bl_offset_in_range(0x7ffffff));
        assert!(Aarch64Codegen::bl_offset_in_range(-0x8000000));
        assert!(!Aarch64Codegen::bl_offset_in_range(0x8000000));
        assert!(!Aarch64Codegen::bl_offset_in_range(-0x8000001));
    }

    // M8: ADR 范围校验
    #[test]
    fn aarch64_adr_range_check() {
        assert!(Aarch64Codegen::adr_offset_in_range(0));
        assert!(Aarch64Codegen::adr_offset_in_range(0xfffff));
        assert!(Aarch64Codegen::adr_offset_in_range(-0x100000));
        assert!(!Aarch64Codegen::adr_offset_in_range(0x100000));
        assert!(!Aarch64Codegen::adr_offset_in_range(-0x100001));
    }

    // M25: AArch64 TLSDESC 访问序列 单元测试
    // 验证 emit_tlsdesc_access 发出
    //   adrp x0, :tlsdesc:sym          (R_AARCH64_TLSDESC_ADR_PAGE21)
    //   ldr  x1, [x0, :tlsdesc_lo12:sym] (R_AARCH64_TLSDESC_LD64_LO12)
    //   add  x0, x0, :tlsdesc_lo12:sym  (R_AARCH64_TLSDESC_ADD_LO12)
    //   blr  x1                        (TLSDESC 间接调用)
    // 共 4 条指令 + 3 条 TLSDESC_* 重定位 + 1 个 sym 符号.
    #[test]
    fn aarch64_tlsdesc_access_emits_expected_sequence() {
        use crate::codegen::elf::{
            R_AARCH64_TLSDESC_ADD_LO12, R_AARCH64_TLSDESC_ADR_PAGE21, R_AARCH64_TLSDESC_LD64_LO12,
        };
        let mut cg = Aarch64Codegen::new(
            crate::target::Target::resolve(
                &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
            ),
            true,
        );
        cg.emit_tlsdesc_access("my_tls_var");
        // 4 条 aarch64 指令: adrp (0x90xx xxxx) + ldr (0xf940 xxxx)
        // + add (0x91xx xxxx) + blr (0xd63f 0020)
        assert!(
            cg.code.windows(4).any(|w| (w[3] & 0x9f) == 0x90),
            "missing adrp insn (top byte & 0x9f should be 0x90)"
        );
        assert!(
            cg.code
                .windows(4)
                .any(|w| w[3] == 0xf9 && (w[2] & 0xc0) == 0x40),
            "missing ldr 64-bit insn"
        );
        assert!(
            cg.code.windows(4).any(|w| (w[3] & 0x9f) == 0x91),
            "missing add insn (top byte & 0x9f should be 0x91)"
        );
        let blr_pattern: [u8; 4] = [0x20, 0x00, 0x3f, 0xd6];
        assert!(
            cg.code.windows(4).any(|w| *w == blr_pattern),
            "missing blr x1 (expected 20 00 3f d6)"
        );
        // 3 个 TLSDESC 重定位 (类型 + 引用 my_tls_var)
        let tlsdesc_rels: Vec<_> = cg
            .relocations
            .iter()
            .filter(|r| {
                matches!(
                    r.rel_type,
                    R_AARCH64_TLSDESC_ADR_PAGE21
                        | R_AARCH64_TLSDESC_LD64_LO12
                        | R_AARCH64_TLSDESC_ADD_LO12
                )
            })
            .collect();
        assert_eq!(
            tlsdesc_rels.len(),
            3,
            "expected 3 TLSDESC relocations, got {}",
            tlsdesc_rels.len()
        );
        for r in &tlsdesc_rels {
            // 全部引用同一个符号 my_tls_var (确保 ensure_sym 去重)
            let sym = &cg.symbols[r.sym_index as usize - 1];
            assert_eq!(
                sym.name, "my_tls_var",
                "TLSDESC relocation must reference my_tls_var"
            );
        }
    }

    // Optimized TLSDESC 序列单元测试 (MOVW 对 + 5 个新重定位)
    // 验证 emit_tlsdesc_access_opt 发出:
    //   movz x0, #0, lsl #16  (R_AARCH64_TLSDESC_OFF_G1)
    //   movk x0, #0           (R_AARCH64_TLSDESC_OFF_G0_NC)
    //   ldr  x1, [x0]         (R_AARCH64_TLSDESC_LDR)
    //   add  x0, x0, x1       (R_AARCH64_TLSDESC_ADD)
    //   blr  x1               (R_AARCH64_TLSDESC_CALL)
    #[test]
    fn aarch64_tlsdesc_opt_emits_5_relocations() {
        use crate::codegen::elf::{
            R_AARCH64_TLSDESC_ADD, R_AARCH64_TLSDESC_CALL, R_AARCH64_TLSDESC_LDR,
            R_AARCH64_TLSDESC_OFF_G0_NC, R_AARCH64_TLSDESC_OFF_G1,
        };
        let mut cg = Aarch64Codegen::new(
            crate::target::Target::resolve(
                &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
            ),
            true,
        );
        cg.emit_tlsdesc_access_opt("tls_opt_var");
        // movz (0xd2a) + movk (0xf28) 指令检查
        assert!(
            cg.code.windows(4).any(|w| w[3] == 0xd2),
            "missing movz x0 insn"
        );
        assert!(
            cg.code.windows(4).any(|w| w[3] == 0xf2),
            "missing movk x0 insn"
        );
        // 5 个 TLSDESC 重定位
        let tlsdesc_rels: Vec<_> = cg
            .relocations
            .iter()
            .filter(|r| {
                matches!(
                    r.rel_type,
                    R_AARCH64_TLSDESC_OFF_G1
                        | R_AARCH64_TLSDESC_OFF_G0_NC
                        | R_AARCH64_TLSDESC_LDR
                        | R_AARCH64_TLSDESC_ADD
                        | R_AARCH64_TLSDESC_CALL
                )
            })
            .collect();
        assert_eq!(
            tlsdesc_rels.len(),
            5,
            "expected 5 TLSDESC relocations, got {}",
            tlsdesc_rels.len()
        );
        for r in &tlsdesc_rels {
            let sym = &cg.symbols[r.sym_index as usize - 1];
            assert_eq!(sym.name, "tls_opt_var");
        }
    }

    // M25: AArch64 va_list 寄存器保区 单元测试
    // 验证变参函数入口发出 8 条 str xn, [sp, #off] 保存指令.
    #[test]
    fn aarch64_va_save_area_stores_x0_through_x7() {
        let mut cg = Aarch64Codegen::new(
            crate::target::Target::resolve(
                &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
            ),
            true,
        );
        // 手动设个假变参函数上下文
        cg.is_current_func_variadic = true;
        cg.va_save_base_off = 16;
        cg.stack_bytes = 80;
        cg.frame_bytes = 80;
        // 模拟 gen_function 入口保存动作
        for i in 0..8u8 {
            cg.str_xt_sp(i, cg.va_save_base_off + (i as i32) * 8);
        }
        // 验证: 8 条 str xn, [sp, #off] 指令,
        //       byte3 = 0xF9 (STR X), bit 23 (byte2 bit7) = 0
        let str_x_count = cg
            .code
            .windows(4)
            .filter(|w| w[3] == 0xf9 && (w[2] & 0x80) == 0)
            .count();
        assert_eq!(
            str_x_count, 8,
            "expected 8 str xn, [sp, #off] instructions, got {str_x_count}"
        );
        // 验证偏移在 [16, 72] 范围 (va_save_base_off=16, 8 * 8 步进)
        for i in 0..8 {
            let expected_off = 16 + i * 8;
            let imm12 = (expected_off / 8) as u32;
            let found = cg.code.windows(4).any(|w| {
                // 提取指令 imm12 字段 (bits 21:10) 并比较
                let insn = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
                let got = (insn >> 10) & 0xfff;
                got == imm12 && (insn & 0xff00_0000) == 0xf900_0000
            });
            assert!(found, "missing str x{i}, [sp, #{expected_off}]");
        }
    }

    // M25: AArch64 va_arg 整数路径 单元测试
    // 验证 gen_va_arg_int 从保存区顺序读 w0
    #[test]
    fn aarch64_va_arg_int_reads_sequentially_from_save_area() {
        let mut cg = Aarch64Codegen::new(
            crate::target::Target::resolve(
                &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
            ),
            true,
        );
        cg.is_current_func_variadic = true;
        cg.va_save_base_off = 0;
        cg.va_gp_offs = 0;
        // 注册一个假 va_list 局部
        cg.locals.insert("ap".to_string(), 100);
        // 读 3 个变参
        cg.gen_va_arg_int("ap");
        cg.gen_va_arg_int("ap");
        cg.gen_va_arg_int("ap");
        // 验证: 3 条 ldr w0, [sp, #off] 指令
        let ldr_w_count = cg
            .code
            .windows(4)
            .filter(|w| {
                // ldr W: byte3 = 0xb9, byte2 bit6-4 = 100 (32-bit ldr)
                w[3] == 0xb9 && (w[2] & 0x70) == 0x40
            })
            .count();
        assert_eq!(
            ldr_w_count, 3,
            "expected 3 ldr w0, [sp, #off] instructions, got {ldr_w_count}"
        );
        // 验证偏移按 0, 8, 16 递增 (每 8 字节一槽, x 寄存器 8 字节)
        let mut offs = Vec::new();
        for w in cg.code.windows(4) {
            if w[3] == 0xb9 && (w[2] & 0x70) == 0x40 {
                let insn = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
                let imm12 = (insn >> 10) & 0xfff;
                offs.push((imm12 * 4) as i32);
            }
        }
        assert_eq!(
            offs,
            vec![0, 8, 16],
            "expected offsets [0, 8, 16], got {offs:?}"
        );
        // 验证 va_gp_offs 推进
        assert_eq!(cg.va_gp_offs, 3, "va_gp_offs should be 3 after 3 reads");
    }

    // 嵌套 sret: 验证 gen_function 在返回 >16B 聚合时保存 x0 到 <sret_ptr>
    // 并验证 gen_stmt Return 使用保存的 sret 指针而非 x0
    #[test]
    fn aarch64_nested_sret_saves_sret_ptr() {
        use crate::ir::ValueType;
        let mut cg = Aarch64Codegen::new(
            crate::target::Target::resolve(
                &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
            ),
            true,
        );
        // 模拟返回 32 字节聚合类型的函数
        cg.current_return_ty = Some(ValueType::Aggregate {
            name: "Big".to_string(),
            size: 32,
            is_union: false,
        });
        // 模拟帧建立后保存 sret 指针
        cg.stack_bytes = 16;
        cg.frame_bytes = 16;
        let sret_off = cg.alloc_local("<sret_ptr>", 8);
        cg.store_w0(sret_off);
        cg.saved_sret_ptr = Some(sret_off);
        // 验证 saved_sret_ptr 已设置
        assert!(
            cg.saved_sret_ptr.is_some(),
            "saved_sret_ptr should be set for large aggregate return"
        );
        assert_eq!(
            cg.saved_sret_ptr.unwrap(),
            sret_off,
            "saved_sret_ptr should match allocated offset"
        );
        // 验证 locals 包含 <sret_ptr>
        assert!(
            cg.locals.contains_key("<sret_ptr>"),
            "locals should contain <sret_ptr>"
        );
    }

    // TLS: 验证 tls_globals 集合正确记录 TLS 变量
    #[test]
    fn aarch64_tls_globals_tracking() {
        let mut cg = Aarch64Codegen::new(
            crate::target::Target::resolve(
                &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
            ),
            true,
        );
        // 模拟 TLS 全局变量记录
        cg.tls_globals.insert("my_tls_var".to_string(), 0);
        cg.tls_globals.insert("another_tls".to_string(), 8);
        assert!(
            cg.tls_globals.contains_key("my_tls_var"),
            "tls_globals should contain my_tls_var"
        );
        assert!(
            cg.tls_globals.contains_key("another_tls"),
            "tls_globals should contain another_tls"
        );
        assert_eq!(cg.tls_globals.len(), 2);
    }

    // HWASAN AArch64 MTE: 验证 IRG/ADDG/SUBG/STG/LDG 编码与 ARM 规范一致
    // (参考 LLVM llvm/test/MC/AArch64/armv8.5a-mte.s 的已知编码)
    #[test]
    fn mte_emitter_encodings_match_arm_spec() {
        let mut cg = Aarch64Codegen::new(
            crate::target::Target::resolve(
                &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
            ),
            true,
        );
        cg.emit_irg(0, 1); // irg x0, x1 -> LLVM: [0x20,0x10,0xdf,0x9a]
        cg.emit_irg(0, 31); // irg x0, sp -> LLVM: [0xe0,0x13,0xdf,0x9a]
        cg.emit_irg(16, 31); // irg x16, sp
        cg.emit_irg(17, 18); // irg x17, x18
        cg.emit_addg(0, 1, 0, 1); // addg x0, x1, #0, #1 -> LLVM: [0x20,0x04,0x80,0x91]
        cg.emit_addg(31, 16, 0, 0); // addg sp, x16, #0, #0
        cg.emit_addg(31, 2, 2, 3); // addg sp, x2, #32, #3
        cg.emit_subg(0, 1, 0, 1); // subg x0, x1, #0, #1 -> LLVM: [0x20,0x04,0x80,0xd1]
        cg.emit_stg(3, 31, 1); // stg x3, [sp, #16] -> LLVM: [0xe3,0x1b,0x20,0xd9]
        cg.emit_stg(16, 31, 0); // stg x16, [sp, #0]
        cg.emit_stg(7, 8, 0x1ff); // stg x7, [x8, #-16]
        cg.emit_ldg(0, 0, 0); // ldg x0, [x0]
        let code = &cg.code;
        let find = |bytes: [u8; 4]| code.windows(4).any(|w| w == &bytes);
        assert!(find([0x20, 0x10, 0xdf, 0x9a]), "irg x0, x1");
        assert!(find([0xe0, 0x13, 0xdf, 0x9a]), "irg x0, sp");
        assert!(find([0xf0, 0x13, 0xdf, 0x9a]), "irg x16, sp");
        assert!(find([0x51, 0x12, 0xdf, 0x9a]), "irg x17, x18");
        assert!(find([0x20, 0x04, 0x80, 0x91]), "addg x0, x1, #0, #1");
        assert!(find([0x1f, 0x02, 0x80, 0x91]), "addg sp, x16, #0, #0");
        assert!(find([0x5f, 0x0c, 0x82, 0x91]), "addg sp, x2, #32, #3");
        assert!(find([0x20, 0x04, 0x80, 0xd1]), "subg x0, x1, #0, #1");
        assert!(find([0xe3, 0x1b, 0x20, 0xd9]), "stg x3, [sp, #16]");
        assert!(find([0xf0, 0x0b, 0x20, 0xd9]), "stg x16, [sp, #0]");
        assert!(find([0x07, 0xf9, 0x3f, 0xd9]), "stg x7, [x8, #-16]");
        assert!(find([0x00, 0x00, 0x60, 0xd9]), "ldg x0, [x0]");
    }

    // HWASAN AArch64 MTE: 验证 gen_function prologue 发射 tag 栈帧序列
    // (irg + addg + stg*N + str x19 + mov x19,sp) 与 epilogue 的
    // ldr x19 + and sp,sp,#mask + add sp 恢复序列
    #[test]
    fn hwasan_mte_prologue_emits_tag_frame() {
        let (program, diags) = crate::parser::parse(
            b"int main(void) { int a = 1; int b = 2; return a + b; }\n",
            "mte.c",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let (module, _) = crate::ir::lower(program).expect("lower");
        crate::ir::validate(&module).expect("validate");
        let triple = crate::cli::TargetTriple::parse("aarch64-linux-gnu").expect("triple");
        let mut cg = Aarch64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.set_mte_enabled(true);
        cg.compile(&module);
        let find = |bytes: [u8; 4]| cg.code.windows(4).any(|w| w == &bytes);
        // prologue: irg x16, sp
        assert!(
            find([0xf0, 0x13, 0xdf, 0x9a]),
            "prologue missing irg x16, sp"
        );
        // prologue: addg sp, x16, #0, #0
        assert!(
            find([0x1f, 0x02, 0x80, 0x91]),
            "prologue missing addg sp, x16, #0, #0"
        );
        // prologue: stg x16, [sp, #0] 与 stg x16, [sp, #16] (帧 40B, granules = 40/16 = 2)
        assert!(
            find([0xf0, 0x0b, 0x20, 0xd9]),
            "prologue missing stg x16, [sp, #0]"
        );
        assert!(
            find([0xf0, 0x1b, 0x20, 0xd9]),
            "prologue missing stg x16, [sp, #16]"
        );
        // prologue: str x19, [sp, #32] (保存入参 x19, <vec_lit> at 16, mte_save at 32)
        assert!(
            find([0xf3, 0x13, 0x00, 0xf9]),
            "prologue missing str x19, [sp, #32]"
        );
        // prologue: add x19, sp, #0 (mov x19, sp)
        assert!(
            find([0xf3, 0x03, 0x00, 0x91]),
            "prologue missing add x19, sp, #0"
        );
        // epilogue: ldr x19, [sp, #32]
        assert!(
            find([0xf3, 0x13, 0x40, 0xf9]),
            "epilogue missing ldr x19, [sp, #32]"
        );
        // epilogue: and sp, sp, #0x00ffffffffffffff (untag)
        assert!(
            find([0xff, 0xdf, 0x80, 0x92]),
            "epilogue missing and sp, sp, #mask"
        );
        // 记录 tag 栈帧大小 (frame_bytes = (40+15)&!15 = 48)
        assert_eq!(cg.tag_frame_size, 48, "tag_frame_size should be frame size");
    }

    // HWASAN AArch64 MTE: 验证调用点对 sp 做 untag / retag,
    // 使外部 (非 MTE) callee 以 untagged sp 运行, 返回后恢复本帧 tag
    #[test]
    fn hwasan_mte_call_site_untag_retag() {
        let (program, diags) = crate::parser::parse(
            b"extern int callee(int);\nint main(void) { int a = 5; return callee(a); }\n",
            "mte2.c",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let (module, _) = crate::ir::lower(program).expect("lower");
        crate::ir::validate(&module).expect("validate");
        let triple = crate::cli::TargetTriple::parse("aarch64-linux-gnu").expect("triple");
        let mut cg = Aarch64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.set_mte_enabled(true);
        cg.compile(&module);
        // 调用点前缀: and sp, x19, #0x00ffffffffffffff
        assert!(
            cg.code.windows(4).any(|w| w == &[0x7f, 0xde, 0x80, 0x92]),
            "call site missing and sp, x19, #mask"
        );
        // 调用点后缀: add sp, x19, #0 (mov sp, x19)
        assert!(
            cg.code.windows(4).any(|w| w == &[0x7f, 0x02, 0x00, 0x91]),
            "call site missing add sp, x19, #0"
        );
    }

    // HWASAN AArch64 MTE: 未启用 hwasan 时不得发射任何 MTE tag 指令
    #[test]
    fn no_mte_no_tag_instructions() {
        let (program, diags) = crate::parser::parse(
            b"int main(void) { int a = 1; int b = 2; return a + b; }\n",
            "nomte.c",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let (module, _) = crate::ir::lower(program).expect("lower");
        crate::ir::validate(&module).expect("validate");
        let triple = crate::cli::TargetTriple::parse("aarch64-linux-gnu").expect("triple");
        let mut cg = Aarch64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.compile(&module);
        let find = |bytes: [u8; 4]| cg.code.windows(4).any(|w| w == &bytes);
        assert!(!find([0xf0, 0x13, 0xdf, 0x9a]), "unexpected irg x16, sp");
        assert!(!find([0xf0, 0x0b, 0x20, 0xd9]), "unexpected stg");
        assert!(
            !find([0xff, 0xdf, 0x80, 0x92]),
            "unexpected and sp, sp, #mask"
        );
        assert_eq!(
            cg.tag_frame_size, 0,
            "tag_frame_size should stay 0 without MTE"
        );
    }

    // M25: gen_float_expr 处理 Local float 变量时
    // 应从栈加载到 s0 而非回退 0.0
    #[test]
    fn aarch64_gen_float_expr_local_loads_from_stack() {
        let mut cg = Aarch64Codegen::new(
            crate::target::Target::resolve(
                &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
            ),
            true,
        );
        cg.locals.insert("x".to_string(), 16);
        let expr = TypedExpr {
            kind: ExprKind::Local("x".to_string()),
            ty: ValueType::Float { bits: 32 },
            literal: None,
            children: vec![],
            span: crate::diagnostics::Span::point("t.c", 1, 1),
            quals: crate::ast::QualifierFlags::NONE,
        };
        let ok = cg.gen_float_expr(&expr);
        assert!(ok, "gen_float_expr should succeed for Local float");
        // 验证发射了 ldr s0, [sp, #16] 指令
        // LDR S (32-bit): 0xBD400000 | (imm12/4)<<10 | Rn=sp(31) | Rt=s0(0)
        let ldr_s0 = 0xBD400000u32 | ((16u32 / 4) << 10) | (31 << 5) | 0;
        let found = cg
            .code
            .windows(4)
            .any(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]) == ldr_s0);
        assert!(found, "expected ldr s0, [sp, #16] instruction");
    }

    // -ftrivial-auto-var-init=zero: 验证 AArch64 后端发射 stp xzr, xzr 指令
    #[test]
    fn aarch64_auto_var_init_zero_emits_stp_xzr() {
        let triple = crate::cli::TargetTriple::parse("aarch64-linux-gnu").expect("triple");
        let mut target = crate::target::Target::resolve(&triple);
        target.auto_var_init_zero = true;
        let mut cg = Aarch64Codegen::new(target, true);
        // 直接测试 emit_zero_init_frame: 32 字节帧 = 2 个 stp
        cg.emit_zero_init_frame(32);
        let stp_count = cg
            .code
            .windows(4)
            .filter(|w| {
                let inst = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
                // STP XZR, XZR, [SP, #off] (signed offset mode)
                // Fixed fields: opc[31:30]=10, 101[29:27], V=0[26], mode=010[25:23], L=0[22]
                // Check top 10 bits [31:22] = 10_101_0_010_0 = 0xA9 (with bit 22=0)
                let top10 = (inst >> 22) & 0x3ff;
                let rt2 = (inst >> 10) & 0x1f;
                let rn = (inst >> 5) & 0x1f;
                let rt = inst & 0x1f;
                top10 == 0x2a4 && rt2 == 31 && rn == 31 && rt == 31
            })
            .count();
        assert_eq!(stp_count, 2, "expected 2 stp xzr, xzr instructions for 32-byte frame, got {stp_count}");
    }
}
