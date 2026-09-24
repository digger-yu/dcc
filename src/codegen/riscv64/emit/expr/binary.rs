// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{BinOp, Type};
use crate::codegen::riscv64::codegen::{BranchKind, Riscv64Codegen, align_rodata};
use crate::ir::{ValueType};

impl Riscv64Codegen {
    pub(crate) fn alloc_local(&mut self, name: &str, bytes: i32) -> i32 {
        let aligned = (bytes + 7) & !7;
        let off = self.stack_bytes;
        self.stack_bytes += aligned;
        self.locals.insert(name.to_string(), off);
        off
    }

    /// 判断 AST 类型是否为有符号整数 (用于移位等有符号性判定)
    pub(crate) fn is_signed_ast_type(ty: &Type) -> bool {
        matches!(
            ty,
            Type::SChar | Type::Short | Type::Int | Type::Long | Type::LongLong | Type::Int128
        )
    }

    pub(crate) fn binop_a0_a1(&mut self, op: BinOp, is_signed: bool) {
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

    pub(crate) fn float_binop_fa0(&mut self, op: BinOp, ty: &ValueType) {
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
    pub(crate) fn float_cmp_a0_fa1_fa0(&mut self, op: BinOp, ty: &ValueType) {
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
    pub(crate) fn emit_align_padding(&mut self, alignment: u32) {
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
    pub(crate) fn maybe_align_loop(&mut self) {
        if self._target.align_loops > 1 {
            self.emit_align_padding(self._target.align_loops);
        }
    }

    /// 如果配置了 align_jumps, 在跳转目标标签前发射对齐填充
    pub(crate) fn maybe_align_jump(&mut self) {
        if self._target.align_jumps > 1 {
            self.emit_align_padding(self._target.align_jumps);
        }
    }

    pub(crate) fn fresh_label(&mut self, prefix: &str) -> String {
        self.label_serial += 1;
        format!("{prefix}{}", self.label_serial)
    }

    pub(crate) fn pos(&self) -> u64 {
        self.code.len() as u64
    }

    pub(crate) fn label(&mut self, name: &str) {
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

    pub(crate) fn emit(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    pub(crate) fn li_a0(&mut self, imm: i32) {
        self.li_reg(10, imm);
    }

    pub(crate) fn li_a7(&mut self, imm: i32) {
        self.li_reg(17, imm);
    }

    pub(crate) fn li_reg(&mut self, rd: u8, imm: i32) {
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

    pub(crate) fn load_float_fa0(&mut self, v: f32) {
        let off = align_rodata(&mut self.rodata, 4);
        let bits = v.to_bits();
        self.rodata.extend_from_slice(&bits.to_le_bytes());
        let pos = self.pos();
        self.emit_auipc_a0(0);
        self.emit_flw_fa0_a0(0);
        self.float_refs.push((pos, off));
    }

    pub(crate) fn store_a0(&mut self, off: i32) {
        self.emit_sw(10, 2, off);
    }

    pub(crate) fn load_a0(&mut self, off: i32) {
        self.emit_lw(10, 2, off);
    }

    /// 整数提升: 将小于 int 的整数类型提升到 64 位
    /// char/short → 符号扩展; uchar/ushort/bool → 零扩展
    pub(crate) fn gen_int_promote(&mut self, ty: &Type) {
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
    pub(crate) fn gen_int_promote_ir(&mut self, ty: &ValueType) {
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

    pub(crate) fn mv_a1_a0(&mut self) {
        self.emit_addi(11, 10, 0);
    }

    pub(crate) fn mv_a_reg(&mut self, idx: u8) {
        let rd = 10 + idx;
        self.emit_addi(rd, 10, 0);
    }

    /// Load a local variable from stack into an arbitrary GP register (32-bit LW)
    pub(crate) fn asm_load_var_to_reg(&mut self, var_name: &str, reg_x86: u8) {
        // reg_x86 is the x-register index (e.g. 10 for a0, 5 for t0)
        if let Some(&off) = self.locals.get(var_name) {
            self.emit_lw(reg_x86, 2, off);
        }
    }

    /// Store a GP register value back to a local variable on stack (32-bit SW)
    pub(crate) fn asm_store_reg_to_var(&mut self, var_name: &str, reg_x86: u8) {
        if let Some(&off) = self.locals.get(var_name) {
            self.emit_sw(reg_x86, 2, off);
        }
    }

}
