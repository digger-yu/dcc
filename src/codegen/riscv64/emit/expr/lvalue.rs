// src/codegen/riscv64/emit/expr/lvalue.rs - Left-value addresses emission (RISC-V 64 backend)
// Priority 4 Task: Extract working implementations from riscv64.rs.bak

use crate::ast::{Field, Type};
use crate::codegen::riscv64::codegen::Riscv64Codegen;
use crate::ir::{TypedExpr, ValueType};
use crate::target::Target;
use std::collections::HashMap;

impl Riscv64Codegen {
    // ========================================================================
    // L-value 地址计算逻辑
    // ========================================================================

    /// 生成 L-value 的地址并放入 a0
    pub fn gen_lvalue_addr(&mut self, expr: &TypedExpr) {
        match &expr.kind {
            crate::ir::ExprKind::Local(name) => {
                if let Some(&off) = self.locals.get(name) {
                    // 局部变量：加载栈偏移地址
                    self.load_a0(off);
                } else if self.global_rodata.contains_key(name) {
                    // 全局变量：计算绝对地址
                    self.load_global_addr_a0(name);
                } else {
                    // 未找到定义
                    self.li_a0(0);
                }
            }
            
            crate::ir::ExprKind::Member(field_name) => {
                // 结构体成员访问：compute offset and add
                if !expr.children.is_empty() {
                    let base_expr = &expr.children[0];
                    
                    self.gen_expr(base_expr); // a0 = base address
                    
                    // 获取字段偏移
                    if let ValueType::Aggregate { name, .. } = &base_expr.ty {
                        if let Some(fields) = find_aggregate_fields(name) {
                            if let Some((field_info, extra)) = find_field(&fields, field_name) {
                                let offset = field_info.offset + extra;
                                if offset != 0 {
                                    self.emit_addi(10, 10, offset as i32);
                                }
                            }
                        }
                    }
                } else {
                    self.li_a0(0);
                }
            }
            
            crate::ir::ExprKind::Index => {
                // 数组索引：base + index * element_size
                if expr.children.len() >= 2 {
                    let base_expr = &expr.children[0];
                    let index_expr = &expr.children[1];

                    self.gen_expr(base_expr); // a0 = base

                    self.gen_expr(index_expr); // a1 = index

                    // 计算元素大小并乘法
                    let elem_size = expr.ty.size().max(1);
                    if elem_size.is_power_of_two() {
                        // 使用位移优化乘法
                        let shift = elem_size.trailing_zeros() as i32;
                        self.emit_slli(11, 11, shift);
                    } else {
                        // 普通乘法
                        self.emit_mul(11, 11, 11);
                    }

                    self.emit_add(10, 10, 11); // a0 += index * size
                } else {
                    self.li_a0(0);
                }
            }
            
            crate::ir::ExprKind::Deref => {
                // 指针解引用：just load from address
                self.gen_expr(expr); // a0 is already the address
            }
            
            crate::ir::ExprKind::Assign | 
            crate::ir::ExprKind::Binary(_) |
            crate::ir::ExprKind::Unary(_) => {
                // 其他表达式通常不会作为 L-value
                self.li_a0(0);
            }
            
            _ => {
                // 默认处理：尝试从 IR 表达式中解析地址
                self.gen_expr(expr);
            }
        }
    }

