// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::aarch64::codegen::Aarch64Codegen;

impl Aarch64Codegen {
    pub(crate) fn emit_dc_cvac(&mut self, reg: u8) {
        let insn = 0xd50b7420u32 | ((reg as u32 & 0x1f) << 5);
        self.emit(&insn.to_le_bytes());
    }

    /// dc civac, xN — Data Cache Clean and Invalidate by VA to PoC
    pub(crate) fn emit_dc_civac(&mut self, reg: u8) {
        let insn = 0xd50b7620u32 | ((reg as u32 & 0x1f) << 5);
        self.emit(&insn.to_le_bytes());
    }

    /// dc cvau, xN — Data Cache Clean by VA to PoU
    pub(crate) fn emit_dc_cvau(&mut self, reg: u8) {
        let insn = 0xd50b7520u32 | ((reg as u32 & 0x1f) << 5);
        self.emit(&insn.to_le_bytes());
    }

    /// dc ivac, xN — Data Cache Invalidate by VA to PoC
    pub(crate) fn emit_dc_ivac(&mut self, reg: u8) {
        let insn = 0xd5087600u32 | ((reg as u32 & 0x1f) << 5);
        self.emit(&insn.to_le_bytes());
    }

    /// ic ivau, xN — Instruction Cache Invalidate by VA to PoU
    pub(crate) fn emit_ic_ivau(&mut self, reg: u8) {
        let insn = 0xd50b7520u32 | ((reg as u32 & 0x1f) << 5);
        self.emit(&insn.to_le_bytes());
    }

    /// ic iallu — Instruction Cache Invalidate All to PoU
    pub(crate) fn emit_ic_iallu(&mut self) {
        self.emit(&[0x7f, 0x78, 0x03, 0xd5]);
    }

    /// dsb ish
    pub(crate) fn emit_dsb_ish(&mut self) {
        self.emit(&[0x9f, 0x3b, 0x03, 0xd5]);
    }

    /// dmb ish — 内部共享内存屏障 (memory clobber 编译器屏障)
    pub(crate) fn emit_dmb_ish(&mut self) {
        // DMB ISH: CRm=0xB, op2=4, Rt=0b0101 => D5033BBF
        self.emit(&[0xbf, 0x3b, 0x03, 0xd5]);
    }

    /// dsb sy — 全系统屏障
    pub(crate) fn emit_dsb_sy(&mut self) {
        self.emit(&[0xbf, 0x3f, 0x03, 0xd5]);
    }

    /// isb
    pub(crate) fn emit_isb(&mut self) {
        self.emit(&[0xdf, 0x3b, 0x03, 0xd5]);
    }

    /// 同步 [start, end) 区间指令/数据缓存 (Linux __clear_cache 语义)
    /// 正确循环结构: 每轮先比较, 再操作, 再递增, 条件跳回
    pub(crate) fn emit_cache_sync_range(&mut self, start_reg: u8, end_reg: u8) {
        // === Phase 1: Data Cache Clean ===
        let dc_loop = self.fresh_label("Ldc_loop");
        let dc_exit = self.fresh_label("Ldc_exit");
        self.label(&dc_loop);
        // 先比较: if start >= end, 跳出
        self.emit_cmp_regs(start_reg, end_reg);
        self.emit_bcond(&dc_exit, 0xa); // b.ge exit
                                        // 操作: dc cvac, start
        self.emit_dc_cvac(start_reg);
        // 递增: start += 8
        self.emit_add_imm_x(start_reg, start_reg, 8);
        // 跳回循环顶
        self.b(&dc_loop);
        self.label(&dc_exit);
        self.emit_dsb_ish();

        // === Phase 2: Instruction Cache Invalidate ===
        let ic_loop = self.fresh_label("Lic_loop");
        let ic_exit = self.fresh_label("Lic_exit");
        self.label(&ic_loop);
        self.emit_cmp_regs(start_reg, end_reg);
        self.emit_bcond(&ic_exit, 0xa); // b.ge exit
        self.emit_ic_ivau(start_reg);
        self.emit_add_imm_x(start_reg, start_reg, 8);
        self.b(&ic_loop);
        self.label(&ic_exit);
        self.emit_dsb_ish();
        self.emit_isb();
    }

    /// cmp xn, xm — 64 位寄存器比较 (subs wzr, xn, xm)
    pub(crate) fn emit_cmp_regs(&mut self, rn: u8, rm: u8) {
        let insn = 0xeb00001fu32 | ((rm as u32 & 0x1f) << 16) | ((rn as u32 & 0x1f) << 5);
        self.emit(&insn.to_le_bytes());
    }

    /// b.cond label — 条件分支 (占位 + 延迟修补)
    pub(crate) fn emit_bcond(&mut self, label: &str, cond: u8) {
        let pos = self.pos();
        let insn = 0x54000000u32 | (cond as u32 & 0xf);
        self.emit(&insn.to_le_bytes());
        if let Some(&target) = self.labels.get(label) {
            self.patch_bcond(pos, target, cond);
        } else {
            // 记录待修补 (复用 pending_branches 但编码为条件分支)
            self.pending_bcond.push((pos, label.to_string(), cond));
        }
    }

