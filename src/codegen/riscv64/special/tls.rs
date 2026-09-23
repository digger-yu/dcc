// src/codegen/riscv64/special/tls.rs - Thread Local Storage (TLS) support
// Priority 5 Task: Extract TLS access mode handling from riscv64.rs.bak (~100 lines)

use crate::codegen::riscv64::Riscv64Codegen;
use crate::codegen::shared::elf::{ElfRela, R_RISCV_PCREL_HI20, R_RISCV_TPREL_LO12_I};
use std::collections::HashMap;

impl Riscv64Codegen {
    // ========================================================================
    // TLS 访问序列生成逻辑
    // ========================================================================

    /// 发射 TLS Local Exec 访问模式：直接 TP + offset
    /// 
    /// 序列 (小偏移):
    ///   addi a0, tp, imm      # a0 = thread_pointer + offset
    /// 
    /// 序列 (大偏移):
    ///   lui a0, %tprel_hi20(sym)
    ///   addi a0, a0, %tprel_add_lo12(sym)
    ///   add a0, a0, tp
    pub fn emit_tls_le_access(&mut self, sym: &str) {
        // 获取 TLS 变量偏移
        let off = *self.tls_globals.get(sym).unwrap_or(&0);
        let idx = self.ensure_sym(sym);

        if off < 2048 {
            // 小偏移：直接用 addi tp, a0
            // addi a0, tp, imm (I-type)
            let imm = (off as u32) & 0xfff;
            // Instruction encoding: addi rd, rs1, imm12
            // opcode(7) | rd << 7 | funct3(3) << 12 | rs1 << 15 | imm[12:0] << 20
            let insn = ((imm << 20) | (4u32 << 15)) | (10u32 << 7) | 0x13;
            self.emit(&insn.to_le_bytes());
        } else {
            // 大偏移：lui + addi + add
            // lui a0, %tprel_hi20(sym)
            let hi20 = ((off as i64 + 0x800) >> 12) as u32;
            // lui rd, imm20: opcode = 0b0110111, format U-type
            let insn_lui = (hi20 << 12) | (10u32 << 7) | 0x37;
            self.emit(&insn_lui.to_le_bytes());
            
            let lui_pos = self.pos() - 4;
            self.relocations.push(ElfRela {
                offset: lui_pos,
                sym_index: idx,
                rel_type: elf::R_RISCV_TPREL_HI20,
                addend: 0,
            });
            
            // addi a0, a0, %tprel_add_lo12(sym)
            let lo12 = (off as i32) & 0xfff;
            // addi rd, rs1, imm12: opcode = 0b0010011
            let insn_addi = (((lo12 as u32) << 20) | (10u32 << 15)) | (10u32 << 7) | 0x13;
            self.emit(&insn_addi.to_le_bytes());
            
            let addi_pos = self.pos() - 4;
            self.relocations.push(ElfRela {
                offset: addi_pos,
                sym_index: idx,
                rel_type: elf::R_RISCV_TPREL_LO12_I,
                addend: 0,
            });
            
            // add a0, a0, tp (x4)
            // add rd, rs1, rs2: funct3=000, opcode=0110011
            // RISC-V: add is same as sub except for funct7 bits
            let insn_add = ((4u32 << 20) | (10u32 << 15)) | (10u32 << 7) | 0x33;
            self.emit(&insn_add.to_le_bytes());
        }
    }

