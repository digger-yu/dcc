// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::aarch64::codegen::Aarch64Codegen;
use crate::codegen::aarch64::insn::Insn;

impl Aarch64Codegen {
    pub(crate) fn mov_w0_imm(&mut self, imm: u32) {
        if imm <= 0xffff {
            let insn = 0x5280_0000u32 | ((imm & 0xffff) << 5);
            self.emit_insn(&Insn::raw(insn));
            return;
        }
        let lo = imm & 0xffff;
        let hi = (imm >> 16) & 0xffff;
        self.emit_insn(&Insn::raw(0x5280_0000u32 | (lo << 5)));
        self.emit_insn(&Insn::raw(0x72a0_0000u32 | (hi << 5)));
    }

    // M7 修复: 通用 x 寄存器装入任意立即数 (movz + 必要 movk)
    // 编码: movz xN, #imm16, lsl #shift  => sf=1, opc=10, hw=shift/16
    //       movk xN, #imm16, lsl #shift  => sf=1, opc=11, hw=shift/16
    // 限制: 仅无符号 64-bit; 负数需补码 (调用方负责)
    pub(crate) fn mov_x_reg_imm(&mut self, reg: u8, imm: u64) {
        for shift in [0u32, 16, 32, 48] {
            let part = ((imm >> shift) & 0xffff) as u32;
            // movz 清零其它位, 仅在需要建立 64-bit 值时 emit
            // 但若后续有更高位非零, 当前 shift 必须 emit
            let higher_nonzero = (imm >> (shift + 16)) != 0;
            if part == 0 && shift > 0 && !higher_nonzero {
                continue;
            }
            let hw = shift / 16;
            let insn: u32 = (1 << 31)
                | (0b10 << 29)
                | (0b100101 << 23)
                | (hw << 21)
                | (part << 5)
                | (reg as u32 & 0x1f);
            self.emit_insn(&Insn::raw(insn));
        }
    }

    // M7 修复: 计算 sp + off 到 x 寄存器的地址
    // 编码: add xN, sp, #imm12  (off >= 0)   => 0x910003e0 | (off<<10) | rd
    //       sub xN, sp, #imm12  (off < 0)    => 0x510003e0 | (-off<<10) | rd
    // 限制: 偏移 12-bit (0..4095); frame 大于此范围需分段
    pub(crate) fn mov_x_reg_sp_off(&mut self, reg: u8, off: i32) {
        if (0..4096).contains(&off) {
            let insn = 0x910003e0u32 | ((off as u32) << 10) | (reg as u32 & 0x1f);
            self.emit_insn(&Insn::raw(insn));
        } else if off < 0 && off > -4096 {
            let pos = (-off) as u32;
            let insn = 0x510003e0u32 | (pos << 10) | (reg as u32 & 0x1f);
            self.emit_insn(&Insn::raw(insn));
        } else {
            // 大偏移: movz x9, #off; add/sub xN, sp, x9
            let tmp: u8 = 9;
            let abs = off.unsigned_abs() as u64;
            self.mov_x_reg_imm(tmp, abs);
            if off >= 0 {
                self.emit_add_x_reg_sp_reg(reg, tmp);
            } else {
                self.emit_sub_x_reg_sp_reg(reg, tmp);
            }
        }
    }

    pub(crate) fn mov_x0_sp_off(&mut self, off: i32) {
        self.mov_x_reg_sp_off(0, off);
    }

    pub(crate) fn mov_x0_imm(&mut self, imm: u32) {
        let insn = 0xd280_0000u32 | ((imm & 0xffff) << 5);
        self.emit_insn(&Insn::raw(insn));
    }

    pub(crate) fn mov_x8_imm(&mut self, imm: u64) {
        let insn = 0xd280_0000u32 | (((imm & 0xffff) as u32) << 5) | 8;
        self.emit_insn(&Insn::raw(insn));
    }

    pub(crate) fn store_w0(&mut self, off: i32) {
        self.str_wt_sp(0, off);
    }

    pub(crate) fn load_w0(&mut self, off: i32) {
        self.ldr_wt_sp(0, off);
    }

