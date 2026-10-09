// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::riscv64::codegen::Riscv64Codegen;

impl Riscv64Codegen {
    // ===== 原子操作辅助指令 =====

    /// AMOSWAP - 原子交换
    pub(crate) fn emit_amoswap(&mut self, sz: usize, rd: u8, rs2: u8, rs1: u8) {
        let insn = match sz {
            4 => {
                0x0800202fu32 // amoswap.w.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7)
            },
            _ => {
                0x0800302fu32 // amoswap.d.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7)
            },
        };
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// AMOADD - 原子加
    pub(crate) fn emit_amoadd(&mut self, sz: usize, rd: u8, rs2: u8, rs1: u8) {
        let insn = match sz {
            4 => {
                0x0000202fu32 // amoadd.w.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7)
            },
            _ => {
                0x0000302fu32 // amoadd.d.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7)
            },
        };
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// AMOAND - 原子与
    pub(crate) fn emit_amoand(&mut self, sz: usize, rd: u8, rs2: u8, rs1: u8) {
        let insn = match sz {
            4 => {
                0x6000202fu32 // amoand.w.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7)
            },
            _ => {
                0x6000302fu32 // amoand.d.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7)
            },
        };
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// AMOOR - 原子或
    pub(crate) fn emit_amoor(&mut self, sz: usize, rd: u8, rs2: u8, rs1: u8) {
        let insn = match sz {
            4 => {
                0x4000202fu32 // amoor.w.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7)
            },
            _ => {
                0x4000302fu32 // amoor.d.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7)
            },
        };
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// AMOXOR - 原子异或
    pub(crate) fn emit_amoxor(&mut self, sz: usize, rd: u8, rs2: u8, rs1: u8) {
        let insn = match sz {
            4 => {
                0x2000202fu32 // amoxor.w.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7)
            },
            _ => {
                0x2000302fu32 // amoxor.d.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7)
            },
        };
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// LR (Load Reserved) - 原子加载
    pub(crate) fn emit_lr(&mut self, sz: usize, rd: u8, rs1: u8) {
        let insn = match sz {
            4 => {
                0x1000202fu32 // lr.w.aq
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7)
            },
            _ => {
                0x1000302fu32 // lr.d.aq
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7)
            },
        };
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// SC (Store Conditional) - 条件存储
    pub(crate) fn emit_sc(&mut self, sz: usize, rd: u8, rs2: u8, rs1: u8) {
        let insn = match sz {
            4 => {
                0x1800202fu32 // sc.w.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7)
            },
            _ => {
                0x1800302fu32 // sc.d.aq
                | ((rs2 as u32 & 0x1f) << 20)
                | ((rs1 as u32 & 0x1f) << 15)
                | ((rd as u32 & 0x1f) << 7)
            },
        };
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// 按尺寸加载 (用于原子操作)
    pub(crate) fn emit_load_sz(&mut self, rd: u8, rs1: u8, imm: i32, sz: usize) {
        match sz {
            1 => self.emit_lb(rd, rs1, imm),
            2 => self.emit_lh(rd, rs1, imm),
            4 => self.emit_lw(rd, rs1, imm),
            _ => self.emit_ld(rd, rs1, imm),
        }
    }

    /// 按尺寸存储 (用于原子操作)
    pub(crate) fn emit_store_sz(&mut self, rs2: u8, rs1: u8, imm: i32, sz: usize) {
        match sz {
            1 => self.emit_sb(rs2, rs1, imm),
            2 => self.emit_sh(rs2, rs1, imm),
            4 => self.emit_sw(rs2, rs1, imm),
            _ => self.emit_sd(rs2, rs1, imm),
        }
    }

    /// NEG - 取反 (sub rd, x0, rs)
    pub(crate) fn emit_neg(&mut self, rd: u8, rs: u8) {
        self.emit_sub(rd, 0, rs);
    }

    /// BNEZ - 分支非零
    pub(crate) fn emit_bnez(&mut self, rs: u8, label: &str) {
        self.emit_bne(rs, 0, label);
    }

    // ========================================================================
    // TLS 访问序列 (Local Exec)
    // ========================================================================
}
