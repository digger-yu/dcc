// src/codegen/x86_64.rs - x86_64 代码生成
// 第一性原理: x86_64 指令编码固定,寄存器按 SysV ABI 选。
// suckless 准则: 不引入汇编库,直接 emit 字节。
// M0 范围:
//   - mov 立即数到寄存器
//   - 加/减/乘/与/或/异或/移位/比较
//   - 函数调用 (call/ret)
//   - exit (syscall, rax=60, rdi=ret)
//   - main 包装为 _start (call main; mov edi,eax; mov eax,60; syscall)
//
// 寄存器约定 (SysV ABI): rax=ret, rdi/rsi/rdx/rcx/r8/r9=参数, rbx/r12-r15=callee-saved
// 临时: rcx/rdx/r8-r11 (调用者保存)
// 栈: 16 字节对齐,返回地址在 [rsp]

use crate::ast::*;
use crate::target::Target;
use std::collections::HashMap;

pub struct Codegen {
    pub code: Vec<u8>,
    pub entry: u64,
    pub target: Target,
    /// label -> offset
    labels: std::collections::HashMap<String, u64>,
    /// 尚未解析的跳转
    pending: Vec<PendingJump>,
    next_label: u32,
    /// 变量名 -> [rbp+off] 偏移
    var_map: HashMap<String, i32>,
}

struct PendingJump {
    at: u64,             // 跳转指令的偏移
    target: String,
    kind: JumpKind,
}
#[derive(PartialEq)]
enum JumpKind { Jmp, Je, Jne, Jl, Jle, Jg, Jge }

impl Codegen {
    pub fn new(target: Target) -> Self {
        Self {
            code: vec![],
            entry: 0,
            target,
            labels: Default::default(),
            pending: vec![],
            next_label: 0,
            var_map: HashMap::new(),
        }
    }

    pub fn emit(&mut self, b: &[u8]) { self.code.extend_from_slice(b); }
    pub fn pos(&self) -> u64 { self.code.len() as u64 }

    pub fn label(&mut self, name: &str) {
        self.labels.insert(name.to_string(), self.pos());
    }
    pub fn fresh_label(&mut self) -> String {
        let n = self.next_label; self.next_label += 1;
        format!(".L{}", n)
    }

    fn jmp_rel32(&mut self, target: &str, kind: JumpKind) {
        let at = self.pos();
        // 第一性原理: 统一 6 字节布局,方便 patch
        //   短 jmp:  E9 + rel32           (5 字节) + 1 字节 NOP pad
        //   长 jcc:  0F 8x + rel32       (6 字节)
        // PendingJump.at 指向 6 字节起点,resolve() 时统一 patch rel32
        if kind == JumpKind::Jmp {
            self.emit(&[0xE9, 0, 0, 0, 0, 0x90]);   // E9 + rel32 + NOP
        } else {
            let cc: u8 = match kind {
                JumpKind::Je  => 0x84,
                JumpKind::Jne => 0x85,
                JumpKind::Jl  => 0x8C,
                JumpKind::Jle => 0x8E,
                JumpKind::Jg  => 0x8F,
                JumpKind::Jge => 0x8D,
                _ => unreachable!(),
            };
            self.emit(&[0x0F, cc, 0, 0, 0, 0]);
        }
        self.pending.push(PendingJump { at, target: target.into(), kind });
    }

    pub fn resolve(&mut self) {
        for p in self.pending.drain(..) {
            let dst = *self.labels.get(&p.target).expect("unresolved label");
            let off = p.at as usize;
            // rel32 起点不同:
            //   短 jmp: E9 之后 1 字节起, 指向 at+5
            //   长 jcc: 0F 8x 之后 2 字节起, 指向 at+6
            let (rel_from, rel_off) = match p.kind {
                JumpKind::Jmp => (p.at + 5, off + 1),
                _             => (p.at + 6, off + 2),
            };
            let rel = (dst as i64) - (rel_from as i64);
            let bytes = (rel as i32).to_le_bytes();
            self.code[rel_off..rel_off + 4].copy_from_slice(&bytes);
        }
    }

