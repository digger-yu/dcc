// src/codegen/riscv64/helpers/encoding.rs - RISC-V instruction encoding utilities
// Priority 5 Task: Complete encoding utilities (~250 lines)
// Based on AArch64 pattern but for RISC-V fixed-width encoding

/// RISC-V 指令编码工具函数
/// 
/// 本模块提供 RISC-V 指令格式的各种编码辅助函数。
/// RISC-V 使用固定宽度的 32 位指令，主要分为以下几种格式：
/// - R-Type: 寄存器操作
/// - I-Type: 立即数操作 (load, register imm)
/// - S-Type: store
/// - B-Type: branch
/// - U-Type: upper immediate (auipc, lui)
/// - J-Type: jump

// ============================================================================
// 主要编码函数
// ============================================================================

/// 编码 R-type 指令 (例如 add, sub, and, or, xor)
/// 
/// Format: [opcode(7)][rd(5)][funct3(3)][rs1(5)][rs2(5)][funct7(7)]
/// Encoded as: opcode | rd << 8 | funct3 << 15 | rs1 << 20 | rs2 << 27 | funct7 << 25
#[inline]
pub fn encode_r_type(opcode: u7, rd: u5, rs1: u5, rs2: u5, funct3: u3, funct7: u7) -> u32 {
    ((funct7 as u32) << 25) |
    ((rs2 as u32) << 20) |
    ((rs1 as u32) << 15) |
    ((funct3 as u32) << 12) |
    ((rd as u32) << 7) |
    (opcode as u32)
}

/// 编码 I-type 指令 (例如 lw, addi, slti)
/// 
/// Format: [opcode(7)][rd(5)][funct3(3)][rs1(5)][imm12(12)]
/// Encoded as: opcode | rd << 8 | funct3 << 15 | rs1 << 20 | imm12[11:0]
#[inline]
pub fn encode_i_type(opcode: u7, rd: u5, rs1: u5, imm_imm12: i12, funct3: u3) -> u32 {
    ((imm_imm12 as u32) << 20) |
    ((rs1 as u32) << 15) |
    ((funct3 as u32) << 12) |
    ((rd as u32) << 7) |
    (opcode as u32)
}

/// 编码 S-type 指令 (例如 sw, sb, sh)
/// 
/// Format: [opcode(7)][rs1(5)][funct3(3)][rs2(5)][imm4(4)][imm12-5(8)]
/// Encoded as: opcode | rs2 << 8 | funct3 << 12 | rs1 << 15 | imm8[7:0] | imm11[4:0]
#[inline]
pub fn encode_s_type(rs1: u5, rs2: u5, imm_imm12: i12, funct3: u3) -> u32 {
    let imm_low = (imm_imm12 & 0x0F) as u32; // bits [4:0]
    let imm_high = ((imm_imm12 >> 4) & 0xFF) as u32; // bits [11:5]
    
    (imm_high << 25) |
    ((rs2 as u32) << 20) |
    ((rs1 as u32) << 15) |
    ((funct3 as u32) << 12) |
    (imm_low << 8) |
    (0b0100000u32) // opcode for store
}

/// 编码 B-type 指令 (例如 beq, bne, blt, bge)
/// 
/// Format: [opcode(7)][rs1(5)][rs2(5)][funct3(3)][imm4(4)][imm11-5(7)][imm13(1)][imm12(1)]
/// Encoded as: opcode | rs2 << 8 | rs1 << 15 | imm9[8:0] | funct3 << 12 | imm10[7:4] | imm11[3:0]
#[inline]
pub fn encode_b_type(rs1: u5, rs2: u5, imm_imm13: i13, funct3: u3) -> u32 {
    let imm_sign = ((imm_imm13 >> 12) & 1) as u32; // bit [12]
    let imm_11_5 = ((imm_imm13 >> 5) & 0x7F) as u32; // bits [11:5]
    let imm_4_1 = (imm_imm13 & 0x0F) as u32; // bits [4:1]
    let imm_0 = ((imm_imm13 >> 12) & 1) as u32; // bit [0], same as sign
    
    (imm_sign << 31) |
    (imm_11_5 << 25) |
    ((rs2 as u32) << 20) |
    ((rs1 as u32) << 15) |
    ((funct3 as u32) << 12) |
    (imm_4_1 << 8) |
    (imm_0 << 7) |
    (0b1100011u32) // opcode for branch
}

