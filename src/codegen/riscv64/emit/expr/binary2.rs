// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::riscv64::codegen::{align_rodata, BranchKind, Riscv64Codegen};
use crate::codegen::shared::elf::{
    self, ElfRela, ElfSym, R_RISCV_JAL, STB_GLOBAL, STT_FUNC, STT_OBJECT,
};

impl Riscv64Codegen {
    pub(crate) fn mv_fa_reg(&mut self, idx: u8, bits: u16) {
        let rd = 10 + idx;
        if bits <= 32 {
            self.emit_fsgnj_s(rd, 10, 10);
        } else {
            self.emit_fsgnj_d(rd, 10, 10);
        }
    }

    // M7 修复: 计算 sp + off 的地址到指定 a 寄存器
    // 编码: add rd, sp, #off   (off >= 0)   => 0x0000_? ? ?B 形式 addi
    //       addi rd, sp, #off  (off 在 ±2048)
    // 限制: 偏移 12-bit 有符号; 越界需 auipc+addi 序列
    pub(crate) fn mov_a_reg_sp_off(&mut self, reg: u8, off: i32) {
        if (-2048..=2047).contains(&off) {
            self.emit_addi(reg, 2, off);
        } else {
            // 越界: 走 lui + addi 序列 (与 li_reg 一致)
            self.li_reg(18, off); // t0 = off (临时)
            self.emit_add(reg, reg, 18);
        }
    }

    pub(crate) fn mov_a0_sp_off(&mut self, off: i32) {
        self.mov_a_reg_sp_off(10, off);
    }

    pub(crate) fn neg_a0(&mut self) {
        self.emit_sub(10, 0, 10);
    }

    pub(crate) fn not_a0(&mut self) {
        self.emit_xori(10, 10, -1);
    }

    pub(crate) fn cmp_a0_zero(&mut self) {
        self.emit_sltu(10, 0, 10);
    }

    pub(crate) fn addi_sp(&mut self, delta: i32) {
        self.emit_addi(2, 2, delta);
    }