    // ============================================================
    // 顶层驱动:编译一个 program
    // ============================================================
    pub fn compile(&mut self, prog: &Program) {
        // _start 入口 - Linux 加载器跳转到这里
        self.entry = 0x400000 + 64 + 56;  // header + phdr

        // 1. 确认 main 存在
        let _main = prog.decls.iter().find_map(|d| match d {
            Decl::Func(f) if f.name.name == "main" => Some(f),
            _ => None,
        }).expect("no main() defined");

        // 2. 发射 _start 桩
        self.label("_start");
        self.emit(&[0x55]);                 // push rbp
        self.emit(&[0x48, 0x89, 0xe5]);     // mov rbp, rsp
        self.emit(&[0x48, 0x31, 0xc0]);     // xor rax, rax
        self.call_label("main");
        self.emit(&[0x89, 0xc7]);           // mov edi, eax
        self.emit(&[0xb8, 0x3c, 0x00, 0x00, 0x00]); // mov eax, 60
        self.emit(&[0x0f, 0x05]);           // syscall
        self.emit(&[0xeb, 0xfe]);           // jmp $ (死循环)

        // 3. 编译所有函数
        // SysV ABI 参数寄存器顺序: rdi, rsi, rdx, rcx, r8, r9
        let arg_regs: [u8; 6] = [7, 6, 2, 1, 8, 9];

        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                self.var_map.clear();

                self.label(&f.name.name);
                self.emit(&[0x55]);                 // push rbp
                self.emit(&[0x48, 0x89, 0xe5]);     // mov rbp, rsp

                // 非 main 函数: 把寄存器参数存入栈槽
                if f.name.name != "main" {
                    for (i, p) in f.params.iter().enumerate() {
                        if let Some(name) = &p.name {
                            if i < 6 {
                                // 预分配栈槽并记录偏移
                                let off = -(self.var_map.len() as i32 * 8 + 8);
                                self.var_map.insert(name.name.clone(), off);
                                // 从参数寄存器存到 [rbp+off]
                                self.store_stack(arg_regs[i], off);
                            }
                        }
                    }
                }

                // 编译函数体
                if let Some(body) = &f.body {
                    for item in &body.items {
                        match item {
                            BlockItem::Stmt(s) => self.gen_stmt(s),
                            BlockItem::Decl(Decl::Var(v)) => {
                                if let Some(init) = &v.init {
                                    self.gen_expr(init, 0);
                                    self.mov_to_stack(0, &v.name.name);
                                }
                            }
                            _ => {}
                        }
                    }
                }

                // 默认返回 0
                self.emit(&[0x31, 0xc0]);             // xor eax, eax
                self.emit(&[0xc9]);                   // leave
                self.emit(&[0xc3]);                   // ret
            }
        }

        self.resolve();

        eprintln!("=== codegen ({} bytes) ===", self.code.len());
        for (i, chunk) in self.code.chunks(16).enumerate() {
            eprint!("  {:04x}: ", i * 16);
            for b in chunk { eprint!("{:02x} ", b); }
            eprintln!();
        }
        eprintln!("=== var_map: {:?} ===", self.var_map);
        eprintln!("=== labels: {:?} ===", self.labels);
    }

    fn call_label(&mut self, l: &str) {
        let at = self.pos();
        self.emit(&[0xe8]); self.emit(&[0,0,0,0]);
        self.pending.push(PendingJump { at, target: l.into(), kind: JumpKind::Jmp });
    }

    // ============================================================
    // 语句
    // ============================================================
    fn gen_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Return { val, .. } => {
                if let Some(e) = val { self.gen_expr(e, 0 /* eax */); }
                else { self.emit(&[0x31, 0xc0]); /* xor eax, eax */ }
                self.emit(&[0xc9, 0xc3]);   // leave; ret
            }
            Stmt::Expr { e, .. } => { self.gen_expr(e, 0); }
            Stmt::If { cond, then_, else_, .. } => {
                self.gen_expr(cond, 0);                 // rax = cond
                self.emit(&[0x85, 0xc0]);               // test eax, eax
                let l_else = self.fresh_label();
                let l_end  = self.fresh_label();
                self.jmp_rel32(&l_else, JumpKind::Je);
                self.gen_stmt(then_);
                self.jmp_rel32(&l_end, JumpKind::Jmp);
                self.label(&l_else);
                if let Some(el) = else_ { self.gen_stmt(el); }
                self.label(&l_end);
            }
            Stmt::While { cond, body, .. } => {
                let l_start = self.fresh_label();
                let l_end   = self.fresh_label();
                self.label(&l_start);
                self.gen_expr(cond, 0);
                self.emit(&[0x85, 0xc0]);
                self.jmp_rel32(&l_end, JumpKind::Je);
                self.gen_stmt(body);
                self.jmp_rel32(&l_start, JumpKind::Jmp);
                self.label(&l_end);
            }
            Stmt::Block { b, .. } => {
                for it in &b.items {
                    match it {
                        BlockItem::Stmt(s) => self.gen_stmt(s),
                        BlockItem::Decl(Decl::Var(v)) => {
                            if let Some(init) = &v.init {
                                self.gen_expr(init, 0);
                                self.mov_to_stack(0, &v.name.name);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Stmt::Break { .. } => { /* M0 简化:while 内的 break 假定唯一 - 不实现 */ }
            Stmt::Continue { .. } => {}
            _ => panic!("unsupported stmt in M0: {:?}", s),
        }
    }

    // ============================================================
    // 表达式 - 把结果放在指定寄存器 (M0: reg 0..7 -> rax..rdi)
    // ============================================================
    fn gen_expr(&mut self, e: &Expr, dst: u8) {
        match e {
            Expr::Int { v, .. } => {
                self.mov_imm(dst, *v);
            }
            Expr::Ident { name, .. } => {
                // M0: 标识符视为已绑定到 [rbp-8] 局部 - 未实现存储位置,M0 不支持 var 读取
                // 允许 main 内 a=1 后读 a:我们存到 [rbp-8]
                self.mov_from_stack(dst, &name.name);
            }
            Expr::Binop { op, l, r, .. } => {
                // gen l -> dst, push dst, gen r -> src(1), pop dst, dst = dst OP src
                self.gen_expr(l, dst);
                self.push(dst);
                self.gen_expr(r, 1);    // 1 = rcx (临时)
                self.pop(dst);
                self.binop(*op, dst, 1);
            }
            Expr::Assign { op: _, l, r, .. } => {
                // M0: 仅支持 a = expr 形式
                if let Expr::Ident { name, .. } = l.as_ref() {
                    self.gen_expr(r, dst);
                    self.mov_to_stack(dst, &name.name);
                }
            }
            Expr::Unary { op, e, .. } => {
                self.gen_expr(e, dst);
                match op {
                    UnOp::Neg   => { self.emit(&[0xf7, 0xd8 | (dst << 3)]); }
                    UnOp::Not   => { self.emit(&[0xf7, 0xd0 | (dst << 3)]); /* not eax */ }
                    UnOp::BitNot=> { self.emit(&[0xf7, 0xd0 | (dst << 3)]); }
                    UnOp::Plus  => {}
                    UnOp::Deref | UnOp::AddrOf => panic!("M0: pointer unary not yet"),
                    UnOp::PreInc | UnOp::PreDec |
                    UnOp::PostInc| UnOp::PostDec => panic!("M0: inc/dec not yet"),
                }
            }
            Expr::Call { callee, args, .. } => {
                // M0: 最多支持 6 个参数, 顺序: rdi rsi rdx rcx r8 r9
                if args.len() > 6 { panic!("M0: more than 6 args"); }
                let arg_reg = [7u8, 6, 2, 1, 8, 9];   // rdi..r9 in our reg encoding
                for (i, a) in args.iter().enumerate() {
                    self.gen_expr(a, arg_reg[i]);
                }
                // call
                self.call_label(&callee.name);
            }
            Expr::Cast { .. } => { /* M0: 不做 cast */ }
            Expr::Sizeof { .. } => { self.mov_imm(dst, 8); }
            _ => panic!("unsupported expr in M0: {:?}", e),
        }
    }

    // ============================================================
    // 指令发射
    // ============================================================
    fn mov_imm(&mut self, reg: u8, v: i128) {
        // reg: 0=rax,1=rcx,2=rdx,3=rbx,4=rsp,5=rbp,6=rsi,7=rdi
        // REX.W=0x48, REX.R=0x44, REX.B=0x41
        if v >= i32::MIN as i128 && v <= i32::MAX as i128 {
            // mov r32, imm32
            if reg < 8 {
                self.emit(&[0xb8 + reg]);
                self.emit(&(v as i32).to_le_bytes());
            } else {
                // REX.B prefix
                self.emit(&[0x41, 0xb8 + (reg - 8)]);
                self.emit(&(v as i32).to_le_bytes());
            }
        } else {
            // movabs r64, imm64
            self.emit(&[0x48 + if reg >= 8 { 0x41 } else { 0 }]);
            self.emit(&[0xb8 + (reg & 7)]);
            self.emit(&(v as i64).to_le_bytes());
        }
    }

    fn mov(&mut self, dst: u8, src: u8) {
        // mov dst, src (64-bit)
        let mut rex = 0x48u8;
        if dst >= 8 { rex |= 0x44; }   // REX.R
        if src >= 8 { rex |= 0x41; }   // REX.B
        self.emit(&[rex]);
        self.emit(&[0x89]);
        // ModR/M: 11 (reg-reg) dst:src
        let d = (dst & 7) << 3;
        let s = src & 7;
        self.emit(&[0xC0 | d | s]);
    }

    fn push(&mut self, reg: u8) {
        if reg < 8 { self.emit(&[0x50 + reg]); }
        else { self.emit(&[0x41, 0x50 + (reg - 8)]); }
    }
    fn pop(&mut self, reg: u8) {
        if reg < 8 { self.emit(&[0x58 + reg]); }
        else { self.emit(&[0x41, 0x58 + (reg - 8)]); }
    }

    fn binop(&mut self, op: BinOp, dst: u8, src: u8) {
        // dst = dst OP src
        // 比较指令自行处理 REX (因为 cmp 的 reg/rm 角色互换)
        let is_cmp = matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne);
        if !is_cmp {
            let mut rex = 0x48u8;
            if dst >= 8 { rex |= 0x44; }
            if src >= 8 { rex |= 0x41; }
            self.emit(&[rex]);
        }
        let d = (dst & 7) << 3;
        let s = src & 7;
        match op {
            BinOp::Add => { self.emit(&[0x01, 0xC0 | d | s]); }
            BinOp::Sub => { self.emit(&[0x29, 0xC0 | d | s]); }
            BinOp::And => { self.emit(&[0x21, 0xC0 | d | s]); }
            BinOp::Or  => { self.emit(&[0x09, 0xC0 | d | s]); }
            BinOp::Xor => { self.emit(&[0x31, 0xC0 | d | s]); }
            BinOp::Mul => { self.emit(&[0x0F, 0xAF, 0xC0 | d | s]); }
            BinOp::Div | BinOp::Mod => {
                // cqto (sign-extend rax -> rdx:rax) + idiv r/m64 (ModR/M = 0xF8 | s)
                self.emit(&[0x48, 0x99]); // cqto
                let mut rex = 0x48u8;
                if src >= 8 { rex |= 0x41; }
                self.emit(&[rex, 0xF7, 0xF8 | (src & 7)]);
                if matches!(op, BinOp::Mod) { self.mov(dst, 2); /* rdx -> dst */ }
            }
            BinOp::Shl => { self.emit(&[0xD3, 0xE0 | d]); /* shift by cl */ }
            BinOp::Shr => { self.emit(&[0xD3, 0xE8 | d]); }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne => {
                // cmp dst, src  →  computes dst - src (x86: cmp r/m, reg = r/m - reg)
                // r/m=dst, reg=src
                let mut rex = 0x48u8;
                if src >= 8 { rex |= 0x44; }   // REX.R for reg field (src)
                if dst >= 8 { rex |= 0x41; }   // REX.B for r/m field (dst)
                let d = (src & 7) << 3;        // reg field = src
                let s = dst & 7;               // r/m field = dst
                self.emit(&[rex, 0x39, 0xC0 | d | s]);
                // setXX al
                let cc = match op {
                    BinOp::Lt => 0x9C, BinOp::Le => 0x9E,
                    BinOp::Gt => 0x9F, BinOp::Ge => 0x9D,
                    BinOp::EqEq => 0x94, BinOp::Ne => 0x95,
                    _ => unreachable!(),
                };
                self.emit(&[0x0F, cc, 0xC0]);       // setcc al
                self.emit(&[0x48, 0x0F, 0xB6, 0xC0]); // movzx rax, al
                if dst != 0 { self.mov(dst, 0); }
            }
            _ => panic!("binop not yet: {:?}", op),
        }
    }

    // 变量存储 - M0 简化:所有局部变量共享 [rbp-8], [rbp-16]..栈
    fn var_offset(&mut self, name: &str) -> i32 {
        if let Some(&off) = self.var_map.get(name) {
            return off;
        }
        let off = -(self.var_map.len() as i32 * 8 + 8);
        self.var_map.insert(name.to_string(), off);
        off
    }
    // 借用,避免递归可变借用
    fn mov_to_stack(&mut self, reg: u8, name: &str) {
        let off = self.var_offset(name);
        eprintln!("DEBUG: store {} to rbp{:+}(reg={})", name, off, reg);
        self.store_stack(reg, off);
    }
    fn mov_from_stack(&mut self, reg: u8, name: &str) {
        let off = self.var_offset(name);
        eprintln!("DEBUG: load {} from rbp{:+}(reg={})", name, off, reg);
        self.load_stack(reg, off);
    }
    fn store_stack(&mut self, reg: u8, off: i32) {
        // mov [rbp+off], reg
        let mut rex = 0x48u8;
        if reg >= 8 { rex |= 0x44; }
        self.emit(&[rex, 0x89]);
        if (off as i32).abs() < 128 {
            self.emit(&[0x45 | ((reg & 7) << 3), off as i8 as u8]);
        } else {
            self.emit(&[0x85 | ((reg & 7) << 3)]);
            self.emit(&off.to_le_bytes());
        }
    }
    fn load_stack(&mut self, reg: u8, off: i32) {
        let mut rex = 0x48u8;
        if reg >= 8 { rex |= 0x44; }
        self.emit(&[rex, 0x8B]);
        if (off as i32).abs() < 128 {
            self.emit(&[0x45 | ((reg & 7) << 3), off as i8 as u8]);
        } else {
            self.emit(&[0x85 | ((reg & 7) << 3)]);
            self.emit(&off.to_le_bytes());
        }
    }
}
