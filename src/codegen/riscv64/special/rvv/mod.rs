// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::riscv64::codegen::{Riscv64Codegen};

impl Riscv64Codegen {
    // ===== M8 新增: 向量扩展 (V) 基础 RVV 指令 =====

    /// VSETVLI rd, rs1, vtype: 设置向量类型
    pub(crate) fn emit_vsetvli(&mut self, rd: u8, rs1: u8, vtype: u32) {
        let insn =
            (vtype & 0x7ff) << 20 | (rs1 as u32) << 15 | 0b111 << 12 | (rd as u32) << 7 | 0x57; // OP-V
        self.emit(&insn.to_le_bytes());
    }

    /// VSETIVLI rd, uimm, vtype: 立即数设置向量类型
    pub(crate) fn emit_vsetivli(&mut self, rd: u8, uimm: u8, vtype: u32) {
        let insn = 0b11 << 20
            | (vtype & 0x1f) << 15
            | ((uimm as u32) & 0x1f) << 15
            | 0b111 << 12
            | (rd as u32) << 7
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VLE32.V vd, (rs1), vm: 向量 32-bit 元素加载
    pub(crate) fn emit_vle32_v(&mut self, vd: u8, rs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn =
            vm_bit | (rs1 as u32) << 15 | 0b010 << 12 | (vd as u32) << 7 | 0b0000111 << 25 | 0x07;
        self.emit(&insn.to_le_bytes());
    }

    /// VSE32.V vs3, (rs1), vm: 向量 32-bit 元素存储
    pub(crate) fn emit_vse32_v(&mut self, vs3: u8, rs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = (vm_bit | (vs3 as u32) << 20 | (rs1 as u32) << 15 | 0b010 << 12)
            | 0b0000111 << 25
            | 0x27;
        self.emit(&insn.to_le_bytes());
    }

    /// VADD.VV vd, vs2, vs1, vm: 向量加法
    pub(crate) fn emit_vadd_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = (vm_bit | (vs2 as u32) << 20 | (vs1 as u32) << 15)
            | (vd as u32) << 7
            | 0b1000001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VMUL.VV vd, vs2, vs1, vm: 向量乘法 (取低 SEW 位)
    pub(crate) fn emit_vmul_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b100 << 12
            | (vd as u32) << 7
            | 0b1010001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VREDUCE.VS vd, vs2, vs1, vm: 向量归约 (简化)
    pub(crate) fn emit_vredsum_vs(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = (vm_bit | (vs2 as u32) << 20 | (vs1 as u32) << 15)
            | (vd as u32) << 7
            | 0b1100001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    // ===== RVV 扩展: 完整向量指令集 =====

    /// VLE8.V vd, (rs1), vm: 向量 8-bit 元素加载
    pub(crate) fn emit_vle8_v(&mut self, vd: u8, rs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = (vm_bit | (rs1 as u32) << 15) | (vd as u32) << 7 | 0b0000111 << 25 | 0x07;
        self.emit(&insn.to_le_bytes());
    }

    /// VLE16.V vd, (rs1), vm: 向量 16-bit 元素加载
    pub(crate) fn emit_vle16_v(&mut self, vd: u8, rs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn =
            vm_bit | (rs1 as u32) << 15 | 0b101 << 12 | (vd as u32) << 7 | 0b0000111 << 25 | 0x07;
        self.emit(&insn.to_le_bytes());
    }

    /// VLE64.V vd, (rs1), vm: 向量 64-bit 元素加载
    pub(crate) fn emit_vle64_v(&mut self, vd: u8, rs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn =
            vm_bit | (rs1 as u32) << 15 | 0b011 << 12 | (vd as u32) << 7 | 0b0000111 << 25 | 0x07;
        self.emit(&insn.to_le_bytes());
    }

    /// VSE8.V vs3, (rs1), vm
    pub(crate) fn emit_vse8_v(&mut self, vs3: u8, rs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = (vm_bit | (vs3 as u32) << 20 | (rs1 as u32) << 15) | 0b0000111 << 25 | 0x27;
        self.emit(&insn.to_le_bytes());
    }

    /// VSE16.V vs3, (rs1), vm
    pub(crate) fn emit_vse16_v(&mut self, vs3: u8, rs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn =
            vm_bit | (vs3 as u32) << 20 | (rs1 as u32) << 15 | 0b101 << 12 | 0b0000111 << 25 | 0x27;
        self.emit(&insn.to_le_bytes());
    }

    /// VSE64.V vs3, (rs1), vm
    pub(crate) fn emit_vse64_v(&mut self, vs3: u8, rs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn =
            vm_bit | (vs3 as u32) << 20 | (rs1 as u32) << 15 | 0b011 << 12 | 0b0000111 << 25 | 0x27;
        self.emit(&insn.to_le_bytes());
    }

    /// VSUB.VV vd, vs2, vs1, vm: 向量减法
    pub(crate) fn emit_vsub_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | (vd as u32) << 7
            | 0b0000101 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VAND.VV vd, vs2, vs1, vm
    pub(crate) fn emit_vand_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b001 << 12
            | (vd as u32) << 7
            | 0b0010011 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VOR.VV vd, vs2, vs1, vm
    pub(crate) fn emit_vor_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b010 << 12
            | (vd as u32) << 7
            | 0b0010101 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VXOR.VV vd, vs2, vs1, vm
    pub(crate) fn emit_vxor_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b011 << 12
            | (vd as u32) << 7
            | 0b0010111 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VSLL.VV vd, vs2, vs1, vm: 向量左移
    pub(crate) fn emit_vsll_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b101 << 12
            | (vd as u32) << 7
            | 0b1000011 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VSRL.VV vd, vs2, vs1, vm: 向量逻辑右移
    pub(crate) fn emit_vsrl_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b101 << 12
            | (vd as u32) << 7
            | 0b1010001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VSRA.VV vd, vs2, vs1, vm: 向量算术右移
    pub(crate) fn emit_vsra_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b101 << 12
            | (vd as u32) << 7
            | 0b1010011 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    // --- 宽化/窄化指令 ---

    /// VWADD.VV vd, vs2, vs1, vm: 宽化加法 (2*SEW)
    pub(crate) fn emit_vwadd_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b010 << 12
            | (vd as u32) << 7
            | 0b1100011 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VWSUB.VV vd, vs2, vs1, vm: 宽化减法
    pub(crate) fn emit_vwsub_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b010 << 12
            | (vd as u32) << 7
            | 0b1100101 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VWMUL.VV vd, vs2, vs1, vm: 宽化乘法
    pub(crate) fn emit_vwmul_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b010 << 12
            | (vd as u32) << 7
            | 0b1110111 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VWMULU.VV vd, vs2, vs1, vm: 宽化无符号乘法
    pub(crate) fn emit_vwmulu_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = (vm_bit | (vs2 as u32) << 20 | (vs1 as u32) << 15)
            | (vd as u32) << 7
            | 0b1110001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VNSRL.WI vd, vs2, uimm, vm: 窄化右移 (立即数)
    pub(crate) fn emit_vnsrl_wi(&mut self, vd: u8, vs2: u8, uimm: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = (vm_bit | (vs2 as u32) << 20 | ((uimm as u32) & 0x1f) << 15)
            | (vd as u32) << 7
            | 0b1011001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VNSRA.WI vd, vs2, uimm, vm: 窄化算术右移
    pub(crate) fn emit_vnsra_wi(&mut self, vd: u8, vs2: u8, uimm: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = (vm_bit | (vs2 as u32) << 20 | ((uimm as u32) & 0x1f) << 15)
            | (vd as u32) << 7
            | 0b1011011 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    // --- 窄化钳位指令 ---

    /// VNCLIP.WV vd, vs2, vs1, vm: 有符号窄化钳位右移
    pub(crate) fn emit_vnclip_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1u32 << 25 } else { 0 };
        let insn = (0b100110u32 << 26)
            | vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b011 << 12
            | (vd as u32) << 7
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VNCLIPU.WV vd, vs2, vs1, vm: 无符号窄化钳位右移
    pub(crate) fn emit_vnclipu_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1u32 << 25 } else { 0 };
        let insn = (0b100110u32 << 26)
            | vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b010 << 12
            | (vd as u32) << 7
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    // --- 掩码指令 ---

    /// VMAND.MM vd, vs2, vs1: 掩码 AND
    pub(crate) fn emit_vmand_mm(&mut self, vd: u8, vs2: u8, vs1: u8) {
        let insn = (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b001 << 12
            | (vd as u32) << 7
            | 0b0110011 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VMOR.MM vd, vs2, vs1: 掩码 OR
    pub(crate) fn emit_vmor_mm(&mut self, vd: u8, vs2: u8, vs1: u8) {
        let insn = (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b010 << 12
            | (vd as u32) << 7
            | 0b0110101 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VMXOR.MM vd, vs2, vs1: 掩码 XOR
    pub(crate) fn emit_vmxor_mm(&mut self, vd: u8, vs2: u8, vs1: u8) {
        let insn = (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b011 << 12
            | (vd as u32) << 7
            | 0b0110111 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VMNOT.M vd, vs2: 掩码 NOT (vmandn with v0=ones)
    pub(crate) fn emit_vmnot_m(&mut self, vd: u8, vs2: u8) {
        // VMNOT.M vd, vs2 = VMXNOR.M vd, vs2, v0 (all-ones mask)
        let insn = ((vs2 as u32) << 20) | 0b100 << 12 | (vd as u32) << 7 | 0b0111001 << 25 | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    // --- 浮点向量指令 ---

    /// VFADD.VV vd, vs2, vs1, vm: 浮点向量加法
    pub(crate) fn emit_vfadd_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b001 << 12
            | (vd as u32) << 7
            | 0b0000001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VFSUB.VV vd, vs2, vs1, vm: 浮点向量减法
    pub(crate) fn emit_vfsub_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b001 << 12
            | (vd as u32) << 7
            | 0b0000101 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VFMUL.VV vd, vs2, vs1, vm: 浮点向量乘法
    pub(crate) fn emit_vfmul_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b001 << 12
            | (vd as u32) << 7
            | 0b1001001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VFDIV.VV vd, vs2, vs1, vm: 浮点向量除法
    pub(crate) fn emit_vfdiv_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b001 << 12
            | (vd as u32) << 7
            | 0b1000001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VFMADD.VV vd, vs1, vs2, vm: 浮点向量 fused multiply-add
    pub(crate) fn emit_vfmadd_vv(&mut self, vd: u8, vs1: u8, vs2: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b001 << 12
            | (vd as u32) << 7
            | 0b1010001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VFMIN.VV vd, vs2, vs1, vm: 浮点向量 min
    pub(crate) fn emit_vfmin_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b001 << 12
            | (vd as u32) << 7
            | 0b0001001 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VFMAX.VV vd, vs2, vs1, vm: 浮点向量 max
    pub(crate) fn emit_vfmax_vv(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b001 << 12
            | (vd as u32) << 7
            | 0b0001101 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    /// VFREDSUM.VS vd, vs2, vs1, vm: 浮点归约求和
    pub(crate) fn emit_vfredsum_vs(&mut self, vd: u8, vs2: u8, vs1: u8, vm: bool) {
        let vm_bit = if vm { 1 << 25 } else { 0 };
        let insn = vm_bit
            | (vs2 as u32) << 20
            | (vs1 as u32) << 15
            | 0b001 << 12
            | (vd as u32) << 7
            | 0b0000011 << 25
            | 0x57;
        self.emit(&insn.to_le_bytes());
    }

    // ===== M8 新增: 特权指令 / CSR 访问 =====
}
