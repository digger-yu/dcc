// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::aarch64::codegen::Aarch64Codegen;
use crate::codegen::aarch64::insn::Insn;

impl Aarch64Codegen {
    // ===== 原子操作辅助指令 =====

    /// LDAR (Load-Acquire) - 按尺寸选 8/16/32/64 位
    pub(crate) fn emit_ldar(&mut self, sz: usize, rt: u8, rn: u8) {
        let insn = match sz {
            1 => 0x08dffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            2 => 0x48dffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            4 => 0x88dffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            _ => 0xc8dffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
        };
        self.emit_insn(&Insn::raw(insn));
    }

    /// STLR (Store-Release) - 按尺寸选 8/16/32/64 位
    pub(crate) fn emit_stlr(&mut self, sz: usize, rt: u8, rn: u8) {
        let insn = match sz {
            1 => 0x089ffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            2 => 0x489ffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            4 => 0x889ffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            _ => 0xc89ffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
        };
        self.emit_insn(&Insn::raw(insn));
    }

    /// LDAXR (Load-Acquire Exclusive) - 按尺寸选 8/16/32/64 位
    pub(crate) fn emit_ldaxr(&mut self, sz: usize, rt: u8, rn: u8) {
        let insn = match sz {
            1 => 0x085ffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            2 => 0x485ffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            4 => 0x885ffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            _ => 0xc85ffe00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
        };
        self.emit_insn(&Insn::raw(insn));
    }

    /// LDXR (Load Exclusive) - 按尺寸选 8/16/32/64 位
    pub(crate) fn emit_ldxr(&mut self, sz: usize, rt: u8, rn: u8) {
        let insn = match sz {
            1 => 0x085f7c00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            2 => 0x485f7c00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            4 => 0x885f7c00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            _ => 0xc85f7c00u32 | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
        };
        self.emit_insn(&Insn::raw(insn));
    }

    /// STLXR (Store-Release Exclusive) - 按尺寸选 8/16/32/64 位
    /// rs = status register, rt = value to store, rn = base register
    pub(crate) fn emit_stlxr(&mut self, sz: usize, rs: u8, rt: u8, rn: u8) {
        let insn = match sz {
            1 => {
                0x0800fc00u32
                    | ((rs as u32 & 0x1f) << 16)
                    | ((rn as u32 & 0x1f) << 5)
                    | (rt as u32 & 0x1f)
            },
            2 => {
                0x4800fc00u32
                    | ((rs as u32 & 0x1f) << 16)
                    | ((rn as u32 & 0x1f) << 5)
                    | (rt as u32 & 0x1f)
            },
            4 => {
                0x8800fc00u32
                    | ((rs as u32 & 0x1f) << 16)
                    | ((rn as u32 & 0x1f) << 5)
                    | (rt as u32 & 0x1f)
            },
            _ => {
                0xc800fc00u32
                    | ((rs as u32 & 0x1f) << 16)
                    | ((rn as u32 & 0x1f) << 5)
                    | (rt as u32 & 0x1f)
            },
        };
        self.emit_insn(&Insn::raw(insn));
    }

    /// CLREX - 清除 exclusive monitor
    pub(crate) fn emit_clrex(&mut self) {
        self.emit_insn(&Insn::raw(0xd5033f5f));
    }

    /// CBNZ - Compare and Branch Non-Zero (32-bit)
    pub(crate) fn emit_cbnz(&mut self, rt: u8, label: &str) {
        let pos = self.pos();
        // cbnz w<rt>, #0 (placeholder)
        let insn = 0x35000000u32 | (rt as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
        if let Some(&target) = self.labels.get(label) {
            self.patch_cbnz(pos, target, rt);
        } else {
            self.pending_bcond.push((pos, label.to_string(), 0x10)); // 0x10 = CBNZ marker
        }
    }

    /// 修补 CBNZ 偏移
    pub(crate) fn patch_cbnz(&mut self, at: u64, target: u64, rt: u8) {
        let pc = at + 4;
        let imm19 = (((target as i64 - pc as i64) >> 2) as u32) & 0x7ffff;
        let insn = 0x35000000u32 | (imm19 << 5) | (rt as u32 & 0x1f);
        let off = at as usize;
        if off + 4 <= self.code.len() {
            self.code[off..off + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    // ========================================================================
    // 嵌套大结构体返回 (multi-level sret)
    // ========================================================================

    /// 嵌套 sret: 从 [sp+off] 拷贝 size 字节到 [dst_reg]
    /// 支持多层嵌套调用链: 每一层都通过 x0 传递 sret 指针
    pub(crate) fn memcpy_sp_to_reg(&mut self, off: i32, size: usize, dst_reg: u8) {
        // 先把 dst_reg 保存到 x1 (src = sp+off)
        self.mov_x_reg_sp_off(2, off); // x2 = src (sp+off)
                                       // x0 = dst_reg (sret 指针已在 dst_reg)
        if dst_reg != 0 {
            // mov x0, dst_reg
            let insn = 0xaa0003e0u32 | ((dst_reg as u32 & 0x1f) << 16);
            self.emit_insn(&Insn::raw(insn));
        }
        let mut remaining = size;
        let mut src_off = 0i32;
        while remaining > 0 {
            let chunk = remaining.min(8);
            if chunk >= 8 {
                // ldr x3, [x2, #src_off]
                let imm12 = (src_off / 8) as u32 & 0xfff;
                let insn_ldr = 0xf9400043u32 | (imm12 << 10);
                self.emit_insn(&Insn::raw(insn_ldr));
                // str x3, [x0, #src_off]
                let insn_str = 0xf9000003u32 | (imm12 << 10);
                self.emit_insn(&Insn::raw(insn_str));
            } else if chunk >= 4 {
                let imm12 = (src_off / 4) as u32 & 0xfff;
                let insn_ldr = 0xb9400043u32 | (imm12 << 10);
                self.emit_insn(&Insn::raw(insn_ldr));
                let insn_str = 0xb9000003u32 | (imm12 << 10);
                self.emit_insn(&Insn::raw(insn_str));
            } else {
                // byte-by-byte for tail
                for b in 0..chunk as i32 {
                    let ldrb = 0x39400043u32 | (((src_off + b) as u32 & 0xfff) << 10);
                    self.emit_insn(&Insn::raw(ldrb));
                    let strb = 0x39000003u32 | (((src_off + b) as u32 & 0xfff) << 10);
                    self.emit_insn(&Insn::raw(strb));
                }
            }
            src_off += chunk as i32;
            remaining -= chunk;
        }
    }
}
