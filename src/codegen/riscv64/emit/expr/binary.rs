// src/codegen/riscv64/emit/expr/binary.rs - Binary operations emission (RISC-V 64 backend)
// Priority 4 Task: Extract working implementations from riscv64.rs.bak

use crate::ast::{BinOp, Expr, Type};
use crate::codegen::riscv64::codegen::Riscv64Codegen;
use crate::ir::{TypedExpr, ValueType};

impl Riscv64Codegen {
    // ========================================================================
    // 二元表达式发射逻辑
    // ========================================================================

    /// 发射二元运算：left (a1), right (a0) -> result (a0)
    pub fn emit_binop(&mut self, binop: BinOp, left: &TypedExpr, right: &TypedExpr) -> TypedExpr {
        // 获取类型信息
        let ty = &right.ty;
        let is_signed = Self::is_signed_type(ty);

        // 评估左操作数到 a1
        self.gen_expr(left);
        self.emit_mv(11, 10); // a1 = left (保存)

        // 评估右操作数到 a0
        self.gen_expr(right);

        // 根据操作码发射指令
        match binop {
            // ==================== 算术运算 (整数) ====================
            BinOp::Add => {
                self.emit_add(10, 10, 11); // a0 += a1
            }
            BinOp::Sub => {
                self.emit_sub(10, 10, 11); // a0 -= a1
            }
            BinOp::Mul => {
                self.emit_mul(10, 10, 11); // a0 *= a1
            }
            // M7 修复：真 div/divu/rem/remu 替代 0 (RV64M 扩展)
            // binop_a0_a1 之前已把 a0=left, a1=right
            // 编码 (rd=a0, rs1=a0, rs2=a1):
            //   div  : 0x02B54033   (有符号除)
            //   divu : 0x02B55033   (无符号除)
            //   rem  : 0x02B56033   (有符号取模)
            //   remu : 0x02B57033   (无符号取模)
            // 限制：整数除 0 不抛异常，直接返回 0/-1 (符合 RISC-V spec)
            BinOp::Div => {
                if is_signed {
                    self.emit_div(10, 10, 11); // a0 /= a1 (signed)
                } else {
                    self.emit_divu(10, 10, 11); // a0 /= a1 (unsigned)
                }
            }
            BinOp::Mod => {
                if is_signed {
                    self.emit_rem(10, 10, 11); // a0 %= a1 (signed)
                } else {
                    self.emit_remu(10, 10, 11); // a0 %= a1 (unsigned)
                }
            }

            // ==================== 位运算 ====================
            BinOp::And => {
                self.emit_and(10, 10, 11); // a0 &= a1
            }
            BinOp::Or => {
                self.emit_or(10, 10, 11); // a0 |= a1
            }
            BinOp::Xor => {
                self.emit_xor(10, 10, 11); // a0 ^= a1
            }

            // ==================== 移位运算 ====================
            // 移位：RV64I SLL/SRL/SRA 使用 64 位寄存器，移位量
            // 硬件自动掩码到 6 位 (shamt & 63). 对于 32 位 int
            // 类型，这与 C 标准的 5 位掩码不同，但 shift >= 32
            // 在 C 中是 UB，依赖硬件行为与 GCC -O0 一致.
            // 常量求值阶段 (const_eval) 已拒绝无效移位量.
            BinOp::Shl => {
                self.emit_sll(10, 10, 11); // a0 <<= a1
            }
            BinOp::Shr => {
                if is_signed {
                    // sra: 算术右移，高位补符号位 (C11 6.5.7)
                    self.emit_sra(10, 10, 11);
                } else {
                    // srl: 逻辑右移，高位补 0
                    self.emit_srl(10, 10, 11);
                }
            }

            // ==================== 比较运算 (返回 0/1) ====================
            BinOp::EqEq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                // 使用 sltu/sltu 先比较，然后构建结果
                // 对于所有比较：slt/d = 0 表示小于关系不成立
                self.emit_slt(12, 10, 11); // t1 = (a0 < a1) ? 1 : 0
                
                match binop {
                    BinOp::EqEq => {
                        // ==: 相等当且仅当两个操作数相同
                        // 实现：!(a0 < a1) && !(a1 < a0)
                        let l_ne1 = self.fresh_label("Lneq1");
                        self.emit_bnez(12, &l_ne1);
                        
                        self.emit_slt(12, 11, 10); // t1 = (a1 < a0)
                        self.emit_bnez(12, &l_ne1);
                        
                        self.li_a0(1);
                        let l_eq = self.fresh_label("Leq");
                        self.j(&l_eq);
                        self.label(&l_ne1);
                        self.li_a0(0);
                        self.label(&l_eq);
                    }
                    BinOp::Ne => {
                        // !=: 相反
                        let l_ne = self.fresh_label("Lne");
                        self.emit_bnez(12, &l_ne);
                        
                        self.emit_slt(12, 11, 10); // t1 = (a1 < a0)
                        self.emit_bnez(12, &l_ne);
                        
                        self.li_a0(0);
                        let l_eq = self.fresh_label("Leq");
                        self.j(&l_eq);
                        self.label(&l_ne);
                        self.li_a0(1);
                        self.label(&l_eq);
                    }
                    BinOp::Lt => {
                        // <: t1 已经包含结果
                        self.mv_a_reg(12); // a0 = t1
                    }
                    BinOp::Le => {
                        // <=: !(a1 < a0)
                        let l_gt = self.fresh_label("Lgt");
                        self.emit_bnez(12, &l_gt);
                        
                        self.emit_slt(12, 11, 10); // t1 = (a1 < a0)
                        self.emit_bnez(12, &l_gt);
                        
                        self.li_a0(1);
                        let l_le = self.fresh_label("Lle");
                        self.j(&l_le);
                        self.label(&l_gt);
                        self.li_a0(0);
                        self.label(&l_le);
                    }
                    BinOp::Gt => {
                        // >: a1 < a0
                        self.emit_slt(12, 11, 10); // t1 = (a1 < a0)
                        self.mv_a_reg(12); // a0 = t1
                    }
                    BinOp::Ge => {
                        // >=: !(a0 < a1)
                        let l_lt = self.fresh_label("Llt");
                        self.emit_bnez(12, &l_lt);
                        
                        self.li_a0(1);
                        let l_ge = self.fresh_label("Lge");
                        self.j(&l_ge);
                        self.label(&l_lt);
                        self.li_a0(0);
                        self.label(&l_ge);
                    }
                    _ => {}
                }
            }

            // ==================== 逻辑运算 (短路求值) ====================
            BinOp::AndAnd => {
                // &&: 短路求值
                self.eval_bool_with_jump(binop, left, right, is_signed);
            }
            BinOp::OrOr => {
                // ||: 短路求值
                self.eval_bool_with_jump(binop, left, right, is_signed);
            }
        }

