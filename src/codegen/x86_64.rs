// src/codegen/x86_64.rs - x86_64 代码生成
// 第一性原理: x86_64 指令编码固定,寄存器按 SysV ABI 选。
// suckless: 直接 emit 字节,不引入汇编库。

use crate::ast::*;
use crate::target::Target;
use crate::codegen::elf::{ElfSym, ElfRela, R_X86_64_PC32, R_X86_64_PLT32,
    STB_LOCAL, STB_GLOBAL, STT_NOTYPE, STT_FUNC, STT_OBJECT, STT_SECTION, SHN_UNDEF};
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
    func_sizes: HashMap<String, u64>, // 函数名 → 代码大小 (用于符号 size)
}

struct PendingJump { at: u64, target: String, kind: JumpKind }
#[derive(PartialEq)]
enum JumpKind { Jmp, Je, Jne, Jl, Jle, Jg, Jge }
#[derive(Clone)]
struct GlobalVar { offset: u64, size: usize, ty: Type, init: Option<Vec<u8>> }

impl Codegen {
    pub fn new(target: Target) -> Self {
        Self { code: vec![], entry: 0, target, labels: Default::default(),
               pending: vec![], next_label: 0, var_map: HashMap::new(),
               loop_continue: None, loop_break: None,
               rodata: vec![], string_pool: HashMap::new(), string_refs: vec![], float_refs: vec![],
               globals: HashMap::new(), frame_size: 0, vla_offsets: HashMap::new(),
               vla_total_size: 0,
               relocatable: false, symbols: vec![], sym_index: HashMap::new(),
               relocations: vec![], pending_func_rels: vec![], pending_data_rels: vec![],
               data_section: vec![], bss_size: 0,
               func_sizes: HashMap::new() }
    }
    pub fn emit(&mut self, b: &[u8]) { self.code.extend_from_slice(b); }
    pub fn pos(&self) -> u64 { self.code.len() as u64 }
    pub fn label(&mut self, name: &str) { self.labels.insert(name.to_string(), self.pos()); }
    pub fn fresh_label(&mut self) -> String { let n = self.next_label; self.next_label += 1; format!(".L{}", n) }

    fn jmp_rel32(&mut self, target: &str, kind: JumpKind) {
        let at = self.pos();
        if kind == JumpKind::Jmp { self.emit(&[0xE9, 0, 0, 0, 0, 0x90]); }
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
            let (rel_from, rel_off) = match p.kind { JumpKind::Jmp => (p.at+5, off+1), _ => (p.at+6, off+2) };
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
    }

