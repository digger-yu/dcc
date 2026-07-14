// src/codegen/x86_64.rs - x86_64 代码生成
// 第一性原理: x86_64 指令编码固定,寄存器按 SysV ABI 选。
// suckless: 直接 emit 字节,不引入汇编库。

use crate::ast::*;
use crate::target::Target;
use crate::codegen::elf::{ElfSym, ElfRela, ElfSection, R_X86_64_PC32, R_X86_64_PLT32, R_X86_64_64,
    STB_LOCAL, STB_GLOBAL, STB_WEAK, STT_NOTYPE, STT_FUNC, STT_OBJECT, STT_SECTION, SHN_UNDEF,
    STV_DEFAULT, STV_HIDDEN, STV_PROTECTED,
    SHT_PROGBITS, SHF_ALLOC, SHF_WRITE, SHF_EXECINSTR};
use crate::codegen::elf;
use std::collections::HashMap;
use crate::ast::ArraySize;

pub struct Codegen {
    pub code: Vec<u8>,
    pub entry: u64,
    pub target: Target,
    labels: HashMap<String, u64>,
    pending: Vec<PendingJump>,
    next_label: u32,
    var_map: HashMap<String, (i32, Type)>,
    loop_continue: Option<String>,
    loop_break: Option<String>,
    // .rodata: 字符串池 + 待修补引用
    pub rodata: Vec<u8>,
    string_pool: HashMap<Vec<u8>, u64>,   // data -> offset in rodata
    string_refs: Vec<(u64, Vec<u8>)>,     // (code_pos, data)
    float_refs: Vec<(u64, u64)>,          // (code_pos, rodata_offset) - 浮点字面量引用
    // 全局变量
    globals: HashMap<String, GlobalVar>,
    frame_size: i32,
    expr_depth: u32,              // 当前表达式嵌套深度 (用于帧临时槽分配)
    max_expr_depth: u32,          // 最大表达式嵌套深度
    temp_slots: i32,              // 帧临时槽数量 (用于变量偏移计算)
    vla_offsets: HashMap<String, i32>,
    vla_total_size: i32,
    // 可重定位模式 (.o)
    pub relocatable: bool,
    pub symbols: Vec<ElfSym>,
    sym_index: HashMap<String, u32>,
    pub relocations: Vec<ElfRela>,
    pending_func_rels: Vec<(u64, String)>, // (code_offset, func_name) - 延迟到符号表构建后解析
    pending_data_rels: Vec<(u64, String)>,  // (disp32_pos, var_name) - 全局变量重定位
    pub data_section: Vec<u8>,    // .data 节 (已初始化全局变量)
    pub bss_size: u64,            // .bss 大小
    pub extra_sections: Vec<elf::ElfSection>, // 自定义节 (section 属性)
    custom_sec_indices: HashMap<String, usize>, // section name → index in extra_sections
    func_sizes: HashMap<String, u64>, // 函数名 → 代码大小 (用于符号 size)
    variadic_funcs: HashMap<String, bool>, // 函数名 → 是否可变参数
    variadic_named_params: i32, // 当前可变参数函数的命名参数数量
    // 嵌套函数静态链
    nested_funcs: HashMap<String, Vec<(String, i32)>>, // 嵌套函数名 → upvars (name, parent_offset)
    current_func_is_nested: bool,  // 当前编译的函数是否是嵌套函数
    static_chain_offset: i32,      // 静态链在当前帧中的偏移
    upvar_offsets: HashMap<String, i32>, // upvar 名称 → 在父帧中的偏移
    // &&label 地址引用: (disp32_pos, label_name)
    label_addr_refs: Vec<(u64, String)>,
    // register asm 变量绑定: 变量名 → 寄存器编号
    reg_bindings: HashMap<String, u8>,
    // data 节 64 位符号引用重定位: (data_section_offset, symbol_name)
    pending_data64_rels: Vec<(u64, String)>,
    // data 节最终重定位条目
    pub data_relocations: Vec<ElfRela>,
    // cleanup 属性变量: (变量名, 清理函数名)
    cleanup_vars: Vec<(String, String)>,
}

struct PendingJump { at: u64, target: String, kind: JumpKind }
#[derive(PartialEq)]
enum JumpKind { Jmp, Je, Jne, Jl, Jle, Jg, Jge }
#[derive(Clone)]
struct GlobalVar { offset: u64, size: usize, ty: Type, init: Option<Vec<u8>> }

/// 内联汇编操作数
enum AsmOperand {
    None,
    Reg(u8, u8),           // (reg_index, size_bits)
    Imm(i64),              // 立即数
    Mem { disp: i32, base: u8, index: Option<u8>, scale: u8 },
    Sym(String),           // 符号/标签名
}

impl Codegen {
    pub fn new(target: Target) -> Self {
        Self { code: vec![], entry: 0, target, labels: Default::default(),
               pending: vec![], next_label: 0, var_map: HashMap::new(),
               loop_continue: None, loop_break: None,
               rodata: vec![], string_pool: HashMap::new(), string_refs: vec![], float_refs: vec![],
               globals: HashMap::new(), frame_size: 0, expr_depth: 0, max_expr_depth: 0, temp_slots: 0,
               vla_offsets: HashMap::new(),
               vla_total_size: 0,
               relocatable: false, symbols: vec![], sym_index: HashMap::new(),
               relocations: vec![], pending_func_rels: vec![], pending_data_rels: vec![],
               data_section: vec![], bss_size: 0, extra_sections: vec![],
               custom_sec_indices: HashMap::new(), func_sizes: HashMap::new(),
               variadic_funcs: HashMap::new(),
               variadic_named_params: 0,
               nested_funcs: HashMap::new(),
               current_func_is_nested: false,
               static_chain_offset: 0,
               upvar_offsets: HashMap::new(),
               label_addr_refs: vec![],
               reg_bindings: HashMap::new(),
               pending_data64_rels: vec![],
               data_relocations: vec![], cleanup_vars: vec![] }
    }
    pub fn emit(&mut self, b: &[u8]) { self.code.extend_from_slice(b); }
    pub fn pos(&self) -> u64 { self.code.len() as u64 }
    pub fn label(&mut self, name: &str) { self.labels.insert(name.to_string(), self.pos()); }
    pub fn fresh_label(&mut self) -> String { let n = self.next_label; self.next_label += 1; format!(".L{}", n) }

    /// 解析 visibility 字符串为 ELF 常量
    fn parse_visibility(vis: &Option<String>) -> u8 {
        match vis.as_deref() {
            Some("hidden") => STV_HIDDEN,
            Some("protected") => STV_PROTECTED,
            _ => STV_DEFAULT,
        }
    }

    /// P1-5: 解析寄存器名 ("r10", "eax" 等) 为寄存器编号
    fn parse_reg_name(name: &str) -> Option<u8> {
        match name.to_lowercase().as_str() {
            "al" | "ah" | "ax" | "eax" | "rax" | "%rax" | "%eax" | "%ax" => Some(0),
            "bl" | "bh" | "bx" | "ebx" | "rbx" | "%rbx" | "%ebx" | "%bx" => Some(3),
            "cl" | "ch" | "cx" | "ecx" | "rcx" | "%rcx" | "%ecx" | "%cx" => Some(1),
            "dl" | "dh" | "dx" | "edx" | "rdx" | "%rdx" | "%edx" | "%dx" => Some(2),
            "sil" | "si" | "esi" | "rsi" | "%rsi" | "%esi" | "%si" => Some(6),
            "dil" | "di" | "edi" | "rdi" | "%rdi" | "%edi" | "%di" => Some(7),
            "r8" | "r8d" | "r8w" | "r8b" | "%r8" => Some(8),
            "r9" | "r9d" | "r9w" | "r9b" | "%r9" => Some(9),
            "r10" | "r10d" | "r10w" | "r10b" | "%r10" => Some(10),
            "r11" | "r11d" | "r11w" | "r11b" | "%r11" => Some(11),
            "r12" | "r12d" | "r12w" | "r12b" | "%r12" => Some(12),
            "r13" | "r13d" | "r13w" | "r13b" | "%r13" => Some(13),
            "r14" | "r14d" | "r14w" | "r14b" | "%r14" => Some(14),
            "r15" | "r15d" | "r15w" | "r15b" | "%r15" => Some(15),
            _ => None,
        }
    }

    /// 判断函数是否需要放入自定义 section
    fn is_custom_section_func(f: &FuncDecl) -> bool {
        f.section.as_ref().map_or(false, |s| !s.is_empty())
    }
    /// 判断变量是否需要放入自定义 section
    fn is_custom_section_var(v: &VarDecl) -> bool {
        v.section.as_ref().map_or(false, |s| !s.is_empty())
    }
    /// 确保自定义 section 存在于 extra_sections 中，返回其在 vec 中的索引
    fn ensure_custom_section(&mut self, name: &str, is_exec: bool, is_writable: bool) -> usize {
        if let Some(&idx) = self.custom_sec_indices.get(name) {
            return idx;
        }
        let idx = self.extra_sections.len();
        let mut flags = SHF_ALLOC;
        if is_writable { flags |= SHF_WRITE; }
        if is_exec { flags |= SHF_EXECINSTR; }
        self.extra_sections.push(ElfSection {
            name: name.to_string(),
            data: Vec::new(),
            ty: SHT_PROGBITS,
            flags,
            align: if is_exec { 16 } else { 8 },
        });
        self.custom_sec_indices.insert(name.to_string(), idx);
        idx
    }

    fn jmp_rel32(&mut self, target: &str, kind: JumpKind) {
        let at = self.pos();
        if kind == JumpKind::Jmp { self.emit(&[0xE9, 0, 0, 0, 0]); }
        else {
            let cc = match kind { JumpKind::Je=>0x84, JumpKind::Jne=>0x85, JumpKind::Jl=>0x8C,
                JumpKind::Jle=>0x8E, JumpKind::Jg=>0x8F, JumpKind::Jge=>0x8D, _=>unreachable!() };
            self.emit(&[0x0F, cc, 0, 0, 0, 0]);
        }
        self.pending.push(PendingJump { at, target: target.into(), kind });
    }

