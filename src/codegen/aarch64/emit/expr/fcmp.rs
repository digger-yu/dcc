// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{BinOp, Expr};
use crate::codegen::aarch64::codegen::{Aarch64Codegen, HfaInfo};
use crate::codegen::aarch64::insn::Insn;
use crate::ir::{ExprKind, TypedExpr};

impl Aarch64Codegen {
    pub(crate) fn fcmp_s0_s1(&mut self) {
        // fcmp s0, s1
        self.emit_insn(&Insn::raw(0x1e280000));
    }

    /// fcmp d0, d1 — 双精度浮点比较
    pub(crate) fn fcmp_d0_d1(&mut self) {
        // fcmp d0, d1: sf=1, ftype=01, Rm=1, Rn=0
        // 0x1E682000 LE = 00 20 68 1e
        self.emit_insn(&Insn::raw(0x1e682000));
    }

    /// fcmp s0, #0.0 — 单精度与零比较
    pub(crate) fn fcmp_s0_zero(&mut self) {
        self.emit_insn(&Insn::raw(0x1e280008));
    }

    /// fcmp d0, #0.0 — 双精度与零比较
    pub(crate) fn fcmp_d0_zero(&mut self) {
        // fcmp d0, #0.0: 0x1E680008 LE = 08 00 68 1e
        self.emit_insn(&Insn::raw(0x1e680008));
    }

    pub(crate) fn fset_w0_from_fcmp(&mut self, op: BinOp) {
        let cond = match op {
            BinOp::EqEq => 0x1u32,
            BinOp::Ne => 0x0,
            BinOp::Lt => 0xb,
            BinOp::Le => 0xa,
            BinOp::Gt => 0xc,
            BinOp::Ge => 0xd,
            _ => 0x1,
        };
        // cset w0, cond
        let insn = 0x1a9f_0000u32 | (cond << 12) | 0x20;
        self.emit_insn(&Insn::raw(insn));
    }

    /// fccmp s0, s1, #nzcv, ne — 条件浮点比较保留 flags
    pub(crate) fn emit_fccmp_s0_s1_ne(&mut self, nzcv: u8) {
        // fccmp s0, s1, #nzcv, ne
        // encoding: 0x1e210420 | (nzcv << 12) | (cond_ne=1 << 12)
        let insn = 0x1e610420u32 | ((nzcv as u32 & 0xf) << 12) | (1 << 12);
        self.emit_insn(&Insn::raw(insn));
    }

    /// fccmp d0, d1, #nzcv, ne — 条件双精度浮点比较
    pub(crate) fn emit_fccmp_d0_d1_ne(&mut self, nzcv: u8) {
        let insn = 0x1e610420u32 | 0x400000 | ((nzcv as u32 & 0xf) << 12) | (1 << 12);
        self.emit_insn(&Insn::raw(insn));
    }

    pub(crate) fn cmp_w15_zero(&mut self) {
        // subs wzr, w15, #0  (cmp w15, #0)
        self.emit_insn(&Insn::raw(0x710003ff));
    }

    pub(crate) fn mov_w15_w0(&mut self) {
        self.emit_insn(&Insn::raw(0x2a8003ef)); // mov w15, w0
    }

    /// csel w0, w0, w1, ne
    pub(crate) fn csel_w0_w0_w1_ne(&mut self) {
        self.emit_insn(&Insn::raw(0x1a8101e0)); // cond=ne(1)
    }

    /// ccmp w0, w1, #nzcv, ne — 条件比较保留 flags
    pub(crate) fn emit_ccmp_w0_w1(&mut self, nzcv: u8) {
        // ccmp w0, w1, #nzcv, ne
        // encoding: sf=0, op=1, S=1, 011010, Rm=1, cond=ne(1), 0, Rn=0, imm4=nzcv
        let insn = 0x7a400000u32
            | ((nzcv as u32 & 0xf) << 12)
            | (1u32 << 12)  // cond = ne
            | (1u32 << 16); // Rn = w0
        self.emit_insn(&Insn::raw(insn));
    }

    /// ccmp w0, #imm5, #nzcv, cond — 立即数条件比较
    pub(crate) fn emit_ccmp_w0_imm(&mut self, imm5: u8, nzcv: u8, cond: u8) {
        // ccmp w0, #imm5, #nzcv, cond
        // sf=0, op=1, S=1, 01101, imm5, cond, 1, Rn=0, imm4=nzcv
        let insn = 0x7a000000u32
            | ((imm5 as u32 & 0x1f) << 16)
            | ((cond as u32 & 0xf) << 12)
            | (1u32 << 4)  // o2=1 (immediate form)
            | (nzcv as u32 & 0xf);
        self.emit_insn(&Insn::raw(insn));
    }

