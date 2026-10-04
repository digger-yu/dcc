//! emit/expr/complex.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
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
    fn emit_complex_div_call(&mut self, is_float: bool, inner_sz: i64) {
        let helper = if is_float { "__divsc3" } else { "__divdc3" };
        let pfx = if is_float { 0xF3u8 } else { 0xF2u8 };
        let r_imag_slot = inner_sz * 3;
        self.emit(&[pfx, 0x0F, 0x10, 0x04, 0x24]);
        self.emit(&[pfx, 0x0F, 0x10, 0x4C, 0x24, inner_sz as u8]);
        self.emit(&[pfx, 0x0F, 0x10, 0x54, 0x24, (inner_sz * 2) as u8]);
        self.emit(&[pfx, 0x0F, 0x10, 0x5C, 0x24, r_imag_slot as u8]);
        self.emit_call_extern(helper);
        if is_float {
            self.emit(&[0x66, 0x0F, 0xD6, 0x04, 0x24]);
        } else {
            self.emit(&[pfx, 0x0F, 0x11, 0x04, 0x24]);
            self.emit(&[pfx, 0x0F, 0x11, 0x4C, 0x24, inner_sz as u8]);
        }
    }
    fn emit_complex_div_limited(
        &mut self,
        load_prefix: u8,
        op_add: u8,
        op_sub: u8,
        op_mul: u8,
        op_div: u8,
        inner_sz: i64,
        total_sz: i64,
    ) {
        // (a+bi)/(c+di): real=(ac+bd)/den, imag=(bc-ad)/den, den=c²+d².
        self.emit(&[load_prefix, 0x0F, 0x10, 0x04, 0x24]);
        self.emit(&[load_prefix, 0x0F, 0x10, 0x4C, 0x24, inner_sz as u8]);
        self.emit(&[load_prefix, 0x0F, 0x10, 0x54, 0x24, total_sz as u8]);
        let r_imag_slot = total_sz + inner_sz;
        self.emit(&[load_prefix, 0x0F, 0x10, 0x5C, 0x24, r_imag_slot as u8]);
        self.mov_xmm(6, 2);
        self.emit(&[load_prefix, 0x0F, op_mul, Self::modrm_reg_reg(6, 6)]);
        self.mov_xmm(7, 3);
        self.emit(&[load_prefix, 0x0F, op_mul, Self::modrm_reg_reg(7, 7)]);
        self.emit(&[load_prefix, 0x0F, op_add, Self::modrm_reg_reg(6, 7)]);
        self.mov_xmm(4, 0);
        self.emit(&[load_prefix, 0x0F, op_mul, Self::modrm_reg_reg(4, 2)]);
        self.mov_xmm(7, 1);
        self.emit(&[load_prefix, 0x0F, op_mul, Self::modrm_reg_reg(7, 3)]);
        self.emit(&[load_prefix, 0x0F, op_add, Self::modrm_reg_reg(4, 7)]);
        self.emit(&[load_prefix, 0x0F, op_div, Self::modrm_reg_reg(4, 6)]);
        self.emit(&[load_prefix, 0x0F, 0x11, 0x24, 0x24]);
        self.mov_xmm(7, 1);
        self.emit(&[load_prefix, 0x0F, op_mul, Self::modrm_reg_reg(7, 2)]);
        self.mov_xmm(5, 0);
        self.emit(&[load_prefix, 0x0F, op_mul, Self::modrm_reg_reg(5, 3)]);
        self.emit(&[load_prefix, 0x0F, op_sub, Self::modrm_reg_reg(7, 5)]);
        self.emit(&[load_prefix, 0x0F, op_div, Self::modrm_reg_reg(7, 6)]);
        self.emit(&[load_prefix, 0x0F, 0x11, 0x7C, 0x24, inner_sz as u8]);
    }
    fn emit_complex_mul_call(&mut self, is_float: bool, inner_sz: i64) {
        let helper = if is_float { "__mulsc3" } else { "__muldc3" };
        let pfx = if is_float { 0xF3u8 } else { 0xF2u8 };
        let r_imag_slot = inner_sz * 3;
        self.emit(&[pfx, 0x0F, 0x10, 0x04, 0x24]);
        self.emit(&[pfx, 0x0F, 0x10, 0x4C, 0x24, inner_sz as u8]);
        self.emit(&[pfx, 0x0F, 0x10, 0x54, 0x24, (inner_sz * 2) as u8]);
        self.emit(&[pfx, 0x0F, 0x10, 0x5C, 0x24, r_imag_slot as u8]);
        self.emit_call_extern(helper);
        if is_float {
            self.emit(&[0x66, 0x0F, 0xD6, 0x04, 0x24]);
        } else {
            self.emit(&[pfx, 0x0F, 0x11, 0x04, 0x24]);
            self.emit(&[pfx, 0x0F, 0x11, 0x4C, 0x24, inner_sz as u8]);
        }
    }
    fn emit_complex_mul_limited(
        &mut self,
        load_prefix: u8,
        op_add: u8,
        op_sub: u8,
        op_mul: u8,
        inner_sz: i64,
        total_sz: i64,
    ) {
        self.emit(&[load_prefix, 0x0F, 0x10, 0x04, 0x24]);
        self.emit(&[load_prefix, 0x0F, 0x10, 0x54, 0x24, total_sz as u8]);
        self.emit(&[load_prefix, 0x0F, 0x10, 0x4C, 0x24, inner_sz as u8]);
        let r_imag_slot = total_sz + inner_sz;
        self.emit(&[load_prefix, 0x0F, 0x10, 0x5C, 0x24, r_imag_slot as u8]);
        self.emit(&[0x66, 0x0F, 0x28, 0xE0]);
        self.emit(&[load_prefix, 0x0F, op_mul, 0xE2]);
        self.emit(&[0x66, 0x0F, 0x28, 0xE9]);
        self.emit(&[load_prefix, 0x0F, op_mul, 0xEB]);
        self.emit(&[load_prefix, 0x0F, op_sub, 0xE5]);
        self.emit(&[load_prefix, 0x0F, 0x11, 0x24, 0x24]);
        self.emit(&[0x66, 0x0F, 0x28, 0xE0]);
        self.emit(&[load_prefix, 0x0F, op_mul, 0xE3]);
        self.emit(&[0x66, 0x0F, 0x28, 0xE9]);
        self.emit(&[load_prefix, 0x0F, op_mul, 0xEA]);
        self.emit(&[load_prefix, 0x0F, op_add, 0xE5]);
        self.emit(&[load_prefix, 0x0F, 0x11, 0x64, 0x24, inner_sz as u8]);
    }
    /// 赋值表达式的值 = 左值转换后的值: 把已写入目标的结果按统一 complex
    /// rvalue 约定再物化到帧底复数带 (`complex_result_off`), 以 rax = 该带
    /// 地址交付。目标寻址方式交给 [`AggDst`] / [`Self::emit_agg_dst_addr`]
    /// (旧写法在此重复一遍 global/vla/local 三分支, 无法服务运行时地址目标)。
    pub(crate) fn finish_complex_assign_value(&mut self, dst: &AggDst, dst_ty: &Type) {
        if let Type::Complex(inner) = dst_ty {
            let total = inner.size() * 2;
            let result_off = self.complex_result_off();
            self.emit_agg_dst_addr(dst, 6);
            self.emit_lea_r13(7, result_off);
            self.mov_imm(1, total as i128);
            self.emit(&[0xFC, 0xF3, 0xA4]); // cld; rep movsb
                                            // f/d rvalue 约定 xmm0=实部; LD rvalue 约定是帧内存+rax,
                                            // 消费端用 fld tbyte 读, 不碰 xmm0。
            if matches!(inner.base, Type::Float | Type::Double) {
                let pfx = if matches!(inner.base, Type::Float) {
                    0xF3u8
                } else {
                    0xF2u8
                };
                self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], result_off);
            }
            self.emit_lea_r13(0, result_off);
        }
    }
    pub(crate) fn gen_complex_binop(&mut self, op: BinOp, l: &Expr, r: &Expr, dst: u8) {
        // 运算 inner 取所有浮点候选中的最宽者 (C99 6.3.1.8 常用算术转换):
        // Complex/Imaginary 的 inner 与标量浮点操作数同场竞争, 整数操作数
        // 不提升复数 inner (int + _Complex float → _Complex float)。旧实现
        // l 侧 Complex inner 优先, _Complex float op _Complex double 误按
        // Float 运算: RHS (Complex double 表达式) 物化 result_off 按 Double
        // 位模式, spill 按运算 inner_sz=4 rep movsb 只拷 8B → r 槽实/虚部
        // 错位 (complex-6: r1 != 1.0-2.0i 误判不等)。
        let cand = |t: &Type| -> Option<Type> {
            match t {
                Type::Complex(c) | Type::Imaginary(c) => Some(c.base.clone()),
                Type::Float | Type::Double | Type::LongDouble => Some(t.clone()),
                _ => None,
            }
        };
        let is_fp_cand = |t: &Type| matches!(t, Type::Float | Type::Double | Type::LongDouble);
        let inner_ty = match (cand(&l.ty()), cand(&r.ty())) {
            (Some(a), Some(b)) => {
                // C99 6.3.1.8: 浮点类别优先于整数 (整数操作数提升到浮点
                // 操作数的类型), 同类别内按宽度取宽者。旧实现仅按宽度,
                // _Complex long (8B) vs _Complex double (8B) 误取 Long,
                // double 位模式被当整数比较恒不等 (complex-6
                // test_long_int: res != 1.0 - 2.0i)。
                if is_fp_cand(&a) && !is_fp_cand(&b) {
                    a
                } else if !is_fp_cand(&a) && is_fp_cand(&b) {
                    b
                } else if a.size() >= b.size() {
                    a
                } else {
                    b
                }
            },
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => return,
        };
        let inner_sz = inner_ty.size() as i64;
        let is_float = matches!(inner_ty, Type::Float);
        let is_float_inner = matches!(inner_ty, Type::Float | Type::Double | Type::LongDouble);
        let total_sz = inner_sz * 2;
        let save_sz = total_sz * 2;
        let result_off = self.complex_result_off();
        if matches!(inner_ty, Type::LongDouble) {
            // `_Complex long double` 走独立 x87 链路（f/d 仍走下方 SSE 路径）。
            // 详见 gen_complex_ld_binop。
            self.gen_complex_ld_binop(op, l, r, dst, result_off);
            return;
        }
        self.emit(&[0x48, 0x83, 0xEC, save_sz as u8]);
        self.spill_complex_operand(l, 0, inner_sz, is_float, is_float_inner, &inner_ty);
        self.spill_complex_operand(r, total_sz, inner_sz, is_float, is_float_inner, &inner_ty);
        let (load_prefix, op_add, op_sub, op_mul, op_div) = if is_float {
            (0xF3u8, 0x58u8, 0x5Cu8, 0x59u8, 0x5Eu8)
        } else {
            (0xF2u8, 0x58u8, 0x5Cu8, 0x59u8, 0x5Eu8)
        };
        match op {
            BinOp::Add | BinOp::Sub if !is_float_inner => {
                // 整数 complex: 实/虚部按内型宽度用整数运算.
                // 栈布局: [rsp+0]=l.real, [rsp+inner]=l.imag,
                //         [rsp+total]=r.real, [rsp+total+inner]=r.imag
                let r_imag_slot = total_sz + inner_sz;
                let add_sub: u8 = if matches!(op, BinOp::Add) { 0x03 } else { 0x2B };
                if inner_sz == 4 {
                    // 32 位分量运算, 避免 64 位加法把实部进位带进虚部
                    self.emit(&[0x8B, 0x04, 0x24]); // mov eax, [rsp+0]
                    self.emit(&[add_sub, 0x44, 0x24, total_sz as u8]);
                    self.emit(&[0x89, 0x04, 0x24]); // mov [rsp+0], eax
                    self.emit(&[0x8B, 0x44, 0x24, inner_sz as u8]); // mov eax, [rsp+inner]
                    self.emit(&[add_sub, 0x44, 0x24, r_imag_slot as u8]);
                    self.emit(&[0x89, 0x44, 0x24, inner_sz as u8]); // mov [rsp+inner], eax
                } else {
                    self.emit(&[0x48, 0x8B, 0x04, 0x24]);
                    self.emit(&[0x48, add_sub, 0x44, 0x24, total_sz as u8]);
                    self.emit(&[0x48, 0x89, 0x04, 0x24]);
                    self.emit(&[0x48, 0x8B, 0x44, 0x24, inner_sz as u8]);
                    self.emit(&[0x48, add_sub, 0x44, 0x24, r_imag_slot as u8]);
                    self.emit(&[0x48, 0x89, 0x44, 0x24, inner_sz as u8]);
                }
            },
            BinOp::Add | BinOp::Sub => {
                let op_code = if matches!(op, BinOp::Add) { op_add } else { op_sub };
                self.emit(&[load_prefix, 0x0F, 0x10, 0x04, 0x24]);
                self.emit(&[load_prefix, 0x0F, 0x10, 0x4C, 0x24, total_sz as u8]);
                self.emit(&[load_prefix, 0x0F, op_code, 0xC1]);
                self.emit(&[load_prefix, 0x0F, 0x11, 0x04, 0x24]);
                self.emit(&[load_prefix, 0x0F, 0x10, 0x44, 0x24, inner_sz as u8]);
                let r_imag_slot = total_sz + inner_sz;
                self.emit(&[load_prefix, 0x0F, 0x10, 0x4C, 0x24, r_imag_slot as u8]);
                self.emit(&[load_prefix, 0x0F, op_code, 0xC1]);
                self.emit(&[load_prefix, 0x0F, 0x11, 0x44, 0x24, inner_sz as u8]);
            },
            BinOp::Mul if !is_float_inner => {
                // 整数 complex 乘法: real=ac-bd, imag=ad+bc (GCC
                // expand_complex_multiplication)。中间积用 imul 低半
                // (与分量宽度回绕语义一致); inner<4 时乘积必不溢出,
                // 用 32 位运算后按分量宽度截断存回。旧实现走 SSE
                // 浮点 limited 路径, 整数位模式当浮点算出垃圾。
                // 栈布局: [rsp+0]=a, [rsp+inner]=b, [rsp+total]=c,
                //         [rsp+total+inner]=d
                self.gen_complex_int_mul(inner_sz, total_sz, &inner_ty);
            },
            BinOp::Div if !is_float_inner => {
                // 整数 complex 除法 (GCC expand_complex_div_wide / Smith
                // 算法): |c|>|d| 时 r=d/c, den=c+r*d, real=(r*b+a)/den,
                // imag=(b-r*a)/den; 否则对偶分支。实测与 GCC 输出一致
                // (直式公式 (ac+bd)/(c²+d²) 与 GCC 中间商回绕语义不符)。
                self.gen_complex_int_div(inner_sz, total_sz, &inner_ty);
            },
            BinOp::Mul => {
                if matches!(inner_ty, Type::Float | Type::Double) && !self.cx_limited_range {
                    self.emit_complex_mul_call(is_float, inner_sz);
                } else {
                    self.emit_complex_mul_limited(
                        load_prefix,
                        op_add,
                        op_sub,
                        op_mul,
                        inner_sz,
                        total_sz,
                    );
                }
            },
            BinOp::Div => {
                if matches!(inner_ty, Type::Float | Type::Double) && !self.cx_limited_range {
                    self.emit_complex_div_call(is_float, inner_sz);
                } else {
                    self.emit_complex_div_limited(
                        load_prefix,
                        op_add,
                        op_sub,
                        op_mul,
                        op_div,
                        inner_sz,
                        total_sz,
                    );
                }
            },
            BinOp::EqEq | BinOp::Ne => {
                // _Complex 比较: a==b ⟺ real(a)==real(b) && imag(a)==imag(b)
                // 栈布局: [rsp+0]=l.real, [rsp+inner]=l.imag,
                //         [rsp+total]=r.real, [rsp+total+inner]=r.imag
                let false_label = self.fresh_label();
                let end_label = self.fresh_label();
                let float_inner = matches!(inner_ty, Type::Float | Type::Double | Type::LongDouble);
                let r_imag_slot = total_sz + inner_sz;
                if float_inner {
                    // 浮点 complex: ucomi 比较实虚部
                    self.emit(&[load_prefix, 0x0F, 0x10, 0x04, 0x24]); // xmm0 = l.real
                    self.emit(&[load_prefix, 0x0F, 0x10, 0x4C, 0x24, total_sz as u8]); // xmm1 = r.real
                    if is_float {
                        self.emit(&[0x0F, 0x2E, 0xC8]); // ucomiss xmm1, xmm0
                    } else {
                        self.emit(&[0x66, 0x0F, 0x2E, 0xC8]); // ucomisd xmm1, xmm0
                    }
                    self.jmp_rel32(&false_label, JumpKind::Jne);
                    self.emit(&[load_prefix, 0x0F, 0x10, 0x44, 0x24, inner_sz as u8]); // xmm0 = l.imag
                    self.emit(&[load_prefix, 0x0F, 0x10, 0x4C, 0x24, r_imag_slot as u8]); // xmm1 = r.imag
                    if is_float {
                        self.emit(&[0x0F, 0x2E, 0xC8]);
                    } else {
                        self.emit(&[0x66, 0x0F, 0x2E, 0xC8]);
                    }
                    self.jmp_rel32(&false_label, JumpKind::Jne);
                } else {
                    // 整数 complex: 按内型宽度比较实虚部
                    if inner_sz <= 4 {
                        // [rsp+0..total) 连续含实部+虚部, 单次整槽比较覆盖
                        // 两者 (否则槽尾垃圾会污染后续分量比较):
                        // inner=4 → 8B, inner=2 → 4B, inner=1 → 2B。
                        // 旧实现仅处理 inner=4, inner<4 时 8B 读取越过槽尾,
                        // 邻接垃圾导致 _Complex short/char 比较误判 (foo().x)。
                        match inner_sz {
                            4 => {
                                self.emit(&[0x48, 0x8B, 0x04, 0x24]); // mov rax, [rsp+0]
                                self.emit(&[0x48, 0x8B, 0x4C, 0x24, total_sz as u8]); // mov rcx, [rsp+total]
                                self.emit(&[0x48, 0x39, 0xC8]); // cmp rax, rcx
                            },
                            2 => {
                                self.emit(&[0x8B, 0x04, 0x24]); // mov eax, [rsp+0]
                                self.emit(&[0x8B, 0x4C, 0x24, total_sz as u8]); // mov ecx, [rsp+total]
                                self.emit(&[0x39, 0xC8]); // cmp eax, ecx
                            },
                            _ => {
                                self.emit(&[0x66, 0x8B, 0x04, 0x24]); // mov ax, [rsp+0]
                                self.emit(&[0x66, 0x8B, 0x4C, 0x24, total_sz as u8]); // mov cx, [rsp+total]
                                self.emit(&[0x66, 0x39, 0xC8]); // cmp ax, cx
                            },
                        }
                        self.jmp_rel32(&false_label, JumpKind::Jne);
                    } else {
                        // 8 字节内型: 实部/虚部各占独立 8 字节, 分两次比较
                        self.emit(&[0x48, 0x8B, 0x04, 0x24]); // mov rax, [rsp+0]
                        self.emit(&[0x48, 0x8B, 0x4C, 0x24, total_sz as u8]); // mov rcx, [rsp+total]
                        self.emit(&[0x48, 0x39, 0xC8]); // cmp rax, rcx
                        self.jmp_rel32(&false_label, JumpKind::Jne);
                        self.emit(&[0x48, 0x8B, 0x44, 0x24, inner_sz as u8]); // mov rax, [rsp+inner]
                        self.emit(&[0x48, 0x8B, 0x4C, 0x24, r_imag_slot as u8]); // mov rcx, [rsp+total+inner]
                        self.emit(&[0x48, 0x39, 0xC8]); // cmp rax, rcx
                        self.jmp_rel32(&false_label, JumpKind::Jne);
                    }
                }
                // 实虚部均相等
                self.mov_imm(dst, if matches!(op, BinOp::EqEq) { 1 } else { 0 });
                self.jmp_rel32(&end_label, JumpKind::Jmp);
                self.label(&false_label);
                self.mov_imm(dst, if matches!(op, BinOp::EqEq) { 0 } else { 1 });
                self.label(&end_label);
                // 比较结果是整数, 不写 complex 结果区
                self.emit(&[0x48, 0x83, 0xC4, save_sz as u8]);
                return;
            },
            _ => {},
        }
        self.store_complex_pair_from_rsp(result_off, inner_sz, is_float);
        self.emit(&[0x48, 0x83, 0xC4, save_sz as u8]);
        if is_float_inner {
            // 实部载入 xmm0，供浮点转换
            let pfx = if is_float { 0xF3u8 } else { 0xF2u8 };
            self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], result_off);
            // rax 指向结果区, 供 spill_complex_operand 的 rep movsb 使用
            self.emit_lea_r13(0, result_off);
        } else {
            // 整数内型: rax = result_off 地址 (与浮点内型统一约定,
            // 消费端从内存对读取)。旧实现 load_stack 使 rax=实部值,
            // 与 Call 物化/spill 的 rax=地址约定冲突 — gen_complex_init
            // 把地址当值存储 (complex-6 c=cm(a) 实部垃圾)。
            self.emit_lea_r13(0, result_off);
        }
    }

    /// 把一个算术操作数（complex / imaginary / 标量，inner 可窄于 LD）
    /// 溢出到 LD 运算区的两个 **16 字节间距** 栈槽：
    ///   real → [rsp + base], imag → [rsp + base + 16]
    /// 每个槽是真 80-bit tbyte（fstpt），与 gcc 为 __mulxc3/__divxc3
    /// 准备入参的布局完全一致；窄操作数先精确拓宽到 st(0) 再 fstpt，
    /// 不做任何 f64 中转。调用后 x87 栈保持平衡。
    ///
    /// 关键：源表达式**只求值一次**。先经 gen_complex_ld_materialize 把整体
    /// 物化成帧内一对 tbyte（返回 real 偏移，imag=real+16），再 fld/fstp 搬到
    /// rsp 运算槽。旧实现 real/imag 各独立 gen_expr 一次，对 complex
    /// rvalue 会把带副作用的 gen_expr 跑两遍（副作用翻倍且第二次覆盖
    /// 同一 result_off 槽）。
    fn spill_complex_ld_operand(&mut self, e: &Expr, base: u8) {
        let src = self.gen_complex_ld_materialize(e);
        // 实部
        self.emit_x87_fld_at(13, src as i32, &Type::LongDouble);
        self.emit_x87_fstp_at(4, base as i32, &Type::LongDouble);
        // 虚部
        self.emit_x87_fld_at(13, src as i32 + 16, &Type::LongDouble);
        self.emit_x87_fstp_at(4, base as i32 + 16, &Type::LongDouble);
    }

    /// 把一个 complex / imaginary / 标量表达式**只求值一次**，物化成帧内一对
    /// 16 字节 80-bit tbyte 槽（real@ret、imag@ret+16，共 32 字节，对齐 16），
    /// 返回 real 槽的 r13 相对偏移。
    ///
    /// 这是所有“需要分别消费 real 与 imag”站点（spill / init / neg / conj /
    /// store / 实参 / 返回）的**唯一物化入口**，消除旧路径对 complex
    /// rvalue 双 gen_expr 的致命模式。
    /// - 标量：real=值本身（窄源精确拓宽到 80-bit），imag=+0.0；
    /// - imaginary：real=+0.0，imag=值本身；
    /// - complex lvalue：从内存对逐分量 fld（按源内型宽度读）后 fstp tbyte；
    /// - complex rvalue（lit/binop/Call/Cast）：gen_expr 一次，整体已按源内型
    ///   物化在 result_off（rax=地址），逐分量 fld 后 fstp tbyte。
    /// 窄于 LD 的源由 fld 硬件无损拓宽（f32/f64 是 80-bit 子集），无 f64 中转。
    /// 物化结束后 x87 栈平衡，rax 不被保留（消费端按返回偏移用 r13 寻址）。
    pub(crate) fn gen_complex_ld_materialize(&mut self, e: &Expr) -> i64 {
        let dst = self.alloc_local(32);
        let (is_cx, src_inner) = match e.ty() {
            Type::Complex(c) | Type::Imaginary(c) => (true, c.base.clone()),
            t => (false, t),
        };
        // 标量源：real=值, imag=0
        if !is_cx {
            self.gen_expr(e, 0);
            self.emit_operand_to_st0(&e.ty(), 0);
            self.emit_x87_fstp_at(13, dst as i32, &Type::LongDouble);
            self.emit_x87_fldz();
            self.emit_x87_fstp_at(13, dst as i32 + 16, &Type::LongDouble);
            return dst;
        }
        let imaginary = matches!(e.ty(), Type::Imaginary(_));
        // imaginary 源：real=0, imag=值
        if imaginary {
            self.emit_x87_fldz();
            self.emit_x87_fstp_at(13, dst as i32, &Type::LongDouble);
            self.gen_expr(e, 0);
            self.emit_operand_to_st0(&src_inner, 0);
            self.emit_x87_fstp_at(13, dst as i32 + 16, &Type::LongDouble);
            return dst;
        }
        // complex 源：lvalue 直读内存对；rvalue gen_expr 一次后读 result_off。
        // src_isz 是源分量宽度（f=4/d=8/LD=16），用于 rvalue 区的 imag 偏移。
        let src_isz = src_inner.size() as i64;
        let is_lvalue = self.gen_lvalue_addr(e, 0);
        if !is_lvalue {
            self.gen_expr(e, 0);
        }
        // 基址：lvalue 在 rax；rvalue 在 r13+result_off。逐分量 fld（按源内型）
        // 后 fstp tbyte 到 dst。两次加载之间不能依赖 rax：fld 不破坏 rax。
        let (base_reg, real_base) = if is_lvalue {
            (0u8, 0i64)
        } else {
            (13u8, self.complex_result_off())
        };
        self.emit_x87_fld_at(base_reg, real_base as i32, &src_inner);
        self.emit_x87_fstp_at(13, dst as i32, &Type::LongDouble);
        self.emit_x87_fld_at(base_reg, (real_base + src_isz) as i32, &src_inner);
        self.emit_x87_fstp_at(13, dst as i32 + 16, &Type::LongDouble);
        dst
    }

    /// 求值 complex/imaginary/标量表达式的一个分量并把结果留在 st(0)。
    /// - complex/imaginary lvalue：fld tbyte/dword/qword 直读内存分量；
    /// - complex/imaginary rvalue（binop/字面量/Call）：gen_expr 已把整体
    ///   物化成 r13 帧里的 [result_off, result_off+src_isz]，按源内型 fld；
    /// - 标量：实部=值本身，虚部=+0.0。
    /// 窄于 LD 的源由 fld 硬件无损拓宽（f32/f64 是 80-bit 子集）。
    /// `_Complex long double` 算术/比较的独立 x87 实现。
    ///
    /// 栈布局（相对 rsp，每分量一个 16 字节 tbyte 槽，对齐 16）：
    ///   [0]=l.real [16]=l.imag [32]=r.real [48]=r.imag
    /// - Add/Sub：内联 x87（fldt/faddp|fsubp），实/虚各一次，结果在
    ///   st0=real、st1=imag；
    /// - Mul/Div：直接 `call __mulxc3`/`__divxc3`（与 gcc 一致），
    ///   8 次 pushq 传 64 字节，返回 st0=real、st1=imag；
    /// - EqEq/Ne：fucomip 逐分量比较。
    /// 结果整体 fstpt 物化为帧 result_off 的两个 16 字节槽（共 32 字节），
    /// 并按 dcc complex rvalue 约定把 rax 置为 result_off。
    fn gen_complex_ld_binop(&mut self, op: BinOp, l: &Expr, r: &Expr, dst: u8, result_off: i64) {
        match op {
            BinOp::EqEq | BinOp::Ne => {
                self.gen_complex_ld_compare(op, l, r, dst);
                return;
            },
            BinOp::Add | BinOp::Sub => {
                // 预留 64 字节（4×16），保持调用对齐（本分支无 call）
                self.emit(&[0x48, 0x83, 0xEC, 0x40]);
                self.spill_complex_ld_operand(l, 0);
                self.spill_complex_ld_operand(r, 32);
                // 先 fld 左、后 fld 右: st0=right, st1=left。
                // 实部: DE C1=faddp(st1=st1+st0) / DE E9=fsubp
                // (st1=st1-st0=left-right, 弹栈)。
                // 切勿用 DE E1=fsubrp — 那算 st1=st0-st1=right-left,
                // 实/虚两部会整体取反 (a-b 被算成 b-a)。
                self.emit_x87_fld_at(4, 0, &Type::LongDouble);
                self.emit_x87_fld_at(4, 32, &Type::LongDouble);
                let real_op = if matches!(op, BinOp::Add) {
                    [0xDE, 0xC1]
                } else {
                    [0xDE, 0xE9]
                };
                self.emit(&real_op); // 弹出后 st0=real
                                     // 虚部同理, 结果压在 real 之上: st0=imag, st1=real
                self.emit_x87_fld_at(4, 16, &Type::LongDouble);
                self.emit_x87_fld_at(4, 48, &Type::LongDouble);
                let imag_op = real_op;
                self.emit(&imag_op);
                // 归一化到返回规范 st0=real, st1=imag（与 gcc add 的末尾
                // fxch %st(1) 一致）
                self.emit(&[0xD9, 0xC9]); // fxch st(1)
            },
            BinOp::Mul | BinOp::Div => {
                let helper = if matches!(op, BinOp::Mul) {
                    "__mulxc3"
                } else {
                    "__divxc3"
                };
                // gcc: subq $64,%rsp 后 fldt/fstpt 落 4 槽，再 8 次 pushq
                // 传参（64 字节，与预留区同尺寸，push 后内容上移，
                // 调用约定读的是 push 出来的栈），call，addq $64,%rsp。
                self.emit(&[0x48, 0x83, 0xEC, 0x40]);
                self.spill_complex_ld_operand(l, 0);
                self.spill_complex_ld_operand(r, 32);
                // 8 次 pushq 传参。最后 push 的 qword 落在最低地址（=首个
                // 入参 a.real 的低字），故 push 顺序须为
                //   b.imag(hi,lo) → b.real(hi,lo) → a.imag(hi,lo) → a.real(hi,lo)
                // 每个 tbyte 高 qword 先 push、低 qword 后 push（与 gcc
                // -O0 的 push -8,-16,...,-64(%rbp) 序列一致）。
                // 关键：每 push 一次 rsp 下移 8，剩余源槽相对*当前* rsp 的
                // 位移增加 8。第 k 次（0-based）push 读取 disp + k*8，
                // 展开后每次都读当前 rsp+56（即数据随 push 向下移动）。
                // 各 tbyte 高/低 qword 相对 sub 后 rsp 的初始偏移：
                let pairs: [(u8, u8); 4] = [
                    (56, 48), // b.imag: hi@56 lo@48
                    (40, 32), // b.real: hi@40 lo@32
                    (24, 16), // a.imag: hi@24 lo@16
                    (8, 0),   // a.real: hi@8  lo@0
                ];
                let mut k = 0u8;
                for (hi, lo) in pairs {
                    self.emit(&[0xFF, 0x74, 0x24, hi + k * 8]); // push hi
                    k += 1;
                    self.emit(&[0xFF, 0x74, 0x24, lo + k * 8]); // push lo
                    k += 1;
                }
                self.emit_call_extern(helper);
                self.emit(&[0x48, 0x83, 0xC4, 0x40]); // add rsp, 64
                                                      // 返回 st0=real, st1=imag（fadd 分支同序，统一收尾）
            },
            _ => return,
        }
        // 统一收尾：两个分支结束时都是 st0=real、st1=imag（add/sub 已用
        // fxch 归一化；__mulxc3/__divxc3 按规范即此序）。先 fstpt real
        // （弹栈后 imag 升到 st0），再 fstpt imag。
        self.emit_x87_fstp_at(13, result_off as i32, &Type::LongDouble);
        self.emit_x87_fstp_at(13, result_off as i32 + 16, &Type::LongDouble);
        self.emit(&[0x48, 0x83, 0xC4, 0x40]); // 释放 64 字节运算区
        self.emit_lea_r13(0, result_off); // rax = 结果区地址
    }

    /// complex LD 的 == / !=：实部与虚部分别 fucomip，皆等才算相等。
    /// 左右表达式各单次物化成帧内一对 tbyte（带副作用 rvalue 只求值一次），
    /// 每个分量比较后 x87 栈平衡（fucomip 弹右值，再 fstp 弹左值）。
    fn gen_complex_ld_compare(&mut self, op: BinOp, l: &Expr, r: &Expr, dst: u8) {
        let false_label = self.fresh_label();
        let end_label = self.fresh_label();
        let ls = self.gen_complex_ld_materialize(l);
        let rs = self.gen_complex_ld_materialize(r);
        for is_real in [true, false] {
            let off = if is_real { 0 } else { 16 };
            // 左值入栈后右值入栈: st1=left, st0=right
            self.emit_x87_fld_at(13, (ls + off) as i32, &Type::LongDouble);
            self.emit_x87_fld_at(13, (rs + off) as i32, &Type::LongDouble);
            // fucomip st0, st1 (DF E9): 比较 st1 vs st0, 弹 st0
            self.emit(&[0xDF, 0xE9]);
            self.jmp_rel32(&false_label, JumpKind::Jne);
            self.emit_x87_fstp_st0(); // 弹掉左值
        }
        self.mov_imm(dst, if matches!(op, BinOp::EqEq) { 1 } else { 0 });
        self.jmp_rel32(&end_label, JumpKind::Jmp);
        self.label(&false_label);
        // 任一不等时该分量的左值还在栈上（fucomip 已弹右值），弹掉它
        // 以把 x87 栈恢复到比较前状态。
        self.emit_x87_fstp_st0();
        self.mov_imm(dst, if matches!(op, BinOp::EqEq) { 0 } else { 1 });
        self.label(&end_label);
    }

    pub(crate) fn gen_complex_imag_into_xmm0(&mut self, r: &Expr, inner: &Type) {
        let r_inner = match r.ty() {
            Type::Complex(ref c) | Type::Imaginary(ref c) => Some(c.base.clone()),
            _ => None,
        };
        // __builtin_conj 系列内联物化在 result_off (与 binop 同布局),
        // Call 返回也统一物化在 result_off — 两者都走下方内存对读取。
        // (旧实现对非 conj Call 读 xmm1 虚部: 对 SSE 类返回碰巧成立,
        // 对整数 _Complex (rax/rdx 返回) 是垃圾, complex-6 test_int。)
        if matches!(r.ty(), Type::Complex(_)) {
            // 物化槽偏移与位模式按源内型 (而非目标 inner)
            let ri = r_inner.unwrap_or_else(|| inner.clone());
            let rpfx = if matches!(ri, Type::Float) { 0xF3u8 } else { 0xF2u8 };
            let r_isz = ri.size() as i64;
            let result_off = self.complex_result_off();
            self.emit_xmm0_r13_disp(&[rpfx, 0x41, 0x0F, 0x10], result_off + r_isz);
            self.emit_float_convert(&ri, inner);
        } else {
            self.emit(&[0x0F, 0x57, 0xC0]); // xorps xmm0, xmm0 (标量: 虚部 0)
        }
    }
    pub(crate) fn gen_complex_init(&mut self, name: &str, inner: &Type, init: &Expr) {
        let off = self.var_offset(name);
        let inner_sz = inner.size() as i64;
        // 虚数字面量 (4.0i / 3j): 实部+虚部都来自字面量, 通用路径
        // (gen_expr 后只存实部并清零虚部) 无法表达, 直接分存两分量
        if let Expr::ComplexLit { real, imag, .. } = init {
            if matches!(inner, Type::Float | Type::Double | Type::LongDouble) {
                self.gen_float_imm_load(*real, inner);
                self.gen_float_store_at(off, inner);
                self.gen_float_imm_load(*imag, inner);
                // 虚部槽统一连续布局 [off+inner_sz]。旧 LongDouble 向后
                // 写 off-8-inner_sz, 越出 register_var 窗口 [off, off+32)
                // 踩相邻变量, 多变量时虚部互相覆盖 (c2 虚部丢失)。
                let imag_base = off + inner_sz;
                self.gen_float_store_at(imag_base, inner);
            } else {
                // 整数内型: _Complex int z = 3i;
                self.mov_imm(0, real.as_f64() as i128);
                self.store_stack(0, off);
                self.mov_imm(0, imag.as_f64() as i128);
                self.store_stack(0, off + inner_sz);
            }
            return;
        }
        // 整型复数字面量: 分量是精确 i128，按 inner 宽度直存两槽。
        if let Expr::ComplexIntLit { real, imag, .. } = init {
            self.mov_imm(0, *real);
            self.emit_store(0, off, inner_sz as usize);
            self.mov_imm(0, *imag);
            self.emit_store(0, off + inner_sz, inner_sz as usize);
            return;
        }
        // `_Complex long double` 初始化: 全程 x87。经 materialize 单次
        // 物化把分量取到帧内 tbyte 对（自动覆盖字面量/binop/Call
        // rvalue/lvalue/标量/虚数源，并按源内型读槽、硬件无损拓宽窄源），
        // fstp tbyte 落到目标 real@off、imag@off+16（每分量 16 字节槽）。
        // 不走下方 SSE movsd 路径——它只搬 8 字节，LD 高位与第二分量皆错
        // （局部 complex LD 变量出垃圾的直接链路）。
        if matches!(inner, Type::LongDouble) {
            // 源只求值一次，物化成帧内 real/imag tbyte 对，再搬到目标 real@off、
            // imag@off+16（每分量 16 字节槽）。窄源由物化器硬件无损拓宽。
            let src = self.gen_complex_ld_materialize(init);
            self.emit_x87_fld_at(13, src as i32, &Type::LongDouble);
            self.emit_x87_fstp_at(13, off as i32, &Type::LongDouble);
            self.emit_x87_fld_at(13, src as i32 + 16, &Type::LongDouble);
            self.emit_x87_fstp_at(13, (off + inner_sz) as i32, &Type::LongDouble);
            return;
        }
        self.gen_expr(init, 0);
        let init_is_complex = matches!(init.ty(), Type::Complex(_));
        // 源内型宽度: init 可能窄于目标 (_Complex int 字面量表达式 →
        // _Complex long 变量, complex-6 test_long_int)。源/目标分量
        // 布局偏移与符号扩展都按源宽度计算。
        let src_inner_sz = match init.ty() {
            // Complex 的 size() 是总宽 (inner*2), 分量宽度取 inner 宽
            // (虚部偏移 result_off+src_inner_sz 才是 imag 槽)
            Type::Complex(ref ci) => ci.base.size() as i64,
            Type::Imaginary(ref ci) => ci.size() as i64,
            _ => inner_sz,
        };
        // 存实部: 统一从 result_off 物化区读 (与整数路径一致)
        // 旧实现信任 gen_expr 后实部在 xmm0 — 对 binop/字面量成立
        // (尾部重载), 对 sret Call 返回 (Memory 类, xmm0 未定义)
        // 不成立: rr = cl2(x) 实部被垃圾污染 (B 打印 0.0)。
        if matches!(inner, Type::Float | Type::Double | Type::LongDouble) {
            let result_off = self.complex_result_off();
            if init_is_complex {
                // 读取宽度按源内型: 产出端 (binop 等) 按运算 inner 写
                // result_off 位模式, 目标更窄 (Float) 时按 movss 读低
                // 32 位得 0 (1.0d 低 32 位), 须按源宽度读再 cvtsd2ss
                // (complex-6 test_float)
                let src_inner = match init.ty() {
                    Type::Complex(ref ci) | Type::Imaginary(ref ci) => ci.base.clone(),
                    _ => inner.clone(),
                };
                let rpfx = if matches!(src_inner, Type::Float) {
                    0xF3u8
                } else {
                    0xF2u8
                };
                self.emit_xmm0_r13_disp(&[rpfx, 0x41, 0x0F, 0x10], result_off);
                self.emit_float_convert(&src_inner, inner);
            } else {
                // 标量 init: 浮点值在 xmm0 (按源宽度), 整数在 rax —
                // 转换到目标 inner 后作实部 (虚部下方清零)。旧实现按
                // dst inner 读 result_off / 直接 movq 写 xmm0 位模式,
                // 对 Float 目标 + Double 标量 (1.5d 低 32 位 = 0) 恒错。
                self.emit_scalar_to_xmm0_if_needed(init, inner);
            }
            self.gen_float_store_at(off, inner);
        } else {
            // 整数 rvalue 统一物化在 result_off (rax=地址约定):
            // 实部/虚部均从内存对读取, 不信任 rax/rdx 寄存器内容。
            // 源 4 字节时 result_off 低 8 字节为实部+虚部打包,
            // movsxd 取实部符号扩展 (_Complex int → _Complex long)。
            let result_off = self.complex_result_off();
            if !init_is_complex {
                // 标量 init: gen_expr 后 rax = 值 (非 result_off 地址
                // 约定), 旧路径直接 load_stack(result_off) 丢弃 rax,
                // 实部为残留垃圾 (_Complex int cj = 3 实部错)。先物化
                // (值, 0) 同宽布局, 复用下方统一读取。
                if matches!(init.ty(), Type::Float | Type::Double | Type::LongDouble) {
                    // 浮点标量源: 值在 xmm0, 截断为整数
                    self.emit(&[0xF2, 0x48, 0x0F, 0x2C, 0xC0]); // cvttsd2si
                }
                let s_isz = src_inner_sz as usize;
                self.emit_store(0, result_off, s_isz);
                self.emit(&[0x48, 0x31, 0xC0]); // xor rax, rax (虚部 = 0)
                self.emit_store(0, result_off + src_inner_sz, s_isz);
            }
            self.load_stack(0, result_off);
            if src_inner_sz == 4 && inner_sz == 8 {
                self.emit(&[0x48, 0x63, 0xC0]); // movsxd rax, eax
            }
            let sz = inner.size();
            self.store_stack(0, off);
            if sz > 8 {
                self.load_stack(2, result_off + 8);
                self.store_stack(2, off + 8);
            }
        }
        // 虚部: complex rvalue 从物化位置取 (binop 结果/调用返回值),
        // 标量初始化保持清零
        let imag_base = off + inner_sz;
        let imag_bytes = inner_sz as usize;
        if matches!(inner, Type::Float | Type::Double | Type::LongDouble) {
            if init_is_complex {
                self.gen_complex_imag_into_xmm0(init, inner);
            } else {
                // 浮点虚部仅 inner_sz 字节，勿用 64 位 store 以免覆盖相邻实部
                self.emit(&[0x0F, 0x57, 0xC0]); // xorps xmm0, xmm0
            }
            self.gen_float_store_at(imag_base, inner);
            let _ = imag_bytes;
        } else {
            if init_is_complex {
                let result_off = self.complex_result_off();
                // 虚部按源内型偏移读取 (源 4 字节时在 result_off+4,
                // 而非目标的 inner_sz)
                self.load_stack(0, result_off + src_inner_sz);
                if src_inner_sz == 4 && inner_sz == 8 {
                    self.emit(&[0x48, 0x63, 0xC0]); // movsxd rax, eax
                }
            } else {
                self.emit(&[0x48, 0x31, 0xC0]); // xor rax, rax
            }
            // 整数虚部: 按内型宽度存储, 勿用 64 位 store 覆盖相邻实部
            // (如 y.imag 槽 = y.off+4, 8 字节 store 会踩到下一个变量的实部)
            match inner.size() {
                1 | 2 | 4 => self.emit_store(0, imag_base, inner.size()),
                _ => {
                    for o in (0..imag_bytes).step_by(8) {
                        self.store_stack(0, imag_base + o as i64);
                    }
                },
            }
        }
    }
    fn gen_complex_int_div(&mut self, inner_sz: i64, total_sz: i64, inner_ty: &Type) {
        let is_signed = Self::is_signed_int(inner_ty);
        let lc = self.fresh_label();
        let ld_ = self.fresh_label();
        let lend = self.fresh_label();
        // 预载 a/b (宽度扩展): r10 = a, r11 = b
        match inner_sz {
            8 => {
                self.emit(&[0x4C, 0x8B, 0x54, 0x24, 0x00]); // mov r10, [rsp+0]
                self.emit(&[0x4C, 0x8B, 0x5C, 0x24, inner_sz as u8]); // r11 = b
            },
            4 => {
                self.emit(&[0x44, 0x8B, 0x54, 0x24, 0x00]); // mov r10d, [rsp+0]
                self.emit(&[0x44, 0x8B, 0x5C, 0x24, inner_sz as u8]);
            },
            2 => {
                let ld = if is_signed { 0xBF } else { 0xB7 };
                self.emit(&[0x44, 0x0F, ld, 0x54, 0x24, 0x00]);
                self.emit(&[0x44, 0x0F, ld, 0x5C, 0x24, inner_sz as u8]);
            },
            _ => {
                let ld = if is_signed { 0xBE } else { 0xB6 };
                self.emit(&[0x44, 0x0F, ld, 0x54, 0x24, 0x00]);
                self.emit(&[0x44, 0x0F, ld, 0x5C, 0x24, inner_sz as u8]);
            },
        }
        // 分支选择: |c| > |d| → lc
        if inner_sz == 8 {
            self.emit(&[0x48, 0x8B, 0x44, 0x24, total_sz as u8]); // rax = c
            self.emit(&[0x48, 0x8B, 0x54, 0x24, (total_sz + inner_sz) as u8]); // rdx = d
            if is_signed {
                // |rax| = (rax^(rax>>63)) - (rax>>63)
                self.emit(&[0x48, 0x89, 0xC1]); // mov rcx, rax
                self.emit(&[0x48, 0xC1, 0xF9, 0x3F]); // sar rcx, 63
                self.emit(&[0x48, 0x31, 0xC8]); // xor rax, rcx
                self.emit(&[0x48, 0x29, 0xC8]); // sub rax, rcx
                self.emit(&[0x48, 0x89, 0xD1]); // mov rcx, rdx
                self.emit(&[0x48, 0xC1, 0xF9, 0x3F]);
                self.emit(&[0x48, 0x31, 0xCA]); // xor rdx, rcx
                self.emit(&[0x48, 0x29, 0xCA]); // sub rdx, rcx
            }
            self.emit(&[0x48, 0x39, 0xD0]); // cmp rax, rdx
        } else {
            // eax = |c|, edx = |d| (inner<4 时按宽度扩展加载)
            match inner_sz {
                4 => {
                    self.emit(&[0x8B, 0x44, 0x24, total_sz as u8]);
                    self.emit(&[0x8B, 0x54, 0x24, (total_sz + inner_sz) as u8]);
                },
                2 => {
                    let ld = if is_signed { 0xBF } else { 0xB7 };
                    self.emit(&[0x0F, ld, 0x44, 0x24, total_sz as u8]);
                    self.emit(&[0x0F, ld, 0x54, 0x24, (total_sz + inner_sz) as u8]);
                },
                _ => {
                    let ld = if is_signed { 0xBE } else { 0xB6 };
                    self.emit(&[0x0F, ld, 0x44, 0x24, total_sz as u8]);
                    self.emit(&[0x0F, ld, 0x54, 0x24, (total_sz + inner_sz) as u8]);
                },
            }
            if is_signed {
                self.emit(&[0x89, 0xC1]); // mov ecx, eax
                self.emit(&[0xC1, 0xF9, 0x1F]); // sar ecx, 31
                self.emit(&[0x31, 0xC8]); // xor eax, ecx
                self.emit(&[0x29, 0xC8]); // sub eax, ecx
                self.emit(&[0x89, 0xD1]); // mov ecx, edx
                self.emit(&[0xC1, 0xF9, 0x1F]);
                self.emit(&[0x31, 0xCA]); // xor edx, ecx
                self.emit(&[0x29, 0xCA]); // sub edx, ecx
            }
            self.emit(&[0x39, 0xD0]); // cmp eax, edx
        }
        self.jmp_rel32(&lc, JumpKind::Ja);
        // ---- |d| >= |c| 分支: r = c/d ----
        self.label(&ld_);
        if inner_sz == 8 {
            self.emit(&[0x48, 0x8B, 0x44, 0x24, total_sz as u8]); // rax = c
            self.emit(&[0x48, 0x8B, 0x4C, 0x24, (total_sz + inner_sz) as u8]); // rcx = d
        } else {
            self.emit_c_ld32(inner_sz, total_sz, total_sz + inner_sz, is_signed);
            // eax=c, ecx=d
        }
        if is_signed {
            self.emit_cqo(inner_sz);
        } else {
            self.emit_xor_edx(inner_sz);
        }
        self.emit_div_rm(inner_sz, is_signed, 0xF1); // idiv/div ecx
        self.emit(&[0x89, 0xC6]); // mov esi, eax (r)
                                  // den = d + r*c
        self.emit(&[0x89, 0xF0]); // mov eax, esi
        self.emit_imul_rm(inner_sz, total_sz); // imul rax, [rsp+total]
        self.emit_add_rm(inner_sz, total_sz + inner_sz); // add, [rsp+ti]
        self.emit(&[0x89, 0xC7]); // mov edi, eax (den)
                                  // real = (a*r + b) / den
        self.emit(&[0x89, 0xF0]); // mov eax, esi
        self.emit_imul_r10(inner_sz); // imul rax, r10
        self.emit_add_r11(inner_sz); // add rax, r11
        if is_signed {
            self.emit_cqo(inner_sz);
        } else {
            self.emit_xor_edx(inner_sz);
        }
        self.emit_div_rm(inner_sz, is_signed, 0xF7); // idiv/div edi
        self.emit_store_rsp0(inner_sz); // store real
                                        // imag = (b*r - a) / den
        self.emit(&[0x89, 0xF0]); // mov eax, esi
        self.emit_imul_r11(inner_sz); // imul rax, r11
        self.emit_mov_rcx_r10(inner_sz); // mov rcx, r10
        self.emit_sub_rax_rcx(inner_sz); // sub rax, rcx
        if is_signed {
            self.emit_cqo(inner_sz);
        } else {
            self.emit_xor_edx(inner_sz);
        }
        self.emit_div_rm(inner_sz, is_signed, 0xF7);
        self.emit_store_rspi(inner_sz); // store imag
        self.jmp_rel32(&lend, JumpKind::Jmp);
        // ---- |c| > |d| 分支: r = d/c ----
        self.label(&lc);
        if inner_sz == 8 {
            self.emit(&[0x48, 0x8B, 0x44, 0x24, (total_sz + inner_sz) as u8]); // rax = d
            self.emit(&[0x48, 0x8B, 0x4C, 0x24, total_sz as u8]); // rcx = c
        } else {
            self.emit_c_ld32(inner_sz, total_sz + inner_sz, total_sz, is_signed);
            // eax=d, ecx=c
        }
        if is_signed {
            self.emit_cqo(inner_sz);
        } else {
            self.emit_xor_edx(inner_sz);
        }
        self.emit_div_rm(inner_sz, is_signed, 0xF1); // idiv/div ecx
        self.emit(&[0x89, 0xC6]); // mov esi, eax (r)
                                  // den = c + r*d
        self.emit(&[0x89, 0xF0]); // mov eax, esi
        self.emit_imul_rm(inner_sz, total_sz + inner_sz); // imul rax, [rsp+ti]
        self.emit_add_rm(inner_sz, total_sz); // add, [rsp+total]
        self.emit(&[0x89, 0xC7]); // mov edi, eax (den)
                                  // real = (r*b + a) / den
        self.emit(&[0x89, 0xF0]); // mov eax, esi
        self.emit_imul_r11(inner_sz); // imul rax, r11
        self.emit_add_r10(inner_sz); // add rax, r10
        if is_signed {
            self.emit_cqo(inner_sz);
        } else {
            self.emit_xor_edx(inner_sz);
        }
        self.emit_div_rm(inner_sz, is_signed, 0xF7);
        self.emit_store_rsp0(inner_sz); // store real
                                        // imag = (b - r*a) / den
        self.emit(&[0x89, 0xF0]); // mov eax, esi
        self.emit_imul_r10(inner_sz); // imul rax, r10 (r*a)
        self.emit_mov_rcx_r11(inner_sz); // mov rcx, r11 (b)
        self.emit_sub_rcx_rax(inner_sz); // sub rcx, rax
        if inner_sz == 8 {
            self.emit(&[0x48, 0x89, 0xC8]); // mov rax, rcx
        } else {
            self.emit(&[0x89, 0xC8]); // mov eax, ecx
        }
        if is_signed {
            self.emit_cqo(inner_sz);
        } else {
            self.emit_xor_edx(inner_sz);
        }
        self.emit_div_rm(inner_sz, is_signed, 0xF7);
        self.emit_store_rspi(inner_sz); // store imag
        self.label(&lend);
    }
    fn gen_complex_int_mul(&mut self, inner_sz: i64, total_sz: i64, inner_ty: &Type) {
        if inner_sz == 8 {
            // rax=a, rcx=c, rdx=b, rsi=d
            self.emit(&[0x48, 0x8B, 0x04, 0x24]);
            self.emit(&[0x48, 0x8B, 0x4C, 0x24, total_sz as u8]);
            self.emit(&[0x48, 0x8B, 0x54, 0x24, inner_sz as u8]);
            self.emit(&[0x48, 0x8B, 0x74, 0x24, (total_sz + inner_sz) as u8]);
            self.emit(&[0x48, 0x89, 0xC7]); // rdi = a
            self.emit(&[0x48, 0x0F, 0xAF, 0xF9]); // imul rdi, rcx (ac)
            self.emit(&[0x49, 0x89, 0xD0]); // mov r8, rdx (b)
            self.emit(&[0x4C, 0x0F, 0xAF, 0xC6]); // imul r8, rsi (bd)
            self.emit(&[0x4C, 0x29, 0xC7]); // sub rdi, r8 (real)
            self.emit(&[0x49, 0x89, 0xC0]); // mov r8, rax (a)
            self.emit(&[0x4C, 0x0F, 0xAF, 0xC6]); // imul r8, rsi (ad)
            self.emit(&[0x48, 0x89, 0xD0]); // rax = b
            self.emit(&[0x48, 0x0F, 0xAF, 0xC1]); // imul rax, rcx (bc)
            self.emit(&[0x4C, 0x01, 0xC0]); // add rax, r8 (imag)
            self.emit(&[0x48, 0x89, 0x3C, 0x24]); // mov [rsp+0], rdi
            self.emit(&[0x48, 0x89, 0x44, 0x24, inner_sz as u8]);
        } else {
            let (lsz, ld_s, ld_u) = match inner_sz {
                4 => (0x8Bu8, 0u8, 0u8),
                2 => (0x0Fu8, 0xBFu8, 0xB7u8), // movsx/movzx r32, m16
                _ => (0x0Fu8, 0xBEu8, 0xB6u8), // movsx/movzx r32, m8
            };
            let ld = if Self::is_signed_int(inner_ty) { ld_s } else { ld_u };
            let t = total_sz as u8;
            let i = inner_sz as u8;
            let ti = (total_sz + inner_sz) as u8;
            if inner_sz == 4 {
                self.emit(&[lsz, 0x04, 0x24]); // eax = a
                self.emit(&[lsz, 0x4C, 0x24, t]); // ecx = c
                self.emit(&[lsz, 0x54, 0x24, i]); // edx = b
                self.emit(&[lsz, 0x74, 0x24, ti]); // esi = d
            } else {
                self.emit(&[0x0F, ld, 0x04, 0x24]);
                self.emit(&[0x0F, ld, 0x4C, 0x24, t]);
                self.emit(&[0x0F, ld, 0x54, 0x24, i]);
                self.emit(&[0x0F, ld, 0x74, 0x24, ti]);
            }
            self.emit(&[0x89, 0xC7]); // edi = a
            self.emit(&[0x0F, 0xAF, 0xF9]); // imul edi, ecx (ac)
            self.emit(&[0x41, 0x89, 0xD0]); // mov r8d, edx (b)
            self.emit(&[0x44, 0x0F, 0xAF, 0xC6]); // imul r8d, esi (bd)
            self.emit(&[0x44, 0x29, 0xC7]); // sub edi, r8d (real)
            self.emit(&[0x41, 0x89, 0xC0]); // r8d = a
            self.emit(&[0x44, 0x0F, 0xAF, 0xC6]); // imul r8d, esi (ad)
            self.emit(&[0x89, 0xD0]); // eax = b
            self.emit(&[0x0F, 0xAF, 0xC1]); // imul eax, ecx (bc)
            self.emit(&[0x44, 0x01, 0xC0]); // add eax, r8d (imag)
                                            // 存回 (截断到分量宽度)
            match inner_sz {
                4 => {
                    self.emit(&[0x89, 0x3C, 0x24]); // mov [rsp+0], edi
                    self.emit(&[0x89, 0x44, 0x24, i]);
                },
                2 => {
                    self.emit(&[0x66, 0x89, 0x3C, 0x24]);
                    self.emit(&[0x66, 0x89, 0x44, 0x24, i]);
                },
                _ => {
                    self.emit(&[0x88, 0x3C, 0x24]);
                    self.emit(&[0x88, 0x44, 0x24, i]);
                },
            }
        }
    }
    pub(crate) fn gen_complex_lvalue_rvalue(&mut self, e: &Expr) -> bool {
        let ty = e.ty();
        let inner = match &ty {
            Type::Complex(inner) => inner.base.clone(),
            _ => return false,
        };
        if !self.gen_lvalue_addr(e, 0) {
            return false;
        }
        let isz = inner.size() as i64;
        let result_off = self.complex_result_off();
        if let Type::LongDouble = &inner {
            // LD: 源在 [rax+0]/[rax+16]（每分量 16 字节 tbyte 槽），逐分量
            // fld tbyte 起、fstp tbyte 物化到 result_off/result_off+16。
            // rax（lvalue 地址）在整个序列保持不变。
            self.emit_x87_fld_at(0, 0, &inner);
            self.emit_x87_fstp_at(13, result_off as i32, &inner);
            self.emit_x87_fld_at(0, isz as i32, &inner);
            self.emit_x87_fstp_at(13, (result_off + isz) as i32, &inner);
        } else {
            match &inner {
                t @ (Type::Float | Type::Double) => {
                    let pfx = if matches!(t, Type::Float) { 0xF3u8 } else { 0xF2u8 };
                    // 实部: xmm0 = [rax] → result_off
                    self.emit(&[pfx, 0x0F, 0x10, 0x00]);
                    self.gen_float_store_at(result_off, t);
                    // 虚部: xmm0 = [rax+isz] → result_off+isz
                    self.emit_load_mem_typed_disp(isz as usize, t, isz);
                    self.gen_float_store_at(result_off + isz, t);
                    // xmm0 = 实部
                    self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], result_off);
                },
                _ => {
                    // 整数内型: 分量按类型宽度读取.
                    // 布局: 实部 @ result_off, 虚部 @ result_off + isz
                    // (与全局 result_off + inner_sz 约定一致).
                    // 使用 emit_store(isz) 而非 store_stack(8B), 避免
                    // isz < 8 时 8B 写覆盖相邻虚部槽.
                    self.push(0); // save base address (&complex_var)
                    self.emit_load_mem_typed(isz as usize, &inner);
                    self.emit_store(0, result_off, isz as usize);
                    self.pop(0); // restore base address
                    self.emit_load_mem_typed_disp(isz as usize, &inner, isz);
                    self.emit_store(0, result_off + isz, isz as usize);
                },
            }
        }
        // rax = result_off (dcc complex rvalue 约定, 供 spill/整体拷贝)
        self.emit_lea_r13(0, result_off);
        true
    }
    pub(crate) fn gen_complex_part(&mut self, e: &Expr, is_real: bool, ty: &Type, dst: u8) {
        // _Imaginary 类型: __real__ 返回 0, __imag__ 返回值本身
        if let Type::Imaginary(ref inner) = e.ty() {
            if is_real {
                // __real__ imaginary → 0
                if matches!(&inner.base, Type::Float | Type::Double | Type::LongDouble) {
                    if matches!(&inner.base, Type::Float) {
                        self.emit(&[0x0F, 0x57, 0xC0]); // xorps xmm0, xmm0
                    } else {
                        self.emit(&[0x66, 0x0F, 0x57, 0xC0]); // xorpd xmm0, xmm0
                    }
                } else {
                    self.mov_imm(dst, 0);
                }
            } else {
                // __imag__ imaginary → 值本身
                self.gen_expr(e, dst);
            }
            return;
        }
        let inner = match &e.ty() {
            Type::Complex(i) => i.base.clone(),
            _ => Type::Double, // 标量路径占位
        };
        if !matches!(e.ty(), Type::Complex(_)) {
            // 标量: __real__ = 自身, __imag__ = 0
            if is_real {
                self.gen_expr(e, dst);
            } else if matches!(ty, Type::Float | Type::Double | Type::LongDouble) {
                self.emit(&[0x0F, 0x57, 0xC0]); // xorps xmm0, xmm0
            } else {
                self.mov_imm(dst, 0);
            }
            return;
        }
        let inner_sz = inner.size() as i64;
        let is_float = matches!(inner, Type::Float | Type::Double | Type::LongDouble);
        // 分量在变量/字段内存中的偏移: 统一连续布局, 虚部槽 = off+inner_sz
        // (旧 LongDouble 向后 -8-inner_sz 与整体拷贝窗口 [off, off+size) 错位)
        let part_off = if is_real { 0 } else { inner_sz };
        let lva_ok = self.gen_lvalue_addr(e, 0);
        if lva_ok {
            // lvalue 形态 (变量/字段/下标/解引用): 直接对内存分量寻址
            if matches!(inner, Type::LongDouble) {
                // long double 分量必须经 x87 `fld tbyte`（part_off 是 0
                // 或 16 的 16 字节槽偏移）；发 movsd 只读 8 字节当
                // f64 位型，st0/xmm 读侧取回即 -nan。
                self.emit_x87_fld_at(0, part_off as i32, &inner);
            } else if is_float {
                let pfx = if matches!(inner, Type::Float) { 0xF3u8 } else { 0xF2u8 };
                self.emit(&[pfx, 0x0F, 0x10]); // movss/movsd xmm0, [rax+disp]
                if part_off == 0 {
                    self.emit(&[0x00]);
                } else if part_off.abs() < 128 {
                    self.emit(&[0x40, part_off as u8]);
                } else {
                    self.emit(&[0x80]);
                    self.emit(&(part_off as i32).to_le_bytes());
                }
            } else {
                // 整数内型: 先加分量偏移, 再按 size 加载到 rax
                if part_off != 0 {
                    self.emit(&[0x48, 0x05]);
                    self.emit(&(part_off as i32).to_le_bytes());
                }
                let sz = inner.size();
                match sz {
                    1 => self.emit(&[0x0F, 0xB6, 0x00]), // movzx eax, byte [rax]
                    2 => {
                        self.emit(&[0x66, 0x8B, 0x00]);
                        self.emit(&[0x48, 0x0F, 0xB7, 0xC0]);
                    },
                    4 => self.emit(&[0x8B, 0x00]), // mov eax, [rax]
                    _ => self.emit(&[0x48, 0x8B, 0x00]),
                }
            }
        } else if is_float {
            // 非 lvalue complex 表达式 (binop/取负/字面量/Call): rvalue
            // 统一物化在 result_off 内存对。不能信任寄存器残留:
            // Call sret 返回 (Memory 类) 后 xmm0 未定义。
            self.gen_expr(e, 0);
            let result_off = self.complex_result_off();
            if matches!(inner, Type::LongDouble) {
                // LD: 实部/虚部各在 16 字节 tbyte 槽 (result_off / +16),
                // fld tbyte 把该分量留在 st(0)（x87 标量通道）。
                let read_off = if is_real { result_off } else { result_off + inner_sz };
                self.emit_x87_fld_at(13, read_off as i32, &inner);
            } else {
                let pfx = if matches!(inner, Type::Float) { 0xF3u8 } else { 0xF2u8 };
                let read_off = if is_real { result_off } else { result_off + inner_sz };
                self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], read_off);
            }
        } else {
            // 整数内型非 lvalue (如 3i + 5i): 实部/虚部在帧临时区
            self.gen_expr(e, 0);
            let result_off = self.complex_result_off();
            let off = if is_real { result_off } else { result_off + inner_sz };
            self.load_stack(0, off);
        }
        // ── 分量类型 → 表达式类型 的标量转换 ────────────────────────────────
        // `creal(z)` / `cimag(z)` 在 C 里是**函数** `double creal(double
        // _Complex)`: 实参先做复数算术转换, 结果再按返回类型交付。本函数把它
        // 们内联成 `__real__`/`__imag__` 求值, 省掉的是调用, 不能省掉那两层
        // 类型转换 —— 否则 `_Complex float` 的 4 字节位模式被下游当成 double
        // (`cimag(1.0f+1.0fi)` 读出 5.26e-315 而非 1.0)。真值通道只有两条:
        // 浮点分量在 xmm0, 整数分量在 rax; LongDouble 分量在 st(0) 且其
        // f80↔f64 折算需要 x87 侧独立支持, 两侧任一为 LongDouble 时不折算
        // (`creall` 的返回类型就是 long double, 本就无需折算)。
        if !matches!(inner, Type::LongDouble) && !matches!(ty, Type::LongDouble) {
            let src_float = matches!(inner, Type::Float | Type::Double);
            let dst_float = matches!(ty, Type::Float | Type::Double);
            match (src_float, dst_float) {
                (true, true) => self.emit_float_convert(&inner, ty),
                (false, true) => {
                    // 整数分量 → 浮点返回: cvtsi2s{s,d} xmm0, eax/eax(64)
                    let pfx = if matches!(ty, Type::Float) { 0xF3u8 } else { 0xF2u8 };
                    if inner.size() >= 8 {
                        self.emit(&[pfx, 0x48, 0x0F, 0x2A, 0xC0]);
                    } else {
                        self.emit(&[pfx, 0x0F, 0x2A, 0xC0]);
                    }
                },
                (true, false) => {
                    // 浮点分量 → 整数返回: C6.3.1.7 向零截断
                    let pfx = if matches!(inner, Type::Float) { 0xF3u8 } else { 0xF2u8 };
                    self.emit(&[pfx, 0x48, 0x0F, 0x2C, 0xC0]);
                },
                (false, false) => {},
            }
        }
        if dst != 0 {
            self.mov(dst, 0);
        }
    }
    /// `_Complex long double` 经 x87 返回：SysV 要求 st0=real、st1=imag。
    /// 先单次物化成帧内一对 tbyte（带副作用 rvalue 只求值一次），再按
    /// imag 先入栈、real 后入栈的顺序 fld tbyte，使 st0=real、st1=imag。
    pub(crate) fn gen_complex_x87_return(&mut self, e: &Expr) {
        let src = self.gen_complex_ld_materialize(e);
        self.emit_x87_fld_at(13, src as i32 + 16, &Type::LongDouble); // imag → st0
        self.emit_x87_fld_at(13, src as i32, &Type::LongDouble); // real → st0
    }
    pub(crate) fn gen_complex_sse_return(&mut self, e: &Expr, inner: &Type) {
        let inner_sz = inner.size() as i64;
        let pfx = if matches!(inner, Type::Float) { 0xF3u8 } else { 0xF2u8 };
        let is_float = matches!(inner, Type::Float);
        if matches!(e, Expr::Call { .. }) {
            self.gen_expr(e, 0);
            if is_float {
                // SysV ABI: _Complex float 是单个 8 字节 SSE 类, 实部/虚部
                // 打包在 xmm0 低/高 32 位。Call 物化后 [result_off] 已是
                // [real@0..4][imag@4..8], movq 读出即打包 (旧实现虚部放
                // xmm1 — 那是 _Complex double 的两寄存器分类, float
                // complex 调用方 movsd 只读 xmm0, 虚部丢失
                // complex-6 test_float)。
                let result_off = self.complex_result_off();
                self.emit_xmm0_r13_disp(&[0x66, 0x41, 0x0F, 0x10], result_off);
            }
            return;
        }
        if self.gen_lvalue_addr(e, 0) {
            if is_float {
                // 打包: movss xmm0,[rax] / movss xmm1,[rax+4] /
                // unpcklps xmm0,xmm1 → xmm0 = [real | imag]
                // (注意 unpcklps 无 66 前缀; 66 0F 14 是 unpcklpd,
                // 按 64 位双字打包 → 虚部被挤到 64..96 位, movsd 读丢)
                self.emit(&[0xF3, 0x0F, 0x10, 0x00]);
                self.emit(&[0xF3, 0x0F, 0x10, 0x48, 0x04]);
                self.emit(&[0x0F, 0x14, 0xC1]);
                return;
            }
            self.emit_xmm_rax_disp(pfx, 0, 0); // xmm0 = 实部
                                               // SysV: _Complex double 虚部在独立寄存器 xmm1 (各 8 字节)
            self.emit_xmm_rax_disp(pfx, 1, inner_sz); // xmm1 = 虚部
            return;
        }
        self.gen_expr(e, 0);
        let result_off = self.complex_result_off();
        self.emit_xmm_r13_disp(pfx, 0x10, 0, result_off);
        self.emit_xmm_r13_disp(pfx, 0x10, 1, result_off + inner_sz);
        if is_float {
            // 物化区 [real@0..4][imag@4..8] → 打包 xmm0 (unpcklps: 0F 14)
            self.emit(&[0x0F, 0x14, 0xC1]); // unpcklps xmm0, xmm1
        }
    }
    /// `_Complex` 返回值的交付总入口。先问“源是否已经是目标复数类型的
    /// **对象表示**” ([`Self::complex_src_needs_convert`] 单一真源): 不是则先按
    /// 目标类型物化到帧内临时槽 ([`Self::materialize_value_slot`], 内部就是
    /// 赋值共用的 `gen_aggregate_assign_to`), 再按 SysV 分类装载; 是则直接走
    /// 原有三条同型交付链。本站点只区分“要不要转换”与“怎么装载”,
    /// 不得重写任何分量宽度/符号判据。
    ///
    /// 旧写法在返回上下文里没有隐式转换这条轴 (与形参侧同类缺失), 实测:
    ///  · `cd f(int n){ return n; }` 把 int 的位模式当 double → 0;
    ///  · `cf g(double d){ return d; }` 实部偶然正确, 虚部读穿到隔壁槽;
    ///  · `cd w(cf z){ return z; }` 两个 float 被当成一个 double (1.5→4);
    ///  · 整数内型 `ci f(int n){ return n; }` 交回的是地址而非值。
    pub(crate) fn gen_complex_return(&mut self, e: &Expr, cx_ty: &Type) {
        let inner = match cx_ty {
            Type::Complex(c) => c.base.clone(),
            _ => unreachable!("gen_complex_return 只接受 _Complex 返回类型"),
        };
        if Self::complex_src_needs_convert(cx_ty, e.ty()) {
            let slot = self.materialize_value_slot(cx_ty, e);
            self.emit_complex_return_from_slot(&inner, slot);
            return;
        }
        if matches!(inner, Type::Float | Type::Double) {
            self.gen_complex_sse_return(e, &inner);
        } else if matches!(inner, Type::LongDouble) {
            // SysV: _Complex long double 经 x87 返回, st0=real、st1=imag。
            self.gen_complex_x87_return(e);
        } else {
            // 整数内型 `_Complex`: 8B 在 rax, 16B 在 rax:rdx (复数 rvalue 统一
            // 物化在复数结果带, rax=地址约定不可信)。
            self.gen_expr(e, 0);
            let result_off = self.complex_result_off();
            self.load_stack(0, result_off);
            if cx_ty.size() > 8 {
                self.load_stack(2, result_off + inner.size() as i64);
            }
        }
    }
    /// 从“已是目标内型对象表示”的内存对装载返回寄存器 (仅由
    /// [`Self::gen_complex_return`] 的转换臂调用)。
    fn emit_complex_return_from_slot(&mut self, inner: &Type, off: i64) {
        match inner {
            Type::Float => {
                // `_Complex float` 是**单个** SSE eightbyte: 槽内
                // [real@0..4][imag@4..8] 就是打包好的对象表示, movsd 整体装载。
                self.emit_xmm0_r13_disp(&[0xF2, 0x41, 0x0F, 0x10], off);
            },
            Type::LongDouble => {
                // 先压虚部再压实部 → st0=real、st1=imag。
                self.emit_x87_fld_at(13, off as i32 + 16, &Type::LongDouble);
                self.emit_x87_fld_at(13, off as i32, &Type::LongDouble);
            },
            other => {
                let sz = other.size() as i64;
                if matches!(other, Type::Double) {
                    // SysV: `_Complex double` 两个独立 SSE eightbyte → xmm0/xmm1
                    self.emit_xmm_r13_disp(0xF2, 0x10, 0, off);
                    self.emit_xmm_r13_disp(0xF2, 0x10, 1, off + sz);
                } else {
                    // 整数内型: rax[:rdx]
                    self.load_stack(0, off);
                    if sz * 2 > 8 {
                        self.load_stack(2, off + sz);
                    }
                }
            },
        }
    }
    pub(crate) fn gen_complex_store_rcx(&mut self, r: &Expr, l_ty: &Type) {
        if let Type::Complex(ref inner) = *l_ty {
            let result_off = self.complex_result_off();
            if matches!(&inner.base, Type::Float | Type::Double | Type::LongDouble) {
                // 目标地址由调用方 push 入栈，store_complex_at_rcx 内部会
                // 自行 pop 到 rcx 并对 r 只求值一次（LD 单次物化；f/d 经
                // emit_scalar_to_xmm0_if_needed 单次求值）。此处不得再预
                // gen_expr，否则带副作用 rvalue 被求值两次。
                self.store_complex_at_rcx(r, &inner.base);
                // 统一 rvalue 约定收尾: [rcx] → result_off,
                // xmm0=实部, rax=地址 (赋值表达式值消费端依赖)
                self.mov(6, 1);
                self.emit_lea_r13(7, result_off);
                self.mov_imm(1, (inner.size() * 2) as i128);
                self.emit(&[0xFC, 0xF3, 0xA4]); // cld; rep movsb
                                                // LD rvalue 消费端用 fld tbyte, 不经 xmm0 (见上)。
                if matches!(inner.base, Type::Float | Type::Double) {
                    let pfx = if matches!(inner.base, Type::Float) {
                        0xF3u8
                    } else {
                        0xF2u8
                    };
                    self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], result_off);
                }
                self.emit_lea_r13(0, result_off);
                return;
            }
            let isz = inner.base.size() as i64;
            let result_off = self.complex_result_off();
            self.gen_expr(r, 0);
            self.pop(1); // rcx = 目标地址
            let src_isz = match r.ty() {
                // Complex 的 size() 是总宽 (inner*2), 分量步长取 inner 宽
                Type::Complex(ref c) => c.base.size() as i64,
                Type::Imaginary(ref c) => c.size() as i64,
                _ => 0,
            };
            if src_isz == 0 {
                // 标量 RHS: rax = 值 (浮点源先 cvttsd2si 截断)
                if matches!(r.ty(), Type::Float | Type::Double | Type::LongDouble) {
                    self.emit(&[0xF2, 0x48, 0x0F, 0x2C, 0xC0]); // cvttsd2si
                }
                self.emit_store(0, result_off, isz as usize);
                self.emit(&[0x48, 0x31, 0xC0]); // xor rax, rax (虚部 = 0)
                self.emit_store(0, result_off + isz, isz as usize);
            } else if src_isz == isz {
                // 同宽 complex RHS: 源已物化在 result_off (C99 6.3.1.7)
            } else {
                // 跨宽: 实/虚部分量分别转换 (倒序防自覆盖),
                // 与 gen_aggregate_assign 跨宽分支同策略
                for part in [1usize, 0] {
                    self.emit_lea_r13(0, result_off);
                    self.emit_load_mem_typed_disp(
                        src_isz as usize,
                        &inner.base,
                        (part as i64) * src_isz,
                    );
                    self.store_stack(0, result_off + (part as i64) * isz);
                }
            }
            // result_off → [rcx] 拷贝 2*isz 字节
            self.mov(7, 1); // rdi = rcx (目标)
            self.emit_lea_r13(6, result_off); // rsi = 源
            self.mov_imm(1, (isz * 2) as i128); // rcx = 字节数
            self.emit(&[0xFC]); // cld
            self.emit(&[0xF3, 0xA4]); // rep movsb
                                            // 统一 rvalue 约定: result_off 已含结果, rax=地址
            self.emit_lea_r13(0, result_off);
        }
    }
    pub(crate) fn gen_imaginary_binop(&mut self, op: BinOp, l: &Expr, r: &Expr, dst: u8) {
        let l_imag = matches!(l.ty(), Type::Imaginary(_));
        let r_imag = matches!(r.ty(), Type::Imaginary(_));
        // 确定内型: 取两个操作数中较宽的浮点类型
        let inner_a = if l_imag {
            if let Type::Imaginary(ref i) = l.ty() {
                i.base.clone()
            } else {
                Type::Double
            }
        } else {
            l.ty()
        };
        let inner_b = if r_imag {
            if let Type::Imaginary(ref i) = r.ty() {
                i.base.clone()
            } else {
                Type::Double
            }
        } else {
            r.ty()
        };
        let inner_ty = if inner_a.size() >= inner_b.size() {
            inner_a
        } else {
            inner_b
        };
        let is_float_inner = matches!(inner_ty, Type::Float | Type::Double | Type::LongDouble);
        let pfx = if matches!(inner_ty, Type::Float) {
            0xF3u8
        } else {
            0xF2u8
        };

        if l_imag && r_imag {
            // imag op imag: 两个操作数都是单浮点值
            match op {
                BinOp::Add | BinOp::Sub => {
                    // imag ± imag → imag: 结果仍在 xmm0
                    // 先算 r 保存到 [rsp], 再算 l 到 xmm0, 然后 xmm0 op xmm1
                    self.gen_expr(r, dst);
                    self.emit_scalar_to_xmm0_if_needed(r, &inner_ty);
                    // 保存右操作数
                    self.emit(&[pfx, 0x0F, 0x11, 0x04, 0x24]); // movss/movsd [rsp], xmm0
                    self.gen_expr(l, dst);
                    self.emit_scalar_to_xmm0_if_needed(l, &inner_ty);
                    // 加载右操作数到 xmm1
                    self.emit(&[pfx, 0x0F, 0x10, 0x4C, 0x24, 0x00]); // movss/movsd xmm1, [rsp]
                    let add_sub = if matches!(op, BinOp::Add) { 0x58u8 } else { 0x5Cu8 };
                    self.emit(&[pfx, 0x0F, add_sub, 0xC1]); // addss/subss xmm0, xmm1 = l op r
                                                            // 结果在 xmm0
                },
                BinOp::Mul => {
                    // imag * imag → real (负乘积): (a*i)*(b*i) = -a*b
                    self.gen_expr(l, dst);
                    self.emit_scalar_to_xmm0_if_needed(l, &inner_ty);
                    self.emit(&[pfx, 0x0F, 0x11, 0x04, 0x24]);
                    self.gen_expr(r, dst);
                    self.emit_scalar_to_xmm0_if_needed(r, &inner_ty);
                    self.emit(&[pfx, 0x0F, 0x10, 0x4C, 0x24, 0x00]);
                    self.emit(&[pfx, 0x0F, 0x59, 0xC1]); // mulss/mulsd xmm0, xmm1
                                                         // 取负: xor with sign bit
                    if matches!(inner_ty, Type::Float) {
                        // xorps xmm0, [neg_mask_f32]
                        self.emit_float_neg(&inner_ty);
                    } else {
                        self.emit_float_neg(&inner_ty);
                    }
                    // 结果在 xmm0 (实数值)
                },
                BinOp::Div => {
                    // imag / imag → real: (a*i)/(b*i) = a/b
                    // 先算 r 保存, 再算 l, 确保 xmm0=l, xmm1=r → l/r
                    self.gen_expr(r, dst);
                    self.emit_scalar_to_xmm0_if_needed(r, &inner_ty);
                    self.emit(&[pfx, 0x0F, 0x11, 0x04, 0x24]); // [rsp] = r
                    self.gen_expr(l, dst);
                    self.emit_scalar_to_xmm0_if_needed(l, &inner_ty);
                    self.emit(&[pfx, 0x0F, 0x10, 0x4C, 0x24, 0x00]); // xmm1 = r
                    self.emit(&[pfx, 0x0F, 0x5E, 0xC1]); // divsd xmm0, xmm1 = l/r
                                                         // 结果在 xmm0 (实数值)
                },
                BinOp::EqEq | BinOp::Ne => {
                    // 比较虚部
                    self.gen_expr(l, dst);
                    self.emit_scalar_to_xmm0_if_needed(l, &inner_ty);
                    self.emit(&[pfx, 0x0F, 0x11, 0x04, 0x24]);
                    self.gen_expr(r, dst);
                    self.emit_scalar_to_xmm0_if_needed(r, &inner_ty);
                    self.emit(&[pfx, 0x0F, 0x10, 0x4C, 0x24, 0x00]);
                    if matches!(inner_ty, Type::Float) {
                        self.emit(&[0x0F, 0x2E, 0xC8]); // ucomiss xmm1, xmm0
                    } else {
                        self.emit(&[0x66, 0x0F, 0x2E, 0xC8]); // ucomisd xmm1, xmm0
                    }
                    let false_label = self.fresh_label();
                    let end_label = self.fresh_label();
                    self.jmp_rel32(&false_label, JumpKind::Jne);
                    self.mov_imm(dst, if matches!(op, BinOp::EqEq) { 1 } else { 0 });
                    self.jmp_rel32(&end_label, JumpKind::Jmp);
                    self.label(&false_label);
                    self.mov_imm(dst, if matches!(op, BinOp::EqEq) { 0 } else { 1 });
                    self.label(&end_label);
                    return;
                },
                _ => {},
            }
        } else {
            // 一个 imaginary + 一个 real
            match op {
                BinOp::Add | BinOp::Sub => {
                    // real ± imag → complex: 结果需要存 (real, imag) 对
                    let result_off = self.complex_result_off();
                    let inner_sz = inner_ty.size() as i64;
                    let total_sz = inner_sz * 2;
                    let save_sz = total_sz * 2;
                    self.emit(&[0x48, 0x83, 0xEC, save_sz as u8]);
                    if l_imag && !r_imag {
                        // imag + real: real part = real, imag part = ±imag
                        // 先算实部 (来自 r)
                        self.gen_expr(r, dst);
                        self.emit_scalar_to_xmm0_if_needed(r, &inner_ty);
                        self.emit(&[pfx, 0x0F, 0x11, 0x04, 0x24]); // [rsp+0] = real
                                                                   // 再算虚部 (来自 l)
                        self.gen_expr(l, dst);
                        self.emit_scalar_to_xmm0_if_needed(l, &inner_ty);
                        if matches!(op, BinOp::Sub) {
                            self.emit_float_neg(&inner_ty);
                        }
                        self.emit(&[pfx, 0x0F, 0x11, 0x44, 0x24, inner_sz as u8]);
                    // [rsp+inner] = ±imag
                    } else {
                        // real ± imag: real part = real, imag part = ±imag
                        self.gen_expr(l, dst);
                        self.emit_scalar_to_xmm0_if_needed(l, &inner_ty);
                        self.emit(&[pfx, 0x0F, 0x11, 0x04, 0x24]); // [rsp+0] = real
                        self.gen_expr(r, dst);
                        self.emit_scalar_to_xmm0_if_needed(r, &inner_ty);
                        if matches!(op, BinOp::Sub) {
                            self.emit_float_neg(&inner_ty);
                        }
                        self.emit(&[pfx, 0x0F, 0x11, 0x44, 0x24, inner_sz as u8]);
                        // [rsp+inner] = ±imag
                    }
                    // 加载实部到 xmm0 供后续使用
                    self.emit(&[pfx, 0x0F, 0x10, 0x04, 0x24]);
                    // 将 (real, imag) 对拷贝到帧临时区供后续 complex 操作使用
                    let is_float_store = matches!(inner_ty, Type::Float);
                    self.store_complex_pair_from_rsp(result_off, inner_sz, is_float_store);
                    self.emit(&[0x48, 0x83, 0xC4, save_sz as u8]);
                },
                BinOp::Mul => {
                    // real * imag → imag: 单浮点值乘法
                    self.gen_expr(l, dst);
                    self.emit_scalar_to_xmm0_if_needed(l, &inner_ty);
                    self.emit(&[pfx, 0x0F, 0x11, 0x04, 0x24]);
                    self.gen_expr(r, dst);
                    self.emit_scalar_to_xmm0_if_needed(r, &inner_ty);
                    self.emit(&[pfx, 0x0F, 0x10, 0x4C, 0x24, 0x00]);
                    self.emit(&[pfx, 0x0F, 0x59, 0xC1]); // mulss/mulsd xmm0, xmm1
                },
                BinOp::Div => {
                    // real / imag → imag: -(real/imag_mag)
                    // 先算 r 保存, 再算 l, 确保 xmm0=l, xmm1=r → l/r
                    self.gen_expr(r, dst);
                    self.emit_scalar_to_xmm0_if_needed(r, &inner_ty);
                    self.emit(&[pfx, 0x0F, 0x11, 0x04, 0x24]); // [rsp] = r
                    self.gen_expr(l, dst);
                    self.emit_scalar_to_xmm0_if_needed(l, &inner_ty);
                    self.emit(&[pfx, 0x0F, 0x10, 0x4C, 0x24, 0x00]); // xmm1 = r
                    self.emit(&[pfx, 0x0F, 0x5E, 0xC1]); // divsd xmm0, xmm1 = l/r
                    self.emit_float_neg(&inner_ty);
                },
                _ => {},
            }
        }
        let _ = is_float_inner;
    }
    pub(crate) fn gen_imaginary_init(&mut self, name: &str, inner: &Type, init: &Expr) {
        let off = self.var_offset(name);
        let is_float = matches!(inner, Type::Float | Type::Double | Type::LongDouble);
        // 虚数字面量 (4.0i / 3j): 取 imag 字段
        if let Expr::ComplexLit { imag, .. } = init {
            if is_float {
                self.gen_float_imm_load(*imag, inner);
                self.gen_float_store_at(off, inner);
            } else {
                self.mov_imm(0, imag.as_f64() as i128);
                self.store_stack(0, off);
            }
            return;
        }
        // 整型虚数字面量: imag 是精确 i128，按 inner 宽度直存。
        if let Expr::ComplexIntLit { imag, .. } = init {
            self.mov_imm(0, *imag);
            self.emit_store(0, off, inner.size());
            return;
        }
        // 非字面量: 求值后存储
        self.gen_expr(init, 0);
        if is_float {
            self.gen_float_store_at(off, inner);
        } else {
            self.store_stack(0, off);
        }
    }
    fn spill_complex_operand(
        &mut self,
        e: &Expr,
        rsp_slot: i64,
        inner_sz: i64,
        is_float: bool,
        is_float_inner: bool,
        op_inner: &Type,
    ) {
        let total_sz = inner_sz * 2;
        let result_off = self.complex_result_off();
        match e {
            Expr::Ident { name, ty, .. } => {
                // 分量偏移/读取宽度按变量自身 inner (C99 提升后运算 inner
                // = 更宽者, 变量可窄于运算 inner — Float 变量 movss 读 +
                // cvtss2sd); 同宽时 cvt 为 no-op, 行为不变
                let is_cplx = matches!(ty, Type::Complex(_) | Type::Imaginary(_));
                // 分量偏移/读取宽度按变量自身 inner; 标量变量按自身类型
                let v_inner = match ty {
                    Type::Complex(c) | Type::Imaginary(c) => c.base.clone(),
                    _ => ty.clone(),
                };
                let v_pfx = if matches!(v_inner, Type::Float) { 0xF3u8 } else { 0xF2u8 };
                let v_isz = v_inner.size() as i64;
                let pfx = if is_float { 0xF3u8 } else { 0xF2u8 };
                let v_is_float = matches!(v_inner, Type::Float | Type::Double | Type::LongDouble);
                if !is_cplx && v_is_float {
                    // 标量浮点变量提升: 实部=自身宽度读 (movss/movsd),
                    // 虚部=0。旧路径把标量 home 当 complex 变量读
                    // 2*inner 字节, Float 变量 (4B) 的 movsd 读会带上
                    // 相邻内存位模式。
                    if self.is_global_not_local(&name.name) {
                        self.emit_global_addr(0, &name.name);
                        self.emit(&[v_pfx, 0x0F, 0x10, 0x00]);
                    } else {
                        let off = self.var_offset(&name.name);
                        self.emit_xmm0_r13_disp(&[v_pfx, 0x41, 0x0F, 0x10], off);
                    }
                    self.emit_float_convert(&v_inner, op_inner);
                    self.emit(&[pfx, 0x0F, 0x11, 0x44, 0x24, rsp_slot as u8]);
                    self.emit(&[0x0F, 0x57, 0xC0]); // xorps xmm0, xmm0
                    self.emit(&[pfx, 0x0F, 0x11, 0x44, 0x24, (rsp_slot + inner_sz) as u8]);
                } else if !is_cplx {
                    // 标量整数变量提升为整数 _Complex: 实部=按自身宽度
                    // 符号/零扩展加载, 虚部=0。旧路径把标量 home 当
                    // complex 变量 movsd 读 2*inner 字节, 分量宽度不足时
                    // 高位垃圾进实部 (pr104604: uchar v + _Complex uint t
                    // 实部被污染)。
                    if self.is_global_not_local(&name.name) {
                        self.emit_global_addr(0, &name.name);
                    } else {
                        let off = self.var_offset(&name.name);
                        self.emit_lea_r13(0, off);
                    }
                    self.emit_load_mem_typed(v_isz as usize, &v_inner);
                    if is_float_inner {
                        let cvt_pfx = if is_float { 0xF3u8 } else { 0xF2u8 };
                        self.emit(&[cvt_pfx, 0x48, 0x0F, 0x2A, 0xC0]);
                        self.emit(&[pfx, 0x0F, 0x11, 0x44, 0x24, rsp_slot as u8]);
                        self.emit(&[0x0F, 0x57, 0xC0]);
                        self.emit(&[pfx, 0x0F, 0x11, 0x44, 0x24, (rsp_slot + inner_sz) as u8]);
                    } else {
                        self.store_rsp_qword(0, rsp_slot as usize);
                        self.mov_imm(0, 0);
                        self.store_rsp_qword(0, (rsp_slot + inner_sz) as usize);
                    }
                } else if !v_is_float && is_float_inner {
                    // 整数 inner 复数变量参与浮点运算 (C99 常用算术转换,
                    // _Complex int op _Complex double → double): 分量按
                    // 变量自身整数宽度加载 → cvtsi2ss/cvtsi2sd → 按运算
                    // inner 存槽。旧实现统一走浮点 movsd 8B 读, 把
                    // (real|imag) 位模式当一个 double, emit_float_convert
                    // 对整数 no-op → 比较恒不等 (complex-6 test_int)。
                    let cvt_pfx = if is_float { 0xF3u8 } else { 0xF2u8 };
                    let v_is_global = self.is_global_not_local(&name.name);
                    let v_slot_off = if v_is_global { 0 } else { self.var_offset(&name.name) };
                    // 逐分量一轮: **每轮重取分量基址**。`emit_load_mem_typed{,_disp}`
                    // 用 rax 同时作基址与结果寄存器, 一轮结束时 rax 已是分量
                    // 值 (如 -5) 而非地址; 不重取就退化成 `movslq 0x4(%rax)`
                    // 解引用一个整数 → SIGSEGV (全局 `_Complex int` 参与
                    // `_Complex double` 加减乘除/比较整片崩)。局部臂本来就
                    // 每轮 `emit_lea_r13`, 全局臂漏了 —— 收敛成一个循环后
                    // 这条不变量不可能再被某一侧破坏。
                    for part in 0..2i64 {
                        if v_is_global {
                            self.emit_global_addr(0, &name.name);
                        } else {
                            self.emit_lea_r13(0, v_slot_off);
                        }
                        if part == 0 {
                            self.emit_load_mem_typed(v_isz as usize, &v_inner);
                        } else {
                            self.emit_load_mem_typed_disp(v_isz as usize, &v_inner, v_isz);
                        }
                        self.emit(&[cvt_pfx, 0x48, 0x0F, 0x2A, 0xC0]);
                        self.emit(&[
                            pfx,
                            0x0F,
                            0x11,
                            0x44,
                            0x24,
                            (rsp_slot + part * inner_sz) as u8,
                        ]);
                    }
                } else if !v_is_float && !is_float_inner {
                    // 整数 complex 变量参与整数运算: 分量按变量自身宽度
                    // 扩展加载, 按运算 inner 宽度存槽。旧 movsd 整块读在
                    // v_isz<inner_sz 时把 real|imag 打包位模式当宽分量
                    // (_Complex int op _Complex long 实部混入虚部)。
                    if self.is_global_not_local(&name.name) {
                        self.emit_global_addr(0, &name.name);
                        self.emit_load_mem_typed(v_isz as usize, &v_inner);
                        self.store_rsp_qword(0, rsp_slot as usize);
                        self.emit_global_addr(0, &name.name);
                    } else {
                        let off = self.var_offset(&name.name);
                        self.emit_lea_r13(0, off);
                        self.emit_load_mem_typed(v_isz as usize, &v_inner);
                        self.store_rsp_qword(0, rsp_slot as usize);
                        self.emit_lea_r13(0, off);
                    }
                    self.emit_load_mem_typed_disp(v_isz as usize, &v_inner, v_isz);
                    self.store_rsp_qword(0, (rsp_slot + inner_sz) as usize);
                } else if self.is_global_not_local(&name.name) {
                    // 全局 complex 变量: 通过 RIP-relative 寻址加载
                    self.emit_global_addr(0, &name.name); // rax = &global
                                                          // 加载实部: movss/movsd xmm0, [rax]
                    self.emit(&[v_pfx, 0x0F, 0x10, 0x00]);
                    self.emit_float_convert(&v_inner, op_inner);
                    self.emit(&[pfx, 0x0F, 0x11, 0x44, 0x24, rsp_slot as u8]);
                    // 加载虚部: movss/movsd xmm0, [rax+v_isz]
                    if v_isz < 128 {
                        self.emit(&[v_pfx, 0x0F, 0x10, 0x40, v_isz as u8]);
                    } else {
                        self.emit(&[v_pfx, 0x0F, 0x10, 0x80]);
                        self.emit(&(v_isz as i32).to_le_bytes());
                    }
                    self.emit_float_convert(&v_inner, op_inner);
                    self.emit(&[pfx, 0x0F, 0x11, 0x44, 0x24, (rsp_slot + inner_sz) as u8]);
                } else {
                    // 局部 complex 变量: 从 [r13+off] 加载
                    let off = self.var_offset(&name.name);
                    self.emit_xmm0_r13_disp(&[v_pfx, 0x41, 0x0F, 0x10], off);
                    self.emit_float_convert(&v_inner, op_inner);
                    self.emit(&[pfx, 0x0F, 0x11, 0x44, 0x24, rsp_slot as u8]);
                    self.emit_xmm0_r13_disp(&[v_pfx, 0x41, 0x0F, 0x10], off + v_isz);
                    self.emit_float_convert(&v_inner, op_inner);
                    self.emit(&[pfx, 0x0F, 0x11, 0x44, 0x24, (rsp_slot + inner_sz) as u8]);
                }
            },
            _ => {
                // Binop/Call 不再单列: 浮点 _Complex 落入下方通用分支
                // (gen_expr 物化 result_off 后按 e 自身 inner 逐分量读 +
                // cvt 到运算 inner), 整数 _Complex 落入 rep movsb 分支。
                // 旧 Binop 分支按运算 inner_sz rep movsb 整块拷贝, e 自身
                // inner 更宽时位模式错位 (complex-6 lit_ne); 旧 Call 分支
                // 按运算 inner 读内存对, 不做跨宽度转换 (float complex
                // 返回参与 double 运算时实/虚部读窄)。
                if matches!(e.ty(), Type::Complex(_)) {
                    // _Complex rvalue (ComplexLit/binop/Call): gen_expr 统一
                    // 物化 result_off (rax=地址), 位模式/分量偏移按 e 自身
                    // inner。逐分量读取后按运算 inner 转换存槽:
                    // - 浮点→浮点: movss/movsd 源宽读 + 精度 cvt
                    // - 整数→浮点: 整数宽读 + cvtsi2ss/cvtsi2sd (旧实现把
                    //   整数位模式当浮点读, _Complex long Call 返回与
                    //   _Complex double 比较恒不等, complex-6 test_long_int)
                    // - 整数→整数: 同宽 rep movsb / 跨宽逐分量 movsxd
                    //   (旧实现按 e 自身大小整块拷贝到按运算 inner 布局的
                    //   槽, _Complex int 物化 8B 进 16B 槽错位+未转换)
                    let e_inner = match e.ty() {
                        Type::Complex(c) => c.base.clone(),
                        _ => op_inner.clone(),
                    };
                    let e_isz = e_inner.size() as i64;
                    let e_is_float =
                        matches!(&e_inner, Type::Float | Type::Double | Type::LongDouble);
                    let pfx = if is_float { 0xF3u8 } else { 0xF2u8 };
                    self.gen_expr(e, 0);
                    if e_is_float && is_float_inner {
                        // 实部: [result_off] (e 自身宽度) → cvt → [rsp+slot]
                        let e_pfx = if matches!(e_inner, Type::Float) { 0xF3u8 } else { 0xF2u8 };
                        self.emit_xmm0_r13_disp(&[e_pfx, 0x41, 0x0F, 0x10], result_off);
                        self.emit_float_convert(&e_inner, op_inner);
                        self.emit(&[pfx, 0x0F, 0x11, 0x44, 0x24, rsp_slot as u8]);
                        // 虚部: [result_off+e_isz]
                        self.emit_xmm0_r13_disp(&[e_pfx, 0x41, 0x0F, 0x10], result_off + e_isz);
                        self.emit_float_convert(&e_inner, op_inner);
                        self.emit(&[pfx, 0x0F, 0x11, 0x44, 0x24, (rsp_slot + inner_sz) as u8]);
                    } else if is_float_inner {
                        // 整数源 → 浮点运算 inner: 分量按整数宽度加载后
                        // cvtsi2ss/cvtsi2sd (Ident 变量分支同构)。每轮重取
                        // 基址: load 覆盖 rax。
                        let cvt_pfx = if is_float { 0xF3u8 } else { 0xF2u8 };
                        for part in 0..2 {
                            self.emit_lea_r13(0, result_off);
                            self.emit_load_mem_typed_disp(e_isz as usize, &e_inner, part * e_isz);
                            self.emit(&[cvt_pfx, 0x48, 0x0F, 0x2A, 0xC0]);
                            self.emit(&[
                                pfx,
                                0x0F,
                                0x11,
                                0x44,
                                0x24,
                                (rsp_slot + part * inner_sz) as u8,
                            ]);
                        }
                    } else if e_isz == inner_sz {
                        // 整数同宽: rax 指向物化区, 整块拷贝
                        self.emit(&[0x48, 0x89, 0xC6]); // rsi = rax
                        self.emit(&[0x48, 0x89, 0xE7]); // rdi = rsp
                        if rsp_slot != 0 {
                            if (-128..=127).contains(&rsp_slot) {
                                self.emit(&[0x48, 0x83, 0xC7, rsp_slot as u8]);
                            } else {
                                self.emit(&[0x48, 0x81, 0xC7]);
                                self.emit(&(rsp_slot as i32).to_le_bytes());
                            }
                        }
                        let sz = (inner_sz * 2) as i128;
                        self.mov_imm(1, sz);
                        self.emit(&[0xFC, 0xF3, 0xA4]); // cld; rep movsb
                    } else {
                        // 整数跨宽 (提升只宽化: e_isz=4, inner_sz=8):
                        // 分量 movsxd 符号扩展到运算宽度存槽。每轮重取
                        // 基址: load 覆盖 rax, 第二轮不能依赖上一轮 rax。
                        for part in 0..2 {
                            self.emit_lea_r13(0, result_off);
                            self.emit_load_mem_typed_disp(e_isz as usize, &e_inner, part * e_isz);
                            self.store_rsp_qword(0, (rsp_slot + part * inner_sz) as usize);
                        }
                    }
                } else if is_float_inner {
                    // 浮点 inner: 实部=浮点值位模式, 虚部=0 (比较端
                    // ucomiss/ucomisd 按 float/double 位模式读取)。
                    // 标量整数提升必须先转浮点: 旧实现按 64 位整数存槽
                    // (store_rsp_qword), int 1 的低 32 位是 0.0f,
                    // `y->f != 1` 恒真误 abort (20020227-1)。
                    self.gen_expr(e, 0);
                    if !matches!(e.ty(), Type::Float | Type::Double | Type::LongDouble) {
                        // cvtsi2ss/cvtsi2sd xmm0, rax (rax 已符号扩展)
                        let cvt_pfx = if is_float { 0xF3u8 } else { 0xF2u8 };
                        self.emit(&[cvt_pfx, 0x48, 0x0F, 0x2A, 0xC0]);
                    }
                    let pfx = if is_float { 0xF3u8 } else { 0xF2u8 };
                    self.emit(&[pfx, 0x0F, 0x11, 0x44, 0x24, rsp_slot as u8]);
                    self.emit(&[0x0F, 0x57, 0xC0]); // xorps xmm0, xmm0
                    self.emit(&[pfx, 0x0F, 0x11, 0x44, 0x24, (rsp_slot + inner_sz) as u8]);
                } else {
                    // 标量整数提升为整数 _Complex: 实部=值, 虚部=0
                    // 注意: 操作数槽在 [rsp+slot], 用 store_rsp_qword (非 r13 相对)
                    self.gen_expr(e, 0);
                    // 按 e 自身宽度符号/零扩展, 防高位脏值进分量
                    // (Call 返回窄整型时 rax 高位为垃圾)
                    if Self::is_integer_type(&e.ty()) {
                        match e.ty().size() {
                            1 | 2 => self.gen_int_narrow_cast(0, &e.ty()),
                            4 => {
                                if Self::is_signed_int(&e.ty()) {
                                    self.emit(&[0x48, 0x63, 0xC0]); // movsxd rax, eax
                                } else {
                                    self.emit(&[0x89, 0xC0]); // mov eax, eax (零扩展高 32)
                                }
                            },
                            _ => {},
                        }
                    }
                    self.store_rsp_qword(0, rsp_slot as usize);
                    self.mov_imm(0, 0);
                    self.store_rsp_qword(0, (rsp_slot + inner_sz) as usize);
                }
            },
        }
        let _ = total_sz;
    }
    pub(crate) fn store_complex_at_rcx(&mut self, r: &Expr, inner: &Type) {
        let isz = inner.size() as i64;
        // 统一约定：调用方把目标地址 push 入栈，这里 pop 到 rcx。这样所有
        // 调用点都是 `push(addr); store_complex_at_rcx(...)`，无需在求值 r
        // 与拷贝之间插入 pop（求值可能占用 rcx）。
        if matches!(inner, Type::LongDouble) {
            // LD: 不能走 SSE，也不能对 complex rvalue 双求值。物化在本函数
            // 内部完成 → pop 必须排在物化 **之后**：`r` 自身就是一个 Call 时
            // (全局 `_Complex long double g; g = f();`)，物化会发起一次嵌套
            // 调用，而 rcx 按 SysV 是 caller-saved — 先 pop 再物化等于把目标
            // 地址交给一个注定被毁掉的寄存器，后面的 `fstp tbyte (%rcx)` 就
            // 写到垃圾地址上，目标恒为 0。push/pop 跨过 call 是安全的：调用
            // 只写 [rsp] 以下，已压入的槽不会被踩。
            let src = self.gen_complex_ld_materialize(r);
            self.pop(1); // rcx = dst addr (物化之后才取)
            self.emit_x87_fld_at(13, src as i32, inner);
            self.emit_x87_fstp_at(1, 0, inner);
            self.emit_x87_fld_at(13, src as i32 + 16, inner);
            self.emit_x87_fstp_at(1, isz as i32, inner);
            return;
        }
        self.pop(1); // rcx = dst addr
        self.emit_scalar_to_xmm0_if_needed(r, inner);
        // 源为不同内型 complex/imaginary 时实部需精度转换
        // (标量源的转换已由 emit_scalar_to_xmm0_if_needed 处理)
        if let Type::Complex(ref c) | Type::Imaginary(ref c) = r.ty() {
            self.emit_float_convert(&c.base, inner);
        }
        let pfx = if matches!(inner, Type::Float) { 0xF3u8 } else { 0xF2u8 };
        self.emit(&[pfx, 0x0F, 0x11, 0x01]); // movss/movsd [rcx], xmm0 (实部)
        self.gen_complex_imag_into_xmm0(r, inner);
        self.emit(&[pfx, 0x0F, 0x11, 0x41]); // movss/movsd [rcx+disp8], xmm0
        self.emit(&[isz as u8]);
    }
    fn store_complex_pair_from_rsp(&mut self, result_off: i64, inner_sz: i64, is_float: bool) {
        let pfx = if is_float { 0xF3u8 } else { 0xF2u8 };
        self.emit(&[pfx, 0x0F, 0x10, 0x04, 0x24]);
        self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off);
        self.emit(&[pfx, 0x0F, 0x10]);
        self.emit_rsp_modrm_disp(0, inner_sz);
        self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off + inner_sz);
    }
}
