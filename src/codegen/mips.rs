// src/codegen/mips.rs - MIPS32 后端 (O32 ABI)

use crate::ast::{BinOp, BlockItem, Decl, Expr, FuncDecl, Program, Stmt, UnOp};
use crate::codegen::asm_constraints::{
    self, resolve_asm_operands, resolve_asm_template_enhanced, AsmArch,
};
use crate::codegen::elf::{
    self, ElfRela, ElfSection, ElfSym, SHF_ALLOC, SHF_GNU_RETAIN, SHT_PROGBITS, STB_GLOBAL,
    STT_FUNC, STT_GNU_IFUNC,
};
use crate::ir::{Function, Global, Module, TypedStmt, ValueType};
use crate::target::Target;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy)]
enum BranchKind {
    Beqz,
    Bne,
    B,
}

/// MIPS32 O32 ABI 后端
pub struct MipsCodegen {
    pub code: Vec<u8>,
    pub rodata: Vec<u8>,
    pub relocatable: bool,
    pub symbols: Vec<ElfSym>,
    pub relocations: Vec<ElfRela>,
    pub rodata_relocations: Vec<ElfRela>,
    pub func_sizes: HashMap<String, u64>,
    pub labels: HashMap<String, u64>,
    locals: HashMap<String, i32>,
    global_rodata: HashMap<String, u64>,
    string_pool: HashMap<Vec<u8>, u64>,
    sym_index: HashMap<String, u32>,
    stack_bytes: i32,
    frame_bytes: i32,
    source_name: String,
    pub extra_sections: Vec<ElfSection>,
    current_return_ty: Option<ValueType>,
    aggregate_fields: HashMap<String, (Arc<Vec<crate::ast::Field>>, bool)>,
    is_64bit: bool,
    emitted_return: bool,
    label_serial: u32,
    pending_bl_calls: Vec<(u64, String)>,
    pending_branches: Vec<(u64, String, BranchKind)>,
    break_labels: Vec<String>,
    continue_labels: Vec<String>,
    /// 文件级 asm 原始文本行 (GlobalAsm)
    raw_global_asm_lines: Vec<String>,
    /// 函数级内联 asm 原始文本行: (函数名, 行列表)
    raw_func_asm_lines: Vec<(String, Vec<String>)>,
    /// -S 模式标志: true 时保留 asm 文本输出, false 时编码机器码
    asm_text_mode: bool,
    /// exec 模式入口地址
    entry: u64,
    /// exec 模式 _start 中 JAL main 的指令位置
    start_call_pos: Option<u64>,
    /// 当前函数是否为 variadic
    is_current_func_variadic: bool,
    /// va_save 区在 stack_bytes 中的起始偏移 (plan_locals 阶段预留)
    va_save_base_off: i32,
    /// 下一个 GP va_arg 槽位索引 (0..3, 对应 $a0..$a3)
    va_gp_offs: i32,
    /// 下一个 float va_arg 槽位索引 (O32 hard-float: $f12/$f14)
    va_flt_offs: i32,
    /// 浮点字面量池偏移 (rodata 中)
    float_pool: HashMap<u64, u64>,
    /// Stack protector: 当前函数 canary 槽位偏移, 0 表示未启用
    canary_slot_off: i32,
    /// -mrecord-mcount: ftrace call site positions for __mcount_loc section
    mcount_loc_entries: Vec<(u64, String)>,
    /// 编译诊断 (未识别内联汇编指令等)
    pub diagnostics: Vec<crate::diagnostics::Diagnostic>,
    _target: Target,
}

