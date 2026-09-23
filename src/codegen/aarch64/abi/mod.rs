// src/codegen/aarch64/abi/mod.rs
// AAPCS64 Calling Convention Module
// 
// Handles calling convention logic for AArch64 architecture following ARM's
// AAPCS64 (ARM Architecture Procedure Call Standard) specification.
//
// This module extracts calling convention-related functionality from the
// monolithic codegen implementation into a focused, testable module.

mod call;
mod classes;
mod frame;

pub use call::*;
pub use classes::*;
pub use frame::*;

/// AAPCS64 同构浮点聚合 (HFA) 描述
#[derive(Clone, Copy, Debug)]
pub struct HfaInfo {
    pub elem_bits: u16,     // 元素位数 (32= float, 64= double)
    pub count: u8,          // 元素数量 (1-8)
    pub total_size: usize,  // 总字节数
}

impl HfaInfo {
    /// 检查是否为有效 HFA
    pub fn is_valid(&self) -> bool {
        self.count >= 1 && self.count <= 8 && (self.elem_bits == 32 || self.elem_bits == 64)
    }

    /// 获取每个元素的字节数
    pub fn elem_bytes(&self) -> usize {
        (self.elem_bits / 8) as usize
    }

    /// 计算总共需要的槽位数 (每槽 8 字节)
    pub fn num_slots(&self) -> usize {
        self.count as usize * self.elem_bytes() / 8 + 
        if self.count as usize * self.elem_bytes() % 8 != 0 { 1 } else { 0 }
    }
}

/// 返回值分类结果 - 是否需要 SRET 隐藏指针
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnClass {
    /// 标量返回 (整型、指针、单精度/双精度浮点)
    /// 返回寄存器：X0-X1 (整数), V0-V1 (浮点)
    Scalar,
    
    /// 向量/HFA 返回
    /// 返回寄存器：V0-V7 (最多 8 个 SIMD 寄存器)
    VectorOrHfa { hfa_info: Option<HfaInfo> },
    
    /// 小结构体 (≤16 字节)
    /// 可以使用 X0-X1 和/或 V0-V1
    SmallAggregate,
    
    /// 大结构体 (>16 字节)
    /// 需要隐藏的 sret 指针作为第一个参数，在 X0 返回
    LargeAggregate,
}

impl ReturnClass {
    /// 检查是否需要 sret 隐藏指针
    pub fn requires_sret(&self) -> bool {
        matches!(self, ReturnClass::LargeAggregate)
    }

    /// 检查是否可以完全通过寄存器返回
    pub fn can_return_in_regs(&self) -> bool {
        !matches!(self, ReturnClass::LargeAggregate)
    }

    /// 检查是否包含浮点元素
    pub fn has_float(&self) -> bool {
        matches!(self, ReturnClass::VectorOrHfa { .. } | ReturnClass::Scalar)
    }
}

/// 参数分类结果 - 参数如何传递
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgClass {
    /// 整型/指针/枚举
    /// 传递寄存器：X0-X7
    Gp,
    
    /// 浮点/HFA 元素
    /// 传递寄存器：V0-V7
    Fp,
    
    /// 聚合类型 (整体放在栈上，传递地址)
    /// 用于 >16B 的非 HFA 类型
    Memory,
}

impl ArgClass {
    /// 检查是否是 GP 参数
    pub fn is_gp(&self) -> bool {
        matches!(self, ArgClass::Gp)
    }

    /// 检查是否是 FP 参数
    pub fn is_fp(&self) -> bool {
        matches!(self, ArgClass::Fp)
    }

    /// 检查是否是内存参数
    pub fn is_memory(&self) -> bool {
        matches!(self, ArgClass::Memory)
    }
}

/// AAPCS64 最大寄存器参数槽位
/// GP 寄存器：X0-X7 (8 个)
/// FP/SIMD 寄存器：V0-V7 (8 个)
pub const MAX_REG_ARG_SLOTS: usize = 16;

/// AAPCS64 变参函数寄存器保存区大小
/// GP 区域：48 字节 (前 6 个 GP 参数可能溢出到这里)
/// FP 区域：64 字节 (前 8 个 FP 参数可能溢出到这里)
pub const VA_SAVE_AREA_SIZE: usize = 128;

/// AAPCS64 16 字节对齐要求
pub const ALIGNMENT_16B: usize = 16;
