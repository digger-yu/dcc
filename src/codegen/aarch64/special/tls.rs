// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::aarch64::codegen::Aarch64Codegen;
use crate::codegen::aarch64::insn::Insn;
use crate::codegen::shared::elf::ElfRela;

impl Aarch64Codegen {
    pub(crate) fn emit_tlsdesc_access(&mut self, sym: &str) {
        // adrp x0, :tlsdesc:var (R_AARCH64_TLSDESC_ADR_PAGE21)
        let insn_adrp = 0x90000000u32; // adrp x0, #0 (placeholder)
        self.emit_insn(&Insn::raw(insn_adrp));
        let adrp_pos = self.pos() - 4;
        let idx = self.ensure_sym(sym);
        self.relocations.push(ElfRela {
            offset: adrp_pos,
            sym_index: idx,
            rel_type: crate::codegen::shared::elf::R_AARCH64_TLSDESC_ADR_PAGE21,
            addend: 0,
        });
        // ldr x1, [x0, :tlsdesc_lo12:var] (R_AARCH64_TLSDESC_LD64_LO12)
        let insn_ldr = 0xf9400001u32; // ldr x1, [x0]
        self.emit_insn(&Insn::raw(insn_ldr));
        let ldr_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: ldr_pos,
            sym_index: idx,
            rel_type: crate::codegen::shared::elf::R_AARCH64_TLSDESC_LD64_LO12,
            addend: 0,
        });
        // add x0, x0, :tlsdesc_lo12:var (R_AARCH64_TLSDESC_ADD_LO12)
        let insn_add = 0x91000000u32; // add x0, x0, #0
        self.emit_insn(&Insn::raw(insn_add));
        let add_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: add_pos,
            sym_index: idx,
            rel_type: crate::codegen::shared::elf::R_AARCH64_TLSDESC_ADD_LO12,
            addend: 0,
        });
        // blr x1 (TLSDESC call)
        if self.mte_active() {
            self.mte_untag_sp_for_call();
        }
        let insn_blr = 0xd63f0020u32; // blr x1
        self.emit_insn(&Insn::raw(insn_blr));
        if self.mte_active() {
            self.mte_retag_sp_after_call();
        }
        // mrs x1, tpidr_el0
        let insn_mrs = 0xd53bd041u32; // mrs x1, tpidr_el0
        self.emit_insn(&Insn::raw(insn_mrs));
        // add x0, x0, x1
        let insn_add2 = 0x8b010000u32; // add x0, x0, x1
        self.emit_insn(&Insn::raw(insn_add2));
    }

    /// Optimized TLSDESC 访问序列 (binutils 2.33+ / glibc 2.33+)
    /// 用 MOVW 对代替 ADRP, 消除页对齐限制并允许 linker 选择更短序列.
    /// 序列: movz x0, #:tlsdesc_off_g1:var
    ///       movk x0, #:tlsdesc_off_g0_nc:var
    ///       ldr  x1, [x0]
    ///       add  x0, x0, x1
    ///       blr  x1
    ///       mrs  x1, tpidr_el0
    ///       add  x0, x0, x1
    pub(crate) fn emit_tlsdesc_access_opt(&mut self, sym: &str) {
        let idx = self.ensure_sym(sym);
        // movz x0, #:tlsdesc_off_g1:var (R_AARCH64_TLSDESC_OFF_G1)
        let insn_movz = 0xd2a00000u32; // movz x0, #0, lsl #16
        self.emit_insn(&Insn::raw(insn_movz));
        let movz_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: movz_pos,
            sym_index: idx,
            rel_type: crate::codegen::shared::elf::R_AARCH64_TLSDESC_OFF_G1,
            addend: 0,
        });
        // movk x0, #:tlsdesc_off_g0_nc:var (R_AARCH64_TLSDESC_OFF_G0_NC)
        let insn_movk = 0xf2800000u32; // movk x0, #0
        self.emit_insn(&Insn::raw(insn_movk));
        let movk_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: movk_pos,
            sym_index: idx,
            rel_type: crate::codegen::shared::elf::R_AARCH64_TLSDESC_OFF_G0_NC,
            addend: 0,
        });
        // ldr x1, [x0] (R_AARCH64_TLSDESC_LDR — marker for ld.so)
        let insn_ldr = 0xf9400001u32; // ldr x1, [x0]
        self.emit_insn(&Insn::raw(insn_ldr));
        let ldr_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: ldr_pos,
            sym_index: idx,
            rel_type: crate::codegen::shared::elf::R_AARCH64_TLSDESC_LDR,
            addend: 0,
        });
        // add x0, x0, x1 (R_AARCH64_TLSDESC_ADD — marker for ld.so)
        let insn_add = 0x8b010000u32; // add x0, x0, x1
        self.emit_insn(&Insn::raw(insn_add));
        let add_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: add_pos,
            sym_index: idx,
            rel_type: crate::codegen::shared::elf::R_AARCH64_TLSDESC_ADD,
            addend: 0,
        });
        // blr x1 (R_AARCH64_TLSDESC_CALL — marker for ld.so)
        if self.mte_active() {
            self.mte_untag_sp_for_call();
        }
        let insn_blr = 0xd63f0020u32; // blr x1
        self.emit_insn(&Insn::raw(insn_blr));
        if self.mte_active() {
            self.mte_retag_sp_after_call();
        }
        let blr_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: blr_pos,
            sym_index: idx,
            rel_type: crate::codegen::shared::elf::R_AARCH64_TLSDESC_CALL,
            addend: 0,
        });
        // mrs x1, tpidr_el0
        self.emit_insn(&Insn::raw(0xd53bd041));
        // add x0, x0, x1
        self.emit_insn(&Insn::raw(0x8b010000));
    }

    /// Initial Exec TLS 访问: 通过 GOT 获取 tpoff
    /// 序列: adrp x0, :gottprel:var
    ///       ldr  x0, [x0, :gottprel_lo12:var]
    ///       mrs  x1, tpidr_el0
    ///       add  x0, x0, x1
    pub(crate) fn emit_tls_ie_access(&mut self, sym: &str) {
        // adrp x0, :gottprel:var
        let insn_adrp = 0x90000000u32;
        self.emit_insn(&Insn::raw(insn_adrp));
        let adrp_pos = self.pos() - 4;
        let idx = self.ensure_sym(sym);
        self.relocations.push(ElfRela {
            offset: adrp_pos,
            sym_index: idx,
            rel_type: crate::codegen::shared::elf::R_AARCH64_TLSIE_ADR_GOTTPREL_PAGE21,
            addend: 0,
        });
        // ldr x0, [x0, :gottprel_lo12:var]
        let insn_ldr = 0xf9400000u32; // ldr x0, [x0]
        self.emit_insn(&Insn::raw(insn_ldr));
        let ldr_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: ldr_pos,
            sym_index: idx,
            rel_type: crate::codegen::shared::elf::R_AARCH64_TLSIE_LD64_GOTTPREL_LO12_NC,
            addend: 0,
        });
        // mrs x1, tpidr_el0
        self.emit_insn(&Insn::raw(0xd53bd041));
        // add x0, x0, x1
        self.emit_insn(&Insn::raw(0x8b010000));
    }

    /// Local Exec TLS 访问: 直接 TP + offset
    /// 序列: movz x0, #:tprel_g1:var
    ///       movk x0, #:tprel_g0_nc:var
    ///       mrs  x1, tpidr_el0
    ///       add  x0, x0, x1
    pub(crate) fn emit_tls_le_access(&mut self, sym: &str) {
        // movz x0, #:tprel_g1:var
        let insn_movz = 0xd2a00000u32; // movz x0, #0, lsl #16
        self.emit_insn(&Insn::raw(insn_movz));
        let movz_pos = self.pos() - 4;
        let idx = self.ensure_sym(sym);
        self.relocations.push(ElfRela {
            offset: movz_pos,
            sym_index: idx,
            rel_type: crate::codegen::shared::elf::R_AARCH64_TLSLE_MOVW_TPREL_G1,
            addend: 0,
        });
        // movk x0, #:tprel_g0_nc:var
        let insn_movk = 0xf2800000u32; // movk x0, #0
        self.emit_insn(&Insn::raw(insn_movk));
        let movk_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: movk_pos,
            sym_index: idx,
            rel_type: crate::codegen::shared::elf::R_AARCH64_TLSLE_MOVW_TPREL_G0_NC,
            addend: 0,
        });
        // mrs x1, tpidr_el0
        self.emit_insn(&Insn::raw(0xd53bd041));
        // add x0, x0, x1
        self.emit_insn(&Insn::raw(0x8b010000));
    }

    // ========================================================================
    // 原子操作内建函数 (__atomic_* 系列)
    // ========================================================================
}
