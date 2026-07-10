// src/codegen/x86_64.rs - x86_64 代码生成
// 第一性原理: x86_64 指令编码固定,寄存器按 SysV ABI 选。
// suckless: 直接 emit 字节,不引入汇编库。

use crate::ast::*;
use crate::target::Target;
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
    rodata: Vec<u8>,
    string_pool: HashMap<Vec<u8>, u64>,   // data -> offset in rodata
    string_refs: Vec<(u64, Vec<u8>)>,     // (code_pos, data)
    // 全局变量
    globals: HashMap<String, GlobalVar>,
    frame_size: i32,  // 当前函数栈帧大小（字节）
    vla_offsets: HashMap<String, i32>,  // VLA 变量名 → 距 r13 的偏移
    vla_total_size: i32,  // 当前函数所有 VLA 总大小
}

struct PendingJump { at: u64, target: String, kind: JumpKind }
#[derive(PartialEq)]
enum JumpKind { Jmp, Je, Jne, Jl, Jle, Jg, Jge }
#[derive(Clone)]
struct GlobalVar { offset: u64, size: usize, init: Option<Vec<u8>> }

impl Codegen {
    pub fn new(target: Target) -> Self {
        Self { code: vec![], entry: 0, target, labels: Default::default(),
               pending: vec![], next_label: 0, var_map: HashMap::new(),
               loop_continue: None, loop_break: None,
               rodata: vec![], string_pool: HashMap::new(), string_refs: vec![],
               globals: HashMap::new(), frame_size: 0, vla_offsets: HashMap::new(),
               vla_total_size: 0 }
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
    }