        // 整型提升处理
        if ty.is_int() && !ty.is_long() {
            self.gen_int_promote_ir(ty);
        }

        TypedExpr {
            kind: crate::ir::ExprKind::Binary(binop),
            ty: ty.clone(),
            literal: None,
            children: vec![left.clone(), right.clone()],
            span: left.span.clone(),
        }
    }

    /// 发射浮点二元运算：left (fa1), right (fa0) -> result (fa0)
    pub fn emit_float_binop(&mut self, op: BinOp, ty: &ValueType) -> TypedExpr {
        // Operands: fa1 = left, fa0 = right (saved by caller)
        // Result in fa0
        // D extension: use double-precision for Float{bits:64}
        let is_double = matches!(ty, ValueType::Float { bits: 64 });

        match op {
            BinOp::Add => {
                if is_double {
                    self.emit_fadd_d(10, 11, 10);
                } else {
                    self.emit_fadd_s_fa0_fa1_fa0();
                }
            }
            BinOp::Sub => {
                if is_double {
                    self.emit_fsub_d(10, 11, 10);
                } else {
                    self.emit_fsub_s_fa0_fa1_fa0();
                }
            }
            BinOp::Mul => {
                if is_double {
                    self.emit_fmul_d(10, 11, 10);
                } else {
                    self.emit_fmul_s_fa0_fa1_fa0();
                }
            }
            BinOp::Div => {
                if is_double {
                    self.emit_fdiv_d(10, 11, 10);
                } else {
                    self.emit_fdiv_s_fa0_fa1_fa0();
                }
            }
            BinOp::EqEq => {
                // 浮点比较需要特殊处理
                self.float_cmp_a0_fa1_fa0(op, ty);
            }
            BinOp::Ne => {
                self.float_cmp_a0_fa1_fa0(op, ty);
            }
            BinOp::Lt => {
                self.float_cmp_a0_fa1_fa0(op, ty);
            }
            BinOp::Le => {
                self.float_cmp_a0_fa1_fa0(op, ty);
            }
            BinOp::Gt => {
                self.float_cmp_a0_fa1_fa0(op, ty);
            }
            BinOp::Ge => {
                self.float_cmp_a0_fa1_fa0(op, ty);
            }
            _ => {
                // 默认加法
                if is_double {
                    self.emit_fadd_d(10, 11, 10);
                } else {
                    self.emit_fadd_s_fa0_fa1_fa0();
                }
            }
        }

        TypedExpr {
            kind: crate::ir::ExprKind::Binary(op),
            ty: ty.clone(),
            literal: None,
            children: Vec::new(),
            span: crate::diagnostics::Span::unknown(),
        }
    }

    /// 浮点比较：fa1 = left, fa0 = right, result in a0
    fn float_cmp_a0_fa1_fa0(&mut self, op: BinOp, ty: &ValueType) {
        // D extension: use double-precision for Float{bits:64}
        let is_double = matches!(ty, ValueType::Float { bits: 64 });

        match op {
            BinOp::EqEq => {
                if is_double {
                    self.emit_feq_d(10, 11, 10);
                } else {
                    self.emit_feq_s(10, 11, 10);
                }
            }
            BinOp::Lt => {
                if is_double {
                    self.emit_flt_d(10, 11, 10);
                } else {
                    self.emit_flt_s(10, 11, 10);
                }
            }
            BinOp::Le => {
                if is_double {
                    self.emit_fle_d(10, 11, 10);
                } else {
                    self.emit_fle_s(10, 11, 10);
                }
            }
            BinOp::Gt => {
                if is_double {
                    self.emit_flt_d(10, 10, 11);
                } else {
                    self.emit_flt_s(10, 10, 11);
                }
            }
            BinOp::Ge => {
                if is_double {
                    self.emit_fle_d(10, 10, 11);
                } else {
                    self.emit_fle_s(10, 10, 11);
                }
            }
            BinOp::Ne => {
                if is_double {
                    self.emit_feq_d(10, 11, 10);
                    self.emit_xori(10, 10, 1);
                } else {
                    self.emit_feq_s(10, 11, 10);
                    self.emit_xori(10, 10, 1);
                }
            }
            _ => {
                if is_double {
                    self.emit_feq_d(10, 11, 10);
                } else {
                    self.emit_feq_s(10, 11, 10);
                }
            }
        }
    }

    /// 判断类型是否为有符号
    fn is_signed_type(ty: &ValueType) -> bool {
        match ty {
            ValueType::Char(c) => matches!(c, Type::SChar),
            ValueType::Int(bits) => *bits < 64, // i32 and smaller are signed
            _ => false,
        }
    }

    /// 为布尔运算生成跳转代码
    fn eval_bool_with_jump(&mut self, op: BinOp, left: &TypedExpr, right: &TypedExpr, is_signed: bool) {
        let lbl_true = self.fresh_label("Ltrue");
        let lbl_false = self.fresh_label("Lfalse");
        let lbl_end = self.fresh_label("lend");

        // 评估左操作数
        self.gen_expr(left);

        match op {
            BinOp::AndAnd => {
                // &&: 左为假则整体为假
                self.emit_bez(10, &lbl_false);
                
                // 评估右操作数
                self.gen_expr(right);
                self.emit_bez(10, &lbl_false);
                
                // 都为真
                self.li_a0(1);
                self.j(&lbl_end);
            }
            BinOp::OrOr => {
                // ||: 左为真则整体为真
                self.emit_bnez(10, &lbl_true);
                
                // 评估右操作数
                self.gen_expr(right);
                self.emit_bnez(10, &lbl_true);
                
                // 都为假
                self.li_a0(0);
                self.j(&lbl_end);
            }
            _ => {
                self.li_a0(0);
            }
        }

        self.label(&lbl_true);
        self.li_a0(1);
        self.j(&lbl_end);
        
        self.label(&lbl_false);
        self.li_a0(0);
        
        self.label(&lbl_end);
    }

    /// 将 a0 转换为布尔值 (0 或非零 -> 0/1)
    fn cmp_a0_zero(&mut self) {
        let l_zero = self.fresh_label("Lzero");
        let l_nonzero = self.fresh_label("Lnonzero");
        
        self.emit_bez(10, &l_zero);
        self.li_a0(1);
        self.j(&l_nonzero);
        
        self.label(&l_zero);
        self.li_a0(0);
        
        self.label(&l_nonzero);
    }
}