    pub fn resolve(&mut self, rodata_start: u64) {
        // 修补跳转
        for p in self.pending.drain(..) {
            let dst = *self.labels.get(&p.target).expect(&format!("unresolved label: {}", p.target));
            let off = p.at as usize;
            let (rel_from, rel_off) = match p.kind { JumpKind::Jmp => (p.at+5, off+1), _ => (p.at+6, off+2) }; // Jmp=5字节, Jcc=6字节
            let rel = (dst as i64) - (rel_from as i64);
            self.code[rel_off..rel_off+4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
        // 修补 RIP-relative 字符串引用 (LEA dst, [RIP+disp32])
        for (code_pos, data) in self.string_refs.drain(..) {
            let str_off = self.string_pool[&data];
            let str_vaddr = rodata_start + str_off;
            let rip = self.entry + code_pos + 7; // LEA is 7 bytes
            let rel = (str_vaddr as i64) - (rip as i64);
            let p = code_pos as usize + 3; // disp32 starts at byte 3
            self.code[p..p+4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
        // 修补 RIP-relative 浮点字面量引用 (movsd/movss xmm0, [RIP+disp32])
        for (code_pos, rodata_off) in self.float_refs.drain(..) {
            let flt_vaddr = rodata_start + rodata_off;
            let rip = self.entry + code_pos + 8; // movsd/movss is 8 bytes
            let rel = (flt_vaddr as i64) - (rip as i64);
            let p = code_pos as usize + 4; // disp32 starts at byte 4
            self.code[p..p+4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
        // 修补 RIP-relative 全局变量引用 (mov/lea reg, [RIP+disp32])
        for (disp32_pos, var_name) in self.pending_data_rels.drain(..) {
            if let Some(g) = self.globals.get(&var_name) {
                let var_vaddr = rodata_start + g.offset;
                let rip = self.entry + disp32_pos + 4; // disp32 后紧跟 4 字节
                let rel = (var_vaddr as i64) - (rip as i64);
                let p = disp32_pos as usize;
                self.code[p..p+4].copy_from_slice(&(rel as i32).to_le_bytes());
            }
        }
        // 修补 &&label 地址引用 (LEA reg, [RIP+disp32])
        for (disp32_pos, label_name) in self.label_addr_refs.drain(..) {
            if let Some(&label_off) = self.labels.get(&label_name) {
                let label_vaddr = self.entry + label_off;
                let rip = self.entry + disp32_pos + 4;
                let rel = (label_vaddr as i64) - (rip as i64);
                let p = disp32_pos as usize;
                self.code[p..p+4].copy_from_slice(&(rel as i32).to_le_bytes());
            }
        }
    }

    pub fn compile(&mut self, prog: &Program) {
        // 预扫描: 记录哪些函数是可变参数的
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if f.is_variadic { self.variadic_funcs.insert(f.name.name.clone(), true); }
                // 记录嵌套函数及其 upvars
                if f.is_nested { self.nested_funcs.insert(f.name.name.clone(), f.upvars.clone()); }
            }
        }
        if self.relocatable {
            self.compile_rel(prog);
        } else {
            self.compile_exec(prog);
        }
    }

    /// ET_EXEC 模式: 生成可直接执行的 ELF
    fn compile_exec(&mut self, prog: &Program) {
        self.entry = 0x400000 + 64 + 56;
        // 收集全局变量到 rodata (声明顺序分配偏移, 直接写入初始化数据)
        for decl in &prog.decls {
            if let Decl::Var(v) = decl {
                let sz = v.ty.size().max(1);
                let align = v.alignment.unwrap_or(v.ty.align()).max(1);
                let off = (self.rodata.len() + align - 1) & !(align - 1);
                while self.rodata.len() < off { self.rodata.push(0); }
                let offset = self.rodata.len() as u64;
                let init_data = if let Some(init) = &v.init {
                    match init {
                        Expr::Int { v, .. } => Some(v.to_le_bytes()[..sz].to_vec()),
                        Expr::String { data, .. } => Some(data.clone()),
                        Expr::InitList { inits, .. } => self.gen_global_init_list(&v.ty, inits),
                        Expr::AddrOf { e, .. } => {
                            // P1-6: 全局变量初始化中的符号引用 (&other_var)
                            if let Expr::Ident { name: ref_name, .. } = e.as_ref() {
                                let mut bytes = vec![0u8; 8];
                                // ET_EXEC: 直接解析符号地址
                                if let Some(ref g) = self.globals.get(&ref_name.name) {
                                    let vaddr = self.entry + g.offset;
                                    bytes = vaddr.to_le_bytes().to_vec();
                                }
                                Some(bytes)
                            } else { None }
                        }
                        _ => None,
                    }
                } else { None };
                // 直接写入初始化数据 (或零填充)
                if let Some(ref data) = init_data {
                    self.rodata.extend_from_slice(data);
                    for _ in data.len()..sz { self.rodata.push(0); }
                } else {
                    for _ in 0..sz { self.rodata.push(0); }
                }
                self.globals.insert(v.name.name.clone(), GlobalVar { offset, size: sz, ty: v.ty.clone(), init: init_data });
            }
        }
        let _rodata_size = self.rodata.len();
        let _main = prog.decls.iter().find_map(|d| match d { Decl::Func(f) if f.name.name == "main" => Some(f), _ => None }).expect("no main()");
        self.label("_start");
        self.emit(&[0x49, 0x89, 0xe4]); // mov r12, rsp (保存原始 rsp, 用于 leave 后恢复)
        self.emit(&[0x55]);             // push rbp
        self.emit(&[0x48, 0x89, 0xe5]); // mov rbp, rsp
        self.emit(&[0x48, 0x31, 0xc0]); // xor rax, rax
        self.call_label("main");
        self.emit(&[0x4c, 0x89, 0xe4]); // mov rsp, r12 (恢复原始 rsp)
        self.emit(&[0x89, 0xc7]);       // mov edi, eax
        self.emit(&[0xb8, 0x3c, 0x00, 0x00, 0x00]); // mov eax, 0x3c
        self.emit(&[0x0f, 0x05]);       // syscall
        self.emit(&[0xeb, 0xfe]);       // jmp self
        self.emit_functions(prog);
        // 对齐 code 到 8 字节 (ELF 的 .rodata 也按 8 对齐)
        while self.code.len() % 8 != 0 { self.code.push(0); }
        let rodata_start = self.entry + self.code.len() as u64;
        self.code.extend_from_slice(&self.rodata);
        self.resolve(rodata_start);
    }

    /// ET_REL 模式: 生成可重定位 .o 文件
    fn compile_rel(&mut self, prog: &Program) {
        // Phase 1: 生成代码, 收集数据
        // 收集全局变量 → .data / .bss (或自定义 section)
        for decl in &prog.decls {
            if let Decl::Var(v) = decl {
                let sz = v.ty.size().max(1);
                let align = v.alignment.unwrap_or(v.ty.align()).max(1);
                // 自定义 section 变量: 路由到 extra_sections
                if Self::is_custom_section_var(v) {
                    let sec_name = v.section.as_ref().unwrap().clone();
                    let is_writable = !matches!(&v.init, Some(Expr::String { .. }));
                    let sec_idx = self.ensure_custom_section(&sec_name, false, is_writable);
                    let sec = &mut self.extra_sections[sec_idx];
                    let off = (sec.data.len() + align - 1) & !(align - 1);
                    while sec.data.len() < off { sec.data.push(0); }
                    let offset = sec.data.len() as u64;
                    let mut has_init = false;
                    if let Some(init) = &v.init {
                        match init {
                            Expr::Int { v: iv, .. } => {
                                let bytes = iv.to_le_bytes()[..sz].to_vec();
                                self.extra_sections[sec_idx].data.extend_from_slice(&bytes);
                                has_init = true;
                            }
                            Expr::String { data, .. } => {
                                self.extra_sections[sec_idx].data.extend_from_slice(data);
                                for _ in data.len()..sz { self.extra_sections[sec_idx].data.push(0); }
                                has_init = true;
                            }
                            Expr::InitList { inits, .. } => {
                                if let Some(init_data) = self.gen_global_init_list(&v.ty, inits) {
                                    self.extra_sections[sec_idx].data.extend_from_slice(&init_data);
                                    for _ in init_data.len()..sz { self.extra_sections[sec_idx].data.push(0); }
                                    has_init = true;
                                }
                            }
                            Expr::AddrOf { e, .. } => {
                                // P1-6: 自定义 section 中的符号引用
                                if let Expr::Ident { name: ref_name, .. } = e.as_ref() {
                                    let ref_off = self.extra_sections[sec_idx].data.len() as u64;
                                    for _ in 0..8 { self.extra_sections[sec_idx].data.push(0); }
                                    self.pending_data64_rels.push((ref_off, ref_name.name.clone()));
                                    has_init = true;
                                }
                            }
                            _ => {}
                        }
                    }
                    if !has_init {
                        for _ in 0..sz { self.extra_sections[sec_idx].data.push(0); }
                    }
                    self.globals.insert(v.name.name.clone(), GlobalVar { offset, size: sz, ty: v.ty.clone(), init: None });
                    continue;
                }
                let off = (self.data_section.len() + align - 1) & !(align - 1);
                while self.data_section.len() < off { self.data_section.push(0); }
                let offset = self.data_section.len() as u64;
                let has_init = if let Some(init) = &v.init {
                    match init {
                        Expr::Int { v: iv, .. } => {
                            let bytes = iv.to_le_bytes()[..sz].to_vec();
                            self.data_section.extend_from_slice(&bytes);
                            true
                        }
                        Expr::String { data, .. } => {
                            self.data_section.extend_from_slice(data);
                            for _ in data.len()..sz { self.data_section.push(0); }
                            true
                        }
                        Expr::InitList { inits, .. } => {
                            if let Some(init_data) = self.gen_global_init_list(&v.ty, inits) {
                                self.data_section.extend_from_slice(&init_data);
                                for _ in init_data.len()..sz { self.data_section.push(0); }
                                true
                            } else { false }
                        }
                        Expr::AddrOf { e, .. } => {
                            // P1-6: .data 节中的符号引用
                            if let Expr::Ident { name: ref_name, .. } = e.as_ref() {
                                let ref_off = self.data_section.len() as u64;
                                self.data_section.extend_from_slice(&[0u8; 8]);
                                self.pending_data64_rels.push((ref_off, ref_name.name.clone()));
                                true
                            } else { false }
                        }
                        _ => false,
                    }
                } else { false };
                if !has_init {
                    for _ in 0..sz { self.data_section.push(0); }
                    self.bss_size += sz as u64;
                }
                self.globals.insert(v.name.name.clone(), GlobalVar { offset, size: sz, ty: v.ty.clone(), init: None });
            }
        }

        // 生成函数代码 (无 _start)
        self.emit_functions(prog);

        // 后处理: 将自定义 section 函数的代码从 .text 提取到 extra_sections
        // 记录函数在自定义 section 内的偏移 (用于符号表)
        let mut custom_func_offsets: HashMap<String, (u16, u64)> = HashMap::new(); // func_name → (section_idx+5, offset_in_section)
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if f.body.is_none() { continue; }
                if let Some(sec_name) = &f.section {
                    if sec_name.is_empty() { continue; }
                    if let Some(&func_start) = self.labels.get(&f.name.name) {
                        let func_size = self.func_sizes.get(&f.name.name).copied().unwrap_or(0);
                        if func_size == 0 { continue; }
                        let sec_idx = self.ensure_custom_section(sec_name, true, false);
                        let sec_off = self.extra_sections[sec_idx].data.len() as u64;
                        // 从 .text 拷贝代码到自定义节
                        let code_slice = self.code[func_start as usize..(func_start + func_size) as usize].to_vec();
                        self.extra_sections[sec_idx].data.extend_from_slice(&code_slice);
                        // 在 .text 中填零 (保持其他函数偏移不变)
                        for i in func_start as usize..(func_start + func_size) as usize {
                            self.code[i] = 0;
                        }
                        custom_func_offsets.insert(f.name.name.clone(), ((sec_idx + 5) as u16, sec_off));
                    }
                }
            }
        }

        // 处理字符串引用 → 确保所有字符串在 rodata 中
        let string_data_list: Vec<Vec<u8>> = self.string_refs.iter().map(|(_, d)| d.clone()).collect();
        for data in &string_data_list {
            if !self.string_pool.contains_key(data) {
                let off = self.rodata.len() as u64;
                self.rodata.extend_from_slice(data);
                self.string_pool.insert(data.clone(), off);
            }
        }

        // Phase 2: 收集所有符号 (正确顺序: STN_UNDEF → LOCAL → GLOBAL)
        // 先收集到临时 vec, 然后排序构建最终 symtab
        let mut local_syms: Vec<ElfSym> = vec![];
        let mut global_syms: Vec<ElfSym> = vec![];

        // 全局变量符号
        for decl in &prog.decls {
            if let Decl::Var(v) = decl {
                let g = self.globals[&v.name.name].clone();
                // 确定 section 索引: 自定义 section 或默认 .data(3)
                let sec_idx = if Self::is_custom_section_var(v) {
                    self.custom_sec_indices.get(v.section.as_ref().unwrap()).copied().unwrap_or(3) + 5
                } else { 3 };
                let sym = ElfSym {
                    name: v.name.name.clone(), section: sec_idx as u16, value: g.offset,
                    size: g.size as u64,
                    bind: if v.is_weak { STB_WEAK } else if v.is_static { STB_LOCAL } else { STB_GLOBAL },
                    ty: STT_OBJECT,
                    visibility: Self::parse_visibility(&v.visibility),
                };
                if v.is_static { local_syms.push(sym); } else { global_syms.push(sym); }
            }
        }

        // extern 函数声明 (无函数体 = 外部引用)
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if f.body.is_none() {
                    global_syms.push(ElfSym {
                        name: f.name.name.clone(), section: SHN_UNDEF, value: 0, size: 0,
                        bind: STB_GLOBAL, ty: STT_FUNC, visibility: STV_DEFAULT,
                    });
                }
            }
        }

        // 函数定义符号 (包括嵌套函数)
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if f.body.is_some() {
                    // 收集此函数的嵌套函数 (递归, 不含自身)
                    let mut nested = vec![];
                    if let Some(body) = &f.body {
                        Self::collect_nested_funcs(&body.items, &mut nested);
                    }
                    // 先处理顶层函数自身
                    if let Some(&func_start) = self.labels.get(&f.name.name) {
                        let func_size = self.func_sizes.get(&f.name.name).copied().unwrap_or(0);
                        let (sec_idx, sym_value) = if let Some(&(si, off)) = custom_func_offsets.get(&f.name.name) {
                            (si, off)
                        } else {
                            (1u16, func_start)
                        };
                        let sym = ElfSym {
                            name: f.name.name.clone(), section: sec_idx as u16, value: sym_value,
                            size: func_size,
                            bind: if f.is_weak { STB_WEAK } else if f.is_static { STB_LOCAL } else { STB_GLOBAL },
                            ty: STT_FUNC,
                            visibility: Self::parse_visibility(&f.visibility),
                        };
                        if f.is_static { local_syms.push(sym); } else { global_syms.push(sym); }
                    }
                    // 再处理嵌套函数
                    for func in &nested {
                        if let Some(&func_start) = self.labels.get(&func.name.name) {
                            let func_size = self.func_sizes.get(&func.name.name).copied().unwrap_or(0);
                            let (sec_idx, sym_value) = if let Some(&(si, off)) = custom_func_offsets.get(&func.name.name) {
                                (si, off)
                            } else {
                                (1u16, func_start)
                            };
                            let sym = ElfSym {
                                name: func.name.name.clone(), section: sec_idx as u16, value: sym_value,
                                size: func_size,
                                bind: if func.is_weak { STB_WEAK } else if func.is_static { STB_LOCAL } else { STB_GLOBAL },
                                ty: STT_FUNC,
                                visibility: Self::parse_visibility(&func.visibility),
                            };
                            if func.is_static { local_syms.push(sym); } else { global_syms.push(sym); }
                        }
                    }
                }
            }
        }

        // .rodata section 符号 (LOCAL)
        if !self.rodata.is_empty() {
            local_syms.push(ElfSym {
                name: ".rodata".to_string(), section: 2, value: 0,
                size: self.rodata.len() as u64,
                bind: STB_LOCAL, ty: STT_SECTION, visibility: STV_DEFAULT,
            });
        }

        // 构建最终符号表: STN_UNDEF + locals + globals
        self.symbols.clear();
        self.sym_index.clear();
        // [0] STN_UNDEF
        self.symbols.push(ElfSym {
            name: String::new(), section: SHN_UNDEF, value: 0, size: 0,
            bind: STB_LOCAL, ty: STT_NOTYPE, visibility: STV_DEFAULT,
        });
        for sym in &local_syms {
            let idx = self.symbols.len() as u32;
            self.symbols.push(sym.clone());
            self.sym_index.insert(sym.name.clone(), idx);
        }
        for sym in &global_syms {
            let idx = self.symbols.len() as u32;
            self.symbols.push(sym.clone());
            self.sym_index.insert(sym.name.clone(), idx);
        }

        // Phase 3: 构建重定位 (使用正确的符号索引)
        // 查找 .rodata section 符号索引
        let rodata_sym_idx = self.sym_index.get(".rodata").copied();

        for (code_pos, data) in self.string_refs.drain(..) {
            let str_off = self.string_pool[&data] as i64;
            self.relocations.push(ElfRela {
                offset: code_pos + 3,
                sym_index: rodata_sym_idx.unwrap_or(0),
                rel_type: R_X86_64_PC32,
                addend: str_off - 7,
            });
        }

        // 浮点字面量重定位
        for (code_pos, rodata_off) in self.float_refs.drain(..) {
            self.relocations.push(ElfRela {
                offset: code_pos + 4,
                sym_index: rodata_sym_idx.unwrap_or(0),
                rel_type: R_X86_64_PC32,
                addend: rodata_off as i64 - 8,
            });
        }

        // 解析函数调用重定位 (使用最终符号表, 用 PLT32 以兼容 PIE)
        for (code_off, func_name) in self.pending_func_rels.drain(..) {
            let sym_idx = self.sym_index.get(&func_name).copied().unwrap_or(0);
            self.relocations.push(ElfRela {
                offset: code_off,
                sym_index: sym_idx,
                rel_type: R_X86_64_PLT32,
                addend: -4,
            });
        }

        // 解析全局变量重定位 (使用最终符号表)
        for (disp32_pos, var_name) in self.pending_data_rels.drain(..) {
            let sym_idx = self.sym_index.get(&var_name).copied().unwrap_or(0);
            self.relocations.push(ElfRela {
                offset: disp32_pos,
                sym_index: sym_idx,
                rel_type: R_X86_64_PC32,
                addend: -4,
            });
        }

        // P1-6: 解析 data 节 64 位符号引用重定位
        for (data_off, var_name) in self.pending_data64_rels.drain(..) {
            let sym_idx = self.sym_index.get(&var_name).copied().unwrap_or(0);
            self.data_relocations.push(ElfRela {
                offset: data_off,
                sym_index: sym_idx,
                rel_type: R_X86_64_64,
                addend: 0,
            });
        }

        // 解析内部跳转 (本地标签)
        self.resolve(0);
    }

    /// 生成所有函数代码 (单 pass, 所有函数进入 .text)
    fn emit_functions(&mut self, prog: &Program) {
        // 收集顶层函数 (owned, 避免借用冲突)
        let top_funcs: Vec<FuncDecl> = prog.decls.iter().filter_map(|d| {
            if let Decl::Func(f) = d { if f.body.is_some() { Some(f.clone()) } else { None } } else { None }
        }).collect();
        for f in &top_funcs {
            self.compile_func(f, prog);
        }
    }

    /// 递归收集块中的嵌套函数声明
    fn collect_nested_funcs(items: &[BlockItem], out: &mut Vec<FuncDecl>) {
        for item in items {
            if let BlockItem::Decl(Decl::Func(f)) = item {
                out.push(f.clone());
                if let Some(body) = &f.body {
                    Self::collect_nested_funcs(&body.items, out);
                }
            }
            // 也扫描嵌套块 (if/for/while 等)
            if let BlockItem::Stmt(s) = item {
                Self::collect_nested_funcs_stmt(s, out);
            }
        }
    }
    fn collect_nested_funcs_stmt(s: &Stmt, out: &mut Vec<FuncDecl>) {
        match s {
            Stmt::Block { b, .. } => Self::collect_nested_funcs(&b.items, out),
            Stmt::If { then_, else_, .. } => {
                if let Stmt::Block { b, .. } = then_.as_ref() { Self::collect_nested_funcs(&b.items, out); }
                if let Some(e) = else_ { if let Stmt::Block { b, .. } = e.as_ref() { Self::collect_nested_funcs(&b.items, out); } }
            }
            Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
                if let Stmt::Block { b, .. } = body.as_ref() { Self::collect_nested_funcs(&b.items, out); }
            }
            _ => {}
        }
    }

    /// 编译单个函数 (支持嵌套函数递归调用)
    fn compile_func(&mut self, f: &FuncDecl, prog: &Program) {
        // 先编译嵌套函数 (在父函数标签之前)
        if let Some(body) = &f.body {
            let nested: Vec<FuncDecl> = {
                let mut nf = Vec::new();
                Self::collect_nested_funcs(&body.items, &mut nf);
                nf
            };
            for nf_decl in &nested {
                self.compile_func(nf_decl, prog);
            }
        }
        let arg_regs: [u8; 6] = [7, 6, 2, 1, 8, 9];
        self.var_map.clear();
        self.vla_offsets.clear();
        self.reg_bindings.clear();
        self.cleanup_vars.clear();
        self.vla_total_size = 0;
        // 设置嵌套函数状态
        self.current_func_is_nested = f.is_nested;
        self.upvar_offsets.clear();
        if f.is_nested {
            // 记录 upvar 的父帧偏移 (占位符, 实际值在父函数编译时已知)
            for (name, _off) in &f.upvars {
                self.upvar_offsets.insert(name.clone(), 0); // 偏移将在代码生成时解析
            }
        }
        let func_start = self.pos();
        self.label(&f.name.name);
        self.emit(&[0x41, 0x55, 0x49, 0x89, 0xe5]);
        self.expr_depth = 0; self.max_expr_depth = 0; self.temp_slots = 0;
        if let Some(body) = &f.body { self.max_expr_depth = Self::max_expr_depth_stmt(&body.items); }
        let temp_slots = (self.max_expr_depth as i32 + 1).max(2);
        self.temp_slots = temp_slots;
        if f.name.name != "main" || !self.relocatable {
                    // 先预注册函数体中的局部变量 (使参数偏移自然在局部变量下方)
                    if let Some(body) = &f.body { Self::pre_register_vars(&mut self.var_map, &body.items, temp_slots); }
                    // 注册参数到 var_map (偏移公式自动避开已注册的局部变量)
                    // 记录命名参数数量 (va_start 需要)
                    self.variadic_named_params = f.params.iter().filter(|p| p.name.is_some()).count() as i32;
                    let mut param_stores: Vec<(u8, i32)> = Vec::new(); // (reg, offset)
                    let mut arg_idx = 0usize;
                    for p in f.params.iter() {
                        if let Some(nm) = &p.name {
                            let is_agg_param = matches!(&p.ty, Type::Struct(_,_) | Type::Union(_,_) | Type::Array(_,ArraySize::Const(_))
                                | Type::Atomic(_))
                                && p.ty.size() > 8;
                            if is_agg_param {
                                let chunks = (p.ty.size() + 7) / 8;
                                let used: i32 = self.var_map.values()
                                    .filter(|(_, ty)| !matches!(ty, Type::Void))
                                    .map(|(_, ty)| ((ty.size() + 7) / 8) as i32 * 8)
                                    .sum();
                                let off = -(used + 16);
                                self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                                for c in 0..chunks as usize {
                                    if arg_idx + c < 6 {
                                        param_stores.push((arg_regs[arg_idx + c], off + (c as i32 * 8)));
                                    }
                                }
                                arg_idx += chunks as usize;
                            } else if arg_idx < 6 {
                                let used: i32 = self.var_map.values()
                                    .filter(|(_, ty)| !matches!(ty, Type::Void))
                                    .map(|(_, ty)| ((ty.size() + 7) / 8) as i32 * 8)
                                    .sum();
                                let off = -(used + 16);
                                self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                                param_stores.push((arg_regs[arg_idx], off));
                                arg_idx += 1;
                            }
                        }
                    }
                // frame_size = 变量实际字节 + 16(保存寄存器) + temp_slots*8
                let var_bytes: i32 = self.var_map.values()
                    .filter(|(_, ty)| !matches!(ty, Type::Void))
                    .map(|(_, ty)| ((ty.size() + 7) / 8) as i32 * 8)
                    .sum();
                let sc_slot = if f.is_nested { 8 } else { 0 }; // 嵌套函数需要静态链槽位
                let raw = var_bytes + 16 + temp_slots * 8 + sc_slot;
                self.frame_size = ((raw + 15) / 16) * 16; // 16 字节对齐
                if self.frame_size > 0 {
                    self.emit(&[0x48, 0x81, 0xec]);
                    self.emit(&(self.frame_size as i32).to_le_bytes());
                }
                self.emit(&[0x4d, 0x8d, 0xa5]);
                self.emit(&(-(self.frame_size) as i32).to_le_bytes());
                // 嵌套函数: 保存静态链 (r10) 到帧底部
                if f.is_nested {
                    self.static_chain_offset = -(self.frame_size as i32) + 8;
                    // mov [r13 + static_chain_offset], r10
                    self.emit(&[0x49, 0x89, 0x95]);
                    self.emit(&self.static_chain_offset.to_le_bytes());
                }
                // Phase 2: 帧已建立 (r13 已设置), 现在存储参数到栈槽
                for (reg, off) in param_stores {
                    self.store_stack(reg, off);
                }
                } else {
                if let Some(body) = &f.body { Self::pre_register_vars(&mut self.var_map, &body.items, temp_slots); }
                let var_bytes: i32 = self.var_map.values()
                    .filter(|(_, ty)| !matches!(ty, Type::Void))
                    .map(|(_, ty)| ((ty.size() + 7) / 8) as i32 * 8)
                    .sum();
                let raw = var_bytes + 16 + temp_slots * 8;
                self.frame_size = ((raw + 15) / 16) * 16;
                if self.frame_size > 0 {
                    self.emit(&[0x48, 0x81, 0xec]);
                    self.emit(&(self.frame_size as i32).to_le_bytes());
                }
                self.emit(&[0x4d, 0x8d, 0xa5]);
                self.emit(&(-(self.frame_size) as i32).to_le_bytes());
                }
                let ends_ret = if let Some(body) = &f.body {
                    for item in &body.items { match item {
                        BlockItem::Stmt(s) => self.gen_stmt(s),
                        BlockItem::Decl(Decl::Var(v)) => {
                            self.register_var(&v.name.name, &v.ty);
                            if matches!(&v.ty, Type::Array(_, ArraySize::Var(_))) {
                                self.gen_vla_alloc(v);
                            } else if let Some(init) = &v.init {
                                if let Expr::InitList { inits, .. } = init {
                                    self.gen_init_list(&v.name.name, &v.ty, inits);
                                } else {
                                    let is_vec_init = matches!(&v.ty, Type::Vector(_, _));
                                    let is_agg = matches!(&v.ty, Type::Struct(_,_) | Type::Union(_,_) | Type::Array(_,ArraySize::Const(_))
                                        | Type::Atomic(_))
                                        && v.ty.size() > 8;
                                    if is_vec_init {
                                        self.gen_expr(init, 0);
                                        self.gen_vector_store(&v.name.name);
                                    } else if is_agg {
                                        // struct/array 初始化: 整体拷贝
                                        let dst_ty = v.ty.clone();
                                        let dst_name = v.name.name.clone();
                                        self.gen_aggregate_assign(&dst_name, &dst_ty, init);
                                    } else if let Type::Complex(inner) = &v.ty {
                                        self.gen_complex_init(&v.name.name, inner, init);
                                    } else if matches!(&v.ty, Type::Float | Type::Double | Type::LongDouble) {
                                        self.gen_expr(init, 0);
                                        self.gen_float_store(&v.name.name, &v.ty);
                                    } else {
                                        self.gen_expr(init, 0);
                                        self.mov_to_stack(0, &v.name.name);
                                    }
                                }
                            }
                        }
                        _ => {}
                    }}
                    Self::ends_with_return(&body.items)
                } else { false };
                if !ends_ret { self.emit_func_epilogue(); }
                // 记录函数大小
                let func_end = self.pos();
                self.func_sizes.insert(f.name.name.clone(), func_end - func_start);
    }

    fn call_label(&mut self, l: &str) {
        let at = self.pos(); self.emit(&[0xe8, 0, 0, 0, 0]);
        if self.relocatable {
            // 延迟到 Phase 3 符号表构建后再解析
            self.pending_func_rels.push((at + 1, l.to_string()));
        } else {
            self.pending.push(PendingJump { at, target: l.into(), kind: JumpKind::Jmp });
        }
    }

    // 生成表达式值, 数组标识符衰减为指针 (arr → &arr[0])
    fn gen_expr_decay(&mut self, e: &Expr, dst: u8) {
        if let Expr::Ident { name, ty, .. } = e {
            if matches!(ty, Type::Array(_, _)) {
                if self.globals.contains_key(&name.name) {
                    // 全局数组: RIP-relative LEA 取地址
                    self.emit_global_addr(dst, &name.name);
                } else {
                    let off = self.var_offset(&name.name);
                    let rex = 0x49u8; // REX.W + REX.B (r13)
                    self.emit(&[rex, 0x8D]);
                    if off.abs() < 128 {
                        self.emit(&[0x45 | ((dst & 7) << 3), off as i8 as u8]);
                    } else {
                        self.emit(&[0x85 | ((dst & 7) << 3)]);
                        self.emit(&off.to_le_bytes());
                    }
                }
                return;
            }
        }
        self.gen_expr(e, dst);
    }

    fn gen_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Return { val, .. } => {
                if let Some(e) = val {
                    let ret_ty = e.ty();
                    let ret_sz = ret_ty.size();
                    let is_agg_ret = matches!(&ret_ty, Type::Struct(_,_) | Type::Union(_,_) | Type::Array(_,ArraySize::Const(_))
                        | Type::Complex(_) | Type::Atomic(_))
                        && ret_sz > 8;
                    if is_agg_ret {
                        // struct 返回: 加载到 rax[:rdx]
                        if let Expr::Ident { name, .. } = &e {
                            let off = self.var_offset(&name.name);
                            self.load_stack(0, off); // rax = 前 8 字节
                            if ret_sz > 8 {
                                self.load_stack(2, off + 8); // rdx = 后 8 字节
                            }
                        } else {
                            self.gen_expr(e, 0);
                        }
                    } else {
                        self.gen_expr(e, 0);
                    }
                } else { self.emit(&[0x31, 0xc0]); }
                self.emit_func_epilogue();
            }
            Stmt::Expr { e, .. } => { self.gen_expr(e, 0); }
            Stmt::If { cond, then_, else_, .. } => {
                self.gen_expr(cond, 0); self.emit(&[0x85, 0xc0]);
                let le = self.fresh_label(); let lend = self.fresh_label();
                self.jmp_rel32(&le, JumpKind::Je);
                self.gen_stmt(then_); self.jmp_rel32(&lend, JumpKind::Jmp);
                self.label(&le); if let Some(el) = else_ { self.gen_stmt(el); }
                self.label(&lend);
            }
            Stmt::While { cond, body, .. } => {
                let ls = self.fresh_label(); let le = self.fresh_label();
                let (sc, sb) = (self.loop_continue.clone(), self.loop_break.clone());
                self.loop_continue = Some(ls.clone()); self.loop_break = Some(le.clone());
                self.label(&ls); self.gen_expr(cond, 0); self.emit(&[0x85, 0xc0]);
                self.jmp_rel32(&le, JumpKind::Je); self.gen_stmt(body);
                self.jmp_rel32(&ls, JumpKind::Jmp); self.label(&le);
                self.loop_continue = sc; self.loop_break = sb;
            }
            Stmt::For { init, cond, step, body, .. } => {
                if let Some(i) = init { self.gen_stmt(i); }
                let ls = self.fresh_label(); let le = self.fresh_label(); let lst = self.fresh_label();
                let (sc, sb) = (self.loop_continue.clone(), self.loop_break.clone());
                self.loop_continue = Some(lst.clone()); self.loop_break = Some(le.clone());
                self.label(&ls);
                if let Some(c) = cond { self.gen_expr(c, 0); self.emit(&[0x85, 0xc0]); self.jmp_rel32(&le, JumpKind::Je); }
                self.gen_stmt(body); self.label(&lst);
                if let Some(s) = step { self.gen_expr(s, 0); }
                self.jmp_rel32(&ls, JumpKind::Jmp); self.label(&le);
                self.loop_continue = sc; self.loop_break = sb;
            }
            Stmt::Do { body, cond, .. } => {
                let ls = self.fresh_label(); let le = self.fresh_label();
                let (sc, sb) = (self.loop_continue.clone(), self.loop_break.clone());
                self.loop_continue = Some(ls.clone()); self.loop_break = Some(le.clone());
                self.label(&ls); self.gen_stmt(body);
                self.gen_expr(cond, 0); self.emit(&[0x85, 0xc0]);
                self.jmp_rel32(&ls, JumpKind::Jne); self.label(&le);
                self.loop_continue = sc; self.loop_break = sb;
            }
            Stmt::Block { b, .. } => {
                let cleanup_base = self.cleanup_vars.len();
                for it in &b.items { match it {
                    BlockItem::Stmt(s) => self.gen_stmt(s),
                    BlockItem::Decl(Decl::Var(v)) => {
                        // P1-5: register asm 变量绑定到指定寄存器
                        if let Some(ref reg_name) = v.register_name {
                            if let Some(reg_idx) = Self::parse_reg_name(reg_name) {
                                self.reg_bindings.insert(v.name.name.clone(), reg_idx);
                            }
                        }
                        // P2-9: cleanup 属性变量注册
                        if let Some(ref fn_name) = v.cleanup_fn {
                            self.cleanup_vars.push((v.name.name.clone(), fn_name.clone()));
                        }
                        self.register_var(&v.name.name, &v.ty);
                        if matches!(&v.ty, Type::Array(_, ArraySize::Var(_))) {
                            self.gen_vla_alloc(v);
                        } else if let Some(init) = &v.init {
                            if let Expr::InitList { inits, .. } = init {
                                self.gen_init_list(&v.name.name, &v.ty, inits);
                            } else {
                                let is_agg = matches!(&v.ty, Type::Struct(_,_) | Type::Union(_,_) | Type::Array(_,ArraySize::Const(_))
                                    | Type::Atomic(_))
                                    && v.ty.size() > 8;
                                if matches!(&v.ty, Type::Vector(_, _)) {
                                    self.gen_expr(init, 0);
                                    self.gen_vector_store(&v.name.name);
                                } else if is_agg {
                                    let dst_ty = v.ty.clone();
                                    let dst_name = v.name.name.clone();
                                    self.gen_aggregate_assign(&dst_name, &dst_ty, init);
                                } else if let Type::Complex(inner) = &v.ty {
                                    self.gen_complex_init(&v.name.name, inner, init);
                                } else {
                                    self.gen_expr(init, 0);
                                    self.mov_to_stack(0, &v.name.name);
                                }
                            }
                        }
                    }
                    _ => {}
                }}
                // P2-9: 块结束时逆序调用 cleanup 函数
                while self.cleanup_vars.len() > cleanup_base {
                    if let Some((var_name, fn_name)) = self.cleanup_vars.pop() {
                        // 发射: lea rdi, [r13 + offset]; call fn_name
                        let off = self.var_offset(&var_name);
                        self.emit_lea_r13(7, off); // rdi = &var
                        self.call_label(&fn_name);
                    }
                }
            }
            Stmt::Break { .. } => { if let Some(l) = self.loop_break.clone() { self.jmp_rel32(&l, JumpKind::Jmp); } }
            Stmt::Continue { .. } => { if let Some(l) = self.loop_continue.clone() { self.jmp_rel32(&l, JumpKind::Jmp); } }
            Stmt::Goto { target, .. } => { self.jmp_rel32(&target.name, JumpKind::Jmp); }
            Stmt::Label { name, stmt, .. } => { self.label(&name.name); self.gen_stmt(stmt); }
            Stmt::Switch { expr, cases, .. } => {
                // switch: 计算表达式, 依次比较每个case, 跳到匹配处
                let l_end = self.fresh_label();
                let l_default = self.fresh_label();
                let mut has_default = false;
                // 计算switch表达式到rax
                self.gen_expr(expr, 0);
                self.push(0); // 保存switch值
                // 生成每个case的比较和跳转
                for case in cases {
                    if let Some(ref val) = case.val {
                        // case N: 比较 rax == N
                        let l_case = self.fresh_label();
                        // 重新加载switch值到rcx
                        self.emit(&[0x48, 0x8B, 0x0C, 0x24]); // mov rcx, [rsp]
                        // 加载case值到rdx
                        self.gen_expr(val, 2);
                        // cmp rcx, rdx
                        self.emit(&[0x48, 0x39, 0xD1]);
                        // je l_case
                        self.jmp_rel32(&l_case, JumpKind::Je);
                        // 生成case体
                        self.label(&l_case);
                        for item in &case.body.items {
                            if let BlockItem::Stmt(stmt) = item {
                                self.gen_stmt(stmt);
                            }
                        }
                    } else {
                        // default: 记录标签
                        has_default = true;
                    }
                }
                // 如果没有匹配, 跳到default或end
                if has_default {
                    self.jmp_rel32(&l_default, JumpKind::Jmp);
                } else {
                    self.jmp_rel32(&l_end, JumpKind::Jmp);
                }
                // 生成default体
                if has_default {
                    self.label(&l_default);
                    for case in cases {
                        if case.val.is_none() {
                            for item in &case.body.items {
                                if let BlockItem::Stmt(stmt) = item {
                                    self.gen_stmt(stmt);
                                }
                            }
                        }
                    }
                }
                self.label(&l_end);
                self.emit(&[0x48, 0x83, 0xC4, 0x08]); // add rsp, 8 (清理switch值)
            }
            Stmt::GotoIndirect { expr, .. } => {
                // goto *expr: 计算目标地址并跳转
                self.gen_expr(expr, 0); // rax = 目标地址
                self.emit(&[0xFF, 0xE0]); // jmp rax
            }
            Stmt::AsmGoto { asm_str, outputs, inputs, clobbers, goto_labels, .. } => {
                // asm goto: 实际发射汇编指令
                let op_map = self.build_asm_operand_map(outputs, inputs);
                self.emit_inline_asm(asm_str, &op_map);
                // asm goto 标签已在 emit_inline_asm 中处理
                let _ = (clobbers, goto_labels);
            }
        }
    }

    fn gen_expr(&mut self, e: &Expr, dst: u8) {
        match e {
            Expr::Int { v, .. } => self.mov_imm(dst, *v),
            Expr::Char { v, .. } => self.mov_imm(dst, *v as i128),
            Expr::Ident { name, ty, .. } => {
                if matches!(ty, Type::Vector(_, _)) {
                    // 向量变量: 用 movups 加载到 xmm0
                    self.gen_vector_load(e);
                } else if self.globals.contains_key(&name.name) {
                    self.emit_global_load(dst, &name.name);
                } else if matches!(ty, Type::Float | Type::Double | Type::LongDouble) {
                    self.gen_float_load(&name.name, ty);
                } else if self.current_func_is_nested && self.upvar_offsets.contains_key(&name.name) {
                    // upvar 访问: 通过静态链加载
                    // 1. 加载静态链到 dst: mov dst, [r13 + static_chain_offset]
                    let sc_off = self.static_chain_offset;
                    self.emit(&[0x49, 0x8b]); // mov dst, [r13 + off]
                    if dst >= 8 { self.emit(&[0x84 | ((dst & 7) << 3)]); }
                    else { self.emit(&[0x45 | ((dst & 7) << 3)]); }
                    self.emit(&sc_off.to_le_bytes());
                    // 2. 从父帧加载 upvar: mov dst, [dst + parent_offset]
                    // 注意: upvar_offsets 中存储的是占位符 0, 实际偏移需要从父函数获取
                    // 简化实现: 假设 upvar 偏移为 0 (需要更复杂的偏移解析)
                    let parent_off = self.upvar_offsets.get(&name.name).copied().unwrap_or(0);
                    if parent_off != 0 {
                        let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                        self.emit(&[rex, 0x8b]);
                        if dst >= 8 { self.emit(&[0x84 | ((dst & 7) << 3)]); }
                        else { self.emit(&[0x44 | ((dst & 7) << 3)]); }
                        self.emit(&parent_off.to_le_bytes());
                    } else {
                        // 偏移为 0: 直接解引用
                        let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                        self.emit(&[rex, 0x8b, 0x00 | ((dst & 7) << 3) | (dst & 7)]);
                    }
                } else {
                    // P1-4: __int128 变量加载 (两个 64 位槽)
                    if matches!(ty, Type::Int128 | Type::UInt128) {
                        let off = self.var_offset(&name.name);
                        self.load_stack(dst, off);       // low 64 bits
                        self.load_stack(2, off + 8);     // high 64 bits → rdx
                        if dst != 0 { /* rax already set */ }
                    } else {
                        self.mov_from_stack(dst, &name.name);
                    }
                }
                // 整数提升: char/short → int
                if ty.is_int() { self.gen_int_promote(dst, ty); }
            }
            Expr::Binop { op, l, r, .. } => {
                // 浮点运算: 使用 SSE 指令
                if matches!(l.ty(), Type::Float | Type::Double | Type::LongDouble) {
                    self.gen_float_binop(*op, l, r);
                    return;
                }
                // SIMD 向量运算: 使用 SSE 指令
                if matches!(l.ty(), Type::Vector(_, _)) {
                    self.gen_vector_binop(*op, l, r);
                    return;
                }
                // P1-4: __int128 运算
                if matches!(l.ty(), Type::Int128 | Type::UInt128) {
                    self.gen_int128_binop(*op, l, r, dst);
                    return;
                }
                // P2-14: _Complex 算术运算
                if matches!(l.ty(), Type::Complex(_)) {
                    self.gen_complex_binop(*op, l, r, dst);
                    return;
                }
                // 逻辑与/或: 短路求值
                if matches!(op, BinOp::AndAnd | BinOp::OrOr) {
                    let l_false = self.fresh_label();
                    let l_end = self.fresh_label();
                    self.gen_expr_decay(l, dst);
                    // test dst, dst
                    let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                    self.emit(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                    match op {
                        BinOp::AndAnd => {
                            // 如果 dst == 0, 跳到 false (短路: 结果为 0)
                            self.jmp_rel32(&l_false, JumpKind::Je);
                            self.gen_expr_decay(r, dst);
                            // test dst, dst
                            self.emit(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                            // setne al (非零 → 1, 零 → 0)
                            self.emit(&[0x0F, 0x95, 0xC0]);
                            // movzx dst, al
                            let rex2 = 0x48 | if dst >= 8 { 0x41 } else { 0 };
                            self.emit(&[rex2, 0x0F, 0xB6, 0xC0 | (dst & 7)]);
                            self.jmp_rel32(&l_end, JumpKind::Jmp);
                            self.label(&l_false);
                            // dst = 0 (短路: 左操作数为 0, 结果为 0)
                            self.mov_imm(dst, 0);
                            self.label(&l_end);
                        }
                        BinOp::OrOr => {
                            // 如果 dst != 0, 跳到 true (短路: 结果为 1)
                            self.jmp_rel32(&l_false, JumpKind::Jne);
                            self.gen_expr_decay(r, dst);
                            // test dst, dst
                            self.emit(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                            // setne al (非零 → 1, 零 → 0)
                            self.emit(&[0x0F, 0x95, 0xC0]);
                            // movzx dst, al
                            let rex2 = 0x48 | if dst >= 8 { 0x41 } else { 0 };
                            self.emit(&[rex2, 0x0F, 0xB6, 0xC0 | (dst & 7)]);
                            self.jmp_rel32(&l_end, JumpKind::Jmp);
                            self.label(&l_false);
                            // dst = 1 (短路: 左操作数非 0, 结果为 1)
                            self.mov_imm(dst, 1);
                            self.label(&l_end);
                        }
                        _ => unreachable!(),
                    }
                    return;
                }
                // 指针/数组运算缩放: ptr ± n 步进 sizeof(*ptr), ptr - ptr 返回元素数
                // 数组衰减为指针: Array(T,_) 视为 Ptr(T)
                let l_inner = match l.ty() { Type::Ptr(ref t) | Type::Array(ref t, _) => Some(t.size()), _ => None };
                let r_inner = match r.ty() { Type::Ptr(ref t) | Type::Array(ref t, _) => Some(t.size()), _ => None };
                let is_ptr_sub = matches!(op, BinOp::Sub) && l_inner.is_some() && r_inner.is_some();
                let needs_scale = matches!(op, BinOp::Add | BinOp::Sub) && !is_ptr_sub;
                let scale = if needs_scale {
                    if let Some(s) = l_inner { s } else if let Some(s) = r_inner { s } else { 1 }
                } else { 1 };
                let elem_size = if is_ptr_sub { l_inner.unwrap_or(1) } else { 1 };
                // 统一路径: 使用帧临时槽保存左操作数 (避免 push 与 call 返回地址冲突)
                // 使用 gen_expr_decay: 数组标识符在运算中衰减为指针
                // binop 临时槽: 放在 temp_slots 区域 (保存寄存器下方, 变量上方)
                // 布局: r13 → saved_regs(-8,-16) → temp_slots → variables
                let var_bytes: i32 = self.var_map.values()
                    .filter(|(_, ty)| !matches!(ty, Type::Void))
                    .map(|(_, ty)| ((ty.size() + 7) / 8) as i32 * 8)
                    .sum();
                let slot_off = -(24 + var_bytes + self.expr_depth as i32 * 8);
                self.gen_expr_decay(l, dst);
                // 保存左操作数到帧临时槽: mov [r13+slot_off], dst
                {
                    let mut rex = 0x49u8; if dst >= 8 { rex |= 0x04; }
                    self.emit(&[rex, 0x89]);
                    if slot_off.abs() < 128 { self.emit(&[0x45 | ((dst & 7) << 3), slot_off as i8 as u8]); }
                    else { self.emit(&[0x85 | ((dst & 7) << 3)]); self.emit(&slot_off.to_le_bytes()); }
                }
                self.expr_depth += 1;
                self.gen_expr_decay(r, 1);
                self.expr_depth -= 1;
                // 对整数操作数乘以指针所指类型大小 (跳过 char* 和 ptr-ptr)
                if scale > 1 {
                    self.emit(&[0x48, 0x69, 0xc9]); // imul rcx, rcx, imm32
                    self.emit(&(scale as i32).to_le_bytes());
                }
                // 当 dst == 1 (rcx) 时, 恢复左操作数会覆盖右操作数
                // 解决: 先将右操作数 push 到栈, 恢复左操作数后 pop 到 rdx(2)
                if dst == 1 {
                    self.push(1); // push rcx (右操作数)
                    // 恢复左操作数到 dst
                    {
                        let mut rex = 0x49u8; if dst >= 8 { rex |= 0x04; }
                        self.emit(&[rex, 0x8B]);
                        if slot_off.abs() < 128 { self.emit(&[0x45 | ((dst & 7) << 3), slot_off as i8 as u8]); }
                        else { self.emit(&[0x85 | ((dst & 7) << 3)]); self.emit(&slot_off.to_le_bytes()); }
                    }
                    self.pop(2); // pop rdx (右操作数)
                    self.binop(*op, dst, 2);
                } else {
                    // 恢复左操作数: mov dst, [r13+slot_off]
                    {
                        let mut rex = 0x49u8; if dst >= 8 { rex |= 0x04; }
                        self.emit(&[rex, 0x8B]);
                        if slot_off.abs() < 128 { self.emit(&[0x45 | ((dst & 7) << 3), slot_off as i8 as u8]); }
                        else { self.emit(&[0x85 | ((dst & 7) << 3)]); self.emit(&slot_off.to_le_bytes()); }
                    }
                    self.binop(*op, dst, 1);
                }
                // ptr - ptr: 字节差 ÷ 元素大小 = 元素数
                if is_ptr_sub && elem_size > 1 {
                    self.mov_imm(1, elem_size as i128); // rcx = elem_size
                    self.emit(&[0x48, 0x99]); // cqo (符号扩展 rax→rdx:rax)
                    self.emit(&[0x48, 0xf7, 0xf9]); // idiv rcx
                }
            }
            Expr::Assign { l, r, .. } => {
                if let Expr::Ident { name, ty: l_ty, .. } = l.as_ref() {
                    let sz = l_ty.size();
                    let is_aggregate = matches!(l_ty, Type::Struct(_,_) | Type::Union(_,_) | Type::Array(_,ArraySize::Const(_))
                        | Type::Atomic(_))
                        && sz > 8;
                    if is_aggregate {
                        // struct/array 整体赋值: 逐块拷贝
                        self.gen_aggregate_assign(&name.name, l_ty, r);
                    } else if matches!(l_ty, Type::Complex(_)) {
                        if sz > 8 {
                            // Complex > 8B: use aggregate assign
                            self.gen_aggregate_assign(&name.name, l_ty, r);
                        } else {
                            self.gen_expr_decay(r, dst);
                            self.mov_to_stack(dst, &name.name);
                        }
                    } else {
                        self.gen_expr_decay(r, dst); // 数组衰减: int *p = arr
                        if self.globals.contains_key(&name.name) {
                            self.emit_global_store(dst, &name.name);
                        } else if matches!(l_ty, Type::Vector(_, _)) {
                            self.gen_vector_store(&name.name);
                        } else if matches!(l_ty, Type::Float | Type::Double | Type::LongDouble) {
                            self.gen_float_store(&name.name, l_ty);
                        } else if matches!(l_ty, Type::Int128 | Type::UInt128) {
                            // P1-4: __int128 赋值 (rax=low, rdx=high)
                            let off = self.var_offset(&name.name);
                            self.store_stack(0, off);      // low → [r13+off]
                            self.store_stack(2, off + 8);  // high → [r13+off+8]
                        } else {
                            self.mov_to_stack(dst, &name.name);
                        }
                    }
                } else if let Expr::Index { arr, idx, ty: elem_ty, .. } = l.as_ref() {
                    // arr[i] = val
                    let es = elem_ty.size() as i32;
                    if let Expr::Ident { name: arr_name, ty: arr_ty, .. } = arr.as_ref() {
                        if self.vla_offsets.contains_key(&arr_name.name) {
                            // VLA: 基址存储在变量栈槽中
                            let off = self.var_offset(&arr_name.name);
                            self.load_stack(0, off); // rax = VLA 基址
                        } else if matches!(arr_ty, Type::Array(_, _)) {
                            // 静态数组: LEA 取基址
                            if self.globals.contains_key(&arr_name.name) {
                                self.emit_global_addr(0, &arr_name.name);
                            } else {
                                let off = self.var_offset(&arr_name.name);
                                self.emit_lea_r13(0, off);
                            }
                        } else {
                            // 指针: 加载指针值 (但全局数组衰减需用 LEA)
                            if self.globals.get(&arr_name.name).map_or(false, |g| matches!(g.ty, Type::Array(_, _))) {
                                self.emit_global_addr(0, &arr_name.name);
                            } else if self.globals.contains_key(&arr_name.name) {
                                self.emit_global_load(0, &arr_name.name);
                            } else {
                                let off = self.var_offset(&arr_name.name);
                                self.load_stack(0, off);
                            }
                        }
                        self.push(0); // 保存基址 (gen_expr(idx) 可能破坏 rax)
                        self.gen_expr(idx, 1); // rcx = index
                        self.pop(0); // 恢复基址到 rax
                        if es != 1 {
                            self.emit(&[0x48, 0x69, 0xc9]); // imul rcx, rcx, imm32
                            self.emit(&es.to_le_bytes());
                        }
                        self.emit(&[0x48, 0x01, 0xc8]); // add rax, rcx (addr in rax)
                        self.push(0); // save addr
                        self.gen_expr(r, 0); // rax = val
                        self.pop(1); // rcx = addr
                        // mov [rcx], rax: 48 89 01
                        self.emit(&[0x48, 0x89, 0x01]);
                    }
                } else if let Expr::Deref { e: ptr_e, .. } = l.as_ref() {
                    // *ptr = val
                    self.gen_expr(ptr_e, 0); // rax = address
                    self.push(0);             // save addr
                    self.gen_expr(r, 0);      // rax = val
                    self.pop(1);              // rcx = addr
                    self.emit(&[0x48, 0x89, 0x01]); // mov [rcx], rax
                } else if let Expr::Member { expr, field, is_arrow, .. } = l.as_ref() {
                    // struct.field = val 或 ptr->field = val
                    // 1. 查找字段信息 (支持匿名 struct/union)
                    let field_info = if let Type::Struct(_, fields) | Type::Union(_, fields) = &expr.ty() {
                        find_field(fields, &field.name).map(|(f, extra)| {
                            let mut fc = f.clone();
                            fc.offset += extra;
                            fc
                        })
                    } else { None };
                    let is_bitfield = field_info.as_ref().map_or(false, |f| f.bit_width.is_some());
                    // 2. 计算目标地址
                    if *is_arrow {
                        self.gen_expr(expr, 0); // rax = 指针值
                    } else if let Expr::Ident { name, .. } = expr.as_ref() {
                        let off = self.var_offset(&name.name);
                        // LEA rax, [r13 + off]
                        self.emit(&[0x49, 0x8D]);
                        if off.abs() < 128 {
                            self.emit(&[0x45, off as i8 as u8]); // mod=01, rm=r13
                        } else {
                            self.emit(&[0x85]); // mod=00, rm=r13
                            self.emit(&off.to_le_bytes());
                        }
                    }
                    // 3. 加上字段偏移
                    if let Some(ref f) = field_info {
                        let field_off = f.offset as i32;
                        if field_off != 0 {
                            self.emit(&[0x48, 0x05]);
                            self.emit(&field_off.to_le_bytes());
                        }
                    }
                    if is_bitfield {
                        // 位域写入: 读-改-写 (使用帧内临时槽, 避免 push 与 call 返回地址冲突)
                        let f = field_info.as_ref().unwrap();
                        let bw = f.bit_width.unwrap();
                        let container_size = f.ty.size();
                        let bit_off = f.bit_offset;
                        // rax = 容器地址
                        // 保存地址到帧临时槽1: [r13-8]
                        self.emit(&[0x49, 0x89, 0x45, 0xf8]); // mov [r13-8], rax
                        // 加载当前容器值 (结果统一放入 rax)
                        match container_size {
                            4 => self.emit(&[0x8B, 0x00]), // mov eax, [rax] (零扩展)
                            8 => self.emit(&[0x48, 0x8B, 0x00]), // mov rax, [rax]
                            2 => { self.emit(&[0x66, 0x8B, 0x00]); // mov ax, [rax]
                                   self.emit(&[0x48, 0x0F, 0xB7, 0xC0]); } // movzx eax, ax
                            1 => self.emit(&[0x0F, 0xB6, 0x00]), // movzx eax, byte [rax]
                            _ => {}
                        }
                        // 保存容器值到帧临时槽2: [r13-16]
                        self.emit(&[0x49, 0x89, 0x45, 0xf0]); // mov [r13-16], rax
                        // 计算右值
                        self.gen_expr(r, 0); // rax = 新值
                        // 掩码: and rax, (1<<bw)-1
                        if bw < 64 {
                            let mask: u64 = (1u64 << bw) - 1;
                            self.emit(&[0x48, 0x25]); // and eax, imm32
                            self.emit(&(mask as u32).to_le_bytes());
                        }
                        // 左移到位置: shl rax, bit_off
                        if bit_off > 0 {
                            self.emit(&[0x48, 0xC1, 0xE0, bit_off as u8]); // shl rax, imm8
                        }
                        // rax = 移位后的新值
                        // 加载容器值: rcx = [r13-16]
                        self.emit(&[0x49, 0x8B, 0x4D, 0xf0]); // mov rcx, [r13-16]
                        // 清除目标位: inv_mask = ~(((1<<bw)-1) << bit_off)
                        let mask: u64 = if bw >= 64 { u64::MAX } else { (1u64 << bw) - 1 };
                        let inv_mask = !(mask << bit_off);
                        // and rcx, inv_mask
                        self.emit(&[0x48, 0x81, 0xE1]); // and rcx, imm32
                        self.emit(&(inv_mask as u32).to_le_bytes());
                        // or rcx, rax
                        self.emit(&[0x48, 0x09, 0xC1]); // or rcx, rax
                        // 加载地址: rdx = [r13-8]
                        self.emit(&[0x49, 0x8B, 0x55, 0xf8]); // mov rdx, [r13-8]
                        // 存储容器 (结果在 rcx 中)
                        match container_size {
                            4 => self.emit(&[0x89, 0x0A]), // mov [rdx], ecx
                            8 => self.emit(&[0x48, 0x89, 0x0A]), // mov [rdx], rcx
                            2 => self.emit(&[0x66, 0x89, 0x0A]), // mov [rdx], cx
                            1 => self.emit(&[0x88, 0x0A]), // mov [rdx], cl
                            _ => {}
                        }
                    } else {
                        self.push(0); // 保存目标地址
                        self.gen_expr(r, 0); // rax = 值
                        self.pop(1); // rcx = 目标地址
                        let size = field_info.as_ref().map_or(4, |f| f.ty.size());
                        match size {
                            1 => self.emit(&[0x88, 0x01]),
                            2 => self.emit(&[0x66, 0x89, 0x01]),
                            4 => self.emit(&[0x89, 0x01]),
                            8 => self.emit(&[0x48, 0x89, 0x01]),
                            _ => {}
                        }
                    }
                }
            }
            Expr::Unary { op, e, .. } => {
                self.gen_expr(e, dst);
                match op {
                    UnOp::Neg => {
                        // neg r64 (带 REX.W 确保 64 位操作, 避免 32 位零扩展问题)
                        let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                        self.emit(&[rex, 0xf7, 0xd8 | (dst & 7)]);
                    }
                    UnOp::Not => {
                        // 逻辑非: !x = (x == 0) ? 1 : 0
                        // test dst, dst
                        let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                        self.emit(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                        // sete al: 零 → 1, 非零 → 0 (0F 94 C0)
                        self.emit(&[0x0F, 0x94, 0xC0]);
                        // movzx dst, al (48 0F B6 C0)
                        let rex2 = 0x48 | if dst >= 8 { 0x41 } else { 0 };
                        self.emit(&[rex2, 0x0F, 0xB6, 0xC0 | (dst & 7)]);
                    }
                    UnOp::BitNot => {
                        let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                        self.emit(&[rex, 0xf7, 0xd0 | (dst & 7)]);
                    }
                    UnOp::Plus => {}
                }
            }
            Expr::Call { callee, args, .. } => {
                // 检查是否为直接调用 (callee 是 Ident)
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    // __atomic_* 内置函数特殊处理 (生成 lock 前缀指令)
                    if name.name.starts_with("__atomic_") && self.gen_atomic_builtin(&name.name, args, dst) {
                        // 已成功生成原子指令
                    } else if self.gen_builtin(&name.name, args, dst) {
                        // 已成功处理 __builtin_* 函数
                    } else {
                        // 直接调用: 检查是否为可变参数函数
                        let is_va_call = self.variadic_funcs.contains_key(&name.name);
                        if is_va_call {
                            // 可变参数函数: 先压栈 (反向), 再设寄存器
                            // 这样 push 循环不会 clobber 寄存器值
                            for a in args.iter().rev() {
                                self.gen_expr(a, 0);
                                self.emit(&[0x50]); // push rax
                            }
                            // 命名参数放入寄存器 (重新求值, 简单参数无副作用)
                            let ar = [7u8, 6, 2, 1, 8, 9];
                            for (i, a) in args.iter().enumerate() {
                                if i < 6 { self.gen_expr_decay(a, ar[i]); }
                            }
                            self.call_label(&name.name);
                            // 清理压栈的参数
                            let nargs = args.len() as i32 * 8;
                            if nargs > 0 {
                                self.emit(&[0x48, 0x81, 0xc4]); // add rsp, nargs
                                self.emit(&nargs.to_le_bytes());
                            }
                            if dst != 0 { self.mov(dst, 0); }
                        } else {
                        // 普通函数: 使用寄存器传参
                        let ar = [7u8, 6, 2, 1, 8, 9];
                        let mut arg_idx = 0usize;
                        for a in args.iter() {
                            let a_ty = a.ty();
                            let a_sz = a_ty.size();
                            let is_agg_arg = matches!(&a_ty, Type::Struct(_,_) | Type::Union(_,_) | Type::Array(_,ArraySize::Const(_)))
                                && a_sz > 8;
                            if is_agg_arg && arg_idx < 6 {
                                // struct >8B 参数: 加载多个块到连续寄存器
                                if let Expr::Ident { name: src_name, .. } = a {
                                    let src_off = self.var_offset(&src_name.name);
                                    let chunks = (a_sz + 7) / 8;
                                    for c in 0..chunks as usize {
                                        if arg_idx + c < 6 {
                                            self.load_stack(ar[arg_idx + c], src_off + (c as i32 * 8));
                                        }
                                    }
                                    arg_idx += chunks as usize;
                                } else {
                                    // 非 Ident 源: 先求值存临时, 再加载
                                    self.gen_expr(a, 0);
                                    // TODO: handle rdx for >8B non-ident
                                    if arg_idx < 6 { self.mov(ar[arg_idx], 0); }
                                    arg_idx += 1;
                                }
                            } else {
                                if arg_idx < 6 {
                                    self.gen_expr_decay(a, ar[arg_idx]);
                                }
                                arg_idx += 1;
                            }
                        }
                        // 嵌套函数调用: 设置静态链 r10 = r13 (当前帧指针)
                        if self.nested_funcs.contains_key(&name.name) {
                            self.emit(&[0x4c, 0x89, 0xea]); // mov r10, r13
                        }
                        self.emit(&[0x54]); // push rsp
                        self.call_label(&name.name);
                        self.emit(&[0x5c]); // pop rsp
                        if dst != 0 { self.mov(dst, 0); }
                        } // end of non-variadic call
                    }
                } else {
                    // 间接调用: 通过函数指针
                    // 1. 求值 callee 表达式得到函数地址, 保存到 r11 (不在 arg 寄存器列表中)
                    self.gen_expr(callee, 11); // r11 = 函数指针
                    // 2. 求值参数到 SysV ABI 寄存器
                    let ar = [7u8, 6, 2, 1, 8, 9];
                    for (i, a) in args.iter().enumerate() { self.gen_expr(a, ar[i]); }
                    // 3. push rsp 保护
                    self.emit(&[0x54]); // push rsp
                    // 4. call *r11: 49 FF D3
                    self.emit(&[0x49, 0xFF, 0xD3]);
                    self.emit(&[0x5c]); // pop rsp
                    if dst != 0 { self.mov(dst, 0); }
                }
            }
            Expr::SizeofTy { ty, .. } => self.mov_imm(dst, ty.size() as i128),
            Expr::SizeofExpr { e, .. } => self.mov_imm(dst, e.ty().size() as i128),
            Expr::AlignofTy { ty, .. } => self.mov_imm(dst, ty.align() as i128),
            Expr::AlignofExpr { e, .. } => self.mov_imm(dst, e.ty().align() as i128),
            Expr::Offsetof { ty, ref member, .. } => {
                let off = Self::compute_offset(ty, member);
                self.mov_imm(dst, off as i128);
            }
            Expr::Comma { l, r, .. } => {
                // 逗号运算符: 求值 lhs 丢弃, 求值 rhs 结果放入 dst
                self.gen_expr(l, 0);
                self.gen_expr(r, dst);
            }
            Expr::AsmExpr { asm_str, .. } => {
                // 内联汇编表达式: 实际发射汇编指令, 结果在 rax
                let op_map: HashMap<String, String> = HashMap::new();
                self.emit_inline_asm(asm_str, &op_map);
                if dst != 0 { self.mov(dst, 0); }
            }
            Expr::LabelAddr { label, .. } => {
                // &&label: 加载标签地址到 dst (GNU 扩展)
                // 使用 LEA dst, [RIP + disp32] + label_addr_refs 延迟解析
                let label_name = label.name.clone();
                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                let modrm = 0x05 | ((dst & 7) << 3);
                self.emit(&[rex, 0x8D, modrm, 0, 0, 0, 0]);
                let disp32_pos = self.pos() - 4;
                self.label_addr_refs.push((disp32_pos, label_name));
            }
            Expr::StmtExpr { block, .. } => {
                // 语句表达式 ({...}): 执行块内语句, 最后表达式值留在 dst
                let items = &block.items;
                let last_idx = items.len().saturating_sub(1);
                for (i, item) in items.iter().enumerate() {
                    match item {
                        BlockItem::Stmt(Stmt::Expr { e, .. }) if i == last_idx => {
                            // 最后一条表达式语句: 结果放入 dst
                            self.gen_expr(e, dst);
                        }
                        BlockItem::Stmt(Stmt::Return { val: Some(e), .. }) if i == last_idx => {
                            self.gen_expr(e, dst);
                        }
                        BlockItem::Stmt(s) => self.gen_stmt(s),
                        BlockItem::Decl(Decl::Var(v)) => {
                            self.register_var(&v.name.name, &v.ty);
                            if let Some(init) = &v.init {
                                self.gen_expr(init, 0);
                                self.mov_to_stack(0, &v.name.name);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Expr::Cast { ty, e, .. } => {
                let src_ty = e.ty();
                self.gen_expr(e, dst);
                // 类型转换
                match (&src_ty, ty) {
                    // int → float/double
                    (Type::Int | Type::Long | Type::Char | Type::Short, Type::Float) => {
                        // cvtsi2ss xmm0, rax (F3 0F 2A C0)
                        self.emit(&[0xF3, 0x0F, 0x2A, 0xC0]);
                    }
                    (Type::Int | Type::Long | Type::Char | Type::Short, Type::Double | Type::LongDouble) => {
                        // cvtsi2sd xmm0, rax (F2 0F 2A C0)
                        self.emit(&[0xF2, 0x0F, 0x2A, 0xC0]);
                    }
                    // float/double → int
                    (Type::Float, Type::Int | Type::Long | Type::Char | Type::Short) => {
                        // cvttss2si rax, xmm0 (F3 0F 2C C0)
                        self.emit(&[0xF3, 0x0F, 0x2C, 0xC0]);
                    }
                    (Type::Double | Type::LongDouble, Type::Int | Type::Long | Type::Char | Type::Short) => {
                        // cvttsd2si rax, xmm0 (F2 0F 2C C0)
                        self.emit(&[0xF2, 0x0F, 0x2C, 0xC0]);
                    }
                    // float ↔ double
                    (Type::Float, Type::Double | Type::LongDouble) => {
                        // cvtss2sd xmm0, xmm0 (F3 0F 5A C0)
                        self.emit(&[0xF3, 0x0F, 0x5A, 0xC0]);
                    }
                    (Type::Double | Type::LongDouble, Type::Float) => {
                        // cvtsd2ss xmm0, xmm0 (F2 0F 5A C0)
                        self.emit(&[0xF2, 0x0F, 0x5A, 0xC0]);
                    }
                    // 其他情况: 不转换
                    _ => {}
                }
            }
            Expr::Cond { cond, then_, else_, .. } => {
                self.gen_expr(cond, 0); self.emit(&[0x85, 0xc0]);
                let le = self.fresh_label(); let lend = self.fresh_label();
                self.jmp_rel32(&le, JumpKind::Je);
                self.gen_expr(then_, dst); self.jmp_rel32(&lend, JumpKind::Jmp);
                self.label(&le); self.gen_expr(else_, dst); self.label(&lend);
            }
            Expr::String { data, .. } => {
                // 将字符串加入 .rodata 池, 生成 RIP-relative LEA
                let data = data.clone();
                if !self.string_pool.contains_key(&data) {
                    let o = self.rodata.len() as u64;
                    self.rodata.extend_from_slice(&data);
                    self.string_pool.insert(data.clone(), o);
                }
                let at = self.pos();
                // LEA dst, [RIP + disp32]  (7 bytes, disp32 = placeholder 0)
                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                self.emit(&[rex, 0x8D, 0x05 | ((dst & 7) << 3), 0, 0, 0, 0]);
                self.string_refs.push((at, data));
            }
            Expr::Float { v, ty, .. } => {
                // 浮点字面量: 存入 .rodata, 用 movsd/movss 加载到 xmm0
                let is_float = matches!(ty, Type::Float);
                let is_ld = matches!(ty, Type::LongDouble);
                let bytes = if is_float {
                    // float: 4 字节
                    let f = *v as f32;
                    f.to_le_bytes().to_vec()
                } else if is_ld {
                    // long double: 16 字节 (低 8 字节 double, 高 8 字节零)
                    let mut b = v.to_le_bytes().to_vec();
                    b.extend_from_slice(&[0u8; 8]);
                    b
                } else {
                    // double: 8 字节
                    v.to_le_bytes().to_vec()
                };
                let key = format!("f_{}", bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>());
                let key_bytes = key.into_bytes();
                if !self.string_pool.contains_key(&key_bytes) {
                    let o = self.rodata.len() as u64;
                    self.rodata.extend_from_slice(&bytes);
                    self.string_pool.insert(key_bytes.clone(), o);
                }
                let off = self.string_pool[&key_bytes];
                let at = self.pos();
                // movsd xmm0, [RIP + disp32]  (F2 0F 10 05 disp32)
                // movss xmm0, [RIP + disp32]  (F3 0F 10 05 disp32)
                let prefix = if is_float { 0xF3 } else { 0xF2 };
                self.emit(&[prefix, 0x0F, 0x10, 0x05, 0, 0, 0, 0]);
                self.float_refs.push((at, off));
            }
            Expr::Index { arr, idx, ty: elem_ty, .. } => {
                let es = elem_ty.size() as i32;
                if let Expr::Ident { name: arr_name, ty: arr_ty, .. } = arr.as_ref() {
                    if self.vla_offsets.contains_key(&arr_name.name) {
                        // VLA: 基址存储在变量栈槽中
                        let off = self.var_offset(&arr_name.name);
                        self.load_stack(0, off); // rax = VLA 基址
                    } else if matches!(arr_ty, Type::Array(_, _)) {
                        // 静态数组: LEA 取基址
                        if self.globals.contains_key(&arr_name.name) {
                            self.emit_global_addr(0, &arr_name.name);
                        } else {
                            let off = self.var_offset(&arr_name.name);
                            self.emit_lea_r13(0, off);
                        }
                    } else {
                        // 指针: 加载指针值 (但全局数组衰减需用 LEA)
                        if self.globals.get(&arr_name.name).map_or(false, |g| matches!(g.ty, Type::Array(_, _))) {
                            self.emit_global_addr(0, &arr_name.name);
                        } else if self.globals.contains_key(&arr_name.name) {
                            self.emit_global_load(0, &arr_name.name);
                        } else {
                            let off = self.var_offset(&arr_name.name);
                            self.load_stack(0, off);
                        }
                    }
                    self.push(0); // 保存基址 (gen_expr(idx) 可能破坏 rax)
                    self.gen_expr(idx, 1); // rcx = index
                    self.pop(0); // 恢复基址到 rax
                    if es != 1 {
                        self.emit(&[0x48, 0x69, 0xc9]); // imul rcx, rcx, imm32
                        self.emit(&es.to_le_bytes());
                    }
                    self.emit(&[0x48, 0x01, 0xc8]); // add rax, rcx
                    self.emit_load_mem(es as usize); // 按元素大小加载
                    if dst != 0 { self.mov(dst, 0); }
                }
            }
            Expr::PostInc { e, .. } => self.gen_inc_dec(e, dst, false, true),
            Expr::PostDec { e, .. } => self.gen_inc_dec(e, dst, false, false),
            Expr::PreInc  { e, .. } => self.gen_inc_dec(e, dst, true, true),
            Expr::PreDec  { e, .. } => self.gen_inc_dec(e, dst, true, false),
            Expr::Deref { e, .. } => {
                // *e: 求值内部表达式得到地址, 然后读取该地址的值
                self.gen_expr(e, 0);
                self.emit_load_mem(e.ty().size()); // 按类型大小加载
                if dst != 0 { self.mov(dst, 0); }
            }
            Expr::AddrOf { e, .. } => {
                // &var / &struct.field / &ptr->field / *ptr: 计算地址到 dst
                if let Expr::Ident { name, .. } = e.as_ref() {
                    if self.globals.contains_key(&name.name) {
                        self.emit_global_addr(dst, &name.name);
                    } else {
                        let off = self.var_offset(&name.name);
                        let rex = 0x49u8; // REX.W + REX.B (r13)
                        self.emit(&[rex, 0x8D]);
                        if off.abs() < 128 {
                            self.emit(&[0x45 | ((dst & 7) << 3), off as i8 as u8]);
                        } else {
                            self.emit(&[0x85 | ((dst & 7) << 3)]);
                            self.emit(&off.to_le_bytes());
                        }
                    }
                } else if let Expr::Member { expr, field, is_arrow, .. } = e.as_ref() {
                    // &struct.field: 基址 + 字段偏移 (不加载值)
                    if *is_arrow {
                        self.gen_expr(expr, dst);
                    } else if let Expr::Ident { name, .. } = expr.as_ref() {
                        if self.globals.contains_key(&name.name) {
                            self.emit_global_addr(dst, &name.name);
                        } else {
                            let off = self.var_offset(&name.name);
                            let rex = 0x49u8;
                            self.emit(&[rex, 0x8D]);
                            if off.abs() < 128 {
                                self.emit(&[0x45 | ((dst & 7) << 3), off as i8 as u8]);
                            } else {
                                self.emit(&[0x85 | ((dst & 7) << 3)]);
                                self.emit(&off.to_le_bytes());
                            }
                        }
                    } else {
                        self.gen_expr(expr, dst);
                    }
                    let field_info = if let Type::Struct(_, fields) | Type::Union(_, fields) = &expr.ty() {
                        find_field(fields, &field.name).map(|(f, extra)| {
                            let mut fc = f.clone();
                            fc.offset += extra;
                            fc
                        })
                    } else { None };
                    if let Some(ref f) = field_info {
                        let field_off = f.offset as i32;
                        if field_off != 0 {
                            let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                            self.emit(&[rex, 0x81, 0xC0 | (dst & 7)]);
                            self.emit(&field_off.to_le_bytes());
                        }
                    }
                } else if let Expr::Deref { e: inner, .. } = e.as_ref() {
                    // *ptr: 地址就是指针值
                    self.gen_expr(inner, dst);
                } else if let Expr::Index { arr, idx, ty: elem_ty, .. } = e.as_ref() {
                    // &arr[i]: 基址 + i * elem_size
                    let es = elem_ty.size() as i32;
                    if let Expr::Ident { name: arr_name, .. } = arr.as_ref() {
                        if self.globals.contains_key(&arr_name.name) {
                            self.emit_global_addr(0, &arr_name.name);
                        } else if self.vla_offsets.contains_key(&arr_name.name) {
                            let off = self.var_offset(&arr_name.name);
                            self.load_stack(0, off);
                        } else {
                            let off = self.var_offset(&arr_name.name);
                            self.emit_lea_r13(0, off);
                        }
                    }
                    self.push(0);
                    self.gen_expr(idx, 1);
                    self.pop(0);
                    if es != 1 {
                        self.emit(&[0x48, 0x69, 0xC9]); // imul rcx, rcx, es
                        self.emit(&es.to_le_bytes());
                    }
                    self.emit(&[0x48, 0x01, 0xC8]); // add rax, rcx
                    if dst != 0 { self.mov(dst, 0); }
                }
            }
            Expr::Member { expr, field, is_arrow, ty, .. } => {
                // struct/union 成员访问: 基址 + 偏移
                // 1. 计算基址 (struct指针或struct变量地址)
                if *is_arrow {
                    self.gen_expr(expr, dst);
                } else {
                    if let Expr::Ident { name, .. } = expr.as_ref() {
                        if self.globals.contains_key(&name.name) {
                            self.emit_global_addr(dst, &name.name);
                        } else {
                            let off = self.var_offset(&name.name);
                            let rex = 0x49u8;
                            self.emit(&[rex, 0x8D]);
                            if off.abs() < 128 {
                                self.emit(&[0x45 | ((dst & 7) << 3), off as i8 as u8]);
                            } else {
                                self.emit(&[0x85 | ((dst & 7) << 3)]);
                                self.emit(&off.to_le_bytes());
                            }
                        }
                    } else {
                        self.gen_expr(expr, dst);
                    }
                }
                // 2. 查找字段信息 (支持匿名 struct/union), 加上字段偏移
                let field_info = if let Type::Struct(_, fields) | Type::Union(_, fields) = &expr.ty() {
                    find_field(fields, &field.name).map(|(f, extra)| {
                        let mut fc = f.clone();
                        fc.offset += extra;
                        fc
                    })
                } else { None };
                if let Some(ref f) = field_info {
                    let field_off = f.offset as i32;
                    if field_off != 0 {
                        let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                        self.emit(&[rex, 0x81, 0xC0 | (dst & 7)]);
                        self.emit(&field_off.to_le_bytes());
                    }
                }
                // 3. 加载成员值 (位域 vs 普通)
                if let Some(ref f) = field_info {
                    if let Some(bw) = f.bit_width {
                        // 位域读取: 加载容器 → 右移 → 掩码
                        let container_size = f.ty.size();
                        // 加载容器值
                        match container_size {
                            1 => self.emit(&[0x48, 0x0F, 0xB6, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                            2 => self.emit(&[0x48, 0x0F, 0xB7, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                            4 => self.emit(&[0x8B, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                            8 => self.emit(&[0x48, 0x8B, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                            _ => {}
                        }
                        // 右移 bit_offset 位
                        if f.bit_offset > 0 {
                            let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                            self.emit(&[rex, 0xC1, 0xE8 | (dst & 7), f.bit_offset as u8]);
                        }
                        // 掩码: and dst, (1 << bw) - 1
                        if bw < 64 {
                            let mask: u64 = if bw == 64 { u64::MAX } else { (1u64 << bw) - 1 };
                            let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                            self.emit(&[rex, 0x81, 0xE0 | (dst & 7)]);
                            self.emit(&(mask as u32).to_le_bytes());
                        }
                        // 有符号位域: 符号扩展
                        let is_unsigned = matches!(f.ty, Type::UInt | Type::ULong | Type::ULongLong | Type::UShort | Type::UChar | Type::Bool);
                        if !is_unsigned && bw < 64 {
                            // 检查符号位 (bit bw-1)
                            let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                            // test dst, (1 << (bw-1))
                            self.emit(&[rex, 0xF7, 0xC0 | (dst & 7)]);
                            self.emit(&((1u32 << (bw - 1)).to_le_bytes()));
                            // jz +7 (跳过符号扩展)
                            self.emit(&[0x74, 0x07]);
                            // or dst, !(mask)  (填充高位)
                            let inv_mask: u32 = !(((1u32 << bw) - 1)) as u32;
                            let rex2 = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                            self.emit(&[rex2, 0x81, 0xC8 | (dst & 7)]);
                            self.emit(&inv_mask.to_le_bytes());
                        }
                    } else {
                        // 普通字段: 直接加载
                        let size = f.ty.size();
                        match size {
                            1 => self.emit(&[0x48, 0x0F, 0xB6, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                            2 => self.emit(&[0x48, 0x0F, 0xB7, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                            4 => self.emit(&[0x8B, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                            8 => self.emit(&[0x48, 0x8B, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                            _ => {}
                        }
                    }
                } else {
                    let size = ty.size();
                    match size {
                        1 => self.emit(&[0x48, 0x0F, 0xB6, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                        2 => self.emit(&[0x48, 0x0F, 0xB7, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                        4 => self.emit(&[0x8B, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                        8 => self.emit(&[0x48, 0x8B, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                        _ => {}
                    }
                }
            }
            Expr::CompoundLit { ty, inits, .. } => {
                // 复合字面量: (type){...} - 在 rodata 中存储初始化数据, 加载地址
                let size = ty.size();
                let data = self.gen_global_init_list(ty, inits).unwrap_or_else(|| vec![0; size]);
                // 使用 string_pool 存储
                if !self.string_pool.contains_key(&data) {
                    let off = self.rodata.len() as u64;
                    self.rodata.extend_from_slice(&data);
                    self.string_pool.insert(data.clone(), off);
                }
                let off = self.string_pool[&data];
                // 加载地址到 dst (RIP-relative LEA)
                let code_pos = self.pos();
                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                self.emit(&[rex, 0x8D, 0x05 | ((dst & 7) << 3), 0, 0, 0, 0]);
                // 记录引用, 在 resolve 阶段修补
                self.float_refs.push((code_pos + 3, off));
            }
            Expr::VaArg { ap, ty: arg_ty, .. } => {
                // va_arg(ap, type): 从 ap 指向的地址读取值, 然后 ap += 8
                // 使用 push/pop 保存中间值, 避免寄存器冲突
                if let Expr::Ident { name, .. } = ap.as_ref() {
                    let ap_off = self.var_offset(&name.name);
                    // 1. 加载 ap 指针到 rcx
                    self.load_stack(1, ap_off);           // rcx = 当前参数指针
                    // 2. 从 [rcx] 读取值到 rax
                    let sz = arg_ty.size();
                    match sz {
                        1 => { self.emit(&[0x48, 0x0F, 0xB6, 0x01]); }  // movzbl (%rcx),%eax
                        2 => { self.emit(&[0x48, 0x0F, 0xB7, 0x01]); }  // movzwl (%rcx),%eax
                        4 => { self.emit(&[0x8B, 0x01]); }               // mov (%rcx),%eax
                        _ => { self.emit(&[0x48, 0x8B, 0x01]); }         // mov (%rcx),%rax
                    }
                    // 3. 保存指针和值到栈
                    self.push(1);                         // push 指针 (rcx)
                    self.push(0);                         // push 值 (rax)
                    // 4. 更新 ap 指针: 从栈加载原始指针, +8, 存回
                    self.load_stack(1, ap_off);           // rcx = 原始指针 (重新加载)
                    self.emit(&[0x48, 0x83, 0xC1, 8]);   // add $0x8,%rcx
                    self.store_stack(1, ap_off);          // *ap = rcx
                    // 5. 恢复值到 dst
                    self.pop(0);                          // rax = 值
                    if dst != 0 { self.mov(dst, 0); }    // dst = rax
                    self.pop(1);                          // rcx = 恢复指针 (清理栈)
                } else {
                    self.mov_imm(dst, 0);
                }
            }
            _ => { /* M0: 其他表达式暂不处理 */ }
        }
    }

    // 初始化列表 codegen: 依次计算每个元素并存储到变量
    fn gen_init_list(&mut self, var_name: &str, ty: &Type, inits: &[InitItem]) {
        let var_off = self.var_offset(var_name);
        match ty {
            Type::Array(elem_ty, _) => {
                // 数组初始化: int a[] = {1, 2, 3}
                // 数组在栈上布局: arr[0] 在 var_off, arr[1] 在 var_off + elem_size, ...
                // 注意: 虽然栈向下增长，但数组元素地址是递增的
                let elem_size = elem_ty.size();
                for (i, item) in inits.iter().enumerate() {
                    let (_idx, value) = match item {
                        InitItem::Expr(e) => (i, e),
                        InitItem::Designated { designator: Designator::Index(idx_expr), value } => {
                            if let Expr::Int { v, .. } = idx_expr { (*v as usize, value) } else { continue; }
                        }
                        _ => continue,
                    };
                    self.gen_expr(value, 0);
                    // arr[i] 在 var_off + i * elem_size
                    let off = var_off + (i * elem_size) as i32;
                    self.emit_store(0, off, elem_size);
                }
            }
            Type::Vector(elem_ty, n) => {
                // SIMD 向量初始化: v4si a = {1, 2, 3, 4}
                let elem_size = elem_ty.size();
                for (i, item) in inits.iter().enumerate() {
                    if i >= *n { break; }
                    let value = match item {
                        InitItem::Expr(e) => e,
                        _ => continue,
                    };
                    self.gen_expr(value, 0);
                    let off = var_off + (i * elem_size) as i32;
                    self.emit_store(0, off, elem_size);
                }
            }
            Type::Struct(_, fields) | Type::Union(_, fields) => {
                // struct 初始化: struct Point p = {1, 2} 或 {.x = 1, .y = 2}
                let mut field_idx = 0;
                for item in inits {
                    let (field, value) = match item {
                        InitItem::Expr(e) => {
                            if field_idx < fields.len() {
                                let f = &fields[field_idx];
                                field_idx += 1;
                                (f, e)
                            } else { continue; }
                        }
                        InitItem::Designated { designator: Designator::Field(name), value } => {
                            if let Some(f) = fields.iter().find(|f| f.name == name.name) {
                                (f, value)
                            } else { continue; }
                        }
                        _ => continue,
                    };
                    self.gen_expr(value, 0);
                    let off = var_off + field.offset as i32;
                    self.emit_store(0, off, field.ty.size());
                }
            }
            _ => {
                // 标量类型的单元素初始化
                if let Some(InitItem::Expr(e)) = inits.first() {
                    self.gen_expr(e, 0);
                    self.mov_to_stack(0, var_name);
                }
            }
        }
    }

    // 全局变量初始化列表: 生成初始化数据字节
    fn gen_global_init_list(&self, ty: &Type, inits: &[InitItem]) -> Option<Vec<u8>> {
        let mut data = Vec::new();
        match ty {
            Type::Array(elem_ty, _) => {
                let elem_size = elem_ty.size();
                for (i, item) in inits.iter().enumerate() {
                    let value = match item {
                        InitItem::Expr(e) => e,
                        InitItem::Designated { designator: Designator::Index(idx_expr), value } => {
                            if let Expr::Int { v, .. } = idx_expr {
                                // 填充前面的零
                                while data.len() < (*v as usize) * elem_size { data.push(0); }
                                value
                            } else { continue; }
                        }
                        _ => continue,
                    };
                    let elem_data = self.eval_const_expr(value);
                    data.extend_from_slice(&elem_data);
                    // 填充到元素大小
                    while data.len() < (i + 1) * elem_size { data.push(0); }
                }
            }
            Type::Struct(_, fields) | Type::Union(_, fields) => {
                let total_size = ty.size();
                data.resize(total_size, 0);
                let mut field_idx = 0;
                for item in inits {
                    let (field, value) = match item {
                        InitItem::Expr(e) => {
                            if field_idx < fields.len() {
                                let f = &fields[field_idx];
                                field_idx += 1;
                                (f, e)
                            } else { continue; }
                        }
                        InitItem::Designated { designator: Designator::Field(name), value } => {
                            if let Some(f) = fields.iter().find(|f| f.name == name.name) {
                                (f, value)
                            } else { continue; }
                        }
                        _ => continue,
                    };
                    let field_data = self.eval_const_expr(value);
                    let off = field.offset;
                    for (j, &b) in field_data.iter().enumerate() {
                        if off + j < data.len() { data[off + j] = b; }
                    }
                }
            }
            _ => {
                if let Some(InitItem::Expr(e)) = inits.first() {
                    data = self.eval_const_expr(e);
                }
            }
        }
        if data.is_empty() { None } else { Some(data) }
    }

    // 计算常量表达式的字节表示
    fn eval_const_expr(&self, e: &Expr) -> Vec<u8> {
        match e {
            Expr::Int { v, ty, .. } => {
                let sz = ty.size();
                v.to_le_bytes()[..sz].to_vec()
            }
            Expr::Char { v, .. } => vec![*v],
            Expr::String { data, .. } => data.clone(),
            _ => vec![0; e.ty().size()],
        }
    }

    // 存储到内存 [r13 + offset]: 根据大小生成对应的存储指令
    fn emit_store(&mut self, src_reg: u8, offset: i32, size: usize) {
        // 使用 store_stack 的逻辑，但支持任意偏移
        let mut rex = 0x49u8; // REX.W + REX.B (r13)
        if src_reg >= 8 { rex |= 0x04; } // REX.R for src_reg>=8
        match size {
            1 => {
                // mov [r13 + disp], src_reg (byte) - 不需要 REX.W
                let rex_byte = 0x41u8 | if src_reg >= 8 { 0x04 } else { 0 }; // REX.B for r13
                self.emit(&[rex_byte, 0x88]);
                if offset.abs() < 128 {
                    self.emit(&[0x45 | ((src_reg & 7) << 3), offset as i8 as u8]);
                } else {
                    self.emit(&[0x85 | ((src_reg & 7) << 3)]);
                    self.emit(&offset.to_le_bytes());
                }
            }
            2 => {
                // mov [r13 + disp], src_reg (word)
                self.emit(&[0x66]); // operand size override
                let rex_byte = 0x41u8 | if src_reg >= 8 { 0x04 } else { 0 }; // REX.B for r13
                self.emit(&[rex_byte, 0x89]);
                if offset.abs() < 128 {
                    self.emit(&[0x45 | ((src_reg & 7) << 3), offset as i8 as u8]);
                } else {
                    self.emit(&[0x85 | ((src_reg & 7) << 3)]);
                    self.emit(&offset.to_le_bytes());
                }
            }
            4 => {
                // mov [r13 + disp], src_reg (dword) - 不用 REX.W, 否则会变成 8 字节存储
                let rex = 0x41u8; // REX.B only (r13), 无 REX.W
                if src_reg >= 8 { /* REX.R for src_reg>=8 */ }
                self.emit(&[rex | if src_reg >= 8 { 0x04 } else { 0 }, 0x89]);
                if offset.abs() < 128 {
                    self.emit(&[0x45 | ((src_reg & 7) << 3), offset as i8 as u8]);
                } else {
                    self.emit(&[0x85 | ((src_reg & 7) << 3)]);
                    self.emit(&offset.to_le_bytes());
                }
            }
            8 => {
                // mov [r13 + disp], src_reg (qword)
                self.emit(&[rex, 0x89]);
                if offset.abs() < 128 {
                    self.emit(&[0x45 | ((src_reg & 7) << 3), offset as i8 as u8]);
                } else {
                    self.emit(&[0x85 | ((src_reg & 7) << 3)]);
                    self.emit(&offset.to_le_bytes());
                }
            }
            _ => {}
        }
    }

    // P1-4: __int128 二元运算 codegen
    // 使用 rax:rdx = 右操作数, rcx:r8 = 左操作数, 结果 → rax:rdx
    fn gen_int128_binop(&mut self, op: BinOp, l: &Expr, r: &Expr, dst: u8) {
        // 1. 求值左操作数 → rax, push 两次 (低/高)
        self.gen_expr_decay(l, 0);
        self.push(0);           // push low
        self.emit(&[0x48, 0x99]); // cqo: rdx = sign_extend(rax)
        self.push(2);           // push high
        // 2. 求值右操作数 → rax:rdx
        self.gen_expr_decay(r, 0);
        self.emit(&[0x48, 0x99]); // cqo
        // 保存右操作数
        self.push(0);           // push rax (right_low)
        self.push(2);           // push rdx (right_high)
        // 3. 弹出左操作数 → rcx (low), r8 (high)
        self.pop(3);            // r8 = left_high
        self.pop(1);            // rcx = left_low
        // 4. 弹出右操作数 → r9 (low), r10 (high)
        self.pop(11);           // r11 = right_high
        self.pop(9);            // r9 = right_low
        // 现在: rcx=left_low, r8=left_high, r9=right_low, r11=right_high
        match op {
            BinOp::Add => {
                // add rcx, r9 (low): REX.R(r9=9→R=1) = 0x44
                self.emit(&[0x44, 0x01, 0xC9]); // add rcx, r9
                // adc r8, r11 (high): REX.R(r11=11→R=1) + REX.B(r8=8→B=1) = 0x45
                self.emit(&[0x45, 0x11, 0xD8]); // adc r8, r11
                self.mov(0, 1);  // rax = result_low
                self.mov(2, 8);  // rdx = result_high
            }
            BinOp::Sub => {
                // sub rcx, r9 (low): REX.R(r9→R=1) = 0x44
                self.emit(&[0x44, 0x29, 0xC9]); // sub rcx, r9
                // sbb r8, r11 (high): REX.R(r11→R=1) + REX.B(r8→B=1) = 0x45
                self.emit(&[0x45, 0x19, 0xD8]); // sbb r8, r11
                self.mov(0, 1);
                self.mov(2, 8);
            }
            BinOp::And => {
                self.emit(&[0x44, 0x21, 0xC9]); // and rcx, r9
                self.emit(&[0x45, 0x21, 0xC0]); // and r8, r11
                self.mov(0, 1);
                self.mov(2, 8);
            }
            BinOp::Or => {
                self.emit(&[0x44, 0x09, 0xC9]); // or rcx, r9
                self.emit(&[0x45, 0x09, 0xC0]); // or r8, r11
                self.mov(0, 1);
                self.mov(2, 8);
            }
            BinOp::Xor => {
                self.emit(&[0x44, 0x31, 0xC9]); // xor rcx, r9
                self.emit(&[0x45, 0x31, 0xC0]); // xor r8, r11
                self.mov(0, 1);
                self.mov(2, 8);
            }
            BinOp::EqEq => {
                self.emit(&[0x48, 0x31, 0xC0]); // xor rax, rax
                self.emit(&[0x44, 0x39, 0xC9]); // cmp rcx, r9 (low)
                let lne = self.fresh_label();
                self.jmp_rel32(&lne, JumpKind::Jne);
                self.emit(&[0x45, 0x39, 0xC0]); // cmp r8, r11 (high)
                self.jmp_rel32(&lne, JumpKind::Jne);
                self.mov_imm(0, 1);
                self.label(&lne);
                self.emit(&[0x48, 0x31, 0xD2]); // xor rdx, rdx
            }
            BinOp::Ne => {
                self.emit(&[0x48, 0x31, 0xC0]); // xor rax, rax
                self.mov_imm(0, 1);
                let leq = self.fresh_label();
                self.emit(&[0x44, 0x39, 0xC9]); // cmp rcx, r9 (low)
                self.jmp_rel32(&leq, JumpKind::Jne);
                self.emit(&[0x45, 0x39, 0xC0]); // cmp r8, r11 (high)
                self.jmp_rel32(&leq, JumpKind::Jne);
                self.mov_imm(0, 0);
                self.label(&leq);
                self.emit(&[0x48, 0x31, 0xD2]); // xor rdx, rdx
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let is_signed = matches!(op, BinOp::Lt | BinOp::Le);
                self.gen_int128_cmp_ordering(op, is_signed);
            }
            BinOp::Shl => {
                // rcx=left_low, r8=left_high, r9=shift_amount
                // 目标: rax:left_low, rdx:left_high, rcx:shift_count
                self.mov(1, 9);  // rcx = shift_amount (cl)
                // 但 rcx 原来存 left_low, 需要先保存到其他地方
                // 重新组织: 先保存 shift 到 r9→已在那里, 用 rax=left_low, rdx=left_high
                // rcx 被覆盖前 left_low 已在 rax 中? 不, left_low 在 rcx 中
                // 解决: 先 mov rax, rcx (保存 left_low), 然后 mov rcx, r9 (shift)
                self.mov(0, 1);  // rax = rcx = left_low
                self.mov(2, 8);  // rdx = r8 = left_high
                self.mov(1, 9);  // rcx = r9 = shift_amount
                // cmp rcx, 64
                self.emit(&[0x48, 0x83, 0xF9, 64]); // cmp rcx, 64
                let l_small = self.fresh_label();
                let l_end = self.fresh_label();
                self.jmp_rel32(&l_small, JumpKind::Jl);
                // >= 64: rdx = rax << (cl-64), rax = 0
                self.mov(8, 0);  // r8 = rax = left_low (保存)
                self.emit(&[0x48, 0x31, 0xC0]); // xor rax, rax (low = 0)
                self.mov(2, 8);  // rdx = r8 = left_low
                self.emit(&[0x48, 0xD3, 0xE2]); // shl rdx, cl
                self.jmp_rel32(&l_end, JumpKind::Jmp);
                // < 64: shld rdx, rax, cl; shl rax, cl
                self.label(&l_small);
                self.emit(&[0x48, 0x0F, 0xA5, 0xD0]); // shld rdx, rax, cl
                self.emit(&[0x48, 0xD3, 0xE0]);       // shl rax, cl
                self.label(&l_end);
                // rax:rdx = result
            }
            BinOp::Shr => {
                // 逻辑右移 (unsigned)
                // rcx=left_low, r8=left_high, r9=shift_amount
                self.mov(0, 1);  // rax = rcx = left_low
                self.mov(2, 8);  // rdx = r8 = left_high
                self.mov(1, 9);  // rcx = r9 = shift_amount
                self.emit(&[0x48, 0x83, 0xF9, 64]); // cmp rcx, 64
                let l_small = self.fresh_label();
                let l_end = self.fresh_label();
                self.jmp_rel32(&l_small, JumpKind::Jl);
                // >= 64: rax = rdx >> (cl-64), rdx = 0
                self.mov(8, 2);  // r8 = rdx = left_high (保存)
                self.emit(&[0x48, 0x31, 0xD2]); // xor rdx, rdx (high = 0)
                self.mov(0, 8);  // rax = r8 = left_high
                self.emit(&[0x48, 0xD3, 0xE8]); // shr rax, cl
                self.jmp_rel32(&l_end, JumpKind::Jmp);
                // < 64: shrd rax, rdx, cl; shr rdx, cl
                self.label(&l_small);
                self.emit(&[0x48, 0x0F, 0xA5, 0xC2]); // shrd rax, rdx, cl
                self.emit(&[0x48, 0xD3, 0xEA]);       // shr rdx, cl
                self.label(&l_end);
            }
            _ => {
                // 不支持的 __int128 运算 (Mul/Div/Mod): 发射 nop 占位
                // 结果 rax:rdx = 0
                self.emit(&[0x48, 0x31, 0xC0]); // xor rax, rax
                self.emit(&[0x48, 0x31, 0xD2]); // xor rdx, rdx
            }
        }
        // 5. 移动到 dst
        if dst != 0 { self.mov(dst, 0); }
        // 结果的高 64 位在 rdx, 调用者需要存储到栈的第二个槽
    }

    // P1-4: __int128 有序比较 (Lt/Le/Gt/Ge)
    // 输入: rcx=left_low, r8=left_high, r9=right_low, r11=right_high
    // 输出: rax = 0 或 1, rdx = 0
    fn gen_int128_cmp_ordering(&mut self, op: BinOp, is_signed: bool) {
        let l_end = self.fresh_label();
        let l_low = self.fresh_label();
        // 比较高 64 位: cmp r8, r11
        self.emit(&[0x45, 0x39, 0xC0]); // cmp r8, r11
        let (_cc_high, cc_low_eq, setcc_ne) = match op {
            BinOp::Lt if is_signed  => (0x9C, 0x92, 0x9C),
            BinOp::Le if is_signed  => (0x9C, 0x92, 0x9E),
            BinOp::Gt if is_signed  => (0x9F, 0x97, 0x9F),
            BinOp::Ge if is_signed  => (0x9F, 0x97, 0x9D),
            BinOp::Lt                 => (0x92, 0x92, 0x92),
            BinOp::Le                 => (0x92, 0x92, 0x96),
            BinOp::Gt                 => (0x97, 0x97, 0x97),
            BinOp::Ge                 => (0x97, 0x97, 0x93),
            _ => unreachable!(),
        };
        // jne l_low (高半部不等时跳到低半部处理)
        self.jmp_rel32(&l_low, JumpKind::Jne);
        // 高半部相等: 比较低半部 cmp rcx, r9
        self.emit(&[0x44, 0x39, 0xC9]); // cmp rcx, r9
        self.emit(&[0x48, 0x31, 0xC0]); // xor rax, rax
        self.emit(&[0x0F, cc_low_eq, 0xC0]); // setcc al
        self.jmp_rel32(&l_end, JumpKind::Jmp);
        // 高半部不等: 使用高半部比较结果
        self.label(&l_low);
        self.emit(&[0x48, 0x31, 0xC0]); // xor rax, rax
        self.emit(&[0x0F, setcc_ne, 0xC0]); // setcc al
        self.label(&l_end);
        self.emit(&[0x48, 0x31, 0xD2]); // xor rdx, rdx
    }

    // P1-4: __int128 加载变量到 rax:rdx (低:高)
    fn gen_int128_load(&mut self, name: &str) {
        if self.globals.contains_key(name) {
            // 全局变量: 加载 16 字节
            self.emit_global_load(0, name); // rax = low 64 bits
            // 高 64 位在全局偏移 +8
            // 简化: 使用 RIP-relative 加载
            self.push(0);
            // lea rax, [RIP + off] 然后 mov rdx, [rax]
            // 简化: 使用绝对偏移
            self.emit(&[0x48, 0x8B, 0x15]); // mov rdx, [RIP+disp32]
            let disp32_pos = self.pos();
            self.emit(&[0, 0, 0, 0]);
            self.pending_data_rels.push((disp32_pos, name.to_string()));
            self.pop(0);
        } else {
            let off = self.var_offset(name);
            // rax = [r13 + off] (low)
            self.load_stack(0, off);
            // rdx = [r13 + off + 8] (high)
            self.load_stack(2, off + 8);
        }
    }

    // P1-4: __int128 从 rax:rdx 存储到变量
    fn gen_int128_store(&mut self, name: &str) {
        if self.globals.contains_key(name) {
            self.emit_global_store(0, name);
            // 高 64 位: 简化处理
        } else {
            let off = self.var_offset(name);
            self.store_stack(0, off);      // low → [r13+off]
            self.store_stack(2, off + 8);  // high → [r13+off+8]
        }
    }

    // 指针/整数自增自减: step = sizeof(*ptr) 或 1
    fn gen_inc_dec(&mut self, e: &Expr, dst: u8, is_pre: bool, is_inc: bool) {
        let inner_ty = e.ty();
        let step = if inner_ty.is_ptr() {
            if let Type::Ptr(ref inner) = inner_ty { inner.size() as i32 } else { 1 }
        } else { 1 };
        if let Expr::Ident { name, .. } = e {
            let off = self.var_offset(&name.name);
            self.load_stack(dst, off);
            if !is_pre { self.push(dst); } // 后缀: 保存旧值
            // add/sub imm32 (0x81 /0 for add, 0x81 /5 for sub)
            let opcode = if is_inc { 0x81u8 } else { 0x81u8 };
            let modrm_base = if is_inc { 0xC0u8 | ((dst & 7) << 3) } else { 0xE8u8 | ((dst & 7) << 3) };
            if dst < 8 {
                self.emit(&[0x48, opcode, modrm_base]);
                self.emit(&(step as i32).to_le_bytes());
            } else {
                self.emit(&[0x49, opcode, modrm_base]);
                self.emit(&(step as i32).to_le_bytes());
            }
            self.store_stack(dst, off);
            if !is_pre { self.pop(dst); } // 后缀: 恢复旧值到 dst
        }
    }

    // 浮点运算: SSE 指令 (addsd/sd, subsd, mulsd, divsd, ucomisd)
    // 使用 xmm0 和 xmm1, 结果在 xmm0
    // 左操作数保存到帧临时槽 (避免嵌套 binop 时 xmm 寄存器冲突)
    fn gen_float_binop(&mut self, op: BinOp, l: &Expr, r: &Expr) {
        let is_float = matches!(l.ty(), Type::Float);
        // 保存左操作数到 rsp 下方 (红区)
        // 用 sub rsp, 8 分配空间, 避免 movq [rsp-8] 编码问题
        // 嵌套时每个深度用不同的偏移: [rsp - 8*(depth+1)]
        let save_bytes = (self.expr_depth as i32 + 1) * 8;
        // 计算左操作数到 xmm0
        self.gen_expr(l, 0);
        // 保存 xmm0: sub rsp, save_bytes; movsd [rsp], xmm0
        self.emit(&[0x48, 0x83, 0xEC, save_bytes as u8]); // sub rsp, save_bytes
        if is_float {
            self.emit(&[0xF3, 0x0F, 0x11, 0x04, 0x24]); // movss [rsp], xmm0
        } else {
            self.emit(&[0xF2, 0x0F, 0x11, 0x04, 0x24]); // movsd [rsp], xmm0
        }
        // 计算右操作数到 xmm0
        self.expr_depth += 1;
        self.gen_expr(r, 0);
        self.expr_depth -= 1;
        // xmm1 = 右操作数, 恢复左操作数到 xmm0
        self.emit(&[0x66, 0x0F, 0x28, 0xC8]); // movapd xmm1, xmm0 (右→xmm1)
        if is_float {
            self.emit(&[0xF3, 0x0F, 0x10, 0x04, 0x24]); // movss xmm0, [rsp]
        } else {
            self.emit(&[0xF2, 0x0F, 0x10, 0x04, 0x24]); // movsd xmm0, [rsp]
        }
        // 恢复 rsp
        self.emit(&[0x48, 0x83, 0xC4, save_bytes as u8]); // add rsp, save_bytes
        // 现在 xmm0=左, xmm1=右 → xmm0 op xmm1 = 左 op 右
        // 执行运算
        let (prefix, opcode) = match op {
            BinOp::Add => (if is_float { 0xF3 } else { 0xF2 }, 0x58), // addss/addsd
            BinOp::Sub => (if is_float { 0xF3 } else { 0xF2 }, 0x5C), // subss/subsd
            BinOp::Mul => (if is_float { 0xF3 } else { 0xF2 }, 0x59), // mulss/mulsd
            BinOp::Div => (if is_float { 0xF3 } else { 0xF2 }, 0x5E), // divss/divsd
            // 比较运算
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne => {
                self.gen_float_cmp(op, &l.ty());
                return;
            }
            _ => return, // 不支持的运算
        };
        // xmm0 op xmm1 -> xmm0
        self.emit(&[prefix, 0x0F, opcode, 0xC1]);
    }

    // 浮点比较: ucomisd + setcc -> al -> movzx rax, al
    fn gen_float_cmp(&mut self, op: BinOp, ty: &Type) {
        let is_float = matches!(ty, Type::Float);
        // xmm0 = 左操作数, xmm1 = 右操作数
        // ucomiss/ucomisd xmm0, xmm1 (比较 左 vs 右)
        if is_float {
            self.emit(&[0x0F, 0x2E, 0xC1]); // ucomiss xmm0, xmm1
        } else {
            self.emit(&[0x66, 0x0F, 0x2E, 0xC1]); // ucomisd xmm0, xmm1
        }
        // 设置条件码
        let cc = match op {
            BinOp::Lt => 0x92, // jb (below)
            BinOp::Le => 0x96, // jbe (below or equal)
            BinOp::Gt => 0x97, // ja (above)
            BinOp::Ge => 0x93, // jae (above or equal)
            BinOp::EqEq => 0x94, // je
            BinOp::Ne => 0x95, // jne
            _ => 0x94,
        };
        // setcc al
        self.emit(&[0x0F, cc, 0xC0]);
        // movzx rax, al
        self.emit(&[0x48, 0x0F, 0xB6, 0xC0]);
    }

    // SIMD 向量运算: 使用标量整数运算逐元素操作 (避免 SSE 对齐问题)
    fn gen_vector_binop(&mut self, op: BinOp, l: &Expr, r: &Expr) {
        let (elem_ty, count) = match l.ty() {
            Type::Vector(ref elem, n) => { let et = (**elem).clone(); (et, n) }
            _ => return,
        };
        let elem_size = elem_ty.size();
        // 使用帧底部临时区域保存结果 (完全不用 XMM 寄存器)
        let result_off = -(self.frame_size as i32) + 8;
        for i in 0..count {
            // 加载左操作数第 i 个元素到 rax
            self.gen_scalar_vector_load(l, i, elem_size);
            // 保存到 rcx
            self.emit(&[0x48, 0x89, 0xC1]); // mov rcx, rax
            // 加载右操作数第 i 个元素到 rax
            self.gen_scalar_vector_load(r, i, elem_size);
            // rcx = rcx op rax
            match op {
                BinOp::Add => { self.emit(&[0x48, 0x01, 0xC1]); }
                BinOp::Sub => { self.emit(&[0x48, 0x29, 0xC1]); }
                BinOp::Mul => { self.emit(&[0x48, 0x0F, 0xAF, 0xC8]); } // imul rcx, rax
                _ => { self.emit(&[0x48, 0x01, 0xC1]); }
            }
            // 存储结果到临时区域: mov [r13+off], ecx
            let off = result_off + (i * elem_size) as i32;
            self.emit(&[0x41, 0x89]); // REX.B + mov r/m32, r32
            if off.abs() < 128 {
                // ModRM: mod=01, reg=001(ecx), rm=101(r13) = 0x45 | (1<<3) = 0x4D
                self.emit(&[0x4D, off as i8 as u8]);
            } else {
                self.emit(&[0x8D]); // mod=10, reg=001, rm=101
                self.emit(&off.to_le_bytes());
            }
        }
        // 结果在临时区域, gen_vector_store 会从该区域读取
    }

    /// 加载向量变量的第 i 个元素到 rax (标量)
    fn gen_scalar_vector_load(&mut self, e: &Expr, elem_idx: usize, elem_size: usize) {
        match e {
            Expr::Ident { name, .. } => {
                if self.globals.contains_key(&name.name) {
                    // 全局向量: 加载基地址然后偏移
                    let disp32_pos = self.pos() + 3;
                    self.emit(&[0x48, 0x8D, 0x05, 0, 0, 0, 0]); // lea rax, [RIP+disp32]
                    self.pending_data_rels.push((disp32_pos, name.name.clone()));
                    let off = (elem_idx * elem_size) as i32;
                    if off != 0 {
                        self.emit(&[0x48, 0x01]); // add rax, off (简化)
                        // 用更简洁的方式
                    }
                    // 加载元素
                    self.emit_load_mem(elem_size);
                } else {
                    let off = self.var_offset(&name.name) + (elem_idx * elem_size) as i32;
                    self.load_stack(0, off);
                }
            }
            _ => {
                self.gen_expr(e, 0);
                let off = (elem_idx * elem_size) as i32;
                if off != 0 {
                    self.emit(&[0x48, 0x83, 0xC0]); // add rax, off
                    self.emit(&[off as i8 as u8]);
                }
                self.emit_load_mem(elem_size);
            }
        }
    }

    // 加载向量变量到临时区域 (完全标量, 不用 XMM)
    fn gen_vector_load(&mut self, e: &Expr) {
        let (elem_ty, count) = match e.ty() {
            Type::Vector(ref elem, n) => { let et = (**elem).clone(); (et, n) }
            _ => return,
        };
        let elem_size = elem_ty.size();
        let temp_off = -(self.frame_size as i32) + 8;
        match e {
            Expr::Ident { name, .. } => {
                if self.globals.contains_key(&name.name) {
                    // TODO: 全局向量复制
                    return;
                }
                let var_off = self.var_offset(&name.name);
                for i in 0..count {
                    // 从源变量加载元素 (按元素大小)
                    let src_off = var_off + (i * elem_size) as i32;
                    self.emit(&[0x49, 0x8D]); // lea rax, [r13 + src_off]
                    if src_off.abs() < 128 {
                        self.emit(&[0x45, src_off as i8 as u8]);
                    } else {
                        self.emit(&[0x85]);
                        self.emit(&src_off.to_le_bytes());
                    }
                    self.emit_load_mem(elem_size);
                    // 存储到临时区域
                    let dst_off = temp_off + (i * elem_size) as i32;
                    self.emit_store(0, dst_off, elem_size);
                }
            }
            _ => {
                // 其他表达式: 先求值到 rax (地址), 然后复制
                self.gen_expr(e, 0);
                // TODO: 间接复制
            }
        }
    }

    // 从临时区域存储到向量变量 (完全标量, 不用 XMM)
    fn gen_vector_store(&mut self, name: &str) {
        let var_off = self.var_offset(name);
        let ty = self.var_map.get(name).map(|(_, ty)| ty.clone()).unwrap_or(Type::Int);
        let (elem_ty, count) = match ty {
            Type::Vector(ref elem, n) => { let et = (**elem).clone(); (et, n) }
            _ => return,
        };
        let elem_size = elem_ty.size();
        let temp_off = -(self.frame_size as i32) + 8;
        for i in 0..count {
            // 从临时区域加载: 使用 emit_load_mem 按元素大小加载
            let src_off = temp_off + (i * elem_size) as i32;
            self.emit(&[0x49, 0x8D]); // lea rax, [r13 + src_off]
            if src_off.abs() < 128 {
                self.emit(&[0x45, src_off as i8 as u8]);
            } else {
                self.emit(&[0x85]);
                self.emit(&src_off.to_le_bytes());
            }
            self.emit_load_mem(elem_size); // 按元素大小加载到 rax
            // 存储到目标变量: 使用 emit_store 按元素大小存储
            let dst_off = var_off + (i * elem_size) as i32;
            self.emit_store(0, dst_off, elem_size);
        }
    }

    // 指令发射
    fn mov_imm(&mut self, reg: u8, v: i128) {
        if v >= i32::MIN as i128 && v <= i32::MAX as i128 {
            if reg < 8 { self.emit(&[0xb8 + reg]); self.emit(&(v as i32).to_le_bytes()); }
            else { self.emit(&[0x41, 0xb8 + (reg - 8)]); self.emit(&(v as i32).to_le_bytes()); }
        } else {
            self.emit(&[0x48 + if reg >= 8 { 0x41 } else { 0 }]);
            self.emit(&[0xb8 + (reg & 7)]); self.emit(&(v as i64).to_le_bytes());
        }
    }
    fn mov(&mut self, dst: u8, src: u8) {
        // mov dst, src: 使用 0x8B (mov r64, r/m64) 使 dst 在 reg 字段, src 在 r/m 字段
        let mut rex = 0x48u8; if dst >= 8 { rex |= 0x04; } if src >= 8 { rex |= 0x01; }
        self.emit(&[rex, 0x8B, 0xC0 | ((dst & 7) << 3) | (src & 7)]);
    }

    /// 整数提升: 将小于 int 的整数类型提升到 64 位
    /// char/short → 符号扩展; uchar/ushort/bool → 零扩展
    fn gen_int_promote(&mut self, dst: u8, ty: &Type) {
        let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
        match ty {
            Type::Char | Type::SChar | Type::Bool => {
                // movsx dst, dst_low8: 48 0F BE C0+dst,dst
                self.emit(&[rex, 0x0F, 0xBE, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
            }
            Type::Short => {
                // movsx dst, dst_low16: 48 0F BF C0+dst,dst
                self.emit(&[rex, 0x0F, 0xBF, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
            }
            Type::UChar => {
                // movzx dst, dst_low8: 48 0F B6 C0+dst,dst
                self.emit(&[rex, 0x0F, 0xB6, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
            }
            Type::UShort => {
                // movzx dst, dst_low16: 48 0F B7 C0+dst,dst
                self.emit(&[rex, 0x0F, 0xB7, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
            }
            _ => {} // int/long/ptr 等不需要提升
        }
    }
    fn push(&mut self, reg: u8) {
        // 硬件 push: 用 rsp, 变量用 r13, 互不干扰
        if reg == 0 { self.emit(&[0x50]); }       // push rax
        else if reg == 1 { self.emit(&[0x51]); }   // push rcx
        else if reg == 2 { self.emit(&[0x52]); }   // push rdx
        else if reg == 3 { self.emit(&[0x53]); }   // push rbx
        else if reg == 7 { self.emit(&[0x57]); }   // push rdi
        else if reg == 6 { self.emit(&[0x56]); }   // push rsi
        else if reg == 8 { self.emit(&[0x41, 0x50]); } // push r8
        else if reg == 9 { self.emit(&[0x41, 0x51]); } // push r9
        else if reg == 10 { self.emit(&[0x41, 0x52]); } // push r10
        else if reg == 11 { self.emit(&[0x41, 0x53]); } // push r11
        else if reg == 12 { self.emit(&[0x41, 0x54]); } // push r12
        else if reg == 13 { self.emit(&[0x41, 0x55]); } // push r13
        else if reg == 14 { self.emit(&[0x41, 0x56]); } // push r14
        else if reg == 15 { self.emit(&[0x41, 0x57]); } // push r15
        else { panic!("push: unsupported reg {}", reg); }
    }
    fn pop(&mut self, reg: u8) {
        if reg == 0 { self.emit(&[0x58]); }       // pop rax
        else if reg == 1 { self.emit(&[0x59]); }   // pop rcx
        else if reg == 2 { self.emit(&[0x5a]); }   // pop rdx
        else if reg == 3 { self.emit(&[0x5b]); }   // pop rbx
        else if reg == 7 { self.emit(&[0x5f]); }   // pop rdi
        else if reg == 6 { self.emit(&[0x5e]); }   // pop rsi
        else if reg == 8 { self.emit(&[0x41, 0x58]); } // pop r8
        else if reg == 9 { self.emit(&[0x41, 0x59]); } // pop r9
        else if reg == 10 { self.emit(&[0x41, 0x5a]); } // pop r10
        else if reg == 11 { self.emit(&[0x41, 0x5b]); } // pop r11
        else if reg == 12 { self.emit(&[0x41, 0x5c]); } // pop r12
        else if reg == 13 { self.emit(&[0x41, 0x5d]); } // pop r13
        else if reg == 14 { self.emit(&[0x41, 0x5e]); } // pop r14
        else if reg == 15 { self.emit(&[0x41, 0x5f]); } // pop r15
        else { panic!("pop: unsupported reg {}", reg); }
    }

    fn binop(&mut self, op: BinOp, dst: u8, src: u8) {
        let is_cmp = matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne);
        if !is_cmp && !matches!(op, BinOp::Div | BinOp::Mod) {
            let mut rex = 0x48u8; if src >= 8 { rex |= 0x44; } if dst >= 8 { rex |= 0x41; } self.emit(&[rex]);
        }
        let r = (src & 7) << 3; let m = dst & 7;
        match op {
            BinOp::Add => self.emit(&[0x01, 0xC0 | r | m]),
            BinOp::Sub => self.emit(&[0x29, 0xC0 | r | m]),
            BinOp::And => self.emit(&[0x21, 0xC0 | r | m]),
            BinOp::Or  => self.emit(&[0x09, 0xC0 | r | m]),
            BinOp::Xor => self.emit(&[0x31, 0xC0 | r | m]),
            BinOp::Mul => self.emit(&[0x0F, 0xAF, 0xC0 | ((dst & 7) << 3) | (src & 7)]),
            BinOp::Div | BinOp::Mod => {
                self.emit(&[0x48, 0x99]);
                let mut rex = 0x48u8; if src >= 8 { rex |= 0x41; }
                self.emit(&[rex, 0xF7, 0xF8 | (src & 7)]);
                if matches!(op, BinOp::Mod) { self.mov(dst, 2); }
            }
            BinOp::Shl => { let mut rex = 0x48u8; if dst >= 8 { rex |= 0x41; } self.emit(&[rex, 0xD3, 0xE0 | m]); }
            BinOp::Shr => { let mut rex = 0x48u8; if dst >= 8 { rex |= 0x41; } self.emit(&[rex, 0xD3, 0xE8 | m]); }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne => {
                let mut rex = 0x48u8; if src >= 8 { rex |= 0x44; } if dst >= 8 { rex |= 0x41; }
                self.emit(&[rex, 0x39, 0xC0 | ((src & 7) << 3) | (dst & 7)]);
                let cc = match op { BinOp::Lt=>0x9C, BinOp::Le=>0x9E, BinOp::Gt=>0x9F, BinOp::Ge=>0x9D, BinOp::EqEq=>0x94, BinOp::Ne=>0x95, _=>unreachable!() };
                self.emit(&[0x0F, cc, 0xC0, 0x48, 0x0F, 0xB6, 0xC0]);
                if dst != 0 { self.mov(dst, 0); }
            }
            _ => panic!("binop: {:?}", op),
        }
    }

    fn var_offset(&mut self, name: &str) -> i32 {
        if let Some(&(off, _)) = self.var_map.get(name) { return off; }
        // 基于实际类型大小计算偏移 (过滤 padding 条目)
        // 变量放在保存寄存器下方: offset = -(used + 16)
        let used: i32 = self.var_map.values()
            .filter(|(_, ty)| !matches!(ty, Type::Void))
            .map(|(_, ty)| ((ty.size() + 7) / 8) as i32 * 8)
            .sum();
        let off = -(used + 16);
        self.var_map.insert(name.to_string(), (off, Type::Int)); off
    }

    /// 全局变量 RIP-relative 加载: reg = [global_var]
    /// 根据变量实际大小选择操作数尺寸 (1/2/4/8 字节)
    fn emit_global_load(&mut self, reg: u8, name: &str) {
        let sz = self.globals.get(name).map_or(8, |g| g.size);
        let pref = if reg >= 8 { 0x41 } else { 0x00 }; // REX.R for high reg
        match sz {
            1 => {
                // movzx reg32, byte [rip+disp32]: 0F B6 /r [disp32]
                let pos = self.pos() + 3 + if pref != 0 { 1 } else { 0 };
                let modrm = 0x05 | ((reg & 7) << 3);
                let mut b = if pref != 0 { vec![pref] } else { vec![] };
                b.extend_from_slice(&[0x0F, 0xB6, modrm, 0,0,0,0]);
                self.emit(&b);
                self.pending_data_rels.push((pos, name.to_string()));
            }
            2 => {
                // movzx reg32, word [rip+disp32]: 0F B7 /r [disp32]
                let pos = self.pos() + 3 + if pref != 0 { 1 } else { 0 };
                let modrm = 0x05 | ((reg & 7) << 3);
                let mut b = if pref != 0 { vec![pref] } else { vec![] };
                b.extend_from_slice(&[0x0F, 0xB7, modrm, 0,0,0,0]);
                self.emit(&b);
                self.pending_data_rels.push((pos, name.to_string()));
            }
            4 => {
                // 32-bit load (zero-extends to 64-bit): 8B /r [disp32]
                let pos = self.pos() + 2 + if pref != 0 { 1 } else { 0 };
                let modrm = 0x05 | ((reg & 7) << 3);
                let mut b = if pref != 0 { vec![pref] } else { vec![] };
                b.extend_from_slice(&[0x8B, modrm, 0,0,0,0]);
                self.emit(&b);
                self.pending_data_rels.push((pos, name.to_string()));
            }
            _ => {
                // 64-bit load: REX.W 8B /r [disp32]
                let pos = self.pos() + 3;
                let rex = if reg >= 8 { 0x4C } else { 0x48 };
                let modrm = 0x05 | ((reg & 7) << 3);
                self.emit(&[rex, 0x8B, modrm, 0,0,0,0]);
                self.pending_data_rels.push((pos, name.to_string()));
            }
        }
    }

    /// 全局变量 RIP-relative 存储: [global_var] = reg
    /// 根据变量实际大小选择操作数尺寸 (1/2/4/8 字节)
    fn emit_global_store(&mut self, reg: u8, name: &str) {
        let sz = self.globals.get(name).map_or(8, |g| g.size);
        let modrm = 0x05 | ((reg & 7) << 3);
        match sz {
            1 => {
                // byte store: 88 /r [disp32]
                // REX needed for registers 4-7 (spl/bpl/sil/dil)
                let pos = self.pos() + 2 + if reg >= 4 { 1 } else { 0 };
                if reg >= 4 {
                    self.emit(&[0x40, 0x88, modrm, 0,0,0,0]);
                } else {
                    self.emit(&[0x88, modrm, 0,0,0,0]);
                }
                self.pending_data_rels.push((pos, name.to_string()));
            }
            2 => {
                // word store: 66 89 /r [disp32]
                let pos = self.pos() + 3;
                self.emit(&[0x66, 0x89, modrm, 0,0,0,0]);
                self.pending_data_rels.push((pos, name.to_string()));
            }
            4 => {
                // 32-bit store: 89 /r [disp32]
                let pos = self.pos() + 2;
                self.emit(&[0x89, modrm, 0,0,0,0]);
                self.pending_data_rels.push((pos, name.to_string()));
            }
            _ => {
                // 64-bit store: REX.W 89 /r [disp32]
                let pos = self.pos() + 3;
                let rex = if reg >= 8 { 0x4C } else { 0x48 };
                self.emit(&[rex, 0x89, modrm, 0,0,0,0]);
                self.pending_data_rels.push((pos, name.to_string()));
            }
        }
    }

    /// 全局变量 RIP-relative 取地址: reg = &global_var
    /// 编码: REX.W 8D 05|reg<<3 disp32 (LEA)
    fn emit_global_addr(&mut self, reg: u8, name: &str) {
        let disp32_pos = self.pos() + 3;
        let rex = if reg >= 8 { 0x4C } else { 0x48 };
        self.emit(&[rex, 0x8D, 0x05 | ((reg & 7) << 3), 0, 0, 0, 0]);
        self.pending_data_rels.push((disp32_pos, name.to_string()));
    }

    // 根据大小从内存加载到 rax (零扩展)
    fn emit_load_mem(&mut self, size: usize) {
        match size {
            1 => self.emit(&[0x48, 0x0f, 0xb6, 0x00]), // movzbl (%rax), %rax
            2 => self.emit(&[0x48, 0x0f, 0xb7, 0x00]), // movzwl (%rax), %rax
            4 => self.emit(&[0x8b, 0x00]),              // mov (%rax), %eax (零扩展到64位)
            _ => self.emit(&[0x48, 0x8b, 0x00]),        // mov (%rax), %rax
        }
    }

    // 注册变量偏移 (数组按实际大小分配多个槽)
    fn register_var(&mut self, name: &str, ty: &Type) -> i32 {
        if let Some(&(off, _)) = self.var_map.get(name) { return off; }
        // P1-5: register asm 变量不分配栈槽, 直接绑定到指定寄存器
        if self.reg_bindings.contains_key(name) { return 0; }
        let slots = (ty.size() + 7) / 8; // 向上对齐到 8 字节
        let slots = slots.max(1) as i32;
        // 基于实际类型大小计算偏移 (过滤 padding 条目)
        // 变量放在保存寄存器下方: offset = -(used + 16)
        let used: i32 = self.var_map.values()
            .filter(|(_, ty)| !matches!(ty, Type::Void))
            .map(|(_, ty)| ((ty.size() + 7) / 8) as i32 * 8)
            .sum();
        let off = -(used + 16);
        self.var_map.insert(name.to_string(), (off, ty.clone()));
        // 为数组/向量预留额外槽位 (空插入占位, 保持 var_map 完整性)
        for i in 1..slots {
            let placeholder_name = format!("__pad_{}_{}", name, i);
            let pad_off = off - i * 8;
            self.var_map.insert(placeholder_name, (pad_off, Type::Void));
        }
        off
    }
    fn mov_to_stack(&mut self, reg: u8, name: &str) {
        // P1-5: register asm 变量直接存入绑定寄存器
        if let Some(&bound_reg) = self.reg_bindings.get(name) {
            if reg != bound_reg { self.mov(bound_reg, reg); }
            return;
        }
        let off = self.var_offset(name); self.store_stack(reg, off);
    }
    fn mov_from_stack(&mut self, reg: u8, name: &str) {
        // P1-5: register asm 变量直接从绑定寄存器加载
        if let Some(&bound_reg) = self.reg_bindings.get(name) {
            if reg != bound_reg { self.mov(reg, bound_reg); }
            return;
        }
        let off = self.var_offset(name); self.load_stack(reg, off);
    }

    // 浮点变量存储: xmm0 → 栈 (使用 XMM 指令)
    fn gen_float_store(&mut self, name: &str, ty: &Type) {
        let off = self.var_offset(name);
        let is_float = matches!(ty, Type::Float);
        let is_ld = matches!(ty, Type::LongDouble);
        if is_ld {
            // long double 16 字节: 分两次 movq 存储 (xmm0 低 8 字节 + 高 8 字节零)
            // movq [r13+off], xmm0: 66 49 0F D6 45|4D off
            self.emit(&[0x66, 0x49, 0x0F, 0xD6]);
            if off.abs() < 128 { self.emit(&[0x45, off as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
            // 高 8 字节存零: mov qword [r13+off-8], 0
            // 注意: off 为负数, off-8 更远离 r13 (在变量自身空间内)
            let off2 = off - 8;
            self.emit(&[0x49, 0xC7, 0x45]);
            if off2.abs() < 128 { self.emit(&[off2 as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off2.to_le_bytes()); }
            self.emit(&0u32.to_le_bytes());
        } else if is_float {
            // float: movss [r13+off], xmm0 → F3 49 0F 11 45 off
            self.emit(&[0xF3, 0x49, 0x0F, 0x11]);
            if off.abs() < 128 { self.emit(&[0x45, off as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
        } else {
            // double: movsd [r13+off], xmm0 → F2 49 0F 11 45 off
            self.emit(&[0xF2, 0x49, 0x0F, 0x11]);
            if off.abs() < 128 { self.emit(&[0x45, off as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
        }
    }
    // 浮点变量加载: 栈 → xmm0 (使用 XMM 指令)
    fn gen_float_load(&mut self, name: &str, ty: &Type) {
        let off = self.var_offset(name);
        let is_float = matches!(ty, Type::Float);
        let is_ld = matches!(ty, Type::LongDouble);
        if is_ld {
            self.emit(&[0xF3, 0x49, 0x0F, 0x7E]);
            if off.abs() < 128 { self.emit(&[0x45, off as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
        } else if is_float {
            self.emit(&[0xF3, 0x49, 0x0F, 0x10]);
            if off.abs() < 128 { self.emit(&[0x45, off as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
        } else {
            self.emit(&[0xF2, 0x49, 0x0F, 0x10]);
            if off.abs() < 128 { self.emit(&[0x45, off as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
        }
    }

    // 浮点值按偏移存储: xmm0 → [r13+off]
    fn gen_float_store_at(&mut self, off: i32, ty: &Type) {
        let is_float = matches!(ty, Type::Float);
        let is_ld = matches!(ty, Type::LongDouble);
        if is_ld {
            self.emit(&[0x66, 0x49, 0x0F, 0xD6]);
            if off.abs() < 128 { self.emit(&[0x45, off as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
            let off2 = off - 8;
            self.emit(&[0x49, 0xC7, 0x45]);
            if off2.abs() < 128 { self.emit(&[off2 as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off2.to_le_bytes()); }
            self.emit(&0u32.to_le_bytes());
        } else if is_float {
            self.emit(&[0xF3, 0x49, 0x0F, 0x11]);
            if off.abs() < 128 { self.emit(&[0x45, off as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
        } else {
            self.emit(&[0xF2, 0x49, 0x0F, 0x11]);
            if off.abs() < 128 { self.emit(&[0x45, off as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
        }
    }

    // Complex 变量标量初始化: 存实部、虚部清零
    fn gen_complex_init(&mut self, name: &str, inner: &Type, init: &Expr) {
        let off = self.var_offset(name);
        let inner_sz = inner.size() as i32;
        self.gen_expr(init, 0);
        // 存实部
        if matches!(inner, Type::Float | Type::Double | Type::LongDouble) {
            self.gen_float_store_at(off, inner);
        } else {
            let sz = inner.size();
            self.store_stack(0, off);
            if sz > 8 {
                self.store_stack(2, off + 8);
            }
        }
        // 虚部清零: LongDouble 实部占 [off-8, off+8), 虚部应在 [off-8-inner_sz, off-8)
        let imag_base = if matches!(inner, Type::LongDouble) {
            off - 8 - inner_sz  // LongDouble 向后占 16B
        } else {
            off + inner_sz
        };
        let imag_bytes = inner_sz as usize;
        for o in (0..imag_bytes).step_by(8) {
            self.emit(&[0x48, 0x31, 0xC0]); // xor rax, rax
            self.store_stack(0, imag_base + o as i32);
        }
    }

    // P2-14: _Complex 算术运算 (add/sub/mul)
    // 布局: [real, imag] 各 inner_sz 字节, 存储在变量栈槽中
    fn gen_complex_binop(&mut self, op: BinOp, l: &Expr, r: &Expr, _dst: u8) {
        let inner_ty = if let Type::Complex(inner) = &l.ty() { inner.as_ref().clone() } else { return };
        let inner_sz = inner_ty.size() as i32; // 4 for float, 8 for double
        let is_float = matches!(inner_ty, Type::Float);
        let total_sz = inner_sz * 2; // 16 for _Complex double
        // 保存空间: 左操作数 real/imag 各 total_sz, 右操作数 real/imag 各 total_sz
        let save_sz = total_sz * 2;
        self.emit(&[0x48, 0x83, 0xEC, save_sz as u8]); // sub rsp, save_sz
        // 求值左操作数 → 结果在 rax (整数) 或 xmm0 (浮点)
        // 对于 complex, gen_expr 会将 real/imag 存入变量的栈槽
        // 我们直接通过变量偏移加载
        if let Expr::Ident { name, .. } = l {
            let off = self.var_offset(&name.name);
            // 保存左 real → [rsp]
            if is_float {
                self.emit(&[0xF3, 0x49, 0x0F, 0x10, 0x44]); // movss xmm0, [r13+off]
                if off.abs() < 128 { self.emit(&[(off & 0xFF) as u8]); }
                else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
                self.emit(&[0xF3, 0x0F, 0x11, 0x04, 0x24]); // movss [rsp], xmm0
            } else {
                self.emit(&[0xF2, 0x49, 0x0F, 0x10, 0x44]); // movsd xmm0, [r13+off]
                if off.abs() < 128 { self.emit(&[(off & 0xFF) as u8]); }
                else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
                self.emit(&[0xF2, 0x0F, 0x11, 0x04, 0x24]); // movsd [rsp], xmm0
            }
            // 保存左 imag → [rsp + total_sz]
            let imag_off = off + inner_sz;
            if is_float {
                self.emit(&[0xF3, 0x49, 0x0F, 0x10, 0x44]); // movss xmm0, [r13+imag_off]
                if imag_off.abs() < 128 { self.emit(&[(imag_off & 0xFF) as u8]); }
                else { self.emit(&[0x85]); self.emit(&imag_off.to_le_bytes()); }
                self.emit(&[0xF3, 0x0F, 0x11, 0x44, 0x24, total_sz as u8]); // movss [rsp+total_sz], xmm0
            } else {
                self.emit(&[0xF2, 0x49, 0x0F, 0x10, 0x44]); // movsd xmm0, [r13+imag_off]
                if imag_off.abs() < 128 { self.emit(&[(imag_off & 0xFF) as u8]); }
                else { self.emit(&[0x85]); self.emit(&imag_off.to_le_bytes()); }
                self.emit(&[0xF2, 0x0F, 0x11, 0x44, 0x24]); // movsd [rsp+total_sz], xmm0
                self.emit(&[total_sz as u8]);
            }
        }
        // 求值右操作数
        if let Expr::Ident { name, .. } = r {
            let off = self.var_offset(&name.name);
            // 保存右 real → [rsp + total_sz*2 - inner_sz] 不, 用 [rsp + total_sz] 位置
            // 布局: [rsp+0]=l_real, [rsp+inner_sz]=l_imag, [rsp+total_sz]=r_real, [rsp+total_sz+inner_sz]=r_imag
            if is_float {
                self.emit(&[0xF3, 0x49, 0x0F, 0x10, 0x44]); // movss xmm0, [r13+off]
                if off.abs() < 128 { self.emit(&[(off & 0xFF) as u8]); }
                else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
                self.emit(&[0xF3, 0x0F, 0x11, 0x44, 0x24, total_sz as u8]); // movss [rsp+total_sz], xmm0
            } else {
                self.emit(&[0xF2, 0x49, 0x0F, 0x10, 0x44]); // movsd xmm0, [r13+off]
                if off.abs() < 128 { self.emit(&[(off & 0xFF) as u8]); }
                else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
                self.emit(&[0xF2, 0x0F, 0x11, 0x44, 0x24]); // movsd [rsp+total_sz], xmm0
                self.emit(&[total_sz as u8]);
            }
            // 保存右 imag
            let imag_off = off + inner_sz;
            let r_imag_slot = total_sz + inner_sz;
            if is_float {
                self.emit(&[0xF3, 0x49, 0x0F, 0x10, 0x44]); // movss xmm0, [r13+imag_off]
                if imag_off.abs() < 128 { self.emit(&[(imag_off & 0xFF) as u8]); }
                else { self.emit(&[0x85]); self.emit(&imag_off.to_le_bytes()); }
                self.emit(&[0xF3, 0x0F, 0x11, 0x44, 0x24, r_imag_slot as u8]); // movss [rsp+r_imag_slot], xmm0
            } else {
                self.emit(&[0xF2, 0x49, 0x0F, 0x10, 0x44]); // movsd xmm0, [r13+imag_off]
                if imag_off.abs() < 128 { self.emit(&[(imag_off & 0xFF) as u8]); }
                else { self.emit(&[0x85]); self.emit(&imag_off.to_le_bytes()); }
                self.emit(&[0xF2, 0x0F, 0x11, 0x44, 0x24]); // movsd [rsp+r_imag_slot], xmm0
                self.emit(&[r_imag_slot as u8]);
            }
        }
        // 根据操作执行运算
        // 布局: [rsp+0]=l_real, [rsp+inner_sz]=l_imag, [rsp+total_sz]=r_real, [rsp+total_sz+inner_sz]=r_imag
        let (load_prefix, op_add, op_sub, op_mul) = if is_float {
            (0xF3u8, 0x58u8, 0x5Cu8, 0x59u8) // movss, addss, subss, mulss
        } else {
            (0xF2u8, 0x58u8, 0x5Cu8, 0x59u8) // movsd, addsd, subsd, mulsd
        };
        match op {
            BinOp::Add | BinOp::Sub => {
                let op_code = if matches!(op, BinOp::Add) { op_add } else { op_sub };
                // real = l_real op r_real
                // load l_real → xmm0
                self.emit(&[load_prefix, 0x0F, 0x10, 0x04, 0x24]); // movs[sd] xmm0, [rsp]
                // load r_real → xmm1
                self.emit(&[load_prefix, 0x0F, 0x10, 0x4C, 0x24, total_sz as u8]); // movs[sd] xmm1, [rsp+total_sz]
                // xmm0 op xmm1
                self.emit(&[load_prefix, 0x0F, op_code, 0xC1]); // opss/opss xmm0, xmm1
                // store result real → [rsp]
                self.emit(&[load_prefix, 0x0F, 0x11, 0x04, 0x24]); // movs[sd] [rsp], xmm0
                // imag = l_imag op r_imag
                self.emit(&[load_prefix, 0x0F, 0x10, 0x44, 0x24, inner_sz as u8]); // movs[sd] xmm0, [rsp+inner_sz]
                let r_imag_slot = total_sz + inner_sz;
                self.emit(&[load_prefix, 0x0F, 0x10, 0x4C, 0x24, r_imag_slot as u8]); // movs[sd] xmm1, [rsp+r_imag_slot]
                self.emit(&[load_prefix, 0x0F, op_code, 0xC1]);
                self.emit(&[load_prefix, 0x0F, 0x11, 0x44, 0x24, inner_sz as u8]); // movs[sd] [rsp+inner_sz], xmm0
            }
            BinOp::Mul => {
                // (a+bi)*(c+di) = (ac-bd) + (ad+bc)i
                // xmm0=a, xmm1=b, xmm2=c, xmm3=d
                // load a → xmm0
                self.emit(&[load_prefix, 0x0F, 0x10, 0x04, 0x24]);
                // load c → xmm2
                self.emit(&[load_prefix, 0x0F, 0x10, 0x54, 0x24, total_sz as u8]); // movs[sd] xmm2, [rsp+total_sz]
                // load b → xmm1
                self.emit(&[load_prefix, 0x0F, 0x10, 0x4C, 0x24, inner_sz as u8]); // movs[sd] xmm1, [rsp+inner_sz]
                // load d → xmm3
                let r_imag_slot = total_sz + inner_sz;
                self.emit(&[load_prefix, 0x0F, 0x10, 0x5C, 0x24, r_imag_slot as u8]); // movs[sd] xmm3, [rsp+r_imag_slot]
                // xmm4 = a*c
                self.emit(&[0x66, 0x0F, 0x28, 0xE0]); // movapd xmm4, xmm0
                self.emit(&[load_prefix, 0x0F, op_mul, 0xE2]); // mulss/mulsd xmm4, xmm2
                // xmm5 = b*d
                self.emit(&[0x66, 0x0F, 0x28, 0xE9]); // movapd xmm5, xmm1
                self.emit(&[load_prefix, 0x0F, op_mul, 0xEB]); // mulss/mulsd xmm5, xmm3
                // real = ac - bd
                self.emit(&[load_prefix, 0x0F, op_sub, 0xE5]); // subss/subsd xmm4, xmm5
                // store real → [rsp]
                self.emit(&[load_prefix, 0x0F, 0x11, 0x24, 0x24]); // movs[sd] [rsp], xmm4
                // xmm4 = a*d
                self.emit(&[0x66, 0x0F, 0x28, 0xE0]); // movapd xmm4, xmm0
                self.emit(&[load_prefix, 0x0F, op_mul, 0xE3]); // mulss/mulsd xmm4, xmm3
                // xmm5 = b*c
                self.emit(&[0x66, 0x0F, 0x28, 0xE9]); // movapd xmm5, xmm1
                self.emit(&[load_prefix, 0x0F, op_mul, 0xEA]); // mulss/mulsd xmm5, xmm2
                // imag = ad + bc
                self.emit(&[load_prefix, 0x0F, op_add, 0xE5]); // addss/addsd xmm4, xmm5
                // store imag → [rsp+inner_sz]
                self.emit(&[load_prefix, 0x0F, 0x11, 0x64, 0x24, inner_sz as u8]); // movs[sd] [rsp+inner_sz], xmm4
            }
            _ => {} // 其他操作暂不支持
        }
        // 结果现在在 [rsp] (real) 和 [rsp+inner_sz] (imag)
        // 恢复到 xmm0/xmm1 并存储到目标变量
        // 但 complex 赋值通常通过 aggregate assign 处理
        // 这里将结果存到 rsp 顶部, 让调用者处理
        // 恢复 rsp
        self.emit(&[0x48, 0x83, 0xC4, save_sz as u8]); // add rsp, save_sz
    }

    fn store_stack(&mut self, reg: u8, off: i32) {
        // r13 基址: REX.W + REX.B(=1 for r13) + r/m=5 → [r13+disp8]
        let mut rex = 0x49u8; if reg >= 8 { rex |= 0x04; } // REX.R for reg>=8
        self.emit(&[rex, 0x89]);
        if off.abs() < 128 { self.emit(&[0x45 | ((reg & 7) << 3), off as i8 as u8]); }
        else { self.emit(&[0x85 | ((reg & 7) << 3)]); self.emit(&off.to_le_bytes()); }
    }
    fn load_stack(&mut self, reg: u8, off: i32) {
        let mut rex = 0x49u8; if reg >= 8 { rex |= 0x04; }
        self.emit(&[rex, 0x8B]);
        if off.abs() < 128 { self.emit(&[0x45 | ((reg & 7) << 3), off as i8 as u8]); }
        else { self.emit(&[0x85 | ((reg & 7) << 3)]); self.emit(&off.to_le_bytes()); }
    }

    // LEA reg, [r13 + off] — 取栈变量地址 (与 gen_expr_decay 编码一致)
    fn emit_lea_r13(&mut self, reg: u8, off: i32) {
        let rex = 0x49u8; // REX.W + REX.B (r13)
        self.emit(&[rex, 0x8D]);
        if off.abs() < 128 {
            self.emit(&[0x45 | ((reg & 7) << 3), off as i8 as u8]);
        } else {
            self.emit(&[0x85 | ((reg & 7) << 3)]);
            self.emit(&off.to_le_bytes());
        }
    }

    // struct/array 整体赋值: 逐块拷贝 (src → dst)
    // 策略: 取源地址 + 取目标地址 + rep movsb
    fn gen_aggregate_assign(&mut self, dst_name: &str, dst_ty: &Type, r: &Expr) {
        let sz = dst_ty.size();
        let dst_off = self.var_offset(dst_name);
        // 特殊处理: 源是函数调用 (返回值在 rax[:rdx] 中)
        if let Expr::Call { callee, args, .. } = r {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                // 生成参数
                let ar = [7u8, 6, 2, 1, 8, 9];
                for (i, a) in args.iter().enumerate() { self.gen_expr(a, ar[i]); }
                self.emit(&[0x54]); // push rsp
                self.call_label(&name.name);
                self.emit(&[0x5c]); // pop rsp
                // rax = 前 8 字节, rdx = 后 8 字节
                // 直接存储到目标变量
                self.store_stack(0, dst_off); // mov [r13+dst_off], rax
                if sz > 8 {
                    self.store_stack(2, dst_off + 8); // mov [r13+dst_off+8], rdx
                }
                return;
            }
        }
        // 1. 获取源地址到 rsi
        let got_src = match r {
            Expr::Ident { name: src_name, .. } => {
                if self.globals.contains_key(&src_name.name) {
                    self.emit_global_addr(6, &src_name.name); // rsi = 全局变量地址
                } else {
                    let src_off = self.var_offset(&src_name.name);
                    self.emit_lea_r13(6, src_off); // rsi = 源地址 (reg 6 = rsi)
                }
                true
            }
            _ => false,
        };
        if !got_src {
            // 通用路径: 求值表达式, 存到帧临时区, 然后拷贝
            self.gen_expr(r, 0);
            // 使用目标变量槽上方的安全临时区域
            let temp_off = dst_off - ((sz + 7) & !7) as i32; // 在目标下方留临时空间
            self.store_stack(0, temp_off);
            if sz > 8 {
                self.store_stack(2, temp_off + 8); // rdx = 第二部分
            }
            self.emit_lea_r13(6, temp_off); // rsi = 源 (reg 6 = rsi)
        }
        // 2. 取目标地址到 rdi
        self.emit_lea_r13(7, dst_off); // rdi = 目标地址
        // 3. rep movsq + rep movsb 拷贝
        let qwords = sz / 8;
        let remainder = sz % 8;
        if qwords > 0 {
            self.emit(&[0x48, 0xC7, 0xC1]); // mov rcx, imm32 (qwords)
            self.emit(&(qwords as u32).to_le_bytes());
            self.emit(&[0xF3, 0x48, 0xA5]); // rep movsq
        }
        if remainder > 0 {
            self.emit(&[0x48, 0xC7, 0xC1]); // mov rcx, imm32 (remainder)
            self.emit(&(remainder as u32).to_le_bytes());
            self.emit(&[0xF3, 0xA4]); // rep movsb
        }
    }

    // VLA 运行时分配: 动态栈分配
    // 策略: 计算大小 → 对齐 → sub rsp → 保存基址到变量槽
    fn gen_vla_alloc(&mut self, v: &VarDecl) {
        // VLA 变量类型: Array(elem, Var(expr))
        // 大小表达式在变量的 ArraySize::Var 中
        if let Type::Array(elem_ty, ArraySize::Var(ref size_expr)) = &v.ty {
            // 求值大小表达式到 rax
            self.gen_expr(size_expr, 0);
            // rax = 元素数量, 乘以元素大小
            let es = elem_ty.size() as i32;
            if es != 1 {
                self.emit(&[0x48, 0x69, 0xc0]); // imul rax, rax, imm32
                self.emit(&es.to_le_bytes());
            }
            // 对齐到 16 字节: (rax + 15) & ~15
            self.emit(&[0x48, 0x83, 0xc0, 0x0f]); // add rax, 15
            self.emit(&[0x48, 0x83, 0xe0, 0xf0]); // and rax, -16
            // 保存 VLA 大小到 vla_offsets
            let off = self.var_offset(&v.name.name);
            self.vla_offsets.insert(v.name.name.clone(), off);
            // 保存当前 rsp (VLA 基址)
            // sub rsp, rax: 48 29 C4
            self.emit(&[0x48, 0x29, 0xC4]);
            // mov [r13 + var_off], rsp: 保存 VLA 指针到变量槽
            let rex = 0x49u8; // REX.W + REX.B (r13 base)
            self.emit(&[rex, 0x89]);
            if off.abs() < 128 {
                // mod=01, reg=rsp(4), r/m=r13(5) → 0x65
                self.emit(&[0x65, off as i8 as u8]);
            } else {
                // mod=10, reg=rsp(4), r/m=r13(5) → 0x85
                self.emit(&[0x85]);
                self.emit(&off.to_le_bytes());
            }
            // 对齐 rsp 到 16 字节 (已经是 16 对齐的, 因为 rax 是 16 对齐的)
        }
    }

    // 函数尾声: 恢复 r13 + 返回
    fn emit_func_epilogue(&mut self) {
        // mov rsp, r13: 恢复栈指针
        self.emit(&[0x49, 0x8b, 0xe5]);
        self.emit(&[0x41, 0x5d, 0xc3]); // pop r13; ret
    }

    // 预扫描: 统计函数体中局部变量所需栈槽数 (数组按实际大小)
    fn count_locals(items: &[BlockItem]) -> usize {
        let mut n = 0;
        for item in items {
            match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    n += (v.ty.size() + 7) / 8; // 数组占多个槽
                }
                BlockItem::Stmt(s) => n += Self::count_locals_stmt(s),
                _ => {}
            }
        }
        n
    }

    // 预注册所有局部变量到 var_map (数组分配多槽, 保证帧大小计算正确)
    fn pre_register_vars(var_map: &mut HashMap<String, (i32, Type)>, items: &[BlockItem], temp_slots: i32) {
        for item in items {
            match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    let slots = ((v.ty.size() + 7) / 8).max(1) as i32;
                    // 基于实际类型大小计算偏移 (过滤 padding 条目)
                    let used: i32 = var_map.values()
                        .filter(|(_, ty)| !matches!(ty, Type::Void))
                        .map(|(_, ty)| ((ty.size() + 7) / 8) as i32 * 8)
                        .sum();
                    let off = -(used + 16);
                    var_map.entry(v.name.name.clone()).or_insert((off, v.ty.clone()));
                    for j in 1..slots {
                        let pad = format!("__pad_{}_{}", v.name.name, j);
                        let pad_off = off - j * 8;
                        var_map.entry(pad).or_insert((pad_off, Type::Void));
                    }
                }
                BlockItem::Stmt(s) => Self::pre_register_stmt(var_map, s, temp_slots),
                _ => {}
            }
        }
    }
    fn pre_register_stmt(var_map: &mut HashMap<String, (i32, Type)>, s: &Stmt, temp_slots: i32) {
        match s {
            Stmt::Block { b, .. } => Self::pre_register_vars(var_map, &b.items, temp_slots),
            Stmt::If { then_, else_, .. } => {
                Self::pre_register_stmt(var_map, then_, temp_slots);
                if let Some(e) = else_ { Self::pre_register_stmt(var_map, e, temp_slots); }
            }
            Stmt::While { body, .. } | Stmt::For { body, .. } | Stmt::Do { body, .. } => {
                Self::pre_register_stmt(var_map, body, temp_slots);
            }
            _ => {}
        }
    }
    fn count_locals_stmt(s: &Stmt) -> usize {
        match s {
            Stmt::Block { b, .. } => Self::count_locals(&b.items),
            Stmt::If { then_, else_, .. } => {
                Self::count_locals_stmt(then_)
                    + else_.as_ref().map_or(0, |e| Self::count_locals_stmt(e))
            }
            Stmt::While { body, .. } | Stmt::For { body, .. } | Stmt::Do { body, .. } => {
                Self::count_locals_stmt(body)
            }
            _ => 0,
        }
    }

    fn ends_with_return(items: &[BlockItem]) -> bool {
        match items.last() {
            Some(BlockItem::Stmt(Stmt::Return { .. })) => true,
            Some(BlockItem::Stmt(Stmt::Block { b, .. })) => Self::ends_with_return(&b.items),
            Some(BlockItem::Stmt(Stmt::If { then_, else_, .. })) =>
                Self::ends_with_return(&[BlockItem::Stmt(*then_.clone())])
                && else_.as_ref().map_or(false, |e| Self::ends_with_return(&[BlockItem::Stmt(*e.clone())])),
            _ => false,
        }
    }

    // 扫描最大表达式嵌套深度 (binop/call 各+1)
    fn max_expr_depth_stmt(items: &[BlockItem]) -> u32 {
        items.iter().map(|item| match item {
            BlockItem::Decl(Decl::Var(v)) => v.init.as_ref().map_or(0, |e| Self::max_expr_depth_expr(e)),
            BlockItem::Stmt(s) => Self::max_expr_depth_stmt_inner(s),
            _ => 0,
        }).max().unwrap_or(0)
    }
    fn max_expr_depth_stmt_inner(s: &Stmt) -> u32 {
        match s {
            Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => Self::max_expr_depth_expr(e),
            Stmt::If { cond, then_, else_, .. } => {
                let d = Self::max_expr_depth_expr(cond);
                d.max(Self::max_expr_depth_stmt_inner(then_))
                    .max(else_.as_ref().map_or(0, |e| Self::max_expr_depth_stmt_inner(e)))
            }
            Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
                Self::max_expr_depth_expr(cond).max(Self::max_expr_depth_stmt_inner(body))
            }
            Stmt::For { cond, body, .. } => {
                cond.as_ref().map_or(0, |c| Self::max_expr_depth_expr(c)).max(Self::max_expr_depth_stmt_inner(body))
            }
            Stmt::Block { b, .. } => Self::max_expr_depth_stmt(&b.items),
            _ => 0,
        }
    }
    fn max_expr_depth_expr(e: &Expr) -> u32 {
        match e {
            Expr::Binop { l, r, .. } => {
                1 + Self::max_expr_depth_expr(l).max(Self::max_expr_depth_expr(r))
            }
            Expr::Call { callee, args, .. } => {
                1 + Self::max_expr_depth_expr(callee).max(
                    args.iter().map(|a| Self::max_expr_depth_expr(a)).max().unwrap_or(0))
            }
            Expr::Comma { l, r, .. } => {
                Self::max_expr_depth_expr(l).max(Self::max_expr_depth_expr(r))
            }
            Expr::Assign { l, r, .. } => {
                1 + Self::max_expr_depth_expr(l).max(Self::max_expr_depth_expr(r))
            }
            Expr::Unary { e, .. } | Expr::Cast { e, .. } | Expr::AddrOf { e, .. } | Expr::Deref { e, .. }
            | Expr::PostInc { e, .. } | Expr::PostDec { e, .. } | Expr::PreInc { e, .. } | Expr::PreDec { e, .. } => {
                Self::max_expr_depth_expr(e)
            }
            Expr::Index { arr, idx, .. } => {
                Self::max_expr_depth_expr(arr).max(Self::max_expr_depth_expr(idx))
            }
            Expr::Member { expr, .. } => Self::max_expr_depth_expr(expr),
            Expr::Cond { cond, then_, else_, .. } => {
                Self::max_expr_depth_expr(cond)
                    .max(Self::max_expr_depth_expr(then_))
                    .max(Self::max_expr_depth_expr(else_))
            }
            _ => 0,
        }
    }

    /// __builtin_* 系列内置函数 codegen: 返回 true 表示已处理
    fn gen_builtin(&mut self, name: &str, args: &[Expr], dst: u8) -> bool {
        match name {
            // __builtin_expect(expr, val): 分支预测提示, 直接返回 expr
            "__builtin_expect" | "__builtin_expect_with_probability" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                self.gen_expr(&args[0], dst);
                true
            }
            // __builtin_constant_p(expr): 编译期常量判断, 始终返回 0
            "__builtin_constant_p" => {
                self.mov_imm(dst, 0);
                true
            }
            // __builtin_object_size(ptr, type): 安全检测, 返回 (size_t)-1 表示未知
            "__builtin_object_size" => {
                self.mov_imm(dst, -1i128); // (size_t)-1
                true
            }
            // __builtin_memcpy/__builtin_memmove: 内联 rep movsb (freestanding 无 libc)
            "__builtin_memcpy" | "__builtin_memmove" => {
                if args.len() < 3 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                // 参数: dst(rdi), src(rsi), n(rcx)
                self.gen_expr(&args[1], 6); // rsi = src
                self.gen_expr(&args[0], 7); // rdi = dst
                self.gen_expr(&args[2], 1); // rcx = n
                // 保存 rdi 作为返回值 (memcpy/memmove 返回 dst)
                self.emit(&[0x57]);             // push rdi
                self.emit(&[0xfc]);             // cld (清除方向标志)
                self.emit(&[0xf3, 0x48, 0xa4]); // rep movsb
                self.emit(&[0x5f]);             // pop rax (返回值 = dst)
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_memset: 内联 rep stosb (freestanding 无 libc)
            "__builtin_memset" => {
                if args.len() < 3 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                // 参数: dst(rdi), val(rax), n(rcx)
                self.gen_expr(&args[0], 7); // rdi = dst
                self.gen_expr(&args[1], 0); // rax = val
                self.gen_expr(&args[2], 1); // rcx = n
                self.emit(&[0x57]);             // push rdi
                self.emit(&[0xfc]);             // cld
                self.emit(&[0xf3, 0xaa]);       // rep stosb
                self.emit(&[0x5f]);             // pop rax (返回值 = dst)
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_memcmp: 转发到 libc ( memcmp 不适合简单内联)
            "__builtin_memcmp" => {
                let ar = [7u8, 6, 2, 1, 8, 9];
                for (i, a) in args.iter().enumerate() { self.gen_expr(a, ar[i]); }
                self.emit(&[0x54]); // push rsp
                self.call_label("memcmp");
                self.emit(&[0x5c]); // pop rsp
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_strcpy/__builtin_strncpy/__builtin_strcmp 等
            "__builtin_strcpy" | "__builtin_strncpy" | "__builtin_strcmp" | "__builtin_strlen" => {
                let libc_name = name.strip_prefix("__builtin_").unwrap();
                let ar = [7u8, 6, 2, 1, 8, 9];
                for (i, a) in args.iter().enumerate() { self.gen_expr(a, ar[i]); }
                self.emit(&[0x54]); // push rsp
                self.call_label(libc_name);
                self.emit(&[0x5c]); // pop rsp
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_abs: 绝对值
            "__builtin_abs" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                self.gen_expr(&args[0], dst);
                // abs: test dst, dst; jns skip; neg dst; skip:
                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                self.emit(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]); // test dst, dst
                self.emit(&[0x79, 0x04]); // jns +4
                self.emit(&[rex, 0xf7, 0xd8 | (dst & 7)]); // neg dst
                true
            }
            // __builtin_unreachable: 不可达, 生成 ud2
            "__builtin_unreachable" | "__builtin_trap" => {
                self.emit(&[0x0F, 0x0B]); // ud2
                true
            }
            // __builtin_va_start(ap, last): ap = 第一个可变参数地址 (运行时计算)
            "__builtin_va_start" => {
                if args.len() >= 1 {
                    if let Expr::Ident { name, .. } = &args[0] {
                        let ap_off = self.var_offset(&name.name);
                        // r13 指向 saved r13 (帧顶), 不随 sub rsp 调整
                        // 栈布局: [saved_r13]=r13+0, [ret_addr]=r13+8, [arg_i]=r13+16+8*i
                        // 第一个 variadic arg (索引 K) 在 r13 + 16 + 8*K
                        let va_off = 16 + 8 * self.variadic_named_params;
                        self.emit_lea_r13(dst, va_off); // dst = r13 + va_off
                        // *ap = dst
                        self.store_stack(dst, ap_off);
                    }
                }
                true
            }
            // __builtin_va_end(ap): no-op
            "__builtin_va_end" => {
                true
            }
            // __builtin_va_arg 已通过 Expr::VaArg 处理, 此处不会到达
            "__builtin_va_arg" => {
                true
            }
            // __builtin_va_copy(dest, src): *dest = *src
            "__builtin_va_copy" => {
                if args.len() >= 2 {
                    if let (Expr::Ident { name: dest_name, .. }, Expr::Ident { name: src_name, .. }) = (&args[0], &args[1]) {
                        let dest_off = self.var_offset(&dest_name.name);
                        let src_off = self.var_offset(&src_name.name);
                        self.load_stack(dst, src_off);
                        self.store_stack(dst, dest_off);
                    }
                }
                true
            }
            // __builtin_bswap16/32/64: 字节序反转
            "__builtin_bswap16" | "__builtin_bswap32" | "__builtin_bswap64" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                self.gen_expr(&args[0], dst);
                if name == "__builtin_bswap64" {
                    // 64 位: REX.W bswap dst
                    let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                    self.emit(&[rex, 0x0F, 0xC8 | (dst & 7)]);
                } else {
                    // 32/16 位: 无 REX.W, 操作 32 位寄存器 (高 32 位自动清零)
                    self.emit(&[0x0F, 0xC8 | (dst & 7)]);
                    if name == "__builtin_bswap16" {
                        // bswap16: 结果在低 16 位，用 movzx 扩展到 64 位
                        let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                        self.emit(&[rex, 0x0F, 0xB7, 0xC0 | ((dst & 7) << 3) | (dst & 7)]); // movzx dst, dst(16)
                    }
                }
                true
            }
            // __builtin_clz/clzl/clzll: 前导零计数
            "__builtin_clz" | "__builtin_clzl" | "__builtin_clzll" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                self.gen_expr(&args[0], dst);
                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                // 根据参数类型大小确定位宽
                let bits = args[0].ty().size() as i32 * 8;
                // bsr dst, dst → dst = 最高位位置
                self.emit(&[rex, 0x0F, 0xBD, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                // clz = (bits - 1) - dst
                self.emit(&[rex, 0xF7, 0xD8 | (dst & 7)]); // neg dst
                self.emit(&[rex, 0x83, 0xC0 | ((dst & 7) << 3) | (dst & 7), (bits - 1) as u8]); // add dst, bits-1
                true
            }
            // __builtin_ctz/ctzl/ctzll: 尾随零计数
            "__builtin_ctz" | "__builtin_ctzl" | "__builtin_ctzll" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                self.gen_expr(&args[0], dst);
                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                // bsf dst, dst → dst = 最低位位置
                self.emit(&[rex, 0x0F, 0xBC, 0xC0 | ((dst & 7) << 3) | (dst & 7)]); // bsf dst, dst
                true
            }
            // __builtin_popcount/popcountl/popcountll: 位计数
            "__builtin_popcount" | "__builtin_popcountl" | "__builtin_popcountll" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                self.gen_expr(&args[0], dst);
                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                // popcnt dst, dst: F3 0F B8 /r
                self.emit(&[rex, 0xF3, 0x0F, 0xB8, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                true
            }
            // __builtin_parity/parityl/parityll: 奇偶校验 (popcount & 1)
            "__builtin_parity" | "__builtin_parityl" | "__builtin_parityll" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                self.gen_expr(&args[0], dst);
                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                // popcnt dst, dst
                self.emit(&[rex, 0xF3, 0x0F, 0xB8, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                // and dst, 1
                self.emit(&[rex, 0x83, 0xE0 | ((dst & 7) << 3) | (dst & 7), 1]);
                true
            }
            // __builtin_bitreverse8/16/32/64: 位反转 (转发到 libcall)
            "__builtin_bitreverse8" | "__builtin_bitreverse16"
            | "__builtin_bitreverse32" | "__builtin_bitreverse64" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                // 简化实现: 转发到 __dcc_bitreverse libcall
                self.gen_expr(&args[0], 7);
                self.emit(&[0x54]); // push rsp
                self.call_label("__dcc_bitreverse");
                self.emit(&[0x5c]); // pop rsp
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_frame_address(level): 帧指针
            "__builtin_frame_address" => {
                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                // mov dst, rbp: REX.W 8B /r (rm=5=rbp)
                self.emit(&[rex, 0x8B, 0xC0 | ((dst & 7) << 3) | 5]);
                // N>0: 沿帧指针链回溯 N 次: dst = [dst]
                let level = if let Some(Expr::Int { v, .. }) = args.first() { *v as i32 } else { 0 };
                for _ in 0..level {
                    // mov dst, [dst]: REX.W 8B /r
                    self.emit(&[rex, 0x8B, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                }
                true
            }
            // __builtin_return_address(level): 返回地址
            "__builtin_return_address" => {
                // 简化: level=0 时返回 [rsp] (函数调用后栈顶是返回地址)
                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                self.emit(&[rex, 0x8B, 0x04, 0x24]); // mov dst, [rsp]
                true
            }
            // __builtin_overflow_p: 溢出检测 (简化为 0)
            "__builtin_overflow_p" => {
                self.mov_imm(dst, 0);
                true
            }
            // P2-13: __builtin_apply/__builtin_return 存根 (返回 0)
            "__builtin_apply" | "__builtin_return" => {
                self.mov_imm(dst, 0);
                true
            }
            // __builtin_add_overflow(a, b, &result): 溢出检测
            // 策略: 计算 64 位结果, 截断到操作数类型, 比较检测溢出
            "__builtin_add_overflow" | "__builtin_sub_overflow" | "__builtin_mul_overflow" => {
                if args.len() < 3 { self.mov_imm(dst, 0); return true; }
                let op_size = args[0].ty().size().max(args[1].ty().size()).max(4); // 至少 32 位
                // 1. 求值 ptr → r10
                self.gen_expr(&args[2], 0);
                self.emit(&[0x49, 0x89, 0xc2]); // mov r10, rax
                // 2. 求值 a → push
                self.gen_expr(&args[0], 0);
                self.push(0);
                // 3. 求值 b → rax
                self.gen_expr(&args[1], 0);
                // 4. pop a → rcx
                self.pop(1);
                // rax = b, rcx = a, r10 = ptr
                // 5. 计算 64 位结果
                match name {
                    "__builtin_add_overflow" => {
                        self.emit(&[0x48, 0x01, 0xc8]); // add rax, rcx
                    }
                    "__builtin_sub_overflow" => {
                        self.emit(&[0x48, 0x89, 0xc2]); // mov rdx, rax
                        self.emit(&[0x48, 0x89, 0xc8]); // mov rax, rcx
                        self.emit(&[0x48, 0x29, 0xd0]); // sub rax, rdx
                    }
                    "__builtin_mul_overflow" => {
                        self.emit(&[0x48, 0x0f, 0xaf, 0xc1]); // imul rax, rcx
                    }
                    _ => unreachable!(),
                }
                // rax = 64 位结果
                // 6. 截断到操作数类型, 检测溢出
                // 保存完整结果到 rcx
                self.emit(&[0x48, 0x89, 0xc1]); // mov rcx, rax
                match op_size {
                    4 => {
                        // 32 位: 截断到 eax, 符号扩展到 rdx
                        self.emit(&[0x89, 0xc0]); // mov eax, eax (截断到 32 位)
                        self.emit(&[0x48, 0x63, 0xd0]); // movsxd rdx, eax (符号扩展)
                    }
                    2 => {
                        // 16 位: movsx rdx, ax
                        self.emit(&[0x48, 0x0f, 0xbf, 0xd0]); // movsx rdx, ax
                    }
                    1 => {
                        // 8 位: movsx rdx, al
                        self.emit(&[0x48, 0x0f, 0xbe, 0xd0]); // movsx rdx, al
                    }
                    _ => {
                        // 64 位: 无截断, 用 seto
                        self.emit(&[0x0f, 0x90, 0xc2]); // seto dl
                        self.emit(&[0x49, 0x89, 0x02]); // mov [r10], rax
                        self.emit(&[0x0f, 0xb6, 0xc2]); // movzx eax, dl
                        if dst != 0 { self.mov(dst, 0); }
                        return true;
                    }
                }
                // rdx = 截断后符号扩展的结果, rcx = 64 位结果
                // 比较 rdx 和 rcx: 如果不等则溢出
                self.emit(&[0x48, 0x39, 0xd1]); // cmp rcx, rdx
                self.emit(&[0x0f, 0x94, 0xc2]); // sete dl (相等=无溢出=1)
                self.emit(&[0x80, 0xf2, 0x01]); // xor dl, 1 (反转: 溢出=1)
                // 7. 存储截断结果到 *ptr
                match op_size {
                    4 => { self.emit(&[0x49, 0x89, 0x02]); } // mov [r10], eax
                    2 => { self.emit(&[0x49, 0x89, 0x02]); } // mov [r10], eax (低 16 位)
                    1 => { self.emit(&[0x49, 0x89, 0x02]); } // mov [r10], eax (低 8 位)
                    _ => { self.emit(&[0x49, 0x89, 0x02]); } // mov [r10], rax
                }
                // 8. dst = 溢出标志
                self.emit(&[0x0f, 0xb6, 0xc2]); // movzx eax, dl
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            _ => false,
        }
    }

    /// 计算结构体成员偏移 (用于 __builtin_offsetof)
    fn compute_offset(ty: &Type, member: &str) -> usize {
        let fields = match ty {
            Type::Struct(_, fields) => fields,
            Type::Union(_, _fields) => {
                // union: 所有成员偏移都是 0
                return 0;
            }
            _ => return 0,
        };
        let parts: Vec<&str> = member.split('.').collect();
        let mut offset = 0usize;
        let mut current_fields = fields.as_ref();
        for (i, part) in parts.iter().enumerate() {
            let part = if part.contains('[') { &part[..part.find('[').unwrap()] } else { part };
            let mut found = false;
            for field in current_fields.iter() {
                if field.name == *part {
                    offset += field.offset;
                    if i < parts.len() - 1 {
                        // 嵌套: 进入子结构
                        match &field.ty {
                            Type::Struct(_, sub_fields) => {
                                current_fields = sub_fields.as_ref();
                            }
                            _ => {}
                        }
                    }
                    found = true;
                    break;
                }
            }
            if !found { break; }
        }
        offset
    }

    /// 获取指针参数所指元素的大小
    fn atomic_elem_size(&self, args: &[Expr], idx: usize) -> usize {
        if idx >= args.len() { return 8; }
        match args[idx].ty() {
            Type::Ptr(ref inner) => inner.size(),
            Type::Array(ref inner, _) => inner.size(),
            _ => 8,
        }
    }

    /// 原子内置函数 codegen: 返回 true 表示已处理
    fn gen_atomic_builtin(&mut self, name: &str, args: &[Expr], dst: u8) -> bool {
        match name {
            "__atomic_fetch_add" => {
                // args: (ptr, val, order)
                if args.len() < 3 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], 6); // rsi = val
                // lock xadd [rdi], rsi (按 sz 选操作数尺寸)
                self.emit_atomic_xadd(sz);
                if dst != 6 { self.mov(dst, 6); }
                true
            }
            "__atomic_fetch_sub" => {
                if args.len() < 3 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], 6); // rsi = val
                // neg rsi (按 sz 选操作数尺寸)
                self.emit_neg(sz);
                self.emit_atomic_xadd(sz);
                if dst != 6 { self.mov(dst, 6); }
                true
            }
            "__atomic_fetch_and" => {
                if args.len() < 3 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7);
                self.gen_expr(&args[1], 6);
                self.emit_cas_loop(sz, |cg| {
                    cg.emit_and_rsi();
                });
                if dst != 1 { self.mov(dst, 1); }
                true
            }
            "__atomic_fetch_or" => {
                if args.len() < 3 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7);
                self.gen_expr(&args[1], 6);
                self.emit_cas_loop(sz, |cg| {
                    cg.emit_or_rsi();
                });
                if dst != 1 { self.mov(dst, 1); }
                true
            }
            "__atomic_fetch_xor" => {
                if args.len() < 3 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7);
                self.gen_expr(&args[1], 6);
                self.emit_cas_loop(sz, |cg| {
                    cg.emit_xor_rsi();
                });
                if dst != 1 { self.mov(dst, 1); }
                true
            }
            "__atomic_compare_exchange_n" => {
                // args: (ptr, expected, desired, weak, success_order, failure_order)
                if args.len() < 3 { return false; }
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], 0); // rax = expected (cmpxchg 用 rax)
                // 保存 expected 到帧临时槽, 因为还需要写入
                let slot = -8 - (self.expr_depth as i32 * 8);
                {
                    let rex = 0x49u8;
                    self.emit(&[rex, 0x89]);
                    if slot.abs() < 128 { self.emit(&[0x45, slot as i8 as u8]); }
                    else { self.emit(&[0x85]); self.emit(&slot.to_le_bytes()); }
                }
                // 解引用 expected 指针, rax = *expected (实际期望值)
                self.emit(&[0x48, 0x8B, 0x00]); // mov rax, [rax]
                self.gen_expr(&args[2], 6); // rsi = desired
                // lock cmpxchg [rdi], rsi: 按 sz 选尺寸
                let sz = self.atomic_elem_size(args, 0);
                if sz == 8 {
                    self.emit(&[0xF0, 0x48, 0x0F, 0xB1, 0x37]);
                } else {
                    self.emit(&[0xF0, 0x0F, 0xB1, 0x37]);
                }
                // ZF=1 成功(rax=old), ZF=0 失败(rax=old)
                self.emit(&[0x0F, 0x94, 0xC2]); // sete dl
                let l_skip = self.fresh_label();
                self.jmp_rel32(&l_skip, JumpKind::Je);
                // 失败: 将 rax (当前值) 写入 [expected_ptr]
                // 把 expected 指针加载到 rsi (而非 rax, rax 保留当前值)
                {
                    let rex = 0x49u8;
                    self.emit(&[rex, 0x8B]);
                    if slot.abs() < 128 {
                        self.emit(&[0x75, slot as i8 as u8]); // modrm: [r13+disp8], reg=rsi
                    } else {
                        self.emit(&[0xB5]); // modrm: [r13+disp32], reg=rsi
                        self.emit(&slot.to_le_bytes());
                    }
                }
                self.emit(&[0x48, 0x89, 0x06]); // mov [rsi], rax
                self.label(&l_skip);
                self.emit(&[0x48, 0x0F, 0xB6, 0xC2]); // movzx rax, dl
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            "__atomic_store_n" => {
                if args.len() < 3 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], 6); // rsi = val
                self.emit_mov_to_mem(sz);   // mov [rdi], rsi (按 sz 选尺寸)
                self.emit(&[0x0F, 0xAE, 0xF0]); // mfence
                true
            }
            "__atomic_load_n" => {
                if args.len() < 2 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.emit_mov_from_mem(sz, dst); // mov reg, [rdi] (按 sz 选尺寸)
                true
            }
            "__atomic_exchange_n" => {
                if args.len() < 3 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], 6); // rsi = val
                // lock xchg [rdi], rsi: 按 sz 选尺寸
                if sz == 8 {
                    self.emit(&[0xF0, 0x48, 0x87, 0x37]);
                } else {
                    self.emit(&[0xF0, 0x87, 0x37]);
                }
                if dst != 6 { self.mov(dst, 6); }
                true
            }
            _ => false, // 未识别的 __atomic_ 函数, 走普通调用路径
        }
    }

    // ===== 原子操作辅助指令 =====

    /// lock xadd [rdi], rsi — 按 sz 选 32/64 位
    fn emit_atomic_xadd(&mut self, sz: usize) {
        match sz {
            4 => self.emit(&[0xF0, 0x0F, 0xC1, 0x37]),        // lock xadd [rdi], esi
            _ => self.emit(&[0xF0, 0x48, 0x0F, 0xC1, 0x37]),  // lock xadd [rdi], rsi
        }
    }

    /// neg rsi — 按 sz 选 32/64 位
    fn emit_neg(&mut self, sz: usize) {
        match sz {
            4 => self.emit(&[0xF7, 0xDE]),        // neg esi
            _ => self.emit(&[0x48, 0xF7, 0xDE]),  // neg rsi
        }
    }

    /// mov [rdi], rsi — 按 sz 选操作数尺寸
    fn emit_mov_to_mem(&mut self, sz: usize) {
        match sz {
            1 => self.emit(&[0x40, 0x88, 0x37]),  // mov [rdi], sil
            2 => self.emit(&[0x66, 0x89, 0x37]),  // mov [rdi], si
            4 => self.emit(&[0x89, 0x37]),         // mov [rdi], esi
            _ => self.emit(&[0x48, 0x89, 0x37]),  // mov [rdi], rsi
        }
    }

    /// mov reg, [rdi] — 按 sz 选操作数尺寸, 结果放入 dst
    fn emit_mov_from_mem(&mut self, sz: usize, dst: u8) {
        match sz {
            1 => self.emit(&[0x0F, 0xB6, 0x07]),  // movzx eax, byte [rdi]
            2 => self.emit(&[0x0F, 0xB7, 0x07]),  // movzx eax, word [rdi]
            4 => self.emit(&[0x8B, 0x07]),         // mov eax, [rdi] (zero-extends)
            _ => self.emit(&[0x48, 0x8B, 0x07]),  // mov rax, [rdi]
        }
        if dst != 0 { self.mov(dst, 0); }
    }

    /// CAS 循环: 读旧值, 计算新值, lock cmpxchg, 循环直到成功
    fn emit_cas_loop<F: Fn(&mut Self)>(&mut self, sz: usize, compute_new: F) {
        if sz == 8 {
            // 64-bit
            self.emit(&[0x48, 0x8B, 0x07]); // mov rax, [rdi]
            let l_retry = self.fresh_label();
            self.label(&l_retry);
            self.emit(&[0x48, 0x89, 0xC1]); // mov rcx, rax (save old)
            self.emit(&[0x48, 0x89, 0xC2]); // mov rdx, rax
            compute_new(self);                 // and/or/xor rdx, rsi
            self.emit(&[0xF0, 0x48, 0x0F, 0xB1, 0x17]); // lock cmpxchg [rdi], rdx
            let l_done = self.fresh_label();
            self.jmp_rel32(&l_done, JumpKind::Je);
            self.jmp_rel32(&l_retry, JumpKind::Jmp);
            self.label(&l_done);
            // After successful cmpxchg: RAX = old value, RCX = old value (from mov rcx,rax before cmpxchg)
        } else {
            // 32-bit (zero-extends to 64-bit)
            self.emit(&[0x8B, 0x07]); // mov eax, [rdi]
            let l_retry = self.fresh_label();
            self.label(&l_retry);
            self.emit(&[0x89, 0xC1]); // mov ecx, eax
            self.emit(&[0x89, 0xC2]); // mov edx, eax
            compute_new(self);
            self.emit(&[0xF0, 0x0F, 0xB1, 0x17]); // lock cmpxchg [rdi], edx
            let l_done = self.fresh_label();
            self.jmp_rel32(&l_done, JumpKind::Je);
            self.jmp_rel32(&l_retry, JumpKind::Jmp);
            self.label(&l_done);
        }
    }

    /// and rdx, rsi (32-bit: 21 F2, 64-bit: 48 21 F2)
    /// 由 emit_cas_loop 的 compute_new 回调使用
    fn emit_and_rsi(&mut self) {
        // 32-bit and (rdx = rdx & rsi)
        self.emit(&[0x21, 0xF2]); // and edx, esi
    }
    fn emit_or_rsi(&mut self) {
        self.emit(&[0x09, 0xF2]); // or edx, esi
    }
    fn emit_xor_rsi(&mut self) {
        self.emit(&[0x31, 0xF2]); // xor edx, esi
    }

    // ===== Inline Asm Mini-Assembler (AT&T syntax) =====

    /// 构建 asm 操作数映射: %N → 寄存器名
    fn build_asm_operand_map(&mut self, outputs: &[Expr], inputs: &[Expr]) -> HashMap<String, String> {
        let mut map = HashMap::new();
        // SysV 调用约定可用寄存器 (caller-saved 优先)
        let alloc_regs = ["rax", "rcx", "rdx", "rsi", "rdi", "r8", "r9", "r10", "r11"];
        let mut idx = 0usize;
        // 输出操作数: 分配到具体寄存器
        for _op in outputs.iter() {
            if idx < alloc_regs.len() {
                map.insert(format!("%{}", idx), alloc_regs[idx].to_string());
                idx += 1;
            }
        }
        // 输入操作数
        for _op in inputs.iter() {
            if idx < alloc_regs.len() {
                map.insert(format!("%{}", idx), alloc_regs[idx].to_string());
                idx += 1;
            }
        }
        map
    }

    /// 发射内联汇编模板
    fn emit_inline_asm(&mut self, asm_str: &str, op_map: &HashMap<String, String>) {
        // 清理字符串: 去除引号包裹
        let s = if asm_str.starts_with('"') && asm_str.ends_with('"') && asm_str.len() >= 2 {
            &asm_str[1..asm_str.len()-1]
        } else { asm_str };
        // 按换行和分号分割指令
        for line in s.split(|c| c == '\n' || c == ';') {
            let line = line.trim();
            if line.is_empty() { continue; }
            // 替换操作数引用 %N → 寄存器名
            let mut resolved = line.to_string();
            // 按数字降序替换, 避免 %1 匹配 %10 的前缀
            let mut keys: Vec<_> = op_map.keys().collect();
            keys.sort_by(|a, b| {
                let na: usize = a[1..].parse().unwrap_or(0);
                let nb: usize = b[1..].parse().unwrap_or(0);
                nb.cmp(&na)
            });
            for k in keys {
                if let Some(reg) = op_map.get(k) {
                    resolved = resolved.replace(k, reg);
                }
            }
            // 处理标签引用: %lN → 标签地址 (asm goto)
            // 简化: 跳过 %lN 替换
            self.emit_asm_line(&resolved);
        }
    }

    /// 解析 AT&T 寄存器名 → (reg_index, size_bits)
    fn parse_asm_reg(name: &str) -> Option<(u8, u8)> {
        let n = name.strip_prefix('%').unwrap_or(name);
        match n {
            "rax" => Some((0, 64)), "eax" => Some((0, 32)), "ax" => Some((0, 16)), "al" => Some((0, 8)), "ah" => Some((0, 8)),
            "rbx" => Some((3, 64)), "ebx" => Some((3, 32)), "bx" => Some((3, 16)), "bl" => Some((3, 8)), "bh" => Some((3, 8)),
            "rcx" => Some((1, 64)), "ecx" => Some((1, 32)), "cx" => Some((1, 16)), "cl" => Some((1, 8)), "ch" => Some((1, 8)),
            "rdx" => Some((2, 64)), "edx" => Some((2, 32)), "dx" => Some((2, 16)), "dl" => Some((2, 8)), "dh" => Some((2, 8)),
            "rsi" => Some((6, 64)), "esi" => Some((6, 32)), "si" => Some((6, 16)), "sil" => Some((6, 8)),
            "rdi" => Some((7, 64)), "edi" => Some((7, 32)), "di" => Some((7, 16)), "dil" => Some((7, 8)),
            "rsp" => Some((4, 64)), "esp" => Some((4, 32)), "sp" => Some((4, 16)), "spl" => Some((4, 8)),
            "rbp" => Some((5, 64)), "ebp" => Some((5, 32)), "bp" => Some((5, 16)), "bpl" => Some((5, 8)),
            "r8" => Some((8, 64)), "r8d" => Some((8, 32)), "r8w" => Some((8, 16)), "r8b" => Some((8, 8)),
            "r9" => Some((9, 64)), "r9d" => Some((9, 32)), "r9w" => Some((9, 16)), "r9b" => Some((9, 8)),
            "r10" => Some((10, 64)), "r10d" => Some((10, 32)), "r10w" => Some((10, 16)), "r10b" => Some((10, 8)),
            "r11" => Some((11, 64)), "r11d" => Some((11, 32)), "r11w" => Some((11, 16)), "r11b" => Some((11, 8)),
            "r12" => Some((12, 64)), "r12d" => Some((12, 32)), "r12w" => Some((12, 16)), "r12b" => Some((12, 8)),
            "r13" => Some((13, 64)), "r13d" => Some((13, 32)), "r13w" => Some((13, 16)), "r13b" => Some((13, 8)),
            "r14" => Some((14, 64)), "r14d" => Some((14, 32)), "r14w" => Some((14, 16)), "r14b" => Some((14, 8)),
            "r15" => Some((15, 64)), "r15d" => Some((15, 32)), "r15w" => Some((15, 16)), "r15b" => Some((15, 8)),
            _ => None,
        }
    }

    /// 解析 AT&T 操作数 → AsmOperand
    fn parse_asm_operand_str(s: &str) -> AsmOperand {
        let s = s.trim();
        if s.is_empty() { return AsmOperand::None; }
        // 立即数: $N
        if let Some(rest) = s.strip_prefix('$') {
            let rest = rest.trim();
            if let Some(hex) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
                if let Ok(v) = i64::from_str_radix(hex, 16) { return AsmOperand::Imm(v); }
            } else if let Ok(v) = rest.parse::<i64>() {
                return AsmOperand::Imm(v);
            }
            return AsmOperand::None;
        }
        // 寄存器: %reg
        if s.starts_with('%') {
            if let Some((idx, sz)) = Self::parse_asm_reg(s) {
                return AsmOperand::Reg(idx, sz);
            }
            // 可能是标签引用 %label 或 %lN, 跳过
            return AsmOperand::None;
        }
        // 内存: disp(%base,%idx,scale) 或 (%reg) 或 symbol(%rip)
        if s.contains('(') {
            return Self::parse_asm_mem(s);
        }
        // 符号名 (全局标签引用)
        AsmOperand::Sym(s.to_string())
    }

    /// 解析 AT&T 内存操作数
    fn parse_asm_mem(s: &str) -> AsmOperand {
        // 格式: [disp](%base[,%idx[,scale]])
        let paren_pos = s.find('(').unwrap_or(s.len());
        let disp_str = s[..paren_pos].trim();
        let rest = &s[paren_pos..];
        let mut disp: i32 = 0;
        if !disp_str.is_empty() {
            if let Some(hex) = disp_str.strip_prefix("0x").or_else(|| disp_str.strip_prefix("0X")) {
                disp = i32::from_str_radix(hex, 16).unwrap_or(0);
            } else {
                disp = disp_str.parse::<i32>().unwrap_or(0);
            }
        }
        // 提取括号内的内容
        let inner = if rest.starts_with('(') && rest.ends_with(')') {
            &rest[1..rest.len()-1]
        } else { return AsmOperand::None; };
        let parts: Vec<&str> = inner.split(',').collect();
        let base_str = parts.first().map(|s| s.trim()).unwrap_or("");
        let (base, _bsz) = Self::parse_asm_reg(base_str).unwrap_or((5, 64)); // 默认 rbp
        let mut idx: Option<u8> = None;
        let mut scale: u8 = 1;
        if parts.len() > 1 {
            let idx_str = parts[1].trim();
            if !idx_str.is_empty() {
                idx = Self::parse_asm_reg(idx_str).map(|(r, _)| r);
            }
        }
        if parts.len() > 2 {
            scale = parts[2].trim().parse::<u8>().unwrap_or(1);
        }
        AsmOperand::Mem { disp, base, index: idx, scale }
    }

    /// 发射单条 AT&T 汇编指令
    fn emit_asm_line(&mut self, line: &str) {
        // 处理前缀: lock, rep, repz, repnz
        let (prefix, rest) = if line.starts_with("lock ") || line.starts_with("lock\t") {
            (Some(0xF0u8), line[5..].trim())
        } else if line.starts_with("rep ") || line.starts_with("rep\t") || line.starts_with("repz ") || line.starts_with("repz\t") {
            (Some(0xF3u8), line[4..].trim())
        } else if line.starts_with("repnz ") || line.starts_with("repnz\t") || line.starts_with("repe ") {
            (Some(0xF2u8), line[6..].trim())
        } else {
            (None, line)
        };
        // 分割助记符和操作数
        let (mnemonic, operands_str) = if let Some(pos) = rest.find(|c: char| c == ' ' || c == '\t') {
            (&rest[..pos], rest[pos..].trim())
        } else {
            (rest, "")
        };
        let operands: Vec<&str> = if operands_str.is_empty() { vec![] }
        else { operands_str.split(',').collect() };
        let ops: Vec<AsmOperand> = operands.iter().map(|s| Self::parse_asm_operand_str(s)).collect();
        if let Some(pfx) = prefix { self.emit(&[pfx]); }
        self.emit_asm_instr(mnemonic, &ops);
    }

    /// 编码单条指令
    fn emit_asm_instr(&mut self, mnemonic: &str, ops: &[AsmOperand]) {
        match mnemonic {
            // 无操作数指令
            "nop" => self.emit(&[0x90]),
            "ret" | "retq" => self.emit(&[0xC3]),
            "leave" | "leaveq" => self.emit(&[0xC9]),
            "cdq" => self.emit(&[0x99]),
            "cdqe" => self.emit(&[0x48, 0x98]),
            "cqo" => self.emit(&[0x48, 0x99]),
            "cbtw" => self.emit(&[0x66, 0x98]),
            "cwtl" => self.emit(&[0x98]),
            "cltd" => self.emit(&[0x99]),
            "clac" => self.emit(&[0x0F, 0x01, 0xCA]),
            "stac" => self.emit(&[0x0F, 0x01, 0xCB]),
            "cli" => self.emit(&[0xFA]),
            "sti" => self.emit(&[0xFB]),
            "cld" => self.emit(&[0xFC]),
            "std" => self.emit(&[0xFD]),
            "hlt" => self.emit(&[0xF4]),
            "pause" => self.emit(&[0xF3, 0x90]),
            "mfence" => self.emit(&[0x0F, 0xAE, 0xF0]),
            "lfence" => self.emit(&[0x0F, 0xAE, 0xE8]),
            "sfence" => self.emit(&[0x0F, 0xAE, 0xF8]),
            "syscall" => self.emit(&[0x0F, 0x05]),
            "sysenter" => self.emit(&[0x0F, 0x34]),
            "sysexit" => self.emit(&[0x0F, 0x35]),
            "rdtsc" => self.emit(&[0x0F, 0x31]),
            "rdpmc" => self.emit(&[0x0F, 0x33]),
            "xchg%eax,%eax" | "xchg %eax,%eax" => self.emit(&[0x90]), // nop alias
            "ud2" => self.emit(&[0x0F, 0x0B]),
            "clc" => self.emit(&[0xF8]),
            "stc" => self.emit(&[0xF9]),
            "cmc" => self.emit(&[0xF5]),
            "sahf" => self.emit(&[0x9E]),
            "lahf" => self.emit(&[0x9F]),

            // cpuid
            "cpuid" => self.emit(&[0x0F, 0xA2]),

            // 字符串操作
            "movsb" => self.emit(&[0xA4]),
            "movsw" => self.emit(&[0x66, 0xA5]),
            "movsl" | "movsd" => self.emit(&[0xA5]),
            "movsq" => self.emit(&[0x48, 0xA5]),
            "stosb" => self.emit(&[0xAA]),
            "stosw" => self.emit(&[0x66, 0xAB]),
            "stosl" | "stosd" => self.emit(&[0xAB]),
            "stosq" => self.emit(&[0x48, 0xAB]),
            "scasb" => self.emit(&[0xAE]),
            "cmpsb" => self.emit(&[0xA6]),

            // push/pop 寄存器
            "push" if ops.len() == 1 => self.emit_asm_push(&ops[0]),
            "pop" if ops.len() == 1 => self.emit_asm_pop(&ops[0]),

            // 数据移动
            "mov" | "movq" if ops.len() == 2 => self.emit_asm_mov(&ops[0], &ops[1], 64),
            "movl" if ops.len() == 2 => self.emit_asm_mov(&ops[0], &ops[1], 32),
            "movw" if ops.len() == 2 => self.emit_asm_mov(&ops[0], &ops[1], 16),
            "movb" if ops.len() == 2 => self.emit_asm_mov(&ops[0], &ops[1], 8),
            "movzbl" if ops.len() == 2 => self.emit_asm_movzx(&ops[0], &ops[1], 8, 32),
            "movzbq" if ops.len() == 2 => self.emit_asm_movzx(&ops[0], &ops[1], 8, 64),
            "movzwl" if ops.len() == 2 => self.emit_asm_movzx(&ops[0], &ops[1], 16, 32),
            "movzwq" if ops.len() == 2 => self.emit_asm_movzx(&ops[0], &ops[1], 16, 64),
            "movslq" if ops.len() == 2 => self.emit_asm_movsx(&ops[0], &ops[1], 32, 64),
            "movsbl" if ops.len() == 2 => self.emit_asm_movsx(&ops[0], &ops[1], 8, 32),
            "movsbq" if ops.len() == 2 => self.emit_asm_movsx(&ops[0], &ops[1], 8, 64),
            "movswl" if ops.len() == 2 => self.emit_asm_movsx(&ops[0], &ops[1], 16, 32),
            "lea" | "leaq" if ops.len() == 2 => self.emit_asm_lea(&ops[0], &ops[1], 64),
            "leal" if ops.len() == 2 => self.emit_asm_lea(&ops[0], &ops[1], 32),
            "xchg" if ops.len() == 2 => self.emit_asm_xchg(&ops[0], &ops[1]),
            "cmovz" | "cmovze" | "cmove" if ops.len() == 2 => self.emit_asm_cmov(&ops[0], &ops[1], 0x44),
            "cmovnz" | "cmovne" if ops.len() == 2 => self.emit_asm_cmov(&ops[0], &ops[1], 0x45),
            "cmovl" | "cmovnge" if ops.len() == 2 => self.emit_asm_cmov(&ops[0], &ops[1], 0x4C),
            "cmovg" | "cmovnle" if ops.len() == 2 => self.emit_asm_cmov(&ops[0], &ops[1], 0x4F),
            "cmovle" | "cmovng" if ops.len() == 2 => self.emit_asm_cmov(&ops[0], &ops[1], 0x4E),
            "cmovge" | "cmovnl" if ops.len() == 2 => self.emit_asm_cmov(&ops[0], &ops[1], 0x4D),

            // 算术
            "add" | "addq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 0, 64),
            "addl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 0, 32),
            "addw" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 0, 16),
            "addb" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 0, 8),
            "sub" | "subq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 5, 64),
            "subl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 5, 32),
            "subw" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 5, 16),
            "subb" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 5, 8),
            "and" | "andq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 4, 64),
            "andl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 4, 32),
            "or" | "orq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 1, 64),
            "orl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 1, 32),
            "xor" | "xorq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 6, 64),
            "xorl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 6, 32),
            "cmp" | "cmpq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 7, 64),
            "cmpl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 7, 32),
            "cmpw" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 7, 16),
            "cmpb" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 7, 8),
            "test" | "testq" if ops.len() == 2 => self.emit_asm_test(&ops[0], &ops[1], 64),
            "testl" if ops.len() == 2 => self.emit_asm_test(&ops[0], &ops[1], 32),
            "testb" if ops.len() == 2 => self.emit_asm_test(&ops[0], &ops[1], 8),
            "imul" if ops.len() == 2 => self.emit_asm_imul2(&ops[0], &ops[1]),
            "imul" if ops.len() == 3 => self.emit_asm_imul3(&ops[0], &ops[1], &ops[2]),
            "mul" | "mulq" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 4, 64),
            "div" | "divq" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 6, 64),
            "idiv" | "idivq" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 7, 64),
            "neg" | "negq" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 3, 64),
            "not" | "notq" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 2, 64),
            "inc" | "incq" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 0, 64),
            "dec" | "decq" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 1, 64),

            // 移位
            "shl" | "sal" | "shlq" if ops.len() == 2 => self.emit_asm_shift(&ops[0], &ops[1], 4, 64),
            "shll" | "sall" if ops.len() == 2 => self.emit_asm_shift(&ops[0], &ops[1], 4, 32),
            "shr" | "shrq" if ops.len() == 2 => self.emit_asm_shift(&ops[0], &ops[1], 5, 64),
            "shrl" if ops.len() == 2 => self.emit_asm_shift(&ops[0], &ops[1], 5, 32),
            "sar" | "sarq" if ops.len() == 2 => self.emit_asm_shift(&ops[0], &ops[1], 7, 64),
            "sarl" if ops.len() == 2 => self.emit_asm_shift(&ops[0], &ops[1], 7, 32),

            // 跳转
            "jmp" if ops.len() == 1 => self.emit_asm_jmp(&ops[0]),
            "je" | "jz" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x84),
            "jne" | "jnz" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x85),
            "jl" | "jnge" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x8C),
            "jle" | "jng" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x8E),
            "jg" | "jnle" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x8F),
            "jge" | "jnl" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x8D),
            "ja" | "jnbe" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x87),
            "jae" | "jnb" | "jnc" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x83),
            "jb" | "jnae" | "jc" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x82),
            "jbe" | "jna" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x86),
            "js" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x88),
            "jns" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x89),
            "jo" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x80),
            "jno" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x81),

            // 调用
            "call" if ops.len() == 1 => self.emit_asm_call(&ops[0]),

            // 设置字节 (setcc)
            "sete" | "setz" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x94),
            "setne" | "setnz" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x95),
            "setl" | "setnge" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x9C),
            "setg" | "setnle" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x9F),
            "setle" | "setng" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x9E),
            "setge" | "setnl" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x9D),
            "setb" | "setc" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x92),
            "seta" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x97),

            // 原子操作
            "cmpxchg" | "cmpxchgl" if ops.len() == 2 => self.emit_asm_cmpxchg(&ops[0], &ops[1]),
            "xadd" | "xaddl" if ops.len() == 2 => self.emit_asm_xadd(&ops[0], &ops[1]),

            // 特殊寄存器
            "rdmsr" => self.emit(&[0x0F, 0x32]),
            "wrmsr" => self.emit(&[0x0F, 0x30]),
            "movq" if ops.len() == 2 && mnemonic == "movq" => self.emit_asm_mov(&ops[0], &ops[1], 64),

            // 未识别指令: 发射 nop 占位 (避免编译失败)
            _ => { self.emit(&[0x90]); }
        }
    }

    // --- 辅助编码方法 ---

    fn rex(w: bool, r: u8, x: u8, b: u8) -> u8 {
        let mut v = 0x40u8;
        if w { v |= 0x08; }
        if r >= 8 { v |= 0x04; }
        if x >= 8 { v |= 0x02; }
        if b >= 8 { v |= 0x01; }
        v
    }

    fn modrm_reg_reg(reg: u8, rm: u8) -> u8 {
        0xC0 | ((reg & 7) << 3) | (rm & 7)
    }

    fn emit_asm_push(&mut self, op: &AsmOperand) {
        match op {
            AsmOperand::Reg(r, _) => {
                if *r >= 8 { self.emit(&[0x41]); }
                self.emit(&[0x50 + (r & 7)]);
            }
            AsmOperand::Imm(v) => {
                if *v >= -128 && *v <= 127 {
                    self.emit(&[0x6A, *v as u8]);
                } else {
                    self.emit(&[0x68]); self.emit(&(*v as i32).to_le_bytes());
                }
            }
            _ => { self.emit(&[0x90]); }
        }
    }

    fn emit_asm_pop(&mut self, op: &AsmOperand) {
        match op {
            AsmOperand::Reg(r, _) => {
                if *r >= 8 { self.emit(&[0x41]); }
                self.emit(&[0x58 + (r & 7)]);
            }
            _ => { self.emit(&[0x90]); }
        }
    }

    fn emit_asm_mov(&mut self, src: &AsmOperand, dst: &AsmOperand, bits: u8) {
        let w = bits == 64;
        match (src, dst) {
            // reg → reg
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                if bits == 8 {
                    // mov r8, r8: 88 /r
                    if *s >= 4 || *d >= 4 { self.emit(&[Self::rex(false, *s, 0, *d)]); }
                    self.emit(&[0x88, Self::modrm_reg_reg(*s, *d)]);
                } else {
                    if bits == 16 { self.emit(&[0x66]); }
                    self.emit(&[Self::rex(w, *s, 0, *d), 0x89, Self::modrm_reg_reg(*s, *d)]);
                }
            }
            // imm → reg
            (AsmOperand::Imm(v), AsmOperand::Reg(d, _)) => {
                if bits == 8 {
                    self.emit(&[0xB0 + (d & 7), *v as u8]);
                } else if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 {
                    if *d >= 8 { self.emit(&[0x41]); }
                    if bits == 16 { self.emit(&[0x66]); }
                    self.emit(&[0xB8 + (d & 7)]); self.emit(&(*v as i32).to_le_bytes());
                } else {
                    // movabs reg, imm64
                    if *d >= 8 { self.emit(&[0x49]); } else { self.emit(&[0x48]); }
                    self.emit(&[0xB8 + (d & 7)]); self.emit(&v.to_le_bytes());
                }
            }
            // mem → reg
            (AsmOperand::Mem { disp, base, index, scale }, AsmOperand::Reg(d, _)) => {
                self.emit_asm_load(*d, *disp, *base, *index, *scale, bits);
            }
            // reg → mem
            (AsmOperand::Reg(s, _), AsmOperand::Mem { disp, base, index, scale }) => {
                self.emit_asm_store(*s, *disp, *base, *index, *scale, bits);
            }
            // imm → mem
            (AsmOperand::Imm(v), AsmOperand::Mem { disp, base, index, scale }) => {
                // mov [mem], imm: C7 /0 id (32-bit) or C6 /0 ib (8-bit)
                if bits == 8 {
                    self.emit_asm_modrm_mem(0xC6, 0, *disp, *base, *index, *scale, false);
                    self.emit(&[*v as u8]);
                } else {
                    if bits != 16 && bits != 8 { self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base)]); }
                    self.emit_asm_modrm_mem(0xC7, 0, *disp, *base, *index, *scale, false);
                    self.emit(&(*v as i32).to_le_bytes());
                }
            }
            _ => { self.emit(&[0x90]); }
        }
    }

    fn emit_asm_load(&mut self, dst: u8, disp: i32, base: u8, index: Option<u8>, scale: u8, bits: u8) {
        let w = bits == 64;
        let has_sib = index.is_some() || (base & 7) == 4; // rsp/r12 需要 SIB
        let need_disp = disp != 0 || (base & 7) == 5; // rbp/r13 强制 disp8
        if bits == 16 { self.emit(&[0x66]); }
        self.emit(&[Self::rex(w, dst, index.unwrap_or(0), base)]);
        self.emit(&[0x8B]);
        let mut modrm = (dst & 7) << 3;
        if !need_disp && disp == 0 { modrm |= 0x00; } // mod=00
        else if disp >= -128 && disp <= 127 { modrm |= 0x40; } // mod=01
        else { modrm |= 0x80; } // mod=10
        if has_sib {
            self.emit(&[modrm | 0x04]); // rm=100 (SIB follows)
            let mut sib = ((base & 7));
            if let Some(idx) = index {
                sib |= ((idx & 7) << 3);
                let sc = match scale { 2 => 1, 4 => 2, 8 => 3, _ => 0 };
                sib |= (sc << 6);
            } else {
                sib |= (4 << 3); // idx=100 (no index)
            }
            self.emit(&[sib]);
        } else {
            self.emit(&[modrm | (base & 7)]);
        }
        if need_disp {
            if disp >= -128 && disp <= 127 { self.emit(&[disp as u8]); }
            else { self.emit(&disp.to_le_bytes()); }
        }
    }

    fn emit_asm_store(&mut self, src: u8, disp: i32, base: u8, index: Option<u8>, scale: u8, bits: u8) {
        let w = bits == 64;
        let has_sib = index.is_some() || (base & 7) == 4;
        let need_disp = disp != 0 || (base & 7) == 5;
        if bits == 16 { self.emit(&[0x66]); }
        self.emit(&[Self::rex(w, src, index.unwrap_or(0), base)]);
        self.emit(&[0x89]);
        let mut modrm = (src & 7) << 3;
        if !need_disp && disp == 0 { modrm |= 0x00; }
        else if disp >= -128 && disp <= 127 { modrm |= 0x40; }
        else { modrm |= 0x80; }
        if has_sib {
            self.emit(&[modrm | 0x04]);
            let mut sib = (base & 7);
            if let Some(idx) = index {
                sib |= ((idx & 7) << 3);
                let sc = match scale { 2 => 1, 4 => 2, 8 => 3, _ => 0 };
                sib |= (sc << 6);
            } else {
                sib |= (4 << 3);
            }
            self.emit(&[sib]);
        } else {
            self.emit(&[modrm | (base & 7)]);
        }
        if need_disp {
            if disp >= -128 && disp <= 127 { self.emit(&[disp as u8]); }
            else { self.emit(&disp.to_le_bytes()); }
        }
    }

    fn emit_asm_modrm_mem(&mut self, opcode: u8, reg: u8, disp: i32, base: u8, index: Option<u8>, scale: u8, need_rex: bool) {
        let has_sib = index.is_some() || (base & 7) == 4;
        let need_disp = disp != 0 || (base & 7) == 5;
        if need_rex { self.emit(&[Self::rex(false, reg, index.unwrap_or(0), base)]); }
        self.emit(&[opcode]);
        let mut modrm = (reg & 7) << 3;
        if !need_disp && disp == 0 { modrm |= 0x00; }
        else if disp >= -128 && disp <= 127 { modrm |= 0x40; }
        else { modrm |= 0x80; }
        if has_sib {
            self.emit(&[modrm | 0x04]);
            let mut sib = (base & 7);
            if let Some(idx) = index { sib |= ((idx & 7) << 3); let sc = match scale { 2=>1,4=>2,8=>3,_=>0 }; sib |= (sc<<6); }
            else { sib |= (4 << 3); }
            self.emit(&[sib]);
        } else {
            self.emit(&[modrm | (base & 7)]);
        }
        if need_disp {
            if disp >= -128 && disp <= 127 { self.emit(&[disp as u8]); }
            else { self.emit(&disp.to_le_bytes()); }
        }
    }

    fn emit_asm_lea(&mut self, src: &AsmOperand, dst: &AsmOperand, bits: u8) {
        if let (AsmOperand::Mem { disp, base, index, scale }, AsmOperand::Reg(d, _)) = (src, dst) {
            let w = bits == 64;
            self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x8D]);
            let has_sib = index.is_some() || (base & 7) == 4;
            let need_disp = *disp != 0 || (base & 7) == 5;
            let mut modrm = (d & 7) << 3;
            if !need_disp && *disp == 0 { modrm |= 0x00; }
            else if *disp >= -128 && *disp <= 127 { modrm |= 0x40; }
            else { modrm |= 0x80; }
            if has_sib {
                self.emit(&[modrm | 0x04]);
                let mut sib = (*base & 7);
                if let Some(idx) = index { sib |= ((idx & 7) << 3); let sc = match scale { 2=>1,4=>2,8=>3,_=>0 }; sib |= (sc<<6); }
                else { sib |= (4 << 3); }
                self.emit(&[sib]);
            } else {
                self.emit(&[modrm | (base & 7)]);
            }
            if need_disp {
                if *disp >= -128 && *disp <= 127 { self.emit(&[*disp as u8]); }
                else { self.emit(&disp.to_le_bytes()); }
            }
        } else { self.emit(&[0x90]); }
    }

    fn emit_asm_alu(&mut self, src: &AsmOperand, dst: &AsmOperand, op: u8, bits: u8) {
        let w = bits == 64;
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                if bits == 8 {
                    if *s >= 4 || *d >= 4 { self.emit(&[Self::rex(false, *s, 0, *d)]); }
                    self.emit(&[0x00 + op * 8, Self::modrm_reg_reg(*s, *d)]);
                } else {
                    if bits == 16 { self.emit(&[0x66]); }
                    self.emit(&[Self::rex(w, *s, 0, *d), 0x01 + op * 8, Self::modrm_reg_reg(*s, *d)]);
                }
            }
            (AsmOperand::Imm(v), AsmOperand::Reg(d, _)) => {
                if bits == 16 { self.emit(&[0x66]); }
                if bits != 8 { self.emit(&[Self::rex(w, 0, 0, *d)]); }
                if *v >= -128 && *v <= 127 {
                    self.emit(&[0x83, Self::modrm_reg_reg(op, *d), *v as u8]);
                } else if bits == 8 {
                    self.emit(&[0x80 + op * 8, Self::modrm_reg_reg(op, *d), *v as u8]);
                } else {
                    self.emit(&[0x81, Self::modrm_reg_reg(op, *d)]);
                    self.emit(&(*v as i32).to_le_bytes());
                }
            }
            (AsmOperand::Mem { disp, base, index, scale }, AsmOperand::Reg(d, _)) => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base)]);
                self.emit(&[0x03 + op * 8]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            (AsmOperand::Reg(s, _), AsmOperand::Mem { disp, base, index, scale }) => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base)]);
                self.emit(&[0x01 + op * 8]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            _ => { self.emit(&[0x90]); }
        }
    }

    fn emit_asm_modrm_mem_only(&mut self, reg: u8, disp: i32, base: u8, index: Option<u8>, scale: u8) {
        let has_sib = index.is_some() || (base & 7) == 4;
        let need_disp = disp != 0 || (base & 7) == 5;
        let mut modrm = (reg & 7) << 3;
        if !need_disp && disp == 0 { modrm |= 0x00; }
        else if disp >= -128 && disp <= 127 { modrm |= 0x40; }
        else { modrm |= 0x80; }
        if has_sib {
            self.emit(&[modrm | 0x04]);
            let mut sib = (base & 7);
            if let Some(idx) = index { sib |= ((idx & 7) << 3); let sc = match scale { 2=>1,4=>2,8=>3,_=>0 }; sib |= (sc<<6); }
            else { sib |= (4 << 3); }
            self.emit(&[sib]);
        } else {
            self.emit(&[modrm | (base & 7)]);
        }
        if need_disp {
            if disp >= -128 && disp <= 127 { self.emit(&[disp as u8]); }
            else { self.emit(&disp.to_le_bytes()); }
        }
    }

    fn emit_asm_test(&mut self, src: &AsmOperand, dst: &AsmOperand, bits: u8) {
        let w = bits == 64;
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *s, 0, *d), 0x85, Self::modrm_reg_reg(*s, *d)]);
            }
            (AsmOperand::Imm(v), AsmOperand::Reg(d, _)) => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, 0, 0, *d)]);
                if bits == 8 { self.emit(&[0xF6, Self::modrm_reg_reg(0, *d), *v as u8]); }
                else { self.emit(&[0xF7, Self::modrm_reg_reg(0, *d)]); self.emit(&(*v as i32).to_le_bytes()); }
            }
            _ => { self.emit(&[0x90]); }
        }
    }

    fn emit_asm_shift(&mut self, src: &AsmOperand, dst: &AsmOperand, op: u8, bits: u8) {
        let w = bits == 64;
        match (src, dst) {
            (AsmOperand::Imm(v), AsmOperand::Reg(d, _)) => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, 0, 0, *d)]);
                if *v == 1 { self.emit(&[0xD1, Self::modrm_reg_reg(op, *d)]); }
                else { self.emit(&[0xC1, Self::modrm_reg_reg(op, *d), *v as u8]); }
            }
            (AsmOperand::Reg(c, _), AsmOperand::Reg(d, _)) if *c == 1 => { // cl
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, 0, 0, *d), 0xD3, Self::modrm_reg_reg(op, *d)]);
            }
            _ => { self.emit(&[0x90]); }
        }
    }

    fn emit_asm_imul2(&mut self, src: &AsmOperand, dst: &AsmOperand) {
        if let (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) = (src, dst) {
            self.emit(&[Self::rex(true, *d, 0, *s), 0x0F, 0xAF, Self::modrm_reg_reg(*d, *s)]);
        } else { self.emit(&[0x90]); }
    }

    fn emit_asm_imul3(&mut self, src1: &AsmOperand, src2: &AsmOperand, dst: &AsmOperand) {
        if let (AsmOperand::Reg(s, _), AsmOperand::Imm(v), AsmOperand::Reg(d, _)) = (src2, src1, dst) {
            if *v >= -128 && *v <= 127 {
                self.emit(&[Self::rex(true, *d, 0, *s), 0x6B, Self::modrm_reg_reg(*d, *s), *v as u8]);
            } else {
                self.emit(&[Self::rex(true, *d, 0, *s), 0x69, Self::modrm_reg_reg(*d, *s)]);
                self.emit(&(*v as i32).to_le_bytes());
            }
        } else { self.emit(&[0x90]); }
    }

    fn emit_asm_unary_rm(&mut self, op: &AsmOperand, ext: u8, bits: u8) {
        let w = bits == 64;
        match op {
            AsmOperand::Reg(r, _) => {
                self.emit(&[Self::rex(w, 0, 0, *r), 0xF7, Self::modrm_reg_reg(ext, *r)]);
            }
            _ => { self.emit(&[0x90]); }
        }
    }

    fn emit_asm_jmp(&mut self, op: &AsmOperand) {
        match op {
            AsmOperand::Sym(label) => {
                self.jmp_rel32(label, JumpKind::Jmp);
            }
            AsmOperand::Reg(r, _) => {
                // jmp reg: FF /4
                if *r >= 8 { self.emit(&[0x41]); }
                self.emit(&[0xFF, 0xE0 | (r & 7)]);
            }
            AsmOperand::Mem { disp, base, index, scale } => {
                // jmp [mem]: FF /4
                self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base)]);
                self.emit_asm_modrm_mem(0xFF, 4, *disp, *base, *index, *scale, false);
            }
            _ => { self.emit(&[0x90]); }
        }
    }

    fn emit_asm_jcc(&mut self, op: &AsmOperand, cc: u8) {
        match op {
            AsmOperand::Sym(label) => {
                // 发射条件跳转: 0F 8x disp32 (6 bytes)
                let at = self.pos();
                self.emit(&[0x0F, cc, 0, 0, 0, 0]);
                // 映射 AT&T cc 字节到 JumpKind
                let kind = match cc {
                    0x84 => JumpKind::Je, 0x85 => JumpKind::Jne,
                    0x8C => JumpKind::Jl, 0x8E => JumpKind::Jle,
                    0x8F => JumpKind::Jg, 0x8D => JumpKind::Jge,
                    _ => JumpKind::Jne, // fallback
                };
                self.pending.push(PendingJump { at, target: label.clone(), kind });
            }
            _ => { self.emit(&[0x90]); }
        }
    }

    fn emit_asm_call(&mut self, op: &AsmOperand) {
        match op {
            AsmOperand::Sym(label) => {
                self.call_label(label);
            }
            AsmOperand::Reg(r, _) => {
                if *r >= 8 { self.emit(&[0x41]); }
                self.emit(&[0xFF, 0xD0 | (r & 7)]);
            }
            AsmOperand::Mem { disp, base, index, scale } => {
                self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base)]);
                self.emit_asm_modrm_mem(0xFF, 2, *disp, *base, *index, *scale, false);
            }
            _ => { self.emit(&[0x90]); }
        }
    }

    fn emit_asm_movzx(&mut self, src: &AsmOperand, dst: &AsmOperand, src_bits: u8, dst_bits: u8) {
        if let (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) = (src, dst) {
            let w = dst_bits == 64;
            self.emit(&[Self::rex(w, *d, 0, *s)]);
            match src_bits {
                8 => self.emit(&[0x0F, 0xB6, Self::modrm_reg_reg(*d, *s)]),
                16 => self.emit(&[0x0F, 0xB7, Self::modrm_reg_reg(*d, *s)]),
                _ => self.emit(&[0x90]),
            };
        } else { self.emit(&[0x90]); }
    }

    fn emit_asm_movsx(&mut self, src: &AsmOperand, dst: &AsmOperand, src_bits: u8, dst_bits: u8) {
        if let (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) = (src, dst) {
            let w = dst_bits == 64;
            self.emit(&[Self::rex(w, *d, 0, *s)]);
            match src_bits {
                8 => self.emit(&[0x0F, 0xBE, Self::modrm_reg_reg(*d, *s)]),
                16 => self.emit(&[0x0F, 0xBF, Self::modrm_reg_reg(*d, *s)]),
                32 => self.emit(&[0x63, Self::modrm_reg_reg(*d, *s)]),
                _ => self.emit(&[0x90]),
            };
        } else { self.emit(&[0x90]); }
    }

    fn emit_asm_xchg(&mut self, src: &AsmOperand, dst: &AsmOperand) {
        if let (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) = (src, dst) {
            if *s == 0 { // xchg eax, reg
                if *d >= 8 { self.emit(&[0x41]); }
                self.emit(&[0x90 + (d & 7)]);
            } else if *d == 0 {
                if *s >= 8 { self.emit(&[0x41]); }
                self.emit(&[0x90 + (s & 7)]);
            } else {
                self.emit(&[Self::rex(true, *s, 0, *d), 0x87, Self::modrm_reg_reg(*s, *d)]);
            }
        } else { self.emit(&[0x90]); }
    }

    fn emit_asm_cmov(&mut self, src: &AsmOperand, dst: &AsmOperand, cc: u8) {
        if let (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) = (src, dst) {
            self.emit(&[Self::rex(true, *d, 0, *s), 0x0F, cc, Self::modrm_reg_reg(*d, *s)]);
        } else { self.emit(&[0x90]); }
    }

    fn emit_asm_setcc(&mut self, op: &AsmOperand, cc: u8) {
        match op {
            AsmOperand::Reg(r, _) => {
                if *r >= 8 { self.emit(&[0x41]); }
                self.emit(&[0x0F, cc, 0xC0 | (r & 7)]);
            }
            _ => { self.emit(&[0x90]); }
        }
    }

    fn emit_asm_cmpxchg(&mut self, src: &AsmOperand, dst: &AsmOperand) {
        if let (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) = (src, dst) {
            self.emit(&[Self::rex(true, *s, 0, *d), 0x0F, 0xB1, Self::modrm_reg_reg(*s, *d)]);
        } else { self.emit(&[0x90]); }
    }

    fn emit_asm_xadd(&mut self, src: &AsmOperand, dst: &AsmOperand) {
        if let (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) = (src, dst) {
            self.emit(&[Self::rex(true, *s, 0, *d), 0x0F, 0xC1, Self::modrm_reg_reg(*s, *d)]);
        } else { self.emit(&[0x90]); }
    }

}
