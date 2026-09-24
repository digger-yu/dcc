// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::riscv64::codegen::{aggregate_size, is_aggregate, Riscv64Codegen};
use crate::ir::{ExprKind, ExprLiteral, TypedExpr, ValueType};

impl Riscv64Codegen {
    pub(crate) fn gen_call(&mut self, expr: &TypedExpr) {
        // M25: 识别 __builtin_va_start / __builtin_va_end / __builtin_va_copy
        // 内置函数, 不生成实际 jal 调用.
        let callee = match expr.children[0].kind {
            ExprKind::Local(ref n) => n.clone(),
            _ => "unknown".to_string(),
        };
        if callee == "__builtin_va_start" {
            if let Some(ap_expr) = expr.children.get(1) {
                if let ExprKind::Local(ref ap_name) = ap_expr.kind {
                    self.gen_va_start(ap_name);
                }
            }
            self.li_a0(0);
            return;
        }
        if callee == "__builtin_va_end" {
            self.li_a0(0);
            return;
        }
        if callee == "__builtin_va_copy" {
            if let (Some(dst), Some(src)) = (expr.children.get(1), expr.children.get(2)) {
                // 解析 dst 地址到 a0, 移动到 t0 (x5)
                let dst_is_ptr = matches!(dst.ty, ValueType::Pointer(_));
                if dst_is_ptr {
                    self.gen_expr(dst);
                } else {
                    self.gen_lvalue_addr(dst);
                }
                self.emit_mv(5, 10); // t0 = a0 = dst va_list 地址
                                     // 解析 src 地址到 a0, 移动到 t1 (x6)
                let src_is_ptr = matches!(src.ty, ValueType::Pointer(_));
                if src_is_ptr {
                    self.gen_expr(src);
                } else {
                    self.gen_lvalue_addr(src);
                }
                self.emit_mv(6, 10); // t1 = a0 = src va_list 地址
                                     // 24 字节结构体拷贝: 3 个 doubleword (寄存器间接寻址)
                                     // qword 1: gp_offset(4B) + fp_offset(4B)
                self.emit_ld(10, 6, 0); // ld a0, [t1]
                self.emit_sd(10, 5, 0); // sd a0, [t0]
                                        // qword 2: overflow_arg_area
                self.emit_ld(10, 6, 8); // ld a0, [t1+8]
                self.emit_sd(10, 5, 8); // sd a0, [t0+8]
                                        // qword 3: reg_save_area
                self.emit_ld(10, 6, 16); // ld a0, [t1+16]
                self.emit_sd(10, 5, 16); // sd a0, [t0+16]
            }
            self.li_a0(0);
            return;
        }
        // __atomic_* / __sync_* 内建函数特殊处理 (生成 RISC-V 原子指令)
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
                self.li_a0(0);
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
            self.li_a0(value as i32);
            return;
        }
        // __builtin_constant_p(x): 编译期常量判断
        if callee == "__builtin_constant_p" {
            let value = args
                .first()
                .and_then(|a| a.literal.as_ref())
                .map_or(0, |_| 1);
            self.li_a0(value);
            return;
        }
        // __builtin_object_size(ptr, type): 未知时返回 (size_t)-1
        if callee == "__builtin_object_size" || callee == "__builtin_dynamic_object_size" {
            self.li_a0(-1);
            return;
        }
        // __builtin_mul_loop: 非标准, 返回 0
        if callee == "__builtin_mul_loop" {
            self.li_a0(0);
            return;
        }

        // fenv.h 运行时调用 (C11 7.6): 内联为 CSR 指令序列,
        // 不依赖 libm (freestanding 无 libc).
        match callee.as_str() {
            "fegetround" | "fesetround" | "feclearexcept" | "feraiseexcept" | "fetestexcept"
            | "fegetenv" | "fesetenv" | "feholdexcept" | "feupdateenv"
                if !self.labels.contains_key(&callee) =>
            {
                self.gen_fenv_builtin(&callee, args);
                return;
            },
            _ => {},
        }

