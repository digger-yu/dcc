//! emit/expr/vector.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
//! 仅移动/可见性调整，不改语义（backend_refactor_plan §3.1 原则 1）。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::shared::elf::*;
use crate::codegen::shared::*;
use crate::codegen::x86_64::insn::Insn;
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
    fn emit_global_vector_elem_load(&mut self, name: &str, elem_idx: usize, elem_size: usize) {
        self.emit_global_addr(0, name);
        let byte_off = (elem_idx * elem_size) as i64;
        if byte_off != 0 {
            if (-128..=127).contains(&byte_off) {
                let gas = format!("add ${}, %rax", byte_off);
                self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x83, 0xC0, byte_off as u8], &gas));
            } else {
                let gas = format!("add ${}, %rax", byte_off);
                let mut bytes = vec![0x48, 0x05];
                bytes.extend_from_slice(&(byte_off as i32).to_le_bytes());
                self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
            }
        }
        self.emit_load_mem(elem_size);
    }
    fn emit_global_vector_elem_store(&mut self, name: &str, elem_idx: usize, elem_size: usize) {
        self.emit_global_addr(1, name); // rcx = &global
        let byte_off = (elem_idx * elem_size) as i64;
        if byte_off != 0 {
            if (-128..=127).contains(&byte_off) {
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x83, 0xC1, byte_off as u8]));
            } else {
                self.emit_insn(&Insn::alu_rm_reg_imm32_b64(0, 1, byte_off));
            }
        }
        // mov [rcx], rax (按元素宽度)
        match elem_size {
            1 => {
                self.emit_insn(&Insn::raw_bytes(&[0x88, 0x01]));
            },
            2 => {
                self.emit_insn(&Insn::raw_bytes(&[0x66, 0x89, 0x01]));
            },
            4 => {
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(1),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            ));
            },
            _ => {
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(1),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B64
                    ),
                },
                &[],
            ));
            },
        }
    }
    fn gen_scalar_vector_load(&mut self, e: &Expr, elem_idx: usize, elem_size: usize) {
        let is_float = matches!(e.ty(), Type::Vector(ref inner, _) if matches!(inner.base, Type::Float | Type::Double));
        if is_float {
            self.gen_scalar_vector_load_sse(
                e,
                elem_idx,
                elem_size,
                matches!(e.ty(), Type::Vector(ref inner, _) if matches!(inner.base, Type::Float)),
            );
            return;
        }
        if let Some((elem_ty, n, inits)) = Self::vector_init_from_expr(e) {
            let vec_size = elem_size * n;
            self.gen_vector_init_rvalue(inits, elem_ty, n, self.vector_op_slot);
            let temp_off = self.vector_temp_base(self.vector_op_slot, vec_size)
                + (elem_idx * elem_size) as i64;
            self.load_stack_sized(0, temp_off, elem_size);
            return;
        }
        match e {
            Expr::Ident { name, .. } => {
                if self.is_global_var(&name.name) {
                    self.emit_global_vector_elem_load(&name.name, elem_idx, elem_size);
                } else {
                    let off = self.var_offset(&name.name) + (elem_idx * elem_size) as i64;
                    self.load_stack_sized(0, off, elem_size);
                }
            },
            _ if matches!(e.ty(), Type::Vector(_, _)) => {
                let count = match e.ty() {
                    Type::Vector(_, n) => n,
                    _ => 0,
                };
                let vec_size = elem_size * count;
                self.gen_vector_to_temp(e, self.vector_op_slot);
                let temp_off = self.vector_temp_base(self.vector_op_slot, vec_size)
                    + (elem_idx * elem_size) as i64;
                self.load_stack_sized(0, temp_off, elem_size);
            },
            _ => {
                self.gen_expr(e, Self::EVAL_BASE);
                let off = (elem_idx * elem_size) as i64;
                if off != 0 {
                    if (-128..=127).contains(&off) {
                        let gas = format!("add ${}, %rax", off);
                        self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x83, 0xC0, off as u8], &gas));
                    } else {
                        let gas = format!("add ${}, %rax", off);
                        let bytes = vec![0x48, 0x05]
                            .into_iter()
                            .chain((off as i32).to_le_bytes().into_iter())
                            .collect::<Vec<u8>>();
                        self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
                    }
                }
                self.emit_load_mem(elem_size);
            },
        }
    }
    fn gen_scalar_vector_load_sse(
        &mut self,
        e: &Expr,
        elem_idx: usize,
        elem_size: usize,
        is_float: bool,
    ) {
        if let Some((elem_ty, n, inits)) = Self::vector_init_from_expr(e) {
            let vec_size = elem_size * n;
            self.gen_vector_init_rvalue(inits, elem_ty, n, self.vector_op_slot);
            let temp_off = self.vector_temp_base(self.vector_op_slot, vec_size)
                + (elem_idx * elem_size) as i64;
            self.load_stack_xmm(0, temp_off, elem_size, is_float);
            return;
        }
        match e {
            Expr::Ident { name, .. } => {
                if self.is_global_var(&name.name) {
                    self.emit_global_addr(0, &name.name);
                    let byte_off = (elem_idx * elem_size) as i64;
                    if byte_off != 0 {
                        if (-128..=127).contains(&byte_off) {
                            let gas = format!("add ${}, %rax", byte_off);
                            self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x83, 0xC0, byte_off as u8], &gas));
                        } else {
                            let gas = format!("add ${}, %rax", byte_off);
                            let bytes = vec![0x48, 0x05]
                                .into_iter()
                                .chain((byte_off as i32).to_le_bytes().into_iter())
                                .collect::<Vec<u8>>();
                            self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
                        }
                    }
                    self.load_mem_xmm(0, elem_size, is_float);
                } else {
                    let off = self.var_offset(&name.name) + (elem_idx * elem_size) as i64;
                    self.load_stack_xmm(0, off, elem_size, is_float);
                }
            },
            _ if matches!(e.ty(), Type::Vector(_, _)) => {
                let count = match e.ty() {
                    Type::Vector(_, n) => n,
                    _ => 0,
                };
                let vec_size = elem_size * count;
                self.gen_vector_to_temp(e, self.vector_op_slot);
                let temp_off = self.vector_temp_base(self.vector_op_slot, vec_size)
                    + (elem_idx * elem_size) as i64;
                self.load_stack_xmm(0, temp_off, elem_size, is_float);
            },
            _ => {
                self.gen_expr(e, Self::EVAL_BASE);
            },
        }
    }
    pub(crate) fn gen_vector_binop(&mut self, op: BinOp, l: &Expr, r: &Expr, result_slot: usize) {
        // 从任一向量操作数获取元素类型; 标量操作数需广播 (broadcast)
        let l_is_vec = matches!(l.ty(), Type::Vector(_, _));
        let r_is_vec = matches!(r.ty(), Type::Vector(_, _));
        if !l_is_vec && !r_is_vec {
            return;
        }
        let (elem_ty, count) = if l_is_vec {
            match l.ty() {
                Type::Vector(ref elem, n) => (elem.base.clone(), n),
                _ => unreachable!(),
            }
        } else {
            match r.ty() {
                Type::Vector(ref elem, n) => (elem.base.clone(), n),
                _ => unreachable!(),
            }
        };
        let elem_size = elem_ty.size();
        let vec_size = elem_size * count;
        let result_off = self.vector_temp_base(result_slot, vec_size);
        let is_float = matches!(elem_ty, Type::Float);
        let is_double = matches!(elem_ty, Type::Double);

        // SIMD 快速路径: 128-bit 纯向量运算使用 XMM 打包指令
        // 有标量操作数时走下方逐元素广播路径
        if l_is_vec
            && r_is_vec
            && vec_size == 16
            && self.try_vector_binop_xmm(
                op,
                l,
                r,
                result_slot,
                result_off,
                &elem_ty,
                count,
                elem_size,
                is_float,
                is_double,
            )
        {
            // SIMD 快速路径: 结果已写入 result_off, rax = &result[0]
            self.emit_insn(&Insn::raw_bytes(&[0x49, 0x8D, 0x85]));
            self.emit_insn(&Insn::raw_bytes(&(result_off as i32).to_le_bytes()));
            return;
        }

        // 标量回退路径 (支持标量广播: 标量操作数逐元素复用同一值)
        // 操作数物化槽 = 结果槽+1: 内层子 binop 结果写入更深的槽,
        // 不会覆盖本层已写入结果槽的元素 (pr23135 链式运算)
        let saved_op_slot = self.vector_op_slot;
        self.vector_op_slot = result_slot + 1;
        if is_float || is_double {
            if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne)
            {
                // 向量浮点比较 (GCC 扩展): cmpss/cmpsd 逐元素比较,
                // 结果全 1/0。谓词: eq=0, lt=1, le=2, neq=4, nlt=5,
                // nle=6。SSE 无直接大于谓词, 用 nlt/nle 取反表达:
                // Gt → nle = !(xmm0<=xmm1) = xmm0>xmm1; NaN 时
                // lt/le 为假 (ordered 数值语义)
                let cmp_pfx: u8 = if is_float { 0xF3 } else { 0xF2 };
                let pred: u8 = match op {
                    BinOp::EqEq => 0,
                    BinOp::Ne => 4,
                    BinOp::Lt => 1,
                    BinOp::Le => 2,
                    BinOp::Gt => 6, // nle: !(xmm0<=xmm1) 即 xmm0>xmm1
                    _ => 5,         // nlt: !(xmm0<xmm1) 即 xmm0>=xmm1
                };
                for i in 0..count {
                    // 加载左操作数: 向量按元素取, 标量直接求值 (广播)
                    if l_is_vec {
                        self.gen_scalar_vector_load_sse(l, i, elem_size, is_float);
                    } else {
                        self.gen_expr(l, Self::EVAL_BASE);
                        self.emit_scalar_float_convert(&l.ty(), &elem_ty);
                    }
                    self.emit_sub_rsp(8);
                    let spfx: u8 = if is_float { 0xF3 } else { 0xF2 };
                    self.emit_insn(&Insn::raw_bytes(&[spfx, 0x0F, 0x11, 0x04, 0x24]));
                    // 加载右操作数: 向量按元素取, 标量直接求值 (广播)
                    if r_is_vec {
                        self.gen_scalar_vector_load_sse(r, i, elem_size, is_float);
                    } else {
                        self.gen_expr(r, Self::EVAL_BASE);
                        self.emit_scalar_float_convert(&r.ty(), &elem_ty);
                    }
                    self.emit_insn(&Insn::raw_bytes(&[spfx, 0x0F, 0x10, 0xC8])); // movss xmm1, xmm0
                    self.emit_insn(&Insn::raw_bytes(&[spfx, 0x0F, 0x10, 0x04, 0x24])); // xmm0 = left
                    self.emit_add_rsp(8);
                    // xmm0=left, xmm1=right → cmp xmm0, xmm1, pred
                    self.emit_insn(&Insn::raw_bytes(&[cmp_pfx, 0x0F, 0xC2, 0xC1, pred]));
                    self.emit_insn(&Insn::raw_bytes(&[0x66, 0x48, 0x0F, 0x7E, 0xC0])); // movq rax, xmm0
                    let off = result_off + (i * elem_size) as i64;
                    self.emit_store(0, off, elem_size);
                }
                self.vector_op_slot = saved_op_slot;
                self.emit_insn(&Insn::raw_bytes(&[0x49, 0x8D, 0x85]));
                self.emit_insn(&Insn::raw_bytes(&(result_off as i32).to_le_bytes()));
                return;
            }
            let (prefix, opcode) = match op {
                BinOp::Add => (if is_float { 0xF3 } else { 0xF2 }, 0x58),
                BinOp::Sub => (if is_float { 0xF3 } else { 0xF2 }, 0x5C),
                BinOp::Mul => (if is_float { 0xF3 } else { 0xF2 }, 0x59),
                BinOp::Div => (if is_float { 0xF3 } else { 0xF2 }, 0x5E), // divss/divsd
                _ => return,
            };
            for i in 0..count {
                // 加载左操作数: 向量按元素取, 标量直接求值 (广播)
                if l_is_vec {
                    self.gen_scalar_vector_load_sse(l, i, elem_size, is_float);
                } else {
                    self.gen_expr(l, Self::EVAL_BASE);
                    // 标量精度转换: 匹配向量元素类型
                    self.emit_scalar_float_convert(&l.ty(), &elem_ty);
                }
                self.emit_sub_rsp(8);
                if is_float {
                    self.emit_insn(&Insn::raw_bytes(&[0xF3, 0x0F, 0x11, 0x04, 0x24]));
                } else {
                    self.emit_insn(&Insn::raw_bytes(&[0xF2, 0x0F, 0x11, 0x04, 0x24]));
                }
                // 加载右操作数: 向量按元素取, 标量直接求值 (广播)
                if r_is_vec {
                    self.gen_scalar_vector_load_sse(r, i, elem_size, is_float);
                } else {
                    self.gen_expr(r, Self::EVAL_BASE);
                    // 标量精度转换: 匹配向量元素类型
                    self.emit_scalar_float_convert(&r.ty(), &elem_ty);
                }
                if is_float {
                    self.emit_insn(&Insn::movss_xmm(1, 0)); // movss xmm1, xmm0
                } else {
                    self.emit_insn(&Insn::movsd_xmm(1, 0)); // movsd xmm1, xmm0
                }
                if is_float {
                    self.emit_insn(&Insn::raw_bytes(&[0xF3, 0x0F, 0x10, 0x04, 0x24]));
                } else {
                    self.emit_insn(&Insn::raw_bytes(&[0xF2, 0x0F, 0x10, 0x04, 0x24]));
                }
                self.emit_add_rsp(8);
                self.emit_insn(&Insn::raw_bytes(&[prefix, 0x0F, opcode, 0xC1])); // xmm0 = left op right
                self.store_vector_elem_xmm(
                    0,
                    result_off + (i * elem_size) as i64,
                    elem_size,
                    is_float,
                );
            }
            self.vector_op_slot = saved_op_slot;
            self.emit_insn(&Insn::raw_bytes(&[0x49, 0x8D, 0x85]));
            self.emit_insn(&Insn::raw_bytes(&(result_off as i32).to_le_bytes()));
            return;
        }
        // __int128 / 16-byte 整数元素: GP 寄存器无法承载, 用 XMM 处理
        // (pr105613: unsigned __int128 vector 比较)
        if elem_size == 16 && !is_float && !is_double {
            self.gen_vector_binop_int128(
                op,
                l,
                r,
                result_slot,
                result_off,
                &elem_ty,
                count,
                elem_size,
                l_is_vec,
                r_is_vec,
                saved_op_slot,
            );
            self.emit_insn(&Insn::raw_bytes(&[0x49, 0x8D, 0x85]));
            self.emit_insn(&Insn::raw_bytes(&(result_off as i32).to_le_bytes()));
            return;
        }
        for i in 0..count {
            // 加载左操作数: 向量按元素取, 标量直接求值 (广播)
            // l 值经栈暂存 (push/pop): r 求值 (嵌套 binop/调用) 会
            // 破坏 rcx, 旧 mov rcx,rax 暂存在 r 为复杂表达式时踩 l 值
            // (pr109040: 6 > ((V){...} & m) 外层比较读到内层中间值)
            if l_is_vec {
                self.gen_scalar_vector_load(l, i, elem_size);
            } else {
                self.gen_expr(l, Self::EVAL_BASE);
            }
            self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // push rax (暂存 l 值)
                                // 加载右操作数: 向量按元素取, 标量直接求值 (广播)
            if r_is_vec {
                self.gen_scalar_vector_load(r, i, elem_size);
            } else {
                self.gen_expr(r, Self::EVAL_BASE);
            }
            self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // pop rcx (恢复 l 值)
            match op {
                BinOp::Add => {
                    // P11: 收敛到 AluRm{Add, B64, rcx, rax}.
                    let insn = Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                            op: crate::codegen::x86_64::opcode::AluOp::Add,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                1,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                        },
                        &[],
                    );
                    if !self.emit_insn(&insn) {
                        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x01, 0xC1]));
                    }
                },
                BinOp::Sub => {
                    // P11: 收敛到 AluRm{Sub, B64, rcx, rax}.
                    let insn = Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                            op: crate::codegen::x86_64::opcode::AluOp::Sub,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                1,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                        },
                        &[],
                    );
                    if !self.emit_insn(&insn) {
                        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x29, 0xC1]));
                    }
                },
                BinOp::Mul => {
                    self.emit_insn(&Insn::imul_rm_reg(
                        crate::codegen::shared::asm_operand::Width::B64,
                        1, 0, // rcx, rax
                    ));
                }, // imul rcx, rax
                BinOp::Div | BinOp::Mod => {
                    // 整数除法: xchg 后 rax=被除数, rcx=除数
                    let is_mod = matches!(op, BinOp::Mod);
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x91])); // xchg rax, rcx
                    let signed = matches!(
                        elem_ty,
                        Type::SChar
                            | Type::Short
                            | Type::Int
                            | Type::Long
                            | Type::LongLong
                            | Type::Int128
                    );
                    match elem_size {
                        8 => {
                            if signed {
                                self.emit_insn(&Insn::sign_extend_rax_to_rdx(crate::codegen::shared::asm_operand::Width::B64)); // cqo
                                self.emit_insn(&Insn::raw_bytes(&[0x48, 0xF7, 0xF9])); // idiv rcx
                            } else {
                                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::AluRm { op: crate::codegen::x86_64::opcode::AluOp::Xor, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // xor rdx, rdx
                                self.emit_insn(&Insn::raw_bytes(&[0x48, 0xF7, 0xF1])); // div rcx
                            }
                        },
                        4 => {
                            if signed {
                                self.emit_insn(&Insn::sign_extend_rax_to_rdx(crate::codegen::shared::asm_operand::Width::B32)); // cdq
                                self.emit_insn(&Insn::raw_bytes(&[0xF7, 0xF9])); // idiv ecx
                            } else {
                                self.emit_insn(&Insn::raw_bytes(&[0x31, 0xD2])); // xor edx, edx
                                self.emit_insn(&Insn::raw_bytes(&[0xF7, 0xF1])); // div ecx
                            }
                        },
                        2 => {
                            if signed {
                                self.emit_insn(&Insn::sign_extend_rax_to_rdx(crate::codegen::shared::asm_operand::Width::B16)); // cwd
                                self.emit_insn(&Insn::raw_bytes(&[0x66, 0xF7, 0xF9])); // idiv cx
                            } else {
                                self.emit_insn(&Insn::raw_bytes(&[0x31, 0xD2])); // xor edx, edx
                                self.emit_insn(&Insn::raw_bytes(&[0x66, 0xF7, 0xF1])); // div cx
                            }
                        },
                        _ => {
                            if signed {
                                self.emit_insn(&Insn::raw_bytes(&[0x66, 0x98])); // cbw
                                self.emit_insn(&Insn::raw_bytes(&[0xF6, 0xF9])); // idiv cl
                            } else {
                                self.emit_insn(&Insn::raw_bytes(&[0x31, 0xD2])); // xor edx, edx
                                self.emit_insn(&Insn::raw_bytes(&[0xF6, 0xF1])); // div cl
                            }
                        },
                    }
                    if is_mod {
                        if elem_size == 1 {
                            // 8 位除法余数在 ah (非 rdx): ror rax,8
                            // 后取低字节
                            self.emit_insn(&Insn::raw_bytes(&[0x48, 0xC1, 0xC8, 0x08])); // ror rax, 8
                            self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rcx, rax
                        } else {
                            // 余数在 rdx, 移到 rcx
                            self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rcx, rdx
                        }
                    } else {
                        self.mov(1, 0); // rcx = 商 (rax)
                    }
                },
                BinOp::Xor => {
                    // P11: 收敛到 AluRm{Xor, B64, rcx, rax}.
                    let insn = Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                            op: crate::codegen::x86_64::opcode::AluOp::Xor,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                1,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                        },
                        &[],
                    );
                    if !self.emit_insn(&insn) {
                        self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::AluRm { op: crate::codegen::x86_64::opcode::AluOp::Xor, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // xor rcx, rax
                    }
                },
                BinOp::And => {
                    // P11: 收敛到 AluRm{And, B64, rcx, rax}.
                    let insn = Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                            op: crate::codegen::x86_64::opcode::AluOp::And,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                1,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                        },
                        &[],
                    );
                    if !self.emit_insn(&insn) {
                        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x21, 0xC1])); // and rcx, rax
                    }
                },
                BinOp::Or => {
                    // P11: 收敛到 AluRm{Or, B64, rcx, rax}.
                    let insn = Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                            op: crate::codegen::x86_64::opcode::AluOp::Or,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                1,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                        },
                        &[],
                    );
                    if !self.emit_insn(&insn) {
                        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x09, 0xC1])); // or rcx, rax
                    }
                },
                BinOp::Shl | BinOp::Shr => {
                    // rcx=l_val, rax=r_val. x86 shift: shl/sar r/m, cl
                    // xchg -> rax=l, rcx=r; shift rax by cl; mov rcx,rax
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x91])); // xchg rax, rcx
                    let signed = matches!(
                        elem_ty,
                        Type::SChar | Type::Short | Type::Int | Type::Long | Type::LongLong
                    );
                    match op {
                        BinOp::Shl => {
                            self.emit_insn(&Insn::raw_bytes(&[0x48, 0xD3, 0xE0])); // shl rax, cl
                        },
                        BinOp::Shr if signed => {
                            self.emit_insn(&Insn::raw_bytes(&[0x48, 0xD3, 0xF8])); // sar rax, cl
                        },
                        BinOp::Shr => {
                            self.emit_insn(&Insn::raw_bytes(&[0x48, 0xD3, 0xE8])); // shr rax, cl (unsigned)
                        },
                        _ => unreachable!(),
                    }
                    self.mov(1, 0); // mov rcx, rax
                },
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne => {
                    // 向量比较 (GCC 扩展): 逐元素比较, 结果全 1 (真) /
                    // 0 (假), 元素类型同操作数 (真值 -1 按元素宽度截断)
                    let signed = matches!(
                        elem_ty,
                        Type::SChar
                            | Type::Short
                            | Type::Int
                            | Type::Long
                            | Type::LongLong
                            | Type::Int128
                    );
                    // rcx=left, rax=right → cmp rcx, rax
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x39, 0xC1]));
                    let setcc: u8 = match op {
                        BinOp::EqEq => 0x94,
                        BinOp::Ne => 0x95,
                        BinOp::Lt => {
                            if signed {
                                0x9C
                            } else {
                                0x92
                            }
                        },
                        BinOp::Le => {
                            if signed {
                                0x9E
                            } else {
                                0x96
                            }
                        },
                        BinOp::Gt => {
                            if signed {
                                0x9F
                            } else {
                                0x97
                            }
                        },
                        _ => {
                            if signed {
                                0x9D
                            } else {
                                0x93
                            }
                        },
                    };
                    self.emit_insn(&Insn::raw_bytes(&[0x0F, setcc, 0xC0])); // setcc al
                    // movzbq al, al
                    self.emit_insn(&Insn::mov_ext(
                        false,
                        crate::codegen::shared::asm_operand::Width::B8,
                        crate::codegen::shared::asm_operand::Width::B64,
                        0, 0,
                    ));
                    self.emit_insn(&Insn::unary_rm_reg(crate::codegen::x86_64::opcode::UnaryOp::Neg, crate::codegen::shared::asm_operand::Width::B64, 0)); // neg rax (0/1 → 0/all-ones)
                    self.mov(1, 0); // rcx = 结果 (循环尾统一存 rcx)
                },
                _ => {
                    // P11: 收敛到 AluRm{Add, B64, rcx, rax}.
                    let insn = Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                            op: crate::codegen::x86_64::opcode::AluOp::Add,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                1,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                        },
                        &[],
                    );
                    if !self.emit_insn(&insn) {
                        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x01, 0xC1]));
                    }
                },
            }
            let off = result_off + (i * elem_size) as i64;
            // add/sub/imul 结果在 rcx，按元素宽度写入临时区
            self.emit_store(1, off, elem_size);
        }
        self.vector_op_slot = saved_op_slot;
        // rax = &result[0]: 聚合/向量返回值需要调用者能拿到结果地址
        // (gen_expr → gen_sret_return 兜底分支用 rax 作为 rep movsq 源指针)
        self.emit_insn(&Insn::raw_bytes(&[0x49, 0x8D, 0x85]));
        self.emit_insn(&Insn::raw_bytes(&(result_off as i32).to_le_bytes()));
    }
    pub(crate) fn gen_vector_init_rvalue(
        &mut self,
        inits: &[InitItem],
        elem_ty: &Type,
        count: usize,
        temp_slot: usize,
    ) {
        let elem_size = elem_ty.size();
        let vec_size = elem_size * count;
        let temp_off = self.vector_temp_base(temp_slot, vec_size);
        for i in 0..count {
            let value = inits.get(i).and_then(|item| match item {
                InitItem::Expr(e) => Some(e),
                _ => None,
            });
            let off = temp_off + (i * elem_size) as i64;
            if let Some(v) = value {
                self.gen_expr(v, Self::EVAL_BASE);
                self.emit_scalar_to_xmm0_if_needed(v, elem_ty);
            } else {
                if matches!(elem_ty, Type::Float | Type::Double) {
                    self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x57, 0xC0])); // xorps xmm0, xmm0
                } else {
                    self.mov_imm(0, 0);
                }
            }
            if matches!(elem_ty, Type::Float) {
                self.store_xmm32_stack(0, off);
            } else if matches!(elem_ty, Type::Double) {
                self.store_xmm64_stack(0, off);
            } else if elem_size == 16 {
                // 128-bit 元素 (__int128/__uint128): rax=低64, rdx=高64
                // 若来自 InitList 的标量 (如 0), 需确保 rdx=0
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::AluRm { op: crate::codegen::x86_64::opcode::AluOp::Xor, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // xor rdx, rdx (安全覆盖垃圾高字节)
                self.store_stack(0, off);
                self.store_stack(2, off + 8);
            } else {
                self.emit_store(0, off, elem_size);
            }
        }
    }
    pub(crate) fn gen_vector_load(&mut self, e: &Expr) {
        self.gen_vector_load_into_slot(e, 0);
    }
    pub(crate) fn gen_vector_load_into_slot(&mut self, e: &Expr, temp_slot: usize) {
        let (elem_ty, count) = match e.ty() {
            Type::Vector(ref elem, n) => {
                let et = elem.base.clone();
                (et, n)
            },
            _ => return,
        };
        if let Some((et, n, inits)) = Self::vector_init_from_expr(e) {
            self.gen_vector_init_rvalue(inits, et, n, temp_slot);
            return;
        }
        let elem_size = elem_ty.size();
        let vec_size = elem_size * count;
        let temp_off = self.vector_temp_base(temp_slot, vec_size);
        // 128-bit integer (__int128) 元素: GP 寄存器无法承载, 用 movdqu 处理
        let is_int128_elem = elem_size == 16 && !matches!(elem_ty, Type::Float | Type::Double);
        match e {
            Expr::Ident { name, .. } => {
                if self.is_global_var(&name.name) {
                    for i in 0..count {
                        self.emit_global_vector_elem_load(&name.name, i, elem_size);
                        let dst_off = temp_off + (i * elem_size) as i64;
                        self.emit_store(0, dst_off, elem_size);
                    }
                    return;
                }
                let var_off = self.var_offset(&name.name);
                if is_int128_elem {
                    // 从栈上 movdqu 到临时槽
                    self.emit_movdqu_r13_to_xmm(0, var_off);
                    self.emit_movdqu_xmm_to_r13(0, temp_off);
                    return;
                }
                let is_float_elem = matches!(elem_ty, Type::Float);
                let is_double_elem = matches!(elem_ty, Type::Double);
                for i in 0..count {
                    let src_off = var_off + (i * elem_size) as i64;
                    if is_float_elem || is_double_elem {
                        self.load_stack_xmm(0, src_off, elem_size, is_float_elem);
                    } else {
                        self.emit_lea_r13(0, src_off);
                        self.emit_load_mem(elem_size);
                    }
                    let dst_off = temp_off + (i * elem_size) as i64;
                    if is_float_elem {
                        self.store_xmm32_stack(0, dst_off);
                    } else if is_double_elem {
                        self.store_xmm64_stack(0, dst_off);
                    } else {
                        self.emit_store(0, dst_off, elem_size);
                    }
                }
            },
            Expr::Binop { op, l, r, .. } => {
                self.gen_vector_binop(*op, l, r, temp_slot);
            },
            Expr::Unary { op, e: inner, .. }
                if matches!(inner.ty(), Type::Vector(_, _))
                    && matches!(op, UnOp::Neg | UnOp::BitNot) =>
            {
                // 一元向量运算: 显式传槽 (嵌套在 binop 操作数求值中时,
                // 顶层硬编码槽 0 会覆盖外层逐元素已写入的结果)
                if let Type::Vector(ref elem_ty, n) = inner.ty() {
                    self.gen_vector_unary_into_slot(*op, inner, &elem_ty.base, n, temp_slot);
                }
            },
            Expr::InitList { inits, .. } => {
                self.gen_vector_init_rvalue(inits, &elem_ty, count, temp_slot);
            },
            Expr::Member { .. } | Expr::Index { .. } | Expr::Deref { .. } => {
                // vector lvalue: 取地址后整块拷贝到临时槽
                // (不能回退 gen_expr: 会递归回本函数)
                if self.gen_lvalue_addr(e, Self::EVAL_BASE) {
                    self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(6, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rsi, rax
                    self.emit_lea_r13(7, temp_off); // rdi = 临时槽
                    self.mov_imm(1, vec_size as i128);
                    self.emit_insn(&Insn::raw_bytes(&[0xFC])); // cld
                    self.emit_insn(&Insn::raw_bytes(&[0xF3, 0xA4])); // rep movsb
                }
            },
            Expr::Call { .. } => {
                // vector 返回值: gen_expr 已将 rax:rdx 存入槽 0
                self.gen_expr(e, Self::EVAL_BASE);
                if temp_slot != 0 {
                    let src_off = self.vector_temp_base(0, vec_size);
                    let n_qwords = vec_size.div_ceil(8);
                    for q in 0..n_qwords {
                        self.load_stack(0, src_off + (q * 8) as i64);
                        self.emit_store(0, temp_off + (q * 8) as i64, 8);
                    }
                }
            },
            _ => {
                self.gen_expr(e, Self::EVAL_BASE);
                let total_bytes = elem_size * count;
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(6, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rsi, rax
                self.emit_lea_r13(7, temp_off); // rdi = 临时区
                self.mov_imm(1, total_bytes as i128);
                self.emit_insn(&Insn::raw_bytes(&[0xFC])); // cld
                self.emit_insn(&Insn::raw_bytes(&[0xF3, 0xA4])); // rep movsb
            },
        }
    }
    pub(crate) fn gen_vector_store(&mut self, name: &str) {
        let ty = self
            .var_map
            .get(name)
            .map(|(_, ty)| ty.clone())
            .or_else(|| self.lookup_global_info(name).map(|g| g.ty.clone()))
            .unwrap_or(Type::Int);
        let (elem_ty, count) = match ty {
            Type::Vector(ref elem, n) => {
                let et = elem.base.clone();
                (et, n)
            },
            _ => return,
        };
        let elem_size = elem_ty.size();
        let vec_size = elem_size * count;
        let temp_off = self.vector_temp_base(0, vec_size);
        let is_global = self.is_global_var(name);
        let is_float_elem = matches!(elem_ty, Type::Float);
        let is_double_elem = matches!(elem_ty, Type::Double);
        for i in 0..count {
            let src_off = temp_off + (i * elem_size) as i64;
            if is_float_elem || is_double_elem {
                self.load_stack_xmm(0, src_off, elem_size, is_float_elem);
            } else {
                self.emit_insn(&Insn::raw_bytes(&[0x49, 0x8D])); // lea rax, [r13 + src_off]
                if src_off.abs() < 128 {
                    self.emit_insn(&Insn::raw_bytes(&[0x45, src_off as i8 as u8]));
                } else {
                    self.emit_insn(&Insn::raw_bytes(&[0x85]));
                    self.emit_insn(&Insn::raw_bytes(&(src_off as i32).to_le_bytes()));
                }
                self.emit_load_mem(elem_size);
            }
            if is_global {
                self.emit_global_vector_elem_store(name, i, elem_size);
            } else {
                let var_off = self.var_offset(name);
                let dst_off = var_off + (i * elem_size) as i64;
                if is_float_elem {
                    self.store_xmm32_stack(0, dst_off);
                } else if is_double_elem {
                    self.store_xmm64_stack(0, dst_off);
                } else {
                    self.emit_store(0, dst_off, elem_size);
                }
            }
        }
    }
    pub(crate) fn gen_vector_to_temp(&mut self, e: &Expr, temp_slot: usize) {
        if let Some((elem_ty, n, inits)) = Self::vector_init_from_expr(e) {
            self.gen_vector_init_rvalue(inits, elem_ty, n, temp_slot);
            return;
        }
        match e {
            Expr::Binop { op, l, r, .. } if matches!(e.ty(), Type::Vector(_, _)) => {
                self.gen_vector_binop(*op, l, r, temp_slot);
            },
            Expr::InitList { inits, .. } => {
                if let Type::Vector(elem_ty, n) = e.ty() {
                    self.gen_vector_init_rvalue(inits, &elem_ty.base, n, temp_slot);
                }
            },
            _ => self.gen_vector_load_into_slot(e, temp_slot),
        }
    }
    pub(crate) fn gen_vector_unary_into_slot(
        &mut self,
        op: UnOp,
        e: &Expr,
        elem_ty: &Type,
        count: usize,
        result_slot: usize,
    ) {
        let elem_size = elem_ty.size();
        let vec_size = elem_size * count;
        let result_off = self.vector_temp_base(result_slot, vec_size);
        if self.gen_lvalue_addr(e, Self::EVAL_BASE) {
            // 保存源地址到 rcx (后续 load/store 会破坏 rax)
            self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rcx, rax
        } else {
            // rvalue 内层 (如 -(-a)): 物化到更深的槽, 逐元素从那里读
            self.gen_vector_load_into_slot(e, result_slot + 1);
            let src_off = self.vector_temp_base(result_slot + 1, vec_size);
            self.emit_lea_r13(6, src_off); // rsi = 源
            self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(6, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rcx, rsi
        }
        for i in 0..count {
            let off = (i * elem_size) as i64;
            // 加载元素到 rax: 从 [rcx+off]
            if (-128..128).contains(&off) {
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x8B, 0x41, off as u8])); // mov rax, [rcx+disp8]
            } else {
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x8B, 0x81])); // mov rax, [rcx+disp32]
                self.emit_insn(&Insn::raw_bytes(&(off as i32).to_le_bytes()));
            }
            // 应用操作
            match op {
                UnOp::Neg => {
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0xf7, 0xd8])); // neg rax
                },
                UnOp::BitNot => {
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0xf7, 0xd0])); // not rax
                },
                _ => {},
            }
            // 存储到结果槽
            self.emit_store(0, result_off + off, elem_size);
        }
        // rax 指向结果
        self.emit_lea_r13(0, result_off);
    }
    fn store_vector_elem_xmm(&mut self, xmm: u8, off: i64, elem_size: usize, is_float: bool) {
        if is_float {
            self.store_xmm32_stack(xmm, off);
        } else if elem_size == 8 {
            self.store_xmm64_stack(xmm, off);
        } else {
            self.store_xmm32_stack(xmm, off);
        }
    }
    fn try_vector_binop_xmm(
        &mut self,
        op: BinOp,
        l: &Expr,
        r: &Expr,
        result_slot: usize,
        result_off: i64,
        _elem_ty: &Type,
        count: usize,
        elem_size: usize,
        is_float: bool,
        is_double: bool,
    ) -> bool {
        let vec_size = elem_size * count;
        // 先把左右操作数存入临时槽 (结果槽+1 / 结果槽+2):
        // 嵌套 binop 时操作数自身的计算结果写入更深槽,
        // 不会覆盖本层已物化的另一个操作数或结果槽
        let l_slot = result_slot + 1;
        let r_slot = result_slot + 2;
        self.gen_vector_load_into_slot(l, l_slot);
        self.gen_vector_load_into_slot(r, r_slot);
        let left_off = self.vector_temp_base(l_slot, vec_size);
        let right_off = self.vector_temp_base(r_slot, vec_size);

        // movdqu xmm0, [r13 + left_off]
        self.emit_movdqu_r13_to_xmm(0, left_off);
        // movdqu xmm1, [r13 + right_off]
        self.emit_movdqu_r13_to_xmm(1, right_off);

        if is_float {
            // packed single: addps=0x58, subps=0x5C, mulps=0x59, divps=0x5E
            let opcode = match op {
                BinOp::Add => 0x58,
                BinOp::Sub => 0x5C,
                BinOp::Mul => 0x59,
                BinOp::Div => 0x5E,
                _ => return false,
            };
            self.emit_insn(&Insn::raw_bytes(&[0x0F, opcode, 0xC1])); // addps/subps/mulps/divps xmm0, xmm1
        } else if is_double {
            // packed double: addpd=0x58, subpd=0x5C, mulpd=0x59, divpd=0x5E
            let opcode = match op {
                BinOp::Add => 0x58,
                BinOp::Sub => 0x5C,
                BinOp::Mul => 0x59,
                BinOp::Div => 0x5E,
                _ => return false,
            };
            self.emit_insn(&Insn::raw_bytes(&[0x66, 0x0F, opcode, 0xC1])); // addpd/subpd/mulpd/divpd xmm0, xmm1
        } else {
            // 整数向量: SSE2 packed integer
            match (elem_size, op) {
                (4, BinOp::Add) => {
                    self.emit_insn(&Insn::raw_bytes(&[0x66, 0x0F, 0xFE, 0xC1])); // paddd xmm0, xmm1
                },
                (4, BinOp::Sub) => {
                    self.emit_insn(&Insn::raw_bytes(&[0x66, 0x0F, 0xFA, 0xC1])); // psubd xmm0, xmm1
                },
                (4, BinOp::Mul) => {
                    // pmulld: SSE4.1 66 0F 38 40 /r
                    self.emit_insn(&Insn::raw_bytes(&[0x66, 0x0F, 0x38, 0x40, 0xC1])); // pmulld xmm0, xmm1
                },
                (8, BinOp::Add) => {
                    self.emit_insn(&Insn::raw_bytes(&[0x66, 0x0F, 0xD4, 0xC1])); // paddq xmm0, xmm1
                },
                (8, BinOp::Sub) => {
                    self.emit_insn(&Insn::raw_bytes(&[0x66, 0x0F, 0xFB, 0xC1])); // psubq xmm0, xmm1
                },
                (2, BinOp::Add) => {
                    self.emit_insn(&Insn::raw_bytes(&[0x66, 0x0F, 0xFD, 0xC1])); // paddw xmm0, xmm1
                },
                (2, BinOp::Sub) => {
                    self.emit_insn(&Insn::raw_bytes(&[0x66, 0x0F, 0xF9, 0xC1])); // psubw xmm0, xmm1
                },
                (1, BinOp::Add) => {
                    self.emit_insn(&Insn::raw_bytes(&[0x66, 0x0F, 0xFC, 0xC1])); // paddb xmm0, xmm1
                },
                (1, BinOp::Sub) => {
                    self.emit_insn(&Insn::raw_bytes(&[0x66, 0x0F, 0xF8, 0xC1])); // psubb xmm0, xmm1
                },
                (_, BinOp::And) => {
                    self.emit_insn(&Insn::raw_bytes(&[0x66, 0x0F, 0xDB, 0xC1])); // pand xmm0, xmm1
                },
                (_, BinOp::Or) => {
                    self.emit_insn(&Insn::raw_bytes(&[0x66, 0x0F, 0xEB, 0xC1])); // por xmm0, xmm1
                },
                (_, BinOp::Xor) => {
                    self.emit_insn(&Insn::raw_bytes(&[0x66, 0x0F, 0xEF, 0xC1])); // pxor xmm0, xmm1
                },
                _ => return false, // 不支持的组合回退标量路径
            }
        }

        // movdqu [r13 + result_off], xmm0
        self.emit_movdqu_xmm_to_r13(0, result_off);
        true
    }
    fn vector_init_from_expr(e: &Expr) -> Option<(&Type, usize, &[InitItem])> {
        // 递归剥掉 Cast 包装: (V2SI_d)(V2SI_u){0,1} → Cast → Cast → InitList
        let mut cur = e;
        loop {
            break match cur {
                Expr::CompoundLit {
                    ty: Type::Vector(elem, n),
                    inits,
                    ..
                } => Some((&elem.base, *n, inits.as_slice())),
                Expr::InitList {
                    inits,
                    ty: Type::Vector(elem, n),
                    ..
                } => Some((&elem.base, *n, inits.as_slice())),
                Expr::Cast { e: inner, .. } => {
                    cur = inner.as_ref();
                    continue;
                },
                _ => None,
            };
        }
    }
}