    pub fn compile(&mut self, prog: &Program) {
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
                let align = v.ty.align().max(1);
                let off = (self.rodata.len() + align - 1) & !(align - 1);
                while self.rodata.len() < off { self.rodata.push(0); }
                let offset = self.rodata.len() as u64;
                let init_data = if let Some(init) = &v.init {
                    match init {
                        Expr::Int { v, .. } => Some(v.to_le_bytes()[..sz].to_vec()),
                        Expr::String { data, .. } => Some(data.clone()),
                        Expr::InitList { inits, .. } => self.gen_global_init_list(&v.ty, inits),
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
        let rodata_size = self.rodata.len();
        let _main = prog.decls.iter().find_map(|d| match d { Decl::Func(f) if f.name.name == "main" => Some(f), _ => None }).expect("no main()");
        self.label("_start");
        self.emit(&[0x55, 0x48, 0x89, 0xe5, 0x48, 0x31, 0xc0]);
        self.call_label("main");
        self.emit(&[0x89, 0xc7, 0xb8, 0x3c, 0x00, 0x00, 0x00, 0x0f, 0x05, 0xeb, 0xfe]);
        self.emit_functions(prog);
        self.code.extend_from_slice(&self.rodata);
        let rodata_start = self.entry + (self.code.len() as u64 - rodata_size as u64);
        self.resolve(rodata_start);
    }

    /// ET_REL 模式: 生成可重定位 .o 文件
    fn compile_rel(&mut self, prog: &Program) {
        // Phase 1: 生成代码, 收集数据
        // 收集全局变量 → .data / .bss
        for decl in &prog.decls {
            if let Decl::Var(v) = decl {
                let sz = v.ty.size().max(1);
                let align = v.ty.align().max(1);
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
                            // 填充到变量大小
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
                let sym = ElfSym {
                    name: v.name.name.clone(), section: 3, value: g.offset,
                    size: g.size as u64,
                    bind: if v.is_static { STB_LOCAL } else { STB_GLOBAL },
                    ty: STT_OBJECT,
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
                        bind: STB_GLOBAL, ty: STT_FUNC,
                    });
                }
            }
        }

        // 函数定义符号
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if f.body.is_some() {
                    if let Some(&func_start) = self.labels.get(&f.name.name) {
                        let func_size = self.func_sizes.get(&f.name.name).copied().unwrap_or(0);
                        let sym = ElfSym {
                            name: f.name.name.clone(), section: 1, value: func_start,
                            size: func_size,
                            bind: if f.is_static { STB_LOCAL } else { STB_GLOBAL },
                            ty: STT_FUNC,
                        };
                        if f.is_static { local_syms.push(sym); } else { global_syms.push(sym); }
                    }
                }
            }
        }

        // .rodata section 符号 (LOCAL)
        if !self.rodata.is_empty() {
            local_syms.push(ElfSym {
                name: ".rodata".to_string(), section: 2, value: 0,
                size: self.rodata.len() as u64,
                bind: STB_LOCAL, ty: STT_SECTION,
            });
        }

        // 构建最终符号表: STN_UNDEF + locals + globals
        self.symbols.clear();
        self.sym_index.clear();
        // [0] STN_UNDEF
        self.symbols.push(ElfSym {
            name: String::new(), section: SHN_UNDEF, value: 0, size: 0,
            bind: STB_LOCAL, ty: STT_NOTYPE,
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

        // 解析内部跳转 (本地标签)
        self.resolve(0);
    }

    /// 生成所有函数代码 (共用逻辑)
    fn emit_functions(&mut self, prog: &Program) {
        let arg_regs: [u8; 6] = [7, 6, 2, 1, 8, 9];
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                // 跳过 extern 声明 (无函数体)
                if f.body.is_none() { continue; }
                self.var_map.clear();
                self.vla_offsets.clear();
                self.vla_total_size = 0;
                let func_start = self.pos();
                self.label(&f.name.name);
                self.emit(&[0x41, 0x55, 0x49, 0x89, 0xe5]);
                if f.name.name != "main" || !self.relocatable {
                    for (i, p) in f.params.iter().enumerate() {
                        if let Some(nm) = &p.name {
                            if i < 6 { let off = -(self.var_map.len() as i32 * 8 + 16);
                                self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                                self.store_stack(arg_regs[i], off); }
                        }
                    }
                }
                if let Some(body) = &f.body { Self::pre_register_vars(&mut self.var_map, &body.items); }
                self.frame_size = (self.var_map.len() as i32 + 3) * 8;
                if self.frame_size > 0 {
                    self.emit(&[0x48, 0x81, 0xec]);
                    self.emit(&(self.frame_size as i32).to_le_bytes());
                }
                self.emit(&[0x4d, 0x8d, 0xa5]);
                self.emit(&(-(self.frame_size) as i32).to_le_bytes());
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
                                    self.gen_expr(init, 0);
                                    self.mov_to_stack(0, &v.name.name);
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
        }
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
                if let Some(e) = val { self.gen_expr(e, 0); } else { self.emit(&[0x31, 0xc0]); }
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
                for it in &b.items { match it {
                    BlockItem::Stmt(s) => self.gen_stmt(s),
                    BlockItem::Decl(Decl::Var(v)) => {
                        self.register_var(&v.name.name, &v.ty);
                        if matches!(&v.ty, Type::Array(_, ArraySize::Var(_))) {
                            self.gen_vla_alloc(v);
                        } else if let Some(init) = &v.init {
                            if let Expr::InitList { inits, .. } = init {
                                self.gen_init_list(&v.name.name, &v.ty, inits);
                            } else {
                                self.gen_expr(init, 0);
                                self.mov_to_stack(0, &v.name.name);
                            }
                        }
                    }
                    _ => {}
                }}
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
        }
    }

    fn gen_expr(&mut self, e: &Expr, dst: u8) {
        match e {
            Expr::Int { v, .. } => self.mov_imm(dst, *v),
            Expr::Char { v, .. } => self.mov_imm(dst, *v as i128),
            Expr::Ident { name, .. } => {
                if self.globals.contains_key(&name.name) {
                    self.emit_global_load(dst, &name.name);
                } else {
                    self.mov_from_stack(dst, &name.name);
                }
            }
            Expr::Binop { op, l, r, .. } => {
                // 浮点运算: 使用 SSE 指令
                if matches!(l.ty(), Type::Float | Type::Double | Type::LongDouble) {
                    self.gen_float_binop(*op, l, r);
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
                            // 如果 dst == 0, 跳到 false
                            self.jmp_rel32(&l_false, JumpKind::Je);
                            self.gen_expr_decay(r, dst);
                            // test dst, dst
                            self.emit(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                            // sete al
                            self.emit(&[0x0F, 0x94, 0xC0]);
                            // movzx dst, al
                            let rex2 = 0x48 | if dst >= 8 { 0x41 } else { 0 };
                            self.emit(&[rex2, 0x0F, 0xB6, 0xC0 | (dst & 7)]);
                            self.jmp_rel32(&l_end, JumpKind::Jmp);
                            self.label(&l_false);
                            // xor dst, dst (dst = 0)
                            let rex3 = 0x48 | if dst >= 8 { 0x41 } else { 0 };
                            self.emit(&[rex3, 0x31, 0xC0 | (dst & 7)]);
                            self.label(&l_end);
                        }
                        BinOp::OrOr => {
                            // 如果 dst != 0, 跳到 true
                            self.jmp_rel32(&l_false, JumpKind::Jne);
                            self.gen_expr_decay(r, dst);
                            // test dst, dst
                            self.emit(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                            // sete al
                            self.emit(&[0x0F, 0x94, 0xC0]);
                            // movzx dst, al
                            let rex2 = 0x48 | if dst >= 8 { 0x41 } else { 0 };
                            self.emit(&[rex2, 0x0F, 0xB6, 0xC0 | (dst & 7)]);
                            self.jmp_rel32(&l_end, JumpKind::Jmp);
                            self.label(&l_false);
                            // mov dst, 1
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
                // 统一路径: 硬件 push/pop 求值栈, rsp 在 call 前后 save/restore
                // 使用 gen_expr_decay: 数组标识符在运算中衰减为指针
                self.gen_expr_decay(l, dst); self.push(dst);
                self.gen_expr_decay(r, 1);
                // 对整数操作数乘以指针所指类型大小 (跳过 char* 和 ptr-ptr)
                if scale > 1 {
                    self.emit(&[0x48, 0x69, 0xc9]); // imul rcx, rcx, imm32
                    self.emit(&(scale as i32).to_le_bytes());
                }
                self.pop(dst);
                self.binop(*op, dst, 1);
                // ptr - ptr: 字节差 ÷ 元素大小 = 元素数
                if is_ptr_sub && elem_size > 1 {
                    self.mov_imm(1, elem_size as i128); // rcx = elem_size
                    self.emit(&[0x48, 0x99]); // cqo (符号扩展 rax→rdx:rax)
                    self.emit(&[0x48, 0xf7, 0xf9]); // idiv rcx
                }
            }
            Expr::Assign { l, r, .. } => {
                if let Expr::Ident { name, .. } = l.as_ref() {
                    self.gen_expr_decay(r, dst); // 数组衰减: int *p = arr
                    if self.globals.contains_key(&name.name) {
                        self.emit_global_store(dst, &name.name);
                    } else {
                        self.mov_to_stack(dst, &name.name);
                    }
                } else if let Expr::Index { arr, idx, ty: elem_ty, .. } = l.as_ref() {
                    // arr[i] = val
                    let es = elem_ty.size() as i32;
                    if let Expr::Ident { name: arr_name, ty: arr_ty, .. } = arr.as_ref() {
                        if self.vla_offsets.contains_key(&arr_name.name) {
                            // VLA: 基址在 r12
                            self.emit(&[0x41, 0x54, 0x58]); // push r12; pop rax
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
                        self.gen_expr(idx, 1); // rcx = index
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
                    // 1. 计算目标地址
                    if *is_arrow {
                        self.gen_expr(expr, 0); // rax = 指针值
                    } else if let Expr::Ident { name, .. } = expr.as_ref() {
                        let off = self.var_offset(&name.name);
                        // LEA rax, [r13 + off]
                        self.emit(&[0x49, 0x8D, 0x45]); // lea rax, [r13+disp8]
                        self.emit(&[off as i8 as u8]);
                    }
                    // 2. 加上字段偏移
                    if let Type::Struct(_, fields) | Type::Union(_, fields) = &expr.ty() {
                        if let Some(f) = fields.iter().find(|f| f.name == field.name) {
                            let field_off = f.offset as i32;
                            if field_off != 0 {
                                // add rax, imm32
                                self.emit(&[0x48, 0x05]);
                                self.emit(&field_off.to_le_bytes());
                            }
                        }
                    }
                    self.push(0); // 保存目标地址
                    // 3. 计算右值
                    self.gen_expr(r, 0); // rax = 值
                    self.pop(1); // rcx = 目标地址
                    // 4. 存储
                    let size = l.ty().size();
                    match size {
                        1 => self.emit(&[0x88, 0x01]), // mov [rcx], al
                        2 => self.emit(&[0x66, 0x89, 0x01]), // mov [rcx], ax
                        4 => self.emit(&[0x89, 0x01]), // mov [rcx], eax
                        8 => self.emit(&[0x48, 0x89, 0x01]), // mov [rcx], rax
                        _ => {} // 大struct不在此处理
                    }
                }
            }
            Expr::Unary { op, e, .. } => {
                self.gen_expr(e, dst);
                match op {
                    UnOp::Neg => { self.emit(&[0xf7, 0xd8 | (dst << 3)]); }
                    UnOp::Not => {
                        // 逻辑非: !x = (x == 0) ? 1 : 0
                        // test dst, dst
                        let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                        self.emit(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                        // sete al (0F 94 C0)
                        self.emit(&[0x0F, 0x94, 0xC0]);
                        // movzx dst, al (48 0F B6 C0)
                        let rex2 = 0x48 | if dst >= 8 { 0x41 } else { 0 };
                        self.emit(&[rex2, 0x0F, 0xB6, 0xC0 | (dst & 7)]);
                    }
                    UnOp::BitNot => { self.emit(&[0xf7, 0xd0 | (dst << 3)]); }
                    UnOp::Plus => {}
                }
            }
            Expr::Call { callee, args, .. } => {
                let ar = [7u8, 6, 2, 1, 8, 9];
                for (i, a) in args.iter().enumerate() { self.gen_expr(a, ar[i]); }
                // 保存 rsp (保护求值栈上的硬件 push 值不被 callee 覆盖)
                self.emit(&[0x54]); // push rsp
                self.call_label(&callee.name);
                self.emit(&[0x5c]); // pop rsp (恢复 rsp, 丢弃 callee 的 push r13)
                // 调用约定: 返回值在 rax(0), 移至请求的目标寄存器
                if dst != 0 { self.mov(dst, 0); }
            }
            Expr::SizeofTy { ty, .. } => self.mov_imm(dst, ty.size() as i128),
            Expr::SizeofExpr { .. } => self.mov_imm(dst, 8),
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
                let bytes = if is_float {
                    // float: 4 字节
                    let f = *v as f32;
                    f.to_le_bytes().to_vec()
                } else {
                    // double/long double: 8 字节
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
                        // VLA: 基址在 r12, 用 push r12; pop rax
                        self.emit(&[0x41, 0x54, 0x58]); // push r12; pop rax
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
                    self.gen_expr(idx, 1); // rcx = index
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
                // &x: 计算变量地址加载到 dst
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
                }
            }
            Expr::Member { expr, field, is_arrow, ty, .. } => {
                // struct/union 成员访问: 基址 + 偏移
                // 1. 计算基址 (struct指针或struct变量地址)
                if *is_arrow {
                    // ptr->field: expr 是指针, 直接加载指针值
                    self.gen_expr(expr, dst);
                } else {
                    // obj.field: expr 是struct变量, 取地址
                    if let Expr::Ident { name, .. } = expr.as_ref() {
                        if self.globals.contains_key(&name.name) {
                            self.emit_global_addr(dst, &name.name);
                        } else {
                            let off = self.var_offset(&name.name);
                            // LEA dst, [r13 + off]
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
                        // 复杂表达式: 先计算地址
                        self.gen_expr(expr, dst);
                    }
                }
                // 2. 加上字段偏移
                if let Type::Struct(_, fields) | Type::Union(_, fields) = &expr.ty() {
                    if let Some(f) = fields.iter().find(|f| f.name == field.name) {
                        let field_off = f.offset as i32;
                        if field_off != 0 {
                            // add dst, imm32
                            let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                            self.emit(&[rex, 0x81, 0xC0 | (dst & 7)]);
                            self.emit(&field_off.to_le_bytes());
                        }
                    }
                }
                // 3. 加载成员值
                let size = ty.size();
                match size {
                    1 => self.emit(&[0x48, 0x0F, 0xB6, 0x00 | ((dst & 7) << 3) | (dst & 7)]), // movzx dst, byte [dst]
                    2 => self.emit(&[0x48, 0x0F, 0xB7, 0x00 | ((dst & 7) << 3) | (dst & 7)]), // movzx dst, word [dst]
                    4 => self.emit(&[0x8B, 0x00 | ((dst & 7) << 3) | (dst & 7)]), // mov dst, dword [dst]
                    8 => self.emit(&[0x48, 0x8B, 0x00 | ((dst & 7) << 3) | (dst & 7)]), // mov dst, qword [dst]
                    _ => {} // 大struct不在此处理
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
                let rex_byte = 0x40u8 | if src_reg >= 8 { 0x04 } else { 0 }; // REX.B only
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
                let rex_byte = 0x40u8 | if src_reg >= 8 { 0x04 } else { 0 };
                self.emit(&[rex_byte, 0x89]);
                if offset.abs() < 128 {
                    self.emit(&[0x45 | ((src_reg & 7) << 3), offset as i8 as u8]);
                } else {
                    self.emit(&[0x85 | ((src_reg & 7) << 3)]);
                    self.emit(&offset.to_le_bytes());
                }
            }
            4 => {
                // mov [r13 + disp], src_reg (dword)
                self.emit(&[rex, 0x89]);
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
    fn gen_float_binop(&mut self, op: BinOp, l: &Expr, r: &Expr) {
        let is_float = matches!(l.ty(), Type::Float);
        // 计算左操作数到 xmm0
        self.gen_expr(l, 0);
        // 保存 xmm0 到栈
        self.emit(&[0x66, 0x0F, 0xD6, 0x04, 0x24]); // movq [rsp], xmm0
        // 计算右操作数到 xmm0
        self.gen_expr(r, 0);
        // 恢复左操作数到 xmm1
        self.emit(&[0x66, 0x0F, 0x7E, 0x0C, 0x24]); // movq xmm1, [rsp]
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
        let _is_float = matches!(ty, Type::Float);
        // xmm0 已经是右操作数, xmm1 是左操作数
        // ucomisd xmm1, xmm0
        self.emit(&[0x66, 0x0F, 0x2E, 0xC8]); // ucomisd xmm1, xmm0
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
        let off = -(self.var_map.len() as i32 * 8 + 16);
        self.var_map.insert(name.to_string(), (off, Type::Int)); off
    }

    /// 全局变量 RIP-relative 加载: reg = [global_var]
    /// 编码: REX.W 8B 05|reg<<3 disp32
    fn emit_global_load(&mut self, reg: u8, name: &str) {
        let disp32_pos = self.pos() + 3; // REX + opcode + ModRM = 3 bytes
        let rex = if reg >= 8 { 0x4C } else { 0x48 }; // REX.W (+ REX.B if reg>=8)
        self.emit(&[rex, 0x8B, 0x05 | ((reg & 7) << 3), 0, 0, 0, 0]);
        self.pending_data_rels.push((disp32_pos, name.to_string()));
    }

    /// 全局变量 RIP-relative 存储: [global_var] = reg
    /// 编码: REX.W 89 05|reg<<3 disp32
    fn emit_global_store(&mut self, reg: u8, name: &str) {
        let disp32_pos = self.pos() + 3;
        let rex = if reg >= 8 { 0x4C } else { 0x48 };
        self.emit(&[rex, 0x89, 0x05 | ((reg & 7) << 3), 0, 0, 0, 0]);
        self.pending_data_rels.push((disp32_pos, name.to_string()));
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
        let slots = (ty.size() + 7) / 8; // 向上对齐到 8 字节
        let slots = slots.max(1) as i32;
        let off = -(self.var_map.len() as i32 * 8 + 16);
        self.var_map.insert(name.to_string(), (off, ty.clone()));
        // 为数组预留额外槽位 (空插入占位)
        for i in 1..slots {
            let placeholder_name = format!("__pad_{}_{}", name, i);
            let pad_off = -(self.var_map.len() as i32 * 8 + 16);
            self.var_map.insert(placeholder_name, (pad_off, Type::Int));
        }
        off
    }
    fn mov_to_stack(&mut self, reg: u8, name: &str) { let off = self.var_offset(name); self.store_stack(reg, off); }
    fn mov_from_stack(&mut self, reg: u8, name: &str) { let off = self.var_offset(name); self.load_stack(reg, off); }

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

    // VLA 运行时分配: TODO - 当前架构无法解决 VLA 与固定帧的栈空间重叠问题
    // 需要重新设计栈帧布局 (如使用两个帧指针或堆分配) 才能正确支持
    fn gen_vla_alloc(&mut self, _v: &VarDecl) {
        // TODO: VLA 代码生成
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
    fn pre_register_vars(var_map: &mut HashMap<String, (i32, Type)>, items: &[BlockItem]) {
        for item in items {
            match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    let slots = ((v.ty.size() + 7) / 8).max(1) as i32;
                    let off = -(var_map.len() as i32 * 8 + 16);
                    var_map.entry(v.name.name.clone()).or_insert((off, v.ty.clone()));
                    for j in 1..slots {
                        let pad = format!("__pad_{}_{}", v.name.name, j);
                        let pad_off = -(var_map.len() as i32 * 8 + 16);
                        var_map.entry(pad).or_insert((pad_off, Type::Int));
                    }
                }
                BlockItem::Stmt(s) => Self::pre_register_stmt(var_map, s),
                _ => {}
            }
        }
    }
    fn pre_register_stmt(var_map: &mut HashMap<String, (i32, Type)>, s: &Stmt) {
        match s {
            Stmt::Block { b, .. } => Self::pre_register_vars(var_map, &b.items),
            Stmt::If { then_, else_, .. } => {
                Self::pre_register_stmt(var_map, then_);
                if let Some(e) = else_ { Self::pre_register_stmt(var_map, e); }
            }
            Stmt::While { body, .. } | Stmt::For { body, .. } | Stmt::Do { body, .. } => {
                Self::pre_register_stmt(var_map, body);
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

}