    /// 修补条件分支偏移
    pub(crate) fn patch_bcond(&mut self, at: u64, target: u64, cond: u8) {
        let pc = at + 4;
        let imm19 = (((target as i64 - pc as i64) >> 2) as u32) & 0x7ffff;
        let insn = 0x54000000u32 | imm19 | (cond as u32 & 0xf);
        let off = at as usize;
        if off + 4 <= self.code.len() {
            self.code[off..off + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    /// add xd, xn, #imm — 64 位立即加法
    pub(crate) fn emit_add_imm_x(&mut self, dst: u8, src: u8, imm: u32) {
        if imm <= 0xfff {
            let insn =
                0x91000000u32 | (imm << 10) | ((src as u32 & 0x1f) << 5) | (dst as u32 & 0x1f);
            self.emit(&insn.to_le_bytes());
        } else {
            // 大立即数: movz x9, #imm; add xd, xn, x9
            self.mov_x_reg_imm(9, imm as u64);
            self.emit_add_x_reg(dst, src, 9);
        }
    }

    /// add xd, xn, xm — 64 位寄存器加法
    pub(crate) fn emit_add_x_reg(&mut self, dst: u8, rn: u8, rm: u8) {
        let insn = 0x8b000000u32
            | ((rm as u32 & 0x1f) << 16)
            | ((rn as u32 & 0x1f) << 5)
            | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// MADD Xd, Xn, Xm, XZR — 64-bit multiply (rd = rn * rm)
    pub(crate) fn emit_mul_x(&mut self, dst: u8, rn: u8, rm: u8) {
        let insn = 0x9b000000u32
            | ((rm as u32 & 0x1f) << 16)
            | (31u32 << 10) // ra = xzr
            | ((rn as u32 & 0x1f) << 5)
            | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// sub xd, xn, xm — 64 位寄存器减法
    pub(crate) fn emit_sub_x_reg(&mut self, dst: u8, rn: u8, rm: u8) {
        let insn = 0xcb000000u32
            | ((rm as u32 & 0x1f) << 16)
            | ((rn as u32 & 0x1f) << 5)
            | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// and xd, xn, xm — 64 位寄存器与
    pub(crate) fn emit_and_x_reg(&mut self, dst: u8, rn: u8, rm: u8) {
        let insn = 0x8a000000u32
            | ((rm as u32 & 0x1f) << 16)
            | ((rn as u32 & 0x1f) << 5)
            | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// orr xd, xn, xm — 64 位寄存器或
    pub(crate) fn emit_or_x_reg(&mut self, dst: u8, rn: u8, rm: u8) {
        let insn = 0xaa000000u32
            | ((rm as u32 & 0x1f) << 16)
            | ((rn as u32 & 0x1f) << 5)
            | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// eor xd, xn, xm — 64 位寄存器异或
    pub(crate) fn emit_eor_x_reg(&mut self, dst: u8, rn: u8, rm: u8) {
        let insn = 0xca000000u32
            | ((rm as u32 & 0x1f) << 16)
            | ((rn as u32 & 0x1f) << 5)
            | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// LSR Xd, Xn, #shift — 64 位逻辑右移 (UBFM encoding)
    pub(crate) fn emit_lsr_x(&mut self, dst: u8, src: u8, shift: u8) {
        if shift == 0 {
            return;
        }
        // UBFM X64: 0xD3400000 | (1<<22) | (immr<<16) | (imms<<10) | (Rn<<5) | Rd
        let immr = shift as u32 & 0x3f;
        let imms = 63u32; // imms = N-1 for 64-bit
        let insn = 0xd3400000u32
            | (1u32 << 22) // N bit for 64-bit
            | (immr << 16)
            | (imms << 10)
            | ((src as u32 & 0x1f) << 5)
            | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// LSL Xd, Xn, #shift — 64 位逻辑左移 (UBFM encoding)
    pub(crate) fn emit_lsl_x(&mut self, dst: u8, src: u8, shift: u8) {
        if shift == 0 {
            return;
        }
        // UBFM X64: LSL #shift = UBFM with immr=(64-shift), imms=63
        let immr = (64 - shift as u32) & 0x3f;
        let imms = 63u32;
        let insn = 0xd3400000u32
            | (1u32 << 22)
            | (immr << 16)
            | (imms << 10)
            | ((src as u32 & 0x1f) << 5)
            | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// ANDS Xd, Xn, Xm — 64 位寄存器与并设置标志
    pub(crate) fn emit_ands_x_reg(&mut self, dst: u8, rn: u8, rm: u8) {
        let insn = 0xea000000u32
            | ((rm as u32 & 0x1f) << 16)
            | ((rn as u32 & 0x1f) << 5)
            | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// MVN Xd, Xm — 按位取反 (ORN Xd, XZR, Xm)
    pub(crate) fn emit_mvn_x(&mut self, dst: u8, src: u8) {
        // ORN Xd, XZR, Xm: 0xAA200000 | (1<<21) | (Rm<<16) | (XZR<<5) | Rd
        let insn = 0xaa200000u32
            | ((src as u32 & 0x1f) << 16)
            | (31u32 << 5) // XZR
            | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }
}
