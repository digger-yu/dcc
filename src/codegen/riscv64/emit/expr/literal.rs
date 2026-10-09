// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::riscv64::codegen::{BranchKind, Riscv64Codegen};

impl Riscv64Codegen {
    pub(crate) fn emit_slt(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b010 << 12 | (rd as u32) << 7 | 0x33;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_sltu(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b011 << 12 | (rd as u32) << 7 | 0x33;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_lw(&mut self, rd: u8, rs1: u8, off: i32) {
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
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_sw(&mut self, rs2: u8, rs1: u8, off: i32) {
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
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// LB - 加载字节 (有符号扩展)
    pub(crate) fn emit_lb(&mut self, rd: u8, rs1: u8, off: i32) {
        let imm = off as u32 & 0xfff;
        let insn = (imm << 20 | (rs1 as u32) << 15) | (rd as u32) << 7 | 0x03;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// LH - 加载半字 (有符号扩展)
    pub(crate) fn emit_lh(&mut self, rd: u8, rs1: u8, off: i32) {
        let imm = off as u32 & 0xfff;
        let insn = imm << 20 | (rs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0x03;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// SH - 存储半字
    pub(crate) fn emit_sh(&mut self, rs2: u8, rs1: u8, off: i32) {
        let imm = off as u32;
        let insn = ((imm >> 5) & 0x7f) << 25
            | (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0b001 << 12
            | (imm & 0x1f) << 7
            | 0x23;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_jal(&mut self, rd: u8, _offset: i32) {
        let insn = (rd as u32) << 7 | 0x6f;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_jalr(&mut self, rd: u8, rs1: u8, imm: i32) {
        let imm = imm as u32 & 0xfff;
        let insn = (imm << 20 | (rs1 as u32) << 15) | (rd as u32) << 7 | 0x67;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_beq(&mut self, rs1: u8, rs2: u8, _offset: i32) {
        let insn = ((rs2 as u32) << 20 | (rs1 as u32) << 15) | 0x63;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// AUIPC rd, imm: rd = PC + imm << 12
    pub(crate) fn emit_auipc(&mut self, rd: u8, imm: i32) {
        let imm = (imm as u32 & 0xfffff) << 12;
        let insn = imm | (rd as u32) << 7 | 0x17;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// BNE rs1, rs2, label: Branch if not equal
    pub(crate) fn emit_bne(&mut self, rs1: u8, rs2: u8, label: &str) {
        let pos = self.pos();
        // BNE: funct3=001, opcode=1100011
        let insn = (1u32 << 12) | ((rs2 as u32) << 20) | ((rs1 as u32) << 15) | 0x63;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
        if let Some(&target) = self.labels.get(label) {
            self.patch_bne(pos, rs1, rs2, target);
        } else {
            self.pending_branches
                .push((pos, label.to_string(), BranchKind::Bne));
        }
    }

    /// Patch BNE instruction at given position
    pub(crate) fn patch_bne(&mut self, at: u64, rs1: u8, rs2: u8, target: u64) {
        self.patch_branch(at, rs1, rs2, target, 1);
    }

    /// Patch generic B-type branch instruction
    pub(crate) fn patch_branch(&mut self, at: u64, rs1: u8, rs2: u8, target: u64, funct3: u32) {
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
    pub(crate) fn emit_beq_label(&mut self, rs1: u8, rs2: u8, label: &str) {
        let pos = self.pos();
        // funct3=000, opcode=1100011
        let insn = ((rs2 as u32) << 20) | ((rs1 as u32) << 15) | 0x63;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
        if let Some(&target) = self.labels.get(label) {
            self.patch_branch(pos, rs1, rs2, target, 0);
        } else {
            self.pending_branches
                .push((pos, label.to_string(), BranchKind::Beq));
        }
    }

    /// BGE rs1, rs2, label: Branch if greater or equal (signed)
    pub(crate) fn emit_bge_label(&mut self, rs1: u8, rs2: u8, label: &str) {
        let pos = self.pos();
        // funct3=101, opcode=1100011
        let insn = (5u32 << 12) | ((rs2 as u32) << 20) | ((rs1 as u32) << 15) | 0x63;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
        if let Some(&target) = self.labels.get(label) {
            self.patch_branch(pos, rs1, rs2, target, 5);
        } else {
            self.pending_branches
                .push((pos, label.to_string(), BranchKind::Bge));
        }
    }

    /// BLT rs1, rs2, label: Branch if less than (signed)
    pub(crate) fn emit_blt_label(&mut self, rs1: u8, rs2: u8, label: &str) {
        let pos = self.pos();
        // funct3=100, opcode=1100011
        let insn = (4u32 << 12) | ((rs2 as u32) << 20) | ((rs1 as u32) << 15) | 0x63;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
        if let Some(&target) = self.labels.get(label) {
            self.patch_branch(pos, rs1, rs2, target, 4);
        } else {
            self.pending_branches
                .push((pos, label.to_string(), BranchKind::Blt));
        }
    }

    pub(crate) fn emit_auipc_a0(&mut self, imm: i32) {
        let imm = (imm as u32 & 0xfffff) << 12;
        let insn = imm | 10 << 7 | 0x17;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_ld_a0_a0(&mut self, off: i32) {
        let imm = off as u32 & 0xfff;
        let insn = imm << 20 | 10 << 15 | 0b011 << 12 | 10 << 7 | 0x03;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_flw_fa0_a0(&mut self, off: i32) {
        let imm = off as u32 & 0xfff;
        let insn = imm << 20 | 10 << 15 | 0b010 << 12 | 10 << 7 | 0x07;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_fadd_fa0(&mut self) {
        let insn: u32 = 0x08000053 | 10 << 20 | 10 << 15 | 10 << 7;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// Move fa0 to fa1: FSGNJ.S fa1, fa0, fa0
    pub(crate) fn mv_fa1_fa0(&mut self) {
        self.emit_fsgnj_s(11, 10, 10);
    }

    /// Move fa1 to fa2: FSGNJ.S fa2, fa1, fa1
    pub(crate) fn mv_fa2_fa1(&mut self) {
        self.emit_fsgnj_s(12, 11, 11);
    }

    /// FADD.S fd, fs1, fs2: single-precision add
    pub(crate) fn emit_fadd_s(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn = (((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7) | 0x53;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// FSUB.S fd, fs1, fs2: single-precision subtract
    pub(crate) fn emit_fsub_s(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0000100 << 25 | 0x53;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// FMUL.S fd, fs1, fs2: single-precision multiply
    pub(crate) fn emit_fmul_s(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0001000 << 25 | 0x53;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// FDIV.S fd, fs1, fs2: single-precision divide
    pub(crate) fn emit_fdiv_s(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0001100 << 25 | 0x53;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// FADD.S fa0, fa1, fa0 (convenience for binary op)
    pub(crate) fn emit_fadd_s_fa0_fa1_fa0(&mut self) {
        self.emit_fadd_s(10, 11, 10);
    }

    /// FSUB.S fa0, fa1, fa0 (convenience for binary op)
    pub(crate) fn emit_fsub_s_fa0_fa1_fa0(&mut self) {
        self.emit_fsub_s(10, 11, 10);
    }

    /// FMUL.S fa0, fa1, fa0 (convenience for binary op)
    pub(crate) fn emit_fmul_s_fa0_fa1_fa0(&mut self) {
        self.emit_fmul_s(10, 11, 10);
    }

    /// FDIV.S fa0, fa1, fa0 (convenience for binary op)
    pub(crate) fn emit_fdiv_s_fa0_fa1_fa0(&mut self) {
        self.emit_fdiv_s(10, 11, 10);
    }

    /// FEQ.S rd, fs1, fs2: single-precision equal comparison
    pub(crate) fn emit_feq_s(&mut self, rd: u8, fs1: u8, fs2: u8) {
        let insn = (fs2 as u32) << 20
            | (fs1 as u32) << 15
            | 0b010 << 12
            | (rd as u32) << 7
            | 0b1010000 << 25
            | 0x53;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// FLT.S rd, fs1, fs2: single-precision less-than comparison
    pub(crate) fn emit_flt_s(&mut self, rd: u8, fs1: u8, fs2: u8) {
        let insn = (fs2 as u32) << 20
            | (fs1 as u32) << 15
            | 0b001 << 12
            | (rd as u32) << 7
            | 0b1010000 << 25
            | 0x53;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// FLE.S rd, fs1, fs2: single-precision less-equal comparison
    pub(crate) fn emit_fle_s(&mut self, rd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (rd as u32) << 7 | 0b1010000 << 25 | 0x53;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// FSQRT.S fd, fs1: 单精度平方根, funct7=0101101
    pub(crate) fn emit_fsqrt_s(&mut self, fd: u8, fs1: u8) {
        let insn = ((fs1 as u32) << 15) | (fd as u32) << 7 | 0b0101101 << 25 | 0x53;
        // rm field (bits [14:12]) = 000 (RNE) — encoded as part of funct7 shift
        let insn = insn & !(0b111 << 12); // clear rm bits
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// FMIN.S fd, fs1, fs2: 单精度最小值, funct7=0010101, funct3=000
    pub(crate) fn emit_fmin_s(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0010100 << 25 | 0x53;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// FMAX.S fd, fs1, fs2: 单精度最大值, funct7=0010101, funct3=001
    pub(crate) fn emit_fmax_s(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn = (fs2 as u32) << 20
            | (fs1 as u32) << 15
            | 0b001 << 12
            | (fd as u32) << 7
            | 0b0010100 << 25
            | 0x53;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }
}