    /// -fstack-clash-protection: emit page-sized probes when allocating a
    /// large stack frame.  Each probe stores a zero byte at [sp] to trigger
    /// a fault on the guard page before the kernel maps it.
    pub(crate) fn emit_stack_clash_probes(&mut self, frame_size: u32) {
        let page: u32 = 4096;
        let remainder = frame_size % page;
        let pages = frame_size / page;
        // First: addi sp, sp, -remainder (if any, must fit in 12-bit signed)
        if remainder > 0 {
            self.addi_sp(-(remainder as i32));
        }
        if pages > 0 {
            // Probe loop using t0 (x5, scratch) as counter
            // li t0, pages  (via LUI + ADDI or C.LI)
            self.emit_lui(5, (pages as i32 + 0x800) >> 12);
            let lower = (pages as i32) - (((pages as i32 + 0x800) >> 12) << 12);
            if lower != 0 {
                self.emit_addi(5, 5, lower);
            }
            // .probe_loop:
            let loop_start = self.pos();
            // addi sp, sp, -2048  (two instructions for 4096)
            self.emit_addi(2, 2, -2048);
            self.emit_addi(2, 2, -2048);
            // sb zero, 0(sp)  — probe the page
            self.emit_sb(0, 2, 0);
            // addi t0, t0, -1
            self.emit_addi(5, 5, -1);
            // bne t0, zero, .probe_loop
            let loop_end = self.pos();
            let offset = loop_start as i64 - loop_end as i64;
            let off = offset as u32;
            let bne = (((off >> 12) & 1) << 31 | ((off >> 5) & 0x3f) << 25)
                | (5u32) << 15
                | ((off >> 1) & 0xf) << 8
                | ((off >> 11) & 1) << 7
                | (1u32) << 12
                | 0x63;
            self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(bne));
        }
    }

    pub(crate) fn ret(&mut self) {
        // Stack protector epilogue: verify canary BEFORE frame teardown
        if self._target.stack_protector && self.canary_slot_off > 0 {
            self.emit_stack_chk_epilogue(self.canary_slot_off);
        }
        // Restore ra (if frame size >0, saved ra earlier)
        if self.ret_addr_off >= 0 {
            self.emit_ld(1, 2, self.ret_addr_off); // ld x1, ret_addr_off(sp)
        }
        if self.frame_bytes > 0 {
            self.addi_sp(self.frame_bytes);
        }
        self.emit_jalr(0, 1, 0);
    }

    pub(crate) fn ecall(&mut self) {
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::ecall());
    }

    pub(crate) fn beqz_a0(&mut self, label: &str) {
        let pos = self.pos();
        self.emit_beq(10, 0, 0);
        if let Some(&target) = self.labels.get(label) {
            self.patch_beq(pos, 10, 0, target);
        } else {
            self.pending_branches
                .push((pos, label.to_string(), BranchKind::Beqz));
        }
    }

    pub(crate) fn j(&mut self, label: &str) {
        let pos = self.pos();
        self.emit_jal(0, 0);
        if let Some(&target) = self.labels.get(label) {
            self.patch_jal(pos, target);
        } else {
            self.pending_branches
                .push((pos, label.to_string(), BranchKind::Jal));
        }
    }

    pub(crate) fn emit_jal_placeholder(&mut self, rd: u8) {
        // M8: JAL 范围占位, 后续 patch_jal 填充偏移
        // JAL 偏移范围: ±1MiB (21-bit 有符号, bit 0 恒为 0)
        self.emit_jal(rd, 0);
    }

    /// JAL 偏移范围校验 (±1MiB)
    pub(crate) fn jal_offset_in_range(diff: i64) -> bool {
        (-(1 << 20)..(1 << 20)).contains(&diff)
    }

    pub(crate) fn emit_jal_to(&mut self, name: &str) {
        if let Some(&target) = self.labels.get(name) {
            let pos = self.pos();
            self.emit_jal(1, 0);
            self.patch_jal(pos, target);
        } else if self.relocatable {
            let pos = self.pos();
            self.emit_jal(1, 0);
            let idx = self.ensure_sym(name);
            self.relocations.push(ElfRela {
                offset: pos,
                sym_index: idx,
                rel_type: R_RISCV_JAL,
                addend: 0,
            });
        } else {
            self.emit_jal(1, 0);
        }
    }

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
            visibility: elf::STV_DEFAULT,
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
            ty: STT_OBJECT,
            visibility: elf::STV_DEFAULT,
        });
        let idx = self.symbols.len() as u32;
        self.sym_index.insert(name.to_string(), idx);
        idx
    }

    /// Stack protector prologue: load canary from __stack_chk_guard
    /// and store at [sp, #canary_off]. Uses t0/t1 as scratch.
    pub(crate) fn emit_stack_chk_prologue(&mut self, canary_off: i32) {
        // Load __stack_chk_guard address using AUIPC + LD
        self.ensure_sym_object("__stack_chk_guard");
        let sym_idx = self.sym_index["__stack_chk_guard"];
        let pos = self.pos();
        // AUIPC t0, %hi(__stack_chk_guard)
        self.emit_auipc(5, 0); // t0 = x5
        self.relocations.push(ElfRela {
            offset: pos,
            sym_index: sym_idx,
            rel_type: elf::R_RISCV_PCREL_HI20,
            addend: 0,
        });
        let pos2 = self.pos();
        // LD t0, %lo(__stack_chk_guard)(t0)
        self.emit_ld(5, 5, 0);
        self.rodata_relocations.push(ElfRela {
            offset: pos2,
            sym_index: sym_idx,
            rel_type: elf::R_RISCV_PCREL_LO12_I,
            addend: 0,
        });
        // SD t0, canary_off(sp)
        self.emit_sd(5, 2, canary_off);
    }

    /// Stack protector epilogue: verify canary at [sp, #canary_off]
    /// against __stack_chk_guard. Calls __stack_chk_fail on mismatch.
    pub(crate) fn emit_stack_chk_epilogue(&mut self, canary_off: i32) {
        let fail_label = self.fresh_label(".Lstack_chk_fail_");
        // LD t0, canary_off(sp) - load stored canary
        self.emit_ld(5, 2, canary_off);
        // Load expected canary from __stack_chk_guard
        self.ensure_sym_object("__stack_chk_guard");
        let sym_idx = self.sym_index["__stack_chk_guard"];
        let pos = self.pos();
        // AUIPC t1, %hi(__stack_chk_guard)
        self.emit_auipc(6, 0); // t1 = x6
        self.relocations.push(ElfRela {
            offset: pos,
            sym_index: sym_idx,
            rel_type: elf::R_RISCV_PCREL_HI20,
            addend: 0,
        });
        let pos2 = self.pos();
        // LD t1, %lo(__stack_chk_guard)(t1)
        self.emit_ld(6, 6, 0);
        self.rodata_relocations.push(ElfRela {
            offset: pos2,
            sym_index: sym_idx,
            rel_type: elf::R_RISCV_PCREL_LO12_I,
            addend: 0,
        });
        // BNE t0, t1, fail_label
        self.emit_bne(5, 6, &fail_label);
        // Normal path continues
        // Fail path: call __stack_chk_fail (never returns)
        self.label(&fail_label);
        self.ensure_sym("__stack_chk_fail");
        self.emit_jal_to("__stack_chk_fail");
    }

    // ===== RISC-V 指令编码 =====

    pub(crate) fn emit_addi(&mut self, rd: u8, rs1: u8, imm: i32) {
        // 压缩指令自动选择: C.ADDI / C.LI / C.NOP
        if imm == 0 && rd == rs1 {
            // ADDI rd, rd, 0 => 不发射任何指令 (NOP 语义, 但 C.NOP 仅 rd=0)
            // 实际上 ADDI x0, x0, 0 是标准 NOP, 其他情况保留 32-bit
        } else if rs1 == 0 && rd != 0 && (-32..=31).contains(&imm) {
            // LI => C.LI
            self.emit_c_li(rd, imm as i8);
            return;
        } else if rd == rs1 && rd != 0 && (-32..=31).contains(&imm) && imm != 0 {
            // ADDI rd, rd, imm => C.ADDI
            self.emit_c_addi(rd, imm as i8);
            return;
        }
        let imm = imm as u32 & 0xfff;
        let insn = (imm << 20 | (rs1 as u32) << 15) | (rd as u32) << 7 | 0x13;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// MV rd, rs — pseudo-instruction: ADDI rd, rs, 0
    pub(crate) fn emit_mv(&mut self, rd: u8, rs: u8) {
        if rd == rs {
            return;
        }
        self.emit_addi(rd, rs, 0);
    }

    pub(crate) fn emit_lui(&mut self, rd: u8, imm: i32) {
        // 压缩指令自动选择: C.LUI
        // C.LUI: rd!=0, rd!=2, nzimm in -32..31, nzimm!=0
        let nzimm = (imm & 0xfffff) as u32;
        let signed_nzimm = if nzimm & 0x80000 != 0 {
            // Sign-extend 20-bit to 32-bit
            (nzimm as i32).wrapping_sub(0x100000)
        } else {
            nzimm as i32
        };
        if rd != 0 && rd != 2 && (-32..=31).contains(&signed_nzimm) && signed_nzimm != 0 {
            self.emit_c_lui(rd, signed_nzimm as i8);
            return;
        }
        let imm = (imm as u32 & 0xfffff) << 12;
        let insn = imm | (rd as u32) << 7 | 0x37;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_add(&mut self, rd: u8, rs1: u8, rs2: u8) {
        // 压缩指令自动选择: C.MV / C.ADD
        if rs1 == 0 && rd != 0 && rs2 != 0 {
            // ADD rd, x0, rs2 => C.MV rd, rs2
            self.emit_c_mv(rd, rs2);
            return;
        }
        if rd == rs1 && rd != 0 && rs2 != 0 {
            // ADD rd, rd, rs2 => C.ADD rd, rs2
            self.emit_c_add(rd, rs2);
            return;
        }
        let insn = ((rs2 as u32) << 20 | (rs1 as u32) << 15) | (rd as u32) << 7 | 0x33;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_sub(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = ((rs2 as u32) << 20 | (rs1 as u32) << 15) | (rd as u32) << 7 | 0x33 | 0x40000000;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_mul(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn =
            (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b0000001 << 12 | (rd as u32) << 7 | 0x33;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    // M7 修复: 整数除/取模 (RV64M 扩展)
    // div  : funct7=0000001, funct3=100 (有符号)
    // divu : funct7=0000001, funct3=101 (无符号)
    // rem  : funct7=0000001, funct3=110 (有符号)
    // remu : funct7=0000001, funct3=111 (无符号)
    // 编码: 0x02000000 = funct7=0000001 (bit 25)
    // 除 0 行为: 硬件返回 -1 (div) / -1 (rem) ;在 RV spec 中明确定义
    // 最小整数 -2^63 / -1 同样按 spec 行为, 不抛异常
    pub(crate) fn emit_div(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0b100 << 12
            | (rd as u32) << 7
            | 0x02000000
            | 0x33;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_divu(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0b101 << 12
            | (rd as u32) << 7
            | 0x02000000
            | 0x33;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_rem(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0b110 << 12
            | (rd as u32) << 7
            | 0x02000000
            | 0x33;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_remu(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0b111 << 12
            | (rd as u32) << 7
            | 0x02000000
            | 0x33;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    // M7 修复: 移位指令 (RV64I 基础)
    // sll : funct7=0000000, funct3=001 (逻辑左移)
    // srl : funct7=0000000, funct3=101 (逻辑右移)
    // sra : funct7=0100000, funct3=101 (算术右移)
    // 编码: sra 用 0x40000000 (bit 30 = funct7 的 bit 6)
    // IR 的 Shr 暂视为无符号 (srl), 有符号另由 AST 端 cast 处理
    pub(crate) fn emit_sll(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0x33;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_srl(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b101 << 12 | (rd as u32) << 7 | 0x33;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_sra(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0b101 << 12
            | (rd as u32) << 7
            | 0x40000000
            | 0x33;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_and(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b111 << 12 | (rd as u32) << 7 | 0x33;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_or(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b110 << 12 | (rd as u32) << 7 | 0x33;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_xor(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b100 << 12 | (rd as u32) << 7 | 0x33;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    pub(crate) fn emit_xori(&mut self, rd: u8, rs1: u8, imm: i32) {
        let imm = imm as u32 & 0xfff;
        let insn = imm << 20 | (rs1 as u32) << 15 | 0b100 << 12 | (rd as u32) << 7 | 0x13;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// SRLI rd, rs1, shamt — 64-bit logical right shift immediate
    pub(crate) fn emit_srli(&mut self, rd: u8, rs1: u8, shamt: u8) {
        if shamt == 0 {
            return;
        }
        let shamt = (shamt as u32) & 0x3f;
        // I-type: imm[5:0]=shamt, rs1, funct3=101, rd, opcode=0010011
        let insn = shamt << 20 | (rs1 as u32) << 15 | 0b101 << 12 | (rd as u32) << 7 | 0x13;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// SLLI rd, rs1, shamt — 64-bit logical left shift immediate
    pub(crate) fn emit_slli(&mut self, rd: u8, rs1: u8, shamt: u8) {
        if shamt == 0 {
            return;
        }
        let shamt = (shamt as u32) & 0x3f;
        let insn = shamt << 20 | (rs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0x13;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// SRAI rd, rs1, shamt — 64-bit arithmetic right shift immediate
    pub(crate) fn emit_srai(&mut self, rd: u8, rs1: u8, shamt: u8) {
        if shamt == 0 {
            return;
        }
        let shamt = (shamt as u32) & 0x3f;
        // I-type: imm[11:0] = 0100000|shamt[5:0], rs1, funct3=101, rd, opcode=0010011
        let insn =
            0x40000000 | shamt << 20 | (rs1 as u32) << 15 | 0b101 << 12 | (rd as u32) << 7 | 0x13;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// ANDI rd, rs1, imm — AND immediate (12-bit signed)
    pub(crate) fn emit_andi(&mut self, rd: u8, rs1: u8, imm: i32) {
        let imm = imm as u32 & 0xfff;
        let insn = imm << 20 | (rs1 as u32) << 15 | 0b111 << 12 | (rd as u32) << 7 | 0x13;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// ORI rd, rs1, imm — OR immediate (12-bit signed)
    pub(crate) fn emit_ori(&mut self, rd: u8, rs1: u8, imm: i32) {
        let imm = imm as u32 & 0xfff;
        let insn = imm << 20 | (rs1 as u32) << 15 | 0b110 << 12 | (rd as u32) << 7 | 0x13;
        self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(insn));
    }

    /// Load a 64-bit constant into a0 using AUIPC + LD from rodata.
    /// For constants that fit in 32 bits, uses li_reg (LUI+ADDI) instead.
    pub(crate) fn load_const64_a0(&mut self, val: u64) {
        if val <= 0x7FFFFFFF {
            self.li_a0(val as i32);
            return;
        }
        // Store in rodata (8-byte aligned)
        let off = align_rodata(&mut self.rodata, 8);
        self.rodata.extend_from_slice(&val.to_le_bytes());
        let pos = self.pos();
        self.emit_auipc_a0(0); // placeholder
        self.emit_ld_a0_a0(0); // placeholder
        self.const64_refs.push((pos, off));
    }

    /// Load a 64-bit constant into rd using AUIPC + LD from rodata.
    pub(crate) fn load_const64_reg(&mut self, rd: u8, val: u64) {
        if val <= 0x7FFFFFFF {
            if rd == 10 {
                self.li_a0(val as i32);
            } else {
                self.li_reg(rd, val as i32);
            }
            return;
        }
        // Use a0 as intermediate then move to rd
        self.load_const64_a0(val);
        if rd != 10 {
            self.emit_addi(rd, 10, 0); // MV rd, a0
        }
    }
}