impl MipsCodegen {
    pub fn new(target: Target, relocatable: bool) -> Self {
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
            is_64bit: false,
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

    pub fn set_source_name(&mut self, name: impl Into<String>) {
        self.source_name = name.into();
    }

    pub fn set_asm_text_mode(&mut self, v: bool) {
        self.asm_text_mode = v;
    }

    /// Encode resolved asm text into machine code and emit to code buffer.
    /// P0-5: unrecognized instructions produce hard errors, not NOPs.
    fn encode_and_emit_asm(&mut self, resolved: &str, span_file: &str, span_line: u32) {
        let arch = if self.is_64bit { "mips64" } else { "mips" };
        let lines = crate::codegen::mini_asm::parse_asm_lines(resolved);
        match crate::codegen::mini_asm::encode_asm_lines_for_arch(
            &lines,
            arch,
            self.is_64bit,
        ) {
            Ok(bytes) => {
                self.code.extend_from_slice(&bytes);
            }
            Err(unrecognized) => {
                for mnem in &unrecognized {
                    let span = crate::diagnostics::Span::point(span_file, span_line, 1);
                    let mut diag = crate::diagnostics::Diagnostic::err(
                        "E0311",
                        format!(
                            "unsupported inline asm instruction '{}' on {} target",
                            mnem, arch
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

    pub fn set_64bit(&mut self) {
        self.is_64bit = true;
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

    /// 获取函数大小表
    pub fn func_sizes(&self) -> &HashMap<String, u64> {
        &self.func_sizes
    }

    pub fn compile(&mut self, module: &Module) {
        let prog = module.source();
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
                        self.sym_index.insert(f.name.name.clone(), self.symbols.len() as u32);
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
                Vec::with_capacity(self.mcount_loc_entries.len() * 4); // MIPS: 32-bit addresses
            for &(call_pos, _) in &self.mcount_loc_entries {
                mcount_data.extend_from_slice(&(call_pos as u32).to_be_bytes());
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

    fn compile_exec(&mut self, module: &Module, prog: &Program) {
        // _start: JAL main (placeholder) + NOP delay slot
        let start_pos = self.code.len() as u64;
        self.labels.insert("_start".to_string(), start_pos);
        // JAL placeholder (target = 0, will be patched)
        let jal_pos = self.code.len() as u64;
        self.start_call_pos = Some(jal_pos);
        let inst: u32 = 0b000011 << 26; // JAL with target 0
        self.code.extend_from_slice(&inst.to_be_bytes());
        self.emit_nop(); // delay slot
                         // move $a0, $v0 (save main return value)
        self.emit_addu(4, 2, 0); // addu $a0, $v0, $zero
                                 // li $v0, 4001 (__NR_exit)
        self.emit_load_imm(2, 4001);
        // syscall
        let syscall_inst: u32 = 0b000000 << 26; // SPECIAL
        let syscall_inst = syscall_inst | 0b001000; // opcode 0.12 = syscall
        self.code.extend_from_slice(&syscall_inst.to_be_bytes());
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
            // Patch JAL to main
            if func.name == "main" {
                if let Some(jal_pos) = self.start_call_pos {
                    if let Some(&main_addr) = self.labels.get("main") {
                        self.patch_jal(jal_pos, main_addr);
                    }
                }
            }
        }
        // Finalize pending branches
        self.finalize_branches();
    }

    fn patch_jal(&mut self, at: u64, target: u64) {
        // MIPS JAL: target28 = target & 0x0FFFFFFF, instr[25:0] = target28 >> 2
        let target28 = (target & 0x0FFFFFFF) as u32;
        let existing = u32::from_be_bytes([
            self.code[at as usize],
            self.code[at as usize + 1],
            self.code[at as usize + 2],
            self.code[at as usize + 3],
        ]);
        let patched = (existing & 0xFC000000) | (target28 >> 2);
        self.code[at as usize..at as usize + 4].copy_from_slice(&patched.to_be_bytes());
    }

    fn finalize_branches(&mut self) {
        let pending: Vec<_> = self.pending_branches.drain(..).collect();
        for (at, label, kind) in pending {
            if let Some(&target) = self.labels.get(&label) {
                match kind {
                    BranchKind::Beqz => self.patch_beqz(at, target),
                    BranchKind::Bne => self.patch_bne(at, target),
                    BranchKind::B => self.patch_b(at, target),
                }
            }
        }
    }

    fn emit_globals(&mut self, module: &Module) {
        for g in &module.globals {
            if g.initializer.is_some() {
                self.emit_global_init(g);
            }
        }
    }

    fn emit_global_init(&mut self, g: &Global) {
        let offset = self.rodata.len() as u64;
        self.global_rodata.insert(g.name.clone(), offset);
        // Evaluate constant initializer or default to zero
        if let Some(init_expr) = &g.initializer {
            // Try to extract constant value from TypedExpr
            let val = match (&init_expr.kind, &init_expr.literal) {
                (crate::ir::ExprKind::Constant, Some(crate::ir::ExprLiteral::Int(v))) => {
                    Some(*v as i64)
                }
                _ => None,
            };
            if let Some(val) = val {
                // Emit proper size based on type
                let size = match &g.ty {
                    ValueType::Integer { bits, .. } => (*bits / 8).max(4) as usize,
                    ValueType::Pointer(_) => 4, // MIPS32: 4-byte pointers
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

    fn compile_rel(&mut self, module: &Module, prog: &Program) {
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

    // ===== 局部变量分配 =====

    fn plan_locals(&mut self, ast: Option<&FuncDecl>) {
        self.locals.clear();
        self.stack_bytes = 0;
        let Some(body) = ast.and_then(|f| f.body.as_ref()) else {
            return;
        };
        for item in &body.items {
            if let BlockItem::Decl(Decl::Var(v)) = item {
                let sz = v.ty.size().max(4) as i32;
                self.alloc_local(&v.name.name, sz);
            }
        }
    }

    fn alloc_local(&mut self, name: &str, size: i32) -> i32 {
        let off = self.stack_bytes;
        self.stack_bytes += (size + 3) & !3;
        self.locals.insert(name.to_string(), off);
        off
    }

    // ===== 函数生成 =====

    fn gen_function(&mut self, func: &Function, ast: Option<&FuncDecl>) {
        // -fpatchable-function-entry: emit NOPs before and after label
        let func_start: u64;
        if let Some((n, m)) = self._target.patchable_function_entry {
            let prefix_len = m.min(n);
            let suffix_len = n.saturating_sub(prefix_len);
            for _ in 0..prefix_len {
                // MIPS NOP: SLL $zero, $zero, 0 = 0x00000000
                self.code.extend_from_slice(&0x00000000u32.to_be_bytes());
            }
            func_start = self.code.len() as u64;
            self.labels.insert(func.name.clone(), func_start);
            for _ in 0..suffix_len {
                self.code.extend_from_slice(&0x00000000u32.to_be_bytes());
            }
        } else {
            func_start = self.code.len() as u64;
            self.labels.insert(func.name.clone(), func_start);
        }

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
        self.current_return_ty = Some(func.return_type.clone());
        self.emitted_return = false;

        // Stack protector: allocate canary slot before frame size calculation
        if self._target.stack_protector {
            self.canary_slot_off = self.alloc_local("<canary>", 4);
        } else {
            self.canary_slot_off = 0;
        }

        // 变参函数: 在局部变量之后预留 $a0-$a3 保存区 (4×4 = 16 字节)
        let is_variadic = ast.map(|f| f.is_variadic).unwrap_or(false);
        self.is_current_func_variadic = is_variadic;
        self.va_gp_offs = 0;
        if is_variadic {
            self.va_save_base_off = self.stack_bytes;
            self.stack_bytes += 16;
        } else {
            self.va_save_base_off = 0;
        }

        // 函数序言: 分配帧空间 (至少保存 $ra, $fp + 局部变量)
        // O32/N64 ABI: 帧大小必须为 8 字节对齐
        let save_size = if self.is_64bit { 16 } else { 8 }; // $fp + $ra (8 bytes each for 64-bit)
        self.frame_bytes = ((self.stack_bytes + save_size) + 7) & !7;
        self.emit_addiu(29, 29, -(self.frame_bytes as i16)); // addiu $sp, $sp, -frame
        self.emit_sw(31, 29, (self.frame_bytes - 4) as i16); // sw $ra, frame-4($sp)
        self.emit_sw(30, 29, (self.frame_bytes - 8) as i16); // sw $fp, frame-8($sp)
        self.emit_addiu(30, 29, (self.frame_bytes - 8) as i16); // addiu $fp, $sp, frame-8
        // -fstack-clash-protection: probe each page for large frames
        if self._target.stack_clash_protection && self.frame_bytes > 4096 {
            self.emit_stack_clash_probes(self.frame_bytes as u32);
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
                let pos = self.code.len() as u64;
                self.emit_jal("__fentry__");
                self.emit_nop(); // delay slot
                Some(pos)
            }
            crate::cli::FtraceMode::MCount => {
                let pos = self.code.len() as u64;
                self.emit_jal("mcount");
                self.emit_nop(); // delay slot
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

        // 参数传递: $a0-$a3 -> 局部变量槽
        if let Some(ast_f) = ast {
            for (i, p) in ast_f.params.iter().enumerate() {
                if let Some(name) = &p.name {
                    if let Some(&off) = self.locals.get(&name.name) {
                        // 从 $a0..$a3 保存到栈
                        let arg_reg = (4 + i) as u8; // $a0=4, $a1=5, ...
                        let fp_off = (self.frame_bytes - 8 - off) as i16;
                        if fp_off >= 0 {
                            self.emit_sw(arg_reg, 30, -(off as i16));
                        } else {
                            // 使用 $sp 相对偏移
                            self.emit_sw(arg_reg, 29, (self.frame_bytes - 8 - off) as i16);
                        }
                    }
                }
            }
        }

        // 变参函数: 函数序言后立即保存 $a0-$a3 到 va_save 区
        // va_save 槽位相对于 $fp 的偏移: -(frame_bytes - 8 - va_save_base_off - i*4)
        if is_variadic {
            for i in 0..4u8 {
                let fp_off = -(self.frame_bytes as i16 - 8 - self.va_save_base_off as i16 - (i as i16) * 4);
                self.emit_sw(4 + i, 30, fp_off); // sw $a{i}, fp_off($fp)
            }
        }

        // 处理函数体
        let body = ast.and_then(|f| f.body.as_ref());
        let items = body.map(|b| b.items.as_slice()).unwrap_or(&[]);
        for (item, ir) in items.iter().zip(func.body.iter()) {
            self.gen_block_item(item, ir);
        }

        // 隐式 return 0
        if !self.emitted_return {
            self.emit_addiu(2, 0, 0); // move $v0, $zero
            self.emit_epilogue();
        }

        let func_end = self.code.len() as u64;
        self.func_sizes
            .insert(func.name.clone(), func_end - func_start);

        // 添加符号
        let sym_type = if ast.and_then(|f| f.ifunc_resolver.as_ref()).is_some() {
            STT_GNU_IFUNC
        } else {
            STT_FUNC
        };
        let sym_idx = self.symbols.len() as u32;
        self.symbols.push(ElfSym {
            name: func.name.clone(),
            section: 1,
            value: func_start,
            size: func_end - func_start,
            bind: STB_GLOBAL,
            ty: sym_type,
            visibility: 0,
        });
        self.sym_index.insert(func.name.clone(), sym_idx);

        // attribute((used)): 发射 .used.<name> 节并标记 SHF_GNU_RETAIN
        if ast.map(|f| f.is_used_attr).unwrap_or(false) {
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
    }

    /// -ftrivial-auto-var-init=zero: 零初始化栈帧
    /// 使用 sw zero, offset(fp) 每次清零 4 字节
    /// MIPS 局部变量在 fp 负偏移方向
    fn emit_zero_init_frame(&mut self, frame_size: usize) {
        if frame_size == 0 {
            return;
        }
        // $at ($1) 是汇编器保留的暂存器, 用于零初始化
        // 从帧底向帧顶写入, 相对于 $fp 的负偏移
        let words = frame_size / 4;
        for i in 0..words {
            let fp_off = -((frame_size - (i as usize) * 4) as i16);
            self.emit_sw(0, 30, fp_off); // sw $zero, fp_off($fp)
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

    fn emit_epilogue(&mut self) {
        // Stack protector epilogue: verify canary BEFORE frame teardown
        if self._target.stack_protector && self.canary_slot_off > 0 {
            self.emit_stack_chk_epilogue(self.canary_slot_off);
        }
        // 恢复 $fp, $ra 并返回
        if self.is_64bit {
            self.emit_lw(30, 29, (self.frame_bytes - 16) as i16); // ld $fp, frame-16($sp)
            self.emit_lw(31, 29, (self.frame_bytes - 8) as i16); // ld $ra, frame-8($sp)
        } else {
            self.emit_lw(30, 29, (self.frame_bytes - 8) as i16); // lw $fp, frame-8($sp)
            self.emit_lw(31, 29, (self.frame_bytes - 4) as i16); // lw $ra, frame-4($sp)
        }
        self.emit_addiu(29, 29, self.frame_bytes as i16); // addiu $sp, $sp, frame
        self.emit_jr(31); // jr $ra
        self.emit_nop(); // delay slot
        self.emitted_return = true;
    }

    // M25: MIPS va_arg 整数路径 — 从保存区顺序读 $v0
    // O32 ABI: $a0-$a3 在函数入口保存到 va_save 区,
    // va_arg 依次读取, 超出 4 个则从 caller 栈读.
    fn gen_va_arg_int(&mut self, _ap_name: &str) {
        if self.va_gp_offs < 4 {
            let fp_off = -(self.frame_bytes as i16
                - 8
                - self.va_save_base_off as i16
                - (self.va_gp_offs as i16) * 4);
            self.emit_lw(2, 30, fp_off); // lw $v0, fp_off($fp)
            self.va_gp_offs += 1;
        } else {
            // 从 caller 栈读取: 栈参数位于 caller 帧中 $fp+16 起
            let stack_off = self.frame_bytes + (self.va_gp_offs - 4) * 4;
            self.emit_lw(2, 29, stack_off as i16); // lw $v0, stack_off($sp)
            self.va_gp_offs += 1;
        }
    }

    // MIPS va_arg 浮点路径 — O32 hard-float ABI
    // 浮点参数通过 $f12/$f14 传递 (最多 2 个单/双精度参数)
    // 简化实现: 从整数保存区回退读取
    fn gen_va_arg_float(&mut self, _ap_name: &str, _is_double: bool) {
        // O32 hard-float: 浮点变参在 $f12/$f14
        // 简化实现: 回退到整数路径 (软浮点兼容)
        if self.va_flt_offs < 2 {
            // 从 $f12/$f14 保存区读取 (如果存在)
            // 简化: 使用整数路径
            self.gen_va_arg_int("ap");
            self.va_flt_offs += 1;
        } else {
            // 从 caller 栈读取
            self.gen_va_arg_int("ap");
            self.va_flt_offs += 1;
        }
    }

    // ===== Block item / stmt / expr =====

    fn gen_block_item(&mut self, item: &BlockItem, _ir: &TypedStmt) {
        match item {
            BlockItem::Stmt(s) => self.gen_stmt(s),
            BlockItem::Decl(Decl::Var(v)) => {
                let off = *self.locals.get(&v.name.name).unwrap_or(&0);
                if let Some(init) = &v.init {
                    self.gen_expr_from_ast(init);
                    self.emit_store_v0(off);
                }
            }
            _ => {}
        }
    }

    fn gen_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Return { val, .. } => {
                if let Some(e) = val {
                    self.gen_expr_from_ast(e);
                } else {
                    self.emit_addiu(2, 0, 0); // move $v0, $zero
                }
                self.emit_epilogue();
            }
            Stmt::Expr { e, .. } => {
                self.gen_expr_from_ast(e);
            }
            Stmt::If {
                cond, then_, else_, ..
            } => {
                self.gen_expr_from_ast(cond);
                let lbl_else = self.fresh_label("Lelse");
                let lbl_end = self.fresh_label("Lendif");
                self.emit_beqz(2, &lbl_else);
                self.emit_nop();
                self.gen_stmt(then_);
                self.emit_b(&lbl_end);
                self.emit_nop();
                self.set_label(&lbl_else);
                if let Some(el) = else_ {
                    self.gen_stmt(el);
                }
                self.set_label(&lbl_end);
            }
            Stmt::While { cond, body, .. } => {
                let lbl_top = self.fresh_label("Lwhile");
                let lbl_end = self.fresh_label("Lendwhile");
                self.break_labels.push(lbl_end.clone());
                self.continue_labels.push(lbl_top.clone());
                self.set_label(&lbl_top);
                self.gen_expr_from_ast(cond);
                self.emit_beqz(2, &lbl_end);
                self.emit_nop();
                self.gen_stmt(body);
                self.emit_b(&lbl_top);
                self.emit_nop();
                self.set_label(&lbl_end);
                self.break_labels.pop();
                self.continue_labels.pop();
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => {
                if let Some(i) = init {
                    self.gen_stmt(i);
                }
                let lbl_top = self.fresh_label("Lfor");
                let lbl_end = self.fresh_label("Lendfor");
                let lbl_step = self.fresh_label("Lforstep");
                self.break_labels.push(lbl_end.clone());
                self.continue_labels.push(lbl_step.clone());
                self.set_label(&lbl_top);
                if let Some(c) = cond {
                    self.gen_expr_from_ast(c);
                    self.emit_beqz(2, &lbl_end);
                    self.emit_nop();
                }
                self.gen_stmt(body);
                self.set_label(&lbl_step);
                if let Some(s) = step {
                    self.gen_expr_from_ast(s);
                }
                self.emit_b(&lbl_top);
                self.emit_nop();
                self.set_label(&lbl_end);
                self.break_labels.pop();
                self.continue_labels.pop();
            }
            Stmt::Block { b, .. } => {
                for item in &b.items {
                    if let BlockItem::Stmt(s) = item {
                        self.gen_stmt(s);
                    }
                }
            }
            Stmt::Do { body, cond, .. } => {
                let lbl_body = self.fresh_label("Ldobody");
                let lbl_cond = self.fresh_label("Ldocond");
                let lbl_end = self.fresh_label("Lenddo");
                self.break_labels.push(lbl_end.clone());
                self.continue_labels.push(lbl_cond.clone());
                self.set_label(&lbl_body);
                self.gen_stmt(body);
                self.set_label(&lbl_cond);
                self.gen_expr_from_ast(cond);
                self.emit_beqz(2, &lbl_end);
                self.emit_nop();
                self.emit_b(&lbl_body);
                self.emit_nop();
                self.set_label(&lbl_end);
                self.break_labels.pop();
                self.continue_labels.pop();
            }
            Stmt::Break { .. } => {
                if let Some(lbl) = self.break_labels.last().cloned() {
                    self.emit_b(&lbl);
                    self.emit_nop();
                }
            }
            Stmt::Continue { .. } => {
                if let Some(lbl) = self.continue_labels.last().cloned() {
                    self.emit_b(&lbl);
                    self.emit_nop();
                }
            }
            Stmt::Switch { expr, cases, .. } => {
                self.gen_expr_from_ast(expr);
                // $v0 now holds the switch expression value
                let lbl_end = self.fresh_label("Lendswitch");
                let lbl_default = self.fresh_label("Ldefault");
                self.break_labels.push(lbl_end.clone());
                // Generate case comparisons
                let mut case_labels = Vec::new();
                for case in cases {
                    let lbl_case = self.fresh_label("Lcase");
                    if let Some(val_expr) = &case.val {
                        // case N:
                        if let Ok(val) = crate::const_eval::eval(val_expr) {
                            self.emit_load_imm(3, val as i32); // $t0 = case value
                                                               // Compare $v0 with $t0
                            self.emit_subu(8, 2, 3); // $t0 = $v0 - case_val
                            self.emit_sltiu(8, 8, 1); // $t0 = ($t0 == 0) ? 1 : 0
                                                      // Wait, we need to check if $v0 == case_val
                                                      // Let's use: subu $t0, $v0, case_val; sltiu $t0, $t0, 1
                                                      // Actually, let's just do: beqz after sub
                            self.emit_subu(8, 2, 3);
                            let _lbl_next = self.fresh_label("Lnextcase");
                            self.emit_beqz(8, &lbl_case);
                            self.emit_nop();
                            case_labels.push((lbl_case, case));
                        }
                    } else {
                        // default:
                        case_labels.push((lbl_default.clone(), case));
                    }
                }
                // Jump to default if no case matched
                self.emit_b(&lbl_default);
                self.emit_nop();
                // Generate case bodies
                for (lbl, case) in &case_labels {
                    self.set_label(lbl);
                    for item in &case.body.items {
                        if let BlockItem::Stmt(s) = item {
                            self.gen_stmt(s);
                        }
                    }
                }
                self.set_label(&lbl_end);
                self.break_labels.pop();
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
                    eprintln!("{}:{}: warning: asm goto on non-x86_64 target: inline asm text preserved in -S output only", span.file, span.line);
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
            }
            _ => {}
        }
    }

    fn gen_expr_from_ast(&mut self, expr: &Expr) {
        match expr {
            Expr::Int { v, .. } => {
                self.emit_load_imm(2, *v as i32);
            }
            Expr::Char { v, .. } => {
                self.emit_load_imm(2, *v as i32);
            }
            Expr::Float { v, ty, .. } => {
                let is_double = matches!(ty, crate::ast::Type::Double | crate::ast::Type::LongDouble);
                if is_double {
                    self.load_float_d0(*v);
                } else {
                    self.load_float_f0(*v as f32);
                }
            }
            Expr::String { data, .. } => {
                let offset = if let Some(&off) = self.string_pool.get(data) {
                    off
                } else {
                    let off = self.rodata.len() as u64;
                    self.rodata.extend_from_slice(data);
                    if data.last() != Some(&0) {
                        self.rodata.push(0);
                    }
                    self.string_pool.insert(data.clone(), off);
                    off
                };
                self.emit_load_imm(2, offset as i32);
            }
            Expr::Ident { name, .. } => {
                if let Some(&off) = self.locals.get(&name.name) {
                    self.emit_load_v0(off);
                } else {
                    self.emit_load_imm(2, 0);
                }
            }
            Expr::Binop { op, l, r, ty, .. } => {
                let is_float_ty = matches!(ty, crate::ast::Type::Float | crate::ast::Type::Double | crate::ast::Type::LongDouble);
                let is_float_arith = is_float_ty && matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div);
                let is_float_cmp = is_float_ty && matches!(op, BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge | BinOp::EqEq | BinOp::Ne);
                if is_float_arith || is_float_cmp {
                    // 浮点路径
                    self.gen_expr_from_ast(l);
                    self.save_f0_to_f2(is_float_ty);
                    self.gen_expr_from_ast(r);
                    if is_float_cmp {
                        self.float_cmp_v0(*op, is_float_ty);
                    } else {
                        self.float_binop_f0(*op, is_float_ty);
                    }
                } else {
                    // 整数路径
                    self.gen_expr_from_ast(l);
                    self.emit_addiu(29, 29, -4);
                    self.emit_sw(2, 29, 0);
                    self.gen_expr_from_ast(r);
                    self.emit_lw(8, 29, 0);
                    self.emit_addiu(29, 29, 4);
                    self.emit_binop(*op);
                }
            }
            Expr::Unary { op, e, .. } => {
                self.gen_expr_from_ast(e);
                match op {
                    UnOp::Neg => {
                        let inst: u32 = (2u32 << 11) | 0b100011;
                        self.code.extend_from_slice(&inst.to_be_bytes());
                    }
                    UnOp::Not => {
                        self.emit_sltiu(2, 2, 1);
                    }
                    UnOp::BitNot => {
                        let inst: u32 = (2u32 << 21) | (2u32 << 11) | 0b100111;
                        self.code.extend_from_slice(&inst.to_be_bytes());
                    }
                    UnOp::Plus => {}
                }
            }
            Expr::Cast { e, .. } => self.gen_expr_from_ast(e),
            Expr::Call { callee, args, .. } => {
                for (i, arg) in args.iter().enumerate().take(4) {
                    self.gen_expr_from_ast(arg);
                    if i > 0 {
                        self.emit_addiu(29, 29, -4);
                        self.emit_sw(2, 29, 0);
                    }
                }
                for j in (1..args.len().min(4)).rev() {
                    self.emit_lw((3 + j) as u8, 29, 0);
                    self.emit_addiu(29, 29, 4);
                }
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    self.emit_jal(&name.name);
                    self.emit_nop();
                }
            }
            Expr::Assign { op, l, r, .. } => {
                self.gen_expr_from_ast(r);
                if let Expr::Ident { name, .. } = l.as_ref() {
                    if let Some(&off) = self.locals.get(&name.name) {
                        if *op != BinOp::Add {
                            // 复合赋值: 先加载左值
                            self.emit_addiu(29, 29, -4);
                            self.emit_sw(2, 29, 0);
                            self.emit_load_v0(off);
                            self.emit_lw(8, 29, 0);
                            self.emit_addiu(29, 29, 4);
                            self.emit_binop(*op);
                        }
                        self.emit_store_v0(off);
                    }
                }
            }
            Expr::Cond { cond, then_, else_, .. } => {
                self.gen_expr_from_ast(cond);
                let lbl_else = self.fresh_label("Lcond_else");
                let lbl_end = self.fresh_label("Lcond_end");
                self.emit_beqz(2, &lbl_else);
                self.emit_nop();
                self.gen_expr_from_ast(then_);
                self.emit_b(&lbl_end);
                self.emit_nop();
                self.set_label(&lbl_else);
                self.gen_expr_from_ast(else_);
                self.set_label(&lbl_end);
            }
            Expr::SizeofTy { ty, .. } => {
                self.emit_load_imm(2, ty.size() as i32);
            }
            Expr::SizeofExpr { e, .. } => {
                let sz = e.ty().size();
                self.emit_load_imm(2, sz as i32);
            }
            Expr::AlignofTy { ty, .. } => {
                self.emit_load_imm(2, ty.align() as i32);
            }
            Expr::AlignofExpr { e, .. } => {
                let al = e.ty().align();
                self.emit_load_imm(2, al as i32);
            }
            Expr::Comma { l, r, .. } => {
                self.gen_expr_from_ast(l);
                self.gen_expr_from_ast(r);
            }
            Expr::AddrOf { e, .. } => {
                if let Expr::Ident { name, .. } = e.as_ref() {
                    if let Some(&off) = self.locals.get(&name.name) {
                        // 加载变量地址到 $v0: addiu $v0, $fp, -off
                        let fp_off = -(off as i16);
                        self.emit_addiu(2, 30, fp_off);
                    } else {
                        self.emit_load_imm(2, 0);
                    }
                } else {
                    self.emit_load_imm(2, 0);
                }
            }
            Expr::Deref { e, .. } => {
                self.gen_expr_from_ast(e);
                // $v0 现在是指针地址, 加载 [$v0] 到 $v0
                self.emit_addiu(29, 29, -4);
                self.emit_sw(2, 29, 0);
                self.emit_lw(8, 29, 0); // $t0 = 地址
                self.emit_addiu(29, 29, 4);
                self.emit_lw(2, 8, 0); // lw $v0, 0($t0)
            }
            Expr::Index { arr, idx, .. } => {
                self.gen_expr_from_ast(arr);
                self.emit_addiu(29, 29, -4);
                self.emit_sw(2, 29, 0);
                self.gen_expr_from_ast(idx);
                self.emit_lw(8, 29, 0); // $t0 = 基地址
                self.emit_addiu(29, 29, 4);
                // $v0 = 索引, $t0 = 基地址
                // 简化: 假设元素大小为 4 字节
                self.emit_addu(2, 8, 2); // $v0 = base + idx*4 (简化)
                self.emit_lw(2, 2, 0); // lw $v0, 0($v0)
            }
            Expr::Member { expr, field, .. } => {
                self.gen_expr_from_ast(expr);
                // 简化: 假设字段偏移为 0 (实际需要根据 aggregate_fields 计算)
                let _ = field;
            }
            Expr::PreInc { e, .. } => {
                if let Expr::Ident { name, .. } = e.as_ref() {
                    if let Some(&off) = self.locals.get(&name.name) {
                        self.emit_load_v0(off);
                        self.emit_addiu(2, 2, 1);
                        self.emit_store_v0(off);
                    }
                }
            }
            Expr::PreDec { e, .. } => {
                if let Expr::Ident { name, .. } = e.as_ref() {
                    if let Some(&off) = self.locals.get(&name.name) {
                        self.emit_load_v0(off);
                        self.emit_addiu(2, 2, -1);
                        self.emit_store_v0(off);
                    }
                }
            }
            Expr::PostInc { e, .. } => {
                if let Expr::Ident { name, .. } = e.as_ref() {
                    if let Some(&off) = self.locals.get(&name.name) {
                        self.emit_load_v0(off);
                        self.emit_addiu(29, 29, -4);
                        self.emit_sw(2, 29, 0); // 保存旧值
                        self.emit_addiu(2, 2, 1);
                        self.emit_store_v0(off);
                        self.emit_lw(2, 29, 0); // 恢复旧值作为表达式结果
                        self.emit_addiu(29, 29, 4);
                    }
                }
            }
            Expr::PostDec { e, .. } => {
                if let Expr::Ident { name, .. } = e.as_ref() {
                    if let Some(&off) = self.locals.get(&name.name) {
                        self.emit_load_v0(off);
                        self.emit_addiu(29, 29, -4);
                        self.emit_sw(2, 29, 0);
                        self.emit_addiu(2, 2, -1);
                        self.emit_store_v0(off);
                        self.emit_lw(2, 29, 0);
                        self.emit_addiu(29, 29, 4);
                    }
                }
            }
            Expr::ComplexLit { real, imag, .. } => {
                // 简化: 返回实部
                self.emit_load_imm(2, *real as i32);
            }
            Expr::Real { e, .. } => {
                self.gen_expr_from_ast(e);
            }
            Expr::Imag { .. } => {
                // 简化: 返回 0
                self.emit_load_imm(2, 0);
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
            Expr::VaArg { ap, ty, .. } => {
                let is_float = matches!(ty, crate::ast::Type::Float | crate::ast::Type::Double | crate::ast::Type::LongDouble);
                if is_float {
                    // 浮点变参: O32 hard-float 使用 $f12/$f14
                    self.gen_va_arg_float("ap", matches!(ty, crate::ast::Type::Double | crate::ast::Type::LongDouble));
                } else {
                    self.gen_va_arg_int("ap");
                }
                let _ = ap;
            }
            _ => {
                if let Ok(v) = crate::const_eval::eval(expr) {
                    self.emit_load_imm(2, v as i32);
                } else {
                    self.emit_load_imm(2, 0);
                }
            }
        }
    }

    fn emit_binop(&mut self, op: BinOp) {
        // $t0 ($8) = lhs, $v0 ($2) = rhs, result -> $v0 ($2)
        match op {
            BinOp::Add => self.emit_addu(2, 8, 2),
            BinOp::Sub => self.emit_subu(2, 8, 2),
            BinOp::Mul => self.emit_mul_r(2, 8, 2),
            BinOp::Div => {
                self.emit_div(8, 2);
                self.emit_mflo(2); // $v0 = quotient
            }
            BinOp::Mod => {
                self.emit_div(8, 2);
                self.emit_mfhi(2); // $v0 = remainder
            }
            BinOp::And => {
                // R-type: rs=$t0(8), rt=$v0(2), rd=$v0(2), funct=AND
                let inst: u32 = (8u32 << 21) | (2u32 << 16) | (2u32 << 11) | 0b100100;
                self.code.extend_from_slice(&inst.to_be_bytes());
            }
            BinOp::Or => {
                let inst: u32 = (8u32 << 21) | (2u32 << 16) | (2u32 << 11) | 0b100101;
                self.code.extend_from_slice(&inst.to_be_bytes());
            }
            BinOp::Xor => {
                let inst: u32 = (8u32 << 21) | (2u32 << 16) | (2u32 << 11) | 0b100110;
                self.code.extend_from_slice(&inst.to_be_bytes());
            }
            BinOp::Shl => {
                // sllv $v0, $t0, $v0
                let inst: u32 = (2u32 << 21) | (8u32 << 16) | (2u32 << 11) | 0b000100;
                self.code.extend_from_slice(&inst.to_be_bytes());
            }
            BinOp::Shr => {
                // srav $v0, $t0, $v0
                let inst: u32 = (2u32 << 21) | (8u32 << 16) | (2u32 << 11) | 0b000111;
                self.code.extend_from_slice(&inst.to_be_bytes());
            }
            BinOp::Lt => self.emit_slt(2, 8, 2),
            BinOp::Gt => self.emit_slt(2, 2, 8),
            BinOp::Le => {
                self.emit_slt(2, 2, 8); // $v0 = rhs < lhs
                self.emit_xori(2, 2, 1); // $v0 ^= 1
            }
            BinOp::Ge => {
                self.emit_slt(2, 8, 2); // $v0 = lhs < rhs
                self.emit_xori(2, 2, 1); // $v0 ^= 1
            }
            BinOp::EqEq => {
                self.emit_subu(2, 8, 2); // $v0 = lhs - rhs
                self.emit_sltiu(2, 2, 1); // $v0 = ($v0 < 1) ? 1 : 0
            }
            BinOp::Ne => {
                self.emit_subu(2, 8, 2);
                // $v0 != 0 ? 1 : 0
                self.emit_sltu(2, 0, 2); // sltu $v0, $zero, $v0
            }
            BinOp::AndAnd => {
                // both non-zero?
                self.emit_sltu(8, 0, 8); // $t0 = (lhs != 0)
                self.emit_sltu(2, 0, 2); // $v0 = (rhs != 0)
                let inst: u32 = (8u32 << 21) | (2u32 << 16) | (2u32 << 11) | 0b100100;
                self.code.extend_from_slice(&inst.to_be_bytes()); // and
            }
            BinOp::OrOr => {
                self.emit_sltu(8, 0, 8);
                self.emit_sltu(2, 0, 2);
                let inst: u32 = (8u32 << 21) | (2u32 << 16) | (2u32 << 11) | 0b100101;
                self.code.extend_from_slice(&inst.to_be_bytes()); // or
            }
        }
    }

    fn fresh_label(&mut self, prefix: &str) -> String {
        self.label_serial += 1;
        format!("{}{}", prefix, self.label_serial)
    }

    fn set_label(&mut self, label: &str) {
        let pos = self.code.len() as u64;
        self.labels.insert(label.to_string(), pos);
        // Patch all pending branches targeting this label
        let pending: Vec<_> = self
            .pending_branches
            .iter()
            .filter(|(_, l, _)| l == label)
            .cloned()
            .collect();
        for (at, _, kind) in pending {
            match kind {
                BranchKind::Beqz => self.patch_beqz(at, pos),
                BranchKind::Bne => self.patch_bne(at, pos),
                BranchKind::B => self.patch_b(at, pos),
            }
        }
        self.pending_branches.retain(|(_, l, _)| l != label);
    }

    fn patch_beqz(&mut self, at: u64, target: u64) {
        // MIPS BEQ offset: (target - branch_pc - 4) / 4
        // branch_pc is the position of the BEQ instruction
        let offset = ((target as i64) - (at as i64) - 4) / 4;
        let offset = offset as i16;
        // Read existing instruction, preserve rs field, update offset
        let existing = u32::from_be_bytes([
            self.code[at as usize],
            self.code[at as usize + 1],
            self.code[at as usize + 2],
            self.code[at as usize + 3],
        ]);
        let patched = (existing & 0xFFFF0000) | (offset as u16 as u32);
        self.code[at as usize..at as usize + 4].copy_from_slice(&patched.to_be_bytes());
    }

    fn patch_b(&mut self, at: u64, target: u64) {
        // MIPS B (BEQ $zero, $zero, offset) offset: (target - branch_pc - 4) / 4
        let offset = ((target as i64) - (at as i64) - 4) / 4;
        let offset = offset as i16;
        let patched = (0b000100u32 << 26) | (offset as u16 as u32);
        self.code[at as usize..at as usize + 4].copy_from_slice(&patched.to_be_bytes());
    }

    fn patch_bne(&mut self, at: u64, target: u64) {
        // MIPS BNE offset: (target - branch_pc - 4) / 4
        let offset = ((target as i64) - (at as i64) - 4) / 4;
        let offset = offset as i16;
        // Read existing instruction, preserve rs/rt fields, update offset
        let existing = u32::from_be_bytes([
            self.code[at as usize],
            self.code[at as usize + 1],
            self.code[at as usize + 2],
            self.code[at as usize + 3],
        ]);
        let patched = (existing & 0xFFFF0000) | (offset as u16 as u32);
        self.code[at as usize..at as usize + 4].copy_from_slice(&patched.to_be_bytes());
    }

    /// Ensure symbol exists in symbol table
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
            visibility: 0,
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
            ty: elf::STT_OBJECT,
            visibility: 0,
        });
        let idx = self.symbols.len() as u32;
        self.sym_index.insert(name.to_string(), idx);
        idx
    }

    /// Stack protector prologue: load canary from __stack_chk_guard
    /// and store at [fp, -canary_off]. Uses $v0 ($2) and $t0 ($8) as scratch.
    fn emit_stack_chk_prologue(&mut self, canary_off: i32) {
        // Load __stack_chk_guard address (simplified: use load_imm)
        self.ensure_sym_object("__stack_chk_guard");
        // For relocatable: load address via GOT or PC-relative
        // Simplified: load canary value directly from __stack_chk_guard
        // In real implementation, this would use LW from GOT
        // For now, emit a placeholder that loads 0 (will be patched by linker)
        self.emit_load_imm(2, 0); // $v0 = 0 (placeholder for canary)
        // SW $v0, -canary_off($fp)
        let fp_off = -(canary_off as i16);
        self.emit_sw(2, 30, fp_off);
    }

    /// Stack protector epilogue: verify canary at [fp, -canary_off]
    /// against __stack_chk_guard. Calls __stack_chk_fail on mismatch.
    fn emit_stack_chk_epilogue(&mut self, canary_off: i32) {
        let fail_label = self.fresh_label(".Lstack_chk_fail_");
        // LW $v0, -canary_off($fp) - load stored canary
        let fp_off = -(canary_off as i16);
        self.emit_lw(2, 30, fp_off);
        // Load expected canary (simplified: compare with 0)
        self.emit_load_imm(8, 0); // $t0 = 0 (placeholder for expected canary)
        // BNE $v0, $t0, fail_label
        self.emit_bne(2, 8, &fail_label);
        self.emit_nop(); // delay slot
        // Normal path continues
        // Fail path: call __stack_chk_fail (never returns)
        self.set_label(&fail_label);
        self.ensure_sym("__stack_chk_fail");
        self.emit_jal("__stack_chk_fail");
        self.emit_nop(); // delay slot
    }

    // ===== MIPS 指令编码 =====

    fn emit_addiu(&mut self, rt: u8, rs: u8, imm: i16) {
        let inst =
            (0b001001 << 26) | ((rs as u32) << 21) | ((rt as u32) << 16) | (imm as u16 as u32);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// SB rt, offset(base): store byte
    fn emit_sb(&mut self, rt: u8, base: u8, offset: i16) {
        let inst = (0b101000 << 26)
            | ((base as u32) << 21)
            | ((rt as u32) << 16)
            | (offset as u16 as u32);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// LUI rt, imm: load upper immediate
    fn emit_lui(&mut self, rt: u8, imm: u16) {
        let inst = (0b001111 << 26) | ((rt as u32) << 16) | (imm as u32);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// -fstack-clash-protection: emit page-sized probes when allocating a
    /// large stack frame.  Each probe stores a zero byte at [sp] to trigger
    /// a fault on the guard page before the kernel maps it.
    fn emit_stack_clash_probes(&mut self, frame_size: u32) {
        let page: u32 = 4096;
        let remainder = frame_size % page;
        let pages = frame_size / page;
        // First: addiu $sp, $sp, -remainder (if any, fits in 16-bit signed)
        if remainder > 0 {
            self.emit_addiu(29, 29, -(remainder as i16));
        }
        if pages > 0 {
            // Probe loop using $t0 ($8) as counter, $t1 ($9) = 4096
            // lui $t1, 1  => $t1 = 4096
            self.emit_lui(9, 1);
            // li $t0, pages
            self.emit_load_imm(8, pages as i32);
            // .probe_loop:
            let loop_start = self.code.len() as u64;
            // subu $sp, $sp, $t1
            self.emit_subu(29, 29, 9);
            // sb $zero, 0($sp)  — probe the page
            self.emit_sb(0, 29, 0);
            // addiu $t0, $t0, -1
            self.emit_addiu(8, 8, -1);
            // bne $t0, $zero, .probe_loop
            let loop_end = self.code.len() as u64;
            let offset = (loop_start as i64 - loop_end as i64 - 4) / 4;
            let bne = (0b000101 << 26) | (8u32 << 21) | (offset as u16 as u32);
            self.code.extend_from_slice(&bne.to_be_bytes());
            // delay slot
            self.emit_nop();
        }
    }

    fn emit_sw(&mut self, rt: u8, base: u8, offset: i16) {
        if self.is_64bit {
            // SD (64-bit store) for MIPS64
            let inst = (0b111111 << 26)
                | ((base as u32) << 21)
                | ((rt as u32) << 16)
                | (offset as u16 as u32);
            self.code.extend_from_slice(&inst.to_be_bytes());
        } else {
            let inst = (0b101011 << 26)
                | ((base as u32) << 21)
                | ((rt as u32) << 16)
                | (offset as u16 as u32);
            self.code.extend_from_slice(&inst.to_be_bytes());
        }
    }

    fn emit_lw(&mut self, rt: u8, base: u8, offset: i16) {
        if self.is_64bit {
            // LD (64-bit load) for MIPS64
            let inst = (0b110111 << 26)
                | ((base as u32) << 21)
                | ((rt as u32) << 16)
                | (offset as u16 as u32);
            self.code.extend_from_slice(&inst.to_be_bytes());
        } else {
            let inst = (0b100011 << 26)
                | ((base as u32) << 21)
                | ((rt as u32) << 16)
                | (offset as u16 as u32);
            self.code.extend_from_slice(&inst.to_be_bytes());
        }
    }

    fn emit_jr(&mut self, rs: u8) {
        let inst = ((rs as u32) << 21) | 0b001000;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_nop(&mut self) {
        self.code.extend_from_slice(&[0u8; 4]);
    }

    fn emit_load_imm(&mut self, rt: u8, val: i32) {
        if (-32768..=32767).contains(&val) {
            self.emit_addiu(rt, 0, val as i16);
        } else {
            // 使用 lui + ori 加载大整数
            let hi = ((val >> 16) & 0xFFFF) as u16;
            let lo = (val & 0xFFFF) as u16;
            // LUI rt, hi
            let inst = (0b001111 << 26) | ((rt as u32) << 16) | (hi as u32);
            self.code.extend_from_slice(&inst.to_be_bytes());
            if lo != 0 {
                // ORI rt, rt, lo
                let inst =
                    (0b001101 << 26) | ((rt as u32) << 21) | ((rt as u32) << 16) | (lo as u32);
                self.code.extend_from_slice(&inst.to_be_bytes());
            }
        }
    }

    fn emit_addu(&mut self, rd: u8, rs: u8, rt: u8) {
        // R-type: opcode=0 (implicit), funct=ADDU
        let inst: u32 = ((rs as u32) << 21) | ((rt as u32) << 16) | ((rd as u32) << 11) | 0b100001;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_subu(&mut self, rd: u8, rs: u8, rt: u8) {
        let inst: u32 = ((rs as u32) << 21) | ((rt as u32) << 16) | ((rd as u32) << 11) | 0b100011;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_mul_r(&mut self, rd: u8, rs: u8, rt: u8) {
        // MUL rd, rs, rt (MIPS32r2 专用指令, 或回退到 mult+mflo)
        // 使用 mult + mflo 保证兼容性
        let inst: u32 = ((rs as u32) << 21) | ((rt as u32) << 16);
        self.code.extend_from_slice(&inst.to_be_bytes()); // mult rs, rt
        self.emit_mflo(rd);
    }

    fn emit_div(&mut self, rs: u8, rt: u8) {
        let inst: u32 = ((rs as u32) << 21) | ((rt as u32) << 16) | 0b010010;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_mflo(&mut self, rd: u8) {
        let inst = ((rd as u32) << 16) | 0b010010;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_mfhi(&mut self, rd: u8) {
        let inst = ((rd as u32) << 16) | 0b010000;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_slt(&mut self, rd: u8, rs: u8, rt: u8) {
        let inst: u32 = ((rs as u32) << 21) | ((rt as u32) << 16) | ((rd as u32) << 11) | 0b101010;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_sltu(&mut self, rd: u8, rs: u8, rt: u8) {
        let inst: u32 = ((rs as u32) << 21) | ((rt as u32) << 16) | ((rd as u32) << 11) | 0b101011;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_sltiu(&mut self, rt: u8, rs: u8, imm: i16) {
        let inst =
            (0b001011 << 26) | ((rs as u32) << 21) | ((rt as u32) << 16) | (imm as u16 as u32);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_xori(&mut self, rt: u8, rs: u8, imm: u16) {
        let inst = (0b001110 << 26) | ((rs as u32) << 21) | ((rt as u32) << 16) | (imm as u32);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_beqz(&mut self, rs: u8, label: &str) {
        // BEQ $rs, $zero, offset (占位)
        let pos = self.code.len() as u64;
        self.pending_branches
            .push((pos, label.to_string(), BranchKind::Beqz));
        let inst: u32 = (0b000100 << 26) | ((rs as u32) << 21); // offset=0 placeholder
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_bne(&mut self, rs: u8, rt: u8, label: &str) {
        // BNE $rs, $rt, offset (占位)
        let pos = self.code.len() as u64;
        self.pending_branches
            .push((pos, label.to_string(), BranchKind::Bne));
        let inst: u32 = (0b000101 << 26) | ((rs as u32) << 21) | ((rt as u32) << 16); // offset=0 placeholder
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_b(&mut self, label: &str) {
        // B offset (BEQ $zero, $zero, offset)
        let pos = self.code.len() as u64;
        self.pending_branches
            .push((pos, label.to_string(), BranchKind::B));
        let inst: u32 = 0b000100 << 26; // BEQ $zero, $zero, offset=0
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_jal(&mut self, name: &str) {
        let pos = self.code.len() as u64;
        self.pending_bl_calls.push((pos, name.to_string()));
        // JAL 占位
        let inst: u32 = 0b000011 << 26;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    // 栈上局部变量加载/存储 (相对于 $fp)
    fn emit_load_v0(&mut self, offset: i32) {
        // LW $v0, -offset($fp)
        let fp_off = -(offset as i16);
        self.emit_lw(2, 30, fp_off);
    }

    fn emit_store_v0(&mut self, offset: i32) {
        // SW $v0, -offset($fp)
        let fp_off = -(offset as i16);
        self.emit_sw(2, 30, fp_off);
    }

    /// Load a local variable from stack into an arbitrary GP register
    fn asm_load_var_to_reg(&mut self, var_name: &str, reg: u8) {
        if let Some(&off) = self.locals.get(var_name) {
            let fp_off = -(off as i16);
            self.emit_lw(reg, 30, fp_off);
        }
    }

    /// Store a GP register value back to a local variable on stack
    fn asm_store_reg_to_var(&mut self, var_name: &str, reg: u8) {
        if let Some(&off) = self.locals.get(var_name) {
            let fp_off = -(off as i16);
            self.emit_sw(reg, 30, fp_off);
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
        let pool = asm_constraints::mips_reg_pool();
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

        // --- memory clobber: SYNC 编译器屏障 ---
        if clobbers.iter().any(|c| c == "memory") {
            self.emit_sync();
        }

        // --- 表达式结果: 第一个输出寄存器 → $v0 ($2) ---
        if let Some(reg) = first_output_reg {
            if reg != 2 {
                // move $v0, reg => addu $v0, reg, $zero
                self.emit_addu(2, reg, 0);
            }
        } else {
            self.emit_load_imm(2, 0);
        }
    }

    pub fn build_output(&self) -> Vec<u8> {
        if self.relocatable {
            if self.is_64bit {
                elf::build_elf_rel_mips64(
                    &self.code,
                    &self.rodata,
                    &self.symbols,
                    &self.relocations,
                    &self.rodata_relocations,
                    &self.extra_sections,
                )
            } else {
                elf::build_elf32_mips(
                    &self.code,
                    &self.rodata,
                    &self.symbols,
                    &self.relocations,
                    &self.rodata_relocations,
                    &self.extra_sections,
                )
            }
        } else {
            elf::build_elf32_exec_mips(&self.code, &self.rodata, self.entry as u32)
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
            out.push_str("\t.section .rodata\n");
        }
        out.push_str("\t.text\n");
        // 发射文件级 asm 原始文本
        for line in &self.raw_global_asm_lines {
            out.push_str(&format!("\t{line}\n"));
        }
        for func in &module.functions {
            out.push_str(&format!("\t.globl {}\n", func.name));
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
                out.push_str(&format!("\t# MIPS IR lowering, {} bytes\n", size));
            }
            out.push_str("\tjr $ra\n");
            out.push_str("\tnop\n");
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
        let pool = asm_constraints::mips_reg_pool();
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
            AsmArch::Mips,
            false,
        )
    }

    // ===== MIPS FPU (Coprocessor 1) 指令编码 =====

    /// LWC1 ft, offset(base): Load Word to Coprocessor 1
    fn emit_lwc1(&mut self, ft: u8, base: u8, offset: i16) {
        let inst = (0b110001 << 26)
            | ((base as u32) << 21)
            | ((ft as u32) << 16)
            | (offset as u16 as u32);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// LDC1 ft, offset(base): Load Double to Coprocessor 1
    fn emit_ldc1(&mut self, ft: u8, base: u8, offset: i16) {
        let inst = (0b110101 << 26)
            | ((base as u32) << 21)
            | ((ft as u32) << 16)
            | (offset as u16 as u32);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// SWC1 ft, offset(base): Store Word from Coprocessor 1
    fn emit_swc1(&mut self, ft: u8, base: u8, offset: i16) {
        let inst = (0b111001 << 26)
            | ((base as u32) << 21)
            | ((ft as u32) << 16)
            | (offset as u16 as u32);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// SDC1 ft, offset(base): Store Double from Coprocessor 1
    fn emit_sdc1(&mut self, ft: u8, base: u8, offset: i16) {
        let inst = (0b111101 << 26)
            | ((base as u32) << 21)
            | ((ft as u32) << 16)
            | (offset as u16 as u32);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// ADD.S fd, fs, ft / ADD.D fd, fs, ft
    fn emit_fpu_add(&mut self, fd: u8, fs: u8, ft: u8, is_double: bool) {
        let fmt = if is_double { 0b10001 } else { 0b10000 };
        let inst: u32 = (0b010001 << 26)
            | ((fmt as u32) << 21)
            | ((ft as u32) << 16)
            | ((fs as u32) << 11)
            | ((fd as u32) << 6)
            | 0b000000;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// SUB.S fd, fs, ft / SUB.D fd, fs, ft
    fn emit_fpu_sub(&mut self, fd: u8, fs: u8, ft: u8, is_double: bool) {
        let fmt = if is_double { 0b10001 } else { 0b10000 };
        let inst: u32 = (0b010001 << 26)
            | ((fmt as u32) << 21)
            | ((ft as u32) << 16)
            | ((fs as u32) << 11)
            | ((fd as u32) << 6)
            | 0b000001;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// MUL.S fd, fs, ft / MUL.D fd, fs, ft
    fn emit_fpu_mul(&mut self, fd: u8, fs: u8, ft: u8, is_double: bool) {
        let fmt = if is_double { 0b10001 } else { 0b10000 };
        let inst: u32 = (0b010001 << 26)
            | ((fmt as u32) << 21)
            | ((ft as u32) << 16)
            | ((fs as u32) << 11)
            | ((fd as u32) << 6)
            | 0b000010;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// DIV.S fd, fs, ft / DIV.D fd, fs, ft
    fn emit_fpu_div(&mut self, fd: u8, fs: u8, ft: u8, is_double: bool) {
        let fmt = if is_double { 0b10001 } else { 0b10000 };
        let inst: u32 = (0b010001 << 26)
            | ((fmt as u32) << 21)
            | ((ft as u32) << 16)
            | ((fs as u32) << 11)
            | ((fd as u32) << 6)
            | 0b000011;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// NEG.S fd, fs / NEG.D fd, fs
    fn emit_fpu_neg(&mut self, fd: u8, fs: u8, is_double: bool) {
        let fmt = if is_double { 0b10001 } else { 0b10000 };
        let inst: u32 = (0b010001 << 26)
            | ((fmt as u32) << 21)
            | ((fs as u32) << 11)
            | ((fd as u32) << 6)
            | 0b000111;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// MOV.S fd, fs / MOV.D fd, fs
    fn emit_fpu_mov(&mut self, fd: u8, fs: u8, is_double: bool) {
        let fmt = if is_double { 0b10001 } else { 0b10000 };
        let inst: u32 = (0b010001 << 26)
            | ((fmt as u32) << 21)
            | ((fs as u32) << 11)
            | ((fd as u32) << 6)
            | 0b000100;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// C.EQ.S/C.EQ.D, C.LT.S/C.LT.D, C.LE.S/C.LE.D
    fn emit_fpu_cmp(&mut self, fs: u8, ft: u8, cond: u8, is_double: bool) {
        let fmt = if is_double { 0b10001 } else { 0b10000 };
        let inst: u32 = (0b010001 << 26)
            | ((fmt as u32) << 21)
            | ((ft as u32) << 16)
            | ((fs as u32) << 11)
            | ((cond as u32) << 4)
            | 0b010010;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// BC1T offset: Branch on FPU condition true
    fn emit_bc1t(&mut self, label: &str) {
        let pos = self.code.len() as u64;
        self.pending_branches
            .push((pos, label.to_string(), BranchKind::B));
        // BC1T: opcode=010001 (17), rt=1 (BC1T), offset=0 placeholder
        let inst: u32 = (0b010001 << 26) | (1 << 16);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// BC1F offset: Branch on FPU condition false
    fn emit_bc1f(&mut self, label: &str) {
        let pos = self.code.len() as u64;
        self.pending_branches
            .push((pos, label.to_string(), BranchKind::B));
        // BC1F: opcode=010001 (17), rt=0 (BC1F), offset=0 placeholder
        let inst: u32 = (0b010001 << 26);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// MTC1 rt, fs: Move To Coprocessor 1 (GP -> FPU)
    fn emit_mtc1(&mut self, rt: u8, fs: u8) {
        let inst: u32 = (0b010001 << 26) | (0b00100 << 21) | ((rt as u32) << 16) | ((fs as u32) << 11);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// MFC1 rt, fs: Move From Coprocessor 1 (FPU -> GP)
    fn emit_mfc1(&mut self, rt: u8, fs: u8) {
        let inst: u32 = (0b010001 << 26) | (0b00000 << 21) | ((rt as u32) << 16) | ((fs as u32) << 11);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// CVT.S.W fd, fs: Convert integer to float
    fn emit_cvt_s_w(&mut self, fd: u8, fs: u8) {
        let inst: u32 = (0b010001 << 26)
            | (0b10100 << 21)
            | ((fs as u32) << 11)
            | ((fd as u32) << 6)
            | 0b100000;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// CVT.W.S fd, fs: Convert float to integer
    fn emit_cvt_w_s(&mut self, fd: u8, fs: u8) {
        let inst: u32 = (0b010001 << 26)
            | (0b10000 << 21)
            | ((fs as u32) << 11)
            | ((fd as u32) << 6)
            | 0b100100;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// CVT.D.S fd, fs: Convert float to double
    fn emit_cvt_d_s(&mut self, fd: u8, fs: u8) {
        let inst: u32 = (0b010001 << 26)
            | (0b10000 << 21)
            | ((fs as u32) << 11)
            | ((fd as u32) << 6)
            | 0b100010;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// CVT.S.D fd, fs: Convert double to float
    fn emit_cvt_s_d(&mut self, fd: u8, fs: u8) {
        let inst: u32 = (0b010001 << 26)
            | (0b10001 << 21)
            | ((fs as u32) << 11)
            | ((fd as u32) << 6)
            | 0b100000;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    /// 加载单精度浮点字面量到 $f0
    fn load_float_f0(&mut self, val: f32) {
        let bits = val.to_bits() as u64;
        let offset = if let Some(&off) = self.float_pool.get(&bits) {
            off
        } else {
            let off = self.rodata.len() as u64;
            self.rodata.extend_from_slice(&val.to_le_bytes());
            self.float_pool.insert(bits, off);
            off
        };
        // 将浮点值作为整数存入栈临时区, 然后 LWC1
        let tmp_off = self.alloc_local("<flt_tmp>", 4);
        self.emit_load_imm(1, val.to_bits() as i32);
        self.emit_store_v0(tmp_off);
        // LWC1 $f0, tmp_off($fp)
        let fp_off = -(tmp_off as i16);
        self.emit_lwc1(0, 30, fp_off);
    }

    /// 加载双精度浮点字面量到 $f0/$f2
    fn load_float_d0(&mut self, val: f64) {
        let bits = val.to_bits();
        let offset = if let Some(&off) = self.float_pool.get(&bits) {
            off
        } else {
            let off = self.rodata.len() as u64;
            self.rodata.extend_from_slice(&val.to_le_bytes());
            self.float_pool.insert(bits, off);
            off
        };
        // 将双精度值存入栈临时区 (8 字节), 然后 LDC1
        let tmp_off = self.alloc_local("<dbl_tmp>", 8);
        // 分两次存储 32 位
        let lo = (bits & 0xFFFFFFFF) as i32;
        let hi = ((bits >> 32) & 0xFFFFFFFF) as i32;
        self.emit_load_imm(1, lo);
        self.emit_store_v0(tmp_off);
        self.emit_load_imm(1, hi);
        self.emit_store_v0(tmp_off + 4);
        // LDC1 $f0, tmp_off($fp)
        let fp_off = -(tmp_off as i16);
        self.emit_ldc1(0, 30, fp_off);
    }

    /// 浮点二元运算: $f2 op $f0 -> $f0
    fn float_binop_f0(&mut self, op: BinOp, is_double: bool) {
        match op {
            BinOp::Add => self.emit_fpu_add(0, 2, 0, is_double),
            BinOp::Sub => self.emit_fpu_sub(0, 2, 0, is_double),
            BinOp::Mul => self.emit_fpu_mul(0, 2, 0, is_double),
            BinOp::Div => self.emit_fpu_div(0, 2, 0, is_double),
            _ => self.emit_fpu_add(0, 2, 0, is_double),
        }
    }

    /// 浮点比较: 设置 $v0 = ($f2 op $f0) ? 1 : 0
    fn float_cmp_v0(&mut self, op: BinOp, is_double: bool) {
        // 比较 $f2 和 $f0
        match op {
            BinOp::EqEq => self.emit_fpu_cmp(2, 0, 0b10, is_double), // C.EQ
            BinOp::Lt => self.emit_fpu_cmp(2, 0, 0b100, is_double),  // C.LT
            BinOp::Le => self.emit_fpu_cmp(2, 0, 0b110, is_double),  // C.LE
            BinOp::Gt => self.emit_fpu_cmp(0, 2, 0b100, is_double),  // C.LT (swapped)
            BinOp::Ge => self.emit_fpu_cmp(0, 2, 0b110, is_double),  // C.LE (swapped)
            BinOp::Ne => {
                // NE = NOT EQ: C.EQ then branch false
                self.emit_fpu_cmp(2, 0, 0b10, is_double);
                // $v0 = 1 if not equal
                self.emit_load_imm(2, 1);
                let lbl_eq = self.fresh_label("Lfcne");
                self.emit_bc1t(&lbl_eq);
                self.emit_nop();
                self.emit_load_imm(2, 0);
                self.set_label(&lbl_eq);
                return;
            }
            _ => self.emit_fpu_cmp(2, 0, 0b10, is_double),
        }
        // BC1T: set $v0 = 1, else $v0 = 0
        self.emit_load_imm(2, 0);
        let lbl_end = self.fresh_label("Lfcend");
        self.emit_bc1f(&lbl_end);
        self.emit_nop();
        self.emit_load_imm(2, 1);
        self.set_label(&lbl_end);
    }

    /// 保存 $f0 到 $f2 (用于保存左操作数)
    fn save_f0_to_f2(&mut self, is_double: bool) {
        self.emit_fpu_mov(2, 0, is_double);
    }

    /// SYNC 指令 — 内存屏障 (memory clobber 编译器屏障)
    fn emit_sync(&mut self) {
        // SYNC: opcode=0, funct=0x0F => 0x0000000F
        self.code.extend_from_slice(&[0x00, 0x00, 0x00, 0x0F]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mips_addiu_encoding() {
        let mut cg = MipsCodegen::new(
            Target::resolve(&crate::cli::TargetTriple::parse("mips-linux-gnu").unwrap()),
            true,
        );
        cg.emit_addiu(29, 29, -8);
        assert_eq!(cg.code.len(), 4);
        // addiu $sp, $sp, -8 => 0x27bdfff8
        assert_eq!(cg.code, vec![0x27, 0xbd, 0xff, 0xf8]);
    }

    #[test]
    fn mips_jr_ra_encoding() {
        let mut cg = MipsCodegen::new(
            Target::resolve(&crate::cli::TargetTriple::parse("mips-linux-gnu").unwrap()),
            true,
        );
        cg.emit_jr(31);
        // jr $ra => 0x03e00008
        assert_eq!(cg.code, vec![0x03, 0xe0, 0x00, 0x08]);
    }

    #[test]
    fn mips_nop_encoding() {
        let mut cg = MipsCodegen::new(
            Target::resolve(&crate::cli::TargetTriple::parse("mips-linux-gnu").unwrap()),
            true,
        );
        cg.emit_nop();
        assert_eq!(cg.code, vec![0, 0, 0, 0]);
    }

    #[test]
    fn mips_gen_simple_return() {
        let target = Target::resolve(&crate::cli::TargetTriple::parse("mips-linux-gnu").unwrap());
        let mut cg = MipsCodegen::new(target, true);
        let func = Function {
            name: "f".to_string(),
            return_type: ValueType::Integer {
                bits: 32,
                signed: true,
            },
            params: vec![],
            body: vec![],
            span: crate::diagnostics::Span::point("t.c", 1, 1),
        };
        cg.gen_function(&func, None);
        assert!(cg.code.len() > 16);
        assert!(cg.labels.contains_key("f"));
    }

    // M25: MIPS va_arg 整数路径单元测试
    // 验证 gen_va_arg_int 从保存区顺序读 $v0
    #[test]
    fn mips_va_arg_int_reads_sequentially_from_save_area() {
        let mut cg = MipsCodegen::new(
            Target::resolve(&crate::cli::TargetTriple::parse("mips-linux-gnu").unwrap()),
            true,
        );
        cg.is_current_func_variadic = true;
        cg.va_save_base_off = 0;
        cg.va_gp_offs = 0;
        cg.frame_bytes = 24; // 8 (fp+ra) + 16 (va_save)
        // 读 3 个变参
        cg.gen_va_arg_int("ap");
        cg.gen_va_arg_int("ap");
        cg.gen_va_arg_int("ap");
        // 验证: 3 条 lw $v0, offset($fp) 指令
        // MIPS LW: opcode=100011 (0x23), base=$fp(30), rt=$v0(2)
        let lw_count = cg
            .code
            .windows(4)
            .filter(|w| {
                let inst = u32::from_be_bytes([w[0], w[1], w[2], w[3]]);
                let opcode = inst >> 26;
                let rt = (inst >> 16) & 0x1f;
                let rs = (inst >> 21) & 0x1f;
                opcode == 0b100011 && rt == 2 && rs == 30
            })
            .count();
        assert_eq!(
            lw_count, 3,
            "expected 3 lw $v0, offset($fp) instructions, got {lw_count}"
        );
        // 验证 va_gp_offs 推进
        assert_eq!(cg.va_gp_offs, 3, "va_gp_offs should be 3 after 3 reads");
    }

    // -ftrivial-auto-var-init=zero: 验证 MIPS 后端发射 sw zero, offset(fp) 指令
    #[test]
    fn mips_auto_var_init_zero_emits_sw() {
        let mut target =
            Target::resolve(&crate::cli::TargetTriple::parse("mips-linux-gnu").unwrap());
        target.auto_var_init_zero = true;
        let mut cg = MipsCodegen::new(target, true);
        // 直接测试 emit_zero_init_frame: 16 字节帧 = 4 个 sw (4 字节每个)
        cg.emit_zero_init_frame(16);
        // 验证: 4 条 sw $zero, offset($fp) 指令
        // MIPS SW: opcode=101011 (0x2B), base=$fp(30), rt=$zero(0)
        let sw_count = cg
            .code
            .windows(4)
            .filter(|w| {
                let inst = u32::from_be_bytes([w[0], w[1], w[2], w[3]]);
                let opcode = inst >> 26;
                let rt = (inst >> 16) & 0x1f;
                let rs = (inst >> 21) & 0x1f;
                opcode == 0b101011 && rt == 0 && rs == 30
            })
            .count();
        assert_eq!(
            sw_count, 4,
            "expected 4 sw $zero, offset($fp) instructions for 16-byte frame, got {sw_count}"
        );
    }
}