    pub fn compile(&mut self, prog: &Program) {
        self.entry = 0x400000 + 64 + 56;
        // 收集全局变量
        for decl in &prog.decls {
            if let Decl::Var(v) = decl {
                let sz = v.ty.size().max(1);
                let align = v.ty.align().max(1);
                // 对齐 rodata
                let off = (self.rodata.len() + align - 1) & !(align - 1);
                while self.rodata.len() < off { self.rodata.push(0); }
                let offset = self.rodata.len() as u64;
                let init_data = if let Some(init) = &v.init {
                    if let Expr::Int { v, .. } = init {
                        Some(v.to_le_bytes()[..sz].to_vec())
                    } else { None }
                } else { None };
                // 预留空间
                for _ in 0..sz { self.rodata.push(0); }
                self.globals.insert(v.name.name.clone(), GlobalVar { offset, size: sz, init: init_data });
            }
        }
        let globals_init: Vec<(String, Option<Vec<u8>>)> = self.globals.iter()
            .map(|(n, g)| (n.clone(), g.init.clone())).collect();
        // 写入全局初始值
        for (name, init) in &globals_init {
            if let Some(data) = init {
                let g = self.globals[name].clone();
                let start = g.offset as usize;
                for (i, &b) in data.iter().enumerate() {
                    self.rodata[start + i] = b;
                }
            }
        }
        let rodata_size = self.rodata.len();
        let _main = prog.decls.iter().find_map(|d| match d { Decl::Func(f) if f.name.name == "main" => Some(f), _ => None }).expect("no main()");
        // _start
        self.label("_start");
        self.emit(&[0x55, 0x48, 0x89, 0xe5, 0x48, 0x31, 0xc0]);
        self.call_label("main");
        self.emit(&[0x89, 0xc7, 0xb8, 0x3c, 0x00, 0x00, 0x00, 0x0f, 0x05, 0xeb, 0xfe]);

        let arg_regs: [u8; 6] = [7, 6, 2, 1, 8, 9];
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                self.var_map.clear();
                self.vla_offsets.clear();
                self.vla_total_size = 0;
                self.label(&f.name.name);
                // 序言: push r13(帧指针)
                self.emit(&[0x41, 0x55, 0x49, 0x89, 0xe5]); // push r13; mov r13, rsp
                if f.name.name != "main" {
                    for (i, p) in f.params.iter().enumerate() {
                        if let Some(nm) = &p.name {
                            if i < 6 { let off = -(self.var_map.len() as i32 * 8 + 16);
                                self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                                self.store_stack(arg_regs[i], off); }
                        }
                    }
                }
                // 预留栈空间: 先预注册所有局部变量 (数组分配多槽), 再计算帧大小
                if let Some(body) = &f.body { Self::pre_register_vars(&mut self.var_map, &body.items); }
                // 预注册后 var_map 已包含参数+局部变量+数组填充, 不再加 nlocals
                self.frame_size = (self.var_map.len() as i32 + 3) * 8;
                if self.frame_size > 0 {
                    self.emit(&[0x48, 0x81, 0xec]); // sub rsp, imm32
                    self.emit(&(self.frame_size as i32).to_le_bytes());
                }
                // r12 初始化为帧底部 (VLA 基址等用途)
                // 求值栈使用硬件 push/pop (rsp), 变量用 r13-relative, 互不干扰
                self.emit(&[0x4d, 0x8d, 0xa5]); // lea r12, [r13 + imm32]
                self.emit(&(-(self.frame_size) as i32).to_le_bytes());
                let ends_ret = if let Some(body) = &f.body {
                    for item in &body.items { match item {
                        BlockItem::Stmt(s) => self.gen_stmt(s),
                        BlockItem::Decl(Decl::Var(v)) => {
                            // 预注册变量 (数组分配多槽)
                            self.register_var(&v.name.name, &v.ty);
                            if matches!(&v.ty, Type::Array(_, ArraySize::Var(_))) {
                                self.gen_vla_alloc(v);
                            } else if let Some(init) = &v.init {
                                self.gen_expr(init, 0);
                                self.mov_to_stack(0, &v.name.name);
                            }
                        }
                        _ => {}
                    }}
                    Self::ends_with_return(&body.items)
                } else { false };
                if !ends_ret { self.emit_func_epilogue(); }
            }
        }
        // 追加 .rodata 到代码段末尾
        self.code.extend_from_slice(&self.rodata);
        let rodata_start = self.entry + (self.code.len() as u64 - rodata_size as u64);
        self.resolve(rodata_start);
    }

    fn call_label(&mut self, l: &str) {
        let at = self.pos(); self.emit(&[0xe8, 0, 0, 0, 0]);
        self.pending.push(PendingJump { at, target: l.into(), kind: JumpKind::Jmp });
    }

    // 生成表达式值, 数组标识符衰减为指针 (arr → &arr[0])
    fn gen_expr_decay(&mut self, e: &Expr, dst: u8) {
        if let Expr::Ident { name, ty, .. } = e {
            if matches!(ty, Type::Array(_, _)) {
                let off = self.var_offset(&name.name);
                let rex = 0x49u8; // REX.W + REX.B (r13)
                self.emit(&[rex, 0x8D]);
                if off.abs() < 128 {
                    self.emit(&[0x45 | ((dst & 7) << 3), off as i8 as u8]);
                } else {
                    self.emit(&[0x85 | ((dst & 7) << 3)]);
                    self.emit(&off.to_le_bytes());
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
                            self.gen_expr(init, 0);
                            self.mov_to_stack(0, &v.name.name);
                        }
                    }
                    _ => {}
                }}
            }
            Stmt::Break { .. } => { if let Some(l) = self.loop_break.clone() { self.jmp_rel32(&l, JumpKind::Jmp); } }
            Stmt::Continue { .. } => { if let Some(l) = self.loop_continue.clone() { self.jmp_rel32(&l, JumpKind::Jmp); } }
            Stmt::Goto { target, .. } => { self.jmp_rel32(&target.name, JumpKind::Jmp); }
            Stmt::Label { name, stmt, .. } => { self.label(&name.name); self.gen_stmt(stmt); }
            Stmt::Switch { .. } => { /* TODO */ }
        }
    }

    fn gen_expr(&mut self, e: &Expr, dst: u8) {
        match e {
            Expr::Int { v, .. } => self.mov_imm(dst, *v),
            Expr::Char { v, .. } => self.mov_imm(dst, *v as i128),
            Expr::Ident { name, .. } => self.mov_from_stack(dst, &name.name),
            Expr::Binop { op, l, r, .. } => {
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
                    self.mov_to_stack(dst, &name.name);
                } else if let Expr::Index { arr, idx, ty: elem_ty, .. } = l.as_ref() {
                    // arr[i] = val
                    let es = elem_ty.size() as i32;
                    if let Expr::Ident { name: arr_name, ty: arr_ty, .. } = arr.as_ref() {
                        if self.vla_offsets.contains_key(&arr_name.name) {
                            // VLA: 基址在 r12
                            self.emit(&[0x41, 0x54, 0x58]); // push r12; pop rax
                        } else if matches!(arr_ty, Type::Array(_, _)) {
                            // 静态数组: LEA 取基址
                            let off = self.var_offset(&arr_name.name);
                            self.emit_lea_r13(0, off);
                        } else {
                            // 指针: 加载指针值
                            let off = self.var_offset(&arr_name.name);
                            self.load_stack(0, off);
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
                }
            }
            Expr::Unary { op, e, .. } => {
                self.gen_expr(e, dst);
                match op { UnOp::Neg => { self.emit(&[0xf7, 0xd8 | (dst << 3)]); }
                    UnOp::Not | UnOp::BitNot => { self.emit(&[0xf7, 0xd0 | (dst << 3)]); }
                    UnOp::Plus => {} }
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
            Expr::Cast { e, .. } => self.gen_expr(e, dst),
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
            Expr::Index { arr, idx, ty: elem_ty, .. } => {
                let es = elem_ty.size() as i32;
                if let Expr::Ident { name: arr_name, ty: arr_ty, .. } = arr.as_ref() {
                    if self.vla_offsets.contains_key(&arr_name.name) {
                        // VLA: 基址在 r12, 用 push r12; pop rax
                        self.emit(&[0x41, 0x54, 0x58]); // push r12; pop rax
                    } else if matches!(arr_ty, Type::Array(_, _)) {
                        // 静态数组: LEA 取基址
                        let off = self.var_offset(&arr_name.name);
                        self.emit_lea_r13(0, off);
                    } else {
                        // 指针: 加载指针值
                        let off = self.var_offset(&arr_name.name);
                        self.load_stack(0, off);
                    }
                    self.gen_expr(idx, 1); // rcx = index
                    if es != 1 {
                        self.emit(&[0x48, 0x69, 0xc9]); // imul rcx, rcx, imm32
                        self.emit(&es.to_le_bytes());
                    }
                    self.emit(&[0x48, 0x01, 0xc8]); // add rax, rcx
                    self.emit(&[0x48, 0x8B, 0x00]); // mov rax, [rax]
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
                self.emit(&[0x48, 0x8B, 0x00]); // mov rax, [rax]
                if dst != 0 { self.mov(dst, 0); }
            }
            Expr::AddrOf { e, .. } => {
                // &x: 计算变量地址加载到 dst
                if let Expr::Ident { name, .. } = e.as_ref() {
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
            _ => { /* M0: 其他表达式暂不处理 */ }
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
