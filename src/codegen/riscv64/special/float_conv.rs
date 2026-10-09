// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::riscv64::codegen::Riscv64Codegen;

impl Riscv64Codegen {
    pub(crate) fn emit_fcvt_s_w_fa0(&mut self) {
        let insn = 0xD0000053u32 | (10 << 7) | (10 << 15);
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn patch_jal(&mut self, at: u64, target: u64) {
        let offset = target as i64 - at as i64;
        debug_assert!(
            Self::jal_offset_in_range(offset),
            "JAL offset {} out of ±1MiB range",
            offset
        );
        let off = offset as u32;
        let insn = ((off >> 12) & 0xff) << 12
            | ((off >> 11) & 1) << 20
            | ((off >> 1) & 0x3ff) << 21
            | ((off >> 20) & 1) << 31
            | 1 << 7
            | 0x6f;
        let pos = at as usize;
        if pos + 4 <= self.code.len() {
            self.code[pos..pos + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    pub(crate) fn patch_beq(&mut self, at: u64, rs1: u8, rs2: u8, target: u64) {
        let offset = (target as i64 - at as i64) as i32;
        let off = offset as u32;
        let insn = (((off >> 12) & 1) << 31
            | ((off >> 5) & 0x3f) << 25
            | (rs2 as u32) << 20
            | (rs1 as u32) << 15)
            | ((off >> 1) & 0xf) << 8
            | ((off >> 11) & 1) << 7
            | 0x63;
        let pos = at as usize;
        if pos + 4 <= self.code.len() {
            self.code[pos..pos + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    pub(crate) fn patch_beqz(&mut self, at: u64, target: u64) {
        self.patch_beq(at, 10, 0, target);
    }

    pub(crate) fn patch_auipc_load(&mut self, at: u64, addr: u64) {
        let pc = at;
        let delta = (addr as i64 - pc as i64) as i32;
        let upper = (delta + 0x800) >> 12;
        let lower = delta - (upper << 12);
        let lui = ((upper as u32 & 0xfffff) << 12) | 10 << 7 | 0x17;
        let pos = at as usize;
        if pos + 4 <= self.code.len() {
            self.code[pos..pos + 4].copy_from_slice(&lui.to_le_bytes());
        }
        if pos + 8 <= self.code.len() {
            let flw = (lower as u32 & 0xfff) << 20 | 10 << 15 | 0b010 << 12 | 10 << 7 | 0x07;
            self.code[pos + 4..pos + 8].copy_from_slice(&flw.to_le_bytes());
        }
    }

    /// Patch AUIPC + LD pair for 64-bit integer constant loading
    pub(crate) fn patch_auipc_ld(&mut self, at: u64, addr: u64) {
        let pc = at;
        let delta = (addr as i64 - pc as i64) as i32;
        let upper = (delta + 0x800) >> 12;
        let lower = delta - (upper << 12);
        let auipc = ((upper as u32 & 0xfffff) << 12) | 10 << 7 | 0x17;
        let pos = at as usize;
        if pos + 4 <= self.code.len() {
            self.code[pos..pos + 4].copy_from_slice(&auipc.to_le_bytes());
        }
        if pos + 8 <= self.code.len() {
            // LD a0, [a0 + lower]: funct3=011, opcode=0000011
            let ld = (lower as u32 & 0xfff) << 20 | 10 << 15 | 0b011 << 12 | 10 << 7 | 0x03;
            self.code[pos + 4..pos + 8].copy_from_slice(&ld.to_le_bytes());
        }
    }

    // ===== M8 新增: 压缩指令 (C 扩展) =====

    /// C.ADDI rd, imm (rd != 0, imm != 0): CI 格式
    /// 编码: funct3[15:13]=000, imm[5] bit12, rd bit11-7, imm[4:0] bit6-2, op bit1-0=01
    pub(crate) fn emit_c_addi(&mut self, rd: u8, imm: i8) {
        debug_assert!(rd != 0 && imm != 0, "C.ADDI: rd!=0, imm!=0");
        let imm = imm as u16;
        let insn: u16 = (((imm >> 5) & 1) << 12) | (rd as u16) << 7 | (imm & 0x1f) << 2 | 0x01;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw16(insn));
    }

    /// C.NOP: CI 格式, rd=0, imm=0
    pub(crate) fn emit_c_nop(&mut self) {
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw16(0x0001));
    }

    /// C.LI rd, imm (rd != 0): CI 格式
    pub(crate) fn emit_c_li(&mut self, rd: u8, imm: i8) {
        debug_assert!(rd != 0);
        let imm = imm as u16;
        let insn: u16 =
            0b010 << 13 | ((imm >> 5) & 1) << 12 | (rd as u16) << 7 | (imm & 0x1f) << 2 | 0x01;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw16(insn));
    }

    /// C.LW rd', rs1', offset: CL 格式 (32-bit load)
    pub(crate) fn emit_c_lw(&mut self, rd_prime: u8, rs1_prime: u8, offset: u8) {
        debug_assert!(rd_prime < 8 && rs1_prime < 8);
        let insn: u16 = 0b010 << 13
            | (((offset >> 3) & 0x7) as u16) << 10
            | ((rs1_prime & 7) as u16) << 7
            | (((offset >> 2) & 1) as u16) << 6
            | (((offset >> 6) & 1) as u16) << 5
            | ((rd_prime & 7) as u16) << 2;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw16(insn));
    }

    /// C.SW rs2', rs1', offset: CS 格式 (32-bit store)
    pub(crate) fn emit_c_sw(&mut self, rs2_prime: u8, rs1_prime: u8, offset: u8) {
        debug_assert!(rs2_prime < 8 && rs1_prime < 8);
        let insn: u16 = 0b110 << 13
            | (((offset >> 3) & 0x7) as u16) << 10
            | ((rs1_prime & 7) as u16) << 7
            | (((offset >> 2) & 1) as u16) << 6
            | (((offset >> 6) & 1) as u16) << 5
            | ((rs2_prime & 7) as u16) << 2;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw16(insn));
    }

    /// C.J offset: CJ 格式, 20-bit signed offset (bit 0 = 0)
    pub(crate) fn emit_c_j(&mut self, offset: i32) {
        let off = offset as u32;
        let insn: u16 = 0b101 << 13
            | (((off >> 11) & 1) as u16) << 12
            | (((off >> 4) & 1) as u16) << 11
            | (((off >> 8) & 3) as u16) << 9
            | (((off >> 10) & 1) as u16) << 8
            | (((off >> 6) & 1) as u16) << 7
            | (((off >> 7) & 1) as u16) << 6
            | (((off >> 1) & 7) as u16) << 3
            | (((off >> 5) & 1) as u16) << 2
            | 0x01;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw16(insn));
    }

    /// C.BEQZ rs1', offset: CB 格式, 8-bit signed offset
    pub(crate) fn emit_c_beqz(&mut self, rs1_prime: u8, offset: i16) {
        debug_assert!(rs1_prime < 8);
        let off = offset as u16;
        let insn: u16 = 0b110 << 13
            | ((off >> 8) & 1) << 12
            | ((off >> 3) & 3) << 10
            | ((rs1_prime & 7) as u16) << 7
            | ((off >> 6) & 3) << 5
            | ((off >> 1) & 7) << 2
            | 0x01;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw16(insn));
    }

