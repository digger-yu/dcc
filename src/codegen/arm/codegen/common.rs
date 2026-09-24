// ARM backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{BinOp, BlockItem, FuncDecl};
use crate::codegen::shared::elf::{ElfSym, STB_GLOBAL, STT_FUNC};
use super::struct_def::ArmCodegen;
use super::BranchKind;

impl ArmCodegen {
    // ===== 局部变量分配 =====

    pub(crate) fn plan_locals(&mut self, ast: Option<&FuncDecl>) {
        self.locals.clear();
        self.stack_bytes = 0;
        let Some(body) = ast.and_then(|f| f.body.as_ref()) else {
            return;
        };
        for item in &body.items {
            if let BlockItem::Decl(crate::ast::Decl::Var(v)) = item {
                let sz = v.ty.size().max(4) as i32;
                self.alloc_local(&v.name.name, sz);
            }
        }
    }

    pub(crate) fn alloc_local(&mut self, name: &str, size: i32) -> i32 {
        let off = self.stack_bytes;
        self.stack_bytes += (size + 3) & !3; // 4 字节对齐
        self.locals.insert(name.to_string(), off);
        off
    }

    pub(crate) fn emit_binop(&mut self, op: BinOp) {
        match op {
            BinOp::Add => self.emit_add(0, 1, 0), // add r0, r1, r0
            BinOp::Sub => self.emit_sub(0, 1, 0), // sub r0, r1, r0
            BinOp::Mul => self.emit_mul(0, 1, 0), // mul r0, r1, r0
            BinOp::Div => {
                // 简化: 调用 __aeabi_idiv (需要链接库)
                // 这里先做简单处理
                self.emit_push(vec![14]); // push lr
                self.emit_bl("__aeabi_idiv");
                self.emit_pop(vec![14]); // pop lr
            }
            BinOp::Mod => {
                // 简化: a % b = a - (a/b)*b
                self.emit_push(vec![0, 1]); // save both
                self.emit_push(vec![14]);
                self.emit_bl("__aeabi_idiv");
                self.emit_push(vec![0]); // save quotient
                self.emit_pop(vec![1]); // pop divisor to r1
                self.emit_pop(vec![0]); // pop dividend to r0
                                        // r0 = quotient, r1 = divisor (need to recompute)
                                        // 简化: 直接返回 0
                self.emit_pop(vec![14]);
                self.emit_load_imm(0, 0);
            }
            BinOp::And => self.emit_and(0, 1, 0),
            BinOp::Or => self.emit_orr(0, 1, 0),
            BinOp::Xor => self.emit_eor(0, 1, 0),
            BinOp::Shl => self.emit_lsl(0, 1, 0),
            BinOp::Shr => self.emit_asr(0, 1, 0),
            BinOp::Lt => {
                self.emit_cmp(1, 0);
                self.emit_mov_imm(0, 0);
                self.emit_movlt(0, 1);
            }
            BinOp::Gt => {
                self.emit_cmp(1, 0);
                self.emit_mov_imm(0, 0);
                self.emit_movgt(0, 1);
            }
            BinOp::Le => {
                self.emit_cmp(1, 0);
                self.emit_mov_imm(0, 0);
                self.emit_movle(0, 1);
            }
            BinOp::Ge => {
                self.emit_cmp(1, 0);
                self.emit_mov_imm(0, 0);
                self.emit_movge(0, 1);
            }
            BinOp::EqEq => {
                self.emit_cmp(1, 0);
                self.emit_mov_imm(0, 0);
                self.emit_moveq(0, 1);
            }
            BinOp::Ne => {
                self.emit_cmp(1, 0);
                self.emit_mov_imm(0, 0);
                self.emit_movne(0, 1);
            }
            BinOp::AndAnd => {
                self.emit_and(0, 1, 0);
                self.emit_cmp_imm(0, 0);
                self.emit_mov_imm(0, 0);
                self.emit_movne(0, 1);
            }
            BinOp::OrOr => {
                self.emit_orr(0, 1, 0);
                self.emit_cmp_imm(0, 0);
                self.emit_mov_imm(0, 0);
                self.emit_movne(0, 1);
            }
        }
    }

    pub(crate) fn fresh_label(&mut self, prefix: &str) -> String {
        self.label_serial += 1;
        format!("{}{}", prefix, self.label_serial)
    }