/// 编码 U-type 指令 (例如 auipc, lui)
/// 
/// Format: [opcode(7)][rd(5)][imm20(20)]
/// Encoded as: opcode | imm19[19:12] | imm[11:5] | rd << 7
#[inline]
pub fn encode_u_type(opcode: u7, rd: u5, imm_imm20: u20) -> u32 {
    ((imm_imm20 as u32) << 12) |
    ((rd as u32) << 7) |
    (opcode as u32)
}

/// 编码 J-type 指令 (例如 jal)
/// 
/// Format: [opcode(7)][rd(5)][imm19(1)][imm10-1(10)][imm11(1)][imm12-2(8)]
/// Encoded as: opcode | imm11[11:8] | imm10-1[19:10] | imm12-2[20:13] | imm19[1] | rd << 7
#[inline]
pub fn encode_j_type(opcode: u7, rd: u5, imm_imm20: i20) -> u32 {
    let imm_sign = ((imm_imm20 >> 19) & 1) as u32; // bit [19]
    let imm_11 = ((imm_imm20 >> 11) & 0x01) as u32; // bit [11]
    let imm_10_1 = ((imm_imm20 >> 1) & 0x03FF) as u32; // bits [10:1]
    let imm_12 = ((imm_imm20 >> 12) & 0x0FF) as u32; // bit [12]
    
    (imm_sign << 31) |
    (imm_12 << 30) |
    (imm_11 << 29) |
    (imm_10_1 << 19) |
    ((imm_imm20 >> 20) as u32) | // bit [20] at position 20
    ((rd as u32) << 7) |
    (opcode as u32)
}

// ============================================================================
// 特定指令的便捷编码器
// ============================================================================

/// AUIPC 指令编码
#[inline]
pub fn encode_auipc(rd: u5, imm_imm20: u20) -> u32 {
    encode_u_type(0b0010111, rd, imm_imm20)
}

/// LUI 指令编码
#[inline]
pub fn encode_lui(rd: u5, imm_imm20: u20) -> u32 {
    encode_u_type(0b0110111, rd, imm_imm20)
}

/// jal (jump and link) 指令编码
#[inline]
pub fn encode_jal(rd: u5, imm_imm20: i20) -> u32 {
    encode_j_type(0b1101111, rd, imm_imm20)
}

/// jalr (jump indirect register) 指令编码
#[inline]
pub fn encode_jalr(rd: u5, rs1: u5, imm_imm12: i12) -> u32 {
    encode_i_type(0b1100111, rd, rs1, imm_imm12, 0b000)
}

/// load 指令编码族
#[inline]
pub fn encode_load(op: u7, rd: u5, rs1: u5, imm_imm12: i12, funct3: u3) -> u32 {
    encode_i_type(op, rd, rs1, imm_imm12, funct3)
}

/// store 指令编码族
#[inline]
pub fn encode_store(op: u7, rs1: u5, rs2: u5, imm_imm12: i12, funct3: u3) -> u32 {
    encode_s_type(rs1, rs2, imm_imm12, funct3)
}

// ============================================================================
// 寄存器编码工具
// ============================================================================

/// GPR 寄存器编码
#[inline]
pub fn reg_gpr(gpr: u8) -> u8 {
    gpr
}

/// FPR 寄存器编码
#[inline]
pub fn reg_fpr(fpr: u8) -> u8 {
    fpr
}

/// VPR 寄存器编码 (向量寄存器)
#[inline]
pub fn reg_vpr(vpr: u8) -> u8 {
    vpr
}

/// 获取零寄存器编号
#[inline]
pub fn reg_zero() -> u8 {
    0
}

/// 获取 ra (return address) 寄存器编号
#[inline]
pub fn reg_ra() -> u8 {
    1
}

