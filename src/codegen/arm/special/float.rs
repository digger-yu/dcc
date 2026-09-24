// ARM backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::BinOp;

use crate::codegen::arm::codegen::ArmCodegen;

impl ArmCodegen {
    // ===== VFP 浮点指令发射器 (ARMv7 VFPv3) =====

    /// VLDR: 加载单精度浮点数到 VFP 寄存器
    /// VLDR Sd, [fp, #offset*4] (单精度)
    pub(crate) fn emit_vldr_s(&mut self, sd: u8, fp_off: i32) {
        // VLDR encoding: cond 1101 P U D W 1011 Rn imm8
        // P=1, U=depends, D=Sd bit 0, W=0, Rn=fp(11)
        // imm8 = offset/4 (signed, -255..255 * 4)
        let (u_bit, abs_off) = if fp_off >= 0 {
            (1u32, fp_off as u32)
        } else {
            (0u32, (-fp_off) as u32)
        };
        let imm8 = (abs_off / 4) & 0xFF;
        let d_bit = ((sd & 1) as u32) << 22;
        let sd_hi = ((sd >> 1) & 0xF) as u32;
        let inst = 0xED9B0A00u32 // cond=E, P=1, U=1, W=0, Rn=fp
            | (u_bit << 23)
            | d_bit
            | (sd_hi << 12)
            | imm8;
        // Adjust for U bit: if negative, clear U bit
        let inst = if fp_off < 0 { inst & !(1 << 23) } else { inst };
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VLDR: 加载双精度浮点数到 VFP 寄存器
    /// VLDR Dd, [fp, #offset*4] (双精度)
    pub(crate) fn emit_vldr_d(&mut self, dd: u8, fp_off: i32) {
        // VLDR double encoding: cond 1101 P U D W 1011 Rn imm8
        // Dd: D bit 0 in [22], Dd[3:1] in [15:12]
        let (u_bit, abs_off) = if fp_off >= 0 {
            (1u32, fp_off as u32)
        } else {
            (0u32, (-fp_off) as u32)
        };
        let imm8 = (abs_off / 4) & 0xFF;
        let d_bit = ((dd & 1) as u32) << 22;
        let dd_hi = ((dd >> 1) & 0xF) as u32;
        let inst = 0xED9B0B00u32 // cond=E, P=1, Rn=fp, size=11 (double)
            | (u_bit << 23)
            | d_bit
            | (dd_hi << 12)
            | imm8;
        let inst = if fp_off < 0 { inst & !(1 << 23) } else { inst };
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VSTR: 存储单精度浮点数从 VFP 寄存器到内存
    pub(crate) fn emit_vstr_s(&mut self, sd: u8, fp_off: i32) {
        let (u_bit, abs_off) = if fp_off >= 0 {
            (1u32, fp_off as u32)
        } else {
            (0u32, (-fp_off) as u32)
        };
        let imm8 = (abs_off / 4) & 0xFF;
        let d_bit = ((sd & 1) as u32) << 22;
        let sd_hi = ((sd >> 1) & 0xF) as u32;
        let inst = 0xED8B0A00u32 // VSTR single: cond=E, P=1, Rn=fp
            | (u_bit << 23)
            | d_bit
            | (sd_hi << 12)
            | imm8;
        let inst = if fp_off < 0 { inst & !(1 << 23) } else { inst };
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VSTR: 存储双精度浮点数从 VFP 寄存器到内存
    pub(crate) fn emit_vstr_d(&mut self, dd: u8, fp_off: i32) {
        let (u_bit, abs_off) = if fp_off >= 0 {
            (1u32, fp_off as u32)
        } else {
            (0u32, (-fp_off) as u32)
        };
        let imm8 = (abs_off / 4) & 0xFF;
        let d_bit = ((dd & 1) as u32) << 22;
        let dd_hi = ((dd >> 1) & 0xF) as u32;
        let inst = 0xED8B0B00u32 // VSTR double
            | (u_bit << 23)
            | d_bit
            | (dd_hi << 12)
            | imm8;
        let inst = if fp_off < 0 { inst & !(1 << 23) } else { inst };
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VADD: 单精度浮点加法 Sd = Sn + Sm
    pub(crate) fn emit_vadd_s(&mut self, sd: u8, sn: u8, sm: u8) {
        // VADD.S encoding: cond 1110 0011 N 00 Vn Vd 1010 N M 0 Vm
        let n_bit = ((sn & 1) as u32) << 7;
        let m_bit = ((sm & 1) as u32) << 5;
        let d_bit = ((sd & 1) as u32) << 22;
        let sn_hi = ((sn >> 1) & 0xF) as u32;
        let sm_hi = ((sm >> 1) & 0xF) as u32;
        let sd_hi = ((sd >> 1) & 0xF) as u32;
        let inst = 0xEE300A00u32 | d_bit | (sn_hi << 16) | (sd_hi << 12) | n_bit | sm_hi | m_bit;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VSUB: 单精度浮点减法 Sd = Sn - Sm
    pub(crate) fn emit_vsub_s(&mut self, sd: u8, sn: u8, sm: u8) {
        let n_bit = ((sn & 1) as u32) << 7;
        let m_bit = ((sm & 1) as u32) << 5;
        let d_bit = ((sd & 1) as u32) << 22;
        let sn_hi = ((sn >> 1) & 0xF) as u32;
        let sm_hi = ((sm >> 1) & 0xF) as u32;
        let sd_hi = ((sd >> 1) & 0xF) as u32;
        let inst = 0xEE300A40u32 // VSUB: same as VADD but bit 6 set
            | d_bit
            | (sn_hi << 16)
            | (sd_hi << 12)
            | n_bit
            | sm_hi
            | m_bit;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VMUL: 单精度浮点乘法 Sd = Sn * Sm
    pub(crate) fn emit_vmul_s(&mut self, sd: u8, sn: u8, sm: u8) {
        let n_bit = ((sn & 1) as u32) << 7;
        let m_bit = ((sm & 1) as u32) << 5;
        let d_bit = ((sd & 1) as u32) << 22;
        let sn_hi = ((sn >> 1) & 0xF) as u32;
        let sm_hi = ((sm >> 1) & 0xF) as u32;
        let sd_hi = ((sd >> 1) & 0xF) as u32;
        let inst = 0xEE200A00u32 // VMUL.S
            | d_bit
            | (sn_hi << 16)
            | (sd_hi << 12)
            | n_bit
            | sm_hi
            | m_bit;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VDIV: 单精度浮点除法 Sd = Sn / Sm (VFPv3+)
    pub(crate) fn emit_vdiv_s(&mut self, sd: u8, sn: u8, sm: u8) {
        let n_bit = ((sn & 1) as u32) << 7;
        let m_bit = ((sm & 1) as u32) << 5;
        let d_bit = ((sd & 1) as u32) << 22;
        let sn_hi = ((sn >> 1) & 0xF) as u32;
        let sm_hi = ((sm >> 1) & 0xF) as u32;
        let sd_hi = ((sd >> 1) & 0xF) as u32;
        let inst = 0xEE800A00u32 // VDIV.S
            | d_bit
            | (sn_hi << 16)
            | (sd_hi << 12)
            | n_bit
            | sm_hi
            | m_bit;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VADD: 双精度浮点加法 Dd = Dn + Dm
    pub(crate) fn emit_vadd_d(&mut self, dd: u8, dn: u8, dm: u8) {
        let n_bit = ((dn & 1) as u32) << 7;
        let m_bit = ((dm & 1) as u32) << 5;
        let d_bit = ((dd & 1) as u32) << 22;
        let dn_hi = ((dn >> 1) & 0xF) as u32;
        let dm_hi = ((dm >> 1) & 0xF) as u32;
        let dd_hi = ((dd >> 1) & 0xF) as u32;
        let inst = 0xEE300B00u32 // VADD.D
            | d_bit
            | (dn_hi << 16)
            | (dd_hi << 12)
            | n_bit
            | dm_hi
            | m_bit;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VSUB: 双精度浮点减法 Dd = Dn - Dm
    pub(crate) fn emit_vsub_d(&mut self, dd: u8, dn: u8, dm: u8) {
        let n_bit = ((dn & 1) as u32) << 7;
        let m_bit = ((dm & 1) as u32) << 5;
        let d_bit = ((dd & 1) as u32) << 22;
        let dn_hi = ((dn >> 1) & 0xF) as u32;
        let dm_hi = ((dm >> 1) & 0xF) as u32;
        let dd_hi = ((dd >> 1) & 0xF) as u32;
        let inst = 0xEE300B40u32 // VSUB.D
            | d_bit
            | (dn_hi << 16)
            | (dd_hi << 12)
            | n_bit
            | dm_hi
            | m_bit;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VMUL: 双精度浮点乘法 Dd = Dn * Dm
    pub(crate) fn emit_vmul_d(&mut self, dd: u8, dn: u8, dm: u8) {
        let n_bit = ((dn & 1) as u32) << 7;
        let m_bit = ((dm & 1) as u32) << 5;
        let d_bit = ((dd & 1) as u32) << 22;
        let dn_hi = ((dn >> 1) & 0xF) as u32;
        let dm_hi = ((dm >> 1) & 0xF) as u32;
        let dd_hi = ((dd >> 1) & 0xF) as u32;
        let inst = 0xEE200B00u32 // VMUL.D
            | d_bit
            | (dn_hi << 16)
            | (dd_hi << 12)
            | n_bit
            | dm_hi
            | m_bit;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VDIV: 双精度浮点除法 Dd = Dn / Dm (VFPv3+)
    pub(crate) fn emit_vdiv_d(&mut self, dd: u8, dn: u8, dm: u8) {
        let n_bit = ((dn & 1) as u32) << 7;
        let m_bit = ((dm & 1) as u32) << 5;
        let d_bit = ((dd & 1) as u32) << 22;
        let dn_hi = ((dn >> 1) & 0xF) as u32;
        let dm_hi = ((dm >> 1) & 0xF) as u32;
        let dd_hi = ((dd >> 1) & 0xF) as u32;
        let inst = 0xEE800B00u32 // VDIV.D
            | d_bit
            | (dn_hi << 16)
            | (dd_hi << 12)
            | n_bit
            | dm_hi
            | m_bit;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VCMP: 比较单精度浮点数并更新 FPSCR
    pub(crate) fn emit_vcmp_s(&mut self, sd: u8, sm: u8) {
        let d_bit = ((sd & 1) as u32) << 22;
        let sd_hi = ((sd >> 1) & 0xF) as u32;
        let m_bit = ((sm & 1) as u32) << 5;
        let sm_hi = ((sm >> 1) & 0xF) as u32;
        // VCMP.S Sd, Sm: cond 1110 1011 1 D 00 Vd 1010 E 1 M 0 Vm
        let inst = 0xEEB40A40u32 | d_bit | (sd_hi << 12) | m_bit | sm_hi;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VCMP: 比较双精度浮点数并更新 FPSCR
    pub(crate) fn emit_vcmp_d(&mut self, dd: u8, dm: u8) {
        let d_bit = ((dd & 1) as u32) << 22;
        let dd_hi = ((dd >> 1) & 0xF) as u32;
        let m_bit = ((dm & 1) as u32) << 5;
        let dm_hi = ((dm >> 1) & 0xF) as u32;
        // VCMP.D Dd, Dm
        let inst = 0xEEB40B40u32 | d_bit | (dd_hi << 12) | m_bit | dm_hi;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VMRS APSR_nzcv, FPSCR: 将 FPSCR 标志移到 ARM CPSR
    pub(crate) fn emit_vmrs_apsr(&mut self) {
        // VMRS APSR_nzcv, FPSCR: 0xEEF1FA10
        self.code.extend_from_slice(&0xEEF1FA10u32.to_le_bytes());
    }

    /// VMOV: 整数寄存器到单精度寄存器 VMOV Sd, Rn
    pub(crate) fn emit_vmov_s_from_reg(&mut self, sd: u8, rn: u8) {
        let d_bit = ((sd & 1) as u32) << 22;
        let sd_hi = ((sd >> 1) & 0xF) as u32;
        // VMOV Sd, Rn: cond 1110 0000 0 D 00 Vd 1010 N 0 0 Rn
        let inst = 0xEE000A10u32 | d_bit | (sd_hi << 12) | (rn as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VMOV: 单精度寄存器到整数寄存器 VMOV Rn, Sm
    pub(crate) fn emit_vmov_reg_from_s(&mut self, rn: u8, sm: u8) {
        let m_bit = ((sm & 1) as u32) << 5;
        let sm_hi = ((sm >> 1) & 0xF) as u32;
        // VMOV Rn, Sm: cond 1110 0000 1 D 00 Vn 1010 N 1 0 Rn
        let inst = 0xEE100A10u32 | m_bit | (sm_hi << 12) | (rn as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VMOV: 双精度寄存器对到两个整数寄存器 VMOV Rn, Rn+1, Dm
    pub(crate) fn emit_vmov_rr_from_d(&mut self, rn: u8, dm: u8) {
        let m_bit = ((dm & 1) as u32) << 5;
        let dm_hi = ((dm >> 1) & 0xF) as u32;
        // VMOV r1, r2, Dm: cond 1100 0101 Rn1 Rn2 Dm_hi 1011 M 1 Dm_lo
        let inst = 0xEC500B10u32 | ((rn as u32) << 12) | (((rn + 1) as u32) << 16) | m_bit | dm_hi;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VMOV: 两个整数寄存器到双精度寄存器 VMOV Dm, Rn, Rn+1
    pub(crate) fn emit_vmov_d_from_rr(&mut self, dm: u8, rn: u8) {
        let m_bit = ((dm & 1) as u32) << 5;
        let dm_hi = ((dm >> 1) & 0xF) as u32;
        // VMOV Dm, r1, r2
        let inst = 0xEC400B10u32 | ((rn as u32) << 12) | (((rn + 1) as u32) << 16) | m_bit | dm_hi;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VCVT: 单精度转整数 VCVT.S32.F32 Sd, Sm
    pub(crate) fn emit_vcvt_s32_f32(&mut self, sd: u8, sm: u8) {
        let d_bit = ((sd & 1) as u32) << 22;
        let sd_hi = ((sd >> 1) & 0xF) as u32;
        let m_bit = ((sm & 1) as u32) << 5;
        let sm_hi = ((sm >> 1) & 0xF) as u32;
        // VCVT.S32.F32: cond 1111 1011 1 D 00 Vd 1010 11 M 0 Vm
        let inst = 0xEEBD0AC0u32 | d_bit | (sd_hi << 12) | m_bit | sm_hi;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VCVT: 整数转单精度 VCVT.F32.S32 Sd, Sm
    pub(crate) fn emit_vcvt_f32_s32(&mut self, sd: u8, sm: u8) {
        let d_bit = ((sd & 1) as u32) << 22;
        let sd_hi = ((sd >> 1) & 0xF) as u32;
        let m_bit = ((sm & 1) as u32) << 5;
        let sm_hi = ((sm >> 1) & 0xF) as u32;
        // VCVT.F32.S32: cond 1111 1011 1 D 00 Vd 1010 11 M 0 Vm
        let inst = 0xEEB80AC0u32 | d_bit | (sd_hi << 12) | m_bit | sm_hi;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VCVT: 双精度转单精度 VCVT.F32.F64 Sd, Dm
    pub(crate) fn emit_vcvt_f32_f64(&mut self, sd: u8, dm: u8) {
        let d_bit = ((sd & 1) as u32) << 22;
        let sd_hi = ((sd >> 1) & 0xF) as u32;
        let m_bit = ((dm & 1) as u32) << 5;
        let dm_hi = ((dm >> 1) & 0xF) as u32;
        // VCVT.F32.F64: cond 1111 1011 1 D 00 Vd 1011 11 M 0 Vm
        let inst = 0xEEBC0BC0u32 | d_bit | (sd_hi << 12) | m_bit | dm_hi;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VCVT: 单精度转双精度 VCVT.F64.F32 Dd, Sm
    pub(crate) fn emit_vcvt_f64_f32(&mut self, dd: u8, sm: u8) {
        let d_bit = ((dd & 1) as u32) << 22;
        let dd_hi = ((dd >> 1) & 0xF) as u32;
        let m_bit = ((sm & 1) as u32) << 5;
        let sm_hi = ((sm >> 1) & 0xF) as u32;
        // VCVT.F64.F32: cond 1111 1011 1 D 00 Vd 1011 11 M 0 Vm
        let inst = 0xEEBB0AC0u32 | d_bit | (dd_hi << 12) | m_bit | sm_hi;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VNEG: 单精度取负 Sd = -Sm
    pub(crate) fn emit_vneg_s(&mut self, sd: u8, sm: u8) {
        let d_bit = ((sd & 1) as u32) << 22;
        let sd_hi = ((sd >> 1) & 0xF) as u32;
        let m_bit = ((sm & 1) as u32) << 5;
        let sm_hi = ((sm >> 1) & 0xF) as u32;
        // VNEG.S: cond 1111 1011 1 D 00 Vd 1010 01 M 0 Vm
        let inst = 0xEEB10A40u32 | d_bit | (sd_hi << 12) | m_bit | sm_hi;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// VNEG: 双精度取负 Dd = -Dm
    pub(crate) fn emit_vneg_d(&mut self, dd: u8, dm: u8) {
        let d_bit = ((dd & 1) as u32) << 22;
        let dd_hi = ((dd >> 1) & 0xF) as u32;
        let m_bit = ((dm & 1) as u32) << 5;
        let dm_hi = ((dm >> 1) & 0xF) as u32;
        // VNEG.D
        let inst = 0xEEB10B40u32 | d_bit | (dd_hi << 12) | m_bit | dm_hi;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// 加载单精度浮点字面量到 s0
    pub(crate) fn load_float_s0(&mut self, val: f32) {
        // 将浮点字面量写入 rodata 并加载到 s0
        let bits = val.to_bits() as u64;
        let _offset = if let Some(&off) = self.float_pool.get(&bits) {
            off
        } else {
            let off = self.rodata.len() as u64;
            self.rodata.extend_from_slice(&val.to_le_bytes());
            self.float_pool.insert(bits, off);
            off
        };
        // 使用 literal pool 加载: 先加载地址到 r0, 然后 VLDR
        // 简化: 使用 fp 相对偏移 (假设字面量在可访问范围内)
        // 实际实现: 将值存入栈临时区, 然后 VLDR
        let tmp_off = self.alloc_local("<flt_tmp>", 4);
        // 先将浮点值作为整数存入栈
        self.emit_load_imm(0, val.to_bits() as i32);
        self.emit_store_r0(tmp_off);
        // VLDR s0, [fp, #tmp_off]
        self.emit_vldr_s(0, tmp_off);
    }

    /// 加载双精度浮点字面量到 d0
    pub(crate) fn load_float_d0(&mut self, val: f64) {
        let bits = val.to_bits();
        let _offset = if let Some(&off) = self.float_pool.get(&bits) {
            off
        } else {
            let off = self.rodata.len() as u64;
            self.rodata.extend_from_slice(&val.to_le_bytes());
            self.float_pool.insert(bits, off);
            off
        };
        // 将双精度值存入栈临时区
        let tmp_off = self.alloc_local("<dbl_tmp>", 8);
        // 将 64 位值分成两个 32 位存入
        let lo = (val.to_bits() & 0xFFFFFFFF) as i32;
        let hi = ((val.to_bits() >> 32) & 0xFFFFFFFF) as i32;
        self.emit_load_imm(0, lo);
        self.emit_store_r0(tmp_off);
        self.emit_load_imm(0, hi);
        self.emit_store_r0(tmp_off + 4);
        // VLDR d0, [fp, #tmp_off]
        self.emit_vldr_d(0, tmp_off);
    }

    /// 单精度浮点二元运算: s0 = s2 op s0 (s2 = lhs, s0 = rhs)
    pub(crate) fn float_binop_s0(&mut self, op: BinOp, is_double: bool) {
        if is_double {
            match op {
                BinOp::Add => self.emit_vadd_d(0, 2, 0),
                BinOp::Sub => self.emit_vsub_d(0, 2, 0),
                BinOp::Mul => self.emit_vmul_d(0, 2, 0),
                BinOp::Div => self.emit_vdiv_d(0, 2, 0),
                _ => self.emit_vadd_d(0, 2, 0),
            }
        } else {
            match op {
                BinOp::Add => self.emit_vadd_s(0, 2, 0),
                BinOp::Sub => self.emit_vsub_s(0, 2, 0),
                BinOp::Mul => self.emit_vmul_s(0, 2, 0),
                BinOp::Div => self.emit_vdiv_s(0, 2, 0),
                _ => self.emit_vadd_s(0, 2, 0),
            }
        }
    }

    /// 单精度浮点比较: 设置 r0 = (s2 op s0) ? 1 : 0
    pub(crate) fn float_cmp_r0(&mut self, op: BinOp, is_double: bool) {
        // 比较 s2 和 s0
        if is_double {
            self.emit_vcmp_d(2, 0);
        } else {
            self.emit_vcmp_s(2, 0);
        }
        // VMRS APSR_nzcv, FPSCR
        self.emit_vmrs_apsr();
        // 根据条件设置 r0
        self.emit_mov_imm(0, 0);
        match op {
            BinOp::EqEq => self.emit_moveq(0, 1),
            BinOp::Ne => self.emit_movne(0, 1),
            BinOp::Lt => self.emit_movlt(0, 1),
            BinOp::Le => self.emit_movle(0, 1),
            BinOp::Gt => self.emit_movgt(0, 1),
            BinOp::Ge => self.emit_movge(0, 1),
            _ => self.emit_moveq(0, 1),
        }
    }

    /// 保存 s0 到 s2 (用于保存左操作数)
    pub(crate) fn save_s0_to_s2(&mut self, is_double: bool) {
        if is_double {
            // VMOV.F64 d2, d0
            self.emit_vadd_d(2, 0, 0); // d2 = d0 + 0 = d0
        } else {
            // VMOV.F32 s2, s0
            self.emit_vadd_s(2, 0, 0); // s2 = s0 + 0 = s0
        }
    }

    /// 从栈加载浮点到 s0 (用于加载局部变量)
    pub(crate) fn load_float_to_s0(&mut self, off: i32, is_double: bool) {
        if is_double {
            self.emit_vldr_d(0, off);
        } else {
            self.emit_vldr_s(0, off);
        }
    }

    /// 存储 s0 到栈 (用于存储浮点结果)
    pub(crate) fn store_s0_to_stack(&mut self, off: i32, is_double: bool) {
        if is_double {
            self.emit_vstr_d(0, off);
        } else {
            self.emit_vstr_s(0, off);
        }
    }
}
