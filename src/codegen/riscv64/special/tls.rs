// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::riscv64::codegen::{Riscv64Codegen};
use crate::codegen::shared::elf::{self, ElfRela};

impl Riscv64Codegen {
    /// Local Exec TLS 访问: 直接 TP + offset
    /// 序列: lui  a0, %tprel_hi20(sym)
    ///       addi a0, a0, %tprel_add_lo12(sym)
    ///       add  a0, a0, tp
    /// 对于小偏移 (fits in 12 bits), 简化为:
    ///       addi a0, tp, %tprel_add_lo12(sym)
    pub(crate) fn emit_tls_le_access(&mut self, sym: &str) {
        // 获取 TLS 变量偏移
        let off = self.tls_globals.get(sym).copied().unwrap_or(0);
        let idx = self.ensure_sym(sym);

        if off < 2048 {
            // 小偏移: 直接用 addi tp, a0
            // addi a0, tp, imm (I-type)
            let imm = (off as u32) & 0xfff;
            let insn = ((imm << 20) | (4u32 << 15)) | (10u32 << 7) | 0x13;
            self.emit(&insn.to_le_bytes());
        } else {
            // 大偏移: lui + addi + add
            // lui a0, %tprel_hi20(sym)
            let hi20 = ((off as i64 + 0x800) >> 12) as u32;
            let insn_lui = (hi20 << 12) | (10u32 << 7) | 0x37;
            self.emit(&insn_lui.to_le_bytes());
            let lui_pos = self.pos() - 4;
            self.relocations.push(ElfRela {
                offset: lui_pos,
                sym_index: idx,
                rel_type: elf::R_RISCV_TPREL_HI20,
                addend: 0,
            });
            // addi a0, a0, %tprel_add_lo12(sym)
            let lo12 = (off as i32) & 0xfff;
            let insn_addi = (((lo12 as u32) << 20) | (10u32 << 15)) | (10u32 << 7) | 0x13;
            self.emit(&insn_addi.to_le_bytes());
            let addi_pos = self.pos() - 4;
            self.relocations.push(ElfRela {
                offset: addi_pos,
                sym_index: idx,
                rel_type: elf::R_RISCV_TPREL_LO12_I,
                addend: 0,
            });
            // add a0, a0, tp (x4)
            // add rd, rs1, rs2: 0x33 | rs2<<20 | rs1<<15 | funct3<<12 | rd<<7
            let insn_add = ((4u32 << 20) | (10u32 << 15)) | (10u32 << 7) | 0x33;
            self.emit(&insn_add.to_le_bytes());
        }
    }

    /// Initial Exec TLS access: load TP offset from GOT, then add TP
    /// Sequence: lui  a5, %got_tpoff_hi20(sym)
    ///           ld   a5, %got_tpoff_lo12(sym)(a5)
    ///           add  a0, a5, tp
    /// Used for dynamically linked TLS variables where the TP offset
    /// is not known at link time.
    pub(crate) fn emit_tls_ie_access(&mut self, sym: &str) {
        let idx = self.ensure_sym(sym);
        // lui a5, %got_tpoff_hi20(sym)
        let insn_lui = (15u32 << 7) | 0x37;
        self.emit(&insn_lui.to_le_bytes());
        let lui_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: lui_pos,
            sym_index: idx,
            rel_type: elf::R_RISCV_TPREL_HI20,
            addend: 0,
        });
        // ld a5, %got_tpoff_lo12(sym)(a5) - I-type load
        // ld rd, imm(rs1): funct3=011, opcode=0000011
        let insn_ld = (15u32 << 15) | (0b011 << 12) | (15u32 << 7) | 0x03;
        self.emit(&insn_ld.to_le_bytes());
        let ld_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: ld_pos,
            sym_index: idx,
            rel_type: elf::R_RISCV_TPREL_LO12_I,
            addend: 0,
        });
        // add a0, a5, tp (x4)
        let insn_add = ((4u32 << 20) | (15u32 << 15)) | (10u32 << 7) | 0x33;
        self.emit(&insn_add.to_le_bytes());
    }

    /// TLS Descriptor access: call descriptor to get TP offset
    /// Sequence: lui  a0, %tlsdesc_hi(sym)
    ///           ld   a0, %tlsdesc_load_lo12(sym)(a0)
    ///           addi a0, a0, %tlsdesc_add_lo12(sym)
    ///           jalr ra, a0, %tlsdesc_call(sym)
    ///           add  a0, a0, tp
    /// Most flexible TLS model, supports dynamic TLS allocation.
    pub(crate) fn emit_tls_desc_access(&mut self, sym: &str) {
        let idx = self.ensure_sym(sym);
        // lui a0, %tlsdesc_hi(sym)
        let insn_lui = (10u32 << 7) | 0x37;
        self.emit(&insn_lui.to_le_bytes());
        let lui_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: lui_pos,
            sym_index: idx,
            rel_type: elf::R_RISCV_TLSDESC_HI20,
            addend: 0,
        });
        // ld a0, %tlsdesc_load_lo12(sym)(a0)
        let insn_ld = (10u32 << 15) | (0b011 << 12) | (10u32 << 7) | 0x03;
        self.emit(&insn_ld.to_le_bytes());
        let ld_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: ld_pos,
            sym_index: idx,
            rel_type: elf::R_RISCV_TLSDESC_LOAD_LO12,
            addend: 0,
        });
        // addi a0, a0, %tlsdesc_add_lo12(sym)
        let insn_addi = (10u32 << 15) | (10u32 << 7) | 0x13;
        self.emit(&insn_addi.to_le_bytes());
        let addi_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: addi_pos,
            sym_index: idx,
            rel_type: elf::R_RISCV_TLSDESC_ADD_LO12,
            addend: 0,
        });
        // jalr ra, a0, %tlsdesc_call(sym)
        // jalr rd, rs1, imm: opcode=1101100, rd=ra(1), rs1=a0(10)
        let insn_jalr = (10u32 << 15) | (1u32 << 7) | 0x67;
        self.emit(&insn_jalr.to_le_bytes());
        let jalr_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: jalr_pos,
            sym_index: idx,
            rel_type: elf::R_RISCV_TLSDESC_CALL,
            addend: 0,
        });
        // add a0, a0, tp (x4)
        let insn_add = ((4u32 << 20) | (10u32 << 15)) | (10u32 << 7) | 0x33;
        self.emit(&insn_add.to_le_bytes());
    }

}
