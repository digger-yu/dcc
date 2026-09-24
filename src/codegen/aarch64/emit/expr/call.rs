// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::aarch64::codegen::{
    aggregate_size, classify_hfa, is_aggregate, is_float_ty, is_vector_ty, Aarch64Codegen,
};
use crate::ir::{ExprKind, ExprLiteral, TypedExpr, ValueType};

impl Aarch64Codegen {
    pub(crate) fn gen_call(&mut self, expr: &TypedExpr) {
        // M25: 识别 __builtin_va_start / __builtin_va_end 内置函数, 不生成实际调用.
        // 调用形式: __builtin_va_start(ap, last) — 2 个参数 (children[1..] = [ap, last])
        //           __builtin_va_end(ap)         — 1 个参数 (children[1..] = [ap])
        let callee = self.callee_name(&expr.children[0]);
        if callee == "__builtin_va_start" {
            if let Some(ap_expr) = expr.children.get(1) {
                if let ExprKind::Local(ref ap_name) = ap_expr.kind {
                    self.gen_va_start(ap_name);
                }
            }
            // va_start 返回 void, w0 = 0 是约定值
            self.mov_w0_imm(0);
            return;
        }
        if callee == "__builtin_va_end" {
            // no-op
            self.mov_w0_imm(0);
            return;
        }
        if callee == "__builtin_va_copy" {
            if let (Some(dst), Some(src)) = (expr.children.get(1), expr.children.get(2)) {
                // 解析 dst 地址到 x9
                let dst_is_ptr = matches!(dst.ty, ValueType::Pointer(_));
                if dst_is_ptr {
                    self.gen_expr(dst);
                } else {
                    self.gen_lvalue_addr(dst);
                }
                self.mov_x_reg(9, 0); // x9 = dst va_list 地址
                                      // 解析 src 地址到 x10
                let src_is_ptr = matches!(src.ty, ValueType::Pointer(_));
                if src_is_ptr {
                    self.gen_expr(src);
                } else {
                    self.gen_lvalue_addr(src);
                }
                self.mov_x_reg(10, 0); // x10 = src va_list 地址
                                       // 24 字节结构体拷贝: 3 个 qword (寄存器间接寻址)
                                       // qword 1: gp_offset(4B) + fp_offset(4B)
                self.ldr_xt_indirect(0, 10, 0);
                self.str_xt_indirect(0, 9, 0);
                // qword 2: overflow_arg_area
                self.ldr_xt_indirect(0, 10, 8);
                self.str_xt_indirect(0, 9, 8);
                // qword 3: reg_save_area
                self.ldr_xt_indirect(0, 10, 16);
                self.str_xt_indirect(0, 9, 16);
            }
            self.mov_w0_imm(0);
            return;
        }
        // __atomic_* / __sync_* 内建函数特殊处理 (生成 ARM 原子指令)
        let args = &expr.children[1..];
        if (callee.starts_with("__atomic_") || callee.starts_with("__sync_"))
            && self.gen_atomic_builtin(&callee, args)
        {
            return;
        }

        // GCC 兼容内建函数: 编译期折叠, 不生成实际调用
        // __builtin_expect / __builtin_expect_with_probability:
        //   分支预测提示, 返回第一个实参的值
        if callee == "__builtin_expect" || callee == "__builtin_expect_with_probability" {
            if let Some(arg0) = args.first() {
                self.gen_expr(arg0);
            } else {
                self.mov_w0_imm(0);
            }
            return;
        }
        // __builtin_nonzero_p(x): x 为已知非常量返回 0
        if callee == "__builtin_nonzero_p" {
            let value = args
                .first()
                .and_then(|a| {
                    a.literal.as_ref().and_then(|l| match l {
                        ExprLiteral::Int(v) => Some(if *v != 0 { 1i128 } else { 0i128 }),
                        _ => None,
                    })
                })
                .unwrap_or(0);
            self.mov_w0_imm(value as u32);
            return;
        }
        // __builtin_constant_p(x): 编译期常量判断
        if callee == "__builtin_constant_p" {
            let value = args
                .first()
                .and_then(|a| a.literal.as_ref())
                .map_or(0, |_| 1);
            self.mov_w0_imm(value as u32);
            return;
        }
        // __builtin_object_size(ptr, type): 未知时返回 (size_t)-1
        if callee == "__builtin_object_size" || callee == "__builtin_dynamic_object_size" {
            self.mov_w0_imm(u32::MAX);
            return;
        }
        // __builtin_mul_loop: 非标准, 返回 0
        if callee == "__builtin_mul_loop" {
            self.mov_w0_imm(0);
            return;
        }

        // fenv.h 运行时调用 (C11 7.6): 内联为 FPCR/FPSR 指令序列,
        // 不依赖 libm (freestanding 无 libc). 用户在本 TU
        // 自行定义同名函数时 (labels 命中) 回退到普通调用.
        match callee.as_str() {
            "fegetround" | "fesetround" | "feclearexcept" | "feraiseexcept" | "fetestexcept"
            | "fegetenv" | "fesetenv" | "feholdexcept" | "feupdateenv"
                if !self.labels.contains_key(&callee) =>
            {
                self.gen_fenv_builtin(&callee, args);
                return;
            }
            _ => {}
        }

        // M7 修复: 聚合类型返回 > 16 字节, 走 sret (隐藏 x0 = sret slot)
        // 限制: 仅当 caller 有显式 local sret 时使用; 简化版只在调用方分配
        //       一个 <sret> 局部, callee 写到 [x0]
        let mut sret_reg_idx = 0usize;
        let mut sret_off: Option<i32> = None;
        let needs_sret = is_aggregate(&expr.ty) && aggregate_size(&expr.ty).is_some_and(|s| s > 16);
        if needs_sret {
            let size = aggregate_size(&expr.ty).unwrap();
            let aligned = (size + 15) & !15;
            let off = self.alloc_local("<sret>", aligned as i32);
            self.mov_x0_sp_off(off);
            sret_reg_idx = 1;
            sret_off = Some(off);
        }

        let mut stack_arg_off = self.frame_bytes;
        // 目标为通用寄存器 x0 的整型/指针实参必须最后计算: gen_expr 结果落在
        // w0=x0, 若先算 arg0 再算 arg1, arg1 的 gen_expr 会覆写 x0 而丢失 arg0。
        // (float/vector 实参落 v 寄存器不冲突; 聚合指针→x0 同理需后置。)
        let mut deferred_x0: Option<&crate::ir::TypedExpr> = None;
        for (i, arg) in args.iter().enumerate() {
            let reg_idx = sret_reg_idx + i;
            if let Some(hfa) = classify_hfa(&arg.ty, &self.aggregate_fields) {
                if let ExprKind::Local(ref n) = arg.kind {
                    if let Some(&off) = self.locals.get(n) {
                        self.load_hfa_to_vregs(off, hfa, reg_idx as u8);
                        continue;
                    }
                }
            }
            let to_gp_x0 = reg_idx == 0 && !is_float_ty(&arg.ty) && !is_vector_ty(&arg.ty);
            if to_gp_x0 {
                deferred_x0 = Some(arg);
                continue;
            }
            if is_aggregate(&arg.ty) {
                if let ExprKind::Local(ref n) = arg.kind {
                    if let Some(&off) = self.locals.get(n) {
                        let target_reg = reg_idx as u8;
                        self.mov_x_reg_sp_off(target_reg, off);
                        continue;
                    }
                }
                let target_reg = reg_idx as u8;
                self.mov_x_reg_imm(target_reg, 0);
            } else if reg_idx >= 8 {
                self.gen_expr(arg);
                if let ValueType::Float { bits } = &arg.ty {
                    if *bits <= 32 {
                        self.str_s0_sp(stack_arg_off);
                    } else {
                        self.str_dt_sp(0, stack_arg_off);
                    }
                    stack_arg_off += 8;
                } else if is_vector_ty(&arg.ty) {
                    self.str_q0_sp(stack_arg_off);
                    stack_arg_off += 16;
                } else {
                    self.store_w0(stack_arg_off);
                    stack_arg_off += 8;
                }
            } else {
                self.gen_expr(arg);
                if is_float_ty(&arg.ty) {
                    self.mov_v_reg_s0(reg_idx as u8);
                } else if is_vector_ty(&arg.ty) {
                    self.mov_v_reg_v0(reg_idx as u8);
                } else {
                    self.mov_w_reg_w0(reg_idx as u8);
                }
            }
        }
        // 最后放置 x0 目的地的整型/指针实参。
        if let Some(arg) = deferred_x0 {
            if is_aggregate(&arg.ty) {
                if let ExprKind::Local(ref n) = arg.kind {
                    if let Some(&off) = self.locals.get(n) {
                        self.mov_x_reg_sp_off(0, off);
                    } else {
                        self.mov_x_reg_imm(0, 0);
                    }
                } else {
                    self.mov_x_reg_imm(0, 0);
                }
            } else {
                // gen_expr 结果天然留在 w0 = x0, 无需再 mov。
                self.gen_expr(arg);
            }
        }

        let callee = self.callee_name(&expr.children[0]);
        self.emit_bl_to(&callee);

        if let Some(off) = sret_off {
            // sret 结果从 <sret> 局部首字加载到 w0 (调用方可继续 store)
            self.load_w0(off);
        } else if let Some(hfa) = classify_hfa(&expr.ty, &self.aggregate_fields) {
            // HFA 返回值已在 v0/s0
            let _ = hfa;
        }
    }
}
