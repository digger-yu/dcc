// src/codegen/x86_64/emit/expr/compound.rs
// 复合/组合表达式 codegen (gen_expr 中的 Expr::Comma / Expr::Cond / ComplexLit /
// CompoundLit / InitList rvalue arm)。
//
// 本文件由 mod.rs 逐字搬迁而来 (batch F3~F8): arm body 原样剪切进 `impl Codegen`
// 新方法, 缩进层级与 gen_expr 内层 match 完全一致, 语义 diff 为 0。
// 旧影子 `gen_compound_expr` / `gen_ternary_operator` 自由函数 (unimplemented!
// 占位, 与生产 Cond arm 的 emit_truth_flags 约定不一致) 已按"以生产代码为准"作废。

use crate::ast::{Expr, InitItem, Type};
use crate::codegen::x86_64::{Codegen, JumpKind};

impl Codegen {
    /// gen_expr Comma 分支 (batch F3 逐字搬迁: Expr::Comma arm)。
    pub fn gen_expr_comma(&mut self, e: &Expr, dst: u8) {
        match e {
            Expr::Comma { l, r, .. } => {
                // 逗号运算符: 求值 lhs 丢弃, 求值 rhs 结果放入 dst
                self.gen_expr(l, 0);
                self.gen_expr(r, dst);
            },
            _ => {},
        }
    }

    /// gen_expr 三元条件分支 (batch F4 逐字搬迁: Expr::Cond arm)。
    pub fn gen_expr_cond(&mut self, e: &Expr, dst: u8) {
        match e {
            Expr::Cond {
                cond, then_, else_, ..
            } => {
                self.gen_expr(cond, 0);
                self.emit_truth_flags(&cond.ty());
                let le = self.fresh_label();
                let lend = self.fresh_label();
                self.jmp_rel32(&le, JumpKind::Je);
                self.gen_expr(then_, dst);
                self.jmp_rel32(&lend, JumpKind::Jmp);
                self.label(&le);
                self.gen_expr(else_, dst);
                self.label(&lend);
            },
            _ => {},
        }
    }

    /// gen_expr 复合字面量分支 (batch F7 逐字搬迁: Expr::CompoundLit arm)。
    /// (type){...}: 栈上分配可变内存, 初始化后返回地址到 dst。
    pub fn gen_expr_compound_lit(&mut self, e: &Expr, dst: u8) {
        match e {
            Expr::CompoundLit { ty, inits, .. } => {
                if let Type::Vector(elem_ty, n) = ty {
                    self.gen_vector_init_rvalue(inits, &elem_ty.base, *n, 0);
                    let vec_size = elem_ty.base.size() * n;
                    let temp_off = self.vector_temp_base(0, vec_size);
                    self.emit_lea_r13(dst, temp_off);
                    return;
                }
                // 复合字面量: (type){...} - 在栈上分配可变内存, 初始化后返回地址
                // 用唯一名称注册到 var_map, 复用 register_var 的栈分配逻辑
                let compound_name = format!("__compound_lit_{}", self.pos());
                let base_offset = self.register_var(&compound_name, ty);
                // 生成初始化代码
                self.gen_init_list_at(base_offset, ty, inits);
                // dst = r13 + base_offset (compound lit address)
                self.emit_lea_r13(dst, base_offset);
            },
            _ => {},
        }
    }

    /// gen_expr 初始化列表 rvalue 分支 (batch F7 逐字搬迁: Expr::InitList arm)。
    /// 向量 / 标量 brace 初始化 / 不支持类型 (ud2) 三路由。
    pub fn gen_expr_init_list(&mut self, e: &Expr, dst: u8) {
        match e {
            Expr::InitList { inits, .. } => {
                if let Type::Vector(elem_ty, n) = e.ty() {
                    self.gen_vector_init_rvalue(inits, &elem_ty.base, n, 0);
                    // rax 指向临时槽, 供后续 aggregate_assign 使用
                    let vec_size = elem_ty.base.size() * n;
                    let temp_off = self.vector_temp_base(0, vec_size);
                    self.emit_lea_r13(0, temp_off);
                } else if e.ty().is_arith() || e.ty().is_ptr() || matches!(e.ty(), Type::Bool) {
                    // 标量初始化列表 (如 `int x = {1};` 或带 brace 的复合字面量):
                    // 第一个元素就是要赋的值, 后续 (C99 6.7.8/11) 只能是 0
                    if let Some(first) = inits.first() {
                        let v = match first {
                            InitItem::Expr(ex) => ex,
                            InitItem::Designated { value, .. } => value,
                        };
                        self.gen_expr(v, dst);
                    } else {
                        self.mov_imm(dst, 0);
                    }
                } else {
                    self.expr_warn(
                        e,
                        &format!(
                            "codegen unsupported InitList rvalue for {:?}; emitted as ud2",
                            e.ty()
                        ),
                    );
                    self.emit(&[0x0F, 0x0B]); // ud2
                    self.mov_imm(dst, 0);
                }
            },
            _ => {},
        }
    }

    /// gen_expr 复数字面量分支 (batch F9 逐字搬迁: Expr::ComplexLit arm)。
    pub fn gen_expr_complex_lit(&mut self, e: &Expr, _dst: u8) {
        match e {
            Expr::ComplexLit { real, imag, ty, .. } => {
                // C99 虚数字面量 (4.0i / 3j): 实部 → xmm0, 整个复数对写入
                // 帧临时区 result_off (与 gen_complex_binop 结果布局一致),
                // 供后续整体赋值 / __real__ / __imag__ 读取。
                let inner = match ty {
                    Type::Complex(i) => i.base.clone(),
                    _ => Type::Double,
                };
                let inner_sz = inner.size() as i64;
                let result_off = -self.frame_size + 8;
                if matches!(inner, Type::Float | Type::Double | Type::LongDouble) {
                    self.gen_float_imm_load(*real, &inner); // xmm0 = 实部
                    self.gen_float_store_at(result_off, &inner);
                    self.gen_float_imm_load(*imag, &inner); // xmm0 = 虚部
                    self.gen_float_store_at(result_off + inner_sz, &inner);
                    // 实部重新载入 xmm0 (结果约定)
                    self.gen_float_imm_load(*real, &inner);
                } else {
                    // 整数内型 (_Complex int/short/char):
                    // 实部/虚部按类型宽度存储, 避免 store_stack(8B)
                    // 在 isz < 8 时覆盖相邻虚部槽.
                    self.mov_imm(0, *real as i128);
                    self.emit_store(0, result_off, inner_sz as usize);
                    self.mov_imm(0, *imag as i128);
                    self.emit_store(0, result_off + inner_sz, inner_sz as usize);
                    self.mov_imm(0, *real as i128);
                }
                // rax = 指向 complex 值 (spill_complex_operand / 整体赋值
                // 从 rax rep movsb 物化; 浮点路径 rax 已被 imm 加载破坏)
                self.emit_lea_r13(0, result_off);
            },
            _ => {},
        }
    }
}
