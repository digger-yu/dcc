//! emit/expr/aggregate.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
//! 仅移动/可见性调整，不改语义（backend_refactor_plan §3.1 原则 1）。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::shared::elf::*;
use crate::codegen::shared::*;
use crate::codegen::x86_64::asm::*;
use crate::codegen::x86_64::Codegen;
use crate::codegen::x86_64::*;
use crate::const_eval::{self, ConstValue};
use crate::debug::*;
use crate::diagnostics::{Diagnostic, Severity, Span};
use crate::ir::{self, *};
use crate::semantics::{self, *};
use crate::target::Target;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
impl Codegen {
    pub(crate) fn emit_zero_value(&mut self, ty: &Type, dst: u8, sret_off: Option<i64>) {
        if let Some(off) = sret_off {
            self.mov_imm(0, 0);
            self.zero_mem(off, ty.size().max(1));
            self.emit_lea_r13(0, off);
            if dst != 0 {
                self.mov(dst, 0);
            }
            return;
        }
        match ty {
            Type::Void => {},
            Type::Float | Type::Double | Type::LongDouble => {
                self.emit(&[0x0F, 0x57, 0xC0]); // xorps xmm0, xmm0
                if dst != 0 {
                    self.mov_imm(dst, 0);
                }
            },
            Type::Complex(inner) if matches!(&inner.base, Type::Float | Type::Double) => {
                self.emit(&[0x0F, 0x57, 0xC0]); // xorps xmm0, xmm0
                self.emit(&[0x0F, 0x57, 0xC9]); // xorps xmm1, xmm1
                let result_off = -self.frame_size + 8;
                let pfx = if matches!(&inner.base, Type::Float) {
                    0xF3u8
                } else {
                    0xF2u8
                };
                let inner_sz = inner.size() as i64;
                self.emit_xmm_r13_disp(pfx, 0x11, 0, result_off);
                self.emit_xmm_r13_disp(pfx, 0x11, 1, result_off + inner_sz);
                if dst != 0 {
                    self.mov_imm(dst, 0);
                }
            },
            Type::Vector(_, _) => {
                let off = self.vector_temp_base(0, ty.size());
                self.mov_imm(0, 0);
                self.mov_imm(2, 0);
                self.zero_mem(off, ty.size().max(1));
                if dst != 0 {
                    self.mov_imm(dst, 0);
                }
            },
            Type::Int128 | Type::UInt128 | Type::Complex(_) => {
                self.mov_imm(dst, 0);
                self.mov_imm(2, 0);
            },
            _ => self.mov_imm(dst, 0),
        }
    }
    pub(crate) fn gen_agg_elem_store(&mut self, r: &Expr, elem_ty: &Type, es: i32, dst: u8) {
        self.push(1); // 保存目标地址 (rcx)
        if !self.gen_lvalue_addr(r, 6) {
            // 非 lvalue 源 (CompoundLit 等): gen_expr 约定返回聚合地址
            self.gen_expr(r, 0);
            self.mov(6, 0);
        }
        self.pop(7); // rdi = 目标地址
                     // 表达式值 = 目标地址 (dst=0 → rax), 必须在 rep movsb 之前取:
                     // rep movsb 会把 rdi 前进到目标末尾, 之后取 rdi 得到的是 &elem+n
                     // (20070919-1: a = ({... p[3] = b;}) 的块值变成 p[3]+8, 源读到
                     // 数组外全 0)。大小读取只用 rcx/r11, 不破坏 rax。
        self.mov(dst, 7);
        // rcx = 拷贝大小: VM struct/union 运行时 (声明点绑定槽优先,
        // C11 6.2.4p7), 其余编译期元素大小
        if elem_ty.has_vla() {
            if let Some(slot) = self.vm_size_slot(elem_ty) {
                self.emit_mov_mem_r13(1, slot);
            } else {
                self.gen_sizeof_runtime(elem_ty, 11);
                self.mov(1, 11);
            }
        } else {
            self.mov_imm(1, es.max(1) as i128);
        }
        self.emit(&[0xFC]); // cld
        self.emit(&[0xF3, 0x48, 0xA4]); // rep movsb
    }
    pub(crate) fn gen_aggregate_assign(&mut self, dst_name: &str, dst_ty: &Type, r: &Expr) {
        let sz = dst_ty.size();
        let is_global_dst = self.is_global_not_local(dst_name);
        let dst_off = if is_global_dst {
            0 // placeholder, not used for global dest
        } else {
            self.var_offset(dst_name)
        };
        // 特殊处理: 源是函数调用 (走完整 SysV ABI: 参数分类/暂存/返回值)
        let mut call_sret_handled = false;
        if let Expr::Call {
            callee,
            args,
            ty: call_ty,
            ..
        } = r
        {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                if name.name.starts_with("__builtin_") && !Self::is_fortify_builtin(&name.name) {
                    // 复数 builtin (conj 系列) 放行到下方 _Complex rvalue
                    // 分量路径: gen_expr 内部走 gen_builtin 成功物化
                    // (xmm0=实部, 虚部在临时槽), 再逐分量写入目标。
                    // 其余未在 gen_builtin 注册的 builtin 仍走 fallback。
                    let is_complex_builtin = matches!(
                        name.name.as_str(),
                        "__builtin_conj" | "__builtin_conjf" | "__builtin_conjl"
                    );
                    if !is_complex_builtin {
                        self.gen_unknown_builtin_fallback(&name.name, call_ty, 0, Some(dst_off));
                        return;
                    }
                    // complex builtin: 跳过下方 SysV 调用段 (gen_builtin
                    // 已在 gen_expr 内完成物化), 落到 _Complex 分量路径
                } else if Self::needs_memory_return(call_ty) {
                    // sret 返回: 分配隐藏指针槽, 调用后从槽拷贝到目标变量
                    let sret_off = self.alloc_sret_slot(call_ty.size());
                    self.emit_sysv_call(Some(&name.name), None, args, Some(sret_off));
                    self.emit_lea_r13(6, sret_off); // rsi = 源 (sret 槽)
                    if is_global_dst {
                        self.emit_global_addr(7, dst_name); // rdi = 全局目标地址
                    } else {
                        self.emit_lea_r13(7, dst_off); // rdi = 目标地址
                    }
                    call_sret_handled = true;
                } else {
                    // 寄存器返回: 按 abi_classes 存储
                    // (INTEGER→rax:rdx, SSE/SSEUP→xmm0:xmm1)
                    self.emit_sysv_call(Some(&name.name), None, args, None);
                    // 128-bit SSE 向量: abi_classes 生成 [Sse, SseUp] 两个
                    // eightbyte, 但完整向量存在一个 xmm 寄存器, 不能拆分
                    // 用 store_xmm64(xmm0)+store_xmm64(xmm1), 必须用
                    // store_xmm128_stack 一次性存 128bit (pr92618).
                    let is_sse_vector_128 =
                        matches!(call_ty, Type::Vector(_, _)) && call_ty.size() == 16 && {
                            let c = Self::abi_classes(call_ty);
                            c.len() == 2
                                && c.iter()
                                    .all(|x| matches!(x, AbiClass::Sse | AbiClass::SseUp))
                        };
                    if is_global_dst {
                        // Global destination: dst_off=0 is invalid for
                        // store_stack (would corrupt return address).
                        // Store to sret temp, then rep movsb to global.
                        let temp_off = self.alloc_sret_slot(call_ty.size());
                        if is_sse_vector_128 {
                            self.store_xmm128_stack(0, temp_off);
                        } else {
                            let ret_classes = Self::abi_classes(call_ty);
                            for (c, cls) in ret_classes.iter().enumerate() {
                                let off = temp_off + (c as i64) * 8;
                                match cls {
                                    AbiClass::Integer => {
                                        let reg = if c == 0 { 0u8 } else { 2u8 };
                                        self.store_stack(reg, off);
                                    },
                                    AbiClass::Sse | AbiClass::SseUp => {
                                        self.store_xmm64_stack(c as u8, off);
                                    },
                                    _ => unreachable!(),
                                }
                            }
                        }
                        self.emit_lea_r13(6, temp_off); // rsi = src
                        self.emit_global_addr(7, dst_name); // rdi = dst
                                                            // rep movsb handled below
                        call_sret_handled = true;
                    } else {
                        if is_sse_vector_128 {
                            self.store_xmm128_stack(0, dst_off);
                            return;
                        }
                        let ret_classes = Self::abi_classes(call_ty);
                        for (c, cls) in ret_classes.iter().enumerate() {
                            let off = dst_off + (c as i64) * 8;
                            match cls {
                                AbiClass::Integer => {
                                    let reg = if c == 0 { 0u8 } else { 2u8 };
                                    self.store_stack(reg, off);
                                },
                                AbiClass::Sse | AbiClass::SseUp => {
                                    self.store_xmm64_stack(c as u8, off);
                                },
                                AbiClass::Memory
                                | AbiClass::X87
                                | AbiClass::X87Up
                                | AbiClass::ComplexX87 => {
                                    unreachable!()
                                },
                                AbiClass::NoClass => unreachable!(),
                            }
                        }
                        return;
                    }
                }
            }
        }
        // SysV va_arg 聚合读取: 按 abi 分类从寄存器保存区/溢出区读出,
        // 直接写入目标变量缓冲 (rdi)。通用 rax/rdx 物化路径只覆盖 16 字节,
        // >16B 聚合会截断/污染, 必须绕开。
        if let Expr::VaArg { ap, ty: arg_ty, .. } = r {
            if matches!(arg_ty, Type::Struct { .. } | Type::Union { .. })
                && (arg_ty.size() > 8 || Self::type_has_vla(arg_ty))
            {
                let va_addr = self.resolve_va_list_addr(ap);
                // 设置 rdi = 目标地址 (resolve 之后, 避免被 scratch 覆盖)
                if is_global_dst {
                    self.emit_global_addr(7, dst_name); // rdi = 全局目标地址
                } else if let Some(&vla_off) = self.vla_offsets.get(dst_name) {
                    // VLA/VM struct 目标: var_offset 指向元数据区,
                    // 首槽存数据基址, 必须先加载到 rdi
                    self.load_stack(7, vla_off);
                } else {
                    self.emit_lea_r13(7, dst_off); // rdi = 目标地址
                }
                if let VaListAddress::Named(ref name) = va_addr {
                    self.gen_va_arg_struct(name, arg_ty);
                } else {
                    // rcx 仍有 va_list 指针 (emit_lea_r13/emit_global_addr
                    // 只写 rdi, 不覆盖 rcx)
                    self.gen_va_arg_ptr_struct(arg_ty);
                }
                return;
            }
        }
        // 1. 获取源地址到 rsi
        // _Complex 目标 + 标量 RHS (非 complex/imaginary): 不能把标量
        // 变量地址当聚合源整体拷贝 (c = i 会从 &i 拷 16 字节, 实部
        // 混入 i 的栈上高位垃圾), 强制走下方 got_src==false 的物化
        // 分量转换分支
        let complex_scalar_rhs = matches!(dst_ty, Type::Complex(_))
            && !matches!(r.ty(), Type::Complex(_) | Type::Imaginary(_));
        let got_src = if call_sret_handled {
            true
        } else if complex_scalar_rhs {
            false
        } else {
            match r {
                Expr::Ident { name: src_name, .. } => {
                    if self.is_global_var(&src_name.name) {
                        self.emit_global_addr(6, &src_name.name); // rsi = 全局变量地址
                    } else if let Some(&off) = self.vla_offsets.get(&src_name.name) {
                        // VM struct/VLA 变量: 源地址 = 数据区基址槽
                        self.load_stack(6, off);
                    } else {
                        let src_off = self.var_offset(&src_name.name);
                        self.emit_lea_r13(6, src_off); // rsi = 源地址 (reg 6 = rsi)
                    }
                    true
                },
                // *ptr: evaluate ptr to get address, use directly as source.
                // This avoids the rax/rdx temp path which only handles 16 bytes.
                Expr::Deref { e: inner, .. } => {
                    self.gen_expr(inner, 0); // rax = ptr
                    self.mov(6, 0); // rsi = rax (source address)
                    true
                },
                // 聚合 lvalue 源 (p[i] / s.f): 直接取地址。通用回退路径
                // 把 rax:rdx 当前 16 字节内容物化, 对聚合 rvalue (地址
                // 约定) 会拷成地址字节 (20070919-1)
                Expr::Index { .. } | Expr::Member { .. } => self.gen_lvalue_addr(r, 6),
                // 链式聚合赋值 / 语句表达式块值: gen_expr 约定返回聚合
                // 目标地址, 直接作源 (20070919-1: a = ({ ...; p[3] = b; }))
                Expr::Assign { .. } | Expr::StmtExpr { .. } => {
                    self.gen_expr(r, 0);
                    self.mov(6, 0); // rsi = rax (聚合源地址)
                    true
                },
                _ => false,
            }
        };
        if !got_src {
            if matches!(dst_ty, Type::Vector(_, _)) {
                self.gen_expr(r, 0);
                let temp_off = self.vector_temp_base(0, sz);
                self.emit_lea_r13(6, temp_off);
            } else {
                // _Complex 浮点 rvalue (非 Call/Ident): 求值后实部在 xmm0,
                // 虚部在 [r13+result_off+inner_sz] (binop/取负/字面量物化
                // 位置), 直接分存两分量, 不走 rax 单槽 + rep movsb 通用路径
                if let Type::Complex(ref inner) = dst_ty {
                    let dst_float =
                        matches!(&inner.base, Type::Float | Type::Double | Type::LongDouble);
                    let src_complex = matches!(r.ty(), Type::Complex(_) | Type::Imaginary(_));
                    let src_inner = match r.ty() {
                        Type::Complex(ref c) | Type::Imaginary(ref c) => Some(c.base.clone()),
                        Type::Float | Type::Double | Type::LongDouble => Some(r.ty()),
                        _ => None,
                    };
                    let src_float = src_inner.as_ref().is_some_and(|b| {
                        matches!(b, Type::Float | Type::Double | Type::LongDouble)
                    });
                    if dst_float || src_float {
                        if is_global_dst {
                            // 全局目标: 分量路径的 gen_float_store_at 是
                            // r13 栈槽相对, 对全局变量会写到栈顶附近。
                            // 先求值再取全局地址, 按全局连续布局写入。
                            self.gen_expr(r, 0);
                            self.emit_global_addr(1, dst_name); // rcx = dst
                            if dst_float {
                                self.store_complex_at_rcx(r, &inner.base);
                            } else {
                                // 整数目标 + 浮点源 (C99 6.3.1.7 分量截断):
                                // cvttsd2si 后写 [rcx]/[rcx+isz]
                                let isz = inner.base.size() as i64;
                                let result_off = -self.frame_size + 8;
                                if src_complex {
                                    let si = src_inner.clone().unwrap();
                                    let rpfx =
                                        if matches!(si, Type::Float) { 0xF3u8 } else { 0xF2u8 };
                                    self.emit_xmm0_r13_disp(&[rpfx, 0x49, 0x0F, 0x10], result_off);
                                }
                                self.emit(&[0xF2, 0x48, 0x0F, 0x2C, 0xC0]); // cvttsd2si rax, xmm0
                                if isz == 4 {
                                    self.emit(&[0x89, 0x01]); // mov [rcx], eax
                                } else {
                                    self.emit(&[0x48, 0x89, 0x01]); // mov [rcx], rax
                                }
                                if src_complex {
                                    let si = src_inner.clone().unwrap();
                                    self.gen_complex_imag_into_xmm0(r, &si);
                                } else {
                                    self.emit(&[0x0F, 0x57, 0xC0]); // xorps xmm0, xmm0
                                }
                                self.emit(&[0xF2, 0x48, 0x0F, 0x2C, 0xC0]);
                                if isz == 4 {
                                    self.emit(&[0x89, 0x41, isz as u8]); // mov [rcx+isz], eax
                                } else {
                                    self.emit(&[0x48, 0x89, 0x41]);
                                    self.emit(&(isz as i32).to_le_bytes());
                                }
                            }
                        } else {
                            self.gen_expr(r, 0);
                            if dst_float {
                                // Convert integer RHS to float if needed
                                // (e.g., c = 0 where c is _Complex double)
                                self.emit_scalar_to_xmm0_if_needed(r, &inner.base);
                                // 实部宽度适配: 产出端 (binop 等) 按运算 inner
                                // 重载 xmm0 实部 (Complex Double 结果 = double
                                // 位模式), dst inner 更窄 (Float) 时直接 movss
                                // 取低 32 位得 0 (1.0d 低 32 位), 必须先 cvtsd2ss
                                // (complex-6 test_float: x = 1.0 + 2.0i 实部丢失)
                                if let Type::Complex(ref c) = r.ty() {
                                    self.emit_float_convert(&c.base, &inner.base);
                                }
                                self.gen_float_store_at(dst_off, &inner.base);
                                self.gen_complex_imag_into_xmm0(r, &inner.base);
                                let inner_sz = inner.size() as i64;
                                let imag_base = dst_off + inner_sz;
                                self.gen_float_store_at(imag_base, &inner.base);
                            } else {
                                // 整数目标 + 浮点源: 分量 cvttsd2si 截断转换。
                                // 旧路径按 r 自身 inner 读位模式直写整数槽 →
                                // x = 1.0 + 2.0i (_Complex int x) 实部为
                                // 1.0d 低 32 位 0, conj 得 (0, 巨数)
                                // (complex-6 test_int)。
                                let isz = inner.base.size() as i64;
                                let result_off = -self.frame_size + 8;
                                if src_complex {
                                    let si = src_inner.clone().unwrap();
                                    let rpfx =
                                        if matches!(si, Type::Float) { 0xF3u8 } else { 0xF2u8 };
                                    self.emit_xmm0_r13_disp(&[rpfx, 0x49, 0x0F, 0x10], result_off);
                                }
                                self.emit(&[0xF2, 0x48, 0x0F, 0x2C, 0xC0]); // cvttsd2si rax, xmm0
                                self.emit_store(0, dst_off, isz as usize);
                                if src_complex {
                                    let si = src_inner.clone().unwrap();
                                    self.gen_complex_imag_into_xmm0(r, &si);
                                } else {
                                    self.emit(&[0x0F, 0x57, 0xC0]); // xorps xmm0, xmm0
                                }
                                self.emit(&[0xF2, 0x48, 0x0F, 0x2C, 0xC0]);
                                self.emit_store(0, dst_off + isz, isz as usize);
                            }
                        }
                        // 赋值表达式值 = 左值转换后的值: 按统一 complex
                        // rvalue 约定物化结果 (960512-1: if (c = f()))
                        self.finish_complex_assign_value(is_global_dst, dst_name, dst_off, dst_ty);
                        return;
                    }
                }
                // 整数内型 _Complex rvalue (非 Call/Ident): gen_expr 已按
                // 统一约定把结果物化到 result_off (rax=地址), 直接从
                // result_off 整体拷贝到目标; 整数标量 RHS (rax=值约定)
                // 先物化 (值, 0) 复用同一路径。
                if let Type::Complex(inner) = dst_ty {
                    if !matches!(&inner.base, Type::Float | Type::Double | Type::LongDouble) {
                        let result_off = -self.frame_size + 8;
                        self.gen_expr(r, 0);
                        if !matches!(r.ty(), Type::Complex(_) | Type::Imaginary(_)) {
                            // 标量 RHS: 实部 = rax 值, 虚部清零 (物化为同宽
                            // 布局后走下方同宽整体拷贝)。必须按 inner 宽度
                            // 存储: store_stack 恒写 8 字节, _Complex int 的
                            // 标量 -1 会把 0xFFFFFFFF 同时灌进虚部 (c = -1
                            // 得 (-1,-1) 而非 (-1,0), pr56837)
                            let scalar_isz = inner.base.size();
                            if matches!(r.ty(), Type::Float | Type::Double | Type::LongDouble) {
                                // 浮点标量源: gen_expr 后值在 xmm0,
                                // 先 cvttsd2si 截断为整数 (rax 未定义)
                                self.emit(&[0xF2, 0x48, 0x0F, 0x2C, 0xC0]);
                            }
                            self.emit_store(0, result_off, scalar_isz);
                            self.emit(&[0x48, 0x31, 0xC0]); // xor rax, rax
                            self.emit_store(0, result_off + scalar_isz as i64, scalar_isz);
                        }
                        let dst_isz = inner.size();
                        let src_isz = match r.ty() {
                            Type::Complex(c) | Type::Imaginary(c) => c.size(),
                            _ => dst_isz,
                        };
                        if src_isz == dst_isz {
                            // 同宽: 目标地址 → rdi, 源 result_off → rsi, 整体拷贝
                            if is_global_dst {
                                self.emit_global_addr(7, dst_name);
                            } else {
                                self.emit_lea_r13(7, dst_off);
                            }
                            self.emit_lea_r13(6, result_off);
                            let qwords = sz / 8;
                            let remainder = sz % 8;
                            if qwords > 0 {
                                self.emit(&[0x48, 0xC7, 0xC1]); // mov rcx, imm32
                                self.emit(&(qwords as u32).to_le_bytes());
                                self.emit(&[0xF3, 0x48, 0xA5]); // rep movsq
                            }
                            if remainder > 0 {
                                self.emit(&[0x48, 0xC7, 0xC1]);
                                self.emit(&(remainder as u32).to_le_bytes());
                                self.emit(&[0xF3, 0xA4]); // rep movsb
                            }
                        } else {
                            // 跨宽分量转换 (C99 6.3.1.7): 实/虚部分别转换后
                            // 物化到 result_off 同宽布局再拷贝到目标。源 4B→
                            // 目标 8B movsxd 符号扩展 (旧实现整体拷贝 sz 字节:
                            // _Complex int 物化 8B 打包位模式直接当
                            // _Complex long 实部 + 越界读 8B 垃圾当虚部,
                            // x = 1 + 2i 得 (0x200000001, 垃圾),
                            // complex-6 test_long_int)。
                            let d_inner = &inner.base;
                            // 倒序处理 (先虚部后实部): 源物化区在 result_off
                            // 且实部写回 8B 会覆盖源虚部 (_Complex int 源总宽
                            // 8B), 先处理虚部可避免自覆盖。
                            for part in [1usize, 0] {
                                // 每轮重取物化区基址: load 会覆盖 rax,
                                // 第二轮的分量偏移不能依赖上一轮的 rax
                                self.emit_lea_r13(0, result_off);
                                self.emit_load_mem_typed_disp(
                                    src_isz,
                                    d_inner,
                                    (part * src_isz) as i64,
                                );
                                self.store_stack(0, result_off + (part * dst_isz) as i64);
                            }
                            if is_global_dst {
                                self.emit_global_addr(7, dst_name);
                            } else {
                                self.emit_lea_r13(7, dst_off);
                            }
                            self.emit_lea_r13(6, result_off);
                            let qwords = sz / 8;
                            let remainder = sz % 8;
                            if qwords > 0 {
                                self.emit(&[0x48, 0xC7, 0xC1]); // mov rcx, imm32
                                self.emit(&(qwords as u32).to_le_bytes());
                                self.emit(&[0xF3, 0x48, 0xA5]); // rep movsq
                            }
                            if remainder > 0 {
                                self.emit(&[0x48, 0xC7, 0xC1]);
                                self.emit(&(remainder as u32).to_le_bytes());
                                self.emit(&[0xF3, 0xA4]); // rep movsb
                            }
                        }
                        // 赋值表达式值 = 左值转换后的值: result_off 已含
                        // 结果, 补齐 rax=地址约定 (960512-1: if (c = f()))
                        self.emit_lea_r13(0, result_off);
                        return;
                    }
                }
                // sret 调用返回: gen_expr 后 rax = 结果缓冲地址 (而非数据),
                // 直接以 rax 为源整体拷贝, 不能把 rax/rdx 当作前 16 字节数据物化.
                if matches!(r, Expr::Call { .. }) && Self::needs_memory_return(&r.ty()) {
                    self.gen_expr(r, 0);
                    self.mov(6, 0); // rsi = rax (sret 缓冲地址)
                } else if matches!(r, Expr::CompoundLit { .. }) {
                    // CompoundLit: gen_expr builds the struct on the stack
                    // and returns the address in rax. Use rax directly as
                    // the source address for the copy.
                    self.gen_expr(r, 0);
                    self.mov(6, 0); // rsi = rax (compound lit temp address)
                } else {
                    // 通用路径: 求值表达式, 存到帧临时区, 然后拷贝
                    self.gen_expr(r, 0);
                    // 使用目标变量槽上方的安全临时区域
                    let temp_off = dst_off - ((sz + 7) & !7) as i64; // 在目标下方留临时空间
                    self.store_stack(0, temp_off);
                    if sz > 8 {
                        self.store_stack(2, temp_off + 8); // rdx = 第二部分
                    }
                    self.emit_lea_r13(6, temp_off); // rsi = 源 (reg 6 = rsi)
                }
            }
        }
        // 2. 取目标地址到 rdi
        if is_global_dst {
            self.emit_global_addr(7, dst_name); // rdi = 全局变量地址
        } else if let Some(&off) = self.vla_offsets.get(dst_name) {
            // VM struct/VLA 变量: 目标地址 = 数据区基址 (元数据槽)
            self.load_stack(7, off);
        } else {
            self.emit_lea_r13(7, dst_off); // rdi = 目标地址
        }
        // 3. rep movsq + rep movsb 拷贝
        if dst_ty.has_vla() {
            // VM struct/union: 拷贝大小是运行时值 (声明点绑定槽优先,
            // C11 6.2.4p7), 不能用编译期 size() 回退值
            if let Some(slot) = self.vm_size_slot(dst_ty) {
                self.emit_mov_mem_r13(1, slot); // rcx = 绑定大小
            } else {
                self.gen_sizeof_runtime(dst_ty, 11);
                self.mov(1, 11);
            }
            self.emit(&[0xFC]); // cld
            self.emit(&[0xF3, 0x48, 0xA4]); // rep movsb
            return;
        }
        let qwords = sz / 8;
        let remainder = sz % 8;
        if qwords > 0 {
            self.emit(&[0x48, 0xC7, 0xC1]); // mov rcx, imm32 (qwords)
            self.emit(&(qwords as u32).to_le_bytes());
            self.emit(&[0xF3, 0x48, 0xA5]); // rep movsq
        }
        if remainder > 0 {
            self.emit(&[0x48, 0xC7, 0xC1]); // mov rcx, imm32 (remainder)
            self.emit(&(remainder as u32).to_le_bytes());
            self.emit(&[0xF3, 0xA4]); // rep movsb
        }
    }
    pub(crate) fn gen_init_list(&mut self, var_name: &str, ty: &Type, inits: &[InitItem]) {
        let var_off = self.var_offset(var_name);

        if matches!(
            ty,
            Type::Array(_, _)
                | Type::Vector(_, _)
                | Type::Struct(_, _, _, _)
                | Type::Union(_, _, _, _)
        ) {
            self.gen_init_list_at(var_off, ty, inits);
        } else if let Some(InitItem::Expr(e)) = inits.first() {
            // 标量类型的单元素初始化 (C23 大括号初始化: double x = {1})
            self.gen_expr(e, 0);
            if matches!(ty, Type::Float | Type::Double | Type::LongDouble) {
                self.emit_scalar_to_xmm0_if_needed(e, ty);
                if matches!(ty, Type::Float) {
                    self.store_xmm32_stack(0, var_off);
                } else {
                    self.store_xmm64_stack(0, var_off);
                }
            } else {
                self.mov_to_stack(0, var_name);
            }
        }
    }
    pub(crate) fn gen_init_list_at(&mut self, base: i64, ty: &Type, inits: &[InitItem]) {
        match ty {
            Type::Array(elem_ty, _) => {
                // C11 6.7.9p14 brace elision: 元素类型是聚合体但
                // 初始值不是花括号包裹时, 把标量值分组到子聚合中
                // 如 int a[2][3] = {1,2,3,4,5,6} → {{1,2,3},{4,5,6}}
                let effective_inits;
                if matches!(
                    &elem_ty.base,
                    Type::Array(_, _) | Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)
                ) {
                    // 检查是否需要 brace elision: 首个值是标量 (非 InitList)
                    let needs_elision = inits.iter().any(|item| {
                        matches!(item, InitItem::Expr(e) if !matches!(e, Expr::InitList { .. }))
                    });
                    if needs_elision {
                        effective_inits = Self::group_for_brace_elision(&elem_ty.base, inits);
                    } else {
                        effective_inits = inits.to_vec();
                    }
                } else {
                    effective_inits = inits.to_vec();
                }
                let inits = &effective_inits;
                // 数组初始化: int a[] = {1, 2, 3}
                // 数组在栈上布局: arr[0] 在 base, arr[1] 在 base + elem_size, ...
                // 注意: 虽然栈向下增长，但数组元素地址是递增的
                let elem_size = elem_ty.size();
                // 已初始化覆盖到的最大元素下标 (C 标准: 剩余元素必须零填充)
                let mut covered = 0usize;
                for (i, item) in inits.iter().enumerate() {
                    // GNU 范围设计符 [first ... last]: 范围内每个元素写同一值
                    if let InitItem::Designated {
                        designator: Designator::Range(first, last),
                        value,
                    } = item
                    {
                        let (Ok(f), Ok(l)) = (const_eval::eval(first), const_eval::eval(last))
                        else {
                            continue;
                        };
                        let (f, l) = (f as usize, l as usize);
                        if l < f {
                            continue;
                        }
                        let mut k = f;
                        while k <= l {
                            self.gen_expr(value, 0);
                            if matches!(
                                &elem_ty.base,
                                Type::Float | Type::Double | Type::LongDouble
                            ) {
                                self.emit_scalar_to_xmm0_if_needed(value, &elem_ty.base);
                            }
                            let off = base + (k * elem_size) as i64;
                            if matches!(&elem_ty.base, Type::Float) {
                                self.store_xmm32_stack(0, off);
                            } else if matches!(&elem_ty.base, Type::Double | Type::LongDouble) {
                                self.store_xmm64_stack(0, off);
                            } else {
                                self.emit_store(0, off, elem_size);
                            }
                            covered = covered.max(k + 1);
                            k += 1;
                        }
                        continue;
                    }
                    let (_idx, value) = match item {
                        InitItem::Expr(e) => (i, e),
                        InitItem::Designated { designator, value } => {
                            // 链式 designator 仍可能作用在数组上 (如 [2].field)
                            if let Some((off, _, _)) = resolve_designator(ty, designator) {
                                if off == i * elem_size {
                                    (i, value)
                                } else {
                                    // 跳位设计符 (如 {[3]=7}): 覆盖到 off 之后, 避免补零误清
                                    covered = covered.max(off / elem_size + 1);
                                    continue;
                                }
                            } else {
                                continue;
                            }
                        },
                    };
                    if let Expr::InitList { inits: nested, .. } = value {
                        // 嵌套数组初始化 (如 int m[2][2] = {{1,2},{3,4}}):
                        // 递归展开子数组 (任意深度), 跳过 gen_expr 对 InitList
                        // 的简化处理; 子数组不足的尾部零填充由递归内完成
                        self.gen_init_list_at(base + (i * elem_size) as i64, &elem_ty.base, nested);
                        covered = covered.max(i + 1);
                        continue;
                    }
                    // C11 6.7.9p14 brace elision 已由循环前的
                    // group_for_brace_elision 预处理, 此处不再需要
                    if let Expr::String { data, .. } = value {
                        // 字符串字面量初始化嵌套子对象 (C11 6.7.9p14):
                        // 2D char 数组行 / struct 数组元素。
                        // gen_expr(String) 返回 rodata 指针, 直接
                        // emit_store 会把指针存入数组/字段 (原有缺陷:
                        // char t[2][8] = {"abc"} 存地址)。
                        // 目标块: 2D 时是一行 (elem_size), elision 传入
                        // 的 1D char 数组是整个 ty (ty.size())
                        let blk = if Self::is_char_array(&elem_ty.base) {
                            Some((base + (i * elem_size) as i64, elem_size))
                        } else if Self::is_char_array(ty) {
                            Some((base, ty.size()))
                        } else {
                            None
                        };
                        if let Some((dst_off, blk_sz)) = blk {
                            self.zero_mem(dst_off, blk_sz);
                            for (j, &b) in data.iter().enumerate() {
                                if j < blk_sz {
                                    self.mov_imm(0, b as i128);
                                    self.emit_store(0, dst_off + j as i64, 1);
                                }
                            }
                            // 字符串字面量填充的是 char 数组的多个元素, 不是 1 个.
                            // data 长度含终止符, blk_sz 是 char 数组总字节数 = 元素数.
                            // 例如 char a[3] = "12" 填充 3 个元素 (1,2,\0),
                            // covered 必须反映已覆盖的元素数, 否则 zero_fill_tail
                            // 会错误清零已写入的字符 (本 bug: '2' 被清零).
                            let filled = data.len().min(blk_sz);
                            covered = covered.max(i + filled);
                            continue;
                        }
                        if matches!(
                            &elem_ty.base,
                            Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)
                        ) {
                            // brace elision (C11 6.7.9p17): 字符串初始化
                            // 聚合元素 → 递归到聚合分支的字符串字段路径
                            // (整元素零填 + 首个 char 数组字段逐字节拷贝)
                            let sub: Vec<InitItem> = vec![InitItem::Expr(value.clone())];
                            self.gen_init_list_at(
                                base + (i * elem_size) as i64,
                                &elem_ty.base,
                                &sub,
                            );
                            covered = covered.max(i + 1);
                            continue;
                        }
                    }
                    self.gen_expr(value, 0);
                    // 浮点数组元素: int 常量需要转换到 xmm0 (double a[] = {1, 2})
                    if matches!(&elem_ty.base, Type::Float | Type::Double | Type::LongDouble) {
                        self.emit_scalar_to_xmm0_if_needed(value, &elem_ty.base);
                    }
                    let off = base + (i * elem_size) as i64;
                    if matches!(&elem_ty.base, Type::Float) {
                        self.store_xmm32_stack(0, off);
                    } else if matches!(&elem_ty.base, Type::Double | Type::LongDouble) {
                        self.store_xmm64_stack(0, off);
                    } else {
                        self.emit_store(0, off, elem_size);
                    }
                    covered = covered.max(i + 1);
                }
                // C 标准: 初始化器不足时剩余元素零填充 (int a[4] = {7,8} → a[2]=a[3]=0)
                self.zero_fill_tail(base, ty, covered);
            },
            Type::Vector(elem_ty, n) => {
                // SIMD 向量初始化: v4si a = {1, 2, 3, 4}

                let elem_size = elem_ty.size();
                let mut covered = 0usize;
                for (i, item) in inits.iter().enumerate() {
                    if i >= *n {
                        break;
                    }
                    let value = match item {
                        InitItem::Expr(e) => e,
                        _ => continue,
                    };
                    self.gen_expr(value, 0);
                    self.emit_scalar_to_xmm0_if_needed(value, &elem_ty.base);
                    let off = base + (i * elem_size) as i64;
                    if matches!(elem_ty.base, Type::Float) {
                        self.store_xmm32_stack(0, off);
                    } else if matches!(elem_ty.base, Type::Double) {
                        self.store_xmm64_stack(0, off);
                    } else {
                        self.emit_store(0, off, elem_size);
                    }
                    covered = covered.max(i + 1);
                }
                // C 标准: 初始化器不足时剩余元素零填充
                self.zero_fill_tail(base, ty, covered);
            },
            Type::Struct(_, fields, _, reverse_sso) | Type::Union(_, fields, _, reverse_sso) => {
                // struct 初始化: struct Point p = {1, 2} 或 {.x = 1, .y = 2}
                // 或链式: {.u.x = 1} / {.key.dd_key_true = ...}
                // 支持 C99 brace elision: struct { int f[4]; } s = {1,2,3,4}
                // C99: 未显式初始化的字段应零填充
                let reverse_sso = *reverse_sso;
                let struct_size = ty.size();
                if struct_size > 0 && struct_size <= 128 {
                    // 零填充整个结构 (小结构逐字节, 大结构用 rep stosb)
                    for i in 0..struct_size {
                        self.mov_imm(0, 0i128);
                        self.emit_store(0, base + i as i64, 1);
                    }
                }
                let mut field_idx = 0;
                // 跟踪已清零的位域容器偏移, 避免重复清零
                let mut zeroed_containers: std::collections::HashSet<usize> =
                    std::collections::HashSet::new();
                let mut init_idx = 0;
                while init_idx < inits.len() {
                    let item = &inits[init_idx];
                    // 提取字段信息: (offset, size, type, value, bit_width, bit_offset)
                    let (off, sz, field_ty_opt, value, bit_w, bit_o) = match item {
                        InitItem::Expr(e) => {
                            // 跳过匿名位域 (C11 6.7.9p9: 初始化器不作用于
                            // 无名位域, 后续初始值顺延到下一个具名字段;
                            // 与全局初始化路径 gen_global_init_list 保持一致)
                            while field_idx < fields.len()
                                && fields[field_idx].name.is_empty()
                                && fields[field_idx].bit_width.is_some()
                            {
                                field_idx += 1;
                            }
                            if field_idx < fields.len() {
                                let f = &fields[field_idx];
                                // Brace elision: 如果当前字段是聚合类型 (数组/结构体/联合体)
                                // 但初始化值不是 InitList, 收集剩余标量值作为该字段的初始化器
                                let is_agg_field = matches!(
                                    &f.ty,
                                    Type::Array(_, _)
                                        | Type::Struct(_, _, _, _)
                                        | Type::Union(_, _, _, _)
                                );
                                let is_plain_value = !matches!(e, Expr::InitList { .. });
                                // 字符串 ↔ char 数组字段一对一 (C11 6.7.9p14):
                                // 不能走 brace elision 的连续收集 —
                                // struct { char a[8]; char b[8]; } = {"x","y"}
                                // 会把两个字符串都吞给 a, b 全零。
                                if let (true, Expr::String { data, .. }) =
                                    (Self::is_char_array(&f.ty) && is_plain_value, e)
                                {
                                    let dst = base + f.offset as i64;
                                    self.zero_mem(dst, f.ty.size());
                                    for (j, &b) in data.iter().enumerate() {
                                        if j < f.ty.size() {
                                            self.mov_imm(0, b as i128);
                                            self.emit_store(0, dst + j as i64, 1);
                                        }
                                    }
                                    field_idx += 1;
                                    init_idx += 1;
                                    continue;
                                }
                                // 聚合字段被同类型聚合值初始化 (如 `struct A a = {2, b}` 中 b 是 struct B):
                                // 直接复制整个聚合值, 不走 brace elision (后者将 b 误作标量初始化 b.x)
                                let expr_ty = e.ty();
                                let expr_is_agg = matches!(
                                    &expr_ty,
                                    Type::Struct(_, _, _, _)
                                        | Type::Union(_, _, _, _)
                                        | Type::Array(_, _)
                                );
                                if is_agg_field && is_plain_value && !expr_is_agg {
                                    // 收集从这个字段开始的所有剩余标量 InitItem
                                    let mut collected = Vec::new();
                                    let mut j = init_idx;
                                    while j < inits.len() {
                                        if let InitItem::Expr(expr) = &inits[j] {
                                            if !matches!(expr, Expr::InitList { .. }) {
                                                collected.push(inits[j].clone());
                                                j += 1;
                                            } else {
                                                break;
                                            }
                                        } else {
                                            break;
                                        }
                                    }
                                    // 用收集的初始化器递归初始化该聚合字段
                                    self.gen_init_list_at(
                                        base + f.offset as i64,
                                        &f.ty,
                                        &collected,
                                    );
                                    field_idx += 1;
                                    init_idx = j;
                                    continue;
                                }
                                field_idx += 1;
                                (
                                    f.offset,
                                    f.ty.size(),
                                    Some(f.ty.clone()),
                                    e,
                                    f.bit_width,
                                    f.bit_offset,
                                )
                            } else {
                                init_idx += 1;
                                continue;
                            }
                        },
                        InitItem::Designated { designator, value } => {
                            // 链式 designator: resolve_designator 沿 .a.b.c 累积偏移
                            let resolved = match resolve_designator(ty, designator) {
                                Some(r) => r,
                                None => {
                                    init_idx += 1;
                                    continue;
                                },
                            };
                            // 查找位域信息 (处理直接字段和 designator 路径)
                            let (bf_w, bf_o) =
                                if let InitItem::Designated { designator: d, .. } = item {
                                    match d {
                                        Designator::Field(fname) => find_field(fields, &fname.name)
                                            .map(|(f, _)| (f.bit_width, f.bit_offset))
                                            .unwrap_or((None, 0)),
                                        _ => (None, 0),
                                    }
                                } else {
                                    (None, 0)
                                };
                            (resolved.0, resolved.1, Some(resolved.2), value, bf_w, bf_o)
                        },
                    };
                    if let Expr::InitList { inits: nested, .. } = value {
                        // 嵌套初始化 (数组/结构体字段, 如 { int a[3] } = {{1,2,3}}):
                        // 递归展开字段初始化器, gen_expr 对 InitList 只输出 ud2+0
                        if let Some(ft) = &field_ty_opt {
                            self.gen_init_list_at(base + off as i64, ft, nested);
                            init_idx += 1;
                            continue;
                        }
                    }
                    let store_off = base + off as i64;
                    // 字符串初始化数组字段 (char c[8] = "val" / wchar_t w[4] = L"ab"):
                    // gen_expr(String) 返回 rodata 指针, 不能 emit_store (会存指针值);
                    // 改为整段清零后逐字节写内容 (C 6.7.9p14: 剩余元素补零).
                    if let (Some(Type::Array(_, _)), Expr::String { data, .. }) =
                        (&field_ty_opt, value)
                    {
                        self.zero_mem(store_off, sz.max(1));
                        for (j, &b) in data.iter().enumerate() {
                            if j < sz {
                                self.mov_imm(0, b as i128);
                                self.emit_store(0, store_off + j as i64, 1);
                            }
                        }
                        continue;
                    }
                    if let Some(bw) = bit_w {
                        // ── 位域字段初始化: 读-改-写 ──
                        // 计算容器大小: 扫描同 offset 的所有位域,
                        // 取 ceil(total_bits/8) 与 max_ty_size 的较大值,
                        // 再对齐到 {1,2,4,8} (下游 load/store switch
                        // 只处理这四种大小).
                        let container_size = {
                            let mut max_ty_size = sz;
                            let mut bits_end = bit_o + bw;
                            let mut fi = field_idx + 1;
                            while fi < fields.len() {
                                let nf = &fields[fi];
                                if nf.bit_width.is_some() && nf.offset == off {
                                    let ts = nf.ty.size();
                                    if ts > max_ty_size {
                                        max_ty_size = ts;
                                    }
                                    let end = nf.bit_offset + nf.bit_width.unwrap();
                                    if end > bits_end {
                                        bits_end = end;
                                    }
                                    fi += 1;
                                } else {
                                    break;
                                }
                            }
                            let bytes_for_container = bits_end.div_ceil(8);
                            let raw = (bytes_for_container as usize).max(max_ty_size);
                            if raw <= 1 {
                                1
                            } else if raw <= 2 {
                                2
                            } else if raw <= 4 {
                                4
                            } else {
                                8
                            }
                        };
                        // 首次使用此容器时先清零整个容器 (确保非本字域的位 = 0)
                        if zeroed_containers.insert(off) {
                            self.mov_imm(0, 0); // rax = 0
                            self.emit_store(0, store_off, container_size);
                        }
                        // 生成初始化值 → rax
                        self.gen_expr(value, 0);
                        // 保存值到栈临时槽: [r13-40]
                        self.emit(&[0x49, 0x89, 0x45, 0xd8]); // mov [r13-40], rax
                                                              // 用 mov rax, r13 + add rax, imm32 计算地址, 保存到 [r13-24]
                        self.emit(&[0x49, 0x8B, 0xC5]); // mov rax, r13 (REX.W: 49 8B C5)
                        if store_off != 0 {
                            self.emit(&[0x48, 0x05]); // add eax, imm32
                            self.emit(&(store_off as i32).to_le_bytes());
                        }
                        self.emit(&[0x49, 0x89, 0x45, 0xe8]); // mov [r13-24], rax
                                                              // 加载容器值 → rax
                        match container_size {
                            4 => self.emit(&[0x8B, 0x00]),       // mov eax, [rax]
                            8 => self.emit(&[0x48, 0x8B, 0x00]), // mov rax, [rax]
                            2 => {
                                self.emit(&[0x66, 0x8B, 0x00]);
                                self.emit(&[0x48, 0x0F, 0xB7, 0xC0]);
                            },
                            1 => self.emit(&[0x0F, 0xB6, 0x00]),
                            _ => {},
                        }
                        // 保存容器值 → [r13-32]
                        self.emit(&[0x49, 0x89, 0x45, 0xe0]); // mov [r13-32], rax
                                                              // 加载保存的新值 → rax
                        self.emit(&[0x49, 0x8B, 0x45, 0xd8]); // mov rax, [r13-40]
                                                              // 掩码新值: and eax, (1<<bw)-1
                        if bw < 64 {
                            let mask: u64 = (1u64 << bw) - 1;
                            if mask <= 0xFFFFFFFF {
                                self.emit(&[0x48, 0x25]);
                                self.emit(&(mask as u32).to_le_bytes());
                            } else {
                                self.emit(&[0x48, 0xBA]);
                                self.emit(&mask.to_le_bytes());
                                self.emit(&[0x48, 0x21, 0xD0]);
                            }
                        }
                        // 左移到位置: shl rax, bit_o
                        // 对于 reverse_sso: 反转位域在容器内的位置
                        let effective_bit_o = if reverse_sso {
                            let container_bits = (container_size * 8) as u32;
                            container_bits - bit_o - bw
                        } else {
                            bit_o
                        };
                        if effective_bit_o > 0 {
                            self.emit(&[0x48, 0xC1, 0xE0, effective_bit_o as u8]);
                        }
                        // 加载保存的容器值 → rcx
                        self.emit(&[0x49, 0x8B, 0x4D, 0xe0]); // mov rcx, [r13-32]
                                                              // 清除目标位: and rcx, inv_mask
                        let mask: u64 = if bw >= 64 { u64::MAX } else { (1u64 << bw) - 1 };
                        let inv_mask = !(mask << effective_bit_o);
                        if (inv_mask as u32 as i32 as i64 as u64) == inv_mask {
                            self.emit(&[0x48, 0x81, 0xE1]);
                            self.emit(&(inv_mask as u32).to_le_bytes());
                        } else {
                            self.emit(&[0x48, 0xBA]);
                            self.emit(&inv_mask.to_le_bytes());
                            self.emit(&[0x48, 0x21, 0xD1]);
                        }
                        // 合并: or rcx, rax
                        self.emit(&[0x48, 0x09, 0xC1]);
                        // 保存更新后的容器值回 [r13-32]
                        self.emit(&[0x49, 0x89, 0x4D, 0xe0]); // mov [r13-32], rcx
                                                              // 加载地址 → rdx
                        self.emit(&[0x49, 0x8B, 0x55, 0xe8]); // mov rdx, [r13-24]
                                                              // 存储容器 (如果是最后一个位域且 reverse_sso, 需要先字节序反转)
                                                              // 检查是否还有后续位域在同一容器
                        let has_more_bitfields_in_container = {
                            let mut fi = field_idx;
                            let mut found_more = false;
                            while fi < fields.len() {
                                let f = &fields[fi];
                                if f.bit_width.is_some() && f.offset == off {
                                    // 这是同一容器的位域
                                    if fi >= field_idx {
                                        found_more = true;
                                        break;
                                    }
                                } else if f.offset != off {
                                    break;
                                }
                                fi += 1;
                            }
                            found_more
                        };
                        if reverse_sso && !has_more_bitfields_in_container {
                            // 最后一个位域: 字节序反转整个容器
                            // 注意: reverse_sso 改变位域在容器中的打包方向,
                            // 不仅仅是字节序反转。完整实现需要修改位域布局逻辑。
                            // 当前实现: 反转容器中的字节序 (近似处理)
                            match container_size {
                                2 => {
                                    self.emit(&[0x49, 0x8B, 0x4D, 0xe0]); // mov rcx, [r13-32]
                                    self.emit(&[0x66, 0xC1, 0xC1, 0x08]); // rol cx, 8
                                    self.emit(&[0x66, 0x89, 0x0A]); // mov [rdx], cx
                                },
                                4 => {
                                    self.emit(&[0x49, 0x8B, 0x4D, 0xe0]); // mov rcx, [r13-32]
                                    self.emit(&[0x0F, 0xC9]); // bswap ecx
                                    self.emit(&[0x89, 0x0A]); // mov [rdx], ecx
                                },
                                8 => {
                                    self.emit(&[0x49, 0x8B, 0x4D, 0xe0]); // mov rcx, [r13-32]
                                    self.emit(&[0x48, 0x0F, 0xC9]); // bswap rcx
                                    self.emit(&[0x48, 0x89, 0x0A]); // mov [rdx], rcx
                                },
                                _ => {
                                    // 其他大小: 不反转 (1字节无需反转)
                                    match container_size {
                                        4 => self.emit(&[0x89, 0x0A]),       // mov [rdx], ecx
                                        8 => self.emit(&[0x48, 0x89, 0x0A]), // mov [rdx], rcx
                                        2 => self.emit(&[0x66, 0x89, 0x0A]),
                                        1 => self.emit(&[0x88, 0x0A]),
                                        _ => {},
                                    }
                                },
                            }
                        } else {
                            match container_size {
                                4 => self.emit(&[0x89, 0x0A]),       // mov [rdx], ecx
                                8 => self.emit(&[0x48, 0x89, 0x0A]), // mov [rdx], rcx
                                2 => self.emit(&[0x66, 0x89, 0x0A]),
                                1 => self.emit(&[0x88, 0x0A]),
                                _ => {},
                            }
                        }
                    } else {
                        // 聚合字段被聚合值初始化 (struct/union 复制):
                        // 用 rep movsb 整块复制, 避免 gen_expr 只加载前 8 字节
                        let val_ty = value.ty();
                        let val_is_agg =
                            matches!(&val_ty, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _));
                        if val_is_agg && sz > 0 {
                            let mut src_addr_ok = false;
                            if let Expr::Ident { name: src_name, .. } = value {
                                if let Some(src_off) =
                                    self.var_map.get(&src_name.name).map(|(o, _)| *o)
                                {
                                    // rdi = 目标地址
                                    self.emit_lea_r13(7, store_off); // rdi = dest
                                                                     // rsi = 源地址
                                    self.emit_lea_r13(6, src_off); // rsi = src
                                    self.mov_imm(1, sz as i128); // rcx = count
                                    self.emit(&[0xFC]); // cld
                                    self.emit(&[0xF3, 0x48, 0xA4]); // rep movsb
                                    src_addr_ok = true;
                                }
                            } else if matches!(value, Expr::CompoundLit { .. }) {
                                // CompoundLit: gen_expr builds on stack,
                                // rax = address of temp
                                self.gen_expr(value, 0);
                                // rdi = dest, rsi = rax (src)
                                self.emit_lea_r13(7, store_off); // rdi = dest
                                self.mov(6, 0); // rsi = rax
                                self.mov_imm(1, sz as i128); // rcx = count
                                self.emit(&[0xFC]); // cld
                                self.emit(&[0xF3, 0x48, 0xA4]); // rep movsb
                                src_addr_ok = true;
                            } else if matches!(
                                value,
                                Expr::Member { .. } | Expr::Index { .. } | Expr::Deref { .. }
                            ) {
                                // Lvalue: get address directly
                                if self.gen_lvalue_addr(value, 0) {
                                    self.emit_lea_r13(7, store_off); // rdi = dest
                                    self.mov(6, 0); // rsi = rax
                                    self.mov_imm(1, sz as i128); // rcx = count
                                    self.emit(&[0xFC]); // cld
                                    self.emit(&[0xF3, 0x48, 0xA4]); // rep movsb
                                    src_addr_ok = true;
                                }
                            }
                            if src_addr_ok {
                                init_idx += 1;
                                continue;
                            }
                        }
                        self.gen_expr(value, 0);
                        if let Some(ft) = &field_ty_opt {
                            if matches!(ft, Type::Float | Type::Double | Type::LongDouble) {
                                self.emit_scalar_to_xmm0_if_needed(value, ft);
                                if matches!(ft, Type::Float) {
                                    self.store_xmm32_stack(0, store_off);
                                } else {
                                    self.store_xmm64_stack(0, store_off);
                                }
                                init_idx += 1;
                                continue;
                            }
                        }
                        // SSO reversed: byte-swap scalar value before writing
                        if reverse_sso && sz > 1 {
                            match sz {
                                2 => {
                                    self.emit(&[0x66, 0xC1, 0xC0, 0x08]); // rol ax, 8
                                },
                                4 => {
                                    self.emit(&[0x0F, 0xC8]); // bswap eax
                                },
                                8 => {
                                    self.emit(&[0x48, 0x0F, 0xC8]); // bswap rax
                                },
                                _ => {},
                            }
                        }
                        self.emit_store(0, store_off, sz);
                    }
                    init_idx += 1;
                }
            },
            _ => {
                // 递归路径下不可达 (elem_ty/field_ty 为标量时 value 不会是
                // InitList); 保险: 按标量单元素取首值写内存
                if let Some(InitItem::Expr(e)) = inits.first() {
                    self.gen_expr(e, 0);
                    if matches!(ty, Type::Float | Type::Double | Type::LongDouble) {
                        self.emit_scalar_to_xmm0_if_needed(e, ty);
                        if matches!(ty, Type::Float) {
                            self.store_xmm32_stack(0, base);
                        } else {
                            self.store_xmm64_stack(0, base);
                        }
                    } else {
                        self.emit_store(0, base, ty.size().max(1));
                    }
                }
            },
        }
    }
    pub(crate) fn gen_same_size_bitcast(&mut self, from: &Expr, to: &Type, dst: u8) -> bool {
        let src_ty = from.ty();
        if src_ty.size() != to.size() {
            return false;
        }
        if !matches!(src_ty, Type::Vector(_, _) | Type::Complex(_))
            && !matches!(to, Type::Vector(_, _) | Type::Complex(_))
        {
            return false;
        }
        let slot_off = self.vector_temp_base(2, src_ty.size().max(to.size()));
        self.materialize_bits_to_slot(from, slot_off);
        self.load_bits_from_slot(to, slot_off, dst);
        // 向量 bitcast 后, 结果在 slot 0 (temp slot), 需返回地址给 rax —
        // 否则 Index/Call 等后续消费者拿到垃圾 rax 会段错误 (pr70903).
        if matches!(to, Type::Vector(_, _)) {
            let vec_off = self.vector_temp_base(0, to.size());
            self.emit_lea_r13(0, vec_off);
            if dst != 0 {
                self.mov(dst, 0);
            }
        }
        true
    }
    pub(crate) fn gen_sizeof_runtime(&mut self, ty: &Type, dst: u8) {
        match ty {
            Type::Array(elem, ArraySize::Var(n)) => {
                self.gen_expr(n, dst); // dst = 元素个数 (运行时)
                if Self::type_has_vla(&elem.base) {
                    // 元素本身是 VLA: 递归求值 elem_size 到 rcx, dst *= rcx
                    self.gen_sizeof_runtime(&elem.base, 1);
                    self.imul_reg(dst, 1);
                } else {
                    let es = elem.size() as i128;
                    if es != 1 {
                        self.imul_imm(dst, es);
                    }
                }
            },
            Type::Array(elem, ArraySize::Const(c)) => {
                if Self::type_has_vla(&elem.base) {
                    self.gen_sizeof_runtime(&elem.base, 1); // rcx = elem_size
                    self.mov(dst, 1);
                    self.imul_imm(dst, *c as i128);
                } else {
                    self.mov_imm(dst, ty.size() as i128);
                }
            },
            Type::Struct(_, fields, salign, false) => {
                // VM struct (GCC 扩展 variable-sized struct): VLA 字段
                // 之前的偏移是编译期布局 (parser 不受 VLA fallback 影响),
                // 首个 VLA 字段起转入运行时累计: 字段偏移 =
                // align_up(上一字段末尾, 本字段对齐), 字段大小 =
                // sizeof 运行时值 (VLA 字段) 或静态值。最后圆整到
                // struct 对齐。
                // scratch 与 dst 互斥: dst=r11 时 (聚合拷贝/步长回退)
                // 改用 r10, 否则 add dst,scratch 变自加 (r11+=r11)
                let tmp: u8 = if dst == 11 { 10 } else { 11 };
                let mut acc_open = false;
                for f in fields.iter() {
                    let fa = f.field_align.unwrap_or(f.ty.align()).max(1);
                    if !acc_open {
                        if !f.ty.has_vla() {
                            continue; // 静态前缀: f.offset 编译期正确
                        }
                        // 首个 VLA 字段: dst = f.offset + sizeof(f)
                        self.mov_imm(dst, f.offset as i128);
                        self.gen_sizeof_runtime(&f.ty, tmp);
                        // add dst, tmp: opcode 01 /r → r/m=dst(B), reg=src(R)
                        let rex = 0x48
                            | if tmp >= 8 { 0x04 } else { 0 }
                            | if dst >= 8 { 0x01 } else { 0 };
                        self.emit(&[rex, 0x01, 0xC0 | ((tmp & 7) << 3) | (dst & 7)]);
                        acc_open = true;
                    } else {
                        // 动态区: dst = align_up(dst, fa) + sizeof(f)
                        // add/and r/m64=dst, imm32: dst>=8 需 REX.B(0x01)
                        if fa > 1 {
                            let m = (fa - 1) as i128;
                            let rex = 0x48 | if dst >= 8 { 0x01 } else { 0 };
                            self.emit(&[rex, 0x81, 0xC0 | (dst & 7)]);
                            // imm32: 只能发 4 字节 (i128 to_le_bytes 是 16 字节)
                            self.emit(&(m as u32).to_le_bytes());
                            self.emit(&[rex, 0x81, 0xE0 | (dst & 7)]);
                            self.emit(&(!m as u64 as u32).to_le_bytes());
                        }
                        if f.ty.has_vla() {
                            self.gen_sizeof_runtime(&f.ty, tmp);
                            let rex = 0x48
                                | if tmp >= 8 { 0x04 } else { 0 }
                                | if dst >= 8 { 0x01 } else { 0 };
                            self.emit(&[rex, 0x01, 0xC0 | ((tmp & 7) << 3) | (dst & 7)]);
                        } else {
                            let rex = 0x48 | if dst >= 8 { 0x01 } else { 0 };
                            self.emit(&[rex, 0x81, 0xC0 | (dst & 7)]);
                            self.emit(&(f.ty.size() as u32).to_le_bytes());
                        }
                    }
                }
                if !acc_open {
                    // 无 VLA 字段 (不应发生, has_vla 才进来): 静态大小
                    self.mov_imm(dst, ty.size() as i128);
                } else if *salign > 1 {
                    let m = (*salign - 1) as i128;
                    // add/and r/m64=dst, imm32: dst>=8 需 REX.B(0x01)
                    let rex = 0x48 | if dst >= 8 { 0x01 } else { 0 };
                    self.emit(&[rex, 0x81, 0xC0 | (dst & 7)]);
                    // imm32: 只能发 4 字节 (i128 to_le_bytes 是 16 字节)
                    self.emit(&(m as u32).to_le_bytes());
                    self.emit(&[rex, 0x81, 0xE0 | (dst & 7)]);
                    self.emit(&(!m as u64 as u32).to_le_bytes());
                }
            },
            Type::Union(_, fields, _, false) => {
                // VM union: sizeof = max(字段大小)。静态字段取 size(),
                // VLA 字段运行时求值后取 64 位无符号 max。
                // scratch 与 dst 互斥 (同 Struct 分支)
                let tmp: u8 = if dst == 11 { 10 } else { 11 };
                let mut static_max = 0usize;
                let mut acc_open = false;
                for f in fields.iter() {
                    if f.ty.has_vla() {
                        self.gen_sizeof_runtime(&f.ty, tmp);
                        if acc_open {
                            // dst = max(dst, tmp): cmp dst, tmp; cmova dst, tmp
                            // cmp r/m64(dst,B), r64(tmp,R); cmovcc r/m←reg 同式
                            let rex = 0x48
                                | if tmp >= 8 { 0x04 } else { 0 }
                                | if dst >= 8 { 0x01 } else { 0 };
                            self.emit(&[rex, 0x39, 0xC0 | ((tmp & 7) << 3) | (dst & 7)]);
                            self.emit(&[rex, 0x0F, 0x47, 0xC0 | ((tmp & 7) << 3) | (dst & 7)]);
                        } else {
                            self.mov(dst, tmp);
                            acc_open = true;
                        }
                    } else {
                        static_max = static_max.max(f.ty.size());
                    }
                }
                if acc_open && static_max > 0 {
                    // dst = max(dst, static_max)
                    self.mov_imm(tmp, static_max as i128);
                    let rex =
                        0x48 | if tmp >= 8 { 0x04 } else { 0 } | if dst >= 8 { 0x01 } else { 0 };
                    self.emit(&[rex, 0x39, 0xC0 | ((tmp & 7) << 3) | (dst & 7)]);
                    self.emit(&[rex, 0x0F, 0x47, 0xC0 | ((tmp & 7) << 3) | (dst & 7)]);
                } else if !acc_open {
                    self.mov_imm(dst, static_max as i128);
                }
            },
            _ => self.mov_imm(dst, ty.size() as i128),
        }
    }
    fn group_for_brace_elision(elem_ty: &Type, inits: &[InitItem]) -> Vec<InitItem> {
        let sub_count = match elem_ty {
            Type::Array(_, ArraySize::Const(n)) => *n,
            Type::Struct(_, fields, _, false) => {
                // 注意: 首元素是结构体名 (String), 第二个才是字段列表;
                // 匿名位域不可初始化, 不计入字段数
                fields
                    .iter()
                    .filter(|f| !(f.name.is_empty() && f.bit_width.is_some()))
                    .count()
            },
            Type::Union(_, _, _, _) => 1,
            _ => return inits.to_vec(),
        };
        if sub_count == 0 {
            return inits.to_vec();
        }
        let mut result = Vec::new();
        let mut idx = 0;
        while idx < inits.len() {
            if let InitItem::Expr(Expr::InitList { .. }) = &inits[idx] {
                // 已有花括号, 保持原样
                result.push(inits[idx].clone());
                idx += 1;
            } else {
                // 收集 sub_count 个标量值到一个 InitList
                let mut group = Vec::new();
                for _ in 0..sub_count {
                    if idx < inits.len() {
                        if let InitItem::Expr(e) = &inits[idx] {
                            group.push(InitItem::Expr(e.clone()));
                        }
                        idx += 1;
                    }
                }
                let span = Span::point("", 0, 0);
                result.push(InitItem::Expr(Expr::InitList {
                    inits: group,
                    span: span.clone(),
                    ty: elem_ty.clone(),
                }));
            }
        }
        result
    }
    fn zero_fill_tail(&mut self, base_off: i64, arr_ty: &Type, init_count: usize) {
        let (elem_ty, n) = match arr_ty {
            Type::Array(elem_ty, ArraySize::Const(n)) => (elem_ty, *n),
            Type::Vector(elem_ty, n) => (elem_ty, *n),
            _ => return,
        };
        if init_count >= n {
            return;
        }
        let elem_size = elem_ty.size();
        self.mov_imm(0, 0); // rax = 0 (补零源)
        for i in init_count..n {
            self.zero_mem(base_off + (i * elem_size) as i64, elem_size);
        }
    }
    fn zero_mem(&mut self, off: i64, size: usize) {
        let mut o = off;
        let mut rem = size;
        while rem >= 8 {
            self.emit_store(0, o, 8);
            o += 8;
            rem -= 8;
        }
        if rem >= 4 {
            self.emit_store(0, o, 4);
            o += 4;
            rem -= 4;
        }
        if rem >= 2 {
            self.emit_store(0, o, 2);
            o += 2;
            rem -= 2;
        }
        if rem >= 1 {
            self.emit_store(0, o, 1);
        }
    }
}
pub(crate) fn resolve_designator(ty: &Type, d: &Designator) -> Option<(usize, usize, Type)> {
    match d {
        Designator::Field(name) => {
            let fields = match ty {
                Type::Struct(_, fs, _, _) | Type::Union(_, fs, _, false) => fs.as_ref(),
                _ => return None,
            };
            // Use find_field for anonymous struct/union穿透 support
            let (f, extra_off) = find_field(fields, &name.name)?;
            Some((f.offset + extra_off, f.ty.size(), f.ty.clone()))
        },
        Designator::Index(idx_expr) => {
            let elem_ty = match ty {
                Type::Array(e, _) => e.base.clone(),
                _ => return None,
            };
            let idx = match idx_expr {
                Expr::Int { v, .. } if *v >= 0 => *v as usize,
                _ => return None,
            };
            let elem_size = elem_ty.size();
            Some((idx * elem_size, elem_size, elem_ty))
        },
        // GNU 范围设计符 [first ... last]: 覆盖多个元素, 无法返回单个偏移,
        // 由 gen_init_list / gen_global_init_list 的数组分支显式展开。
        Designator::Range(_, _) => None,
        Designator::Chain(items) => {
            let mut cur_ty = ty.clone();
            let mut off = 0usize;
            for (i, sub) in items.iter().enumerate() {
                let (d_off, d_sz, new_ty) = resolve_designator(&cur_ty, sub)?;
                off += d_off;
                if i + 1 < items.len() {
                    // 中间环节使用子类型继续往下走
                    let _ = d_sz;
                    cur_ty = new_ty;
                } else {
                    // 最后一段: 返回 (累计偏移, 大小, 目标类型)
                    return Some((off, d_sz, new_ty));
                }
            }
            None
        },
    }
}