    /// csinc w0, w0, w1, eq
    pub(crate) fn emit_csinc_w0_w0_w1_eq(&mut self) {
        self.emit_insn(&Insn::raw(0x1a8401e0));
    }

    /// csinc xd, xn, xm, cond — 通用条件选择递增
    pub(crate) fn emit_csinc(&mut self, dst: u8, rn: u8, rm: u8, cond: u8) {
        let insn = 0x1a800000u32
            | ((rm as u32 & 0x1f) << 16)
            | ((cond as u32 & 0xf) << 12)
            | ((rn as u32 & 0x1f) << 5)
            | (dst as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// csel w0, w0, w1, cond — 通用条件选择
    pub(crate) fn emit_csel_w0_w0_w1(&mut self, cond: u8) {
        let insn = 0x1a800000u32
            | (1u32 << 16)   // Rm = w1
            | ((cond as u32 & 0xf) << 12); // Rd = w0
        self.emit_insn(&Insn::raw(insn));
    }

    /// csneg w0, w0, w1, lt — 条件符号取反
    pub(crate) fn emit_csneg_w0_w0_w1_lt(&mut self) {
        let insn = 0x5a800400u32 | (1u32 << 16) | (0xb << 12);
        self.emit_insn(&Insn::raw(insn));
    }

    pub(crate) fn emit_add_x_reg_sp_reg(&mut self, dst: u8, src: u8) {
        let insn = 0x8b0003e0u32 | ((src as u32 & 0x1f) << 16) | (dst as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    pub(crate) fn emit_sub_x_reg_sp_reg(&mut self, dst: u8, src: u8) {
        let insn = 0xcb0003e0u32 | ((src as u32 & 0x1f) << 16) | (dst as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    // ===== 复杂寻址模式: 后置索引 / 前置索引 =====

    /// str xt, [xn], #imm — 后置索引存储 (store then update base)
    pub(crate) fn emit_str_post_index(&mut self, rt: u8, rn: u8, imm9: i32) {
        let imm9_u = (imm9 as u32) & 0x1ff;
        // sf=1, 111000, 00, 0 (post), imm9, 01, Rn, Rt
        // base = 0xF8000400
        let insn = 0xf8000400u32 | (imm9_u << 12) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// ldr xt, [xn], #imm — 后置索引加载 (load then update base)
    pub(crate) fn emit_ldr_post_index(&mut self, rt: u8, rn: u8, imm9: i32) {
        let imm9_u = (imm9 as u32) & 0x1ff;
        // sf=1, 111000, 01, 0 (post), imm9, 01, Rn, Rt
        let insn = 0xf8400400u32 | (imm9_u << 12) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// str xt, [xn, #imm]! — 前置索引存储 (update base then store)
    pub(crate) fn emit_str_pre_index(&mut self, rt: u8, rn: u8, imm9: i32) {
        let imm9_u = (imm9 as u32) & 0x1ff;
        // sf=1, 111000, 00, 1 (pre), imm9, 01, Rn, Rt
        // base = 0xF8800000 (bit23=1 for pre-index)
        let insn = 0xf8800000u32 | (imm9_u << 12) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// ldr xt, [xn, #imm]! — 前置索引加载 (update base then load)
    pub(crate) fn emit_ldr_pre_index(&mut self, rt: u8, rn: u8, imm9: i32) {
        let imm9_u = (imm9 as u32) & 0x1ff;
        // sf=1, 111000, 01, 1 (pre), imm9, 01, Rn, Rt
        let insn = 0xf8c00000u32 | (imm9_u << 12) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// ldr xt, [xn, xm, extend {#shift}] — 扩展寄存器寻址
    pub(crate) fn emit_ldr_ext_reg(&mut self, rt: u8, rn: u8, rm: u8, extend: u8, shift: u8) {
        // LDR (extended register): 0xF8600800 | Rm<<16 | option<<13 | S<<12 | Rn<<5 | Rt
        let insn = 0xf8600800u32
            | ((rm as u32 & 0x1f) << 16)
            | ((extend as u32 & 0x7) << 13)
            | ((shift as u32 & 0x1) << 12)
            | ((rn as u32 & 0x1f) << 5)
            | (rt as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// str xt, [xn, xm, extend {#shift}] — 扩展寄存器存储
    pub(crate) fn emit_str_ext_reg(&mut self, rt: u8, rn: u8, rm: u8, extend: u8, shift: u8) {
        let insn = 0xf8200800u32
            | ((rm as u32 & 0x1f) << 16)
            | ((extend as u32 & 0x7) << 13)
            | ((shift as u32 & 0x1) << 12)
            | ((rn as u32 & 0x1f) << 5)
            | (rt as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// LDR Xt, [Xn, #imm] - 按尺寸无符号偏移加载 (用于解引用指针)
    pub(crate) fn emit_ldr_x(&mut self, rt: u8, rn: u8, imm: u32, sz: usize) {
        let imm12 = (imm / sz as u32) & 0xfff;
        let insn = match sz {
            1 => 0x39400000u32 | (imm12 << 10) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            2 => 0x79400000u32 | (imm12 << 10) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            4 => 0xb9400000u32 | (imm12 << 10) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            _ => 0xf9400000u32 | (imm12 << 10) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
        };
        self.emit_insn(&Insn::raw(insn));
    }

    /// STR Xt, [Xn, #imm] - 按尺寸无符号偏移存储
    pub(crate) fn emit_str_x(&mut self, rt: u8, rn: u8, imm: u32, sz: usize) {
        let imm12 = (imm / sz as u32) & 0xfff;
        let insn = match sz {
            1 => 0x39000000u32 | (imm12 << 10) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            2 => 0x79000000u32 | (imm12 << 10) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            4 => 0xb9000000u32 | (imm12 << 10) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
            _ => 0xf9000000u32 | (imm12 << 10) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f),
        };
        self.emit_insn(&Insn::raw(insn));
    }

    /// 32 位后置索引: str wt, [xn], #imm
    pub(crate) fn emit_str_w_post_index(&mut self, rt: u8, rn: u8, imm9: i32) {
        let imm9_u = (imm9 as u32) & 0x1ff;
        let insn = 0xb8000400u32 | (imm9_u << 12) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// 32 位后置索引: ldr wt, [xn], #imm
    pub(crate) fn emit_ldr_w_post_index(&mut self, rt: u8, rn: u8, imm9: i32) {
        let imm9_u = (imm9 as u32) & 0x1ff;
        let insn = 0xb8400400u32 | (imm9_u << 12) | ((rn as u32 & 0x1f) << 5) | (rt as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    pub(crate) fn str_s0_sp(&mut self, off: i32) {
        let imm12 = (off / 4) as u32 & 0xfff;
        let insn = 0xbd000000u32 | imm12 << 10 | 31 << 5;
        self.emit_insn(&Insn::raw(insn));
    }

    /// `str dt, [sp, #imm12]` —— 存储双精度浮点到 [sp, #off].
    /// STR D: size=11, V=1, opc=00 => 0xFD000000 | imm12<<10 | Rn | Rt
    pub(crate) fn str_dt_sp(&mut self, reg: u8, off: i32) {
        debug_assert!(
            off >= 0 && off % 8 == 0 && off <= 32760,
            "str_dt_sp: off must be non-neg 8-byte aligned, got {off}"
        );
        let imm12 = ((off / 8) as u32) & 0xfff;
        let insn = 0xfd000000u32 | imm12 << 10 | 31u32 << 5 | (reg as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// `ldr s0, [sp, #imm12]` —— 从栈加载单精度浮点到 s0.
    /// imm12 编码: insn[21:10] = imm12/4, 实际偏移范围 ±4 KB / 4 = 1024 个 4 字节槽.
    pub(crate) fn ldr_s0_sp(&mut self, off: i32) {
        let imm12 = (off / 4) as u32 & 0xfff;
        let insn = 0xbd400000u32 | imm12 << 10 | 31 << 5;
        self.emit_insn(&Insn::raw(insn));
    }

    /// `ldr d0, [sp, #imm12]` —— 从栈加载双精度浮点到 d0.
    pub(crate) fn ldr_d0_sp(&mut self, off: i32) {
        let imm12 = (off / 8) as u32 & 0xfff;
        let insn = 0xfd400000u32 | imm12 << 10 | 31 << 5;
        self.emit_insn(&Insn::raw(insn));
    }

    /// 从栈加载 HFA 到 v/s 寄存器组 (AAPCS64 v0-v7)
    pub(crate) fn load_hfa_to_vregs(&mut self, off: i32, hfa: HfaInfo, base_reg: u8) {
        let elem_bytes = (hfa.elem_bits / 8) as i32;
        for i in 0..hfa.count {
            let elem_off = off + i as i32 * elem_bytes;
            let vreg = base_reg + i;
            if hfa.elem_bits == 32 {
                let imm12 = (elem_off / 4) as u32 & 0xfff;
                let insn = 0xbd000000u32 | imm12 << 10 | 31 << 5 | (vreg as u32 & 0x1f);
                self.emit_insn(&Insn::raw(insn));
            } else {
                let imm12 = (elem_off / 8) as u32 & 0xfff;
                let insn = 0xfd000000u32 | imm12 << 10 | 31 << 5 | (vreg as u32 & 0x1f);
                self.emit_insn(&Insn::raw(insn));
            }
        }
    }

    /// 大结构体返回: 从 [sp+off] 拷贝 size 字节到 [x0]
    pub(crate) fn memcpy_sp_to_x0(&mut self, off: i32, size: usize) {
        self.mov_x_reg_sp_off(1, off); // x1 = src
                                       // x0 已是 sret 指针
        let mut remaining = size;
        let mut src_off = 0i32;
        while remaining > 0 {
            let chunk = remaining.min(8);
            if chunk >= 8 {
                self.ldr_x2_sp_indexed(1, src_off);
                self.str_x2_x0_indexed(0, src_off);
            } else if chunk >= 4 {
                self.ldr_w2_sp_indexed(1, src_off);
                self.str_w2_x0_indexed(0, src_off);
            }
            src_off += chunk as i32;
            remaining -= chunk;
        }
    }

    /// [sp + base_off + idx] 加载 x2 (64-bit)
    pub(crate) fn ldr_x2_sp_indexed(&mut self, base: u8, idx: i32) {
        if idx == 0 {
            let insn = 0xf9400000u32 | (base as u32) << 5 | 2;
            self.emit_insn(&Insn::raw(insn));
        } else if (0..4096).contains(&idx) && idx % 8 == 0 {
            let imm12 = (idx / 8) as u32 & 0xfff;
            let insn = 0xf9400000u32 | imm12 << 10 | (base as u32) << 5 | 2;
            self.emit_insn(&Insn::raw(insn));
        }
    }

    pub(crate) fn str_x2_x0_indexed(&mut self, base: u8, idx: i32) {
        if idx == 0 {
            let insn = 0xf9000000u32 | (base as u32) << 5 | 2;
            self.emit_insn(&Insn::raw(insn));
        } else if (0..4096).contains(&idx) && idx % 8 == 0 {
            let imm12 = (idx / 8) as u32 & 0xfff;
            let insn = 0xf9000000u32 | imm12 << 10 | (base as u32) << 5 | 2;
            self.emit_insn(&Insn::raw(insn));
        }
    }

    pub(crate) fn ldr_w2_sp_indexed(&mut self, base: u8, idx: i32) {
        let imm12 = (idx / 4).max(0) as u32 & 0xfff;
        let insn = 0xb9400000u32 | imm12 << 10 | (base as u32) << 5 | 2;
        self.emit_insn(&Insn::raw(insn));
    }

    pub(crate) fn str_w2_x0_indexed(&mut self, base: u8, idx: i32) {
        let imm12 = (idx / 4).max(0) as u32 & 0xfff;
        let insn = 0xb9000000u32 | imm12 << 10 | (base as u32) << 5 | 2;
        self.emit_insn(&Insn::raw(insn));
    }

    pub(crate) fn gen_hfa_expr(&mut self, expr: &TypedExpr, hfa: HfaInfo) {
        if let ExprKind::Local(ref n) = expr.kind {
            if let Some(&off) = self.locals.get(n) {
                self.load_hfa_to_vregs(off, hfa, 0);
                return;
            }
        }
        let _ = self.gen_float_expr(expr);
    }

    pub(crate) fn gen_hfa_expr_from_ast(&mut self, expr: &Expr, hfa: HfaInfo) {
        if let Expr::Ident { name, .. } = expr {
            if let Some(&off) = self.locals.get(&name.name) {
                self.load_hfa_to_vregs(off, hfa, 0);
                return;
            }
        }
        if let Expr::Float { c, .. } = expr {
            self.load_float_s0(c.as_f64() as f32);
        }
        let _ = hfa;
    }
}