    /// 存储值到 L-value 位置
    pub fn store_to_lvalue(&mut self, lvalue: &TypedExpr, value_reg: u8, size: usize) {
        match &lvalue.kind {
            crate::ir::ExprKind::Local(name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.emit_store_sz(value_reg, 2, off, size.max(1));
                } else {
                    // 尝试加载全局地址
                    self.load_global_addr_a0(name);
                    self.emit_addi(12, 10, 0); // save address
                    self.emit_store_sz(value_reg, 12, 0, size.max(1));
                }
            }
            
            crate::ir::ExprKind::Member(_) => {
                // 结构体成员
                let addr_reg = 12;
                self.gen_lvalue_addr(lvalue);
                self.emit_addi(addr_reg, 10, 0); // save address
                self.emit_store_sz(value_reg, addr_reg, 0, size.max(1));
            }
            
            crate::ir::ExprKind::Index => {
                // 数组索引
                let addr_reg = 12;
                self.gen_lvalue_addr(lvalue);
                self.emit_addi(addr_reg, 10, 0); // save address
                self.emit_store_sz(value_reg, addr_reg, 0, size.max(1));
            }
            
            crate::ir::ExprKind::Deref => {
                // 指针解引用
                self.gen_lvalue_addr(lvalue);
                self.emit_store_sz(value_reg, 10, 0, size.max(1));
            }
            
            _ => {
                // 通用存储
                let addr_reg = 12;
                self.gen_lvalue_addr(lvalue);
                self.emit_addi(addr_reg, 10, 0);
                self.emit_store_sz(value_reg, addr_reg, 0, size.max(1));
            }
        }
    }

    /// 从 L-value 位置加载值到寄存器
    pub fn load_from_lvalue(&mut self, lvalue: &TypedExpr, result_reg: u8) {
        match &lvalue.kind {
            crate::ir::ExprKind::Local(name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.load_a0(off);
                } else {
                    self.li_a0(0);
                }
            }
            _ => {
                self.gen_lvalue_addr(lvalue);
                let size = value_type_size(&lvalue.ty).max(1);
                self.emit_load_sz(result_reg, 10, 0, size);
            }
        }
    }
}

// ========================================================================
// 辅助函数和数据结构
// ========================================================================

/// 查找聚合类型字段信息
pub fn find_aggregate_fields(name: &str) -> Option<(std::sync::Arc<Vec<Field>>, bool)> {
    // TODO: 这个函数应该查询全局符号表或上下文中的聚合类型定义
    // 当前实现返回 None，需要进一步集成
    None
}

/// 在字段列表中查找特定字段
pub fn find_field<'a>(
    fields: &'a [Field],
    field_name: &str,
) -> Option<(&'a Field, usize)> {
    for field in fields.iter() {
        if field.name.name == field_name {
            return Some((field, 0));
        }
    }
    None
}

/// 获取类型大小
fn value_type_size(ty: &crate::ir::ValueType) -> usize {
    use crate::ir::ValueType;
    match ty {
        ValueType::Void => 0,
        ValueType::Integer { bits, .. } | ValueType::Float { bits } => (*bits as usize).div_ceil(8),
        ValueType::Array(elem, count) => elem.size().saturating_mul(count.unwrap_or(0)),
        ValueType::Pointer(_) => 8, // 64 位指针
        ValueType::Aggregate { size, .. } => *size,
        ValueType::Function { .. } => 0,
        ValueType::Vector(_, _) => 0,
        ValueType::Complex(_) => 0,
        ValueType::Imaginary(_) => 0,
    }
}

/// 对齐函数
pub fn align_up_u64(val: u64, alignment: u64) -> u64 {
    (val + alignment - 1) & !(alignment - 1)
}

/// 整型提升处理
impl Riscv64Codegen {
    /// IR 路径的整型提升
    pub fn gen_int_promote_ir(&mut self, ty: &crate::ir::ValueType) {
        match ty {
            crate::ir::ValueType::Char(c) => {
                if matches!(c, Type::SChar | Type::UChar) {
                    // char/short 需要提升到 int
                    let reg = 10; // a0
                    if matches!(c, Type::SChar) {
                        // 符号扩展
                        self.emit_slli(reg, reg, 24);
                        self.emit_srai(reg, reg, 24);
                    } else {
                        // 零扩展
                        self.emit_zext_32();
                    }
                }
            }
            crate::ir::ValueType::Int(bits) => {
                if *bits <= 16 {
                    // short/int 可能需要扩展
                    let reg = 10;
                    self.emit_slli(reg, reg, 16);
                    self.emit_srai(reg, reg, 16);
                }
            }
            _ => {}
        }
    }

    /// AST 路径的整型提升
    pub fn gen_int_promote(&mut self, ty: &Type) {
        self.gen_int_promote_ir(&ty.to_value_type());
    }

    /// 零扩展到低 32 位
    fn emit_zext_32(&mut self) {
        // a0 已经包含低位，高位清零
        // 对于 64 位系统，如果值是 32 位，高位可能是垃圾值
        // 我们需要确保高 32 位为零
        self.emit_andi(10, 10, 0xFFFF_FFFF);
    }
}