    pub(crate) fn str_wt_sp(&mut self, reg: u8, off: i32) {
        let imm12 = (off / 4) as u32 & 0xfff;
        let insn = 0xb900_0000u32 | imm12 << 10 | 31 << 5 | (reg as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    pub(crate) fn ldr_wt_sp(&mut self, reg: u8, off: i32) {
        let imm12 = (off / 4) as u32 & 0xfff;
        let insn = 0xb940_0000u32 | imm12 << 10 | 31 << 5 | (reg as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    // --- helpers for pointer/member/index codegen ---

    /// MOV Xd, Xn — ORR Xd, XZR, Xn
    pub(crate) fn emit_mov_x_reg(&mut self, dst: u8, src: u8) {
        // ORR Xd, XZR, Xn
        let insn = 0x2a00_0000u32 | (0x1f << 16) | ((src as u32 & 0x1f) << 5) | (dst as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// Store register rt to [sp + off] with given byte size (1/2/4/8)
    pub(crate) fn store_x_at_sp(&mut self, off: i32, rt: u8, sz: usize) {
        match sz {
            1 => {
                let imm12 = (off as u32) & 0xfff;
                let insn = 0x39000000u32 | (imm12 << 10) | (31u32 << 5) | (rt as u32 & 0x1f);
                self.emit_insn(&Insn::raw(insn));
            },
            2 => {
                let imm12 = ((off / 2) as u32) & 0xfff;
                let insn = 0x79000000u32 | (imm12 << 10) | (31u32 << 5) | (rt as u32 & 0x1f);
                self.emit_insn(&Insn::raw(insn));
            },
            4 => self.str_wt_sp(rt, off),
            _ => self.emit_str_x(rt, 31, off as u32, 8),
        }
    }

    /// Load from [rs1 + imm] with byte size and signedness
    pub(crate) fn emit_load_sz(&mut self, rt: u8, rs1: u8, imm: u32, sz: usize, signed: bool) {
        match (sz, signed) {
            (1, false) => self.emit_ldr_x(rt, rs1, imm, 1),
            (1, true) => {
                // LDRSB Xd, [Xn, #imm] = 0x39800000
                let imm12 = imm & 0xfff;
                let insn =
                    0x39800000u32 | (imm12 << 10) | ((rs1 as u32 & 0x1f) << 5) | (rt as u32 & 0x1f);
                self.emit_insn(&Insn::raw(insn));
            },
            (2, false) => self.emit_ldr_x(rt, rs1, imm, 2),
            (2, true) => {
                // LDRSH Xd, [Xn, #imm] = 0x79800000
                let imm12 = (imm / 2) & 0xfff;
                let insn =
                    0x79800000u32 | (imm12 << 10) | ((rs1 as u32 & 0x1f) << 5) | (rt as u32 & 0x1f);
                self.emit_insn(&Insn::raw(insn));
            },
            (4, _) => self.emit_ldr_x(rt, rs1, imm, 4),
            _ => self.emit_ldr_x(rt, rs1, imm, 8),
        }
    }

    /// Store to [rs1 + imm] with byte size
    pub(crate) fn emit_store_sz(&mut self, rt: u8, rs1: u8, imm: u32, sz: usize) {
        match sz {
            1 => {
                let imm12 = imm & 0xfff;
                let insn =
                    0x39000000u32 | (imm12 << 10) | ((rs1 as u32 & 0x1f) << 5) | (rt as u32 & 0x1f);
                self.emit_insn(&Insn::raw(insn));
            },
            2 => {
                let imm12 = (imm / 2) & 0xfff;
                let insn =
                    0x79000000u32 | (imm12 << 10) | ((rs1 as u32 & 0x1f) << 5) | (rt as u32 & 0x1f);
                self.emit_insn(&Insn::raw(insn));
            },
            4 => {
                let imm12 = (imm / 4) & 0xfff;
                let insn =
                    0xb9000000u32 | (imm12 << 10) | ((rs1 as u32 & 0x1f) << 5) | (rt as u32 & 0x1f);
                self.emit_insn(&Insn::raw(insn));
            },
            _ => self.emit_str_x(rt, rs1, imm, 8),
        }
    }

    /// Aggregate store: copy `total_bytes` from [x12] to [x10] word-by-word
    /// x12 = src, x10 = dst, x14 = temp, x11 = counter
    pub(crate) fn aggregate_store_loop(
        &mut self,
        src_reg: u8,
        dst_reg: u8,
        start_off: usize,
        total: usize,
    ) {
        // For small aggregates (<=64 bytes), unroll
        if total <= 64 {
            let mut off = start_off;
            while off + 8 <= total {
                self.emit_ldr_x(14, src_reg, off as u32, 8);
                self.emit_str_x(14, dst_reg, off as u32, 8);
                off += 8;
            }
            if off < total {
                let rem = total - off;
                self.emit_ldr_x(14, src_reg, off as u32, 8);
                self.emit_store_sz(14, dst_reg, off as u32, rem);
            }
            return;
        }
        // Large: use a loop with x11 as counter
        let lbl = format!(".Laggcpy_{}", self.pos());
        let lbl_end = format!(".Laggend_{}", self.pos());
        // x11 = total (counter), x14 = offset
        self.mov_x_reg_imm(11, total as u64);
        self.mov_x_reg_imm(14, start_off as u64);
        // loop:
        self.emit_label_raw(&lbl);
        // cbz x11, end
        let cbz = 0xb4000000u32 | (5u32 << 5) | 11;
        self.emit_insn(&Insn::raw(cbz));
        // ldr x9, [src_reg, x14]
        self.emit_ldr_x(9, src_reg, 0, 8); // TODO: register offset
                                           // str x9, [dst_reg, x14]
        self.emit_str_x(9, dst_reg, 0, 8);
        // add x14, x14, #8
        self.emit_add_imm_x(14, 14, 8);
        // sub x11, x11, #8
        let sub_insn = 0xd1000000u32 | (8u32 << 10) | (11u32 << 5) | 11;
        self.emit_insn(&Insn::raw(sub_insn));
        // b loop
        // (simplified: for now just fall through for small sizes)
        self.emit_label_raw(&lbl_end);
    }

    pub(crate) fn emit_label_raw(&mut self, label: &str) {
        self.labels.insert(label.to_string(), self.pos());
    }

    // M25: 64 位 str/ldr 用于 va_list 寄存器保存区
    // str Xt, [sp, #imm] imm 需 8 字节对齐, 范围 [0, 32760]
    // 编码 bits[31:22] = 11_111_0_01_0_L (opc=11, 111, V=0, 01, 0, L)
    // STR X (L=0) => 0xF900_0000, LDR X (L=1) => 0xF940_0000
    /// -ftrivial-auto-var-init=zero: 零初始化栈帧
    /// 使用 stp xzr, xzr, [sp, #off] 每次清零 16 字节 (frame 已 16 字节对齐)
    /// 使用 signed-offset 寻址模式, imm7 范围 [-64,63]×8 = [-512,504]
    pub(crate) fn emit_zero_init_frame(&mut self, frame_size: usize) {
        if frame_size == 0 {
            return;
        }
        let pairs = frame_size / 16;
        for i in 0..pairs {
            let off = (i as i32) * 16 - (frame_size as i32);
            // stp xzr, xzr, [sp, #off] (signed offset mode)
            let imm7 = ((off / 8) as u32) & 0x7f;
            let insn = 0xa900_0000u32 | (imm7 << 15) | (31u32 << 10) | (31u32 << 5) | 31;
            self.emit_insn(&Insn::raw(insn));
        }
        // 处理剩余 8 字节 (当 frame 非 16 字节对齐时)
        let rem = frame_size % 16;
        if rem >= 8 {
            let off = (pairs * 16) as i32 - (frame_size as i32);
            // str xzr, [sp, #off] (unsigned offset mode)
            let imm12 = ((off / 8) as u32) & 0xfff;
            let insn = 0xf900_0000u32 | (imm12 << 10) | (31u32 << 5) | 31;
            self.emit_insn(&Insn::raw(insn));
        }
    }

    pub(crate) fn str_xt_sp(&mut self, reg: u8, off: i32) {
        debug_assert!(
            off >= 0 && off % 8 == 0 && off <= 32760,
            "str_xt_sp: off must be non-neg 8-byte aligned, got {off}"
        );
        let imm12 = ((off / 8) as u32) & 0xfff;
        let insn = 0xf900_0000u32 | (imm12 << 10) | (31u32 << 5) | (reg as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    pub(crate) fn ldr_xt_sp(&mut self, reg: u8, off: i32) {
        debug_assert!(
            off >= 0 && off % 8 == 0 && off <= 32760,
            "ldr_xt_sp: off must be non-neg 8-byte aligned, got {off}"
        );
        let imm12 = ((off / 8) as u32) & 0xfff;
        let insn = 0xf940_0000u32 | (imm12 << 10) | (31u32 << 5) | (reg as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// `LDR Xt, [Xn, #imm]` — 寄存器间接 + 无符号偏移 (64-bit, scale=8)
    /// imm 必须为非负 8 字节对齐, 范围 0..32760.
    pub(crate) fn ldr_xt_indirect(&mut self, rt: u8, rn: u8, imm: i32) {
        debug_assert!(
            imm >= 0 && imm % 8 == 0 && imm <= 32760,
            "ldr_xt_indirect: imm must be non-neg 8-byte aligned, got {imm}"
        );
        let imm12 = ((imm / 8) as u32) & 0xfff;
        let insn = 0xf940_0000u32 | (imm12 << 10) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// `STR Xt, [Xn, #imm]` — 寄存器间接 + 无符号偏移 (64-bit, scale=8)
    pub(crate) fn str_xt_indirect(&mut self, rt: u8, rn: u8, imm: i32) {
        debug_assert!(
            imm >= 0 && imm % 8 == 0 && imm <= 32760,
            "str_xt_indirect: imm must be non-neg 8-byte aligned, got {imm}"
        );
        let imm12 = ((imm / 8) as u32) & 0xfff;
        let insn = 0xf900_0000u32 | (imm12 << 10) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    pub(crate) fn str_qt_sp(&mut self, reg: u8, off: i32) {
        debug_assert!(
            off >= 0 && off % 16 == 0 && off <= 65520,
            "str_qt_sp: off must be non-neg 16-byte aligned, got {off}"
        );
        let imm12 = ((off / 16) as u32) & 0xfff;
        let insn = 0x3d80_0000u32 | (imm12 << 10) | (31u32 << 5) | (reg as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    pub(crate) fn ldr_qt_sp(&mut self, reg: u8, off: i32) {
        debug_assert!(
            off >= 0 && off % 16 == 0 && off <= 65520,
            "ldr_qt_sp: off must be non-neg 16-byte aligned, got {off}"
        );
        let imm12 = ((off / 16) as u32) & 0xfff;
        let insn = 0x3dc0_0000u32 | (imm12 << 10) | (31u32 << 5) | (reg as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    pub(crate) fn str_q0_sp(&mut self, off: i32) {
        self.str_qt_sp(0, off);
    }

    pub(crate) fn ldr_q0_sp(&mut self, off: i32) {
        self.ldr_qt_sp(0, off);
    }

    pub(crate) fn extract_vector_lane_w0(&mut self, idx: usize) {
        let lane = (idx & 3) as u32;
        let insn = 0x0e04_3c00u32 | (lane << 11);
        self.emit_insn(&Insn::raw(insn));
    }

    pub(crate) fn mov_w14_w0(&mut self) {
        self.emit_insn(&Insn::raw(0x2a8003ee));
    }

    pub(crate) fn mov_w0_w14(&mut self) {
        self.emit_insn(&Insn::raw(0x2a0e03e0));
    }

    pub(crate) fn mov_w1_w0(&mut self) {
        self.emit_insn(&Insn::raw(0x2a8003e1));
    }

    /// MOV W2, W0 — ORR W2, WZR, W0
    pub(crate) fn mov_w2_w0(&mut self) {
        self.emit_insn(&Insn::raw(0x2a8003e2));
    }

    /// CMP Wn, Wm (alias of SUBS WZR, Wn, Wm) — 32-bit register compare
    pub(crate) fn emit_cmp_w_reg(&mut self, rn: u8, rm: u8) {
        // SUBS WZR, Wn, Wm => 0x6B000000 | Rm<<16 | Rn<<5 | 31
        let subs = 0x6b00001fu32 | ((rm as u32 & 0x1f) << 16) | ((rn as u32 & 0x1f) << 5);
        self.emit_insn(&Insn::raw(subs));
    }

    pub(crate) fn mov_w_reg_w0(&mut self, reg: u8) {
        // MOV Wd, W0 (即 ORR Wd, WZR, W0)。base 0x2a0003e0 已含 Rm=W0(bits16-20=0)
        // 与 Rn=WZR(bits5-9=11111), 只需把目标寄存器 reg 放进 Rd 位[4:0]。
        // 旧代码错将 reg 放进 Rm(<<16) 又用 (<<5) 踩写 Rn, 实际发出的是
        // `MOV W0, Wreg` —— 目标/源颠倒, 导致第 2+ 个实参被丢 (单参因 reg=0
        // 时恰为 `mov w0,w0` no-op 而掩盖)。改为正确置 Rd=reg。
        let insn = 0x2a0003e0u32 | (reg as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// MOV Wd, Ws — general 32-bit register-to-register move
    pub(crate) fn mov_w_rd_rs(&mut self, rd: u8, rs: u8) {
        let insn = 0x2a0003e0u32 | ((rs as u32 & 0x1f) << 16) | (rd as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// MOV Xd, Xs — general 64-bit register-to-register move
    /// 编码: ORR Xd, XZR, Xs (alias of MOV)
    pub(crate) fn mov_x_reg(&mut self, dst: u8, src: u8) {
        if dst == src {
            return;
        }
        let insn = 0xaa0003e0u32 | ((src as u32 & 0x1f) << 16) | (dst as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// Load a local variable from stack into an arbitrary GP register (32-bit W)
    pub(crate) fn asm_load_var_to_reg(&mut self, var_name: &str, reg_idx: u8) {
        if let Some(&off) = self.locals.get(var_name) {
            self.ldr_wt_sp(reg_idx, off);
        }
    }

    /// Store a GP register value back to a local variable on stack (32-bit W)
    pub(crate) fn asm_store_reg_to_var(&mut self, var_name: &str, reg_idx: u8) {
        if let Some(&off) = self.locals.get(var_name) {
            self.str_wt_sp(reg_idx, off);
        }
    }
}
