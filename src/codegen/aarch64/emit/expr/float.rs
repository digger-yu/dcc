// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::aarch64::codegen::Aarch64Codegen;
use crate::codegen::aarch64::insn::Insn;

impl Aarch64Codegen {
    pub(crate) fn mov_v_reg_s0(&mut self, reg: u8) {
        let insn = 0x1e204000u32 | ((reg as u32 & 7) << 5);
        self.emit_insn(&Insn::raw(insn));
    }

    pub(crate) fn mov_v_reg_v0(&mut self, reg: u8) {
        if reg == 0 {
            return;
        }
        let insn = 0x4ea0_1c00u32 | (reg as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    pub(crate) fn dup_v0_4s_w0(&mut self) {
        self.emit_insn(&Insn::raw(0x4e040c00));
    }

    pub(crate) fn mov_s1_s0(&mut self) {
        self.emit_insn(&Insn::raw(0x1e200020));
    }

    pub(crate) fn neg_s0(&mut self) {
        self.emit_insn(&Insn::raw(0x1ea00000)); // fneg s0, s0
    }

    pub(crate) fn mov_v1_v0(&mut self) {
        self.emit_insn(&Insn::raw(0x4e810020)); // orr v1.16b, v0.16b, v0.16b
    }

    pub(crate) fn zero_v0(&mut self) {
        self.emit_insn(&Insn::raw(0x4e800000)); // movi v0.4s, #0
    }

    pub(crate) fn zero_v1(&mut self) {
        self.emit_insn(&Insn::raw(0x4e800001)); // movi v1.4s, #0
    }

    /// `str st, [sp, #imm12]` — store single-precision float to stack.
    /// Generic version for any register.
    pub(crate) fn str_st_sp(&mut self, reg: u8, off: i32) {
        let imm12 = (off / 4) as u32 & 0xfff;
        let insn = 0xbd000000u32 | imm12 << 10 | 31u32 << 5 | (reg as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// `ldr st, [sp, #imm12]` — load single-precision float from stack.
    /// Generic version for any register.
    pub(crate) fn ldr_st_sp(&mut self, reg: u8, off: i32) {
        let imm12 = (off / 4) as u32 & 0xfff;
        let insn = 0xbd400000u32 | imm12 << 10 | 31u32 << 5 | (reg as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    pub(crate) fn str_s1_sp(&mut self, off: i32) {
        self.str_st_sp(1, off);
    }

    pub(crate) fn ldr_s1_sp(&mut self, off: i32) {
        self.ldr_st_sp(1, off);
    }

    pub(crate) fn ldr_s2_sp(&mut self, off: i32) {
        self.ldr_st_sp(2, off);
    }

    pub(crate) fn ldr_s3_sp(&mut self, off: i32) {
        self.ldr_st_sp(3, off);
    }

    /// FADD Sd, Sn, Sm — single-precision floating-point add
    pub(crate) fn emit_fadd(&mut self, rd: u8, rn: u8, rm: u8) {
        // FADD (scalar): 0x1E202800 | Rm<<16 | Rn<<5 | Rd
        let insn = 0x1e202800u32
            | ((rm as u32 & 0x1f) << 16)
            | ((rn as u32 & 0x1f) << 5)
            | (rd as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// FSUB Sd, Sn, Sm — single-precision floating-point subtract
    pub(crate) fn emit_fsub(&mut self, rd: u8, rn: u8, rm: u8) {
        // FSUB (scalar): 0x1E203800 | Rm<<16 | Rn<<5 | Rd
        let insn = 0x1e203800u32
            | ((rm as u32 & 0x1f) << 16)
            | ((rn as u32 & 0x1f) << 5)
            | (rd as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// Complex add helpers: component-wise
    pub(crate) fn emit_fadd_s0_v2_v0(&mut self) {
        self.emit_fadd(0, 2, 0); // fadd s0, s2, s0
    }

    pub(crate) fn emit_fadd_s1_v3_v1(&mut self) {
        self.emit_fadd(1, 3, 1); // fadd s1, s3, s1
    }

    pub(crate) fn emit_fsub_s0_v2_v0(&mut self) {
        self.emit_fsub(0, 2, 0); // fsub s0, s2, s0
    }

    pub(crate) fn emit_fsub_s1_v3_v1(&mut self) {
        self.emit_fsub(1, 3, 1); // fsub s1, s3, s1
    }
}