/// 获取 sp (stack pointer) 寄存器编号
#[inline]
pub fn reg_sp() -> u8 {
    2
}

/// 获取 gp (global pointer) 寄存器编号
#[inline]
pub fn reg_gp() -> u8 {
    3
}

/// 获取 tp (thread pointer) 寄存器编号
#[inline]
pub fn reg_tp() -> u8 {
    4
}

// ============================================================================
// 立即数处理工具
// ============================================================================

/// 符号扩展
#[inline]
pub fn sign_extend(value: u32, bits: u32) -> i32 {
    if bits == 0 {
        return 0;
    }
    let shift = 32 - bits;
    (value << shift) as i32 >> shift
}

/// 零扩展
#[inline]
pub fn zero_extend(value: u32, bits: u32) -> u32 {
    if bits >= 32 {
        value
    } else {
        value & ((1 << bits) - 1)
    }
}

/// 右旋转
#[inline]
pub fn rotate_right(value: u32, count: u32) -> u32 {
    let count = count % 32;
    (value >> count) | (value << (32 - count))
}

/// 左旋转
#[inline]
pub fn rotate_left(value: u32, count: u32) -> u32 {
    let count = count % 32;
    (value << count) | (value >> (32 - count))
}

/// 提取指定位域
#[inline]
pub fn extract_bits(value: u32, low: u32, high: u32) -> u32 {
    assert!(high >= low && high < 32);
    (value >> low) & ((1 << (high - low + 1)) - 1)
}

/// 插入位域
#[inline]
pub fn insert_bits(value: u32, field: u32, low: u32, high: u32) -> u32 {
    assert!(high >= low && high < 32);
    let mask = !(((1 << (high - low + 1)) - 1) << low);
    (value & mask) | ((field << low) & !mask)
}

// ============================================================================
// 指令分解工具
// ============================================================================

/// 从机器码中提取 R-type 字段
#[inline]
pub fn decode_r_type(inst: u32) -> (u7, u5, u3, u5, u5, u7) {
    (
        (inst & 0x7F) as u7,                                    // opcode
        ((inst >> 7) & 0x1F) as u5,                             // rd
        ((inst >> 12) & 0x07) as u3,                            // funct3
        ((inst >> 15) & 0x1F) as u5,                            // rs1
        ((inst >> 20) & 0x1F) as u5,                            // rs2
        ((inst >> 25) & 0x7F) as u7,                            // funct7
    )
}

/// 从机器码中提取 I-type 字段
#[inline]
pub fn decode_i_type(inst: u32) -> (u7, u5, u3, u5, i12) {
    (
        (inst & 0x7F) as u7,                                    // opcode
        ((inst >> 7) & 0x1F) as u5,                             // rd
        ((inst >> 12) & 0x07) as u3,                            // funct3
        ((inst >> 15) & 0x1F) as u5,                            // rs1
        (((inst >> 20) as i32) >> 20) as i12,                   // imm12 (sign extend)
    )
}

/// 从机器码中提取 S-type 字段
#[inline]
pub fn decode_s_type(inst: u32) -> (u7, u5, u3, u5, i12) {
    let imm_low = ((inst >> 8) & 0x0F) as i12;
    let imm_high = (((inst >> 25) as i32) >> 28) as i12; // sign extend
    let imm = (imm_high << 4) | imm_low;
    
    (
        (inst & 0x7F) as u7,                                    // opcode
        ((inst >> 15) & 0x1F) as u5,                            // rs1
        ((inst >> 12) & 0x07) as u3,                            // funct3
        ((inst >> 20) & 0x1F) as u5,                            // rs2
        imm,
    )
}

/// 从机器码中提取 B-type 字段
#[inline]
pub fn decode_b_type(inst: u32) -> (u7, u5, u3, u5, i13) {
    let imm_sign = ((inst >> 31) & 1) as i13;
    let imm_11_5 = ((inst >> 25) & 0x7F) as i13;
    let imm_4_1 = ((inst >> 8) & 0x0F) as i13;
    let imm_0 = ((inst >> 7) & 1) as i13;
    
    let imm = (imm_sign << 12) | (imm_11_5 << 5) | (imm_4_1 << 1) | imm_0;
    
    (
        (inst & 0x7F) as u7,                                    // opcode
        ((inst >> 15) & 0x1F) as u5,                            // rs1
        ((inst >> 12) & 0x07) as u3,                            // funct3
        ((inst >> 20) & 0x1F) as u5,                            // rs2
        imm,
    )
}

