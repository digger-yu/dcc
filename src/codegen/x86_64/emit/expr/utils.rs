//! emit/expr/utils.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
//! 仅移动/可见性调整，不改语义（backend_refactor_plan §3.1 原则 1）。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::shared::elf::*;
use crate::codegen::shared::*;
use crate::codegen::x86_64::asm::*;
use crate::codegen::x86_64::emit::gas::{
    gp as gas_gp, REG32 as GAS_REG32, REG64 as GAS_REG64,
};
use crate::codegen::x86_64::insn::Insn;
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
    pub(crate) fn binop(
        &mut self,
        op: BinOp,
        dst: u8,
        src: u8,
        is_signed: bool,
        lt: &Type,
        rt: &Type,
    ) {
        let is_cmp =
            matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne);
        // Determine operand size for Add/Sub/And/Or/Xor:
        // - Unsigned 32-bit (uint): no REX.W, result auto zero-extends to 64-bit
        //   This is correct for unsigned arithmetic (masking to 32 bits).
        // - Signed 32-bit (int) or 64-bit: REX.W (64-bit operation)
        //   Signed 32-bit results must be sign-extended, not zero-extended.
        // This is critical for correct unsigned arithmetic: `0U ^ (short)-0x8000`
        // must use 32-bit XOR to get 0xFFFF8000 (zero-extended), not 0xFFFFFFFFFFFF8000.
        let op_size = lt.size().max(rt.size());
        let is_unsigned = !is_signed;
        let use_32bit = !is_cmp
            && !matches!(op, BinOp::Div | BinOp::Mod | BinOp::Mul | BinOp::Shl | BinOp::Shr)
            && op_size <= 4
            && is_unsigned;
        if !is_cmp && !matches!(op, BinOp::Div | BinOp::Mod | BinOp::Mul | BinOp::Shl | BinOp::Shr)
        {
            // P9 方案 C: REX 前缀与 opcode+ModRM 合并为单条 raw_with_gas,
            // 不再分成两次 emit_insn (否则 gas_text 丢了整条指令, 且发射两份 record).
            // Add/Sub/And/Or/Xor (0x01/0x29/...): dst 在 r/m 字段 (需 REX.B),
            // src 在 reg 字段 (需 REX.R). 二者独立判断, 不可 else-if.
            // REX.W=0 for 32-bit ops (auto zero-extend), REX.W=1 for 64-bit ops.
        }
        let r = (src & 7) << 3;
        let m = dst & 7;
        // GAS 宽度: 32 位用 %eXX, 64 位用 %rXX.
        let (src_name, dst_name): (&str, &str) = if use_32bit {
            (GAS_REG32[(src & 15) as usize], GAS_REG32[(dst & 15) as usize])
        } else {
            (GAS_REG64[(src & 15) as usize], GAS_REG64[(dst & 15) as usize])
        };
        // 通用: 拼完整 REX 前缀 (Add/Sub/And/Or/Xor 用).
        let build_rex = |use_32bit: bool| -> u8 {
            let mut rex = if use_32bit { 0x40u8 } else { 0x48u8 };
            if src >= 8 { rex |= 0x04; }
            if dst >= 8 { rex |= 0x01; }
            rex
        };
        match op {
            BinOp::Add => {
                let modrm = 0xC0 | r | m;
                let rex = build_rex(use_32bit);
                let bytes: Vec<u8> = if rex != 0x40 {
                    vec![rex, 0x01, modrm]
                } else {
                    vec![0x01, modrm]
                };
                // AT&T: add src, dst (reg 字段=src 在前, r/m=dst 在后).
                let gas = format!("add {src_name}, {dst_name}");
                self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
            },
            BinOp::Sub => {
                let modrm = 0xC0 | r | m;
                let rex = build_rex(use_32bit);
                let bytes: Vec<u8> = if rex != 0x40 {
                    vec![rex, 0x29, modrm]
                } else {
                    vec![0x29, modrm]
                };
                let gas = format!("sub {src_name}, {dst_name}");
                self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
            },
            BinOp::And => {
                let modrm = 0xC0 | r | m;
                let rex = build_rex(use_32bit);
                let bytes: Vec<u8> = if rex != 0x40 {
                    vec![rex, 0x21, modrm]
                } else {
                    vec![0x21, modrm]
                };
                let gas = format!("and {src_name}, {dst_name}");
                self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
            },
            BinOp::Or => {
                let modrm = 0xC0 | r | m;
                let rex = build_rex(use_32bit);
                let bytes: Vec<u8> = if rex != 0x40 {
                    vec![rex, 0x09, modrm]
                } else {
                    vec![0x09, modrm]
                };
                let gas = format!("or {src_name}, {dst_name}");
                self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
            },
            BinOp::Xor => {
                let modrm = 0xC0 | r | m;
                let rex = build_rex(use_32bit);
                let bytes: Vec<u8> = if rex != 0x40 {
                    vec![rex, 0x31, modrm]
                } else {
                    vec![0x31, modrm]
                };
                let gas = format!("xor {src_name}, {dst_name}");
                self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
            },
            BinOp::Mul => {
                // imul dst, src: dst is in reg field (needs REX.R), src in r/m (needs REX.B)
                let mut rex = 0x48u8;
                if dst >= 8 {
                    rex |= 0x04;
                }
                if src >= 8 {
                    rex |= 0x01;
                }
                use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                let gas = format!("imul {}, {}", GAS_REG64[(src & 15) as usize], GAS_REG64[(dst & 15) as usize]);
                self.emit_insn(&Insn::raw_with_gas(&[rex, 0x0F, 0xAF, 0xC0 | ((dst & 7) << 3) | (src & 7)], &gas));
            },
            BinOp::Div | BinOp::Mod => {
                // 有符号: cqto (符号扩展 rax→rdx:rax) + idiv
                // 无符号: xor rdx,rdx (清零 rdx) + div
                // 32位无符号运算前需零扩展操作数到64位
                let op_size = lt.size().max(rt.size());
                let need_zero_ext = !is_signed && op_size <= 4;
                if src == 2 {
                    self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // push rdx (save divisor)
                }
                if dst != 0 {
                    self.mov(0, dst); // rax = 被除数
                }
                // 零扩展被除数 (rax) 从32位到64位
                if need_zero_ext {
                    self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            ));; // mov eax, eax (zero-extends to rax)
                }
                if src == 2 {
                    self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // pop rcx (restore divisor)
                                        // 零扩展除数 (rcx) 从32位到64位
                    if need_zero_ext {
                        self.emit_insn(&Insn::raw_bytes(&[0x89, 0xC9])); // mov ecx, ecx (zero-extends to rcx)
                    }
                    if is_signed {
                        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x99])); // cqto
                        self.emit_insn(&Insn::raw_bytes(&[0x48, 0xF7, 0xF9])); // idiv rcx
                    } else {
                        // P11: xor rdx, rdx via typed opcode.
                        let insn = Insn::new(
                            crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                                op: crate::codegen::x86_64::opcode::AluOp::Xor,
                                width: crate::codegen::shared::asm_operand::Width::B64,
                                dst: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64),
                                src: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64),
                            },
                            &[],
                        );
                        if !self.emit_insn(&insn) {
                            self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::AluRm { op: crate::codegen::x86_64::opcode::AluOp::Xor, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // xor rdx, rdx
                        }
                        self.emit_insn(&Insn::raw_bytes(&[0x48, 0xF7, 0xF1])); // div rcx
                    }
                } else {
                    // 零扩展除数从32位到64位
                    if need_zero_ext {
                        // mov src, src (zero-extends to 64-bit)
                        let src_byte = src & 7;
                        self.emit_insn(&Insn::raw_bytes(&[0x89, 0xC0 | (src_byte << 3) | src_byte]));
                    }
                    if is_signed {
                        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x99])); // cqto
                        let mut rex = 0x48u8;
                        if src >= 8 {
                            rex |= 0x41;
                        }
                        self.emit_insn(&Insn::raw_bytes(&[rex, 0xF7, 0xF8 | (src & 7)])); // idiv src
                    } else {
                        // P11: xor rdx, rdx via typed opcode.
                        let insn = Insn::new(
                            crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                                op: crate::codegen::x86_64::opcode::AluOp::Xor,
                                width: crate::codegen::shared::asm_operand::Width::B64,
                                dst: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64),
                                src: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64),
                            },
                            &[],
                        );
                        if !self.emit_insn(&insn) {
                            self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::AluRm { op: crate::codegen::x86_64::opcode::AluOp::Xor, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // xor rdx, rdx
                        }
                        let mut rex = 0x48u8;
                        if src >= 8 {
                            rex |= 0x41;
                        }
                        self.emit_insn(&Insn::raw_bytes(&[rex, 0xF7, 0xF0 | (src & 7)])); // div src
                    }
                }
                if matches!(op, BinOp::Mod) {
                    self.mov(dst, 2);
                } else if dst != 0 {
                    self.mov(dst, 0); // 商在 rax → dst
                }
            },
            BinOp::Shl | BinOp::Shr => {
                // Shl: shl (0xE0)
                // Shr: 有符号用 sar (0xF8, 算术右移), 无符号用 shr (0xE8, 逻辑右移)
                let sub: u8 = if matches!(op, BinOp::Shl) {
                    0xE0
                } else if is_signed {
                    0xF8 // sar: 算术右移, 高位补符号位
                } else {
                    0xE8 // shr: 逻辑右移, 高位补 0
                };
                if src == 1 {
                    // 移位量已在 cl
                    let mut rex = 0x48u8;
                    if dst >= 8 {
                        rex |= 0x41;
                    }
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0xD3, sub | m]));
                } else if dst == 1 {
                    // dst=rcx 且移位量在 src: xchg 后在 src 上移位, 再搬回 rcx
                    let mut rex = 0x48u8;
                    if src >= 8 {
                        rex |= 0x41;
                    }
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0x87, 0xC8 | (src & 7)])); // xchg rcx, src
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0xD3, sub | (src & 7)])); // shl/shr src, cl
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0x8B, 0xC8 | (src & 7)])); // mov rcx, src
                } else {
                    // 移位量搬到 cl 后对 dst 移位
                    let mut rex1 = 0x48u8;
                    if src >= 8 {
                        rex1 |= 0x41;
                    }
                    self.emit_insn(&Insn::raw_bytes(&[rex1, 0x8B, 0xC8 | (src & 7)])); // mov rcx, src
                    let mut rex = 0x48u8;
                    if dst >= 8 {
                        rex |= 0x41;
                    }
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0xD3, sub | m]));
                }
            },
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne => {
                // 确定比较的位宽: 如果两个操作数都是 32 位或更小, 使用 32 位比较
                // 否则使用 64 位比较 (避免函数返回值高位垃圾数据问题)
                let lt_size = lt.size();
                let rt_size = rt.size();
                let use_32bit = lt_size <= 4 && rt_size <= 4;

                // 无符号/混合符号比较: 零扩展操作数到 64 位.
                // 混合比较时, 有符号操作数符号扩展 (高位为 1), 无符号操作数
                // 零扩展 (高位为 0), 64 位 cmp 结果错误. 零扩展修正此问题.
                if !is_signed && !use_32bit {
                    self.zero_extend_for_cmp(dst, lt);
                    self.zero_extend_for_cmp(src, rt);
                }
                // `cmp r/m(dst), r(src)`：dst 在 r/m 字段 ⇒ 只可能贡献 REX.B，src 在 reg
                // 字段 ⇒ 只可能贡献 REX.R。两个字段各自独立判，不可 else-if：P16 把右操作数
                // 暂存号从写死的 %rcx 换成候选表首号（%r8）之后，「src 与 dst 同属高 8 号」
                // 从死分支变成常走分支，漏掉的 REX.B 会让 `cmp %r8d,%r12d` 解码成
                // `cmp %r8d,%esp`（mod=3 rm=100 是非法编码，objdump 直接点名 %esp）。
                // 位序判据见 docs/ssa-pass-upgrade-plan.md §7.21，机器判据见 rex_matrix::binop_matrix。
                // P9 方案 C: cmp+setcc+movzbq 合并发射时挂 gas_text, 避免事后反推.
                let (cmp_src_name, cmp_dst_name): (&str, &str) = if use_32bit {
                    (GAS_REG32[(src & 15) as usize], GAS_REG32[(dst & 15) as usize])
                } else {
                    (GAS_REG64[(src & 15) as usize], GAS_REG64[(dst & 15) as usize])
                };
                let mut rex = if use_32bit { 0u8 } else { 0x48u8 };
                if src >= 8 {
                    rex |= 0x44;
                }
                if dst >= 8 {
                    rex |= 0x41;
                }
                // 32 位比较且两侧都在低 8 号 ⇒ rex 为 0，不发前缀（64 位的 0x48 是 W 位）。
                let modrm = 0xC0 | ((src & 7) << 3) | (dst & 7);
                let cmp_bytes: Vec<u8> = if rex != 0 {
                    vec![rex, 0x39, modrm]
                } else {
                    vec![0x39, modrm]
                };
                // AT&T 序: cmp src, dst (reg 字段在前, r/m 字段在后).
                self.emit_insn(&Insn::raw_with_gas(&cmp_bytes, &format!("cmp {cmp_src_name}, {cmp_dst_name}")));
                // setcc al + movzbq %al, %rax 拆开各自挂 gas.
                let (cc, cc_letter): (u8, &str) = match op {
                    BinOp::Lt if is_signed => (0x9C, "setl"),
                    BinOp::Lt => (0x92, "setb"),
                    BinOp::Le if is_signed => (0x9E, "setle"),
                    BinOp::Le => (0x96, "setbe"),
                    BinOp::Gt if is_signed => (0x9F, "setg"),
                    BinOp::Gt => (0x97, "seta"),
                    BinOp::Ge if is_signed => (0x9D, "setge"),
                    BinOp::Ge => (0x93, "setae"),
                    BinOp::EqEq => (0x94, "sete"),
                    BinOp::Ne => (0x95, "setne"),
                    _ => unreachable!(),
                };
                self.emit_insn(&Insn::raw_with_gas(&[0x0F, cc, 0xC0], &format!("{cc_letter} %al")));
                self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x0F, 0xB6, 0xC0], "movzbq %al, %rax"));
                if dst != 0 {
                    self.mov(dst, 0);
                }
            },
            // AndAnd/OrOr 由 gen_expr 短路求值处理, 不应到达此处.
            // 使用显式匹配而非通配符, 确保新增 BinOp 变体时编译器
            // 报 non-exhaustive match 错误 (而非运行时 panic).
            BinOp::AndAnd | BinOp::OrOr => {
                unreachable!("logical ops are lowered via short-circuit in gen_expr")
            },
        }
    }
    pub(crate) fn cc_letter_to_setcc_opcode(letters: &str) -> Option<u8> {
        let off = match letters {
            "o" => 0x00,
            "no" => 0x01,
            "b" | "c" | "nae" => 0x02,
            "ae" | "nb" | "nc" => 0x03,
            "e" | "z" => 0x04,
            "ne" | "nz" => 0x05,
            "be" | "na" => 0x06,
            "a" | "nbe" => 0x07,
            "s" => 0x08,
            "ns" => 0x09,
            "p" | "pe" => 0x0A,
            "np" | "po" => 0x0B,
            "l" | "nge" => 0x0C,
            "ge" | "nl" => 0x0D,
            "le" | "ng" => 0x0E,
            "g" | "nle" => 0x0F,
            _ => return None,
        };
        Some(0x90 + off)
    }
    pub(crate) fn classify_field(ty: &Type, base: usize, classes: &mut [AbiClass]) {
        let size = ty.size();
        if size == 0 {
            return;
        }
        match ty {
            Type::Struct(_, fields, _, false) => {
                for field in fields.iter() {
                    Self::classify_field(&field.ty, base + field.offset, classes);
                }
            },
            Type::Union(_, fields, _, false) => {
                // 联合各字段共享同一基址, 各自分类后合并
                for field in fields.iter() {
                    let f_sz = field.ty.size();
                    if f_sz == 0 {
                        continue;
                    }
                    let n = f_sz.div_ceil(8).min(classes.len() - base / 8);
                    let mut f_classes = vec![AbiClass::NoClass; n];
                    Self::classify_field(&field.ty, 0, &mut f_classes);
                    let start = base / 8;
                    for i in 0..n {
                        if start + i < classes.len() {
                            classes[start + i] =
                                Self::merge_classes(classes[start + i], f_classes[i]);
                        }
                    }
                }
            },
            Type::Array(elem, ArraySize::Const(n)) => {
                for j in 0..*n {
                    Self::classify_field(&elem.base, base + j * elem.size(), classes);
                }
            },
            Type::Atomic(inner) => Self::classify_field(&inner.base, base, classes),
            Type::Vector(_, _) => {
                // SIMD 向量: 首 eightbyte Sse, 后续 SseUp (向量延续)
                let start = base / 8;
                let end = (base + size.saturating_sub(1)) / 8;
                for i in start..=end {
                    if i < classes.len() {
                        let cls = if i == start { AbiClass::Sse } else { AbiClass::SseUp };
                        classes[i] = Self::merge_classes(classes[i], cls);
                    }
                }
            },
            _ => {
                // 叶类型: 直接分类到 classes
                let start = base / 8;
                let end = (base + size.saturating_sub(1)) / 8;
                let cls = Self::leaf_abi_class(ty);
                for i in start..=end {
                    if i < classes.len() {
                        classes[i] = Self::merge_classes(classes[i], cls);
                    }
                }
            },
        }
    }
    // complex 静态初始化器只有一份真源：mem/global.rs。历史上这里有第二份
    // 逐字副本（走 elf/init_list.rs），只改一份会出现“测试全绿但 .data
    // 现象不变”。以下关联函数全部委托 global，保证两条路径语义一致。
    pub(crate) fn complex_init_bytes(init: &Expr, inner: &Type) -> Option<Vec<u8>> {
        crate::codegen::x86_64::mem::global::complex_init_bytes(init, inner)
    }
    fn complex_init_parts(e: &Expr, inner: &Type) -> Option<(FloatConst, FloatConst)> {
        crate::codegen::x86_64::mem::global::complex_init_parts(e, inner)
    }
    fn complex_init_parts_i(e: &Expr) -> Option<(i128, i128)> {
        crate::codegen::x86_64::mem::global::complex_init_parts_i(e)
    }
    /// 浮点初始化器 → 数据字节的**单一真源**。
    ///
    /// 接收完整的 FloatConst：LongDouble 直出真 80-bit 编码（10 字节），
    /// 目标 16 字节对象再补 6 字节 padding；F32/F64 按目标宽度截断。
    /// 以前这些站点各自内联写"低 8 字节 = f64 位型 + 高 8 零"，
    /// 而读侧在对象表示迁移后已统一改成 `fld tbyte` —— 于是写出的字节被
    /// 当 80-bit 解释：`long double g = 1.0L` 的 f64 位型落在 80-bit 的
    /// 尾数域，指数域变成高两个零字节 → 指数 0 且整数位 0 → 读回 `-nan`。
    pub(crate) fn float_data_bytes(c: FloatConst, size: usize) -> Vec<u8> {
        match c {
            FloatConst::LongDouble(b) => {
                // 真 80-bit 编码直出；16 字节对象补 6 字节 padding。
                let mut out = b.to_vec();
                if out.len() < size {
                    out.resize(size, 0);
                }
                if out.len() > size {
                    out.truncate(size);
                }
                out
            },
            FloatConst::F32(f) => {
                let n = size.clamp(1, 4);
                f.to_le_bytes()[..n].to_vec()
            },
            FloatConst::F64(f) => {
                if size <= 4 {
                    let n = size.max(1);
                    return (f as f32).to_bits().to_le_bytes()[..n].to_vec();
                }
                let bytes = f.to_bits().to_le_bytes();
                let mut b = bytes[..size.min(8)].to_vec();
                if b.len() < size {
                    b.resize(size, 0);
                }
                b
            },
        }
    }
    fn encode_complex_parts_f(re: FloatConst, im: FloatConst, inner: &Type) -> Vec<u8> {
        crate::codegen::x86_64::mem::global::encode_complex_parts_f(re, im, inner)
    }
    fn encode_complex_parts_i(re: i128, im: i128, inner: &Type) -> Vec<u8> {
        crate::codegen::x86_64::mem::global::encode_complex_parts_i(re, im, inner)
    }
    pub(crate) fn eval_asm_operand(expr: &Expr) -> Option<String> {
        match const_eval::eval_full(expr, false) {
            Ok(ConstValue::Int(v)) => return Some(v.to_string()),
            Ok(ConstValue::Float(f)) => return Some((f.as_f64() as i128).to_string()),
            Ok(ConstValue::Addr { name, offset }) => {
                return Some(Self::format_asm_symbol_immediate(&name, offset));
            },
            Ok(ConstValue::Str { label, .. }) | Ok(ConstValue::Bytes { label, .. }) => {
                return Some(label);
            },
            Err(_) => {},
        }
        if let Expr::Offsetof { ty, member, .. } = expr {
            return Some(Self::compute_offset(ty, member).to_string());
        }
        None
    }
    pub(crate) fn eval_const_expr(
        &mut self,
        e: &Expr,
        rel: Option<(u64, Option<usize>)>,
        width: usize,
        is_float: bool,
        rodata_slots: bool,
    ) -> Vec<u8> {
        // 字符串字面量: 不能走 eval_full —— 它返回 ConstValue::Str, 落到
        // 下方 _ 分支会被 0 填充 (char buf[][8] = {"hello"} 元素全为 0)。
        // 指针类型由调用方在 Ptr 上下文特判 (emit_string_ptr_slot);
        // 此处 (char 数组元素等) 返回内容字节。
        if let Expr::String { data, .. } = e {
            // 截断到 width (如 char[3] = "1234" 只取前 3 字节 "123")
            let mut d = data.clone();
            if width > 0 && d.len() > width {
                d.truncate(width);
            }
            return d;
        }
        // 整型复数字面量: 两分量按 i128 精确 LE 编码，全程不经浮点。
        if let Expr::ComplexIntLit { real, imag, ty, .. } = e {
            let inner = match ty {
                Type::Complex(i) | Type::Imaginary(i) => &i.base,
                other => other,
            };
            let comp_sz = inner.size().max(1).min(16);
            let mut data: Vec<u8> = Vec::new();
            for v in [*real, *imag] {
                data.extend_from_slice(&v.to_le_bytes()[..comp_sz]);
            }
            if width > 0 && data.len() > width {
                data.truncate(width);
            }
            while data.len() < width {
                data.push(0);
            }
            return data;
        }
        // 复数字面量: 必须特殊处理, 因为 eval_full 只返回虚部
        if let Expr::ComplexLit { real, imag, ty, .. } = e {
            // 分量编码由字面量自身类型的 inner 决定: Float→f32 位模式,
            // Double/LongDouble→f64 位模式 (LD 补零到 16B 槽),
            // 整数→整数值 LE 字节。不能用 width/2 猜测分量宽度:
            // width 可能是单分量尺寸 (Complex arm) 或全复数尺寸, 且
            // 整数 inner 目标必须是整数值而非 f64/f32 位模式。
            let inner = match ty {
                Type::Complex(i) | Type::Imaginary(i) => &i.base,
                other => other,
            };
            let comp_sz = inner.size().max(1);
            let mut data: Vec<u8> = Vec::new();
            for v in [*real, *imag] {
                match inner {
                    Type::Float => {
                        data.extend_from_slice(&(v.as_f64() as f32).to_bits().to_le_bytes())
                    },
                    Type::LongDouble => {
                        // 真 80-bit 编码（16B 槽 = 10 字节 + 6 字节 padding）
                        let mut bits = match v {
                            FloatConst::LongDouble(b) => b.to_vec(),
                            other => crate::float80::from_f64(other.as_f64()).to_vec(),
                        };
                        bits.resize(16, 0);
                        data.extend_from_slice(&bits);
                    },
                    Type::Double => data.extend_from_slice(&v.as_f64().to_bits().to_le_bytes()),
                    _ => {
                        let b = (v.as_f64() as i128).to_le_bytes();
                        data.extend_from_slice(&b[..comp_sz.min(16)]);
                    },
                }
            }
            // 分量上下文 (width < 两分量) 只取前部; 不足补零到 width
            if width > 0 && data.len() > width {
                data.truncate(width);
            }
            while data.len() < width {
                data.push(0);
            }
            return data;
        }
        // 优先用 eval_full: 浮点字面量直接走 .to_bits(), 不被截断为整数
        // &&label: 提前处理, 前缀标签名以避免与函数符号名冲突
        if let Expr::LabelAddr { label, .. } = e {
            let size = width.max(8);
            if let Some((off, sec)) = rel {
                let rel_type = if width <= 4 { R_X86_64_32 } else { R_X86_64_64 };
                let prefixed = self.c_label_to_asm(&label.name);
                self.pending_data64_rels
                    .push((off, prefixed, sec, rel_type, 0));
            }
            return vec![0u8; size];
        }
        // &&label_a - &&label_b: label address difference (computed goto tables).
        // 编译期尚不知 label 偏移, 记录待回填, 函数编译完后用 self.labels
        // 已知偏移计算差值. 同时处理 Cast 包装: (int)&&lab_a - (int)&&lab_b.
        if let Expr::Binop {
            op: crate::ast::BinOp::Sub,
            l,
            r,
            ..
        } = e
        {
            let (la, lb) = (Self::extract_label_addr(l), Self::extract_label_addr(r));
            if let (Some(label_a), Some(label_b)) = (la, lb) {
                let prefixed_a = self.c_label_to_asm(&label_a);
                let prefixed_b = self.c_label_to_asm(&label_b);
                if let Some((off, _sec)) = rel {
                    let size = width.max(4);
                    self.pending_label_diff_rels
                        .push((off, prefixed_a, prefixed_b, size));
                }
                return vec![0u8; width.max(4)];
            }
        }
        // &(T){...} / &{...}: 嵌套 AddrOf+CompoundLit 路径
        // (gen_global_init_list 处理 struct 指针字段时走 eval_const_expr,
        // 而 eval_full 不支持 CompoundLit 取地址, 必须显式 materialize)
        if let Expr::AddrOf { e: inner, .. } = e {
            if self.relocatable {
                // ET_REL: 物化到 .rodata, 登记 LOCAL 符号, 返回占位+重定位
                if let Some((anon_name, rodata_off)) = self.materialize_compound_literal_rel(inner)
                {
                    let sz = width.max(8);
                    let off = rel.map(|(o, _)| o).unwrap_or(0);
                    let sec = rel.and_then(|(_, s)| s);
                    let rel_type = if width <= 4 { R_X86_64_32 } else { R_X86_64_64 };
                    if matches!(sec, Some(v) if v == usize::MAX) {
                        // 当前在 rodata section 内: 发射 .rela.rodata 重定位
                        self.pending_rodata_rels.push((off, anon_name, rel_type, 0));
                    } else {
                        // 在 data section (或 custom section) 内: 走 data 重定位
                        self.pending_data64_rels
                            .push((off, anon_name, sec, rel_type, 0));
                    }
                    let _ = rodata_off; // 符号已登记, 链接时自动解析
                    return vec![0u8; sz];
                }
            } else {
                // ET_EXEC: 物化, 返回绝对地址字节 (compile_exec 直接写死)
                if let Some((anon_name, vaddr)) =
                    self.materialize_compound_literal(inner, rodata_slots)
                {
                    let sz = inner.ty().size().max(1);
                    let rodata_offset = vaddr - self.entry;
                    self.globals_raw.insert(
                        anon_name.clone(),
                        GlobalVar {
                            offset: rodata_offset,
                            size: sz,
                            ty: inner.ty().clone(),
                            init: None,
                            section: 1,
                            is_tls: false,
                            in_data_section: false,
                        },
                    );
                    return vaddr.to_le_bytes().to_vec();
                }
            }
        }
        if let Ok(cv) = const_eval::eval_full(e, false) {
            match cv {
                const_eval::ConstValue::Float(f) => {
                    // 按目标宽度截断: 窄元素 (char/short) 数组不能用表达式
                    // 类型宽度 (int=4) 写数据, 否则元素会错位/越界。
                    // LongDouble 经单一真源直出**真 80-bit 编码**。
                    Self::float_data_bytes(f, width.max(1))
                },
                const_eval::ConstValue::Int(v) => {
                    let size = width.max(1);
                    if is_float {
                        // 浮点目标：整型常量在目标精度上精确 RNE 转换
                        // （旧写法经 `v as f64` 中转，|v| >= 2^53 会丢精度）。
                        let target_ty = if size >= 10 {
                            Type::LongDouble
                        } else if size <= 4 {
                            Type::Float
                        } else {
                            Type::Double
                        };
                        let c = FloatConst::from_int(&target_ty, v);
                        Self::float_data_bytes(c, size)
                    } else {
                        let bytes = v.to_le_bytes();
                        bytes[..size.min(bytes.len())].to_vec()
                    }
                },
                const_eval::ConstValue::Addr { name, offset } => {
                    // &&label or &global: emit data relocation
                    // &&label names are prefixed with .Ll_ to avoid collision
                    // with function symbol names
                    let size = width.max(8);
                    if let Some((off, sec)) = rel {
                        let rel_type = if width <= 4 { R_X86_64_32 } else { R_X86_64_64 };
                        self.pending_data64_rels
                            .push((off, name, sec, rel_type, offset as i64));
                    }
                    vec![0u8; size]
                },
                _ => {
                    if let Ok(value) = const_eval::eval(e) {
                        let size = width.max(1);
                        let bytes = value.to_le_bytes();
                        return bytes[..size.min(bytes.len())].to_vec();
                    }
                    vec![0u8; width.max(e.ty().size())]
                },
            }
        } else {
            match e {
                Expr::String { data, .. } => data.clone(),
                Expr::Ident { name, .. } => {
                    if let Some((off, sec)) = rel {
                        let rel_type = if width <= 4 { R_X86_64_32 } else { R_X86_64_64 };
                        self.pending_data64_rels
                            .push((off, name.name.clone(), sec, rel_type, 0));
                    }
                    vec![0u8; width.max(8)]
                },
                Expr::AddrOf { e: inner, .. } => match inner.as_ref() {
                    Expr::Ident { .. } => {
                        self.eval_const_expr(inner, rel, width, is_float, rodata_slots)
                    },
                    Expr::Index {
                        arr,
                        idx,
                        ty: elem_ty,
                        ..
                    } => {
                        if let (Expr::Ident { name, .. }, Ok(i)) =
                            (arr.as_ref(), const_eval::eval(idx))
                        {
                            if let Some((off, sec)) = rel {
                                let rel_type = if width <= 4 { R_X86_64_32 } else { R_X86_64_64 };
                                let addend = (i as i64) * elem_ty.size().max(1) as i64;
                                self.pending_data64_rels.push((
                                    off,
                                    name.name.clone(),
                                    sec,
                                    rel_type,
                                    addend,
                                ));
                            }
                            vec![0u8; width.max(8)]
                        } else if let (Expr::String { data, .. }, Ok(i)) =
                            (arr.as_ref(), const_eval::eval(idx))
                        {
                            // &"str"[i]: string literal element address.
                            // Use string pool to ensure the string is in
                            // rodata and the pointer slot is patched to
                            // the absolute address (ET_EXEC) or has a
                            // proper relocation (ET_REL).
                            let byte_off = (i as usize).min(data.len());
                            self.emit_string_ptr_slot(rel, &data[byte_off..], rodata_slots)
                        } else {
                            vec![0u8; width.max(e.ty().size())]
                        }
                    },
                    _ => vec![0u8; width.max(e.ty().size())],
                },
                // Strip casts to handle (void *)&"str"[0],
                // (char *)&"hello"[1], etc.
                Expr::Cast { e: inner, .. } => {
                    self.eval_const_expr(inner, rel, width, is_float, rodata_slots)
                },
                // String pointer arithmetic: "str" + n, n + "str",
                // with optional Cast wrappers.
                Expr::Binop {
                    op: crate::ast::BinOp::Add,
                    l,
                    r,
                    ..
                } if matches!(l.ty(), Type::Array(_, _) | Type::Ptr(_))
                    || matches!(r.ty(), Type::Array(_, _) | Type::Ptr(_)) =>
                {
                    self.eval_string_ptr_arith(l, r, rel, width, rodata_slots)
                },
                Expr::LabelAddr { label, .. } => {
                    // &&label in data section: record for later patching
                    // Prefix label name to avoid collision with function symbols
                    if let Some((off, sec)) = rel {
                        let rel_type = if width <= 4 { R_X86_64_32 } else { R_X86_64_64 };
                        let prefixed = self.c_label_to_asm(&label.name);
                        self.pending_data64_rels
                            .push((off, prefixed, sec, rel_type, 0));
                    }
                    vec![0u8; width.max(8)]
                },
                _ => vec![0u8; width.max(e.ty().size())],
            }
        }
    }
    pub(crate) fn eval_global_lvalue_offset(
        e: &Expr,
        globals: &std::collections::HashMap<String, GlobalVar>,
    ) -> Option<(String, i64)> {
        match e {
            Expr::Ident { name, .. } => {
                if globals.contains_key(&name.name) {
                    Some((name.name.clone(), 0))
                } else {
                    None
                }
            },
            Expr::Index { arr, idx, .. } => {
                let (base, off) = Self::eval_global_lvalue_offset(arr, globals)?;
                // Evaluate the index as a constant
                let idx_val = const_eval::eval(idx).ok()?;
                // Element size = sizeof the type that arr points to
                // (i.e., the element type of the array)
                let arr_ty = arr.ty();
                let elem_sz = match &arr_ty {
                    Type::Array(elem, _) => elem.size() as i128,
                    Type::Ptr(elem) => elem.size() as i128,
                    _ => arr_ty.size() as i128,
                };
                Some((base, off + (idx_val * elem_sz) as i64))
            },
            Expr::Member {
                expr: inner, field, ..
            } => {
                let (base, off) = Self::eval_global_lvalue_offset(inner, globals)?;
                // Compute field offset within the struct/union
                let inner_ty = inner.ty();
                // For arrow (->), dereference the pointer first
                let struct_ty: &Type = match &inner_ty {
                    Type::Ptr(ref t) => &t.base,
                    other => other,
                };
                let field_off = match struct_ty {
                    Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, false) => {
                        let mut cur = 0i64;
                        let is_union = matches!(struct_ty, Type::Union(_, _, _, _));
                        for f in fields.iter() {
                            if is_union {
                                if f.name == field.name {
                                    cur = 0;
                                    break;
                                }
                            } else {
                                let f_align = f.ty.align() as i64;
                                cur = (cur + f_align - 1) & !(f_align - 1);
                                if f.name == field.name {
                                    break;
                                }
                                cur += f.ty.size() as i64;
                            }
                        }
                        cur
                    },
                    _ => return None,
                };
                Some((base, off + field_off))
            },
            Expr::Deref { .. } => {
                // &(*ptr) = ptr, but ptr must be a constant address
                // This is rare in global init; skip for now
                None
            },
            _ => None,
        }
    }
    fn eval_string_ptr_arith(
        &mut self,
        l: &Expr,
        r: &Expr,
        rel: Option<(u64, Option<usize>)>,
        width: usize,
        rodata_slots: bool,
    ) -> Vec<u8> {
        // Strip casts to find the underlying expressions
        let l_inner = Self::strip_casts(l);
        let r_inner = Self::strip_casts(r);
        // Try to find (String, Int) or (Int, String) pattern
        let (data, offset) = match (l_inner, r_inner) {
            (Expr::String { data, .. }, _) => {
                if let Ok(n) = const_eval::eval(r) {
                    (data, n as usize)
                } else {
                    return vec![0u8; width.max(8)];
                }
            },
            (_, Expr::String { data, .. }) => {
                if let Ok(n) = const_eval::eval(l) {
                    (data, n as usize)
                } else {
                    return vec![0u8; width.max(8)];
                }
            },
            _ => return vec![0u8; width.max(8)],
        };
        let byte_off = offset.min(data.len());
        self.emit_string_ptr_slot(rel, &data[byte_off..], rodata_slots)
    }
    /// 副作用判据：**实体已移到后端无关的语义层**（`crate::semantics::expr_has_side_effects`）。
    ///
    /// 为什么：同一件判据 codegen（复合赋值单求值）与 SSA 构建器（自增自减/复合赋值的
    /// 读—改—写）两边都要用，只写在后端里会让 SSA 侧没法区分「重复降级无害」与
    /// 「把 `i++` 跑两遍」。这里保留 `Self::` 入口是为了不拆改动 20 多个调用点。
    pub(crate) fn expr_has_side_effects(e: &Expr) -> bool {
        crate::semantics::expr_has_side_effects(e)
    }
    pub(crate) fn expr_span(e: &Expr) -> &crate::diagnostics::Span {
        match e {
            Expr::Int { span, .. }
            | Expr::Float { span, .. }
            | Expr::ComplexLit { span, .. }
            | Expr::ComplexIntLit { span, .. }
            | Expr::Real { span, .. }
            | Expr::Imag { span, .. }
            | Expr::String { span, .. }
            | Expr::Char { span, .. }
            | Expr::Ident { span, .. }
            | Expr::Binop { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Call { span, .. }
            | Expr::Assign { span, .. }
            | Expr::Cond { span, .. }
            | Expr::Cast { span, .. }
            | Expr::SizeofTy { span, .. }
            | Expr::SizeofExpr { span, .. }
            | Expr::Index { span, .. }
            | Expr::Member { span, .. }
            | Expr::AddrOf { span, .. }
            | Expr::Deref { span, .. }
            | Expr::PostInc { span, .. }
            | Expr::PostDec { span, .. }
            | Expr::PreInc { span, .. }
            | Expr::PreDec { span, .. }
            | Expr::CompoundLit { span, .. }
            | Expr::InitList { span, .. }
            | Expr::AlignofTy { span, .. }
            | Expr::AlignofExpr { span, .. }
            | Expr::StmtExpr { span, .. }
            | Expr::Comma { span, .. }
            | Expr::AsmExpr { span, .. }
            | Expr::LabelAddr { span, .. }
            | Expr::VaArg { span, .. }
            | Expr::Offsetof { span, .. } => span,
        }
    }
    fn expr_src_pos(&self, span: &crate::diagnostics::Span) -> (String, u32, u32) {
        let fallback_file = if span.file.is_empty() {
            self.source_name.clone()
        } else {
            span.file.to_string()
        };
        let line = span.line;
        if line == 0 || self.line_map.is_empty() {
            return (fallback_file, line, span.col);
        }
        let idx = self.line_map.partition_point(|s| s.out_start <= line);
        if let Some(ls) = idx.checked_sub(1).and_then(|i| self.line_map.get(i)) {
            if line < ls.out_start + ls.count {
                return (ls.file.clone(), ls.src_start + (line - ls.out_start), span.col);
            }
            return (ls.file.clone(), line, span.col);
        }
        if let Some(first) = self.line_map.first() {
            return (first.file.clone(), first.src_start.max(1), span.col);
        }
        (fallback_file, line, span.col)
    }
    /// 结构相等判据：**实体已移到后端无关的语义层**，与上面的副作用判据同一理由
    /// （复合赋值的「r 里哪一半是 lvalue 拷贝」在 codegen 与 SSA 两侧都要判）。
    pub(crate) fn exprs_structurally_equal(a: &Expr, b: &Expr) -> bool {
        crate::semantics::exprs_structurally_equal(a, b)
    }
    pub(crate) fn expr_warn(&self, e: &Expr, msg: &str) {
        eprintln!("{}", self.expr_warning(e, msg));
    }
    fn expr_warning(&self, e: &Expr, msg: &str) -> String {
        let span = Self::expr_span(e);
        let (file, line, col) = self.expr_src_pos(span);
        if line > 0 && col > 0 {
            format!("{}:{}:{}: warning: {}", file, line, col, msg)
        } else if line > 0 {
            format!("{}:{}: warning: {}", file, line, msg)
        } else {
            format!("{}: warning: {}", file, msg)
        }
    }
    fn extract_label_addr(e: &Expr) -> Option<String> {
        let mut cur = e;
        loop {
            match cur {
                Expr::LabelAddr { label, .. } => return Some(label.name.clone()),
                Expr::Cast { e: inner, .. } => cur = inner.as_ref(),
                _ => return None,
            }
        }
    }
    pub(crate) fn find_expr_operator(expr: &str) -> (&str, Option<char>, &str) {
        let bytes = expr.as_bytes();
        let mut depth = 0i32;
        let mut in_quote = false;
        // 从右向左扫描，找到最右边的 +/- (不在括号/引号内)
        for i in (0..bytes.len()).rev() {
            match bytes[i] {
                b'"' => in_quote = !in_quote,
                b')' if !in_quote => depth += 1,
                b'(' if !in_quote => depth -= 1,
                b'+' | b'-' if !in_quote && depth == 0 && i > 0 => {
                    return (&expr[..i], Some(bytes[i] as char), &expr[i + 1..]);
                },
                _ => {},
            }
        }
        (expr, None, "")
    }
    fn format_asm_symbol_immediate(name: &str, offset: i128) -> String {
        match offset.cmp(&0) {
            std::cmp::Ordering::Greater => format!("{name}+{offset}"),
            std::cmp::Ordering::Less => format!("{name}{offset}"),
            std::cmp::Ordering::Equal => name.to_string(),
        }
    }
    pub(crate) fn global_init_bytes(
        init: &Expr,
        sz: usize,
        is_float: bool,
        ty: &Type,
    ) -> Option<Vec<u8>> {
        // 单一真源：全局/静态初始化字节一律由 mem/global.rs 负责，
        // 这里仅保留关联函数入口供 elf/init_list.rs 调用，避免再出现
        // 一份经 f64 中转的副本（见 global.rs 中的根因注释）。
        crate::codegen::x86_64::mem::global::global_init_bytes(init, sz, is_float, ty)
    }
    // 号→名（含 AT&T 形态）只有一份表：`abi/regnames.rs::att_name_of_num`
    // （Phase 6.1 的后半）。这里以前是与 `asm/expansion.rs` 逐字相同的第二份副本。
    pub(crate) fn has_field_data_in_range(ty: &Type, lo: usize, hi: usize) -> bool {
        match ty {
            Type::Struct(_, fields, _, false) => {
                for f in fields.iter() {
                    let f_lo = f.offset;
                    let f_hi = f.offset + f.ty.size();
                    if f_lo < hi && f_hi > lo {
                        return true;
                    }
                }
                false
            },
            Type::Union(_, fields, _, false) => {
                for f in fields.iter() {
                    if 0 < hi && f.ty.size() > lo {
                        return true;
                    }
                }
                false
            },
            Type::Array(elem, ArraySize::Const(n)) => {
                for j in 0..*n {
                    let e_lo = j * elem.size();
                    let e_hi = e_lo + elem.size();
                    if e_lo < hi && e_hi > lo {
                        return true;
                    }
                }
                false
            },
            Type::Atomic(inner) => Self::has_field_data_in_range(&inner.base, lo, hi),
            // 标量/向量/浮点: 整个范围都是数据
            _ => true,
        }
    }
    pub(crate) fn is_asm_directive(name: &str) -> bool {
        matches!(
            name,
            ".pushsection"
                | ".popsection"
                | ".section"
                | ".previous"
                | ".type"
                | ".size"
                | ".ident"
                | ".globl"
                | ".global"
                | ".weak"
                | ".hidden"
                | ".protected"
                | ".balign"
                | ".p2align"
                | ".align"
                | ".long"
                | ".quad"
                | ".byte"
                | ".asciz"
                | ".string"
                | ".text"
                | ".data"
                | ".bss"
                | ".rodata"
                | ".org"
                | ".zero"
                | ".skip"
                | ".set"
                | ".equ"
                | ".macro"
                | ".endm"
                | ".if"
                | ".endif"
                | ".else"
                | ".elseif"
                | ".rept"
                | ".endr"
                | ".irp"
                | ".irpc"
                | ".loc"
                | ".file"
                | ".symver"
                | ".local"
                | ".comm"
                | ".lcomm"
                | ".altmacro"
                | ".code64"
                | ".subsection"
                | ".uleb128"
                | ".sleb128"
                | ".2byte"
                | ".4byte"
                | ".8byte"
                | ".word"
                | ".int"
                | ".error"
                | ".warning"
                | ".include"
                | ".incbin"
                | ".cfi_startproc"
                | ".cfi_endproc"
                | ".cfi_def_cfa_offset"
                | ".cfi_def_cfa"
                | ".cfi_offset"
                | ".cfi_restore"
                | ".cfi_remember_state"
                | ".cfi_restore_state"
                | ".cfi_adjust_cfa_offset"
                | ".cfi_def_cfa_register"
                | ".cfi_return_column"
                | ".cfi_signal_frame"
                | ".cfi_window_save"
                | ".cfi_negate_ra_state"
        )
    }
    pub(crate) fn is_char_array(ty: &Type) -> bool {
        matches!(
            ty,
            Type::Array(e, _)
                if matches!(e.base, Type::Char | Type::SChar | Type::UChar)
        )
    }
    pub(crate) fn is_global_object(&self, source_name: &str) -> bool {
        self.lookup_global_info(source_name).is_some()
            || self
                .static_local_map
                .is_static_local(&self.current_func_name, source_name)
    }
    fn leaf_abi_class(ty: &Type) -> AbiClass {
        match ty {
            Type::Float | Type::Double => AbiClass::Sse,
            Type::LongDouble => AbiClass::Memory,
            // _Complex 分类跟随实数内型 (SysV ABI 3.2.3: 由分量决定):
            // float/double → SSE, long double → Memory,
            // 整数 (_Complex int/long) → INTEGER。此前整数 _Complex
            // 误为 SSE, 参数/返回走错寄存器组 (complex-6)。
            Type::Complex(inner) => Self::leaf_abi_class(&inner.base),
            // _Imaginary float/double: 同 SSE
            Type::Imaginary(inner) if matches!(&inner.base, Type::Float | Type::Double) => {
                AbiClass::Sse
            },
            // SIMD 向量: 首 eightbyte SSE, 后续 SseUp
            Type::Vector(_, _) => AbiClass::Sse,
            // 整数/指针/枚举等
            _ => AbiClass::Integer,
        }
    }
    pub(crate) fn materialize_compound_literal(
        &mut self,
        e: &Expr,
        use_rodata: bool,
    ) -> Option<(String, u64)> {
        let (ty, inits) = match e {
            Expr::CompoundLit { ty, inits, .. } => (ty.clone(), inits.clone()),
            Expr::InitList { inits, ty, .. } => (ty.clone(), inits.clone()),
            _ => return None,
        };
        let sz = ty.size().max(1);
        let align = ty.align().max(1);
        // 匿名名: .L.compound_<next_label>
        let anon_name = format!(".L.compound.{}", self.next_label);
        self.next_label += 1;
        // 分配 rodata 空间并对齐
        let section = if use_rodata {
            while !self.rodata.len().is_multiple_of(align) {
                self.rodata.push(0);
            }
            let base = self.rodata.len() as u64;
            // 递归生成初始化数据 (嵌套 AddrOf→CompoundLit 走 eval_const_expr)
            let init_data = self
                .gen_global_init_list(&ty, &inits, Some((base, None)), use_rodata)
                .unwrap_or_else(|| vec![0u8; sz]);
            self.rodata.extend_from_slice(&init_data);
            while !self.rodata.len().is_multiple_of(align) {
                self.rodata.push(0);
            }
            (base, 1u16)
        } else {
            while !self.tdata_section.len().is_multiple_of(align) {
                self.tdata_section.push(0);
            }
            let base = self.tdata_section.len() as u64;
            let init_data = self
                .gen_global_init_list(&ty, &inits, Some((base, None)), use_rodata)
                .unwrap_or_else(|| vec![0u8; sz]);
            self.tdata_section.extend_from_slice(&init_data);
            while !self.tdata_section.len().is_multiple_of(align) {
                self.tdata_section.push(0);
            }
            (base, 5u16)
        };
        let vaddr = self.entry + section.0;
        // 记录匿名对象 (嵌套 AddrOf→CompoundLit 时 eval_global_lvalue_offset 可查找)
        self.globals_raw.insert(
            anon_name.clone(),
            GlobalVar {
                offset: section.0,
                size: sz,
                ty,
                init: None,
                section: section.1,
                is_tls: false,
                in_data_section: false,
            },
        );
        Some((anon_name, vaddr))
    }
    pub(crate) fn materialize_compound_literal_rel(&mut self, e: &Expr) -> Option<(String, u64)> {
        let (ty, inits) = match e {
            Expr::CompoundLit { ty, inits, .. } => (ty.clone(), inits.clone()),
            Expr::InitList { inits, ty, .. } => (ty.clone(), inits.clone()),
            _ => return None,
        };
        let sz = ty.size().max(1);
        let align = ty.align().max(1);
        // 去重缓存: 同类型同内容只物化一次
        let cache_key = format!("{:?}", e);
        if let Some((cached_name, cached_off)) = self.compound_lit_cache.get(&cache_key) {
            return Some((cached_name.clone(), *cached_off));
        }
        let anon_name = format!(".L.compound.{}", self.next_label);
        self.next_label += 1;
        // 关键: 先递归物化所有子 AddrOf(CompoundLit), 让 rodata 正确增长,
        // 再取 base 保证父子对象的偏移不重叠.
        self.materialize_nested_compound_lits(&inits);
        // 对齐 rodata, 此时所有子对象已经在 rodata 里了
        while !self.rodata.len().is_multiple_of(align) {
            self.rodata.push(0);
        }
        let base = self.rodata.len() as u64;
        let rodata_marker: Option<usize> = Some(usize::MAX);
        let init_data = self
            .gen_global_init_list(&ty, &inits, Some((base, rodata_marker)), false)
            .unwrap_or_else(|| vec![0u8; sz]);
        self.rodata.extend_from_slice(&init_data);
        while !self.rodata.len().is_multiple_of(align) {
            self.rodata.push(0);
        }
        self.globals_raw.insert(
            anon_name.clone(),
            GlobalVar {
                offset: base,
                size: sz,
                ty,
                init: None,
                section: 2,
                is_tls: false,
                in_data_section: false,
            },
        );
        self.compound_lit_cache
            .insert(cache_key, (anon_name.clone(), base));
        Some((anon_name, base))
    }
    fn merge_classes(a: AbiClass, b: AbiClass) -> AbiClass {
        use AbiClass::*;
        if a == b {
            return a;
        }
        // NO_CLASS 为初始值, 合并时取另一方
        if a == NoClass {
            return b;
        }
        if b == NoClass {
            return a;
        }
        // 含 MEMORY 则结果为 MEMORY
        if a == Memory || b == Memory {
            return Memory;
        }
        match (a, b) {
            (Integer, Sse) | (Sse, Integer) => Memory,
            (Integer, X87) | (X87, Integer) => Memory,
            (Integer, X87Up) | (X87Up, Integer) => Memory,
            (Integer, ComplexX87) | (ComplexX87, Integer) => Memory,
            (Sse, X87) | (X87, Sse) => Memory,
            (Sse, X87Up) | (X87Up, Sse) => Memory,
            (Sse, ComplexX87) | (ComplexX87, Sse) => Memory,
            (X87, X87Up) | (X87Up, X87) => Memory,
            (X87, ComplexX87) | (ComplexX87, X87) => Memory,
            (X87Up, ComplexX87) | (ComplexX87, X87Up) => Memory,
            // SSEUP 仅与 SSE 合并产生 SSEUP (向量延续)
            (Sse, SseUp) | (SseUp, Sse) => SseUp,
            _ => Memory,
        }
    }
    pub(crate) fn modrm_reg_reg(reg: u8, rm: u8) -> u8 {
        0xC0 | ((reg & 7) << 3) | (rm & 7)
    }
    pub(crate) fn parse_explicit_reg_name(name: &str) -> Option<ExplicitRegInfo> {
        // 段寄存器
        match name {
            "gs" => {
                return Some(ExplicitRegInfo {
                    reg: 5,
                    size: 16,
                    att_name: "gs",
                    kind: ExplicitRegKind::Seg,
                })
            },
            "fs" => {
                return Some(ExplicitRegInfo {
                    reg: 4,
                    size: 16,
                    att_name: "fs",
                    kind: ExplicitRegKind::Seg,
                })
            },
            "es" => {
                return Some(ExplicitRegInfo {
                    reg: 0,
                    size: 16,
                    att_name: "es",
                    kind: ExplicitRegKind::Seg,
                })
            },
            "cs" => {
                return Some(ExplicitRegInfo {
                    reg: 1,
                    size: 16,
                    att_name: "cs",
                    kind: ExplicitRegKind::Seg,
                })
            },
            "ss" => {
                return Some(ExplicitRegInfo {
                    reg: 2,
                    size: 16,
                    att_name: "ss",
                    kind: ExplicitRegKind::Seg,
                })
            },
            "ds" => {
                return Some(ExplicitRegInfo {
                    reg: 3,
                    size: 16,
                    att_name: "ds",
                    kind: ExplicitRegKind::Seg,
                })
            },
            _ => {},
        }
        // GP 子寄存器
        let (idx, bits, sname) = Self::parse_reg_name_bits(name)?;
        Some(ExplicitRegInfo {
            reg: idx,
            size: bits,
            att_name: sname,
            kind: ExplicitRegKind::Gp,
        })
    }
    /// 显式寄存器引用的名字 → (号, 位宽, 规范名)。名字表只有一份
    /// （`abi/regnames.rs`，Phase 6.1）：这里以前是那张 68 条的本地拷贝，与
    /// `asm/expansion.rs` 里那一份逐字相同（本轮逐条对拍确认），两份各自演化就会
    /// 漂移。strict 口径不认 `%`、不认大写、不认 APX —— 扩大接受集会改变 asm 展开的
    /// 字节，所以这个分寸由表里的 `percent_ok`/`extended` 逐条描述而不是堆在调用点。
    pub(crate) fn parse_reg_name_bits(name: &str) -> Option<(u8, u8, &'static str)> {
        crate::codegen::x86_64::abi::regnames::lookup_reg_name(
            name,
            &crate::codegen::x86_64::abi::regnames::NAME_FORMS_STRICT,
        )
        .map(|e| (e.num, e.bits, e.name))
    }
    pub fn pos(&self) -> u64 {
        self.code.len() as u64
    }
    pub(crate) fn resolve_complete_struct_ty(&self, ty: Type) -> Type {
        match &ty {
            Type::Struct(name, fields, _, _) if fields.is_empty() => {
                if let Some(complete_ty) = self.complete_struct_types.get(name) {
                    return complete_ty.clone();
                }
            },
            Type::Union(name, fields, _, _) if fields.is_empty() => {
                if let Some(complete_ty) = self.complete_struct_types.get(name) {
                    return complete_ty.clone();
                }
            },
            _ => {},
        }
        ty
    }
    pub(crate) fn stmt_contains_label(stmt: &Stmt) -> bool {
        match stmt {
            Stmt::Label { .. } => true,
            Stmt::Block { b, .. } => b.items.iter().any(|item| match item {
                BlockItem::Stmt(s) => Self::stmt_contains_label(s),
                _ => false,
            }),
            Stmt::If { then_, else_, .. } => {
                Self::stmt_contains_label(then_)
                    || else_.as_ref().is_some_and(|e| Self::stmt_contains_label(e))
            },
            Stmt::While { body, .. } | Stmt::Do { body, .. } => Self::stmt_contains_label(body),
            Stmt::For { body, .. } => Self::stmt_contains_label(body),
            _ => false,
        }
    }
    pub(crate) fn stmt_ends_with_return(stmt: &Stmt) -> bool {
        match stmt {
            Stmt::Return { .. } => true,
            Stmt::Break { .. } => true,
            Stmt::Continue { .. } => true,
            Stmt::Block { b, .. } => Self::ends_with_return(&b.items),
            Stmt::If { then_, else_, .. } => {
                Self::stmt_ends_with_return(then_)
                    && else_
                        .as_ref()
                        .is_some_and(|e| Self::stmt_ends_with_return(e))
            },
            _ => false,
        }
    }
    pub(crate) fn strip_asm_seg_prefix(s: &str) -> (&str, Option<u8>) {
        let s = s.trim_start();
        let (head, rest) = if let Some(after) = s.strip_prefix('%') {
            // 找第一个非字母字符作为 head/rs 分割
            let pos = after
                .find(|c: char| !c.is_ascii_alphabetic())
                .unwrap_or(after.len());
            (&after[..pos], &after[pos..])
        } else {
            let pos = s
                .find(|c: char| !c.is_ascii_alphabetic())
                .unwrap_or(s.len());
            (&s[..pos], &s[pos..])
        };
        if let Some((code, _pfx)) = Self::parse_asm_seg(head) {
            if let Some(after_colon) = rest.strip_prefix(':') {
                return (after_colon, Some(code));
            }
            // 单独段名: 整个 s 已经被消耗为段名
            if rest.is_empty() {
                return ("", Some(code));
            }
        }
        (s, None)
    }
    fn strip_casts(e: &Expr) -> &Expr {
        let mut cur = e;
        while let Expr::Cast { e: inner, .. } = cur {
            cur = inner;
        }
        cur
    }
    pub(crate) fn wrap_indirect(inner: AsmOperand) -> AsmOperand {
        // 间接 Reg → 保持 Reg, emit 阶段识别 (jmp *%rax = jmp %rax)
        // 间接 Mem → 保持 Mem
        // 间接 Sym → 标记为 SymDisp(sym, 0) 表示间接跳转
        match inner {
            AsmOperand::Sym(name) => AsmOperand::SymDisp(name, 0),
            other => other, // Reg/Mem 原样返回
        }
    }
}