    pub(crate) fn set_label(&mut self, label: &str) {
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
                BranchKind::Beq => self.patch_beq(at, pos),
                BranchKind::B => self.patch_b(at, pos),
            }
        }
        self.pending_branches.retain(|(_, l, _)| l != label);
    }

    /// Ensure symbol exists in symbol table
    pub(crate) fn ensure_sym(&mut self, name: &str) -> u32 {
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
    pub(crate) fn ensure_sym_object(&mut self, name: &str) -> u32 {
        if let Some(&idx) = self.sym_index.get(name) {
            return idx;
        }
        self.symbols.push(ElfSym {
            name: name.to_string(),
            section: 0,
            value: 0,
            size: 0,
            bind: STB_GLOBAL,
            ty: crate::codegen::shared::elf::STT_OBJECT,
            visibility: 0,
        });
        let idx = self.symbols.len() as u32;
        self.sym_index.insert(name.to_string(), idx);
        idx
    }

    pub(crate) fn patch_beq(&mut self, at: u64, target: u64) {
        // ARM BEQ offset: (target - branch_pc - 8) / 4
        // branch_pc is the position of the BEQ instruction
        let offset = ((target as i64) - (at as i64) - 8) / 4;
        let offset = offset as u32 & 0x00FFFFFF;
        // Read existing instruction, preserve condition code, update offset
        let existing = u32::from_le_bytes([
            self.code[at as usize],
            self.code[at as usize + 1],
            self.code[at as usize + 2],
            self.code[at as usize + 3],
        ]);
        let patched = (existing & 0xFF000000) | offset;
        self.code[at as usize..at as usize + 4].copy_from_slice(&patched.to_le_bytes());
    }

    pub(crate) fn patch_b(&mut self, at: u64, target: u64) {
        // ARM B (AL condition) offset: (target - branch_pc - 8) / 4
        let offset = ((target as i64) - (at as i64) - 8) / 4;
        let offset = offset as u32 & 0x00FFFFFF;
        let patched = 0xEA000000 | offset; // B with AL condition
        self.code[at as usize..at as usize + 4].copy_from_slice(&patched.to_le_bytes());
    }

    // ===== ARM 指令编码 =====

    pub(crate) fn emit_push(&mut self, regs: Vec<u8>) {
        let mut reg_list: u16 = 0;
        for r in regs {
            reg_list |= 1 << r;
        }
        let inst = 0xE92D0000 | reg_list as u32;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_pop(&mut self, regs: Vec<u8>) {
        let mut reg_list: u16 = 0;
        for r in regs {
            reg_list |= 1 << r;
        }
        let inst = 0xE8BD0000 | reg_list as u32;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_mov_reg(&mut self, rd: u8, rm: u8) {
        let inst = 0xE1A00000 | ((rd as u32) << 12) | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_mov_imm(&mut self, rd: u8, imm: u8) {
        // MOV rd, #imm (立即数)
        let inst = 0xE3A00000 | ((rd as u32) << 12) | (imm as u32 & 0xFF);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_load_imm(&mut self, rd: u8, val: i32) {
        // 简化: 使用 MOV + MVN 处理小/大整数
        if (0..256).contains(&val) {
            self.emit_mov_imm(rd, val as u8);
        } else if (-256..0).contains(&val) {
            // MVN rd, #(|val|-1)
            let inst = 0xE3E00000 | ((rd as u32) << 12) | ((-val - 1) as u32 & 0xFF);
            self.code.extend_from_slice(&inst.to_le_bytes());
        } else {
            // 大整数: 使用 literal pool (简化为 movw/movt 序列)
            // ARMv7 支持 MOVW/MOVT
            let lo = (val & 0xFFFF) as u32;
            let hi = ((val >> 16) & 0xFFFF) as u32;
            // MOVW rd, #lo
            let inst = 0xE3000000 | ((rd as u32) << 12) | ((hi & 0xF) << 16) | lo;
            self.code.extend_from_slice(&inst.to_le_bytes());
            if hi > 0 {
                // MOVT rd, #hi
                let inst = 0xE3400000 | ((rd as u32) << 12) | ((hi & 0xF) << 16) | (hi >> 4);
                self.code.extend_from_slice(&inst.to_le_bytes());
            }
        }
    }

    pub(crate) fn emit_add(&mut self, rd: u8, rn: u8, rm: u8) {
        let inst = 0xE0800000 | ((rd as u32) << 12) | ((rn as u32) << 16) | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_sub(&mut self, rd: u8, rn: u8, rm: u8) {
        let inst = 0xE0400000 | ((rd as u32) << 12) | ((rn as u32) << 16) | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_sub_imm(&mut self, rd: u8, rn: u8, imm: u32) {
        let inst = 0xE2400000 | ((rd as u32) << 12) | ((rn as u32) << 16) | (imm & 0xFF);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// -fstack-clash-protection: emit page-sized probes when allocating a
    /// large stack frame.  Each probe stores a zero byte at [sp] to trigger
    /// a fault on the guard page before the kernel maps it.
    pub(crate) fn emit_stack_clash_probes(&mut self, frame_size: u32) {
        let page: u32 = 4096;
        let remainder = frame_size % page;
        let pages = frame_size / page;
        // First: sub sp, sp, #remainder (if any)
        if remainder > 0 {
            self.emit_sub_imm(13, 13, remainder);
        }
        if pages > 0 {
            // Probe loop using R12 (IP, ABI scratch) as counter
            // mov r12, #pages
            self.emit_mov_imm(12, pages as u8);
            // .probe_loop:
            let loop_start = self.code.len() as u64;
            // sub sp, sp, #4096  (0x10 rotated right by 20 = 4096)
            self.emit_sub_imm(13, 13, 0x10);
            // strb r0, [sp]  — probe the page (r0 as scratch)
            let strb = 0xE5CD0000u32;
            self.code.extend_from_slice(&strb.to_le_bytes());
            // subs r12, r12, #1  (subtract and set flags)
            let subs = 0xE25CC001u32;
            self.code.extend_from_slice(&subs.to_le_bytes());
            // bne .probe_loop
            let loop_end = self.code.len() as u64;
            let offset = ((loop_start as i64 - loop_end as i64 - 8) / 4) as u32;
            let bne = 0x1A000000u32 | (offset & 0x00FFFFFF);
            self.code.extend_from_slice(&bne.to_le_bytes());
        }
    }

    pub(crate) fn emit_add_imm(&mut self, rd: u8, rn: u8, imm: u32) {
        let inst = 0xE2800000 | ((rd as u32) << 12) | ((rn as u32) << 16) | (imm & 0xFF);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_mul(&mut self, rd: u8, rm: u8, rs: u8) {
        // MUL rd, rm, rs => rd = rm * rs
        let inst = 0xE0000090 | ((rd as u32) << 16) | ((rs as u32) << 8) | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_and(&mut self, rd: u8, rn: u8, rm: u8) {
        let inst = 0xE0000000 | ((rd as u32) << 12) | ((rn as u32) << 16) | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_orr(&mut self, rd: u8, rn: u8, rm: u8) {
        let inst = 0xE1800000 | ((rd as u32) << 12) | ((rn as u32) << 16) | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_eor(&mut self, rd: u8, rn: u8, rm: u8) {
        let inst = 0xE0200000 | ((rd as u32) << 12) | ((rn as u32) << 16) | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_lsl(&mut self, rd: u8, rm: u8, rs: u8) {
        // LSL rd, rm, rs (寄存器移位)
        let inst = 0xE1A00010 | ((rd as u32) << 12) | (rs as u32) << 8 | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_asr(&mut self, rd: u8, rm: u8, rs: u8) {
        // ASR rd, rm, rs
        let inst = 0xE1A00050 | ((rd as u32) << 12) | (rs as u32) << 8 | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_mvn(&mut self, rd: u8, rm: u8) {
        let inst = 0xE1E00000 | ((rd as u32) << 12) | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_rsb_imm(&mut self, rd: u8, rn: u8, imm: u8) {
        // RSB rd, rn, #imm => rd = imm - rn
        let inst = 0xE2600000 | ((rd as u32) << 12) | ((rn as u32) << 16) | (imm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_cmp(&mut self, rn: u8, rm: u8) {
        let inst = 0xE1500000 | ((rn as u32) << 16) | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_cmp_imm(&mut self, rn: u8, imm: u8) {
        let inst = 0xE3500000 | ((rn as u32) << 16) | (imm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// BNE label: Branch if not equal (condition code NE = 0x1)
    pub(crate) fn emit_bne(&mut self, label: &str) {
        let pos = self.code.len() as u64;
        self.pending_branches
            .push((pos, label.to_string(), BranchKind::Beq));
        // BNE placeholder (condition = NE = 0x1)
        let inst: u32 = 0x0A000000; // B with condition NE
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_b(&mut self, label: &str) {
        // B: 记录 pending branch
        let pos = self.code.len() as u64;
        self.pending_branches
            .push((pos, label.to_string(), BranchKind::B));
        // 占位 B 指令 (AL condition)
        self.code.extend_from_slice(&0xEA000000u32.to_le_bytes());
    }

    pub(crate) fn emit_beq(&mut self, label: &str) {
        // BEQ: 条件码 0x0
        let pos = self.code.len() as u64;
        self.pending_branches
            .push((pos, label.to_string(), BranchKind::Beq));
        // 占位 BEQ 指令 (EQ condition = 0x0)
        self.code.extend_from_slice(&0x0A000000u32.to_le_bytes());
    }

    pub(crate) fn emit_bl(&mut self, name: &str) {
        // BL: 记录 pending call
        self.ensure_sym(name);
        let pos = self.code.len() as u64;
        self.pending_bl_calls.push((pos, name.to_string()));
        // 占位 BL 指令
        self.code.extend_from_slice(&0xEB000000u32.to_le_bytes());
    }

    // 条件 MOV 指令
    pub(crate) fn emit_mov_cond(&mut self, cond: u8, rd: u8, imm: u8) {
        let inst = 0xE3B00000 | ((cond as u32) << 28) | ((rd as u32) << 12) | (imm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_moveq(&mut self, rd: u8, imm: u8) {
        // MOVEQ rd, #imm (cond = 0x0)
        let inst = 0x03A00000 | ((rd as u32) << 12) | (imm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_movne(&mut self, rd: u8, imm: u8) {
        // MOVNE rd, #imm (cond = 0x1)
        let inst = 0x13A00000 | ((rd as u32) << 12) | (imm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_movlt(&mut self, rd: u8, imm: u8) {
        // MOVLT rd, #imm (cond = 0xB)
        let inst = 0xB3A00000 | ((rd as u32) << 12) | (imm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_movgt(&mut self, rd: u8, imm: u8) {
        // MOVGT rd, #imm (cond = 0xC)
        let inst = 0xC3A00000 | ((rd as u32) << 12) | (imm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_movle(&mut self, rd: u8, imm: u8) {
        // MOVLE rd, #imm (cond = 0xD)
        let inst = 0xD3A00000 | ((rd as u32) << 12) | (imm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub(crate) fn emit_movge(&mut self, rd: u8, imm: u8) {
        // MOVGE rd, #imm (cond = 0xA)
        let inst = 0xA3A00000 | ((rd as u32) << 12) | (imm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    // 栈上局部变量加载/存储
    pub(crate) fn emit_load_r0(&mut self, offset: i32) {
        // LDR r0, [fp, #offset]
        if offset >= 0 {
            let inst = 0xE59B0000 | (offset as u32 & 0xFFF);
            self.code.extend_from_slice(&inst.to_le_bytes());
        } else {
            let inst = 0xE51B0000 | ((-offset) as u32 & 0xFFF);
            self.code.extend_from_slice(&inst.to_le_bytes());
        }
    }

    pub(crate) fn emit_store_r0(&mut self, offset: i32) {
        // STR r0, [fp, #offset]
        if offset >= 0 {
            let inst = 0xE58B0000 | (offset as u32 & 0xFFF);
            self.code.extend_from_slice(&inst.to_le_bytes());
        } else {
            let inst = 0xE50B0000 | ((-offset) as u32 & 0xFFF);
            self.code.extend_from_slice(&inst.to_le_bytes());
        }
    }

    /// Load a local variable from stack into an arbitrary GP register
    pub(crate) fn asm_load_var_to_reg(&mut self, var_name: &str, reg: u8) {
        if let Some(&off) = self.locals.get(var_name) {
            // LDR rd, [fp, #off]
            if off >= 0 {
                let inst = 0xE59B0000 | ((reg as u32) << 12) | (off as u32 & 0xFFF);
                self.code.extend_from_slice(&inst.to_le_bytes());
            } else {
                let inst = 0xE51B0000 | ((reg as u32) << 12) | ((-off) as u32 & 0xFFF);
                self.code.extend_from_slice(&inst.to_le_bytes());
            }
        }
    }

    /// Store a GP register value back to a local variable on stack
    pub(crate) fn asm_store_reg_to_var(&mut self, var_name: &str, reg: u8) {
        if let Some(&off) = self.locals.get(var_name) {
            // STR rd, [fp, #off]
            if off >= 0 {
                let inst = 0xE58B0000 | ((reg as u32) << 12) | (off as u32 & 0xFFF);
                self.code.extend_from_slice(&inst.to_le_bytes());
            } else {
                let inst = 0xE50B0000 | ((reg as u32) << 12) | ((-off) as u32 & 0xFFF);
                self.code.extend_from_slice(&inst.to_le_bytes());
            }
        }
    }

    /// MOV rd, rs — register-to-register move
    pub(crate) fn emit_mov_rd_rs(&mut self, rd: u8, rs: u8) {
        let inst = 0xE1A00000u32 | ((rd as u32) << 12) | (rs as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }
}
