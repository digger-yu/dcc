// src/codegen/x86_64/emit/expr/va_arg.rs
// `<stdarg.h>` va_arg 表达式 rvalue codegen (gen_expr 中的 Expr::VaArg arm)。
//
// 本文件由 mod.rs 逐字搬迁而来 (batch F8): arm body 原样剪切进 `impl Codegen`
// 新方法, 缩进层级与 gen_expr 内层 match 完全一致, 语义 diff 为 0。

use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::x86_64::{Codegen, VaListAddress};

impl Codegen {
    /// gen_expr VaArg 分支 (batch F8 逐字搬迁: Expr::VaArg arm)。
    pub fn gen_expr_va_arg(&mut self, e: &Expr, dst: u8) {
        if let Expr::VaArg { ap, ty: arg_ty, .. } = e {
            // 聚合 va_arg: 物化到临时槽, rax = 槽地址 (与 sret Call
            // 约定一致), 供调用方 gen_aggregate_assign 以 rax 为源拷贝
            // VLA 类型编译时 size 为 8 (ArraySize::Var 退化为指针),
            // 但实际运行时大小动态, 必须走 gen_va_arg_struct 路径
            // 用运行时大小进行正确复制 (20020412-1)
            // 交付约定判据只准问 `va_arg_result_in_memory`（单一真源），不得
            // 在此重写书写形态枚举：旧式 `Struct|Union && size > 8` 漏掉裸
            // 16 字节向量（它的值需两个 8 字节块）与 Array。
            if Self::va_arg_result_in_memory(arg_ty) {
                // 复数与普通聚合的**交付契约**不同: 复数 rvalue 的内存对必须
                // 落在帧底固定的复数物化带 (`complex_result_off`) —— 消费端
                // (`gen_complex_part` / `gen_complex_init` /
                // `spill_complex_operand`) 一律按该地址回读, 不信任寄存器残值;
                // 普通聚合则现分配一个 sret/物化槽, 以 rax = 槽地址交付。
                let cplx_inner = match arg_ty {
                    Type::Complex(inner) => Some(inner.base.clone()),
                    _ => None,
                };
                let slot = if Self::va_arg_result_in_cx_band(arg_ty) {
                    self.complex_result_off()
                } else {
                    self.alloc_sret_slot(arg_ty.size())
                };
                let va_addr = self.resolve_va_list_addr(ap);
                if let VaListAddress::Named(ref name) = va_addr {
                    self.emit_lea_r13(7, slot); // rdi = 临时槽
                    self.gen_va_arg_struct(name, arg_ty);
                } else {
                    // Reg/Lvalue: rcx 已有 va_list 指针, 用指针版 struct 读取
                    self.emit_lea_r13(7, slot); // rdi = 临时槽
                    self.gen_va_arg_ptr_struct(arg_ty);
                }
                // SSE 内型 (float/double) 的复数 rvalue 还要求 **xmm0 = 实部**:
                // 按分类逐 eightbyte 的装载体 (`emit_va_arg_reg_save_load`) 把
                // xmm0 当中转寄存器, 循环结束时它留的是**最后一个** eightbyte
                // (`_Complex double` 即虚部), 而溢出区的 `rep movsb` 根本不经过
                // xmm0 —— 故必须从内存对显式重载实部。`_Complex long double`
                // 的分量只经内存对/x87 交付, 无此要求 (见 `gen_complex_part`)。
                if let Some(inner) = cplx_inner {
                    if matches!(inner, Type::Float | Type::Double) {
                        let pfx = if matches!(inner, Type::Float) { 0xF3u8 } else { 0xF2u8 };
                        self.emit_xmm0_r13_disp(&[pfx, 0x49, 0x0F, 0x10], slot);
                    }
                }
                self.emit_lea_r13(0, slot); // rax = 槽地址
                return;
            }
            // 统一分发: 构造 VaListAddress 后按变体分派
            let va_addr = self.resolve_va_list_addr(ap);
            match va_addr {
                VaListAddress::Named(name) => {
                    self.gen_va_arg(&name, arg_ty, dst);
                },
                VaListAddress::InReg | VaListAddress::Lvalue(_) => {
                    // rcx 已有 va_list 指针地址
                    self.gen_va_arg_ptr(arg_ty, dst);
                },
            }
        }
    }
}
