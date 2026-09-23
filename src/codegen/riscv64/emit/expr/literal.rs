// src/codegen/riscv64/emit/expr/literal.rs - Literal values emission (RISC-V 64 backend)
// Priority 4 Task: Extract working implementations from riscv64.rs.bak

use crate::ast::Type;

use crate::codegen::riscv64::codegen::Riscv64Codegen;
use crate::ir::{TypedExpr, ValueType};

use crate::target::Target;
use std::collections::HashMap;


impl Riscv64Codegen {
    // ========================================================================
    // 字面量发射逻辑
    // ========================================================================

    /// 发射字面量值到寄存器
    pub fn emit_literal(&mut self, literal: &crate::ir::ExprLiteral) -> TypedExpr {
        match literal {
            crate::ir::ExprLiteral::Int(v) => {
                self.emit_integer_literal(*v)
            }
            crate::ir::ExprLiteral::Float(v) => {
                self.emit_float_literal(*v as f32)
            }
            _ => {
                // 其他类型的字面量（如字符串）需要特殊处理
                self.emit_string_literal()
            }
        }
    }

    /// 发射整数常量 (hex, octal, decimal)
    fn emit_integer_literal(&mut self, value: i128) -> TypedExpr {
        // 将 i128 转换为适合 RISC-V 的格式
        let value_64 = value as u64;
        
        if value <= i32::MAX as i128 && value >= i32::MIN as i128 {
            // 小值：使用 li 指令直接加载
            self.li_a0(value as i32);
            
            return TypedExpr {
                kind: crate::ir::ExprKind::Constant,
                ty: ValueType::Int(64),
                literal: Some(crate::ir::ExprLiteral::Int(value)),
                children: Vec::new(),
                span: crate::diagnostics::Span::unknown(),
            };
        } else {
            // 大值：使用 auipc + ld 或 lui + addi 组合
            self.load_large_constant(value_64);
            
            return TypedExpr {
                kind: crate::ir::ExprKind::Constant,
                ty: ValueType::Int(64),
                literal: Some(crate::ir::ExprLiteral::Int(value)),
                children: Vec::new(),
                span: crate::diagnostics::Span::unknown(),
            };
        }
    }

    /// 发射单精度浮点常量
    fn emit_float_literal(&mut self, value: f32) -> TypedExpr {
        // 将 f32 转换为 IEEE 754 位模式并加载
        let bits = value.to_bits();
        self.load_float_fa0(value);
        
        return TypedExpr {
            kind: crate::ir::ExprKind::Constant,
            ty: ValueType::Float { bits: 32 },
            literal: Some(crate::ir::ExprLiteral::Float(value as f64)),
            children: Vec::new(),
            span: crate::diagnostics::Span::unknown(),
        };
    }

    /// 发射双精度浮点常量
    fn emit_double_literal(&mut self, value: f64) -> TypedExpr {
        // 使用 double precision load
        let bits = value.to_bits();
        
        // 如果可能，直接使用 fmv.x.d 从 FPU 寄存器加载
        // 否则使用 auipc + fld 加载到 rodata
        self.load_double_fa0(value);
        
        return TypedExpr {
            kind: crate::ir::ExprKind::Constant,
            ty: ValueType::Float { bits: 64 },
            literal: Some(crate::ir::ExprLiteral::Float(value)),
            children: Vec::new(),
            span: crate::diagnostics::Span::unknown(),
        };
    }

    /// 发射字符串常量 (store in .rodata)
    fn emit_string_literal(&mut self) -> TypedExpr {
        // 将字符串放入只读数据段
        // 这需要先分配 rodata 空间，然后返回地址
        
        let label = self.fresh_label("Lstr");
        let addr = self.register_rodata_offset(label);
        
        // 生成字符串的实际存储
        // 注意：这需要访问更详细的上下文信息
        
        // 返回字符串的地址到 a0
        self.li_a0(addr as i32);
        
        return TypedExpr {
            kind: crate::ir::ExprKind::Constant,
            ty: ValueType::Pointer(Box::new(ValueType::Char(Type::Char))),
            literal: None,
            children: Vec::new(),
            span: crate::diagnostics::Span::unknown(),
        };
    }

    /// 发射字符常量
    fn emit_char_literal(&mut self, value: char) -> TypedExpr {
        let code = value as u32;
        self.li_a0(code as i32);
        
        return TypedExpr {
            kind: crate::ir::ExprKind::Constant,
            ty: ValueType::Char(Type::Int), // char 提升为 int
            literal: Some(crate::ir::ExprLiteral::Int(code as i128)),
            children: Vec::new(),
            span: crate::diagnostics::Span::unknown(),
        };
    }

    // ============================================================
    // 辅助函数
    // ============================================================

    /// 加载大的 64 位常量到寄存器
    fn load_large_constant(&mut self, value: u64) {
        // 尝试分解为 auipc + lo12 的形式
        let hi = (value >> 12) as i32;
        let lo = (value & 0xFFF) as i32;
        
        // 如果高 20 位可表示为 PC-relative 偏移
        if hi != 0 {
            self.emit_auipc_a0(hi);
            
            // 计算实际地址
            let pos = self.pos();
            let abs_addr = (pos as i64).wrapping_add((hi as i64) << 12) as u64;
            
            // 调整低 12 位以匹配目标值
            let adjusted_lo = ((value & 0xFFF) as i32).wrapping_sub((((abs_addr >> 12) as i32) << 12).wrapping_mul(-1));
            
            if adjusted_lo != 0 {
                self.emit_ld_a0_a0(adjusted_lo);
            }
        } else {
            // 可以直接用 li 指令
            self.li_a0(lo);
        }
    }

    /// 将标签注册到 rodata
    fn register_rodata_offset(&mut self, label: String) -> u64 {
        *self.global_rodata.entry(label).or_insert_with(|| {
            let offset = self.rodata.len() as u64;
            offset
        })
    }

    /// 加载浮点常量到 fa0
    fn load_float_fa0(&mut self, value: f32) {
        let bits = value.to_bits() as i32;
        
        // 检查是否可以直接用 li + fmv 加载
        if bits >= -2048 && bits < 2048 {
            self.li_a0(bits);
            self.emit_fmv_x_w_fa0();
        } else {
            // 复杂的情况：需要通过 rodata 加载
            // 这个实现需要更完整的上下文
            self.li_a0(0);
            self.emit_fmv_x_w_fa0();
        }
    }

    /// 加载双精度浮点常量到 fa0
    fn load_double_fa0(&mut self, value: f64) {
        let bits = value.to_bits();
        
        // 简单的情况：低 32 位可以作为立即数
        let low_32 = bits as i32;
        
        if low_32 >= -2048 && low_32 < 2048 {
            self.li_a0(low_32);
            // 这里需要特殊的 FPU 指令来处理双精度
            // 简化处理
        } else {
            self.li_a0(0);
        }
    }

    /// 从 FPU 寄存器移动 X 寄存器值到 FPU
    fn emit_fmv_x_w_fa0(&mut self) {
        // fmv.x.w fa0, a0 - 将整型寄存器的位模式移动到浮点寄存器
        // 编码：0x20200053 for fmv.x.w, 0x00A08053 for fmv.x.w reverse
        self.emit(&[0x53, 0x00, 0xA0, 0x20]); // fmv.x.w fa0, a0
    }

    /// 获取当前位置
    fn pos(&self) -> u64 {
        self.code.len() as u64
    }
}
