// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::aarch64::codegen::Aarch64Codegen;
use crate::codegen::aarch64::insn::Insn;

impl Aarch64Codegen {
    pub(crate) fn emit_irg(&mut self, rd: u8, rn: u8) {
        let insn = 0x9ac00000u32
            | (0x1f << 16) // Rm = XZR (省略第二源操作数)
            | (0b000100 << 10)
            | (((rn as u32) & 0x1f) << 5)
            | ((rd as u32) & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// MTE: Add with Tag (ADDG)
    /// `addg xd, xn, #uimm6, #uimm4`: 地址加 uimm6*16, tag 加 uimm4.
    /// 编码 (Add/subtract with tags):
    ///   `1 0 0 1 0 0 0 1 1 0 imm6 (0)(0) imm4 Rn Rd`
    ///   = 0x91800000 | (imm6 << 16) | (imm4 << 10) | (Rn << 5) | Rd
    /// 参考 LLVM armv8.5a-mte.s: addg sp, x2, #32, #3 -> 0x91820c5f
    pub(crate) fn emit_addg(&mut self, rd: u8, rn: u8, imm6: u32, imm4: u32) {
        let insn = 0x91800000u32
            | ((imm6 & 0x3f) << 16)
            | ((imm4 & 0xf) << 10)
            | (((rn as u32) & 0x1f) << 5)
            | ((rd as u32) & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// MTE: Subtract with Tag (SUBG)
    /// `subg xd, xn, #uimm6, #uimm4`: 地址减 uimm6*16, tag 减 uimm4.
    /// 编码: 与 ADDG 相同但 bit30 置位
    ///   = 0xD1800000 | (imm6 << 16) | (imm4 << 10) | (Rn << 5) | Rd
    /// 参考 LLVM armv8.5a-mte.s: subg x0, x1, #0, #1 -> 0xd1800420
    pub(crate) fn emit_subg(&mut self, rd: u8, rn: u8, imm6: u32, imm4: u32) {
        let insn = 0xd1800000u32
            | ((imm6 & 0x3f) << 16)
            | ((imm4 & 0xf) << 10)
            | (((rn as u32) & 0x1f) << 5)
            | ((rd as u32) & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// MTE: Store Allocation Tag (STG, signed-offset 形式)
    /// `stg xt, [xn, #imm9*16]`: 把 xt 顶字节的 tag 写入 [xn+off] 所在 16B granule
    /// 的 tag 内存. imm9 是有符号 granule 偏移 (乘 16, 范围 -256..255).
    /// 编码 (Store tag, signed offset):
    ///   `1 1 0 1 1 0 0 1 0 0 1 imm9 1 0 Rn Rt`
    ///   = 0xD9200000 | (imm9 << 12) | (0b10 << 10) | (Rn << 5) | Rt
    /// 参考 LLVM armv8.5a-mte.s: stg x3, [sp, #16] -> 0xd9201be3
    pub(crate) fn emit_stg(&mut self, rt: u8, rn: u8, imm9: u32) {
        let insn = 0xd9200000u32
            | ((imm9 & 0x1ff) << 12)
            | (0b10 << 10)
            | (((rn as u32) & 0x1f) << 5)
            | ((rt as u32) & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// MTE: Load Allocation Tag (LDG, signed-offset 形式)
    /// `ldg xt, [xn, #imm9*16]`: 读出 [xn+off] 所在 granule 的 tag, 合并进 xt 高字节.
    /// 编码:
    ///   `1 1 0 1 1 0 0 1 0 1 1 imm9 0 0 Rn Rt`
    ///   = 0xD9600000 | (imm9 << 12) | (Rn << 5) | Rt
    pub(crate) fn emit_ldg(&mut self, rt: u8, rn: u8, imm9: u32) {
        let insn = 0xd9600000u32
            | ((imm9 & 0x1ff) << 12)
            | (((rn as u32) & 0x1f) << 5)
            | ((rt as u32) & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    /// MTE: Set PSTATE.TCO (Tag Check Override)
    /// 注意: 当前编码对应 SPSel 选择器, 保留用于显式禁用 tag 检查的扩展点,
    /// 未在 prologue tag 栈帧流程中使用.
    pub(crate) fn emit_settco(&mut self, value: bool) {
        let insn = 0xd500401fu32 | ((value as u32) << 3);
        self.emit_insn(&Insn::raw(insn));
    }
}
