// src/codegen/x86_64.rs - x86_64 代码生成
// 第一性原理: x86_64 指令编码固定,寄存器按 SysV ABI 选。
// suckless: 直接 emit 字节,不引入汇编库。

use crate::ast::*;
use crate::target::Target;
use std::collections::HashMap;

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
    pub rodata: Vec<u8>,
}

struct PendingJump { at: u64, target: String, kind: JumpKind }
#[derive(PartialEq)]
enum JumpKind { Jmp, Je, Jne, Jl, Jle, Jg, Jge }

impl Codegen {
    pub fn new(target: Target) -> Self {
        Self { code: vec![], entry: 0, target, labels: Default::default(),
               pending: vec![], next_label: 0, var_map: HashMap::new(),
               loop_continue: None, loop_break: None, rodata: vec![] }
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

    pub fn resolve(&mut self) {
        for p in self.pending.drain(..) {
            let dst = *self.labels.get(&p.target).expect("unresolved label");
            let off = p.at as usize;
            let (rel_from, rel_off) = match p.kind { JumpKind::Jmp => (p.at+5, off+1), _ => (p.at+6, off+2) };
            let rel = (dst as i64) - (rel_from as i64);
            self.code[rel_off..rel_off+4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
    }

    pub fn compile(&mut self, prog: &Program) {
        self.entry = 0x400000 + 64 + 56;
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
                self.label(&f.name.name);
                self.emit(&[0x41, 0x55, 0x49, 0x89, 0xe5]); // push r13; mov r13, rsp
                if f.name.name != "main" {
                    for (i, p) in f.params.iter().enumerate() {
                        if let Some(nm) = &p.name {
                            if i < 6 { let off = -(self.var_map.len() as i32 * 8 + 8);
                                self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                                self.store_stack(arg_regs[i], off); }
                        }
                    }
                }
                let ends_ret = if let Some(body) = &f.body {
                    for item in &body.items { match item {
                        BlockItem::Stmt(s) => self.gen_stmt(s),
                        BlockItem::Decl(Decl::Var(v)) => { if let Some(init) = &v.init { self.gen_expr(init, 0); self.mov_to_stack(0, &v.name.name); } }
                        _ => {}
                    }}
                    Self::ends_with_return(&body.items)
                } else { false };
                if !ends_ret { self.emit(&[0x31, 0xc0, 0x41, 0x5d, 0xc3]); }
            }
        }
        self.resolve();
    }

    fn call_label(&mut self, l: &str) {
        let at = self.pos(); self.emit(&[0xe8, 0, 0, 0, 0]);
        self.pending.push(PendingJump { at, target: l.into(), kind: JumpKind::Jmp });
    }

    fn gen_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Return { val, .. } => {
                if let Some(e) = val { self.gen_expr(e, 0); } else { self.emit(&[0x31, 0xc0]); }
                self.emit(&[0x41, 0x5d, 0xc3]);
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
                    BlockItem::Decl(Decl::Var(v)) => { if let Some(init) = &v.init { self.gen_expr(init, 0); self.mov_to_stack(0, &v.name.name); } }
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
                self.gen_expr(l, dst); self.push(dst);
                self.gen_expr(r, 1); self.pop(dst);
                self.binop(*op, dst, 1);
            }
            Expr::Assign { l, r, .. } => {
                if let Expr::Ident { name, .. } = l.as_ref() { self.gen_expr(r, dst); self.mov_to_stack(dst, &name.name); }
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
                self.call_label(&callee.name);
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
                // TODO: 正确实现字符串 (需要 .rodata + RIP-relative 寻址)
                self.mov_imm(dst, 0);
            }
            _ => { /* M0: 其他表达式暂不处理 */ }
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
        let mut rex = 0x48u8; if dst >= 8 { rex |= 0x44; } if src >= 8 { rex |= 0x41; }
        self.emit(&[rex, 0x89, 0xC0 | ((dst & 7) << 3) | (src & 7)]);
    }
    fn push(&mut self, reg: u8) { if reg < 8 { self.emit(&[0x50 + reg]); } else { self.emit(&[0x41, 0x50 + (reg - 8)]); } }
    fn pop(&mut self, reg: u8) { if reg < 8 { self.emit(&[0x58 + reg]); } else { self.emit(&[0x41, 0x58 + (reg - 8)]); } }

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
        let off = -(self.var_map.len() as i32 * 8 + 8);
        self.var_map.insert(name.to_string(), (off, Type::Int)); off
    }
    fn mov_to_stack(&mut self, reg: u8, name: &str) { let off = self.var_offset(name); self.store_stack(reg, off); }
    fn mov_from_stack(&mut self, reg: u8, name: &str) { let off = self.var_offset(name); self.load_stack(reg, off); }

    fn store_stack(&mut self, reg: u8, off: i32) {
        let mut rex = 0x48u8; if reg >= 8 { rex |= 0x44; } rex |= 0x01; self.emit(&[rex, 0x89]);
        if off.abs() < 128 { self.emit(&[0x45 | ((reg & 7) << 3), off as i8 as u8]); }
        else { self.emit(&[0x85 | ((reg & 7) << 3)]); self.emit(&off.to_le_bytes()); }
    }
    fn load_stack(&mut self, reg: u8, off: i32) {
        let mut rex = 0x48u8; if reg >= 8 { rex |= 0x44; } rex |= 0x01; self.emit(&[rex, 0x8B]);
        if off.abs() < 128 { self.emit(&[0x45 | ((reg & 7) << 3), off as i8 as u8]); }
        else { self.emit(&[0x85 | ((reg & 7) << 3)]); self.emit(&off.to_le_bytes()); }
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