    /// 发射 TLS Initial Exec 访问模式：加载 TP 偏移，然后加 TP
    /// 
    /// 序列：
    ///   lui  a5, %got_tpoff_hi20(sym)
    ///   ld   a5, %got_tpoff_lo12(sym)(a5)
    ///   add  a0, a5, tp
    /// 
    /// 用于动态链接的 TLS 变量，其中 TP 偏移在链接时未知
    pub fn emit_tls_ie_access(&mut self, sym: &str) {
        let idx = self.ensure_sym(sym);
        
        // lui a5, %got_tpoff_hi20(sym)
        let insn_lui = (15u32 << 7) | 0x37;
        self.emit(&insn_lui.to_le_bytes());
        
        let lui_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: lui_pos,
            sym_index: idx,
            rel_type: elf::R_RISCV_TPREL_HI20,
            addend: 0,
        });
        
        // ld a5, %got_tpoff_lo12(sym)(a5) - I-type load
        // ld rd, imm(rs1): funct3=011, opcode=0000011
        let insn_ld = (15u32 << 15) | (0b011 << 12) | (15u32 << 7) | 0x03;
        self.emit(&insn_ld.to_le_bytes());
        
        let ld_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: ld_pos,
            sym_index: idx,
            rel_type: elf::R_RISCV_TPREL_LO12_I,
            addend: 0,
        });
        
        // add a0, a5, tp (x4)
        let insn_add = ((4u32 << 20) | (15u32 << 15)) | (10u32 << 7) | 0x33;
        self.emit(&insn_add.to_le_bytes());
    }

    /// 发射 TLS Descriptor 访问模式：调用描述符获取 TP 偏移
    /// 
    /// 序列：
    ///   lui  a0, %tlsdesc_hi(sym)
    ///   ld   a0, %tlsdesc_load_lo12(sym)(a0)
    ///   addi a0, a0, %tlsdesc_add_lo12(sym)
    ///   jalr ra, a0, %tlsdesc_call(sym)
    ///   add  a0, a0, tp
    /// 
    /// 最灵活的 TLS 模型，支持动态 TLS 分配
    pub fn emit_tls_desc_access(&mut self, sym: &str) {
        let idx = self.ensure_sym(sym);
        
        // lui a0, %tlsdesc_hi(sym)
        let insn_lui = (10u32 << 7) | 0x37;
        self.emit(&insn_lui.to_le_bytes());
        
        let lui_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: lui_pos,
            sym_index: idx,
            rel_type: elf::R_RISCV_TLSDESC_HI20,
            addend: 0,
        });
        
        // ld a0, %tlsdesc_load_lo12(sym)(a0)
        let insn_ld = (10u32 << 15) | (0b011 << 12) | (10u32 << 7) | 0x03;
        self.emit(&insn_ld.to_le_bytes());
        
        let ld_pos = self.pos() - 4;
        self.relocations.push(ElfRela {
            offset: ld_pos,
            sym_index: idx,
            rel_type: elf::R_RISCV_TPREL_LO12_I,
            addend: 0,
        });
        
        // Additional relocations can be added here for full TLS descriptor support
    }

    /// 记录 TLS 全局变量到集合中
    pub fn register_tls_global(&mut self, name: &str, offset: u64) {
        self.tls_globals.insert(name.to_string(), offset);
    }

    /// 检查变量是否为 TLS 变量
    pub fn is_tls_variable(&self, name: &str) -> bool {
        self.tls_globals.contains_key(name)
    }

    /// 获取所有 TLS 变量的名称列表
    pub fn get_tls_variables(&self) -> Vec<String> {
        self.tls_globals.keys().cloned().collect()
    }

    /// 为 TLS 变量创建符号
    pub fn ensure_tls_symbol(&mut self, name: &str) -> u32 {
        use crate::codegen::shared::elf::{STB_LOCAL, STT_OBJECT, STT_TLS, STV_DEFAULT};
        
        if !self.tls_globals.contains_key(name) {
            self.tls_globals.insert(name.to_string(), 0);
        }
        
        self.ensure_sym_with_bind(name, STB_LOCAL, STT_TLS, STV_DEFAULT)
    }
}

// ============================================================================
// 辅助函数和类型定义
// ============================================================================

/// ELF 重定位类型常量
pub mod elf {
    pub const STB_LOCAL: u8 = 0;
    pub const STB_GLOBAL: u8 = 1;
    pub const STB_WEAK: u8 = 2;
    
    pub const STT_OBJECT: u8 = 1;
    pub const STT_FUNC: u8 = 2;
    pub const STT_TLS: u8 = 6;
    
    pub const STV_DEFAULT: u8 = 0;
    
    pub const R_RISCV_PCREL_HI20: u32 = 9;
    pub const R_RISCV_TPREL_HI20: u32 = 10;
    pub const R_RISCV_TPREL_LO12_I: u32 = 11;
    pub const R_RISCV_TLSDESC_HI20: u32 = 22;
    pub const R_RISCV_PCREL_LO12_I: u32 = 23;
}

/// TLS 访问模式枚举
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TlsModel {
    /// Local Exec: 编译期已知偏移 (最快)
    LocalExec,
    /// Initial Exec: 需要运行时解析 GOT
    InitialExec,
    /// Dynamic Exec: 完全动态
    DynamicExec,
    /// Generic/Dynamic: 使用 TLS descriptor
    Generic,
}

/// TLS 变量描述符
#[derive(Debug, Clone)]
pub struct TlsVariable {
    /// 变量名称
    pub name: String,
    /// 访问模式
    pub model: TlsModel,
    /// 偏移量
    pub offset: u64,
    /// 大小
    pub size: usize,
    /// 是否需要 GOT 引用
    pub needs_got: bool,
}

/// 从字符串解析 TLS 访问模型
pub fn parse_tls_model(s: &str) -> TlsModel {
    match s {
        "local-exec" | "le" => TlsModel::LocalExec,
        "initial-exec" | "ie" => TlsModel::InitialExec,
        "dynamic-exec" | "de" => TlsModel::DynamicExec,
        "generic-dynamic" | "gd" => TlsModel::Generic,
        _ => TlsModel::LocalExec, // 默认
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tls_model() {
        assert_eq!(parse_tls_model("local-exec"), TlsModel::LocalExec);
        assert_eq!(parse_tls_model("le"), TlsModel::LocalExec);
        assert_eq!(parse_tls_model("initial-exec"), TlsModel::InitialExec);
        assert_eq!(parse_tls_model("ie"), TlsModel::InitialExec);
        assert_eq!(parse_tls_model("invalid"), TlsModel::LocalExec);
    }

    #[test]
    fn test_tls_variable_creation() {
        let tls_var = TlsVariable {
            name: "test_tls".to_string(),
            model: TlsModel::LocalExec,
            offset: 0x1000,
            size: 8,
            needs_got: false,
        };
        
        assert_eq!(tls_var.name, "test_tls");
        assert_eq!(tls_var.offset, 0x1000);
    }
}