    /// C.LD rd', rs1', offset: CL 格式 (64-bit load, RV64)
    pub(crate) fn emit_c_ld(&mut self, rd_prime: u8, rs1_prime: u8, offset: u8) {
        debug_assert!(rd_prime < 8 && rs1_prime < 8);
        let insn: u16 = 0b011 << 13
            | (((offset >> 3) & 0x7) as u16) << 10
            | ((rs1_prime & 7) as u16) << 7
            | (((offset >> 6) & 3) as u16) << 5
            | ((rd_prime & 7) as u16) << 2
            | 0x01;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw16(insn));
    }

    /// C.SD rs2', rs1', offset: CS 格式 (64-bit store, RV64)
    pub(crate) fn emit_c_sd(&mut self, rs2_prime: u8, rs1_prime: u8, offset: u8) {
        debug_assert!(rs2_prime < 8 && rs1_prime < 8);
        let insn: u16 = 0b111 << 13
            | (((offset >> 3) & 0x7) as u16) << 10
            | ((rs1_prime & 7) as u16) << 7
            | (((offset >> 6) & 3) as u16) << 5
            | ((rs2_prime & 7) as u16) << 2
            | 0x01;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw16(insn));
    }

    /// C.MV rd, rs2 (rd != 0, rs2 != 0): CR 格式
    pub(crate) fn emit_c_mv(&mut self, rd: u8, rs2: u8) {
        debug_assert!(rd != 0 && rs2 != 0);
        let insn: u16 = 0b100 << 12 | (rd as u16) << 7 | (rs2 as u16) << 2 | 0x02;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw16(insn));
    }

    /// C.ADD rd, rs2 (rd != 0, rs2 != 0): CR 格式, rd == rs1
    pub(crate) fn emit_c_add(&mut self, rd: u8, rs2: u8) {
        debug_assert!(rd != 0 && rs2 != 0);
        let insn: u16 = 0b100 << 12 | 1 << 12 | (rd as u16) << 7 | (rs2 as u16) << 2 | 0x02;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw16(insn));
    }

    /// C.LUI rd, imm (rd != 0, rd != 2, imm != 0, -32..=31): CI 格式
    pub(crate) fn emit_c_lui(&mut self, rd: u8, imm: i8) {
        debug_assert!(rd != 0 && rd != 2 && imm != 0);
        let imm = imm as u16;
        let insn: u16 =
            0b011 << 13 | ((imm >> 5) & 1) << 12 | (rd as u16) << 7 | (imm & 0x1f) << 2 | 0x01;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw16(insn));
    }

    /// C.BNEZ rs1', offset: CB 格式, 8-bit signed offset
    pub(crate) fn emit_c_bnez(&mut self, rs1_prime: u8, offset: i16) {
        debug_assert!(rs1_prime < 8);
        let off = offset as u16;
        let insn: u16 = 0b111 << 13
            | ((off >> 8) & 1) << 12
            | ((off >> 3) & 3) << 10
            | ((rs1_prime & 7) as u16) << 7
            | ((off >> 6) & 3) << 5
            | ((off >> 1) & 7) << 2
            | 0x01;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw16(insn));
    }

    /// C.ADDIW rd, imm (rd != 0, -32..=31): CI 格式, RV64
    pub(crate) fn emit_c_addiw(&mut self, rd: u8, imm: i8) {
        debug_assert!(rd != 0);
        let imm = imm as u16;
        let insn: u16 =
            0b001 << 13 | ((imm >> 5) & 1) << 12 | (rd as u16) << 7 | (imm & 0x1f) << 2 | 0x01;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw16(insn));
    }

    // ===== M8 新增: FP 扩展子集 (D 扩展双精度) =====
}
