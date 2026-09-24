// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::riscv64::codegen::Riscv64Codegen;

impl Riscv64Codegen {
    /// FLD rd, offset(rs1): I 格式, 64-bit 浮点加载 (D 扩展)
    pub(crate) fn emit_fld(&mut self, rd: u8, rs1: u8, off: i32) {
        let imm = off as u32 & 0xfff;
        let insn = imm << 20 | (rs1 as u32) << 15 | 0b011 << 12 | (rd as u32) << 7 | 0x07;
        self.emit(&insn.to_le_bytes());
    }

    /// FSD rs2, offset(rs1): S 格式, 64-bit 浮点存储 (D 扩展)
    pub(crate) fn emit_fsd(&mut self, rs2: u8, rs1: u8, off: i32) {
        let imm = off as u32;
        let insn = ((imm >> 5) & 0x7f) << 25
            | (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0b011 << 12
            | (imm & 0x1f) << 7
            | 0x27;
        self.emit(&insn.to_le_bytes());
    }

    /// FSW rs2, offset(rs1): S 格式, 32-bit 浮点存储 (F 扩展)
    pub(crate) fn emit_fsw(&mut self, rs2: u8, rs1: u8, off: i32) {
        let imm = off as u32;
        let insn = ((imm >> 5) & 0x7f) << 25
            | (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0b010 << 12
            | (imm & 0x1f) << 7
            | 0x27;
        self.emit(&insn.to_le_bytes());
    }

    /// FADD.D fd, fs1, fs2: R 格式, funct7=0000001
    pub(crate) fn emit_fadd_d(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0000001 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FSUB.D fd, fs1, fs2: R 格式, funct7=0000101
    pub(crate) fn emit_fsub_d(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0000101 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FMUL.D fd, fs1, fs2: R 格式, funct7=0001001
    pub(crate) fn emit_fmul_d(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0001001 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FDIV.D fd, fs1, fs2: R 格式, funct7=0001101
    pub(crate) fn emit_fdiv_d(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0001101 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FSQRT.D fd, fs1: 双精度平方根, funct7=0101101
    pub(crate) fn emit_fsqrt_d(&mut self, fd: u8, fs1: u8) {
        let insn = ((fs1 as u32) << 15) | (fd as u32) << 7 | 0b0101101 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FMIN.D fd, fs1, fs2: 双精度最小值, funct7=0010101, funct3=000
    pub(crate) fn emit_fmin_d(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0010101 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FMAX.D fd, fs1, fs2: 双精度最大值, funct7=0010101, funct3=001
    pub(crate) fn emit_fmax_d(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn = (fs2 as u32) << 20
            | (fs1 as u32) << 15
            | 0b001 << 12
            | (fd as u32) << 7
            | 0b0010101 << 25
            | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FEQ.D rd, fs1, fs2: 双精度相等比较, funct7=1010001, funct3=010
    pub(crate) fn emit_feq_d(&mut self, rd: u8, fs1: u8, fs2: u8) {
        let insn = (fs2 as u32) << 20
            | (fs1 as u32) << 15
            | 0b010 << 12
            | (rd as u32) << 7
            | 0b1010001 << 25
            | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FLT.D rd, fs1, fs2: 双精度小于比较, funct7=1010001, funct3=001
    pub(crate) fn emit_flt_d(&mut self, rd: u8, fs1: u8, fs2: u8) {
        let insn = (fs2 as u32) << 20
            | (fs1 as u32) << 15
            | 0b001 << 12
            | (rd as u32) << 7
            | 0b1010001 << 25
            | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FLE.D rd, fs1, fs2: 双精度小于等于比较, funct7=1010001, funct3=000
    pub(crate) fn emit_fle_d(&mut self, rd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (rd as u32) << 7 | 0b1010001 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCVT.D.W fd, rs1: 整数转双精度
    pub(crate) fn emit_fcvt_d_w(&mut self, fd: u8, rs1: u8) {
        let insn = ((rs1 as u32) << 15) | (fd as u32) << 7 | 0b1101000 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCLASS.S rd, fs1: 单精度浮点分类, funct7=1010000, funct3=001
    pub(crate) fn emit_fclass_s(&mut self, rd: u8, fs1: u8) {
        let insn = (fs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0b1010000 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCLASS.D rd, fs1: 双精度浮点分类, funct7=1010001, funct3=001
    pub(crate) fn emit_fclass_d(&mut self, rd: u8, fs1: u8) {
        let insn = (fs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0b1010001 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCVT.W.D rd, fs1: 双精度转整数
    pub(crate) fn emit_fcvt_w_d(&mut self, rd: u8, fs1: u8) {
        let insn = (fs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0b1100000 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCVT.W.S rd, fs1: 单精度转 32-bit 有符号整数
    pub(crate) fn emit_fcvt_w_s(&mut self, rd: u8, fs1: u8) {
        let insn = (fs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0b1100000 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCVT.WU.S rd, fs1: 单精度转 32-bit 无符号整数
    pub(crate) fn emit_fcvt_wu_s(&mut self, rd: u8, fs1: u8) {
        let insn = (fs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0b1100001 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCVT.WU.D rd, fs1: 双精度转 32-bit 无符号整数
    pub(crate) fn emit_fcvt_wu_d(&mut self, rd: u8, fs1: u8) {
        let insn = (fs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0b1100001 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCVT.S.WU fd, rs1: 无符号 32-bit 整数转单精度
    pub(crate) fn emit_fcvt_s_wu(&mut self, fd: u8, rs1: u8) {
        let insn = 0xC8000053u32 | ((rs1 as u32) << 15) | ((fd as u32) << 7);
        self.emit(&insn.to_le_bytes());
    }

    /// FCVT.D.S fd, fs1: float->double 提升
    pub(crate) fn emit_fcvt_d_s(&mut self, fd: u8, fs1: u8) {
        let insn = ((fs1 as u32) << 15) | (fd as u32) << 7 | 0b0100000 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCVT.S.D fd, fs1: double->float 截断
    pub(crate) fn emit_fcvt_s_d(&mut self, fd: u8, fs1: u8) {
        let insn = ((fs1 as u32) << 15) | (fd as u32) << 7 | 0b0100001 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCVT.D.L fd, rs1: 64-bit 整数转双精度 (RV64)
    pub(crate) fn emit_fcvt_d_l(&mut self, fd: u8, rs1: u8) {
        let insn = ((rs1 as u32) << 15) | (fd as u32) << 7 | 0b1101010 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FCVT.L.D rd, fs1: 双精度转 64-bit 整数 (RV64)
    pub(crate) fn emit_fcvt_l_d(&mut self, rd: u8, fs1: u8) {
        let insn = (fs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0b1100010 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FMV.D.X fd, rs1: 整数寄存器移动到浮点寄存器 (64-bit)
    pub(crate) fn emit_fmv_d_x(&mut self, fd: u8, rs1: u8) {
        let insn = ((rs1 as u32) << 15) | (fd as u32) << 7 | 0b1111000 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FMV.X.D rd, fs1: 浮点寄存器移动到整数寄存器 (64-bit)
    pub(crate) fn emit_fmv_x_d(&mut self, rd: u8, fs1: u8) {
        let insn = ((fs1 as u32) << 15) | (rd as u32) << 7 | 0b1110000 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FSGNJ.S fd, fs1, fs2: 单精度符号注入
    pub(crate) fn emit_fsgnj_s(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0010000 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }

    /// FSGNJ.D fd, fs1, fs2: 双精度符号注入
    pub(crate) fn emit_fsgnj_d(&mut self, fd: u8, fs1: u8, fs2: u8) {
        let insn =
            ((fs2 as u32) << 20 | (fs1 as u32) << 15) | (fd as u32) << 7 | 0b0010001 << 25 | 0x53;
        self.emit(&insn.to_le_bytes());
    }
}