        // D 扩展内建函数: sqrt/fmin/fmax 直接使用硬件浮点指令
        if callee == "__builtin_sqrt" || callee == "sqrt" || callee == "sqrtf" {
            let is_double = callee == "__builtin_sqrt" || callee == "sqrt";
            if let Some(arg0) = args.first() {
                self.gen_expr(arg0);
            }
            if is_double {
                self.emit_fsqrt_d(10, 10);
            } else {
                self.emit_fsqrt_s(10, 10);
            }
            return;
        }
        if callee == "__builtin_fmin" || callee == "fmin" || callee == "fminf" {
            let is_double = callee == "__builtin_fmin" || callee == "fmin";
            if args.len() >= 2 {
                self.gen_expr(&args[0]);
                self.mv_fa1_fa0();
                self.gen_expr(&args[1]);
            }
            if is_double {
                self.emit_fmin_d(10, 11, 10);
            } else {
                self.emit_fmin_s(10, 11, 10);
            }
            return;
        }
        if callee == "__builtin_fmax" || callee == "fmax" || callee == "fmaxf" {
            let is_double = callee == "__builtin_fmax" || callee == "fmax";
            if args.len() >= 2 {
                self.gen_expr(&args[0]);
                self.mv_fa1_fa0();
                self.gen_expr(&args[1]);
            }
            if is_double {
                self.emit_fmax_d(10, 11, 10);
            } else {
                self.emit_fmax_s(10, 11, 10);
            }
            return;
        }

        // M7 修复: 聚合类型返回 > 16 字节, 走 sret (隐藏 a0 = sret slot)
        // 限制: 仅 caller 分配 <sret> local, callee 写到 [a0]
        let mut sret_reg_idx = 0usize;
        let needs_sret = is_aggregate(&expr.ty) && aggregate_size(&expr.ty).is_some_and(|s| s > 16);
        if needs_sret {
            let size = aggregate_size(&expr.ty).unwrap();
            let aligned = (size + 15) & !15;
            let off = self.alloc_local("<sret>", aligned as i32);
            self.mov_a0_sp_off(off);
            sret_reg_idx = 1;
        }

        // M7 修复: 聚合类型参数 by-ref; > 8 args 走栈 (varargs 基础)
        // 编码: add aN, sp, #off   (RV64I 形式: addi + 大常量走 mv + addi)
        //       sw t0, off(sp)     => S-type, imm[11:5]|rs2|rs1|010|imm[4:0]|0100011
        // 限制: 偏移 12-bit 对齐, 否则需 auipc 序列
        let mut stack_arg_off = self.frame_bytes;
        for (i, arg) in args.iter().enumerate() {
            if is_aggregate(&arg.ty) {
                if let ExprKind::Local(ref n) = arg.kind {
                    if let Some(&off) = self.locals.get(n) {
                        let target_reg = (10 + sret_reg_idx + i) as u8;
                        self.mov_a_reg_sp_off(target_reg, off);
                        continue;
                    }
                }
                let target_reg = (10 + sret_reg_idx + i) as u8;
                self.li_reg(target_reg, 0);
            } else if sret_reg_idx + i >= 8 {
                // varargs: 溢出的实参压栈 (M7 最小)
                self.gen_expr(arg);
                if let ValueType::Float { bits } = &arg.ty {
                    if *bits <= 32 {
                        self.emit_fsw(10, 2, stack_arg_off);
                    } else {
                        self.emit_fsd(10, 2, stack_arg_off);
                    }
                } else {
                    self.store_a0(stack_arg_off);
                }
                stack_arg_off += 8;
            } else {
                self.gen_expr(arg);
                if let ValueType::Float { bits } = &arg.ty {
                    self.mv_fa_reg((sret_reg_idx + i) as u8, *bits);
                } else {
                    self.mv_a_reg((sret_reg_idx + i) as u8);
                }
            }
        }

        let callee_final = match expr.children[0].kind {
            ExprKind::Local(ref n) => n.clone(),
            _ => "unknown".to_string(),
        };
        self.emit_jal_to(&callee_final);
    }
}
