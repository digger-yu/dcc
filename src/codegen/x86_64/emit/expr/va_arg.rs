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
        match e {
            Expr::VaArg { ap, ty: arg_ty, .. } => {
                // 聚合 va_arg: 物化到临时槽, rax = 槽地址 (与 sret Call
                // 约定一致), 供调用方 gen_aggregate_assign 以 rax 为源拷贝
                // VLA 类型编译时 size 为 8 (ArraySize::Var 退化为指针),
                // 但实际运行时大小动态, 必须走 gen_va_arg_struct 路径
                // 用运行时大小进行正确复制 (20020412-1)
                // 交付约定判据只准问 `va_arg_result_in_memory`（单一真源），不得
                // 在此重写书写形态枚举：旧式 `Struct|Union && size > 8` 漏掉裸
                // 16 字节向量（它的值需两个 8 字节块）与 Array。
                if Self::va_arg_result_in_memory(arg_ty) {
                    let slot = self.alloc_sret_slot(arg_ty.size());
                    let va_addr = self.resolve_va_list_addr(ap);
                    if let VaListAddress::Named(ref name) = va_addr {
                        self.emit_lea_r13(7, slot); // rdi = 临时槽
                        self.gen_va_arg_struct(name, arg_ty);
                    } else {
                        // Reg/Lvalue: rcx 已有 va_list 指针, 用指针版 struct 读取
                        self.emit_lea_r13(7, slot); // rdi = 临时槽
                        self.gen_va_arg_ptr_struct(arg_ty);
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
            },
            _ => {},
        }
    }
}