/// 从机器码中提取 U-type 字段
#[inline]
pub fn decode_u_type(inst: u32) -> (u7, u5, u20) {
    (
        (inst & 0x7F) as u7,                                    // opcode
        ((inst >> 7) & 0x1F) as u5,                             // rd
        ((inst >> 12) & 0xFFFFF) as u20,                        // imm20
    )
}

/// 从机器码中提取 J-type 字段
#[inline]
pub fn decode_j_type(inst: u32) -> (u7, u5, i20) {
    let imm_sign = ((inst >> 31) & 1) as i20;
    let imm_11 = ((inst >> 29) & 1) as i20;
    let imm_10_1 = ((inst >> 19) & 0x3FF) as i20;
    let imm_12_5 = ((inst >> 20) & 0xFF) as i20;
    
    let imm = (imm_sign << 19) | (imm_12_5 << 12) | (imm_11 << 11) | imm_10_1;
    
    (
        (inst & 0x7F) as u7,                                    // opcode
        ((inst >> 7) & 0x1F) as u5,                             // rd
        imm,
    )
}

// ============================================================================
// 类型定义 (简化为 u8/u32 等)
// ============================================================================

type u7 = u8;   // 7-bit fields
type u5 = u8;   // 5-bit fields  
type u3 = u8;   // 3-bit fields
type i12 = i16; // 12-bit signed
type i13 = i16; // 13-bit signed
type i20 = i32; // 20-bit signed
type u20 = u32; // 20-bit unsigned

// ============================================================================
// 测试用例
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_r_type() {
        let opcode = 0b0000000; // add
        let rd = 1;
        let rs1 = 2;
        let rs2 = 3;
        let funct3 = 0b000;
        let funct7 = 0b0000000;
        
        let encoded = encode_r_type(opcode, rd, rs1, rs2, funct3, funct7);
        let (dec_opcode, dec_rd, dec_funct3, dec_rs1, dec_rs2, dec_funct7) = decode_r_type(encoded);
        
        assert_eq!(opcode, dec_opcode);
        assert_eq!(rd, dec_rd);
        assert_eq!(funct3, dec_funct3);
        assert_eq!(rs1, dec_rs1);
        assert_eq!(rs2, dec_rs2);
        assert_eq!(funct7, dec_funct7);
    }

    #[test]
    fn test_encode_decode_i_type() {
        let opcode = 0b0000011; // ld
        let rd = 1;
        let rs1 = 2;
        let imm = 4;
        let funct3 = 0b010;
        
        let encoded = encode_i_type(opcode, rd, rs1, imm, funct3);
        let (dec_opcode, dec_rd, dec_funct3, dec_rs1, dec_imm) = decode_i_type(encoded);
        
        assert_eq!(opcode, dec_opcode);
        assert_eq!(rd, dec_rd);
        assert_eq!(funct3, dec_funct3);
        assert_eq!(rs1, dec_rs1);
        assert_eq!(imm, dec_imm);
    }

    #[test]
    fn test_sign_extend() {
        assert_eq!(sign_extend(0x00000001, 8), 1);
        assert_eq!(sign_extend(0x000000FF, 8), -1);
        assert_eq!(sign_extend(0x00000080, 8), -128);
        assert_eq!(sign_extend(0x00007FFF, 16), 32767);
        assert_eq!(sign_extend(0xFFFF8000, 16), -32768);
    }

    #[test]
    fn test_zero_extend() {
        assert_eq!(zero_extend(0xFFFFFFFF, 8), 0x000000FF);
        assert_eq!(zero_extend(0xFFFF0000, 16), 0x0000FFFF);
        assert_eq!(zero_extend(0x12345678, 32), 0x12345678);
    }
}
